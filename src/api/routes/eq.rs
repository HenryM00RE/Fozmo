use crate::app::state::AppState;
use crate::audio::eq::EqConfig;
use crate::playback::service::{
    apply_active_eq_config, apply_eq_config_for_zone, playback_config_for_zone,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/eq", get(get_eq).post(set_eq))
        .route("/api/zones/:zone_id/eq", get(get_zone_eq).post(set_zone_eq))
}

async fn get_eq(State(state): State<AppState>) -> Json<EqConfig> {
    let zone_id = state.zones().active_zone_id();
    let player = state.zones().active_player();
    Json(playback_config_for_zone(&state, &zone_id, &player).eq)
}

async fn set_eq(
    State(state): State<AppState>,
    Json(req): Json<EqConfig>,
) -> Result<StatusCode, (StatusCode, String)> {
    apply_active_eq_config(&state, req)
        .map_err(|error| internal_error(error.message().to_string()))?;
    Ok(StatusCode::OK)
}

async fn get_zone_eq(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
) -> Result<Json<EqConfig>, (StatusCode, String)> {
    if state.zones().zone_protocol(&zone_id).is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            format!("Zone '{zone_id}' is not available"),
        ));
    }
    let player = state
        .zones()
        .player_for_zone(&zone_id)
        .unwrap_or_else(|| state.zones().active_player());
    Ok(Json(playback_config_for_zone(&state, &zone_id, &player).eq))
}

async fn set_zone_eq(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    Json(req): Json<EqConfig>,
) -> Result<StatusCode, (StatusCode, String)> {
    if state.zones().zone_protocol(&zone_id).is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            format!("Zone '{zone_id}' is not available"),
        ));
    }
    apply_eq_config_for_zone(&state, &zone_id, req)
        .map_err(|error| internal_error(error.message().to_string()))?;
    Ok(StatusCode::OK)
}
use super::internal_error;
