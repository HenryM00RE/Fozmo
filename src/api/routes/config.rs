use crate::api::error::ApiError;
use crate::app::state::AppState;
use crate::playback::service::{
    PlaybackConfigUpdate, update_active_playback_config, update_playback_config_for_zone,
};
use crate::settings::DsdSourceRule;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::post,
};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct ConfigRequest {
    pub filter_type: String,
    pub target_rate: u32,
    #[serde(default = "default_target_bit_depth")]
    pub target_bit_depth: u32,
    #[serde(default = "default_true")]
    pub upsampling_enabled: bool,
    pub exclusive: bool,
    pub output_mode: Option<String>,
    pub dsd_modulator: Option<String>,
    #[serde(default)]
    pub dsd_isi_penalty: f32,
    #[serde(default)]
    pub dsd_rules_enabled: bool,
    #[serde(default)]
    pub dsd_rules: Vec<DsdSourceRule>,
    #[serde(default)]
    pub headroom_db: f32,
    #[serde(default)]
    pub dsp_buffer_ms: u32,
}

fn default_true() -> bool {
    true
}

fn default_target_bit_depth() -> u32 {
    24
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/config", post(update_config))
        .route("/api/zones/:zone_id/config", post(update_zone_config))
}

async fn update_config(
    State(state): State<AppState>,
    Json(req): Json<ConfigRequest>,
) -> Result<StatusCode, ApiError> {
    update_active_playback_config(&state, req.into())?;
    Ok(StatusCode::OK)
}

async fn update_zone_config(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    Json(req): Json<ConfigRequest>,
) -> Result<StatusCode, ApiError> {
    update_playback_config_for_zone(&state, &zone_id, req.into())?;
    Ok(StatusCode::OK)
}

impl From<ConfigRequest> for PlaybackConfigUpdate {
    fn from(req: ConfigRequest) -> Self {
        Self {
            filter_type: req.filter_type,
            target_rate: req.target_rate,
            target_bit_depth: req.target_bit_depth,
            upsampling_enabled: req.upsampling_enabled,
            exclusive: req.exclusive,
            output_mode: req.output_mode,
            dsd_modulator: req.dsd_modulator,
            dsd_isi_penalty: req.dsd_isi_penalty,
            dsd_rules_enabled: req.dsd_rules_enabled,
            dsd_rules: req.dsd_rules,
            headroom_db: req.headroom_db,
            dsp_buffer_ms: req.dsp_buffer_ms,
        }
    }
}
