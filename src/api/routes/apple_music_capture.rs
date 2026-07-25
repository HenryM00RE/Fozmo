use crate::api::error::ApiError;
use crate::app::state::AppState;
use crate::protocol::SinkProtocol;
use crate::services::apple_music::{
    AppleMusicAppControlRequest, AppleMusicCaptureRateRequest, AppleMusicCaptureSettingsUpdate,
    StartAppleMusicCaptureRequest, apply_settings_update, sanitize_settings,
};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/apple-music-capture/status", get(status))
        .route("/api/apple-music-capture/settings", get(settings))
        .route("/api/apple-music-capture/settings", post(update_settings))
        .route("/api/apple-music-capture/devices", get(devices))
        .route("/api/apple-music-capture/start", post(start))
        .route("/api/apple-music-capture/stop", post(stop))
        .route("/api/apple-music-capture/rate", post(set_rate))
        .route("/api/apple-music-capture/metrics", get(metrics))
        .route(
            "/api/apple-music-capture/music-app/status",
            get(music_app_status),
        )
        .route(
            "/api/apple-music-capture/music-app/control",
            post(control_music_app),
        )
}

async fn status(State(state): State<AppState>) -> impl IntoResponse {
    let settings = sanitized_capture_settings(&state);
    Json(state.apple_music_capture().status(&settings))
}

async fn settings(State(state): State<AppState>) -> impl IntoResponse {
    let settings = sanitized_capture_settings(&state);
    Json(state.apple_music_capture().settings_payload(&settings))
}

async fn update_settings(
    State(state): State<AppState>,
    Json(update): Json<AppleMusicCaptureSettingsUpdate>,
) -> impl IntoResponse {
    let _ = state.settings().update(move |settings| {
        apply_settings_update(&mut settings.apple_music_capture, update);
    });
    let settings = state.settings().apple_music_capture_settings();
    Json(state.apple_music_capture().settings_payload(&settings))
}

async fn devices(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.apple_music_capture().devices())
}

async fn start(
    State(state): State<AppState>,
    Json(request): Json<StartAppleMusicCaptureRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let _ = state.settings().update(|settings| {
        sanitize_settings(&mut settings.apple_music_capture);
    });
    let zone_id = request
        .zone_id
        .as_deref()
        .map(str::trim)
        .filter(|zone_id| !zone_id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| state.zones().active_zone_id());
    if state.zones().zone_protocol(&zone_id) != Some(SinkProtocol::LocalCoreAudio) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "Apple Music lossless capture requires a local CoreAudio zone.".to_string(),
        ));
    }
    let player = state.zones().player_for_zone(&zone_id).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("The selected local zone '{zone_id}' is not available."),
        )
    })?;
    let settings = sanitized_capture_settings(&state);
    state
        .apple_music_capture()
        .start(player, &settings, request)
        .map(Json)
        .map_err(|message| ApiError::new(StatusCode::BAD_REQUEST, message))
}

async fn stop(State(state): State<AppState>) -> impl IntoResponse {
    let settings = sanitized_capture_settings(&state);
    Json(state.apple_music_capture().stop(&settings))
}

/// Manual rate override for streaming tracks whose sample rate Apple Music
/// does not report.
async fn set_rate(
    State(state): State<AppState>,
    Json(request): Json<AppleMusicCaptureRateRequest>,
) -> Result<impl IntoResponse, ApiError> {
    state
        .apple_music_capture()
        .set_manual_rate(request.rate_hz)
        .map_err(|message| ApiError::new(StatusCode::BAD_REQUEST, message))?;
    let settings = sanitized_capture_settings(&state);
    Ok(Json(state.apple_music_capture().status(&settings)))
}

async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let settings = sanitized_capture_settings(&state);
    Json(state.apple_music_capture().status(&settings))
}

async fn music_app_status(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.apple_music_capture().music_app_status())
}

async fn control_music_app(
    State(state): State<AppState>,
    Json(request): Json<AppleMusicAppControlRequest>,
) -> Result<impl IntoResponse, ApiError> {
    state
        .apple_music_capture()
        .control_music_app(request.command)
        .map(Json)
        .map_err(|message| ApiError::new(StatusCode::BAD_REQUEST, message))
}

fn sanitized_capture_settings(state: &AppState) -> crate::settings::AppleMusicCaptureSettings {
    let mut settings = state.settings().apple_music_capture_settings();
    sanitize_settings(&mut settings);
    settings
}
