use crate::api::error::ApiError;
use crate::app::auth::ProfileContext;
use crate::app::state::AppState;
use crate::playback::error::PlaybackError;
use crate::playback::queue::{
    QueueMutationRequest, SetNowPlayingQueueRequest, SetQueueRequest,
    now_playing_queue_for_active_zone, now_playing_queue_for_zone,
    set_active_zone_queue_for_profile, set_now_playing_queue_for_active_zone,
    set_now_playing_queue_for_zone, set_zone_queue_by_id_for_profile,
    shuffle_active_zone_queue_for_profile, shuffle_zone_queue_by_id_for_profile,
    zone_queue_for_zone,
};
use axum::{
    Json, Router,
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/queue", post(set_queue))
        .route("/api/queue/shuffle", post(shuffle_queue))
        .route(
            "/api/now-playing-queue",
            get(get_now_playing_queue).post(set_now_playing_queue),
        )
}

pub fn zone_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/zones/:zone_id/queue",
            get(get_zone_queue).post(set_zone_queue),
        )
        .route(
            "/api/zones/:zone_id/queue/shuffle",
            post(shuffle_zone_queue),
        )
        .route(
            "/api/zones/:zone_id/now-playing-queue",
            get(get_zone_now_playing_queue).post(set_zone_now_playing_queue),
        )
}

async fn set_queue(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    Json(req): Json<SetQueueRequest>,
) -> impl IntoResponse {
    let profile_id = request_profile_id(&state, profile);
    playback_result_response(set_active_zone_queue_for_profile(&state, &profile_id, req).await)
}

async fn get_zone_queue(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
) -> impl IntoResponse {
    match zone_queue_for_zone(&state, &zone_id) {
        Ok(queue) => Json(queue).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

async fn set_zone_queue(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    profile: Option<Extension<ProfileContext>>,
    Json(req): Json<SetQueueRequest>,
) -> impl IntoResponse {
    let profile_id = request_profile_id(&state, profile);
    playback_result_response(
        set_zone_queue_by_id_for_profile(&state, &zone_id, &profile_id, req).await,
    )
}

async fn shuffle_queue(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    Json(req): Json<QueueMutationRequest>,
) -> impl IntoResponse {
    let profile_id = request_profile_id(&state, profile);
    playback_result_response(shuffle_active_zone_queue_for_profile(&state, &profile_id, req).await)
}

async fn shuffle_zone_queue(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    profile: Option<Extension<ProfileContext>>,
    Json(req): Json<QueueMutationRequest>,
) -> impl IntoResponse {
    let profile_id = request_profile_id(&state, profile);
    playback_result_response(
        shuffle_zone_queue_by_id_for_profile(&state, &zone_id, &profile_id, req).await,
    )
}

async fn get_now_playing_queue(State(state): State<AppState>) -> impl IntoResponse {
    get_active_now_playing_queue_response(&state)
}

async fn set_now_playing_queue(
    State(state): State<AppState>,
    Json(req): Json<SetNowPlayingQueueRequest>,
) -> impl IntoResponse {
    playback_result_response(set_now_playing_queue_for_active_zone(&state, req))
}

async fn get_zone_now_playing_queue(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
) -> impl IntoResponse {
    get_now_playing_queue_response(&state, &zone_id)
}

async fn set_zone_now_playing_queue(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    Json(req): Json<SetNowPlayingQueueRequest>,
) -> impl IntoResponse {
    playback_result_response(set_now_playing_queue_for_zone(&state, &zone_id, req))
}

fn get_now_playing_queue_response(state: &AppState, zone_id: &str) -> axum::response::Response {
    match now_playing_queue_for_zone(state, zone_id) {
        Ok(queue) => Json(queue).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

fn get_active_now_playing_queue_response(state: &AppState) -> axum::response::Response {
    match now_playing_queue_for_active_zone(state) {
        Ok(queue) => Json(queue).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

fn playback_result_response(result: Result<(), PlaybackError>) -> axum::response::Response {
    match result {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

fn request_profile_id(state: &AppState, profile: Option<Extension<ProfileContext>>) -> String {
    profile
        .map(|Extension(profile)| profile.id)
        .unwrap_or_else(|| state.settings().active_profile_id())
}
