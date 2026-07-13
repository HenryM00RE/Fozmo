use crate::api::routes::{
    create_router, sonos_art, sonos_qobuz_stream, sonos_stream, upnp_art, upnp_art_head,
    upnp_qobuz_stream, upnp_qobuz_stream_head, upnp_qobuz_stream_path, upnp_qobuz_stream_path_head,
    upnp_stream, upnp_stream_head,
};
use crate::app::auth::{require_pairing, trusted_web_origins};
use crate::app::config::AppConfig;
use crate::app::error::AppError;
use crate::app::identity;
use crate::app::paths::AppPaths;
use crate::app::state::AppState;
use crate::app::static_files::{add_static_routes, cache_response_headers};
use crate::services::discovery;
use crate::web;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderName, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use axum::{
    Extension, Json, middleware,
    routing::{get, post},
};
use reqwest::Url;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::HashSet;
use std::future::pending;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::{error, info, warn};

#[derive(Clone)]
struct HealthConfig {
    port: u16,
    lan_enabled: bool,
    pairing_required: bool,
    public_base_url: String,
    browser_base_url: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    port: u16,
    lan_enabled: bool,
    pairing_required: bool,
    public_base_url: String,
    browser_base_url: String,
}

#[derive(Clone)]
struct AllowedHosts(Arc<HashSet<String>>);

#[derive(Clone)]
struct LauncherControl {
    token: Option<String>,
}

#[derive(Debug, Serialize)]
struct LauncherBackupResponse {
    status: &'static str,
    error: Option<String>,
}

pub async fn serve(state: AppState, paths: &AppPaths, config: &AppConfig) -> Result<(), AppError> {
    let app = build_router(state.clone(), paths, config);
    let addr = bind_addr(config);
    info!(event = "server_start", %addr, "Starting web control panel");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|source| AppError::ServerBind { addr, source })?;
    paths
        .record_current_start_success(crate::library::Library::current_schema_version())
        .map_err(AppError::Persistence)?;

    // Apply persisted remote access settings once at startup. Failures are
    // captured in the controller status and never block the local listener.
    let remote_status = state.remote_access().apply(&state).await;
    if let Some(error) = &remote_status.last_error {
        error!(
            event = "remote_access_startup",
            status = "error",
            error_kind = "remote_listener",
            error = %error,
            "Remote access listener failed to start; continuing without it"
        );
    }

    let _core_mdns = advertise_lan_core(config);
    let serve_result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(config.exit_on_stdin_eof))
    .await
    .map_err(AppError::Server);

    shutdown_services(&state).await;
    serve_result?;

    Ok(())
}

fn build_router(state: AppState, paths: &AppPaths, config: &AppConfig) -> axum::Router {
    let api_router = create_router()
        .route("/api/ws", get(web::ws::ws_handler))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::app::auth::resolve_profile_context,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::app::auth::enforce_browser_zone_ownership,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_pairing,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::app::auth::require_lan_admin,
        ));

    let health = HealthConfig {
        port: config.port,
        lan_enabled: config.lan_enabled,
        pairing_required: config.pairing_required,
        public_base_url: config.public_base_url.clone(),
        browser_base_url: discovery::local_browser_base_url(config.port),
    };
    let app = api_router
        .route("/healthz", get(healthz))
        .route("/internal/launcher/backup", post(launcher_backup))
        .route("/sonos/stream/:asset_id", get(sonos_stream))
        .route("/sonos/qobuz/:track_id", get(sonos_qobuz_stream))
        .route("/sonos/art/:asset_id", get(sonos_art))
        .route(
            "/upnp/stream/:asset_id",
            get(upnp_stream).head(upnp_stream_head),
        )
        .route(
            "/upnp/qobuz/:track_id",
            get(upnp_qobuz_stream).head(upnp_qobuz_stream_head),
        )
        .route(
            "/upnp/qobuz/:asset_id/:token/:track_id",
            get(upnp_qobuz_stream_path).head(upnp_qobuz_stream_path_head),
        )
        .route("/upnp/art/:asset_id", get(upnp_art).head(upnp_art_head));

    add_static_routes(app, paths)
        .layer(middleware::from_fn(cache_response_headers))
        .layer(cors_layer(config))
        .layer(middleware::from_fn(validate_host))
        .layer(Extension(allowed_hosts(config)))
        .layer(Extension(trusted_web_origins(config)))
        .layer(Extension(launcher_control_from_env()))
        .layer(Extension(health))
        .with_state(state)
}

async fn launcher_backup(
    State(state): State<AppState>,
    Extension(control): Extension<LauncherControl>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
) -> (StatusCode, Json<LauncherBackupResponse>) {
    if !peer
        .as_ref()
        .is_some_and(|ConnectInfo(address)| address.ip().is_loopback())
    {
        return launcher_backup_error(StatusCode::FORBIDDEN, "loopback access is required");
    }
    if !launcher_token_authorized(&control, &headers) {
        return launcher_backup_error(StatusCode::FORBIDDEN, "launcher authentication failed");
    }

    match tokio::task::spawn_blocking(move || state.create_persistence_backup("pre-update")).await {
        Ok(Ok(_backup)) => (
            StatusCode::OK,
            Json(LauncherBackupResponse {
                status: "ok",
                error: None,
            }),
        ),
        Ok(Err(error)) => launcher_backup_internal_error(&error),
        Err(error) => launcher_backup_internal_error(&format!("backup worker failed: {error}")),
    }
}

fn launcher_backup_internal_error(error: &str) -> (StatusCode, Json<LauncherBackupResponse>) {
    tracing::error!(
        event = "launcher_backup",
        status = "error",
        error_kind = "persistence",
        error = %crate::diagnostics::logging::sanitize_error(error),
        "Launcher persistence backup failed"
    );
    launcher_backup_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
}

fn launcher_backup_error(
    status: StatusCode,
    message: &str,
) -> (StatusCode, Json<LauncherBackupResponse>) {
    (
        status,
        Json(LauncherBackupResponse {
            status: "error",
            error: Some(message.to_string()),
        }),
    )
}

fn launcher_control_from_env() -> LauncherControl {
    let key = identity::env_key("LAUNCHER_CONTROL_TOKEN");
    let token = std::env::var(&key).ok().filter(|value| value.len() >= 32);
    if std::env::var_os(&key).is_some() && token.is_none() {
        warn!(
            event = "launcher_control",
            "Ignoring a launcher control token shorter than 32 bytes"
        );
    }
    LauncherControl { token }
}

fn launcher_token_authorized(control: &LauncherControl, headers: &HeaderMap) -> bool {
    let Some(expected) = control.token.as_ref() else {
        return false;
    };
    let Some(presented) = headers
        .get(identity::LAUNCHER_CONTROL_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    crate::zones::constant_time_token_matches(std::slice::from_ref(expected), presented)
}

async fn healthz(
    Extension(config): Extension<HealthConfig>,
    peer: Option<ConnectInfo<SocketAddr>>,
) -> Result<Json<HealthResponse>, StatusCode> {
    if !peer
        .as_ref()
        .is_some_and(|ConnectInfo(address)| address.ip().is_loopback())
    {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(Json(HealthResponse {
        status: "ready",
        version: env!("CARGO_PKG_VERSION"),
        port: config.port,
        lan_enabled: config.lan_enabled,
        pairing_required: config.pairing_required,
        public_base_url: config.public_base_url,
        browser_base_url: config.browser_base_url,
    }))
}

fn allowed_hosts(config: &AppConfig) -> AllowedHosts {
    let mut hosts = HashSet::from([
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ]);
    hosts.extend(discovery::trusted_browser_hostnames());
    if let Some(host) = Url::parse(&config.public_base_url)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()))
    {
        hosts.insert(host);
    }
    for ip in discovery::active_interface_ips() {
        hosts.insert(ip.to_string().to_ascii_lowercase());
    }
    AllowedHosts(Arc::new(hosts))
}

async fn validate_host(
    Extension(allowed): Extension<AllowedHosts>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(raw_host) = req.headers().get(header::HOST) else {
        // HTTP/2 and in-process tests may omit Host; browsers using HTTP/1.1 do
        // not. A presented Host is always validated to prevent DNS rebinding.
        return Ok(next.run(req).await);
    };
    let raw_host = raw_host.to_str().map_err(|_| StatusCode::BAD_REQUEST)?;
    let host = normalized_host(raw_host).ok_or(StatusCode::BAD_REQUEST)?;
    if !allowed.0.contains(&host) {
        warn!(
            event = "host_validation",
            host, "Rejected unrecognised Host header"
        );
        return Err(StatusCode::MISDIRECTED_REQUEST);
    }
    Ok(next.run(req).await)
}

fn normalized_host(authority: &str) -> Option<String> {
    let authority = authority.trim();
    if authority.is_empty() || authority.chars().any(|ch| matches!(ch, '/' | '\\' | '@')) {
        return None;
    }
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split_once(']')?.0
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        if port.chars().all(|ch| ch.is_ascii_digit()) {
            host
        } else {
            authority
        }
    } else {
        authority
    };
    Some(host.trim_end_matches('.').to_ascii_lowercase())
}

fn cors_layer(config: &AppConfig) -> CorsLayer {
    let allowed_origins = trusted_web_origins(config);
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |origin, _| {
            origin
                .to_str()
                .ok()
                .is_some_and(|origin| allowed_origins.allows(origin))
        }))
        .allow_methods([
            Method::GET,
            Method::HEAD,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::ACCEPT,
            header::RANGE,
            HeaderName::from_static(identity::AUTH_HEADER),
        ])
        .expose_headers([
            header::ACCEPT_RANGES,
            header::CONTENT_LENGTH,
            header::CONTENT_RANGE,
        ])
}

fn bind_addr(config: &AppConfig) -> SocketAddr {
    if config.lan_enabled {
        SocketAddr::from(([0, 0, 0, 0], config.port))
    } else {
        SocketAddr::from(([127, 0, 0, 1], config.port))
    }
}

fn advertise_lan_core(config: &AppConfig) -> Option<discovery::CoreMdnsAdvertisement> {
    if !config.lan_enabled || !config.core_mdns_enabled {
        info!(
            event = "lan_discovery_disabled",
            "LAN discovery disabled; start with --lan or FOZMO_LAN=1 for agents."
        );
        return None;
    }

    let instance_name = format!(
        "{} — {}",
        identity::APP_DISPLAY_NAME,
        discovery::hostname_fallback("Mac")
    );
    match discovery::advertise_core(
        &instance_name,
        config.port,
        &config.public_base_url,
        config.pairing_required,
    ) {
        Ok(advertisement) => {
            info!(
                event = "lan_core_advertise",
                port = config.port,
                pairing_required = config.pairing_required,
                "Advertising LAN core"
            );
            Some(advertisement)
        }
        Err(e) => {
            error!(
                event = "external_service_failure",
                service = "discovery",
                error_kind = "network",
                error = %e,
                "Failed to advertise LAN core"
            );
            None
        }
    }
}

async fn shutdown_signal(exit_on_stdin_eof: bool) {
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                warn!(event = "shutdown_signal", error = %error, "Could not register SIGTERM handler");
                pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = pending::<()>();

    let parent_pipe_closed = async move {
        if !exit_on_stdin_eof {
            pending::<()>().await;
        }
        use tokio::io::AsyncReadExt;
        let mut stdin = tokio::io::stdin();
        let mut buffer = [0_u8; 256];
        loop {
            match stdin.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    };

    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            if let Err(error) = result {
                warn!(event = "shutdown_signal", error = %error, "Could not wait for interrupt signal");
            }
        }
        _ = terminate => {}
        _ = parent_pipe_closed => {}
    }
    info!(event = "server_shutdown", "Graceful shutdown requested");
}

async fn shutdown_services(state: &AppState) {
    for zone in state.zones().list_zones() {
        if let Err(error) = crate::playback::control::stop_for_zone(state, &zone.id).await {
            warn!(
                event = "server_shutdown",
                zone_id = %zone.id,
                error = ?error,
                "Could not stop zone cleanly"
            );
        }
        // A renderer or remote agent can be unreachable during shutdown. The
        // normal playback stop path finalizes history only after its transport
        // command succeeds, so close the in-memory listen here regardless of
        // that result. This is intentionally idempotent when stop_for_zone
        // already finalized the zone successfully.
        state.listening().stop(state.library(), &zone.id);
    }
    state.remote_access().shutdown(state).await;
    if let Err(error) = state.library().checkpoint() {
        warn!(event = "server_shutdown", error = %error, "SQLite checkpoint failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::config::AppMode;
    use crate::listening::PlaybackObservation;
    use crate::playback::test_support::{agent_capabilities, app_state, qobuz_source};
    use axum::{Router, body::Body};
    use tower::ServiceExt;

    fn config(lan_enabled: bool) -> AppConfig {
        AppConfig {
            mode: AppMode::Core,
            log_format: crate::diagnostics::logging::LogFormat::Compact,
            lan_enabled,
            pairing_required: false,
            pairing_token_ttl_secs: crate::zones::DEFAULT_PAIRING_TOKEN_TTL_SECS,
            allow_query_token_auth: false,
            startup_scan_enabled: false,
            exit_on_stdin_eof: false,
            core_mdns_enabled: true,
            release_smoke: false,
            port: 3000,
            public_base_url: "http://127.0.0.1:3000".to_string(),
        }
    }

    #[test]
    fn bind_addr_uses_loopback_by_default() {
        assert_eq!(
            bind_addr(&config(false)),
            SocketAddr::from(([127, 0, 0, 1], 3000))
        );
    }

    #[test]
    fn bind_addr_uses_unspecified_addr_for_lan_mode() {
        assert_eq!(
            bind_addr(&config(true)),
            SocketAddr::from(([0, 0, 0, 0], 3000))
        );
    }

    fn cors_test_app(config: &AppConfig) -> Router {
        Router::new()
            .route("/ok", get(|| async { "ok" }))
            .layer(cors_layer(config))
    }

    async fn cors_get_status_and_origin(config: &AppConfig, origin: &str) -> (u16, Option<String>) {
        let response = cors_test_app(config)
            .oneshot(
                axum::http::Request::builder()
                    .method(Method::GET)
                    .uri("/ok")
                    .header(header::ORIGIN, origin)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        let status = response.status().as_u16();
        let allow_origin = response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        (status, allow_origin)
    }

    fn host_test_app(config: &AppConfig) -> Router {
        Router::new()
            .route("/ok", get(|| async { "ok" }))
            .layer(middleware::from_fn(validate_host))
            .layer(Extension(allowed_hosts(config)))
    }

    #[tokio::test]
    async fn cors_allows_loopback_control_origins() {
        let config = config(false);

        let (status, allow_origin) =
            cors_get_status_and_origin(&config, "http://localhost:3000").await;

        assert_eq!(status, 200);
        assert_eq!(allow_origin.as_deref(), Some("http://localhost:3000"));
    }

    #[tokio::test]
    async fn cors_allows_configured_lan_public_base_origin() {
        let mut config = config(true);
        config.port = 3001;
        config.public_base_url = "http://192.168.1.42:3001".to_string();

        let (status, allow_origin) =
            cors_get_status_and_origin(&config, "http://192.168.1.42:3001").await;

        assert_eq!(status, 200);
        assert_eq!(allow_origin.as_deref(), Some("http://192.168.1.42:3001"));
    }

    #[tokio::test]
    async fn cors_does_not_authorize_unrelated_origins() {
        let config = config(true);

        let (status, allow_origin) = cors_get_status_and_origin(&config, "https://evil.test").await;

        assert_eq!(status, 200);
        assert_eq!(allow_origin, None);
    }

    #[test]
    fn host_normalization_handles_ports_ipv6_and_trailing_dot() {
        assert_eq!(
            normalized_host("Fozmo-Studio.local:3001").as_deref(),
            Some("fozmo-studio.local")
        );
        assert_eq!(normalized_host("[::1]:3001").as_deref(), Some("::1"));
        assert_eq!(normalized_host("localhost.").as_deref(), Some("localhost"));
        assert_eq!(normalized_host("evil.test@localhost"), None);
    }

    #[test]
    fn launcher_backup_requires_the_exact_per_launch_secret() {
        let control = LauncherControl {
            token: Some("abcdefghijklmnopqrstuvwxyz0123456789".to_string()),
        };
        let mut headers = HeaderMap::new();
        assert!(!launcher_token_authorized(&control, &headers));
        headers.insert(
            identity::LAUNCHER_CONTROL_HEADER,
            "wrong-but-still-long-enough-0123456789".parse().unwrap(),
        );
        assert!(!launcher_token_authorized(&control, &headers));
        headers.insert(
            identity::LAUNCHER_CONTROL_HEADER,
            "abcdefghijklmnopqrstuvwxyz0123456789".parse().unwrap(),
        );
        assert!(launcher_token_authorized(&control, &headers));
    }

    #[test]
    fn launcher_backup_internal_failures_hide_persistence_detail() {
        let (status, Json(response)) = launcher_backup_internal_error(
            "backup failed at /Users/fixture/private.db token=private-token",
        );

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.status, "error");
        assert_eq!(response.error.as_deref(), Some("Internal server error"));
    }

    #[tokio::test]
    async fn host_validation_allows_local_names_and_rejects_unrecognised_hosts() {
        let mut config = config(true);
        config.port = 3001;
        config.public_base_url = "http://192.168.1.42:3001".to_string();

        let mut trusted_hosts = discovery::trusted_browser_hostnames();
        trusted_hosts.push("192.168.1.42".to_string());
        for host in trusted_hosts {
            let allowed = host_test_app(&config)
                .oneshot(
                    axum::http::Request::builder()
                        .uri("/ok")
                        .header(header::HOST, format!("{host}:3001"))
                        .body(Body::empty())
                        .expect("request should build"),
                )
                .await
                .expect("router should respond");
            assert_eq!(
                allowed.status(),
                StatusCode::OK,
                "rejected trusted Host {host}"
            );
        }

        for host in ["203.0.113.9:3001", "core.example.test.attacker.test:3001"] {
            let rejected = host_test_app(&config)
                .oneshot(
                    axum::http::Request::builder()
                        .uri("/ok")
                        .header(header::HOST, host)
                        .body(Body::empty())
                        .expect("request should build"),
                )
                .await
                .expect("router should respond");
            assert_eq!(rejected.status(), StatusCode::MISDIRECTED_REQUEST);
        }
    }

    #[tokio::test]
    async fn cors_allows_the_exact_system_hostname_origin() {
        let Some(hostname) = discovery::system_hostname() else {
            return;
        };
        let mut config = config(true);
        config.port = 3001;
        config.public_base_url = "http://192.168.1.42:3001".to_string();
        let origin = format!("http://{hostname}:3001");

        let (status, allow_origin) = cors_get_status_and_origin(&config, &origin).await;
        assert_eq!(status, 200);
        assert_eq!(allow_origin.as_deref(), Some(origin.as_str()));

        let (_, lookalike) =
            cors_get_status_and_origin(&config, &format!("http://{hostname}.attacker.test:3001"))
                .await;
        assert_eq!(lookalike, None);
        let (_, wrong_port) =
            cors_get_status_and_origin(&config, &format!("http://{hostname}:3002")).await;
        assert_eq!(wrong_port, None);
        let (_, wrong_scheme) =
            cors_get_status_and_origin(&config, &format!("https://{hostname}:3001")).await;
        assert_eq!(wrong_scheme, None);
    }

    #[tokio::test]
    async fn health_is_loopback_only() {
        let config = HealthConfig {
            port: 3001,
            lan_enabled: true,
            pairing_required: true,
            public_base_url: "http://192.168.1.42:3001".to_string(),
            browser_base_url: "http://studio.local:3001".to_string(),
        };
        let local = healthz(
            Extension(config.clone()),
            Some(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 50123)))),
        )
        .await
        .expect("loopback health should succeed")
        .0;
        assert_eq!(local.status, "ready");
        assert_eq!(local.port, 3001);

        let remote = healthz(
            Extension(config),
            Some(ConnectInfo(SocketAddr::from(([192, 168, 1, 9], 50123)))),
        )
        .await;
        assert!(matches!(remote, Err(StatusCode::FORBIDDEN)));
    }

    #[tokio::test]
    async fn shutdown_finalizes_history_when_remote_agent_stop_fails() {
        let state = app_state("shutdown-history-after-agent-stop-error");
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        state.zones().register_agent(
            "disconnected-agent".to_string(),
            "Disconnected Agent".to_string(),
            agent_capabilities("Agent DAC"),
            tx,
        );
        drop(rx);
        let zone = state
            .zones()
            .list_zones()
            .into_iter()
            .find(|zone| zone.agent_name.as_deref() == Some("Disconnected Agent"))
            .expect("remote agent zone should be registered");
        let source = qobuz_source(42, false);
        state.listening().start(
            state.library(),
            zone.id.clone(),
            zone.name.clone(),
            "default".to_string(),
            source.clone(),
            Vec::new(),
        );
        let observation = PlaybackObservation {
            state: "Playing".to_string(),
            current_source: Some(source),
            position_secs: 1.0,
            duration_secs: 180.0,
            ..PlaybackObservation::default()
        };
        state.listening().observe(
            state.library(),
            &zone.id,
            "default".to_string(),
            observation.clone(),
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        state.listening().observe(
            state.library(),
            &zone.id,
            "default".to_string(),
            observation,
        );

        let stop_error = crate::playback::control::stop_for_zone(&state, &zone.id)
            .await
            .expect_err("disconnected agent stop should fail");
        assert!(stop_error.message().contains("disconnected"));
        assert!(state.listening().active_source(&zone.id).is_some());

        shutdown_services(&state).await;

        assert!(state.listening().active_source(&zone.id).is_none());
        let history = state
            .library()
            .recent_playback_history(10, true)
            .expect("history query should succeed");
        let entry = history
            .iter()
            .find(|entry| entry.zone_id == zone.id)
            .expect("shutdown should persist the active remote-agent listen");
        assert!(entry.played_secs.is_some_and(|seconds| seconds > 0.25));
    }
}
