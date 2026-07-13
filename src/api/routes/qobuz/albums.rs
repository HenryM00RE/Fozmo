use super::{REMOTE_DETAIL_ARTWORK_SIZE, REMOTE_GRID_ARTWORK_SIZE, artwork_json, internal_error};
use crate::app::auth::{ProfileContext, RequestSurface};
use crate::app::state::AppState;
use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
};

pub(super) async fn qobuz_favorite_albums(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let response = state
        .qobuz()
        .favorite_albums()
        .await
        .map_err(internal_error)?
        .albums;
    artwork_json(response, surface, REMOTE_GRID_ARTWORK_SIZE).map_err(internal_error)
}

pub(super) async fn qobuz_album_detail(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
    profile: Option<Extension<ProfileContext>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let mut detail = state
        .qobuz()
        .album_detail(&id)
        .await
        .map_err(internal_error)?;
    let keys = detail
        .tracks
        .iter()
        .map(|track| format!("qobuz:{}", track.id))
        .collect::<Vec<_>>();
    let profile_id = profile
        .map(|Extension(profile)| profile.id)
        .unwrap_or_else(|| state.settings().active_profile_id());
    let summaries = state
        .library()
        .run_blocking(move |library| {
            library.playback_summaries_for_keys_for_profile(&profile_id, &keys)
        })
        .await
        .map_err(internal_error)?;
    for track in &mut detail.tracks {
        if let Some(summary) = summaries.get(&format!("qobuz:{}", track.id)) {
            track.play_count = summary.play_count;
            track.last_played_at = summary.last_played_at;
            track.listened_secs = summary.listened_secs;
        }
    }
    artwork_json(detail, surface, REMOTE_DETAIL_ARTWORK_SIZE).map_err(internal_error)
}

pub(super) async fn qobuz_track_detail(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
    Path(id): Path<u64>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let response = state
        .qobuz()
        .track_detail(id)
        .await
        .map_err(internal_error)?;
    artwork_json(response, surface, REMOTE_GRID_ARTWORK_SIZE).map_err(internal_error)
}
