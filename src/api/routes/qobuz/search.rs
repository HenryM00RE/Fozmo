use super::{REMOTE_GRID_ARTWORK_SIZE, artwork_json, internal_error};
use crate::app::auth::RequestSurface;
use crate::app::state::AppState;
use axum::{
    extract::{Extension, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
pub(crate) struct QobuzSearchQuery {
    q: Option<String>,
}

pub(super) async fn qobuz_search(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
    Query(query): Query<QobuzSearchQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let response = state
        .qobuz()
        .search_tracks(query.q.as_deref().unwrap_or(""))
        .await
        .map_err(internal_error)?;
    artwork_json(response, surface, REMOTE_GRID_ARTWORK_SIZE).map_err(internal_error)
}

pub(super) async fn qobuz_search_albums(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
    Query(query): Query<QobuzSearchQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let response = state
        .qobuz()
        .search_albums(query.q.as_deref().unwrap_or(""))
        .await
        .map_err(internal_error)?;
    artwork_json(response, surface, REMOTE_GRID_ARTWORK_SIZE).map_err(internal_error)
}
