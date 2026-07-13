use super::internal_error;
use crate::app::state::AppState;
use crate::services::qobuz::{QobuzLoginRequest, QobuzUser};
use axum::{
    Json,
    extract::{ConnectInfo, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use tracing::warn;

const QOBUZ_OAUTH_STATE_COOKIE: &str = "fozmo_qobuz_oauth_state";
const QOBUZ_OAUTH_STATE_MAX_AGE_SECS: u64 = 10 * 60;

#[derive(Serialize, JsonSchema)]
pub struct QobuzStatusResponse {
    pub initialized: bool,
    pub logged_in: bool,
    pub authenticated: bool,
    pub user: Option<QobuzUser>,
    pub radio_enabled: bool,
}

pub(super) async fn qobuz_status(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    Ok(Json(qobuz_status_payload(&state).await))
}

pub(super) async fn qobuz_init(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state.qobuz().init().await.map_err(internal_error)?;
    Ok(Json(qobuz_status_payload(&state).await))
}

pub(super) async fn qobuz_login(
    State(state): State<AppState>,
    Json(req): Json<QobuzLoginRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .qobuz()
        .login(&req.email, &req.password)
        .await
        .map_err(qobuz_auth_error)?;
    Ok(Json(qobuz_status_payload(&state).await))
}

pub(super) async fn qobuz_logout(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state.qobuz().logout().await;
    Ok(Json(qobuz_status_payload(&state).await))
}

async fn qobuz_status_payload(state: &AppState) -> QobuzStatusResponse {
    let status = state.qobuz().status().await;
    QobuzStatusResponse {
        initialized: status.initialized,
        logged_in: status.logged_in,
        authenticated: status.logged_in,
        user: status.user,
        radio_enabled: state.settings().qobuz_radio_enabled(),
    }
}

pub(super) async fn qobuz_oauth_start(
    State(state): State<AppState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, String)> {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "Missing Host header".to_string()))?;
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    if !matches!(proto, "http" | "https") {
        return Err((
            StatusCode::BAD_REQUEST,
            "Invalid request scheme".to_string(),
        ));
    }
    let redirect_url = format!("{proto}://{host}/api/qobuz/oauth/callback");
    let (oauth_url, oauth_state) = state
        .qobuz()
        .oauth_url(&redirect_url, peer.map(|ConnectInfo(address)| address.ip()))
        .await
        .map_err(internal_error)?;
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::SET_COOKIE,
        oauth_state_cookie(&oauth_state, request_is_https(&headers), false)
            .parse()
            .map_err(|error| internal_error(format!("create Qobuz OAuth state cookie: {error}")))?,
    );
    Ok((response_headers, Redirect::temporary(&oauth_url)).into_response())
}

pub(super) async fn qobuz_oauth_callback(
    State(state): State<AppState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    let peer_ip = peer.map(|ConnectInfo(address)| address.ip());
    let cookie_state = oauth_state_cookie_value(&headers).map(str::to_string);
    let oauth_state = if let Some(presented) = params.get("state") {
        let cookie_matches = cookie_state.as_deref().is_some_and(|stored| {
            crate::zones::constant_time_token_matches(&[stored.to_string()], presented)
        });
        let peer_matches = if cookie_matches {
            false
        } else {
            state
                .qobuz()
                .oauth_state_matches_peer(presented, peer_ip)
                .await
        };
        if !cookie_matches && !peer_matches {
            return Err((
                StatusCode::BAD_REQUEST,
                "Invalid or expired OAuth state".to_string(),
            ));
        }
        presented.clone()
    } else if let Some(stored) = cookie_state {
        stored
    } else {
        state
            .qobuz()
            .pending_oauth_state_for_peer(peer_ip)
            .await
            .ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    "Invalid or expired OAuth state".to_string(),
                )
            })?
    };
    let Some(code) = params
        .get("code_autorisation")
        .or_else(|| params.get("code"))
        .cloned()
    else {
        return Ok(Html(
            r#"<html><body style="font-family:system-ui;padding:48px;background:#111;color:#eee"><h2>Qobuz login failed</h2><p>No authorization code was returned.</p><p><a style="color:#fff" href="/#/settings">Return to Fozmo</a></p></body></html>"#,
        )
        .into_response());
    };

    state
        .qobuz()
        .login_with_oauth_code(&code, &oauth_state)
        .await
        .map_err(|error| {
            if error == "Invalid or expired Qobuz OAuth state" {
                (
                    StatusCode::BAD_REQUEST,
                    "Invalid or expired OAuth state".to_string(),
                )
            } else {
                qobuz_auth_error(error)
            }
        })?;

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::SET_COOKIE,
        oauth_state_cookie("", request_is_https(&headers), true)
            .parse()
            .map_err(|error| internal_error(format!("clear Qobuz OAuth state cookie: {error}")))?,
    );
    Ok((
        response_headers,
        Html(
            r#"<html><head><meta http-equiv="refresh" content="0; url=/#/settings"></head><body style="font-family:system-ui;padding:48px;background:#111;color:#eee"><h2>Qobuz connected</h2><p>Returning to Fozmo...</p><p><a style="color:#fff" href="/#/settings">Return to Fozmo</a></p></body></html>"#,
        ),
    )
        .into_response())
}

fn request_is_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("https"))
}

fn oauth_state_cookie(state: &str, secure: bool, clear: bool) -> String {
    format!(
        "{QOBUZ_OAUTH_STATE_COOKIE}={state}; Path=/api/qobuz/oauth/callback; Max-Age={}; HttpOnly; SameSite=Lax{}",
        if clear {
            0
        } else {
            QOBUZ_OAUTH_STATE_MAX_AGE_SECS
        },
        if secure { "; Secure" } else { "" }
    )
}

fn oauth_state_cookie_value(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookie| {
            cookie.split(';').find_map(|part| {
                let (name, value) = part.trim().split_once('=')?;
                (name == QOBUZ_OAUTH_STATE_COOKIE).then_some(value)
            })
        })
}

fn qobuz_auth_error(error: String) -> (StatusCode, String) {
    warn!(
        event = "qobuz_auth_failure",
        error = %crate::diagnostics::logging::sanitize_error(&error),
        "Qobuz authentication failed"
    );
    (
        StatusCode::UNAUTHORIZED,
        "Qobuz authentication failed".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::test_support::app_state;

    #[tokio::test]
    async fn oauth_callback_rejects_a_missing_state_before_contacting_qobuz() {
        let mut params = HashMap::new();
        params.insert("code".to_string(), "private-oauth-code".to_string());

        let result = qobuz_oauth_callback(
            State(app_state("qobuz-oauth-missing-state")),
            None,
            HeaderMap::new(),
            Query(params),
        )
        .await;

        let (status, message) = match result {
            Ok(_) => panic!("missing state must be rejected"),
            Err(error) => error,
        };
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(message, "Invalid or expired OAuth state");
        assert!(!message.contains("private-oauth-code"));
    }

    #[test]
    fn oauth_state_cookie_retains_the_issued_nonce_for_the_callback() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("theme=dark; {QOBUZ_OAUTH_STATE_COOKIE}=expected-state")
                .parse()
                .unwrap(),
        );

        assert_eq!(oauth_state_cookie_value(&headers), Some("expected-state"));
        assert_eq!(oauth_state_cookie_value(&HeaderMap::new()), None);
    }

    #[test]
    fn authentication_errors_are_generic_for_clients() {
        let (status, message) = qobuz_auth_error(
            "request failed for private@example.test password=hunter2".to_string(),
        );

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(message, "Qobuz authentication failed");
        assert!(!message.contains("private@example.test"));
        assert!(!message.contains("hunter2"));
    }
}
