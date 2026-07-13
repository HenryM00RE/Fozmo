use super::internal_error;
use crate::app::state::AppState;
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};

pub(super) async fn qobuz_cache_info(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    Ok(Json(state.qobuz().cache_info()))
}

pub(super) async fn qobuz_cache_clear(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .qobuz()
        .clear_cache()
        .await
        .map(Json)
        .map_err(internal_error)
}
