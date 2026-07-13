use super::auth_token_from_headers;
use crate::app::auth::{TrustedWebOrigins, websocket_origin_allowed};
use crate::app::state::AppState;
use crate::playback::service::{
    register_remote_agent_playback_zones, unregister_remote_agent_playback_zones,
    update_remote_agent_buffer_state, update_remote_agent_playback_state,
    update_remote_agent_signal_path,
};
use crate::protocol::{AgentCapabilities, AgentToCoreMessage};
use axum::{
    Extension, Router,
    extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio::time::{Duration, MissedTickBehavior};
use tracing::{info, warn};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/agent/ws", get(agent_ws_handler))
        .merge(browser_agent_routes())
}

/// The browser-zone agent socket. Pairing is enforced by the LAN middleware
/// before the WebSocket upgrade; the remote surface keeps the route behind
/// `require_remote_auth` so the browser's remote-session cookie authenticates
/// the WebSocket upgrade.
pub fn browser_agent_routes() -> Router<AppState> {
    Router::new().route("/api/agent/browser/ws", get(browser_agent_ws_handler))
}

async fn agent_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    trusted_origins: Option<Extension<TrustedWebOrigins>>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if !websocket_origin_allowed(
        &headers,
        trusted_origins.as_ref().map(|Extension(origins)| origins),
    ) {
        warn!(
            event = "websocket_origin",
            status = "forbidden",
            endpoint = "native_agent",
            origin_present = headers.contains_key(axum::http::header::ORIGIN),
            "Agent WebSocket origin rejected"
        );
        return StatusCode::FORBIDDEN.into_response();
    }
    let token = auth_token_from_headers(&headers);
    let header_authenticated = token
        .as_deref()
        .is_some_and(|token| state.pairing().verify_agent_token(Some(token)));
    if state.pairing().auth_required() && token.is_some() && !header_authenticated {
        warn!(
            event = "agent_ws_auth",
            status = "error",
            error_kind = "auth",
            auth_source = "header",
            "Agent WebSocket authentication failed"
        );
        return (StatusCode::UNAUTHORIZED, "Pairing token required").into_response();
    }
    ws.on_upgrade(move |socket| handle_agent_socket(socket, state, header_authenticated))
        .into_response()
}

async fn browser_agent_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    trusted_origins: Option<Extension<TrustedWebOrigins>>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if !websocket_origin_allowed(
        &headers,
        trusted_origins.as_ref().map(|Extension(origins)| origins),
    ) {
        warn!(
            event = "websocket_origin",
            status = "forbidden",
            endpoint = "browser_agent",
            origin_present = headers.contains_key(axum::http::header::ORIGIN),
            "Browser agent WebSocket origin rejected"
        );
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.on_upgrade(move |socket| handle_browser_agent_socket(socket, state))
        .into_response()
}

async fn handle_agent_socket(socket: WebSocket, state: AppState, header_authenticated: bool) {
    let (ws_tx, mut ws_rx) = socket.split();
    let Some(first) = read_text_message(&mut ws_rx).await else {
        warn!(
            event = "agent_ws_register",
            status = "error",
            error_kind = "bad_request",
            "Agent WebSocket closed before registration"
        );
        return;
    };
    let first = if let Some(auth) = parse_auth_message(&first) {
        if state.pairing().auth_required() && !state.pairing().verify_agent_token(Some(&auth.token))
        {
            warn!(
                event = "agent_ws_auth",
                status = "error",
                error_kind = "auth",
                auth_source = "first_message",
                "Agent WebSocket authentication failed"
            );
            return;
        }
        info!(
            event = "agent_ws_auth",
            status = "ok",
            auth_source = "first_message",
            "Agent WebSocket authenticated"
        );
        let Some(next) = read_text_message(&mut ws_rx).await else {
            warn!(
                event = "agent_ws_register",
                status = "error",
                error_kind = "bad_request",
                "Agent WebSocket closed after authentication"
            );
            return;
        };
        next
    } else {
        if state.pairing().auth_required() && !header_authenticated {
            warn!(
                event = "agent_ws_auth",
                status = "error",
                error_kind = "auth",
                auth_source = "missing",
                "Agent WebSocket authentication missing"
            );
            return;
        }
        if header_authenticated {
            info!(
                event = "agent_ws_auth",
                status = "ok",
                auth_source = "header",
                "Agent WebSocket authenticated"
            );
        }
        first
    };
    let Ok(AgentToCoreMessage::Register {
        agent_id,
        name,
        mut capabilities,
    }) = serde_json::from_str::<AgentToCoreMessage>(&first)
    else {
        warn!(
            event = "agent_ws_register",
            status = "error",
            error_kind = "bad_request",
            "Agent WebSocket registration message was invalid"
        );
        return;
    };
    // Native agents can never claim browser privacy semantics.
    capabilities.browser = false;

    run_agent_socket(ws_tx, ws_rx, state, agent_id, name, capabilities).await;
}

async fn handle_browser_agent_socket(socket: WebSocket, state: AppState) {
    let (ws_tx, mut ws_rx) = socket.split();
    let Some(first) = read_text_message(&mut ws_rx).await else {
        warn!(
            event = "browser_agent_ws_register",
            status = "error",
            error_kind = "bad_request",
            "Browser agent WebSocket closed before registration"
        );
        return;
    };
    let Ok(AgentToCoreMessage::Register {
        agent_id,
        name,
        mut capabilities,
    }) = serde_json::from_str::<AgentToCoreMessage>(&first)
    else {
        warn!(
            event = "browser_agent_ws_register",
            status = "error",
            error_kind = "bad_request",
            "Browser agent WebSocket registration message was invalid"
        );
        return;
    };
    let Some(agent_id) = normalized_browser_agent_id(&agent_id) else {
        warn!(
            event = "browser_agent_ws_register",
            status = "error",
            error_kind = "bad_request",
            "Browser agent id was invalid"
        );
        return;
    };
    // Browser zones are always private, render in the page, and expose no
    // selectable output devices or exclusive mode.
    capabilities.browser = true;
    capabilities.exclusive_supported = false;
    capabilities.supports_dsd128 = false;
    capabilities.supports_dsd256 = false;
    capabilities.output_devices = Vec::new();
    capabilities.output_device_capabilities = Vec::new();

    run_agent_socket(ws_tx, ws_rx, state, agent_id, name, capabilities).await;
}

/// A stable, URL-safe browser agent id. Browser zone ids double as ownership
/// capabilities, so anything that does not look like the client-generated
/// `browser-<random>` form is rejected instead of repaired.
fn normalized_browser_agent_id(agent_id: &str) -> Option<String> {
    let trimmed = agent_id.trim();
    let valid = trimmed.starts_with("browser-")
        && (16..=80).contains(&trimmed.len())
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    valid.then(|| trimmed.to_string())
}

async fn run_agent_socket(
    mut ws_tx: futures_util::stream::SplitSink<WebSocket, Message>,
    mut ws_rx: futures_util::stream::SplitStream<WebSocket>,
    state: AppState,
    agent_id: String,
    name: String,
    capabilities: AgentCapabilities,
) {
    let agent_ref = agent_log_ref(&agent_id);
    info!(
        event = "agent_ws_register",
        status = "ok",
        agent_ref,
        browser = capabilities.browser,
        "Agent connected"
    );
    let browser = capabilities.browser;
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    let connection_id =
        register_remote_agent_playback_zones(&state, agent_id.clone(), name, capabilities, cmd_tx);

    let writer = tokio::spawn(async move {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // Consume the interval's immediate first tick; registration itself is
        // already fresh server activity.
        heartbeat.tick().await;
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else { break; };
                    if let Ok(body) = serde_json::to_string(&cmd)
                        && ws_tx.send(Message::Text(body)).await.is_err()
                    {
                        break;
                    }
                }
                _ = heartbeat.tick(), if browser => {
                    if let Ok(body) = serde_json::to_string(&crate::protocol::CoreToAgentCommand::Heartbeat)
                        && ws_tx.send(Message::Text(body)).await.is_err()
                    {
                        break;
                    }
                }
            }
        }
    });

    while let Some(Ok(msg)) = ws_rx.next().await {
        if let Message::Text(body) = msg {
            match serde_json::from_str::<AgentToCoreMessage>(&body) {
                Ok(AgentToCoreMessage::PlaybackState(playback)) => {
                    update_remote_agent_playback_state(&state, &agent_id, playback);
                }
                Ok(AgentToCoreMessage::BufferState(buffer)) => {
                    update_remote_agent_buffer_state(&state, &agent_id, buffer);
                }
                Ok(AgentToCoreMessage::SyncSignalPath(signal_path)) => {
                    update_remote_agent_signal_path(&state, &agent_id, signal_path);
                }
                Ok(AgentToCoreMessage::Register { .. }) => {}
                Err(e) => warn!(
                    event = "agent_ws_message",
                    status = "error",
                    error_kind = "bad_request",
                    error = %e,
                    agent_ref,
                    "Agent WebSocket message was invalid"
                ),
            }
        }
    }

    writer.abort();
    unregister_remote_agent_playback_zones(&state, &agent_id, connection_id);
    info!(
        event = "agent_ws_disconnect",
        agent_ref, "Agent disconnected"
    );
}

/// Non-reversible correlation label for logs. Browser agent ids are bearer
/// capabilities and must never be written in full.
fn agent_log_ref(agent_id: &str) -> String {
    let digest = Sha256::digest(agent_id.as_bytes());
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    )
}

async fn read_text_message(
    ws_rx: &mut futures_util::stream::SplitStream<WebSocket>,
) -> Option<String> {
    let Some(Ok(Message::Text(body))) = ws_rx.next().await else {
        return None;
    };
    Some(body.to_string())
}

fn parse_auth_message(body: &str) -> Option<AgentWebSocketAuthMessage> {
    let auth = serde_json::from_str::<AgentWebSocketAuthMessage>(body).ok()?;
    (auth.message_type == "auth").then_some(auth)
}

#[derive(Deserialize)]
struct AgentWebSocketAuthMessage {
    #[serde(rename = "type")]
    message_type: String,
    token: String,
}

#[cfg(test)]
mod tests {
    use super::{agent_log_ref, normalized_browser_agent_id};
    use crate::app::state::AppState;
    use crate::playback::test_support::{app_state, app_state_with_pairing};
    use crate::protocol::ZoneProfile;
    use axum::{Router, middleware};
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use std::time::Duration;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    #[test]
    fn browser_agent_ids_require_browser_prefix_and_safe_characters() {
        assert_eq!(
            normalized_browser_agent_id(" browser-3f9c2ab1d0e4 ").as_deref(),
            Some("browser-3f9c2ab1d0e4")
        );
        assert!(normalized_browser_agent_id("agent-1").is_none());
        assert!(normalized_browser_agent_id("browser-").is_none());
        assert!(normalized_browser_agent_id("browser-abc def").is_none());
        assert!(normalized_browser_agent_id(&format!("browser-{}", "a".repeat(90))).is_none());
    }

    #[test]
    fn agent_log_reference_is_short_and_does_not_reveal_capability() {
        let capability = "browser-3f9c2ab1d0e4-secret-capability";
        let reference = agent_log_ref(capability);
        assert_eq!(reference.len(), 8);
        assert!(!reference.contains(capability));
        assert_eq!(reference, agent_log_ref(capability));
    }

    #[tokio::test]
    async fn authenticated_native_agent_upgrade_may_omit_browser_origin() {
        let state = app_state_with_pairing("native-agent-originless", true, false);
        let token = state.pairing().create_agent_token(None).unwrap().token;
        let app = super::routes().with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut request = format!("ws://{address}/api/agent/ws")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            crate::app::identity::AUTH_HEADER,
            token.parse().expect("agent token header"),
        );

        let (mut socket, _) = connect_async(request).await.unwrap();

        socket.close(None).await.unwrap();
    }

    fn lan_style_router(state: &AppState) -> Router {
        // Mirrors the ownership layering applied in `app::server::build_router`.
        Router::new()
            .merge(super::routes())
            .merge(crate::api::routes::zones::routes())
            .merge(crate::api::routes::zone_playback::routes())
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                crate::app::auth::enforce_browser_zone_ownership,
            ))
            .with_state(state.clone())
    }

    async fn http_json(request: reqwest::RequestBuilder) -> (reqwest::StatusCode, Value) {
        let response = request.send().await.expect("request should complete");
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let value = serde_json::from_str(&body).unwrap_or(Value::Null);
        (status, value)
    }

    #[tokio::test]
    async fn browser_zone_registers_over_ws_and_is_owner_controlled() {
        let state = app_state("browser-zone-ws-e2e");
        let app = lan_style_router(&state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let agent_id = "browser-e2e0test0agent0id";
        let (mut socket, _) = connect_async(format!("ws://{addr}/api/agent/browser/ws"))
            .await
            .expect("browser agent websocket should connect");
        socket
            .send(WsMessage::Text(
                json!({
                    "type": "register",
                    "agent_id": agent_id,
                    "name": "Safari on iPhone",
                    "capabilities": {
                        "output_devices": [],
                        "max_sample_rate": 48000,
                        "max_bit_depth": 24,
                        "exclusive_supported": false,
                        "browser": true
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        socket
            .send(WsMessage::Text(
                serde_json::to_string(&crate::protocol::AgentToCoreMessage::PlaybackState(
                    crate::protocol::AgentPlaybackState {
                        state: "Playing".to_string(),
                        track_title: Some("Browser Track".to_string()),
                        position_secs: 12.0,
                        duration_secs: 120.0,
                        ..Default::default()
                    },
                ))
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();

        let client = reqwest::Client::new();
        let base = format!("http://{addr}");

        // Wait for the registration to land.
        let mut owner_zones = Vec::new();
        for _ in 0..50 {
            let (status, body) = http_json(
                client
                    .get(format!("{base}/api/zones"))
                    .header(crate::app::identity::BROWSER_ZONE_HEADER, agent_id),
            )
            .await;
            assert_eq!(status, reqwest::StatusCode::OK);
            owner_zones = serde_json::from_value::<Vec<ZoneProfile>>(body).unwrap();
            if owner_zones.iter().any(|zone| zone.id == agent_id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let browser_zone = owner_zones
            .iter()
            .find(|zone| zone.id == agent_id)
            .expect("owner should see the browser zone");
        assert!(browser_zone.browser);
        assert!(!browser_zone.enabled);

        // Anyone without the owner header never sees the zone.
        let (status, body) = http_json(client.get(format!("{base}/api/zones"))).await;
        assert_eq!(status, reqwest::StatusCode::OK);
        let anonymous_zones = serde_json::from_value::<Vec<ZoneProfile>>(body).unwrap();
        assert!(anonymous_zones.iter().all(|zone| zone.id != agent_id));

        // Zone-scoped routes 404 for non-owners and work for the owner.
        let (status, _) =
            http_json(client.get(format!("{base}/api/zones/{agent_id}/status"))).await;
        assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
        let (status, body) = http_json(
            client
                .get(format!("{base}/api/zones/{agent_id}/status"))
                .header(crate::app::identity::BROWSER_ZONE_HEADER, agent_id),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::OK);
        assert_eq!(body["state"], "Playing");
        assert_eq!(body["track_title"], "Browser Track");
        assert_eq!(body["active_zone_id"], agent_id);

        // The browser zone can never become the shared active zone.
        let (status, _) = http_json(
            client
                .post(format!("{base}/api/zones/select"))
                .header(crate::app::identity::BROWSER_ZONE_HEADER, agent_id)
                .json(&json!({ "zone_id": agent_id })),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);

        // Owner can enable its private zone through the normal zone flow.
        let (status, _) = http_json(
            client
                .post(format!("{base}/api/zones/{agent_id}/enable"))
                .header(crate::app::identity::BROWSER_ZONE_HEADER, agent_id)
                .json(&json!({})),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::OK);

        // Owner transport commands reach the agent over the socket.
        let (status, _) = http_json(
            client
                .post(format!("{base}/api/zones/{agent_id}/pause"))
                .header(crate::app::identity::BROWSER_ZONE_HEADER, agent_id)
                .json(&json!({})),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::OK);
        let command = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("agent should receive the pause command")
            .expect("socket should stay open")
            .expect("frame should be readable");
        let command: Value = serde_json::from_str(command.to_text().unwrap()).unwrap();
        assert_eq!(command["type"], "pause");

        // Non-owner transport commands are rejected before dispatch.
        let (status, _) = http_json(
            client
                .post(format!("{base}/api/zones/{agent_id}/pause"))
                .json(&json!({})),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::NOT_FOUND);

        // Disconnecting unregisters the zone.
        socket.close(None).await.unwrap();
        let mut zone_gone = false;
        for _ in 0..50 {
            let (status, body) = http_json(
                client
                    .get(format!("{base}/api/zones"))
                    .header(crate::app::identity::BROWSER_ZONE_HEADER, agent_id),
            )
            .await;
            assert_eq!(status, reqwest::StatusCode::OK);
            let zones = serde_json::from_value::<Vec<ZoneProfile>>(body).unwrap();
            if zones.iter().all(|zone| zone.id != agent_id) {
                zone_gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(zone_gone, "browser zone should unregister on disconnect");
    }

    #[tokio::test]
    async fn browser_agent_ws_rejects_invalid_agent_ids() {
        let state = app_state("browser-zone-ws-bad-id");
        let app = lan_style_router(&state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let (mut socket, _) = connect_async(format!("ws://{addr}/api/agent/browser/ws"))
            .await
            .expect("browser agent websocket should connect");
        socket
            .send(WsMessage::Text(
                json!({
                    "type": "register",
                    "agent_id": "agent-1",
                    "name": "Not a browser",
                    "capabilities": {
                        "output_devices": [],
                        "max_sample_rate": 48000,
                        "max_bit_depth": 24,
                        "exclusive_supported": false
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

        // The server drops the socket instead of registering the agent.
        let closed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match socket.next().await {
                    None => break true,
                    Some(Ok(WsMessage::Close(_))) => break true,
                    Some(Ok(_)) => continue,
                    Some(Err(_)) => break true,
                }
            }
        })
        .await
        .expect("socket should close after an invalid registration");
        assert!(closed);
        assert!(
            crate::playback::service::refresh_playback_zones(&state)
                .iter()
                .all(|zone| zone.id != "agent-1")
        );
    }
}
