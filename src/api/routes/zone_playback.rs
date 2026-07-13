use super::{playback_sequence::playback_request_sequence_from_headers, queue};
use crate::api::error::ApiError;
use crate::app::auth::ProfileContext;
use crate::app::state::AppState;
use crate::playback::control::{
    next_for_zone, pause_for_zone, resume_for_zone, seek_for_zone, set_device_volume_for_zone,
    set_loop_mode_for_zone, set_volume_for_zone, stop_for_zone,
};
use crate::playback::error::PlaybackError;
use crate::playback::local::play_file_request_for_zone_with_profile;
use crate::playback::qobuz::{
    play_qobuz_request_for_zone_with_profile, prefetch_qobuz_request_for_zone,
};
use crate::playback::resolver::QueueRequestItem;
use crate::playback::transfer::{TransferRequest, transfer_zone};
use crate::protocol::PlaylistContext;
use crate::services::qobuz::QobuzPlayRequest;
use axum::{
    Json, Router,
    extract::{Extension, Path, State},
    http::{HeaderMap, StatusCode},
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

pub fn routes() -> Router<AppState> {
    playback_routes().route(
        "/api/zones/:zone_id/device-volume",
        post(set_zone_device_volume),
    )
}

/// Zone playback routes safe for remote sessions. Device-volume remains
/// LAN-only because a local zone may proxy it to a configured Hegel TCP target.
pub fn remote_routes() -> Router<AppState> {
    playback_routes()
}

fn playback_routes() -> Router<AppState> {
    Router::new()
        .merge(queue::zone_routes())
        .route("/api/zones/:zone_id/play", post(play_file_in_zone))
        .route("/api/zones/:zone_id/qobuz/play", post(play_qobuz_in_zone))
        .route(
            "/api/zones/:zone_id/qobuz/prefetch",
            post(prefetch_qobuz_in_zone),
        )
        .route("/api/zones/:zone_id/pause", post(pause_zone))
        .route("/api/zones/:zone_id/resume", post(resume_zone))
        .route("/api/zones/:zone_id/stop", post(stop_zone))
        .route("/api/zones/:zone_id/seek", post(seek_zone))
        .route("/api/zones/:zone_id/next", post(next_zone))
        .route("/api/zones/:zone_id/transfer", post(transfer_zone_route))
        .route("/api/zones/:zone_id/loop-mode", post(set_zone_loop_mode))
        .route("/api/zones/:zone_id/volume", post(set_zone_volume))
}

async fn play_file_in_zone(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    profile: Option<Extension<ProfileContext>>,
    headers: HeaderMap,
    Json(req): Json<PlayRequest>,
) -> impl IntoResponse {
    let profile_id = profile
        .map(|Extension(profile)| profile.id)
        .unwrap_or_else(|| state.settings().active_profile_id());
    play_file_for_zone(&state, &zone_id, &profile_id, &headers, req).await
}

pub(super) async fn play_file_for_zone(
    state: &AppState,
    zone_id: &str,
    profile_id: &str,
    headers: &HeaderMap,
    req: PlayRequest,
) -> axum::response::Response {
    let sequence = playback_request_sequence_from_headers(headers);
    match play_file_request_for_zone_with_profile(
        state,
        zone_id,
        profile_id,
        sequence,
        req.track_id,
        req.file_name.as_deref(),
        req.playlist_context,
        &req.queue,
    )
    .await
    {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

async fn play_qobuz_in_zone(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    profile: Option<Extension<ProfileContext>>,
    headers: HeaderMap,
    Json(req): Json<QobuzPlayRequest>,
) -> impl IntoResponse {
    let sequence = playback_request_sequence_from_headers(&headers);
    let profile_id = profile
        .map(|Extension(profile)| profile.id)
        .unwrap_or_else(|| state.settings().active_profile_id());
    playback_result_response(
        play_qobuz_request_for_zone_with_profile(state, &zone_id, &profile_id, sequence, req).await,
    )
}

async fn prefetch_qobuz_in_zone(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<QobuzPlayRequest>,
) -> impl IntoResponse {
    let sequence = playback_request_sequence_from_headers(&headers);
    playback_result_response(prefetch_qobuz_request_for_zone(state, &zone_id, sequence, req).await)
}

async fn next_zone(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
) -> impl IntoResponse {
    next_zone_by_id(&state, &zone_id).await
}

pub(super) async fn next_zone_by_id(state: &AppState, zone_id: &str) -> axum::response::Response {
    playback_result_response(next_for_zone(state, zone_id).await)
}

async fn set_zone_loop_mode(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    Json(req): Json<LoopModeRequest>,
) -> impl IntoResponse {
    set_zone_loop_mode_by_id(&state, &zone_id, req)
}

pub(super) fn set_zone_loop_mode_by_id(
    state: &AppState,
    zone_id: &str,
    req: LoopModeRequest,
) -> axum::response::Response {
    playback_result_response(set_loop_mode_for_zone(state, zone_id, &req.mode))
}

async fn set_zone_volume(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    Json(req): Json<VolumeRequest>,
) -> impl IntoResponse {
    playback_result_response(set_volume_for_zone(&state, &zone_id, req.volume).await)
}

async fn set_zone_device_volume(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    Json(req): Json<VolumeRequest>,
) -> impl IntoResponse {
    playback_result_response(set_device_volume_for_zone(&state, &zone_id, req.volume).await)
}

async fn pause_zone(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
) -> impl IntoResponse {
    pause_zone_by_id(&state, &zone_id).await
}

pub(super) async fn pause_zone_by_id(state: &AppState, zone_id: &str) -> axum::response::Response {
    playback_result_response(pause_for_zone(state, zone_id).await)
}

async fn resume_zone(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
) -> impl IntoResponse {
    resume_zone_by_id(&state, &zone_id).await
}

pub(super) async fn resume_zone_by_id(state: &AppState, zone_id: &str) -> axum::response::Response {
    playback_result_response(resume_for_zone(state, zone_id).await)
}

async fn stop_zone(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
) -> impl IntoResponse {
    stop_zone_by_id(&state, &zone_id).await
}

pub(super) async fn stop_zone_by_id(state: &AppState, zone_id: &str) -> axum::response::Response {
    playback_result_response(stop_for_zone(state, zone_id).await)
}

async fn seek_zone(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    Json(req): Json<SeekRequest>,
) -> impl IntoResponse {
    seek_zone_by_id(&state, &zone_id, req.seconds).await
}

pub(super) async fn seek_zone_by_id(
    state: &AppState,
    zone_id: &str,
    seconds: f64,
) -> axum::response::Response {
    playback_result_response(seek_for_zone(state, zone_id, seconds).await)
}

async fn transfer_zone_route(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    Json(req): Json<TransferRequest>,
) -> impl IntoResponse {
    match transfer_zone(&state, &zone_id, req).await {
        Ok(response) => Json(response).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

fn playback_result_response(result: Result<(), PlaybackError>) -> axum::response::Response {
    match result {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}
