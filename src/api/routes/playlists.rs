use crate::api::error::{ApiError, ApiResult};
use crate::app::state::AppState;
use crate::library::PlaylistSaveRequest;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, put},
};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
pub struct RecentPlaylistsQuery {
    pub limit: Option<i64>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/playlists", get(playlists))
        .route("/api/playlists/recent", get(recent_playlists))
        .route(
            "/api/playlists/:id",
            put(save_playlist).delete(delete_playlist),
        )
        .route(
            "/api/playlists/:id/played",
            axum::routing::post(record_playlist_played),
        )
}

async fn playlists(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    state
        .library()
        .run_blocking(|library| library.playlists())
        .await
        .map(Json)
        .map_err(ApiError::internal)
}

async fn save_playlist(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<PlaylistSaveRequest>,
) -> ApiResult<impl IntoResponse> {
    state
        .library()
        .run_blocking(move |library| library.save_playlist(&id, req))
        .await
        .map(Json)
        .map_err(ApiError::internal)
}

async fn delete_playlist(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    state
        .library()
        .run_blocking(move |library| library.delete_playlist(&id))
        .await
        .map(|removed| Json(serde_json::json!({ "removed": removed })))
        .map_err(ApiError::internal)
}

async fn record_playlist_played(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    state
        .library()
        .run_blocking(move |library| library.record_playlist_played(&id))
        .await
        .map(|_| StatusCode::CREATED)
        .map_err(ApiError::internal)
}

async fn recent_playlists(
    State(state): State<AppState>,
    Query(query): Query<RecentPlaylistsQuery>,
) -> ApiResult<impl IntoResponse> {
    let limit = query.limit.unwrap_or(50);
    state
        .library()
        .run_blocking(move |library| library.recent_playlists(limit))
        .await
        .map(Json)
        .map_err(ApiError::internal)
}
