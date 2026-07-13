use super::playback_sequence::playback_request_sequence_from_headers;
use crate::api::error::ApiError;
use crate::app::auth::ProfileContext;
use crate::app::state::AppState;
use crate::playback::control::{
    next_for_active_zone, pause_for_active_zone, resume_for_active_zone, seek_active_zone,
    set_active_device_volume, set_active_volume, set_loop_mode_for_active_zone, stop_active_zone,
};
use crate::playback::error::PlaybackError;
use crate::playback::local::play_file_request_for_active_zone_with_profile;
use crate::playback::resolver::QueueRequestItem;
use crate::protocol::PlaylistContext;
use axum::http::HeaderMap;
use axum::{
    Json, Router,
    extract::{Extension, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
};
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct PlayRequest {
    pub file_name: Option<String>,
    pub track_id: Option<i64>,
    #[serde(default)]
    pub playlist_context: Option<PlaylistContext>,
    /// Tracks to enqueue after the requested track. Replaces any existing queue.
    #[serde(default)]
    pub queue: Vec<QueueRequestItem>,
}

#[derive(Deserialize)]
pub(super) struct SeekRequest {
    pub seconds: f64,
}

#[derive(Deserialize)]
pub(super) struct LoopModeRequest {
    pub mode: String,
}

#[derive(Deserialize)]
pub(super) struct VolumeRequest {
    pub volume: f32,
}

#[derive(Deserialize)]
pub(super) struct DeviceVolumeRequest {
    pub volume: f32,
}

pub fn routes() -> Router<AppState> {
    playback_routes().route("/api/device-volume", post(set_device_volume))
}

/// Active-zone routes safe for remote sessions. Device-volume remains LAN-only
/// because the active local zone may proxy it to a configured Hegel TCP target.
pub fn remote_routes() -> Router<AppState> {
    playback_routes()
}

fn playback_routes() -> Router<AppState> {
    Router::new()
        .route("/api/play", post(play_file))
        .route("/api/next", post(next_track))
        .route("/api/loop-mode", post(set_loop_mode))
        .route("/api/pause", post(pause_playback))
        .route("/api/resume", post(resume_playback))
        .route("/api/stop", post(stop_playback))
        .route("/api/seek", post(seek_playback))
        .route("/api/volume", post(set_volume))
}

async fn play_file(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    headers: HeaderMap,
    Json(req): Json<PlayRequest>,
) -> impl IntoResponse {
    let sequence = playback_request_sequence_from_headers(&headers);
    let profile_id = profile
        .map(|Extension(profile)| profile.id)
        .unwrap_or_else(|| state.settings().active_profile_id());
    playback_result_response(
        play_file_request_for_active_zone_with_profile(
            &state,
            &profile_id,
            sequence,
            req.track_id,
            req.file_name.as_deref(),
            req.playlist_context,
            &req.queue,
        )
        .await,
    )
}

async fn next_track(State(state): State<AppState>) -> impl IntoResponse {
    playback_result_response(next_for_active_zone(&state).await)
}

async fn set_loop_mode(
    State(state): State<AppState>,
    Json(req): Json<LoopModeRequest>,
) -> impl IntoResponse {
    playback_result_response(set_loop_mode_for_active_zone(&state, &req.mode))
}

async fn pause_playback(State(state): State<AppState>) -> impl IntoResponse {
    playback_result_response(pause_for_active_zone(&state).await)
}

async fn resume_playback(State(state): State<AppState>) -> impl IntoResponse {
    playback_result_response(resume_for_active_zone(&state).await)
}

async fn stop_playback(State(state): State<AppState>) -> impl IntoResponse {
    playback_result_response(stop_active_zone(&state).await)
}

async fn seek_playback(
    State(state): State<AppState>,
    Json(req): Json<SeekRequest>,
) -> impl IntoResponse {
    playback_result_response(seek_active_zone(&state, req.seconds).await)
}

async fn set_volume(
    State(state): State<AppState>,
    Json(req): Json<VolumeRequest>,
) -> impl IntoResponse {
    playback_result_response(set_active_volume(&state, req.volume).await)
}

async fn set_device_volume(
    State(state): State<AppState>,
    Json(req): Json<DeviceVolumeRequest>,
) -> impl IntoResponse {
    playback_result_response(set_active_device_volume(&state, req.volume).await)
}

fn playback_result_response(result: Result<(), PlaybackError>) -> axum::response::Response {
    match result {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}
