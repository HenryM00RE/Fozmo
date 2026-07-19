use crate::api::error::ApiError;
use crate::app::identity;
use crate::app::state::AppState;
use axum::{
    Router,
    http::{HeaderMap, StatusCode, header},
};

mod agents;
mod appearance;
#[cfg(feature = "apple_music_capture")]
mod apple_music_capture;
mod artist_radio;
mod artwork;
mod config;
mod devices;
mod diagnostics;
mod eq;
#[cfg_attr(not(feature = "hegel"), allow(dead_code))]
mod hegel_control;
mod history;
mod lastfm;
mod library_basic;
mod library_detail;
mod pairing;
mod playback;
mod playback_sequence;
mod playlists;
mod presets;
mod profiles;
#[cfg_attr(not(feature = "qobuz"), allow(dead_code))]
mod qobuz;
mod queue;
mod remote;
#[cfg_attr(not(feature = "qobuz"), allow(dead_code))]
mod remote_artwork;
mod status;
mod streams;
mod upload;
mod zone_playback;
#[cfg_attr(not(feature = "hegel"), allow(dead_code))]
mod zones;

pub(crate) use history::{HistoryStatsQuery, RecentHistoryQuery};
pub(crate) use library_basic::{
    LibraryBrowseQueryParams, LibraryFoldersResponse, LibrarySearchQuery, RecentAlbumsQuery,
};
pub use pairing::{
    BrowserSessionRequest, BrowserSessionResponse, PairingRevocationResponse, PairingStartResponse,
};
pub(crate) use playlists::RecentPlaylistsQuery;
pub use profiles::{ProfilesResponse, RecentSearchesResponse};
pub(crate) use qobuz::QobuzSearchQuery;
pub use qobuz::QobuzStatusResponse;
#[cfg(test)]
use qobuz::{QobuzRadioNextRequest, QobuzRadioSeed};
pub use remote::{
    RemoteAccessSettingsDto, RemoteAccessSettingsResponse, RemoteAccessSettingsUpdateRequest,
    RemoteLinkCodeResponse, RemoteSessionMetadataDto, RemoteSessionRequest, RemoteSessionResponse,
    RemoteSessionRevocationResponse, RemoteSessionsResponse, remote_session_routes,
};
pub use streams::{
    sonos_art, sonos_qobuz_stream, sonos_stream, upnp_art, upnp_art_head, upnp_qobuz_stream,
    upnp_qobuz_stream_head, upnp_qobuz_stream_path, upnp_qobuz_stream_path_head, upnp_stream,
    upnp_stream_head,
};
pub use zones::ZoneCalibrationResponse;

pub fn create_router() -> Router<AppState> {
    let router = Router::new()
        .merge(zones::routes())
        .merge(zone_playback::routes())
        .merge(history::routes())
        .merge(profiles::routes())
        .merge(playlists::routes())
        .merge(pairing::routes())
        .merge(agents::routes())
        .merge(appearance::routes())
        .merge(artist_radio::routes())
        .merge(devices::routes())
        .merge(library_basic::routes())
        .merge(library_detail::routes())
        .merge(lastfm::routes())
        .merge(upload::routes())
        .merge(playback::routes())
        .merge(queue::routes())
        .merge(config::routes())
        .merge(diagnostics::routes())
        .merge(status::routes())
        .merge(artwork::routes())
        .merge(eq::routes())
        // Streams back browser playback (`<audio>`), not just Sonos/UPnP, so
        // they are registered for every feature set.
        .merge(streams::routes());
    #[cfg(feature = "apple_music_capture")]
    let router = router.merge(apple_music_capture::routes());
    #[cfg(feature = "qobuz")]
    let router = router.merge(qobuz::routes());
    #[cfg(feature = "hegel")]
    let router = router.merge(hegel_control::routes());
    router.merge(presets::routes()).merge(remote::routes())
}

/// Allowlisted router for the TLS remote listener.
///
/// Built from explicit includes only — never from `create_router()` with
/// removals — so a route added to the LAN surface can never leak remotely by
/// accident. Anything not registered here returns `404` on the remote
/// listener, even with a valid remote session cookie. Notable exclusions:
/// pairing/agent token issuance, library folder management, uploads, Hegel
/// amplifier control, Qobuz account/session management, diagnostics,
/// Sonos/UPnP media endpoints, and `/api/remote/settings`.
///
/// The `/api/stream/*` endpoints are included for browser playback: their
/// handlers require the `RemoteAuthenticated` marker on remote requests, so
/// they are only ever reachable behind `require_remote_auth`.
pub fn create_remote_router() -> Router<AppState> {
    let router = Router::new()
        .merge(status::routes())
        .merge(streams::routes())
        .merge(zones::remote_routes())
        .merge(zone_playback::remote_routes())
        // Lets a Remote Access browser register itself as a private playback
        // zone; the remote-session cookie authenticates the upgrade.
        .merge(agents::browser_agent_routes())
        .merge(playback::remote_routes())
        .merge(queue::routes())
        .merge(history::routes())
        .merge(profiles::routes())
        .merge(playlists::routes())
        .merge(artist_radio::routes())
        .merge(artwork::routes())
        .merge(eq::routes())
        .merge(presets::routes())
        .merge(config::routes())
        .merge(devices::routes())
        .merge(library_basic::remote_routes())
        .merge(library_detail::remote_routes())
        .merge(lastfm::remote_routes())
        .merge(appearance::remote_routes())
        .merge(remote::read_only_routes());
    #[cfg(feature = "qobuz")]
    let router = router.merge(qobuz::remote_routes());
    router
}

pub fn auth_token_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(identity::AUTH_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(str::to_string)
        })
}

fn internal_error(e: String) -> (StatusCode, String) {
    let error = ApiError::internal(e);
    (error.status(), error.message().to_string())
}

fn internal_status(e: String) -> StatusCode {
    ApiError::internal(e).status()
}

fn internal_response(e: String) -> axum::response::Response {
    use axum::response::IntoResponse;
    ApiError::internal(e).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::player::{PlaybackState, TrackCover};
    use crate::playback::test_support::{app_state, app_state_with_pairing};
    use crate::settings::HegelSettings;
    use crate::zones::local_device_zone_id;
    use axum::{
        Json,
        body::{Body, to_bytes},
        extract::State,
        http::{
            Method, Request,
            header::{CACHE_CONTROL, CONTENT_TYPE, COOKIE, HOST, SET_COOKIE},
        },
        middleware,
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    enum JsonExpectation {
        Array,
        ObjectKeys(&'static [&'static str]),
    }

    async fn request_json(
        app: Router,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let method_label = method.as_str().to_string();
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(HOST, "127.0.0.1:3000");
        let body = match body {
            Some(body) => {
                builder = builder.header(CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let response = app
            .oneshot(builder.body(body).expect("request should build"))
            .await
            .expect("router should respond");
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("response body should collect");
        let json: Value = serde_json::from_slice(&body).unwrap_or_else(|err| {
            panic!(
                "{method_label} {path} should return JSON, got {err}: {}",
                String::from_utf8_lossy(&body)
            )
        });
        (status, json)
    }

    async fn request_status(
        app: Router,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> StatusCode {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(HOST, "127.0.0.1:3000");
        let body = match body {
            Some(body) => {
                builder = builder.header(CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        app.oneshot(builder.body(body).expect("request should build"))
            .await
            .expect("router should respond")
            .status()
    }

    fn protected_router(state: AppState) -> Router {
        create_router()
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                crate::app::auth::require_pairing,
            ))
            .with_state(state)
    }

    async fn request_status_with_token(
        app: Router,
        path: &str,
        header_name: Option<&str>,
        token: Option<&str>,
    ) -> StatusCode {
        let mut builder = Request::builder()
            .method(Method::GET)
            .uri(path)
            .header(HOST, "127.0.0.1:3000");
        if let (Some(header_name), Some(token)) = (header_name, token) {
            builder = builder.header(header_name, token);
        }
        app.oneshot(builder.body(Body::empty()).expect("request should build"))
            .await
            .expect("router should respond")
            .status()
    }

    #[tokio::test]
    async fn pairing_middleware_rejects_query_token_by_default() {
        let state = app_state_with_pairing("pairing-query-default", true, false);
        let token = state.pairing().create_control_session(None).unwrap().token;
        let app = protected_router(state);

        assert_eq!(
            request_status_with_token(app.clone(), "/api/status", None, None).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request_status_with_token(
                app.clone(),
                "/api/status",
                Some(identity::AUTH_HEADER),
                Some(&token),
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(
            request_status_with_token(
                app.clone(),
                "/api/status",
                Some("authorization"),
                Some(&format!("Bearer {token}")),
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(
            request_status_with_token(
                app,
                &format!("/api/status?token={}", urlencoding::encode(&token)),
                None,
                None,
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn local_status_reports_local_surface() {
        let state = app_state("status-surface-local");
        let app = create_router().with_state(state);

        let (status, body) = request_json(app, Method::GET, "/api/status", None).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.get("surface").and_then(Value::as_str), Some("local"));
    }

    #[tokio::test]
    async fn trusted_lan_surface_does_not_require_a_browser_session() {
        let state = app_state_with_pairing("trusted-lan-no-pairing", false, false);
        let app = protected_router(state);

        let status = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/status")
                    .header(HOST, "fozmo-studio.local:3001")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond")
            .status();

        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn pairing_middleware_allows_query_token_only_for_local_dev_mode() {
        let state = app_state_with_pairing("pairing-query-dev", true, true);
        let token = state.pairing().create_control_session(None).unwrap().token;
        let app = protected_router(state);

        assert_eq!(
            request_status_with_token(
                app.clone(),
                &format!("/api/status?token={}", urlencoding::encode(&token)),
                None,
                None,
            )
            .await,
            StatusCode::OK
        );

        let status = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/api/status?token={}", urlencoding::encode(&token)))
                    .header(HOST, "127.0.0.1:3000")
                    .header("origin", "https://evil.test")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond")
            .status();

        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn pairing_revocation_endpoints_invalidate_tokens() {
        let state = app_state_with_pairing("pairing-revocation", true, false);
        let current = state.pairing().create_control_session(None).unwrap().token;
        let other = state.pairing().create_control_session(None).unwrap().token;
        let app = protected_router(state);

        assert_eq!(
            request_status_with_token(
                app.clone(),
                "/api/status",
                Some(identity::AUTH_HEADER),
                Some(&current),
            )
            .await,
            StatusCode::OK
        );

        let revoke_current = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/pairing/revoke-current")
                    .header(HOST, "127.0.0.1:3000")
                    .header(identity::AUTH_HEADER, &current)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond")
            .status();
        assert_eq!(revoke_current, StatusCode::OK);
        assert_eq!(
            request_status_with_token(
                app.clone(),
                "/api/status",
                Some(identity::AUTH_HEADER),
                Some(&current),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request_status_with_token(
                app.clone(),
                "/api/status",
                Some(identity::AUTH_HEADER),
                Some(&other),
            )
            .await,
            StatusCode::OK
        );

        let revoke_all = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/pairing/revoke-all")
                    .header(HOST, "127.0.0.1:3000")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond")
            .status();
        assert_eq!(revoke_all, StatusCode::OK);
        assert_eq!(
            request_status_with_token(
                app,
                "/api/status",
                Some(identity::AUTH_HEADER),
                Some(&other),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn browser_session_exchange_consumes_pairing_token_and_sets_cookie() {
        let state = app_state_with_pairing("browser-session-exchange", true, false);
        let pairing_token = state.pairing().create_token().unwrap().token;
        let app = protected_router(state);

        assert_eq!(
            request_status_with_token(
                app.clone(),
                "/api/status",
                Some(identity::AUTH_HEADER),
                Some(&pairing_token),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/sessions/browser")
                    .header(HOST, "127.0.0.1:3000")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "pairing_token": pairing_token.clone() }).to_string(),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get(SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .expect("session response should set a cookie")
            .to_string();
        assert!(cookie.contains(crate::zones::CONTROL_SESSION_COOKIE));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
        let cookie_pair = cookie.split(';').next().unwrap();

        let status = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/status")
                    .header(HOST, "127.0.0.1:3000")
                    .header(COOKIE, cookie_pair)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond")
            .status();
        assert_eq!(status, StatusCode::OK);

        let reuse = request_status(
            app,
            Method::POST,
            "/api/sessions/browser",
            Some(json!({ "pairing_token": pairing_token })),
        )
        .await;
        assert_eq!(reuse, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn remote_session_token_cannot_replay_against_lan_pairing_middleware() {
        let state = app_state_with_pairing("pairing-rejects-remote-session", true, false);
        let remote_token = state.pairing().create_remote_session(None).unwrap().token;

        assert!(state.pairing().verify_remote_token(Some(&remote_token)));
        assert!(!state.pairing().verify_control_token(Some(&remote_token)));
        let app = protected_router(state);

        let status = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/remote/settings")
                    .header(HOST, "lan.example:3000")
                    .header("origin", "https://evil.test")
                    .header(identity::AUTH_HEADER, remote_token)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond")
            .status();

        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn zone_cover_route_reads_the_requested_zone_player() {
        let state = app_state("zone-cover-route");
        let hegel_device = "Hegel H390 USB";
        let hegel_zone_id = local_device_zone_id(hegel_device);
        state
            .zones()
            .sync_local_devices(vec![hegel_device.to_string()]);
        state.zones().enable_zone(&hegel_zone_id).unwrap();
        state
            .zones()
            .select_zone(crate::zones::LOCAL_ZONE_ID)
            .unwrap();
        state
            .zones()
            .player_for_zone(&hegel_zone_id)
            .unwrap()
            .set_cover_for_test(Some(TrackCover {
                mime: "image/png".to_string(),
                data: tiny_png(),
            }));
        let app = create_router().with_state(state);

        let active_status = request_status(app.clone(), Method::GET, "/api/cover", None).await;
        let zone_status = request_status(
            app.clone(),
            Method::GET,
            &format!("/api/zones/{hegel_zone_id}/cover"),
            None,
        )
        .await;

        assert_eq!(active_status, StatusCode::NOT_FOUND);
        assert_eq!(zone_status, StatusCode::OK);
    }

    fn local_track_source(track_id: i64) -> crate::protocol::SourceRef {
        crate::protocol::SourceRef::LocalTrack {
            track_id,
            file_name: None,
            title: Some(format!("Track {track_id}")),
            artist: None,
            album: None,
            album_artist: None,
            album_id: None,
            art_id: None,
            duration_secs: None,
            ext_hint: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }

    fn tiny_png() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]));
        let mut cursor = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        cursor.into_inner()
    }

    #[tokio::test]
    async fn cover_response_rejects_unsafe_artwork_bytes() {
        let state = app_state("cover-response-hardens-unsafe");
        state
            .zones()
            .player_for_zone(crate::zones::LOCAL_ZONE_ID)
            .unwrap()
            .set_cover_for_test(Some(TrackCover {
                mime: "image/svg+xml".to_string(),
                data: br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#
                    .to_vec(),
            }));
        let app = create_router().with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/cover")
                    .header(HOST, "127.0.0.1:3000")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(
            response
                .headers()
                .get("x-content-type-options")
                .and_then(|v| v.to_str().ok()),
            Some("nosniff")
        );
    }

    #[tokio::test]
    async fn library_art_route_does_not_serve_stored_unsafe_mime() {
        let state = app_state("library-art-route-rejects-unsafe");
        let art_id = state.library().insert_unsafe_artwork_for_test(
            "text/html",
            b"<!doctype html><script>alert(1)</script>",
        );
        let app = create_router().with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/api/library/art/{art_id}"))
                    .header(HOST, "127.0.0.1:3000")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn album_cover_upload_rejects_active_content() {
        let state = app_state("album-cover-upload-rejects-active-content");
        let album_dir = state.music_dir().join("Artist - Album");
        std::fs::create_dir_all(&album_dir).unwrap();
        std::fs::write(album_dir.join("01 Track.wav"), b"not a real wav").unwrap();
        state.library().scan().unwrap();
        let album_id = state.library().albums().unwrap()[0].id;
        let app = create_router().with_state(state);
        let boundary = "cover-upload-boundary";
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            b"Content-Disposition: form-data; name=\"cover\"; filename=\"cover.svg\"\r\n",
        );
        body.extend_from_slice(b"Content-Type: image/jpeg\r\n\r\n");
        body.extend_from_slice(svg);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let status = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/api/library/albums/{album_id}/cover"))
                    .header(HOST, "127.0.0.1:3000")
                    .header(
                        CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond")
            .status();

        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn library_rescan_returns_accepted_progress() {
        let state = app_state("library-rescan-accepted-progress");
        std::fs::create_dir_all(state.music_dir()).unwrap();
        let app = create_router().with_state(state);

        let (status, progress) = request_json(app, Method::POST, "/api/library/rescan", None).await;

        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(progress.get("running").and_then(Value::as_bool).is_some());
        assert!(progress.get("phase").and_then(Value::as_str).is_some());
        assert!(progress.get("scanned").and_then(Value::as_u64).is_some());
        assert!(progress.get("total").and_then(Value::as_u64).is_some());
    }

    #[tokio::test]
    async fn zone_now_playing_art_route_serves_current_cover_for_matching_source() {
        let state = app_state("zone-now-playing-art-route");
        let zone_id = crate::zones::LOCAL_ZONE_ID;
        let cover_data = tiny_png();
        state
            .zones()
            .player_for_zone(zone_id)
            .unwrap()
            .set_cover_for_test(Some(TrackCover {
                mime: "image/png".to_string(),
                data: cover_data.clone(),
            }));
        state.listening().start(
            state.library(),
            zone_id.to_string(),
            "Local".to_string(),
            "default".to_string(),
            local_track_source(1),
            Vec::new(),
        );
        let app = create_router().with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!(
                        "/api/zones/{zone_id}/now-playing-art?source=local%3A1"
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("image/png")
        );
        // The live player cover is not derived from the requested source, so
        // it must never be cached under the per-track URL.
        assert_eq!(
            response
                .headers()
                .get(CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store")
        );
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("response body should collect");
        assert_eq!(&body[..], cover_data.as_slice());
    }

    #[tokio::test]
    async fn zone_now_playing_art_route_uses_current_upnp_asset_before_listening_starts() {
        let state = app_state("zone-now-playing-art-upnp-current");
        let target = crate::audio::upnp::UpnpRendererTarget {
            id: "renderer-1".to_string(),
            name: "UPnP Test".to_string(),
            host: "127.0.0.1".to_string(),
            port: 1400,
            model: Some("Test Renderer".to_string()),
            manufacturer: Some("Test".to_string()),
            av_transport_control_url: "/MediaRenderer/AVTransport/Control".to_string(),
            rendering_control_url: Some("/MediaRenderer/RenderingControl/Control".to_string()),
            connection_manager_url: None,
            max_sample_rate: 192_000,
            max_bit_depth: 24,
            max_dsd_rate: None,
            capability_detection_source: crate::protocol::CapabilityDetectionSource::Advertised,
            capability_detection_status: crate::protocol::CapabilityDetectionStatus::Complete,
            capability_detection_message: None,
            protocol_info: Vec::new(),
            pcm_containers: Vec::new(),
        };
        let zone_id = crate::audio::upnp::receiver_zone_id(&target.id);
        state
            .zones()
            .sync_upnp_renderers(vec![crate::audio::upnp::UpnpRenderer {
                target,
                online: true,
            }]);
        state.upnp().register_remote_stream(
            "asset-1",
            Some(TrackCover {
                mime: "image/png".to_string(),
                data: tiny_png(),
            }),
            "audio/flac".to_string(),
            Some(1024),
            None,
        );
        state.upnp().seed_playback_for_test(
            &zone_id,
            crate::audio::upnp::UpnpAsset {
                id: "asset-1".to_string(),
                source_ref: local_track_source(9),
                stream_url: "http://core.test/upnp/stream/asset-1?token=abc".to_string(),
                mime_type: "audio/flac".to_string(),
                byte_len: Some(1024),
                art_url: None,
                title: Some("UPnP Track".to_string()),
                artist: Some("Artist".to_string()),
                album: Some("Album".to_string()),
                duration_secs: Some(180.0),
                source_rate: 44_100,
                target_rate: 44_100,
                source_bits: 16,
                target_bits: 16,
                active_output_mode: None,
                qobuz_resolve_ms: None,
                asset_registration_ms: None,
                render_signature: Some("asset-1-sig".to_string()),
                configured_render_signature: Some("asset-1-sig".to_string()),
                render_ms: None,
                prepare_ms: None,
                cache_hit: None,
                render_or_stream_plan: None,
                cache_lookup_ms: None,
                cache_wait_ms: None,
            },
            "Playing",
        );
        let app = create_router().with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!(
                        "/api/zones/{zone_id}/now-playing-art?source=local%3A9"
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("image/png")
        );
        assert_eq!(
            response
                .headers()
                .get(CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store")
        );
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("response body should collect");
        assert_eq!(
            crate::library::safe_raster_artwork_mime(&body, "image/png"),
            Some("image/png")
        );
    }

    #[tokio::test]
    async fn zone_now_playing_art_route_rejects_current_cover_for_other_source() {
        let state = app_state("zone-now-playing-art-mismatch");
        let zone_id = crate::zones::LOCAL_ZONE_ID;
        state
            .zones()
            .player_for_zone(zone_id)
            .unwrap()
            .set_cover_for_test(Some(TrackCover {
                mime: "image/png".to_string(),
                data: tiny_png(),
            }));
        // The zone is already playing track 2; a stale request for track 1's
        // art must not be answered (and cached) with track 2's cover.
        state.listening().start(
            state.library(),
            zone_id.to_string(),
            "Local".to_string(),
            "default".to_string(),
            local_track_source(2),
            Vec::new(),
        );
        let app = create_router().with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!(
                        "/api/zones/{zone_id}/now-playing-art?source=local%3A1"
                    ))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store")
        );
    }

    #[test]
    fn qobuz_radio_request_rejects_missing_seed() {
        let req: QobuzRadioNextRequest = serde_json::from_value(json!({
            "exclude_track_ids": [1, 2],
            "limit": 10
        }))
        .unwrap();

        let err = req.seed().unwrap_err();

        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn qobuz_radio_request_accepts_track_seed() {
        let req: QobuzRadioNextRequest = serde_json::from_value(json!({
            "seed_track_id": 23929516,
            "exclude_track_ids": [23929516],
            "limit": 50
        }))
        .unwrap();

        assert!(matches!(
            req.seed().unwrap(),
            QobuzRadioSeed::Track(23929516)
        ));
    }

    #[test]
    fn qobuz_radio_request_accepts_artist_name_seed() {
        let req: QobuzRadioNextRequest = serde_json::from_value(json!({
            "seed_artist_name": "Radiohead",
            "exclude_track_ids": [],
            "limit": 50
        }))
        .unwrap();

        assert!(matches!(
            req.seed().unwrap(),
            QobuzRadioSeed::ArtistName(name) if name == "Radiohead"
        ));
    }

    #[tokio::test]
    async fn hegel_volume_invalid_direction_returns_bad_request() {
        let state = app_state("hegel-volume-direction");
        let _ = state.settings().update(|persisted| {
            persisted.hegel = HegelSettings {
                enabled: true,
                zone_id: Some(state.zones().active_zone_id()),
                linked_airplay_zone_id: None,
                host: Some("192.168.1.50".to_string()),
                port: 50001,
                input: 9,
                default_volume: 20,
                max_volume: 50,
                standby_usb_visible: false,
            };
        });

        let err = hegel_control::hegel_volume(
            State(state),
            Json(hegel_control::HegelVolumeRequest {
                host: "192.168.1.50".to_string(),
                port: Some(50001),
                volume: None,
                direction: Some("left".to_string()),
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn critical_get_routes_return_successful_json() {
        let app = create_router().with_state(app_state("api-smoke"));
        let routes = vec![
            (
                "/api/status",
                JsonExpectation::ObjectKeys(&["state", "active_zone_id", "target_rate"]),
            ),
            (
                "/api/library/summary",
                JsonExpectation::ObjectKeys(&["albums", "tracks", "artists"]),
            ),
            ("/api/library/albums", JsonExpectation::Array),
            ("/api/library/recent-albums", JsonExpectation::Array),
            ("/api/library/favorite-albums", JsonExpectation::Array),
            ("/api/library/tracks", JsonExpectation::Array),
            ("/api/library/artists", JsonExpectation::Array),
            (
                "/api/library/search?q=",
                JsonExpectation::ObjectKeys(&["albums", "artists", "tracks"]),
            ),
            (
                "/api/library/folders",
                JsonExpectation::ObjectKeys(&["folders"]),
            ),
            ("/api/history/recent", JsonExpectation::Array),
            (
                "/api/history/stats",
                JsonExpectation::ObjectKeys(&["range", "total_listened_secs", "top_artists"]),
            ),
            (
                "/api/history/export",
                JsonExpectation::ObjectKeys(&["entries"]),
            ),
            (
                "/api/profiles",
                JsonExpectation::ObjectKeys(&["profiles", "active_profile_id"]),
            ),
            ("/api/playlists", JsonExpectation::Array),
            ("/api/playlists/recent", JsonExpectation::Array),
            #[cfg(feature = "qobuz")]
            (
                "/api/qobuz/status",
                JsonExpectation::ObjectKeys(&["initialized", "logged_in", "radio_enabled"]),
            ),
            #[cfg(feature = "qobuz")]
            (
                "/api/qobuz/settings",
                JsonExpectation::ObjectKeys(&["radio_enabled"]),
            ),
            #[cfg(feature = "qobuz")]
            (
                "/api/qobuz/cache",
                JsonExpectation::ObjectKeys(&["bytes", "files"]),
            ),
            (
                "/api/lastfm/status",
                JsonExpectation::ObjectKeys(&[
                    "configured",
                    "source",
                    "radio_enabled",
                    "radio_active",
                ]),
            ),
            ("/api/zones", JsonExpectation::Array),
            (
                "/api/eq",
                JsonExpectation::ObjectKeys(&["enabled", "preamp_db", "bands"]),
            ),
            ("/api/eq/presets", JsonExpectation::Array),
            ("/api/files", JsonExpectation::Array),
        ];

        for (path, expectation) in routes {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header(HOST, "127.0.0.1:3000")
                        .body(Body::empty())
                        .expect("request should build"),
                )
                .await
                .expect("router should respond");
            let status = response.status();
            let body = to_bytes(response.into_body(), 1024 * 1024)
                .await
                .expect("response body should collect");
            assert_eq!(
                status,
                StatusCode::OK,
                "{path} returned {status}: {}",
                String::from_utf8_lossy(&body)
            );
            let json: Value = serde_json::from_slice(&body).unwrap_or_else(|err| {
                panic!(
                    "{path} should return JSON, got {err}: {}",
                    String::from_utf8_lossy(&body)
                )
            });

            match expectation {
                JsonExpectation::Array => {
                    assert!(json.is_array(), "{path} should return a JSON array: {json}");
                }
                JsonExpectation::ObjectKeys(keys) => {
                    let object = json
                        .as_object()
                        .unwrap_or_else(|| panic!("{path} should return a JSON object: {json}"));
                    for key in keys {
                        assert!(
                            object.contains_key(*key),
                            "{path} response missing key '{key}': {json}"
                        );
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn lastfm_settings_status_never_returns_secret() {
        let app = create_router().with_state(app_state("api-lastfm-settings"));

        let (status, json) = request_json(
            app.clone(),
            Method::POST,
            "/api/lastfm/settings",
            Some(json!({ "api_key": "test-secret" })),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json.get("configured").and_then(Value::as_bool), Some(true));
        assert_eq!(
            json.get("source").and_then(Value::as_str),
            Some("secret_store")
        );
        assert_eq!(
            json.get("radio_enabled").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            json.get("radio_active").and_then(Value::as_bool),
            Some(true)
        );
        assert!(json.get("api_key").is_none());

        #[cfg(feature = "qobuz")]
        {
            let (_, qobuz) = request_json(app, Method::GET, "/api/qobuz/settings", None).await;
            assert_eq!(
                qobuz.get("radio_enabled").and_then(Value::as_bool),
                Some(false)
            );
        }
    }

    #[cfg(feature = "qobuz")]
    #[tokio::test]
    async fn radio_settings_are_mutually_exclusive() {
        let app = create_router().with_state(app_state("api-radio-mutual-exclusion"));

        let (status, lastfm) = request_json(
            app.clone(),
            Method::POST,
            "/api/lastfm/settings",
            Some(json!({ "api_key": "test-secret", "radio_enabled": true })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            lastfm.get("radio_enabled").and_then(Value::as_bool),
            Some(true)
        );
        let (_, qobuz) = request_json(app.clone(), Method::GET, "/api/qobuz/settings", None).await;
        assert_eq!(
            qobuz.get("radio_enabled").and_then(Value::as_bool),
            Some(false)
        );

        let (status, qobuz) = request_json(
            app.clone(),
            Method::POST,
            "/api/qobuz/settings",
            Some(json!({ "radio_enabled": true })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            qobuz.get("radio_enabled").and_then(Value::as_bool),
            Some(true)
        );
        let (_, lastfm) = request_json(app, Method::GET, "/api/lastfm/status", None).await;
        assert_eq!(
            lastfm.get("radio_enabled").and_then(Value::as_bool),
            Some(false)
        );
    }

    #[tokio::test]
    async fn playback_control_routes_execute_router_without_hardware() {
        let state = app_state("api-playback-controls-smoke");
        let app = create_router().with_state(state.clone());
        let zone_id = crate::zones::LOCAL_ZONE_ID;
        let player = state.zones().active_player();

        player.set_playback_state_for_test(PlaybackState::Playing);
        let status = request_status(app.clone(), Method::POST, "/api/pause", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(player.playback_state(), PlaybackState::Paused);

        let status = request_status(app.clone(), Method::POST, "/api/resume", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(matches!(
            player.playback_state(),
            PlaybackState::Paused | PlaybackState::Starting | PlaybackState::Playing
        ));

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/seek",
            Some(json!({ "seconds": 12.5 })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let status = request_status(app.clone(), Method::POST, "/api/next", None).await;
        assert_eq!(status, StatusCode::OK);

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/loop-mode",
            Some(json!({ "mode": "loop" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            state
                .library()
                .now_playing_queue(zone_id)
                .unwrap()
                .unwrap()
                .state["loopMode"],
            "loop"
        );

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/volume",
            Some(json!({ "volume": 0.42 })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            state.settings().playback_for_zone(zone_id).volume,
            Some(0.42)
        );

        player.set_playback_state_for_test(PlaybackState::Playing);
        let status = request_status(
            app.clone(),
            Method::POST,
            &format!("/api/zones/{zone_id}/pause"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(player.playback_state(), PlaybackState::Paused);

        let status = request_status(
            app.clone(),
            Method::POST,
            &format!("/api/zones/{zone_id}/resume"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(matches!(
            player.playback_state(),
            PlaybackState::Paused | PlaybackState::Starting | PlaybackState::Playing
        ));

        let status = request_status(
            app.clone(),
            Method::POST,
            &format!("/api/zones/{zone_id}/seek"),
            Some(json!({ "seconds": 4.0 })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let status = request_status(
            app.clone(),
            Method::POST,
            &format!("/api/zones/{zone_id}/next"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let status = request_status(
            app.clone(),
            Method::POST,
            &format!("/api/zones/{zone_id}/loop-mode"),
            Some(json!({ "mode": "one" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            state
                .library()
                .now_playing_queue(zone_id)
                .unwrap()
                .unwrap()
                .state["loopMode"],
            "one"
        );

        let status = request_status(
            app,
            Method::POST,
            &format!("/api/zones/{zone_id}/stop"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(player.playback_state(), PlaybackState::Stopped);
    }

    #[tokio::test]
    async fn filesystem_sensitive_routes_reject_cross_site_requests() {
        let app = create_router().with_state(app_state("api-filesystem-sensitive"));

        let routes = vec![
            (Method::POST, "/api/pairing/start", None),
            (Method::GET, "/api/library/folders", None),
            (
                Method::POST,
                "/api/library/folders",
                Some(json!({ "path": "." })),
            ),
            (Method::POST, "/api/library/rescan", None),
            (Method::GET, "/api/stream/local/1", None),
        ];

        for (method, path, body) in routes {
            let mut builder = Request::builder()
                .method(method)
                .uri(path)
                .header(HOST, "127.0.0.1:3000")
                .header("origin", "https://evil.test");
            let body = match body {
                Some(body) => {
                    builder = builder.header(CONTENT_TYPE, "application/json");
                    Body::from(body.to_string())
                }
                None => Body::empty(),
            };
            let status = app
                .clone()
                .oneshot(builder.body(body).expect("request should build"))
                .await
                .expect("router should respond")
                .status();

            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{path} should reject cross-site access"
            );
        }
    }

    #[tokio::test]
    async fn critical_write_routes_accept_deterministic_requests() {
        let state = app_state("api-write-smoke");
        let app = create_router().with_state(state.clone());

        let (status, zones) = request_json(app.clone(), Method::GET, "/api/zones", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            zones
                .as_array()
                .is_some_and(|zones| zones.iter().any(|zone| {
                    zone.get("id").and_then(Value::as_str) == Some(crate::zones::LOCAL_ZONE_ID)
                })),
            "zones response should include the local core zone: {zones}"
        );

        let (status, pairing) =
            request_json(app.clone(), Method::POST, "/api/pairing/start", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            pairing
                .get("token")
                .and_then(Value::as_str)
                .is_some_and(|token| !token.is_empty()),
            "pairing response should include a token: {pairing}"
        );
        assert_eq!(
            pairing.get("auth_required").and_then(Value::as_bool),
            Some(false)
        );

        let (status, created) = request_json(
            app.clone(),
            Method::POST,
            "/api/profiles",
            Some(json!({ "name": "Late Night" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let created_profile_id = created
            .get("active_profile_id")
            .and_then(Value::as_str)
            .expect("created profile response should include an active profile id")
            .to_string();
        assert_ne!(created_profile_id, crate::settings::DEFAULT_PROFILE_ID);
        assert!(
            created
                .get("profiles")
                .and_then(Value::as_array)
                .is_some_and(|profiles| profiles
                    .iter()
                    .any(
                        |profile| profile.get("name").and_then(Value::as_str) == Some("Late Night")
                    )),
            "created profile response should include the new profile: {created}"
        );

        let (status, updated) = request_json(
            app.clone(),
            Method::PUT,
            &format!("/api/profiles/{created_profile_id}"),
            Some(json!({ "name": "Focused", "color": "#4f84a5" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            updated
                .get("profiles")
                .and_then(Value::as_array)
                .is_some_and(|profiles| profiles.iter().any(|profile| {
                    profile.get("id").and_then(Value::as_str) == Some(&created_profile_id)
                        && profile.get("name").and_then(Value::as_str) == Some("Focused")
                        && profile.get("color").and_then(Value::as_str) == Some("#4f84a5")
                })),
            "updated profile response should include edited profile fields: {updated}"
        );

        let (status, selected) = request_json(
            app.clone(),
            Method::POST,
            "/api/profiles/select",
            Some(json!({ "profile_id": crate::settings::DEFAULT_PROFILE_ID })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            selected.get("active_profile_id").and_then(Value::as_str),
            Some(crate::settings::DEFAULT_PROFILE_ID)
        );

        #[cfg(feature = "qobuz")]
        {
            let (status, qobuz_settings) = request_json(
                app.clone(),
                Method::POST,
                "/api/qobuz/settings",
                Some(json!({ "radio_enabled": false })),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                qobuz_settings.get("radio_enabled").and_then(Value::as_bool),
                Some(false)
            );
        }

        let smoke_source = json!({
            "kind": "qobuz_track",
            "track_id": 42_4242,
            "title": "Smoke Track",
            "artist": "Smoke Artist",
            "album": "Smoke Album",
            "album_id": "smoke-album",
            "image_url": null,
            "duration_secs": 180.0,
            "radio": false
        });

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/history/record",
            Some(json!({
                "source": smoke_source.clone(),
                "zone_id": crate::zones::LOCAL_ZONE_ID,
                "zone_name": "Local Core",
                "played_secs": 42.0,
                "duration_secs": 180.0,
                "completed": false,
                "counted": true,
                "radio": false
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, history) = request_json(
            app.clone(),
            Method::GET,
            "/api/history/recent?limit=1",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            history.as_array().is_some_and(|entries| entries
                .first()
                .is_some_and(
                    |entry| entry.get("title").and_then(Value::as_str) == Some("Smoke Track")
                )),
            "recent history should include the recorded track: {history}"
        );

        let (status, playlist) = request_json(
            app.clone(),
            Method::PUT,
            "/api/playlists/smoke-list",
            Some(json!({
                "name": "Smoke List",
                "createdAt": 1_700_000_000_000_i64,
                "updatedAt": 1_700_000_000_001_i64,
                "items": [smoke_source.clone()]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            playlist.get("id").and_then(Value::as_str),
            Some("smoke-list")
        );
        assert_eq!(
            playlist.get("name").and_then(Value::as_str),
            Some("Smoke List")
        );
        assert_eq!(
            playlist
                .get("items")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/playlists/smoke-list/played",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, recent_playlists) = request_json(
            app.clone(),
            Method::GET,
            "/api/playlists/recent?limit=1",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            recent_playlists
                .as_array()
                .is_some_and(|playlists| playlists.first().is_some_and(|playlist| {
                    playlist.get("playlist_id").and_then(Value::as_str) == Some("smoke-list")
                })),
            "recent playlists should include the played playlist: {recent_playlists}"
        );

        let (status, favorite) = request_json(
            app.clone(),
            Method::POST,
            "/api/library/favorite-albums",
            Some(json!({
                "id": "qobuz:smoke-album",
                "provider": "qobuz",
                "title": "Smoke Album",
                "album_artist": "Smoke Artist",
                "artist": null,
                "art_id": null,
                "image_url": "https://static.qobuz.com/images/smoke.jpg",
                "year": 2026,
                "is_qobuz": true,
                "qobuz_id": "smoke-album",
                "qobuz_album_id": "smoke-album",
                "hires": true
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            favorite.get("provider").and_then(Value::as_str),
            Some("qobuz")
        );
        assert_eq!(
            favorite.get("qobuz_album_id").and_then(Value::as_str),
            Some("smoke-album")
        );

        let (status, favorites) = request_json(
            app.clone(),
            Method::GET,
            "/api/library/favorite-albums",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            favorites
                .as_array()
                .is_some_and(|favorites| favorites.iter().any(|favorite| favorite
                    .get("qobuz_album_id")
                    .and_then(Value::as_str)
                    == Some("smoke-album"))),
            "favorite albums should include the Qobuz favorite: {favorites}"
        );

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/zones/select",
            Some(json!({ "zone_id": crate::zones::LOCAL_ZONE_ID })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let status = request_status(
            app.clone(),
            Method::POST,
            &format!("/api/zones/{}/rename", crate::zones::LOCAL_ZONE_ID),
            Some(json!({ "name": "Desk" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, zone_settings) = request_json(
            app.clone(),
            Method::POST,
            &format!("/api/zones/{}/settings", crate::zones::LOCAL_ZONE_ID),
            Some(json!({
                "airplay_default_volume_enabled": true,
                "airplay_default_volume": 0.35
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            zone_settings
                .get("airplay_default_volume")
                .and_then(Value::as_f64),
            Some(0.35)
        );

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/queue",
            Some(json!({ "queue": [], "expected_current": null })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, zone_queue) = request_json(
            app.clone(),
            Method::GET,
            &format!("/api/zones/{}/queue", crate::zones::LOCAL_ZONE_ID),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(zone_queue.as_array().map(Vec::len), Some(0));

        let (status, zone_status) = request_json(
            app.clone(),
            Method::GET,
            &format!("/api/zones/{}/status", crate::zones::LOCAL_ZONE_ID),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            zone_status.get("active_zone_id").and_then(Value::as_str),
            Some(crate::zones::LOCAL_ZONE_ID)
        );
        assert_eq!(
            zone_status.get("active_zone_name").and_then(Value::as_str),
            Some("Desk")
        );

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/queue/shuffle",
            Some(json!({ "expected_current": null })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/now-playing-queue",
            Some(json!({ "state": { "view": "smoke", "items": [1, 2, 3] } })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let status = request_status(
            app.clone(),
            Method::POST,
            &format!(
                "/api/zones/{}/now-playing-queue",
                crate::zones::LOCAL_ZONE_ID
            ),
            Some(json!({ "state": { "view": "zone-smoke", "items": [4, 5] } })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, zone_now_playing_queue) = request_json(
            app.clone(),
            Method::GET,
            &format!(
                "/api/zones/{}/now-playing-queue",
                crate::zones::LOCAL_ZONE_ID
            ),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            zone_now_playing_queue.get("state"),
            Some(&json!({ "view": "zone-smoke", "items": [4, 5] }))
        );

        let (status, queue) =
            request_json(app.clone(), Method::GET, "/api/now-playing-queue", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            queue.get("state"),
            Some(&json!({ "view": "zone-smoke", "items": [4, 5] }))
        );

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/config",
            Some(json!({
                "filter_type": "Linear",
                "target_rate": 96000,
                "upsampling_enabled": true,
                "exclusive": false,
                "dither_mode": "Off",
                "output_mode": "Pcm",
                "dsd_rules_enabled": false,
                "dsd_rules": [],
                "headroom_db": -6.0,
                "dsp_buffer_ms": 200
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let playback_settings = state
            .settings()
            .playback_for_zone(crate::zones::LOCAL_ZONE_ID);
        assert_eq!(
            playback_settings.filter_type.as_deref(),
            Some("SplitPhase128kE2v3")
        );
        assert_eq!(playback_settings.target_rate, Some(96_000));
        assert_eq!(playback_settings.upsampling_enabled, Some(true));
        assert_eq!(playback_settings.exclusive, Some(false));
        assert_eq!(playback_settings.dither_mode.as_deref(), Some("Auto"));
        assert_eq!(playback_settings.output_mode.as_deref(), Some("Pcm"));
        assert_eq!(playback_settings.headroom_db, Some(-4.0));
        assert_eq!(playback_settings.dsp_buffer_ms, Some(200));

        let eq = crate::audio::eq::EqConfig {
            enabled: true,
            preamp_db: -3.0,
            ..crate::audio::eq::EqConfig::default()
        };
        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/eq",
            Some(serde_json::to_value(&eq).expect("eq should serialize")),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, saved_eq) = request_json(app.clone(), Method::GET, "/api/eq", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(saved_eq.get("enabled").and_then(Value::as_bool), Some(true));
        assert_eq!(
            saved_eq.get("preamp_db").and_then(Value::as_f64),
            Some(-3.0)
        );

        let mut preset = serde_json::to_value(&eq)
            .expect("eq should serialize")
            .as_object()
            .expect("eq should serialize to an object")
            .clone();
        preset.insert("name".to_string(), json!("Smoke Preset"));
        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/eq/presets",
            Some(Value::Object(preset)),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, loaded_preset) = request_json(
            app.clone(),
            Method::GET,
            "/api/eq/presets/Smoke%20Preset",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            loaded_preset.get("preamp_db").and_then(Value::as_f64),
            Some(-3.0)
        );

        let status = request_status(
            app.clone(),
            Method::DELETE,
            "/api/eq/presets/Smoke%20Preset",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, removed_favorite) = request_json(
            app.clone(),
            Method::DELETE,
            "/api/library/favorite-albums",
            Some(json!({
                "id": "qobuz:smoke-album",
                "provider": "qobuz",
                "qobuz_id": "smoke-album",
                "qobuz_album_id": "smoke-album",
                "is_qobuz": true
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            removed_favorite.get("removed").and_then(Value::as_bool),
            Some(true)
        );

        let (status, removed_playlist) =
            request_json(app, Method::DELETE, "/api/playlists/smoke-list", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            removed_playlist.get("removed").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[tokio::test]
    async fn history_top_supports_read_only_profile_selection() {
        let state = app_state("api-history-top-profile");
        let app = create_router().with_state(state.clone());

        let (status, created) = request_json(
            app.clone(),
            Method::POST,
            "/api/profiles",
            Some(json!({ "name": "Henry" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let henry_profile_id = created
            .get("active_profile_id")
            .and_then(Value::as_str)
            .expect("created profile should include active id")
            .to_string();

        let (status, _) = request_json(
            app.clone(),
            Method::POST,
            "/api/profiles/select",
            Some(json!({ "profile_id": crate::settings::DEFAULT_PROFILE_ID })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let default_source = json!({
            "kind": "qobuz_track",
            "track_id": 1,
            "title": "Default Song",
            "artist": "Default Artist",
            "album": "Default Album",
            "album_id": "default-album",
            "image_url": null,
            "duration_secs": 180.0,
            "radio": false
        });
        let henry_source = json!({
            "kind": "qobuz_track",
            "track_id": 2,
            "title": "Henry Song",
            "artist": "Henry Artist",
            "album": "Henry Album",
            "album_id": "henry-album",
            "image_url": null,
            "duration_secs": 180.0,
            "radio": false
        });

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/history/record",
            Some(json!({
                "source": default_source,
                "zone_id": crate::zones::LOCAL_ZONE_ID,
                "zone_name": "Local Core",
                "played_secs": 90.0,
                "duration_secs": 180.0,
                "completed": false,
                "counted": true,
                "radio": false
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/history/record",
            Some(json!({
                "profile_id": henry_profile_id,
                "source": henry_source,
                "zone_id": crate::zones::LOCAL_ZONE_ID,
                "zone_name": "Local Core",
                "played_secs": 120.0,
                "duration_secs": 180.0,
                "completed": false,
                "counted": true,
                "radio": false
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, active_top) =
            request_json(app.clone(), Method::GET, "/api/history/top", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            active_top.pointer("/profile/id").and_then(Value::as_str),
            Some(crate::settings::DEFAULT_PROFILE_ID)
        );
        assert_eq!(
            active_top.pointer("/items/0/title").and_then(Value::as_str),
            Some("Default Song")
        );

        let (status, henry_top) = request_json(
            app.clone(),
            Method::GET,
            &format!("/api/history/top?profile_id={henry_profile_id}&limit=500"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            henry_top.pointer("/profile/name").and_then(Value::as_str),
            Some("Henry")
        );
        assert_eq!(
            henry_top.pointer("/items/0/title").and_then(Value::as_str),
            Some("Henry Song")
        );

        let (status, profiles) =
            request_json(app.clone(), Method::GET, "/api/profiles", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            profiles.get("active_profile_id").and_then(Value::as_str),
            Some(crate::settings::DEFAULT_PROFILE_ID)
        );

        let status = request_status(
            app.clone(),
            Method::GET,
            "/api/history/top?profile_id=missing",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn critical_write_routes_reject_invalid_requests_before_side_effects() {
        let app = create_router().with_state(app_state("api-write-validation-smoke"));

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/config",
            Some(json!({
                "filter_type": "Linear",
                "target_rate": 12345,
                "upsampling_enabled": true,
                "exclusive": false,
                "dither_mode": "Off",
                "output_mode": "Pcm",
                "dsd_rules_enabled": false,
                "dsd_rules": [],
                "headroom_db": 0.0
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/config",
            Some(json!({
                "filter_type": "Linear",
                "target_rate": 96000,
                "upsampling_enabled": true,
                "exclusive": false,
                "dither_mode": "Off",
                "output_mode": "Pcm",
                "dsd_rules_enabled": false,
                "dsd_rules": [],
                "headroom_db": 0.0,
                "dsp_buffer_ms": 1001
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let status = request_status(
            app.clone(),
            Method::POST,
            &format!("/api/zones/{}/settings", crate::zones::LOCAL_ZONE_ID),
            Some(json!({ "airplay_default_volume_enabled": true })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/zones/missing-zone/queue",
            Some(json!({ "queue": [], "expected_current": null })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let status = request_status(
            app.clone(),
            Method::GET,
            "/api/zones/missing-zone/status",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let status = request_status(app.clone(), Method::GET, "/api/cover", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let status =
            request_status(app.clone(), Method::GET, "/api/library/art/999999", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let status = request_status(
            app.clone(),
            Method::GET,
            "/api/files/missing.flac/cover",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/profiles",
            Some(json!({ "name": "   " })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        #[cfg(feature = "qobuz")]
        {
            let status = request_status(
                app.clone(),
                Method::POST,
                "/api/qobuz/radio/next",
                Some(json!({ "exclude_track_ids": [1, 2], "limit": 10 })),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/artist-radio/play",
            Some(json!({ "mode": "auto" })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/zones/local/artist-radio/play",
            Some(json!({ "artist_name": "Radiohead", "mode": "weird" })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/lastfm/radio/test",
            Some(json!({ "limit": 10 })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let status = request_status(
            app.clone(),
            Method::POST,
            "/api/lastfm/radio/test",
            Some(json!({
                "seed": { "title": "Believe", "artist": "Cher" },
                "limit": 10
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let mut invalid_preset = serde_json::to_value(crate::audio::eq::EqConfig::default())
            .expect("eq should serialize")
            .as_object()
            .expect("eq should serialize to an object")
            .clone();
        invalid_preset.insert("name".to_string(), json!("../bad"));
        let status = request_status(
            app,
            Method::POST,
            "/api/eq/presets",
            Some(Value::Object(invalid_preset)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
