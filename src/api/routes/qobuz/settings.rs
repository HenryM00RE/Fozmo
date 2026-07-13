use crate::app::state::AppState;
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
pub(super) struct QobuzSettingsRequest {
    pub radio_enabled: bool,
}

pub(super) async fn qobuz_settings(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    Ok(Json(json!({
        "radio_enabled": state.settings().qobuz_radio_enabled(),
    })))
}

pub(super) async fn update_qobuz_settings(
    State(state): State<AppState>,
    Json(req): Json<QobuzSettingsRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .settings()
        .try_update(|settings| {
            settings.qobuz_radio_enabled = Some(req.radio_enabled);
            if req.radio_enabled {
                settings.lastfm_radio_enabled = Some(false);
            }
        })
        .map_err(super::super::internal_error)?;
    Ok(Json(json!({
        "radio_enabled": state.settings().qobuz_radio_enabled(),
    })))
}
