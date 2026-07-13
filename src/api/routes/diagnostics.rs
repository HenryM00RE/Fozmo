use crate::api::error::ApiError;
use crate::app::state::AppState;
use crate::diagnostics::status::{PopDiagnosticEntry, PopDiagnosticsExport};
use crate::playback::error::PlaybackError;
use crate::playback::status::{build_status_response, refresh_active_output_status};
use crate::playback::upnp::upnp_target_for_zone;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/diagnostics/pop", post(log_pop))
        .route("/api/diagnostics/pop-log", get(export_pop_log))
        .route("/api/diagnostics/export", get(export_pop_log))
        .route("/api/diagnostics/upnp/:zone_id", get(upnp_diagnostics))
}

async fn log_pop(State(state): State<AppState>) -> Json<PopDiagnosticEntry> {
    let response = build_status_response(&state);
    let entry = state.diagnostics().record_pop_snapshot(&response);
    let refresh_state = state.clone();
    tokio::spawn(async move {
        refresh_active_output_status(&refresh_state).await;
    });
    Json(entry)
}

async fn export_pop_log(State(state): State<AppState>) -> Json<PopDiagnosticsExport> {
    Json(state.diagnostics().export_pop_log())
}

async fn upnp_diagnostics(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
) -> Result<Json<crate::audio::upnp::UpnpDiagnostics>, ApiError> {
    let target = upnp_target_for_zone(&state, &zone_id).map_err(diagnostics_error)?;
    Ok(Json(state.upnp().diagnostics_for_zone(
        &zone_id,
        state.public_base_url().clone(),
        target,
    )))
}

fn diagnostics_error(error: PlaybackError) -> ApiError {
    match error {
        PlaybackError::Conflict(message) => ApiError::new(StatusCode::CONFLICT, message),
        PlaybackError::BadRequest(message) => ApiError::new(StatusCode::BAD_REQUEST, message),
        PlaybackError::Forbidden(message) => ApiError::new(StatusCode::FORBIDDEN, message),
        PlaybackError::NotFound(message) => ApiError::new(StatusCode::NOT_FOUND, message),
        PlaybackError::ZoneNotAvailable => {
            ApiError::new(StatusCode::NOT_FOUND, "Zone not available")
        }
        PlaybackError::Library(error)
        | PlaybackError::Integration(error)
        | PlaybackError::RetryableNetwork(error)
        | PlaybackError::Persistence(error)
        | PlaybackError::InternalInvariant(error) => ApiError::internal(error.to_string()),
    }
}
