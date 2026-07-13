use crate::app::auth::{RequestSurface, TrustedWebOrigins, websocket_origin_allowed};
use crate::app::state::AppState;
use crate::playback::status::{build_status_response, refresh_active_output_status};
use axum::{
    Extension,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::time::Duration;
use tokio::time::{sleep, timeout};
use tracing::{debug, info, warn};

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
    trusted_origins: Option<Extension<TrustedWebOrigins>>,
    headers: HeaderMap,
) -> Response {
    let request_surface = surface
        .map(|Extension(surface)| surface)
        .unwrap_or(RequestSurface::Local);
    if !websocket_origin_allowed(
        &headers,
        trusted_origins.as_ref().map(|Extension(origins)| origins),
    ) {
        warn!(
            event = "websocket_origin",
            status = "forbidden",
            surface = if request_surface == RequestSurface::Remote {
                "remote"
            } else {
                "local"
            },
            origin_present = headers.contains_key(axum::http::header::ORIGIN),
            "Playback WebSocket origin rejected"
        );
        return StatusCode::FORBIDDEN.into_response();
    }
    if request_surface == RequestSurface::Remote {
        // Remote sockets authenticate strictly via the remote session cookie
        // before the upgrade; the LAN first-message token fallback and the
        // auth_required() skip below must stay unreachable remotely.
        let remote_authenticated = crate::app::auth::remote_session_token_from_headers(&headers)
            .as_deref()
            .is_some_and(|token| state.pairing().verify_remote_token(Some(token)));
        if !remote_authenticated {
            warn!(
                event = "playback_ws_auth",
                status = "error",
                error_kind = "auth",
                surface = "remote",
                "Remote playback WebSocket rejected before upgrade"
            );
            return StatusCode::UNAUTHORIZED.into_response();
        }
        return ws
            .on_upgrade(move |socket| handle_socket(socket, state, true, request_surface))
            .into_response();
    }

    let cookie_authenticated = crate::app::auth::control_session_token_from_headers(&headers)
        .as_deref()
        .is_some_and(|token| state.pairing().verify_control_token(Some(token)));
    ws.on_upgrade(move |socket| handle_socket(socket, state, cookie_authenticated, request_surface))
        .into_response()
}

async fn handle_socket(
    mut socket: WebSocket,
    state: AppState,
    cookie_authenticated: bool,
    surface: RequestSurface,
) {
    info!(
        event = "playback_ws_connect",
        "Playback WebSocket connected"
    );

    if state.pairing().auth_required()
        && !cookie_authenticated
        && !authenticate_socket(&mut socket, &state).await
    {
        warn!(
            event = "playback_ws_auth",
            status = "error",
            error_kind = "auth",
            auth_source = "missing",
            "Playback WebSocket authentication failed"
        );
        return;
    }
    debug!(
        event = "playback_ws_auth",
        status = "ok",
        auth_source = if cookie_authenticated {
            "cookie"
        } else {
            "first_message"
        },
        "Playback WebSocket authenticated"
    );

    let update_interval = playback_update_interval(surface);

    loop {
        let mut status = build_status_response(&state);
        status.surface = status_surface(surface).to_string();

        // Serialize status to JSON
        if let Ok(json_str) = serde_json::to_string(&status)
            && let Err(e) = socket.send(Message::Text(json_str)).await
        {
            info!(
                event = "playback_ws_disconnect",
                error_kind = "network",
                error = ?e,
                "Playback WebSocket disconnected"
            );
            break;
        }

        refresh_active_output_status(&state).await;

        sleep(update_interval).await;
    }
}

fn status_surface(surface: RequestSurface) -> &'static str {
    match surface {
        RequestSurface::Remote => "remote",
        RequestSurface::Local => "local",
    }
}

fn playback_update_interval(surface: RequestSurface) -> Duration {
    match surface {
        RequestSurface::Remote => Duration::from_millis(250),
        RequestSurface::Local => Duration::from_millis(40),
    }
}

async fn authenticate_socket(socket: &mut WebSocket, state: &AppState) -> bool {
    let Ok(Some(Ok(Message::Text(body)))) = timeout(Duration::from_secs(5), socket.recv()).await
    else {
        return false;
    };
    let Ok(auth) = serde_json::from_str::<WebSocketAuthMessage>(&body) else {
        return false;
    };
    auth.message_type == "auth" && state.pairing().verify_control_token(Some(&auth.token))
}

#[derive(Deserialize)]
struct WebSocketAuthMessage {
    #[serde(rename = "type")]
    message_type: String,
    token: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::test_support::app_state;
    use axum::{Router, routing::get};
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    async fn websocket_test_server(name: &str) -> std::net::SocketAddr {
        let app = Router::new()
            .route("/api/ws", get(ws_handler))
            .with_state(app_state(name));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        address
    }

    #[test]
    fn remote_socket_keeps_remote_surface_and_lower_update_rate() {
        assert_eq!(status_surface(RequestSurface::Remote), "remote");
        assert_eq!(
            playback_update_interval(RequestSurface::Remote),
            Duration::from_millis(250)
        );
        assert_eq!(status_surface(RequestSurface::Local), "local");
        assert_eq!(
            playback_update_interval(RequestSurface::Local),
            Duration::from_millis(40)
        );
    }

    #[tokio::test]
    async fn playback_websocket_rejects_a_hostile_browser_origin() {
        let address = websocket_test_server("playback-ws-hostile-origin").await;
        let mut request = format!("ws://{address}/api/ws")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "origin",
            "https://evil.test".parse().expect("valid hostile origin"),
        );

        let error = connect_async(request).await.unwrap_err();

        let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
            panic!("expected an HTTP rejection, got {error}");
        };
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn playback_websocket_accepts_same_origin_and_originless_upgrades() {
        let address = websocket_test_server("playback-ws-valid-origin").await;
        let url = format!("ws://{address}/api/ws");
        let mut same_origin_request = url.clone().into_client_request().unwrap();
        same_origin_request
            .headers_mut()
            .insert("origin", format!("http://{address}").parse().unwrap());

        let (mut same_origin, _) = connect_async(same_origin_request).await.unwrap();
        let (mut originless, _) = connect_async(url).await.unwrap();

        same_origin.close(None).await.unwrap();
        originless.close(None).await.unwrap();
    }
}
