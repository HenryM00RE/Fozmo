use crate::app::state::AppState;
use crate::services::apple_music_musickit::{
    AppleMusicAuthorizeRequest, AppleMusicDevPlaySongRequest, AppleMusicMvpError,
    AppleMusicMvpStatus, AppleMusicProcessTapStartRequest, AppleMusicTransportRequest,
};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};

type AppleMusicApiResult =
    Result<Json<AppleMusicMvpStatus>, (StatusCode, Json<AppleMusicMvpError>)>;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/apple-music/status", get(status))
        .route("/api/apple-music/launch", post(launch))
        .route("/api/apple-music/authorize", post(authorize))
        .route("/api/apple-music/dev/play-song", post(play_song))
        .route("/api/apple-music/transport", post(transport))
        .route("/api/apple-music/stop", post(stop))
        .route("/api/apple-music/shutdown", post(shutdown))
        .route(
            "/api/apple-music/process-tap/start",
            post(start_process_tap),
        )
        .route("/api/apple-music/process-tap/stop", post(stop_process_tap))
}

async fn launch(State(state): State<AppState>) -> AppleMusicApiResult {
    state
        .apple_music()
        .launch()
        .await
        .map(Json)
        .map_err(api_error)
}

async fn status(State(state): State<AppState>) -> AppleMusicApiResult {
    state
        .apple_music()
        .refresh_status()
        .await
        .map(Json)
        .map_err(api_error)
}

async fn authorize(
    State(state): State<AppState>,
    Json(request): Json<AppleMusicAuthorizeRequest>,
) -> AppleMusicApiResult {
    state
        .apple_music()
        .authorize(request.present_ui)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn play_song(
    State(state): State<AppState>,
    Json(request): Json<AppleMusicDevPlaySongRequest>,
) -> AppleMusicApiResult {
    state
        .apple_music()
        .play_song(request.song_id, request.storefront)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn transport(
    State(state): State<AppState>,
    Json(request): Json<AppleMusicTransportRequest>,
) -> AppleMusicApiResult {
    state
        .apple_music()
        .transport(&request.command)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn stop(State(state): State<AppState>) -> AppleMusicApiResult {
    state
        .apple_music()
        .transport("stop")
        .await
        .map(Json)
        .map_err(api_error)
}

async fn shutdown(State(state): State<AppState>) -> AppleMusicApiResult {
    state
        .apple_music()
        .transport("shutdown")
        .await
        .map(Json)
        .map_err(api_error)
}

async fn start_process_tap(
    State(state): State<AppState>,
    Json(request): Json<AppleMusicProcessTapStartRequest>,
) -> AppleMusicApiResult {
    state
        .apple_music()
        .start_process_tap(
            state.zones().active_player(),
            request.confirm_system_audio_capture,
            request.mute_original_audio,
        )
        .map(Json)
        .map_err(api_error)
}

async fn stop_process_tap(State(state): State<AppState>) -> AppleMusicApiResult {
    Ok(Json(state.apple_music().stop_process_tap()))
}

fn api_error(error: AppleMusicMvpError) -> (StatusCode, Json<AppleMusicMvpError>) {
    let status = match error.code.as_str() {
        "helper_missing" | "song_not_found" => StatusCode::NOT_FOUND,
        "music_authorization_not_determined"
        | "music_authorization_denied"
        | "process_tap_confirmation_required" => StatusCode::FORBIDDEN,
        "session_limit_reached" | "process_tap_playback_changed" => StatusCode::CONFLICT,
        "music_app_not_running" => StatusCode::NOT_FOUND,
        "helper_launch_failed" | "helper_exited" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_REQUEST,
    };
    (status, Json(error))
}
