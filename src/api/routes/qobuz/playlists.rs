use super::{REMOTE_GRID_ARTWORK_SIZE, artwork_json, internal_error};
use crate::app::auth::RequestSurface;
use crate::app::state::AppState;
use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct QobuzFeaturedPlaylistsQuery {
    limit: Option<u32>,
    offset: Option<u32>,
    genre_id: Option<u64>,
    tag: Option<String>,
}

pub(super) async fn qobuz_featured_playlists(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
    Query(query): Query<QobuzFeaturedPlaylistsQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let response = state
        .qobuz()
        .featured_playlists(
            query.limit.unwrap_or(12),
            query.offset.unwrap_or(0),
            query.genre_id,
            query.tag.as_deref(),
        )
        .await
        .map_err(internal_error)?;
    artwork_json(response, surface, REMOTE_GRID_ARTWORK_SIZE).map_err(internal_error)
}

pub(super) async fn qobuz_playlist_tags(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .qobuz()
        .playlist_tags()
        .await
        .map(Json)
        .map_err(internal_error)
}

pub(super) async fn qobuz_genres(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .qobuz()
        .genres()
        .await
        .map(Json)
        .map_err(internal_error)
}

pub(super) async fn qobuz_playlist_detail(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let response = state
        .qobuz()
        .playlist_detail(&id)
        .await
        .map_err(internal_error)?;
    artwork_json(response, surface, REMOTE_GRID_ARTWORK_SIZE).map_err(internal_error)
}
