use crate::api::error::ApiError;
use crate::app::state::AppState;
use crate::playback::service::{
    normalize_hegel_settings, query_hegel_status_for_target, set_hegel_input_for_target,
    set_hegel_mute_for_target, set_hegel_power_for_target, set_hegel_volume_for_target,
    update_hegel_settings,
};
use crate::services::hegel as hegel_service;
use crate::settings::HegelSettings;
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct HegelRequest {
    pub host: String,
    pub port: Option<u16>,
}

#[derive(Deserialize)]
pub(super) struct HegelPowerRequest {
    pub host: String,
    pub port: Option<u16>,
    pub on: bool,
}

#[derive(Deserialize)]
pub(super) struct HegelInputRequest {
    pub host: String,
    pub port: Option<u16>,
    pub input: u8,
}

#[derive(Deserialize)]
pub(super) struct HegelVolumeRequest {
    pub host: String,
    pub port: Option<u16>,
    pub volume: Option<u8>,
    pub direction: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct HegelMuteRequest {
    pub host: String,
    pub port: Option<u16>,
    pub muted: bool,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/hegel/settings",
            get(get_hegel_settings).post(set_hegel_settings),
        )
        .route("/api/hegel/status", post(hegel_status))
        .route("/api/hegel/power", post(hegel_power))
        .route("/api/hegel/input", post(hegel_input))
        .route("/api/hegel/volume", post(hegel_volume))
        .route("/api/hegel/mute", post(hegel_mute))
}

async fn get_hegel_settings(State(state): State<AppState>) -> Json<HegelSettings> {
    Json(normalize_hegel_settings(state.settings().hegel_settings()))
}

async fn set_hegel_settings(
    State(state): State<AppState>,
    Json(req): Json<HegelSettings>,
) -> Result<Json<HegelSettings>, ApiError> {
    Ok(Json(update_hegel_settings(&state, req)?))
}

async fn hegel_status(
    State(state): State<AppState>,
    Json(req): Json<HegelRequest>,
) -> Result<Json<hegel_service::HegelStatus>, ApiError> {
    Ok(Json(
        query_hegel_status_for_target(&state, &req.host, req.port).await?,
    ))
}

async fn hegel_power(
    State(state): State<AppState>,
    Json(req): Json<HegelPowerRequest>,
) -> Result<Json<hegel_service::HegelStatus>, ApiError> {
    Ok(Json(
        set_hegel_power_for_target(&state, &req.host, req.port, req.on).await?,
    ))
}

async fn hegel_input(
    State(state): State<AppState>,
    Json(req): Json<HegelInputRequest>,
) -> Result<Json<hegel_service::HegelStatus>, ApiError> {
    Ok(Json(
        set_hegel_input_for_target(&state, &req.host, req.port, req.input).await?,
    ))
}

pub(super) async fn hegel_volume(
    State(state): State<AppState>,
    Json(req): Json<HegelVolumeRequest>,
) -> Result<Json<hegel_service::HegelStatus>, ApiError> {
    Ok(Json(
        set_hegel_volume_for_target(
            &state,
            &req.host,
            req.port,
            req.volume,
            req.direction.as_deref(),
        )
        .await?,
    ))
}

async fn hegel_mute(
    State(state): State<AppState>,
    Json(req): Json<HegelMuteRequest>,
) -> Result<Json<hegel_service::HegelStatus>, ApiError> {
    Ok(Json(
        set_hegel_mute_for_target(&state, &req.host, req.port, req.muted).await?,
    ))
}
