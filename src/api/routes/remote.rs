//! Remote access management endpoints.
//!
//! Settings mutation, link-code issuance, and remote-session administration
//! live on the LAN/local router only, guarded like `/api/pairing/start`: a
//! local filesystem request or an authenticated LAN control session. The
//! remote listener carries exactly one
//! unauthenticated route, `POST /api/remote/session`, which exchanges a
//! single-use high-entropy link code for a remote session cookie and is
//! rate-limited per peer IP.

use super::internal_response;
use crate::app::server_remote::{RemoteAccessStatus, RemoteLinkCodeIssuance};
use crate::app::state::AppState;
use crate::settings::RemoteSessionClientMetadata;
use axum::{
    Json, Router,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RemoteAccessSettingsDto {
    pub enabled: bool,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_cert_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_key_path: Option<String>,
}

impl From<crate::settings::RemoteAccessSettings> for RemoteAccessSettingsDto {
    fn from(settings: crate::settings::RemoteAccessSettings) -> Self {
        Self {
            enabled: settings.enabled,
            port: settings.port,
            external_host: settings.external_host,
            custom_cert_path: settings.custom_cert_path,
            custom_key_path: settings.custom_key_path,
        }
    }
}

#[derive(Serialize, JsonSchema)]
pub struct RemoteAccessSettingsResponse {
    pub settings: RemoteAccessSettingsDto,
    pub status: RemoteAccessStatus,
}

#[derive(Deserialize, JsonSchema)]
pub struct RemoteAccessSettingsUpdateRequest {
    pub enabled: bool,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub external_host: Option<String>,
    #[serde(default)]
    pub custom_cert_path: Option<String>,
    #[serde(default)]
    pub custom_key_path: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct RemoteLinkCodeResponse {
    pub code: String,
    pub expires_at_unix_secs: u64,
    /// Display-only convenience URL derived from `external_host`; never an
    /// input to auth decisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_hint: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct RemoteSessionMetadataDto {
    pub id: String,
    pub label: String,
    pub issued_at_unix_secs: u64,
    pub expires_at_unix_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at_unix_secs: Option<u64>,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<RemoteSessionClientMetadata>,
}

impl From<crate::zones::RemoteSessionMetadata> for RemoteSessionMetadataDto {
    fn from(session: crate::zones::RemoteSessionMetadata) -> Self {
        Self {
            id: session.id,
            label: session.label,
            issued_at_unix_secs: session.issued_at_unix_secs,
            expires_at_unix_secs: session.expires_at_unix_secs,
            last_used_at_unix_secs: session.last_used_at_unix_secs,
            active: session.active,
            client: session.client,
        }
    }
}

#[derive(Serialize, JsonSchema)]
pub struct RemoteSessionsResponse {
    pub sessions: Vec<RemoteSessionMetadataDto>,
}

#[derive(Serialize, JsonSchema)]
pub struct RemoteSessionRevocationResponse {
    pub revoked: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct RemoteSessionRequest {
    pub code: String,
}

#[derive(Serialize, JsonSchema)]
pub struct RemoteSessionResponse {
    pub expires_at_unix_secs: u64,
    pub token_kind: String,
    pub scopes: Vec<String>,
}

/// LAN/local router only. Must never be merged into the remote router.
pub fn routes() -> Router<AppState> {
    read_only_routes()
        .route(
            "/api/remote/settings",
            get(get_remote_settings).post(update_remote_settings),
        )
        .route("/api/remote/link-code", post(create_remote_link_code))
        .route("/api/remote/sessions", get(list_remote_sessions))
        .route(
            "/api/remote/sessions/:id/revoke",
            post(revoke_remote_session),
        )
}

/// Read-only listener state available on both authenticated control surfaces.
/// This exposes no settings mutation or session credentials.
pub fn read_only_routes() -> Router<AppState> {
    Router::new().route("/api/remote/status", get(get_remote_status))
}

/// Remote router only: unauthenticated, rate-limited session exchange.
pub fn remote_session_routes() -> Router<AppState> {
    Router::new().route("/api/remote/session", post(remote_session_exchange))
}

/// Same guard posture as `/api/pairing/start` and folder management: local
/// filesystem request or authenticated LAN control session.
fn local_or_control_authorized(
    state: &AppState,
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
) -> bool {
    remote_link_code_issuance(state, headers, peer) != RemoteLinkCodeIssuance::Unavailable
}

fn remote_link_code_issuance(
    state: &AppState,
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
) -> RemoteLinkCodeIssuance {
    if crate::app::auth::local_filesystem_request_allowed(headers, peer) {
        return RemoteLinkCodeIssuance::HostLocal;
    }
    let cookie_token = crate::app::auth::control_session_token_from_headers(headers);
    let header_token = super::auth_token_from_headers(headers);
    if cookie_token
        .as_deref()
        .is_some_and(|token| state.pairing().verify_control_token(Some(token)))
        || header_token
            .as_deref()
            .is_some_and(|token| state.pairing().verify_control_token(Some(token)))
    {
        RemoteLinkCodeIssuance::AuthenticatedLan
    } else {
        RemoteLinkCodeIssuance::Unavailable
    }
}

fn status_for_request(
    state: &AppState,
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
) -> RemoteAccessStatus {
    let mut status = state.remote_access().status(state);
    status.link_code_issuance = remote_link_code_issuance(state, headers, peer);
    status
}

async fn get_remote_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
) -> Json<RemoteAccessSettingsResponse> {
    let peer = peer.map(|ConnectInfo(addr)| addr);
    Json(RemoteAccessSettingsResponse {
        settings: state.settings().remote_access_settings().into(),
        status: status_for_request(&state, &headers, peer),
    })
}

async fn get_remote_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
) -> Json<RemoteAccessStatus> {
    let peer = peer.map(|ConnectInfo(addr)| addr);
    Json(status_for_request(&state, &headers, peer))
}

async fn update_remote_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
    Json(request): Json<RemoteAccessSettingsUpdateRequest>,
) -> Result<Json<RemoteAccessSettingsResponse>, Response> {
    let peer = peer.map(|ConnectInfo(addr)| addr);
    if !local_or_control_authorized(&state, &headers, peer) {
        warn!(
            event = "remote_settings_update",
            status = "forbidden",
            error_kind = "forbidden",
            "Remote access settings update rejected"
        );
        return Err(StatusCode::FORBIDDEN.into_response());
    }

    let current = state.settings().remote_access_settings();
    let mut updated = crate::settings::RemoteAccessSettings {
        enabled: request.enabled,
        port: request.port.unwrap_or(current.port),
        external_host: request.external_host.or(current.external_host),
        custom_cert_path: request.custom_cert_path.or(current.custom_cert_path),
        custom_key_path: request.custom_key_path.or(current.custom_key_path),
    };
    crate::settings::validate_remote_access(&mut updated, state.remote_access().app_port())
        .map_err(|message| (StatusCode::BAD_REQUEST, message).into_response())?;

    state
        .settings()
        .try_update(|persisted| {
            persisted.remote_access = updated.clone();
        })
        .map_err(internal_response)?;
    let mut status = state.remote_access().apply(&state).await;
    status.link_code_issuance = remote_link_code_issuance(&state, &headers, peer);
    info!(
        event = "remote_settings_update",
        status = "ok",
        enabled = updated.enabled,
        port = updated.port,
        running = status.running,
        "Remote access settings applied"
    );
    Ok(Json(RemoteAccessSettingsResponse {
        settings: state.settings().remote_access_settings().into(),
        status,
    }))
}

async fn create_remote_link_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
) -> Result<Json<RemoteLinkCodeResponse>, Response> {
    let peer = peer.map(|ConnectInfo(addr)| addr);
    if !local_or_control_authorized(&state, &headers, peer) {
        warn!(
            event = "remote_link_code",
            status = "forbidden",
            error_kind = "forbidden",
            "Remote link-code issuance rejected"
        );
        return Err(StatusCode::FORBIDDEN.into_response());
    }
    let issued = state
        .pairing()
        .create_remote_link_code(None)
        .map_err(internal_response)?;
    let settings = state.settings().remote_access_settings();
    let url_hint = settings
        .external_host
        .as_deref()
        .map(|host| format!("https://{host}:{}/", settings.port));
    info!(
        event = "remote_link_code",
        status = "ok",
        expires_at_unix_secs = issued.expires_at_unix_secs,
        "Remote link code issued"
    );
    Ok(Json(RemoteLinkCodeResponse {
        code: issued.token,
        expires_at_unix_secs: issued.expires_at_unix_secs,
        url_hint,
    }))
}

async fn list_remote_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
) -> Result<Json<RemoteSessionsResponse>, Response> {
    let peer = peer.map(|ConnectInfo(addr)| addr);
    if !local_or_control_authorized(&state, &headers, peer) {
        warn!(
            event = "remote_sessions_list",
            status = "forbidden",
            error_kind = "forbidden",
            "Remote session listing rejected"
        );
        return Err(StatusCode::FORBIDDEN.into_response());
    }
    let sessions = state
        .pairing()
        .list_remote_sessions()
        .map_err(internal_response)?
        .into_iter()
        .map(RemoteSessionMetadataDto::from)
        .collect();
    Ok(Json(RemoteSessionsResponse { sessions }))
}

async fn revoke_remote_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<RemoteSessionRevocationResponse>, Response> {
    let peer = peer.map(|ConnectInfo(addr)| addr);
    if !local_or_control_authorized(&state, &headers, peer) {
        warn!(
            event = "remote_session_revoke",
            status = "forbidden",
            error_kind = "forbidden",
            "Remote session revocation rejected"
        );
        return Err(StatusCode::FORBIDDEN.into_response());
    }
    let revoked = state
        .pairing()
        .revoke_remote_session_by_id(&id)
        .map_err(internal_response)?;
    Ok(Json(RemoteSessionRevocationResponse { revoked }))
}

async fn remote_session_exchange(
    State(state): State<AppState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Json(request): Json<RemoteSessionRequest>,
) -> Result<Response, Response> {
    let peer_ip = peer.as_ref().map(|ConnectInfo(addr)| addr.ip());
    if let Some(ip) = peer_ip
        && let Some(remaining) = state.remote_auth_limiter().lockout_remaining(ip)
    {
        warn!(
            event = "remote_session_exchange",
            status = "rate_limited",
            error_kind = "rate_limit",
            peer_ip = %ip,
            "Remote session exchange rejected during lockout"
        );
        return Err(crate::app::auth::rate_limited_response(remaining));
    }

    let consumed = state
        .pairing()
        .consume_remote_link_code(Some(&request.code))
        .map_err(internal_response)?;
    if !consumed {
        if let Some(ip) = peer_ip {
            state.remote_auth_limiter().record_failure(ip);
        }
        // Generic 401 for invalid, expired, and reused codes alike; the code
        // value is never logged.
        warn!(
            event = "remote_session_exchange",
            status = "error",
            error_kind = "auth",
            peer_ip = ?peer_ip,
            "Remote session exchange failed"
        );
        return Err(StatusCode::UNAUTHORIZED.into_response());
    }

    let client_metadata = remote_session_client_metadata(&headers, peer_ip);
    let subject = Some(remote_session_display_label(&client_metadata));
    let issued = state
        .pairing()
        .create_remote_session_with_metadata(subject, Some(client_metadata))
        .map_err(internal_response)?;
    if let Some(ip) = peer_ip {
        state.remote_auth_limiter().record_success(ip);
    }
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::SET_COOKIE,
        remote_session_cookie(&issued.token, issued.expires_at_unix_secs)
            .parse()
            .map_err(|error| internal_response(format!("create remote session cookie: {error}")))?,
    );
    info!(
        event = "remote_session_exchange",
        status = "ok",
        expires_at_unix_secs = issued.expires_at_unix_secs,
        "Remote session issued"
    );
    Ok((
        response_headers,
        Json(RemoteSessionResponse {
            expires_at_unix_secs: issued.expires_at_unix_secs,
            token_kind: "remote_session".to_string(),
            scopes: vec![crate::zones::SCOPE_REMOTE.to_string()],
        }),
    )
        .into_response())
}

fn remote_session_client_metadata(
    headers: &HeaderMap,
    peer_ip: Option<IpAddr>,
) -> RemoteSessionClientMetadata {
    let user_agent = header_value(headers, header::USER_AGENT.as_str());
    let platform_hint = header_value(headers, "sec-ch-ua-platform");
    let brand_hint = header_value(headers, "sec-ch-ua");

    let device_family = device_family_from_headers(user_agent, platform_hint);
    let browser = browser_from_headers(user_agent, brand_hint);
    let os = os_from_headers(user_agent, platform_hint);
    let (network_hint, ip_family) = peer_ip
        .map(|ip| {
            (
                Some(network_hint_for_ip(ip)),
                Some(ip_family(ip).to_string()),
            )
        })
        .unwrap_or((None, None));

    RemoteSessionClientMetadata {
        device_family,
        browser,
        os,
        network_hint,
        ip_family,
    }
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)?
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn remote_session_display_label(metadata: &RemoteSessionClientMetadata) -> String {
    let device = metadata
        .device_family
        .as_deref()
        .unwrap_or("Unknown device");
    match metadata.browser.as_deref() {
        Some(browser) if !browser.is_empty() => format!("{device} · {browser}"),
        _ => device.to_string(),
    }
}

fn device_family_from_headers(
    user_agent: Option<&str>,
    platform_hint: Option<&str>,
) -> Option<String> {
    let ua = user_agent.unwrap_or_default();
    let platform = platform_hint.unwrap_or_default().trim_matches('"');
    let lower_ua = ua.to_ascii_lowercase();
    let lower_platform = platform.to_ascii_lowercase();
    let device = if lower_ua.contains("iphone") {
        "iPhone"
    } else if lower_ua.contains("ipad") || lower_platform == "ipados" {
        "iPad"
    } else if lower_ua.contains("android") || lower_platform == "android" {
        "Android"
    } else if lower_ua.contains("macintosh") || lower_platform == "macos" {
        "Mac"
    } else if lower_ua.contains("windows") || lower_platform == "windows" {
        "Windows PC"
    } else if lower_ua.contains("cros") || lower_platform == "chrome os" {
        "Chromebook"
    } else if lower_ua.contains("linux") || lower_platform == "linux" {
        "Linux PC"
    } else {
        "Unknown device"
    };
    Some(device.to_string())
}

fn browser_from_headers(user_agent: Option<&str>, brand_hint: Option<&str>) -> Option<String> {
    let ua = user_agent.unwrap_or_default();
    let lower = ua.to_ascii_lowercase();
    let brands = brand_hint.unwrap_or_default().to_ascii_lowercase();
    let browser = if lower.contains("edg/") || lower.contains("edge/") || brands.contains("edge") {
        "Edge"
    } else if lower.contains("crios/") || lower.contains("chrome/") || brands.contains("chrome") {
        "Chrome"
    } else if lower.contains("fxios/") || lower.contains("firefox/") || brands.contains("firefox") {
        "Firefox"
    } else if lower.contains("opr/") || lower.contains("opera") || brands.contains("opera") {
        "Opera"
    } else if lower.contains("version/") && lower.contains("safari/") {
        "Safari"
    } else {
        return None;
    };
    Some(browser.to_string())
}

fn os_from_headers(user_agent: Option<&str>, platform_hint: Option<&str>) -> Option<String> {
    let ua = user_agent.unwrap_or_default();
    let lower = ua.to_ascii_lowercase();
    let platform = platform_hint.unwrap_or_default().trim_matches('"');
    let lower_platform = platform.to_ascii_lowercase();
    let os = if lower.contains("iphone os") || lower.contains("cpu iphone os") {
        "iOS"
    } else if lower.contains("cpu os") && lower.contains("ipad") {
        "iPadOS"
    } else if lower.contains("android") || lower_platform == "android" {
        "Android"
    } else if lower.contains("mac os x") || lower_platform == "macos" {
        "macOS"
    } else if lower.contains("windows nt") || lower_platform == "windows" {
        "Windows"
    } else if lower.contains("cros") || lower_platform == "chrome os" {
        "ChromeOS"
    } else if lower.contains("linux") || lower_platform == "linux" {
        "Linux"
    } else {
        return None;
    };
    Some(os.to_string())
}

fn network_hint_for_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(addr) if addr.is_loopback() => "local machine".to_string(),
        IpAddr::V4(addr) if addr.is_private() => "home LAN".to_string(),
        IpAddr::V4(addr) if addr.is_link_local() => "link-local network".to_string(),
        IpAddr::V4(addr) => format!("public IPv4 ending .{}", addr.octets()[3]),
        IpAddr::V6(addr) if addr.is_loopback() => "local machine".to_string(),
        IpAddr::V6(addr) if addr.is_unique_local() => "home LAN".to_string(),
        IpAddr::V6(addr) if addr.is_unicast_link_local() => "link-local network".to_string(),
        IpAddr::V6(addr) => {
            let last = addr.segments().last().copied().unwrap_or_default();
            format!("public IPv6 ending :{last:x}")
        }
    }
}

fn ip_family(ip: IpAddr) -> &'static str {
    match ip {
        IpAddr::V4(_) => "IPv4",
        IpAddr::V6(_) => "IPv6",
    }
}

/// Unlike the LAN control-session cookie this never sniffs
/// `x-forwarded-proto` and is always `Secure` + `SameSite=Strict`: the remote
/// listener is TLS-only and same-origin cookies are the CSRF posture.
fn remote_session_cookie(token: &str, expires_at_unix_secs: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_secs();
    let max_age = expires_at_unix_secs.saturating_sub(now).max(1);
    format!(
        "{}={}; Path=/; Max-Age={}; HttpOnly; Secure; SameSite=Strict",
        crate::zones::REMOTE_SESSION_COOKIE,
        token,
        max_age
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::routes::create_router;
    use crate::playback::test_support::app_state;
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Method, Request};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    fn lan_app(state: &AppState) -> Router {
        create_router().with_state(state.clone())
    }

    async fn lan_request(
        app: &Router,
        method: Method,
        path: &str,
        body: Option<Value>,
        cross_site: bool,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "127.0.0.1:3000");
        if cross_site {
            builder = builder.header(header::ORIGIN, "https://evil.test");
        }
        let body = match body {
            Some(body) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let response = app
            .clone()
            .oneshot(builder.body(body).expect("request should build"))
            .await
            .expect("router should respond");
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let json = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn remote_settings_report_disabled_defaults_and_status() {
        let state = app_state("remote-settings-defaults");
        let app = lan_app(&state);

        let (status, body) =
            lan_request(&app, Method::GET, "/api/remote/settings", None, false).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body.pointer("/settings/enabled").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            body.pointer("/settings/port").and_then(Value::as_u64),
            Some(8443)
        );
        assert_eq!(
            body.pointer("/status/running").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            body.pointer("/status/active_remote_sessions")
                .and_then(Value::as_u64),
            Some(0)
        );
    }

    #[tokio::test]
    async fn read_only_remote_status_reports_the_controller_state() {
        let state = app_state("remote-read-only-status");
        let app = lan_app(&state);

        let (status, body) =
            lan_request(&app, Method::GET, "/api/remote/status", None, false).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.get("enabled").and_then(Value::as_bool), Some(false));
        assert_eq!(body.get("running").and_then(Value::as_bool), Some(false));
        assert_eq!(body.get("bound_port").and_then(Value::as_u64), Some(8443));
        assert_eq!(
            body.get("link_code_issuance").and_then(Value::as_str),
            Some("host_local")
        );
    }

    #[tokio::test]
    async fn link_code_capability_reflects_authenticated_lan_authority() {
        let state = app_state("remote-link-code-capability");
        let app = lan_app(&state);
        let control = state.pairing().create_control_session(None).unwrap().token;

        let request = |token: Option<&str>| {
            let mut builder = Request::builder()
                .method(Method::GET)
                .uri("/api/remote/status")
                .header(header::HOST, "player.lan:3000")
                .header(header::ORIGIN, "http://player.lan:3000");
            if let Some(token) = token {
                builder = builder.header(crate::app::identity::AUTH_HEADER, token);
            }
            builder.body(Body::empty()).unwrap()
        };

        for (token, expected) in [
            (None, "unavailable"),
            (Some(control.as_str()), "authenticated_lan"),
        ] {
            let response = app
                .clone()
                .oneshot(request(token))
                .await
                .expect("status request should complete");
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                json.get("link_code_issuance").and_then(Value::as_str),
                Some(expected)
            );
        }
    }

    #[tokio::test]
    async fn durable_remote_credentials_and_settings_updates_reject_cross_site_requests() {
        let state = app_state("remote-settings-cross-site");
        let app = lan_app(&state);

        for (method, path) in [
            (Method::GET, "/api/remote/settings"),
            (Method::GET, "/api/remote/status"),
        ] {
            let (status, _) = lan_request(&app, method.clone(), path, None, true).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "{method} {path} should remain available on the LAN router"
            );
        }

        for (method, path) in [
            (Method::GET, "/api/remote/sessions"),
            (Method::POST, "/api/remote/link-code"),
            (Method::POST, "/api/remote/sessions/session-id/revoke"),
        ] {
            let (status, _) = lan_request(&app, method.clone(), path, None, true).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
        }

        let (status, _) = lan_request(
            &app,
            Method::POST,
            "/api/remote/settings",
            Some(json!({ "enabled": false })),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn remote_credential_admin_requires_loopback_or_lan_control_auth() {
        let state = app_state("remote-credential-admin-boundary");
        let app = lan_app(&state);
        let control = state.pairing().create_control_session(None).unwrap().token;
        let remote = state.pairing().create_remote_session(None).unwrap().token;
        let remote_session_id = state.pairing().list_remote_sessions().unwrap()[0]
            .id
            .clone();

        let request = |method: Method, path: &str, token: Option<&str>| {
            let mut builder = Request::builder()
                .method(method)
                .uri(path)
                .header(header::HOST, "player.lan:3000")
                .header(header::ORIGIN, "http://player.lan:3000");
            if let Some(token) = token {
                builder = builder.header(crate::app::identity::AUTH_HEADER, token);
            }
            builder.body(Body::empty()).unwrap()
        };

        assert_eq!(
            app.clone()
                .oneshot(request(Method::POST, "/api/remote/link-code", None))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.clone()
                .oneshot(request(
                    Method::POST,
                    "/api/remote/link-code",
                    Some(&remote),
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.clone()
                .oneshot(request(
                    Method::POST,
                    "/api/remote/link-code",
                    Some(&control),
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            app.clone()
                .oneshot(request(Method::GET, "/api/remote/sessions", None))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.clone()
                .oneshot(request(Method::GET, "/api/remote/sessions", Some(&control),))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            app.oneshot(request(
                Method::POST,
                &format!("/api/remote/sessions/{remote_session_id}/revoke"),
                Some(&control),
            ))
            .await
            .unwrap()
            .status(),
            StatusCode::OK
        );
        assert!(!state.pairing().verify_remote_token(Some(&remote)));
    }

    #[tokio::test]
    async fn remote_settings_read_is_available_without_a_lan_control_session() {
        let state = app_state("remote-settings-reject-remote-token");
        let app = lan_app(&state);
        let remote_token = state.pairing().create_remote_session(None).unwrap().token;

        assert!(state.pairing().verify_remote_token(Some(&remote_token)));
        assert!(!state.pairing().verify_control_token(Some(&remote_token)));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/remote/settings")
                    .header(header::HOST, "lan.example:3000")
                    .header(header::ORIGIN, "https://evil.test")
                    .header(crate::app::identity::AUTH_HEADER, remote_token)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn remote_settings_update_rejects_app_port_collision() {
        let state = app_state("remote-settings-port-collision");
        let app = lan_app(&state);
        let app_port = state.remote_access().app_port();

        let (status, _) = lan_request(
            &app,
            Method::POST,
            "/api/remote/settings",
            Some(json!({ "enabled": false, "port": app_port })),
            false,
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(!state.settings().remote_access_settings().enabled);
        assert_eq!(state.settings().remote_access_settings().port, 8443);
    }

    #[tokio::test]
    async fn remote_settings_update_persists_and_normalizes() {
        let state = app_state("remote-settings-persist");
        let _ = state.settings().update(|persisted| {
            persisted.remote_access.external_host = Some("old.example.test".to_string());
        });
        let app = lan_app(&state);

        let (status, body) = lan_request(
            &app,
            Method::POST,
            "/api/remote/settings",
            Some(json!({
                "enabled": false,
                "port": 9443,
                "external_host": "  ",
                "custom_cert_path": "",
                "custom_key_path": ""
            })),
            false,
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body.pointer("/settings/port").and_then(Value::as_u64),
            Some(9443)
        );
        let persisted = state.settings().remote_access_settings();
        assert_eq!(persisted.port, 9443);
        assert!(persisted.external_host.is_none());
        assert!(persisted.custom_cert_path.is_none());
        assert!(persisted.custom_key_path.is_none());
    }

    #[tokio::test]
    async fn link_codes_are_issued_with_expiry_and_optional_url_hint() {
        let state = app_state("remote-link-code-issue");
        let _ = state.settings().update(|persisted| {
            persisted.remote_access.external_host = Some("home.example.test".to_string());
            persisted.remote_access.port = 9443;
        });
        let app = lan_app(&state);

        let (status, body) =
            lan_request(&app, Method::POST, "/api/remote/link-code", None, false).await;

        assert_eq!(status, StatusCode::OK);
        let code = body.get("code").and_then(Value::as_str).unwrap();
        assert_eq!(code.len(), 43, "link code should be a 256-bit URL token");
        assert!(
            body.get("expires_at_unix_secs")
                .and_then(Value::as_u64)
                .is_some()
        );
        assert_eq!(
            body.get("url_hint").and_then(Value::as_str),
            Some("https://home.example.test:9443/")
        );
        // The issued code is consumable exactly once.
        assert!(
            state
                .pairing()
                .consume_remote_link_code(Some(code))
                .unwrap()
        );
        assert!(
            !state
                .pairing()
                .consume_remote_link_code(Some(code))
                .unwrap()
        );
    }

    #[tokio::test]
    async fn authenticated_lan_link_code_exchanges_for_an_unlinked_remote_browser() {
        let state = app_state("remote-lan-code-exchange");
        let app = lan_app(&state);
        let control = state.pairing().create_control_session(None).unwrap().token;
        let issue_request = Request::builder()
            .method(Method::POST)
            .uri("/api/remote/link-code")
            .header(header::HOST, "player.lan:3000")
            .header(header::ORIGIN, "http://player.lan:3000")
            .header(crate::app::identity::AUTH_HEADER, control)
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(issue_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let issued: Value = serde_json::from_slice(&body).unwrap();
        let code = issued.get("code").and_then(Value::as_str).unwrap();

        let remote_exchange = remote_session_routes().with_state(state.clone());
        let exchange_request = Request::builder()
            .method(Method::POST)
            .uri("/api/remote/session")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "code": code }).to_string()))
            .unwrap();
        let exchange_response = remote_exchange.oneshot(exchange_request).await.unwrap();

        assert_eq!(exchange_response.status(), StatusCode::OK);
        assert!(exchange_response.headers().contains_key(header::SET_COOKIE));
        assert_eq!(state.pairing().count_active_remote_sessions(), 1);
    }

    #[tokio::test]
    async fn remote_sessions_list_and_revoke_metadata_only() {
        let state = app_state("remote-sessions-list-revoke");
        let app = lan_app(&state);
        let remote = state
            .pairing()
            .create_remote_session_with_metadata(
                Some("Phone · Chrome".to_string()),
                Some(RemoteSessionClientMetadata {
                    device_family: Some("Phone".to_string()),
                    browser: Some("Chrome".to_string()),
                    os: Some("Android".to_string()),
                    network_hint: Some("home LAN".to_string()),
                    ip_family: Some("IPv4".to_string()),
                }),
            )
            .unwrap()
            .token;
        let control = state.pairing().create_control_session(None).unwrap().token;

        assert!(state.pairing().verify_remote_token(Some(&remote)));
        assert!(state.pairing().verify_control_token(Some(&control)));

        let (status, body) =
            lan_request(&app, Method::GET, "/api/remote/sessions", None, false).await;
        assert_eq!(status, StatusCode::OK);
        let sessions = body
            .get("sessions")
            .and_then(Value::as_array)
            .expect("sessions array");
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].get("label").and_then(Value::as_str),
            Some("Phone · Chrome")
        );
        assert_eq!(
            sessions[0]
                .pointer("/client/device_family")
                .and_then(Value::as_str),
            Some("Phone")
        );
        assert_eq!(
            sessions[0]
                .pointer("/client/browser")
                .and_then(Value::as_str),
            Some("Chrome")
        );
        assert_eq!(
            sessions[0]
                .pointer("/client/network_hint")
                .and_then(Value::as_str),
            Some("home LAN")
        );
        assert!(sessions[0].get("token_hash").is_none());
        let id = sessions[0].get("id").and_then(Value::as_str).unwrap();

        let (status, body) = lan_request(
            &app,
            Method::POST,
            &format!("/api/remote/sessions/{id}/revoke"),
            None,
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.get("revoked").and_then(Value::as_bool), Some(true));
        assert!(!state.pairing().verify_remote_token(Some(&remote)));
        assert!(state.pairing().verify_control_token(Some(&control)));

        let (status, body) =
            lan_request(&app, Method::GET, "/api/remote/sessions", None, false).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body.get("sessions")
                .and_then(Value::as_array)
                .unwrap()
                .len(),
            0
        );
    }
}
