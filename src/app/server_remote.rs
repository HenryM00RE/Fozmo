//! TLS-only remote access listener.
//!
//! Runs a second, deliberately allowlisted router on `0.0.0.0:<remote port>`
//! for direct router-port-forward exposure (no vendor cloud, no reverse
//! proxy). Every request except `POST /api/remote/session` and public static
//! app-shell assets requires a remote-scoped session cookie; auth never
//! consults the optional LAN pairing flag. Forwarding headers such as
//! `X-Forwarded-For`/`X-Forwarded-Proto` are never trusted here.

use crate::app::auth::{RequestSurface, remote_security_headers, require_remote_auth};
use crate::app::paths::AppPaths;
use crate::app::remote_tls::{self, RemoteTlsIdentity};
use crate::app::state::AppState;
use crate::app::static_files::{add_static_routes, cache_response_headers};
use crate::settings::RemoteAccessSettings;
use crate::web;
use axum::http::StatusCode;
use axum::{Extension, middleware, routing::any, routing::get};
use axum_server::tls_rustls::RustlsConfig;
use rustls_pki_types::pem::PemObject;
use schemars::JsonSchema;
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{error, info, warn};

const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RemoteLinkCodeIssuance {
    HostLocal,
    AuthenticatedLan,
    Unavailable,
}

/// Controller status as reported through the LAN settings API. The listener
/// state only ever changes through `apply()`, which is reachable from
/// persisted settings at startup and the LAN-only settings endpoint.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RemoteAccessStatus {
    pub enabled: bool,
    pub running: bool,
    pub bound_port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_fingerprint_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub active_remote_sessions: usize,
    /// Request-scoped authority to issue an internet-facing Remote Access
    /// link code. Route handlers replace this conservative default using the
    /// actual peer and control-session credentials on each response.
    pub link_code_issuance: RemoteLinkCodeIssuance,
}

impl RemoteAccessStatus {
    fn stopped(settings: &RemoteAccessSettings, last_error: Option<String>) -> Self {
        Self {
            enabled: settings.enabled,
            running: false,
            bound_port: settings.port,
            external_host: settings.external_host.clone(),
            cert_fingerprint_sha256: None,
            last_error,
            active_remote_sessions: 0,
            link_code_issuance: RemoteLinkCodeIssuance::Unavailable,
        }
    }
}

struct ActiveListener {
    handle: axum_server::Handle,
    task: tokio::task::JoinHandle<()>,
}

struct ControllerInner {
    paths: AppPaths,
    app_port: u16,
    installation_id: String,
    status: Mutex<RemoteAccessStatus>,
    // Serializes apply() so concurrent settings updates cannot interleave
    // stop/start sequences.
    active: tokio::sync::Mutex<Option<ActiveListener>>,
}

#[derive(Clone)]
pub struct RemoteAccessController {
    inner: Arc<ControllerInner>,
}

impl RemoteAccessController {
    pub fn new(paths: &AppPaths, app_port: u16, installation_id: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(ControllerInner {
                paths: paths.clone(),
                app_port,
                installation_id: installation_id.into(),
                status: Mutex::new(RemoteAccessStatus::stopped(
                    &RemoteAccessSettings::default(),
                    None,
                )),
                active: tokio::sync::Mutex::new(None),
            }),
        }
    }

    /// The primary app port, used to reject colliding remote port settings.
    pub fn app_port(&self) -> u16 {
        self.inner.app_port
    }

    /// Last observed listener status with a live remote session count.
    pub fn status(&self, state: &AppState) -> RemoteAccessStatus {
        let mut status = self.inner.status.lock().unwrap().clone();
        status.active_remote_sessions = state.pairing().count_active_remote_sessions();
        status
    }

    /// Start, restart, or stop the listener to match the persisted settings.
    /// Errors are captured in the returned status; they never crash the app
    /// and never block the LAN/local listener.
    pub async fn apply(&self, state: &AppState) -> RemoteAccessStatus {
        let mut settings = state.settings().remote_access_settings();
        let mut active = self.inner.active.lock().await;

        if let Some(existing) = active.take() {
            existing.handle.graceful_shutdown(Some(SHUTDOWN_GRACE));
            let _ = existing.task.await;
            info!(
                event = "remote_access_stop",
                "Remote access listener stopped"
            );
        }

        if let Err(message) =
            crate::settings::validate_remote_access(&mut settings, self.inner.app_port)
        {
            let status = RemoteAccessStatus::stopped(&settings, Some(message));
            *self.inner.status.lock().unwrap() = status;
            return self.status(state);
        }

        if !settings.enabled {
            let status = RemoteAccessStatus::stopped(&settings, None);
            *self.inner.status.lock().unwrap() = status.clone();
            return self.status(state);
        }

        match self.start(state, &settings).await {
            Ok((listener, fingerprint)) => {
                *active = Some(listener);
                let status = RemoteAccessStatus {
                    enabled: true,
                    running: true,
                    bound_port: settings.port,
                    external_host: settings.external_host.clone(),
                    cert_fingerprint_sha256: Some(fingerprint),
                    last_error: None,
                    active_remote_sessions: 0,
                    link_code_issuance: RemoteLinkCodeIssuance::Unavailable,
                };
                *self.inner.status.lock().unwrap() = status;
                info!(
                    event = "remote_access_start",
                    port = settings.port,
                    "Remote access listener started"
                );
            }
            Err(message) => {
                error!(
                    event = "remote_access_start",
                    status = "error",
                    error_kind = "remote_listener",
                    error = %message,
                    "Remote access listener failed to start"
                );
                let status = RemoteAccessStatus::stopped(&settings, Some(message));
                *self.inner.status.lock().unwrap() = status;
            }
        }
        self.status(state)
    }

    /// Stop the remote listener during application shutdown without changing
    /// the user's persisted enabled preference.
    pub async fn shutdown(&self, state: &AppState) {
        let mut active = self.inner.active.lock().await;
        if let Some(existing) = active.take() {
            existing.handle.graceful_shutdown(Some(SHUTDOWN_GRACE));
            let _ = existing.task.await;
            info!(
                event = "remote_access_stop",
                "Remote access listener stopped for shutdown"
            );
        }
        let settings = state.settings().remote_access_settings();
        *self.inner.status.lock().unwrap() = RemoteAccessStatus::stopped(&settings, None);
    }

    async fn start(
        &self,
        state: &AppState,
        settings: &RemoteAccessSettings,
    ) -> Result<(ActiveListener, String), String> {
        let identity = remote_tls::load_or_generate(
            &self.inner.paths.tls_dir,
            state.secrets().as_ref(),
            settings,
            &self.inner.installation_id,
        )
        .map_err(|e| e.to_string())?;
        let fingerprint = identity.fingerprint_sha256.clone();
        let rustls_config = rustls_server_config(&identity)?;

        let addr = SocketAddr::from(([0, 0, 0, 0], settings.port));
        // Bind synchronously so bind failures are reported in the status
        // instead of racing an async serve task.
        let listener =
            std::net::TcpListener::bind(addr).map_err(|e| format!("failed to bind {addr}: {e}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("failed to configure listener: {e}"))?;

        let router = build_remote_app(state.clone(), &self.inner.paths, identity.custom);
        let handle = axum_server::Handle::new();
        let server =
            axum_server::from_tcp_rustls(listener, RustlsConfig::from_config(rustls_config))
                .handle(handle.clone());
        let inner = Arc::clone(&self.inner);
        let task = tokio::spawn(async move {
            if let Err(e) = server
                .serve(router.into_make_service_with_connect_info::<SocketAddr>())
                .await
            {
                warn!(
                    event = "remote_access_serve",
                    status = "error",
                    error_kind = "remote_listener",
                    error = %e,
                    "Remote access listener terminated with an error"
                );
                let mut status = inner.status.lock().unwrap();
                status.running = false;
                status.last_error = Some(e.to_string());
            }
        });

        Ok((ActiveListener { handle, task }, fingerprint))
    }
}

/// The remote request surface: an allowlisted router with mandatory
/// remote-session auth, security headers, no CORS layer (same-origin cookies
/// with `SameSite=Strict` are the CSRF posture), and a hard 404 for every API
/// path that is not explicitly registered.
pub(crate) fn build_remote_app(
    state: AppState,
    paths: &AppPaths,
    custom_cert: bool,
) -> axum::Router {
    let api = crate::api::routes::create_remote_router()
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
            require_remote_auth,
        ))
        // Session exchange is the only unauthenticated API route; it is
        // rate-limited by peer IP inside the handler.
        .merge(crate::api::routes::remote_session_routes())
        // Unregistered API and media-endpoint paths must return 404 instead of
        // falling through to the SPA shell.
        .route("/api/*excluded", any(remote_not_found))
        .route("/sonos/*excluded", any(remote_not_found))
        .route("/upnp/*excluded", any(remote_not_found));

    // HSTS is sent only for user-supplied certs: a self-signed cert plus HSTS
    // can trap browsers behind an unproceedable interstitial.
    add_static_routes(api, paths)
        .layer(middleware::from_fn(cache_response_headers))
        .layer(middleware::from_fn(move |req, next| {
            remote_security_headers(req, next, custom_cert)
        }))
        .layer(Extension(RequestSurface::Remote))
        .with_state(state)
}

async fn remote_not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

pub(crate) fn rustls_server_config(
    identity: &RemoteTlsIdentity,
) -> Result<Arc<rustls::ServerConfig>, String> {
    let certs = rustls_pki_types::CertificateDer::pem_slice_iter(identity.cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("invalid certificate PEM: {e:?}"))?;
    if certs.is_empty() {
        return Err("certificate PEM contained no certificates".to_string());
    }
    let key = rustls_pki_types::PrivateKeyDer::from_pem_slice(identity.key_pem.as_bytes())
        .map_err(|e| format!("invalid private key PEM: {e:?}"))?;
    // Pin the ring provider explicitly instead of relying on a process-wide
    // default that other rustls users in the dependency graph could change.
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS protocol configuration failed: {e}"))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("TLS certificate configuration failed: {e}"))?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::identity;
    use crate::playback::test_support::{app_state, app_state_with_pairing};
    use crate::protocol::AgentCapabilities;
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::extract::ConnectInfo;
    use axum::http::{Method, Request, header};
    use tower::ServiceExt;

    fn remote_app(state: &AppState) -> Router {
        let paths =
            AppPaths::from_workspace_dir(std::env::temp_dir().join("fozmo-remote-router-tests"));
        build_remote_app(state.clone(), &paths, false)
    }

    fn remote_request(
        method: Method,
        path: &str,
        cookie: Option<&str>,
        peer: Option<SocketAddr>,
        body: Option<serde_json::Value>,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "core.example.test:8443");
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        let body = match body {
            Some(body) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let mut request = builder.body(body).expect("request should build");
        if let Some(peer) = peer {
            request.extensions_mut().insert(ConnectInfo(peer));
        }
        request
    }

    async fn remote_status_code(
        app: &Router,
        method: Method,
        path: &str,
        cookie: Option<&str>,
    ) -> StatusCode {
        app.clone()
            .oneshot(remote_request(method, path, cookie, None, None))
            .await
            .expect("router should respond")
            .status()
    }

    fn remote_cookie(state: &AppState) -> String {
        let token = state.pairing().create_remote_session(None).unwrap().token;
        format!("{}={}", crate::zones::REMOTE_SESSION_COOKIE, token)
    }

    fn register_browser_zone(state: &AppState, agent_id: &str) {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        state.zones().register_agent(
            agent_id.to_string(),
            "Remote browser".to_string(),
            AgentCapabilities {
                output_devices: Vec::new(),
                output_device_capabilities: Vec::new(),
                max_sample_rate: 48_000,
                max_bit_depth: 24,
                exclusive_supported: false,
                supports_dsd128: false,
                supports_dsd256: false,
                browser: true,
            },
            tx,
        );
    }

    #[tokio::test]
    async fn remote_routes_require_a_remote_session_cookie() {
        let state = app_state("remote-auth-required");
        let app = remote_app(&state);

        assert_eq!(
            remote_status_code(&app, Method::GET, "/api/status", None).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            remote_status_code(
                &app,
                Method::GET,
                "/api/status",
                Some(&remote_cookie(&state))
            )
            .await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn remote_browser_can_change_only_its_own_stream_settings() {
        let state = app_state("remote-browser-stream-settings");
        let browser_zone = "browser-remote-stream-settings";
        register_browser_zone(&state, browser_zone);
        let cookie = remote_cookie(&state);
        let app = remote_app(&state);

        let mut request = remote_request(
            Method::POST,
            &format!("/api/zones/{browser_zone}/settings"),
            Some(&cookie),
            None,
            Some(serde_json::json!({
                "icon": "computer",
                "browser_stream": { "format": "opus", "opus_kbps": 320 }
            })),
        );
        request.headers_mut().insert(
            identity::BROWSER_ZONE_HEADER,
            header::HeaderValue::from_static(browser_zone),
        );
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let settings: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            settings
                .pointer("/browser_stream/format")
                .and_then(|v| v.as_str()),
            Some("opus")
        );
        assert_eq!(
            settings
                .pointer("/browser_stream/opus_kbps")
                .and_then(|v| v.as_u64()),
            Some(320)
        );

        let mut device_type_request = remote_request(
            Method::POST,
            &format!("/api/zones/{browser_zone}/settings"),
            Some(&cookie),
            None,
            Some(serde_json::json!({ "device_type": "hegel" })),
        );
        device_type_request.headers_mut().insert(
            identity::BROWSER_ZONE_HEADER,
            header::HeaderValue::from_static(browser_zone),
        );
        assert_eq!(
            app.clone()
                .oneshot(device_type_request)
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );

        let mut server_zone_request = remote_request(
            Method::POST,
            "/api/zones/local-core/settings",
            Some(&cookie),
            None,
            Some(serde_json::json!({ "icon": "computer" })),
        );
        server_zone_request.headers_mut().insert(
            identity::BROWSER_ZONE_HEADER,
            header::HeaderValue::from_static(browser_zone),
        );
        assert_eq!(
            app.oneshot(server_zone_request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn remote_auth_ignores_the_lan_auth_required_flag() {
        // auth_required=false must still yield 401 without a remote cookie.
        let state = app_state_with_pairing("remote-auth-flag-independent", false, false);
        assert!(!state.pairing().auth_required());
        let app = remote_app(&state);

        assert_eq!(
            remote_status_code(&app, Method::GET, "/api/status", None).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn lan_control_cookies_are_rejected_on_the_remote_listener() {
        let state = app_state("remote-rejects-lan-cookie");
        let control = state.pairing().create_control_session(None).unwrap().token;
        let app = remote_app(&state);

        let lan_cookie = format!("{}={}", crate::zones::CONTROL_SESSION_COOKIE, control);
        assert_eq!(
            remote_status_code(&app, Method::GET, "/api/status", Some(&lan_cookie)).await,
            StatusCode::UNAUTHORIZED
        );

        // A LAN control token stuffed into the remote cookie name fails the
        // remote scope check too.
        let spoofed = format!("{}={}", crate::zones::REMOTE_SESSION_COOKIE, control);
        assert_eq!(
            remote_status_code(&app, Method::GET, "/api/status", Some(&spoofed)).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn header_bearer_and_query_tokens_are_rejected_on_the_remote_listener() {
        let state = app_state("remote-rejects-header-tokens");
        let token = state.pairing().create_remote_session(None).unwrap().token;
        let app = remote_app(&state);

        let mut header_request = remote_request(Method::GET, "/api/status", None, None, None);
        header_request
            .headers_mut()
            .insert(identity::AUTH_HEADER, token.parse().unwrap());
        assert_eq!(
            app.clone().oneshot(header_request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );

        let mut bearer_request = remote_request(Method::GET, "/api/status", None, None, None);
        bearer_request.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        assert_eq!(
            app.clone().oneshot(bearer_request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );

        assert_eq!(
            remote_status_code(
                &app,
                Method::GET,
                &format!("/api/status?token={}", urlencoding::encode(&token)),
                None,
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn link_codes_are_not_valid_session_cookies() {
        let state = app_state("remote-link-code-not-cookie");
        let code = state.pairing().create_remote_link_code(None).unwrap().token;
        let app = remote_app(&state);

        let cookie = format!("{}={}", crate::zones::REMOTE_SESSION_COOKIE, code);
        assert_eq!(
            remote_status_code(&app, Method::GET, "/api/status", Some(&cookie)).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn excluded_routes_return_404_even_with_a_valid_remote_cookie() {
        let state = app_state("remote-allowlist-404");
        let cookie = remote_cookie(&state);
        let app = remote_app(&state);

        let excluded = [
            (Method::POST, "/api/pairing/start"),
            (Method::POST, "/api/agents/token"),
            (Method::POST, "/api/pairing/revoke-all"),
            (Method::GET, "/api/library/folders"),
            (Method::POST, "/api/library/albums/1/versions/qobuz"),
            (Method::POST, "/api/library/albums/1/versions/1/primary"),
            (Method::POST, "/api/library/albums/1/qobuz/match"),
            (Method::POST, "/api/library/albums/1/qobuz/link"),
            (Method::POST, "/api/library/albums/1/qobuz/unlink"),
            (Method::POST, "/api/library/albums/1/qobuz/credits/refresh"),
            (Method::GET, "/api/library/qobuz-albums/1"),
            (Method::POST, "/api/library/albums/1/match"),
            (Method::POST, "/api/library/albums/1/metabrainz/test"),
            (Method::POST, "/api/library/albums/1/metabrainz/qobuz/test"),
            (Method::POST, "/api/library/autometa/run"),
            (Method::GET, "/api/library/autometa/progress"),
            (Method::GET, "/api/library/autometa/status"),
            (Method::POST, "/api/library/autometa/jobs"),
            (Method::POST, "/api/library/autometa/jobs/1/pause"),
            (Method::POST, "/api/library/autometa/jobs/1/resume"),
            (Method::POST, "/api/library/autometa/jobs/1/stop"),
            (Method::GET, "/api/library/autometa/jobs/1/items"),
            (Method::GET, "/api/library/autometa/audit"),
            (Method::POST, "/api/library/albums/1/reset"),
            (Method::POST, "/api/library/albums/1/mark-reviewed"),
            (Method::POST, "/api/library/albums/1/match/search"),
            (Method::POST, "/api/library/albums/1/match/mbid"),
            (Method::GET, "/api/library/albums/1/candidates/1/preview"),
            (Method::POST, "/api/library/albums/1/cover"),
            (Method::POST, "/api/library/albums/1/art/refresh"),
            (Method::GET, "/api/library/rescan/status"),
            (Method::POST, "/api/library/rescan"),
            (Method::POST, "/api/upload"),
            (Method::GET, "/api/hegel/status"),
            (Method::POST, "/api/zones/local-core/hegel/status"),
            (Method::POST, "/api/zones/local-core/device-volume"),
            (Method::POST, "/api/device-volume"),
            (Method::POST, "/api/qobuz/login"),
            (Method::POST, "/api/qobuz/init"),
            (Method::POST, "/api/qobuz/logout"),
            (Method::GET, "/api/qobuz/oauth/start"),
            (Method::GET, "/api/qobuz/oauth/callback"),
            (Method::GET, "/api/qobuz/settings"),
            (Method::POST, "/api/qobuz/cache/clear"),
            (Method::GET, "/api/diagnostics/errors"),
            (Method::GET, "/sonos/stream/asset-1"),
            (Method::GET, "/upnp/stream/asset-1"),
            (Method::GET, "/api/agent/ws"),
            (Method::GET, "/api/remote/settings"),
            (Method::POST, "/api/remote/settings"),
            (Method::POST, "/api/remote/link-code"),
            (Method::GET, "/api/remote/sessions"),
            (Method::POST, "/api/remote/sessions/session-id/revoke"),
        ];
        for (method, path) in excluded {
            let status = app
                .clone()
                .oneshot(remote_request(
                    method.clone(),
                    path,
                    Some(&cookie),
                    None,
                    None,
                ))
                .await
                .expect("router should respond")
                .status();
            assert_eq!(
                status,
                StatusCode::NOT_FOUND,
                "{method} {path} must be absent (404) on the remote router"
            );
        }

        let status = app
            .clone()
            .oneshot(remote_request(
                Method::PUT,
                "/api/library/albums/1",
                Some(&cookie),
                None,
                None,
            ))
            .await
            .expect("router should respond")
            .status();
        assert_eq!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "remote album details expose GET but must not expose PUT edits"
        );

        let status = app
            .clone()
            .oneshot(remote_request(
                Method::POST,
                "/api/library/art/refresh",
                Some(&cookie),
                None,
                None,
            ))
            .await
            .expect("router should respond")
            .status();
        assert_eq!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "remote library art serves GET reads but must not expose POST refreshes"
        );
    }

    #[tokio::test]
    async fn remote_album_detail_redacts_local_version_provider_ids() {
        let state = app_state("remote-album-detail-redacts-local-provider-id");
        let root = state.music_dir().join("remote-album-sensitive-path");
        let music_dir = root.join("Users").join("alice").join("Private Music");
        let album_dir = music_dir.join("Sensitive Artist").join("Quiet Album");
        std::fs::create_dir_all(&album_dir).unwrap();
        std::fs::write(album_dir.join("01 First Track.wav"), b"not a real wav").unwrap();
        state.library().set_music_dirs(vec![music_dir]);
        state.library().scan().unwrap();
        let album_id = state.library().albums().unwrap()[0].id;
        let detail = state.library().album_detail(album_id).unwrap().unwrap();
        let local_provider_id = detail
            .versions
            .iter()
            .find(|version| version.provider == "local")
            .expect("local version should exist")
            .provider_id
            .clone();
        assert!(local_provider_id.contains("alice"));
        assert!(local_provider_id.contains("private music"));

        let cookie = remote_cookie(&state);
        let app = remote_app(&state);
        let response = app
            .oneshot(remote_request(
                Method::GET,
                &format!("/api/library/albums/{album_id}"),
                Some(&cookie),
                None,
                None,
            ))
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("response body should collect");
        let body_text = String::from_utf8(body.to_vec()).expect("album detail should be UTF-8");
        let json: serde_json::Value =
            serde_json::from_str(&body_text).expect("album detail should be JSON");
        let local_version = json["versions"]
            .as_array()
            .expect("versions should be an array")
            .iter()
            .find(|version| version["provider"].as_str() == Some("local"))
            .expect("remote detail should include local version metadata");

        assert_eq!(
            local_version["id"].as_i64(),
            detail
                .versions
                .iter()
                .find(|v| v.provider == "local")
                .map(|v| v.id)
        );
        assert!(local_version.get("provider_id").is_none());
        assert!(!body_text.contains(&local_provider_id));
        assert!(!body_text.contains("alice"));
        assert!(!body_text.contains("Private Music"));
        assert!(!body_text.contains("users alice"));
    }

    #[tokio::test]
    async fn remote_stream_routes_require_a_remote_session_cookie() {
        // Streams must be gated by remote session auth independent of the LAN
        // `auth_required()` flag (which is off for plain `app_state`).
        let state = app_state("remote-stream-auth");
        assert!(!state.pairing().auth_required());
        let app = remote_app(&state);

        for path in ["/api/stream/local/1", "/api/stream/qobuz/1"] {
            assert_eq!(
                remote_status_code(&app, Method::GET, path, None).await,
                StatusCode::UNAUTHORIZED,
                "{path} must reject missing remote session cookies"
            );

            // A valid LAN control-session cookie is not a remote credential.
            let control = state.pairing().create_control_session(None).unwrap().token;
            let lan_cookie = format!("{}={}", crate::zones::CONTROL_SESSION_COOKIE, control);
            assert_eq!(
                remote_status_code(&app, Method::GET, path, Some(&lan_cookie)).await,
                StatusCode::UNAUTHORIZED,
                "{path} must reject LAN control cookies"
            );
        }
    }

    #[tokio::test]
    async fn remote_stream_serves_local_tracks_with_a_valid_remote_session() {
        let state = app_state("remote-stream-local-ok");
        let media_path = std::env::temp_dir().join("fozmo-remote-stream-local-ok.flac");
        tokio::fs::write(&media_path, b"0123456789").await.unwrap();
        let track_id = state.library().insert_track_for_test(&media_path);
        let cookie = remote_cookie(&state);
        let app = remote_app(&state);
        let path = format!("/api/stream/local/{track_id}");

        let full = app
            .clone()
            .oneshot(remote_request(
                Method::GET,
                &path,
                Some(&cookie),
                None,
                None,
            ))
            .await
            .expect("router should respond");
        assert_eq!(full.status(), StatusCode::OK);

        let mut range_request = remote_request(Method::GET, &path, Some(&cookie), None, None);
        range_request
            .headers_mut()
            .insert(header::RANGE, header::HeaderValue::from_static("bytes=5-"));
        let partial = app.clone().oneshot(range_request).await.unwrap();
        assert_eq!(partial.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            partial
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok()),
            Some("bytes 5-9/10")
        );

        // Revoking every remote session locks the stream out again.
        for session in state.pairing().list_remote_sessions().unwrap() {
            state
                .pairing()
                .revoke_remote_session_by_id(&session.id)
                .unwrap();
        }
        assert_eq!(
            remote_status_code(&app, Method::GET, &path, Some(&cookie)).await,
            StatusCode::UNAUTHORIZED
        );

        let _ = tokio::fs::remove_file(media_path).await;
    }

    #[tokio::test]
    async fn remote_qobuz_stream_passes_auth_with_a_valid_remote_session() {
        let state = app_state("remote-stream-qobuz-auth");
        let cookie = remote_cookie(&state);
        let app = remote_app(&state);

        // Not logged in to Qobuz, so the proxy fails after auth: the status
        // must be a server-side error, never an auth rejection or a 404 from
        // the allowlist.
        let status =
            remote_status_code(&app, Method::GET, "/api/stream/qobuz/1", Some(&cookie)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn remote_session_exchange_issues_a_strict_secure_cookie() {
        let state = app_state("remote-session-exchange");
        let code = state.pairing().create_remote_link_code(None).unwrap().token;
        let app = remote_app(&state);

        let mut request = remote_request(
            Method::POST,
            "/api/remote/session",
            None,
            Some(SocketAddr::from(([203, 0, 113, 203], 49152))),
            Some(serde_json::json!({ "code": code })),
        );
        request.headers_mut().insert(
            header::USER_AGENT,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1"
                .parse()
                .unwrap(),
        );

        let response = app
            .clone()
            .oneshot(request)
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .expect("session exchange should set a cookie")
            .to_string();
        assert!(cookie.contains(crate::zones::REMOTE_SESSION_COOKIE));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("SameSite=Strict"));

        let cookie_pair = cookie.split(';').next().unwrap().to_string();
        assert_eq!(
            remote_status_code(&app, Method::GET, "/api/status", Some(&cookie_pair)).await,
            StatusCode::OK
        );
        let sessions = state.pairing().list_remote_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].label, "iPhone · Safari");
        let client = sessions[0].client.as_ref().expect("client metadata");
        assert_eq!(client.device_family.as_deref(), Some("iPhone"));
        assert_eq!(client.browser.as_deref(), Some("Safari"));
        assert_eq!(client.os.as_deref(), Some("iOS"));
        assert_eq!(
            client.network_hint.as_deref(),
            Some("public IPv4 ending .203")
        );

        // Single use: replaying the consumed code returns a generic 401.
        let reuse = app
            .clone()
            .oneshot(remote_request(
                Method::POST,
                "/api/remote/session",
                None,
                None,
                Some(serde_json::json!({ "code": code })),
            ))
            .await
            .expect("router should respond")
            .status();
        assert_eq!(reuse, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn remote_status_reports_remote_surface_after_auth() {
        let state = app_state("remote-status-surface");
        let cookie = remote_cookie(&state);
        let app = remote_app(&state);

        let response = app
            .oneshot(remote_request(
                Method::GET,
                "/api/status",
                Some(&cookie),
                None,
                None,
            ))
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("response body should collect");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("status JSON");

        assert_eq!(
            json.get("surface").and_then(|value| value.as_str()),
            Some("remote")
        );
    }

    #[tokio::test]
    async fn remote_browser_cannot_issue_link_codes() {
        let state = app_state("remote-link-code-capability");
        let cookie = remote_cookie(&state);
        let app = remote_app(&state);

        let response = app
            .oneshot(remote_request(
                Method::GET,
                "/api/remote/status",
                Some(&cookie),
                None,
                None,
            ))
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            json.get("link_code_issuance")
                .and_then(|value| value.as_str()),
            Some("unavailable")
        );
    }

    #[tokio::test]
    async fn remote_zone_status_reports_remote_surface_after_auth() {
        let state = app_state("remote-zone-status-surface");
        let cookie = remote_cookie(&state);
        let app = remote_app(&state);

        let response = app
            .oneshot(remote_request(
                Method::GET,
                "/api/zones/local-core/status",
                Some(&cookie),
                None,
                None,
            ))
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("response body should collect");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("status JSON");

        assert_eq!(
            json.get("surface").and_then(|value| value.as_str()),
            Some("remote")
        );
    }

    #[tokio::test]
    async fn repeated_auth_failures_rate_limit_by_peer_ip() {
        let state = app_state("remote-rate-limit");
        let app = remote_app(&state);
        let peer = SocketAddr::from(([203, 0, 113, 9], 51000));

        let mut saw_429 = false;
        for _ in 0..12 {
            let response = app
                .clone()
                .oneshot(remote_request(
                    Method::POST,
                    "/api/remote/session",
                    None,
                    Some(peer),
                    Some(serde_json::json!({ "code": "not-a-code" })),
                ))
                .await
                .expect("router should respond");
            if response.status() == StatusCode::TOO_MANY_REQUESTS {
                assert!(
                    response.headers().contains_key(header::RETRY_AFTER),
                    "429 must carry Retry-After"
                );
                saw_429 = true;
                break;
            }
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        assert!(saw_429, "repeated failures should trigger 429");

        // A different peer is unaffected.
        let other = SocketAddr::from(([198, 51, 100, 7], 51000));
        let response = app
            .clone()
            .oneshot(remote_request(
                Method::POST,
                "/api/remote/session",
                None,
                Some(other),
                Some(serde_json::json!({ "code": "not-a-code" })),
            ))
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // Bad cookies on API routes hit the same limiter.
        let locked = app
            .clone()
            .oneshot(remote_request(
                Method::GET,
                "/api/status",
                Some("fozmo_remote_session=bogus"),
                Some(peer),
                None,
            ))
            .await
            .expect("router should respond");
        assert_eq!(locked.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn remote_responses_carry_security_headers_and_no_cors() {
        let state = app_state("remote-security-headers");
        let cookie = remote_cookie(&state);
        let app = remote_app(&state);

        let mut request = remote_request(Method::GET, "/api/status", Some(&cookie), None, None);
        request.headers_mut().insert(
            header::ORIGIN,
            header::HeaderValue::from_static("https://evil.test"),
        );
        let response = app.clone().oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers();
        assert_eq!(
            headers
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .and_then(|v| v.to_str().ok()),
            Some("nosniff")
        );
        assert_eq!(
            headers
                .get(header::REFERRER_POLICY)
                .and_then(|v| v.to_str().ok()),
            Some("no-referrer")
        );
        let csp = headers
            .get(header::CONTENT_SECURITY_POLICY)
            .and_then(|v| v.to_str().ok())
            .expect("remote responses must carry a CSP");
        assert!(csp.contains("default-src 'self'"));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(
            !headers.contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            "no CORS layer may be installed on the remote router"
        );
        // Self-signed identity: no HSTS.
        assert!(!headers.contains_key(header::STRICT_TRANSPORT_SECURITY));
    }

    #[tokio::test]
    async fn hsts_is_sent_only_for_custom_certificates() {
        let state = app_state("remote-hsts-custom");
        let cookie = remote_cookie(&state);
        let paths =
            AppPaths::from_workspace_dir(std::env::temp_dir().join("fozmo-remote-hsts-tests"));
        let app = build_remote_app(state.clone(), &paths, true);

        let response = app
            .oneshot(remote_request(
                Method::GET,
                "/api/status",
                Some(&cookie),
                None,
                None,
            ))
            .await
            .unwrap();

        assert_eq!(
            response
                .headers()
                .get(header::STRICT_TRANSPORT_SECURITY)
                .and_then(|v| v.to_str().ok()),
            Some("max-age=63072000")
        );
    }

    #[tokio::test]
    async fn remote_websocket_requires_cookie_auth_before_upgrade() {
        let state = app_state("remote-ws-auth");
        let app = remote_app(&state);

        // No cookie: rejected by the auth middleware before any upgrade.
        assert_eq!(
            remote_status_code(&app, Method::GET, "/api/ws", None).await,
            StatusCode::UNAUTHORIZED
        );

        // Header token fallback is not accepted remotely either.
        let token = state.pairing().create_remote_session(None).unwrap().token;
        let mut header_request = remote_request(Method::GET, "/api/ws", None, None, None);
        header_request
            .headers_mut()
            .insert(identity::AUTH_HEADER, token.parse().unwrap());
        assert_eq!(
            app.clone().oneshot(header_request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );

        // With a valid cookie the request passes auth and fails only on the
        // missing websocket upgrade headers.
        let cookie = remote_cookie(&state);
        let status = remote_status_code(&app, Method::GET, "/api/ws", Some(&cookie)).await;
        assert_ne!(status, StatusCode::UNAUTHORIZED);
        assert_ne!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn controller_apply_reports_tls_errors_without_crashing() {
        let state = app_state("remote-controller-tls-error");
        let _ = state.settings().update(|persisted| {
            persisted.remote_access = crate::settings::RemoteAccessSettings {
                enabled: true,
                port: 39443,
                external_host: None,
                custom_cert_path: Some("/nonexistent/cert.pem".to_string()),
                custom_key_path: Some("/nonexistent/key.pem".to_string()),
            };
        });

        let status = state.remote_access().apply(&state).await;

        assert!(status.enabled);
        assert!(!status.running);
        assert!(status.last_error.is_some());
    }

    #[tokio::test]
    async fn controller_validates_persisted_remote_settings_before_start() {
        let state = app_state("remote-controller-invalid-settings");
        let _ = state.settings().update(|persisted| {
            persisted.remote_access = crate::settings::RemoteAccessSettings {
                enabled: true,
                port: 39444,
                custom_cert_path: Some("/tmp/cert.pem".to_string()),
                custom_key_path: None,
                external_host: None,
            };
        });

        let status = state.remote_access().apply(&state).await;

        assert!(status.enabled);
        assert!(!status.running);
        assert!(
            status
                .last_error
                .as_deref()
                .is_some_and(|message| message.contains("configured together"))
        );
    }

    #[tokio::test]
    async fn controller_starts_and_stops_the_listener_at_runtime() {
        let state = app_state("remote-controller-lifecycle");
        // Reserve an ephemeral port, then release it for the controller.
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let _ = state.settings().update(|persisted| {
            persisted.remote_access = crate::settings::RemoteAccessSettings {
                enabled: true,
                port,
                ..crate::settings::RemoteAccessSettings::default()
            };
        });
        let started = state.remote_access().apply(&state).await;
        assert!(started.running, "listener should start: {started:?}");
        assert!(started.cert_fingerprint_sha256.is_some());

        // Plaintext HTTP against the TLS-only port fails the handshake.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream
            .write_all(b"GET /api/status HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut buffer = Vec::new();
        let read =
            tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buffer)).await;
        let plaintext_response = String::from_utf8_lossy(&buffer);
        assert!(
            !plaintext_response.contains("HTTP/1.1 200"),
            "plaintext request must not receive an HTTP response: {read:?}"
        );

        let _ = state.settings().update(|persisted| {
            persisted.remote_access.enabled = false;
        });
        let stopped = state.remote_access().apply(&state).await;
        assert!(!stopped.running);
        assert!(stopped.last_error.is_none());
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err(),
            "remote port should stop listening after disable"
        );
    }
}
