use super::{REMOTE_DETAIL_ARTWORK_SIZE, REMOTE_GRID_ARTWORK_SIZE, artwork_json, internal_error};
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
pub(crate) struct QobuzHomeSectionQuery {
    category: Option<String>,
    genre_id: Option<u64>,
    limit: Option<u32>,
    offset: Option<u32>,
}

pub(super) async fn qobuz_home(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let response = state.qobuz().home().await.map_err(internal_error)?;
    artwork_json(response, surface, REMOTE_GRID_ARTWORK_SIZE).map_err(internal_error)
}

pub(super) async fn qobuz_home_section(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
    Query(query): Query<QobuzHomeSectionQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let response = state
        .qobuz()
        .home_section(
            query.category.as_deref().unwrap_or("new"),
            query.genre_id,
            query.limit.unwrap_or(12),
            query.offset.unwrap_or(0),
        )
        .await
        .map_err(internal_error)?;
    artwork_json(response, surface, REMOTE_GRID_ARTWORK_SIZE).map_err(internal_error)
}

pub(super) async fn qobuz_home_album_of_the_week(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let response = state
        .qobuz()
        .home_album_of_the_week()
        .await
        .map_err(internal_error)?;
    artwork_json(response, surface, REMOTE_DETAIL_ARTWORK_SIZE).map_err(internal_error)
}
