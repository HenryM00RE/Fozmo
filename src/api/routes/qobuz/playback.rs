use super::super::playback_sequence::playback_request_sequence_from_headers;
use crate::api::error::ApiError;
use crate::app::auth::ProfileContext;
use crate::app::state::AppState;
use crate::playback::qobuz::{
    play_qobuz_request_for_active_zone_with_profile, prefetch_qobuz_request_for_active_zone,
};
use crate::services::qobuz::QobuzPlayRequest;
use axum::{
    Json,
    extract::{Extension, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};

pub(super) async fn qobuz_play(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    headers: HeaderMap,
    Json(req): Json<QobuzPlayRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let sequence = playback_request_sequence_from_headers(&headers);
    let profile_id = profile
        .map(|Extension(profile)| profile.id)
        .unwrap_or_else(|| state.settings().active_profile_id());
    play_qobuz_request_for_active_zone_with_profile(state, &profile_id, sequence, req).await?;
    Ok(StatusCode::OK)
}

pub(super) async fn qobuz_prefetch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<QobuzPlayRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let sequence = playback_request_sequence_from_headers(&headers);
    prefetch_qobuz_request_for_active_zone(state, sequence, req).await?;
    Ok(StatusCode::ACCEPTED)
}
