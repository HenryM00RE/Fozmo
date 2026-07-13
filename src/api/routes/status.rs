use crate::app::auth::RequestSurface;
use crate::app::state::AppState;
use crate::playback::status::{
    StatusResponse, build_status_response, refresh_active_output_status,
};
use axum::{Extension, Json, Router, extract::State, routing::get};

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/status", get(get_status))
}

async fn get_status(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
) -> Json<StatusResponse> {
    let mut response = build_status_response(&state);
    response.surface = match surface.map(|Extension(surface)| surface) {
        Some(RequestSurface::Remote) => "remote".to_string(),
        _ => "local".to_string(),
    };
    let refresh_state = state.clone();
    tokio::spawn(async move {
        refresh_active_output_status(&refresh_state).await;
    });
    Json(response)
}
