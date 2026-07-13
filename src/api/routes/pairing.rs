use super::{auth_token_from_headers, internal_status};
use crate::app::state::AppState;
use axum::{
    Json, Router,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::post,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tracing::{info, warn};

#[derive(Serialize, JsonSchema)]
pub struct PairingStartResponse {
    pub token: String,
    pub auth_required: bool,
    pub expires_at_unix_secs: u64,
    pub token_kind: String,
    pub scopes: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct PairingRevocationResponse {
    pub revoked: usize,
}

#[derive(Deserialize, JsonSchema)]
pub struct BrowserSessionRequest {
    pub pairing_token: String,
}

#[derive(Serialize, JsonSchema)]
pub struct BrowserSessionResponse {
    pub auth_required: bool,
    pub expires_at_unix_secs: u64,
    pub token_kind: String,
    pub scopes: Vec<String>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/pairing/start", post(pairing_start))
        .route("/api/sessions/browser", post(browser_session_start))
        .route("/api/agents/token", post(agent_token_start))
        .route("/api/pairing/revoke-current", post(pairing_revoke_current))
        .route("/api/pairing/revoke-all", post(pairing_revoke_all))
}

async fn pairing_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
) -> Result<Json<PairingStartResponse>, StatusCode> {
    let peer_loopback = crate::app::auth::local_filesystem_request_allowed(
        &headers,
        peer.as_ref().map(|ConnectInfo(addr)| *addr),
    );
    if !peer_loopback {
        warn!(
            event = "pairing_start",
            status = "forbidden",
            error_kind = "forbidden",
            peer_loopback,
            "Pairing token request rejected"
        );
        return Err(StatusCode::FORBIDDEN);
    }
    let issued = state.pairing().create_token().map_err(internal_status)?;
    info!(
        event = "pairing_start",
        status = "ok",
        peer_loopback,
        auth_required = state.pairing().auth_required(),
        expires_at_unix_secs = issued.expires_at_unix_secs,
        "Pairing token issued"
    );
    Ok(Json(PairingStartResponse {
        token: issued.token,
        auth_required: state.pairing().auth_required(),
        expires_at_unix_secs: issued.expires_at_unix_secs,
        token_kind: "pairing_token".to_string(),
        scopes: vec![crate::zones::SCOPE_SESSION_CREATE.to_string()],
    }))
}

async fn agent_token_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
) -> Result<Json<PairingStartResponse>, StatusCode> {
    let peer_loopback = crate::app::auth::local_filesystem_request_allowed(
        &headers,
        peer.as_ref().map(|ConnectInfo(addr)| *addr),
    );
    if !peer_loopback {
        warn!(
            event = "agent_token_start",
            status = "forbidden",
            error_kind = "forbidden",
            peer_loopback,
            "Agent token request rejected"
        );
        return Err(StatusCode::FORBIDDEN);
    }
    let issued = state
        .pairing()
        .create_agent_token(None)
        .map_err(internal_status)?;
    info!(
        event = "agent_token_start",
        status = "ok",
        peer_loopback,
        expires_at_unix_secs = issued.expires_at_unix_secs,
        "Agent token issued"
    );
    Ok(Json(PairingStartResponse {
        token: issued.token,
        auth_required: state.pairing().auth_required(),
        expires_at_unix_secs: issued.expires_at_unix_secs,
        token_kind: "agent_token".to_string(),
        scopes: vec![
            crate::zones::SCOPE_AGENT_CONNECT.to_string(),
            crate::zones::SCOPE_STREAM_READ.to_string(),
        ],
    }))
}

async fn browser_session_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<BrowserSessionRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    if !state
        .pairing()
        .consume_pairing_token(Some(&request.pairing_token))
        .map_err(internal_status)?
    {
        warn!(
            event = "browser_session_start",
            status = "error",
            error_kind = "auth",
            "Browser session exchange failed"
        );
        return Err(StatusCode::UNAUTHORIZED);
    }
    let issued = state
        .pairing()
        .create_control_session(None)
        .map_err(internal_status)?;
    let mut response_headers = HeaderMap::new();
    let cookie = control_session_cookie(&issued.token, issued.expires_at_unix_secs, &headers);
    response_headers.insert(
        header::SET_COOKIE,
        cookie
            .parse()
            .map_err(|error| internal_status(format!("create browser session cookie: {error}")))?,
    );
    info!(
        event = "browser_session_start",
        status = "ok",
        expires_at_unix_secs = issued.expires_at_unix_secs,
        "Browser control session issued"
    );
    Ok((
        response_headers,
        Json(BrowserSessionResponse {
            auth_required: state.pairing().auth_required(),
            expires_at_unix_secs: issued.expires_at_unix_secs,
            token_kind: "control_session".to_string(),
            scopes: vec![crate::zones::SCOPE_CONTROL.to_string()],
        }),
    ))
}

async fn pairing_revoke_current(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PairingRevocationResponse>, StatusCode> {
    let token = auth_token_from_headers(&headers)
        .or_else(|| crate::app::auth::control_session_token_from_headers(&headers));
    if state
        .pairing()
        .revoke_token(token.as_deref())
        .map_err(internal_status)?
    {
        info!(
            event = "pairing_revoke_current",
            status = "ok",
            auth_source = if token.is_some() { "header" } else { "missing" },
            revoked = 1,
            "Pairing token revoked"
        );
        Ok(Json(PairingRevocationResponse { revoked: 1 }))
    } else {
        warn!(
            event = "pairing_revoke_current",
            status = "error",
            error_kind = "auth",
            auth_source = if token.is_some() { "header" } else { "missing" },
            "Pairing token revoke failed"
        );
        Err(StatusCode::UNAUTHORIZED)
    }
}

async fn pairing_revoke_all(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
) -> Result<Json<PairingRevocationResponse>, StatusCode> {
    let peer_loopback = crate::app::auth::local_filesystem_request_allowed(
        &headers,
        peer.as_ref().map(|ConnectInfo(addr)| *addr),
    );
    if !peer_loopback {
        warn!(
            event = "pairing_revoke_all",
            status = "forbidden",
            error_kind = "forbidden",
            peer_loopback,
            "Pairing revoke-all rejected"
        );
        return Err(StatusCode::FORBIDDEN);
    }
    let revoked = state
        .pairing()
        .revoke_all_active()
        .map_err(internal_status)?;
    info!(
        event = "pairing_revoke_all",
        status = "ok",
        peer_loopback,
        revoked,
        "Pairing tokens revoked"
    );
    Ok(Json(PairingRevocationResponse { revoked }))
}

fn control_session_cookie(token: &str, expires_at_unix_secs: u64, headers: &HeaderMap) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_secs();
    let max_age = expires_at_unix_secs.saturating_sub(now).max(1);
    let secure = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("https"));
    format!(
        "{}={}; Path=/; Max-Age={}; HttpOnly; SameSite=Lax{}",
        crate::zones::CONTROL_SESSION_COOKIE,
        token,
        max_age,
        if secure { "; Secure" } else { "" }
    )
}
