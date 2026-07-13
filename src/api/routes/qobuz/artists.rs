use super::{REMOTE_GRID_ARTWORK_SIZE, artwork_json, internal_error};
use crate::app::auth::RequestSurface;
use crate::app::state::AppState;
use axum::{
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
};
use serde::Deserialize;

const ARTIST_IMAGE_CACHE_CONTROL: &str = "public, max-age=86400, stale-while-revalidate=604800";

#[derive(Deserialize)]
pub(super) struct ArtistSearchQuery {
    q: Option<String>,
    limit: Option<u32>,
}

pub(super) async fn qobuz_search_artists(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
    Query(query): Query<ArtistSearchQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let response = state
        .qobuz()
        .search_artists(query.q.as_deref().unwrap_or(""), query.limit.unwrap_or(10))
        .await
        .map_err(internal_error)?;
    artwork_json(response, surface, REMOTE_GRID_ARTWORK_SIZE).map_err(internal_error)
}

pub(super) async fn qobuz_artist_image(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
    Query(query): Query<ArtistSearchQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let response = state
        .qobuz()
        .artist_image(query.q.as_deref().unwrap_or(""))
        .await
        .map_err(internal_error)?;
    artwork_json(response, surface, REMOTE_GRID_ARTWORK_SIZE).map_err(internal_error)
}

pub(super) async fn qobuz_artist_image_cache(
    State(state): State<AppState>,
    Query(query): Query<ArtistSearchQuery>,
) -> impl IntoResponse {
    let q = query.q.as_deref().unwrap_or("");
    match state.qobuz().cached_artist_portrait(q) {
        Some((mime, data)) => artist_image_bytes_response(&mime, data),
        None => (StatusCode::NOT_FOUND, "No cached artist portrait").into_response(),
    }
}

pub(super) async fn qobuz_artist_detail(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
    Path(id): Path<u64>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let response = state
        .qobuz()
        .artist_detail(id)
        .await
        .map_err(internal_error)?;
    artwork_json(response, surface, REMOTE_GRID_ARTWORK_SIZE).map_err(internal_error)
}

fn artist_image_bytes_response(mime: &str, data: Vec<u8>) -> axum::response::Response {
    let mut headers = HeaderMap::new();
    let mime = HeaderValue::from_str(mime)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    headers.insert(header::CONTENT_TYPE, mime);
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(ARTIST_IMAGE_CACHE_CONTROL),
    );
    (StatusCode::OK, headers, data).into_response()
}

pub(super) async fn qobuz_artist_core(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
    Path(id): Path<u64>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let response = state
        .qobuz()
        .artist_core(id)
        .await
        .map_err(internal_error)?;
    artwork_json(response, surface, REMOTE_GRID_ARTWORK_SIZE).map_err(internal_error)
}

pub(super) async fn qobuz_artist_top_tracks(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
    Path(id): Path<u64>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let response = state
        .qobuz()
        .artist_top_tracks(id)
        .await
        .map_err(internal_error)?;
    artwork_json(response, surface, REMOTE_GRID_ARTWORK_SIZE).map_err(internal_error)
}

pub(super) async fn qobuz_artist_similar(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
    Path(id): Path<u64>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let response = state
        .qobuz()
        .artist_similar(id)
        .await
        .map_err(internal_error)?;
    artwork_json(response, surface, REMOTE_GRID_ARTWORK_SIZE).map_err(internal_error)
}
