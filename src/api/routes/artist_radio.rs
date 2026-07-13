use super::playback_sequence::playback_request_sequence_from_headers;
use crate::api::error::ApiError;
use crate::app::auth::ProfileContext;
use crate::app::state::AppState;
use crate::playback::artist_radio::{
    ArtistRadioMode, play_artist_radio_for_active_zone_with_profile,
    play_artist_radio_for_zone_with_profile,
};
use axum::{
    Json, Router,
    extract::{Extension, Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct ArtistRadioPlayRequest {
    artist_name: Option<String>,
    mode: Option<String>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/artist-radio/play", post(play_artist_radio))
        .route(
            "/api/zones/:zone_id/artist-radio/play",
            post(play_artist_radio_in_zone),
        )
}

async fn play_artist_radio(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    headers: HeaderMap,
    Json(req): Json<ArtistRadioPlayRequest>,
) -> impl IntoResponse {
    let sequence = playback_request_sequence_from_headers(&headers);
    let result =
        artist_radio_request_parts(req).map(|(artist_name, mode)| (artist_name, mode, sequence));
    let (artist_name, mode, sequence) = match result {
        Ok(parts) => parts,
        Err(error) => return error.into_response(),
    };
    let profile_id = profile
        .map(|Extension(profile)| profile.id)
        .unwrap_or_else(|| state.settings().active_profile_id());
    match play_artist_radio_for_active_zone_with_profile(
        state,
        &profile_id,
        sequence,
        &artist_name,
        mode,
    )
    .await
    {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

async fn play_artist_radio_in_zone(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    profile: Option<Extension<ProfileContext>>,
    headers: HeaderMap,
    Json(req): Json<ArtistRadioPlayRequest>,
) -> impl IntoResponse {
    let sequence = playback_request_sequence_from_headers(&headers);
    let (artist_name, mode) = match artist_radio_request_parts(req) {
        Ok(parts) => parts,
        Err(error) => return error.into_response(),
    };
    let profile_id = profile
        .map(|Extension(profile)| profile.id)
        .unwrap_or_else(|| state.settings().active_profile_id());
    match play_artist_radio_for_zone_with_profile(
        state,
        &zone_id,
        &profile_id,
        sequence,
        &artist_name,
        mode,
    )
    .await
    {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

fn artist_radio_request_parts(
    req: ArtistRadioPlayRequest,
) -> Result<(String, ArtistRadioMode), ApiError> {
    let artist_name = req
        .artist_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "artist_name is required"))?;
    let mode = ArtistRadioMode::parse(req.mode.as_deref()).map_err(ApiError::from)?;
    Ok((artist_name, mode))
}
