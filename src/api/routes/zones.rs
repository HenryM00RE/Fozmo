use crate::api::error::ApiError;
use crate::app::auth::RequestSurface;
use crate::app::state::AppState;
use crate::diagnostics::status::DiagnosticActivity;
use crate::library::{
    BrowserStreamSettings, ZoneHegelSettings, ZoneSettings, ZoneUpnpCapabilities,
};
use crate::playback::service::{
    ZoneSettingsUpdate, disable_playback_zone, enable_playback_zone,
    persist_calibrated_upnp_capabilities, query_hegel_status_for_zone_target,
    refresh_playback_zones, rename_playback_zone, select_playback_zone,
    update_playback_zone_settings,
};
use crate::playback::status::{StatusResponse, build_status_response_for_zone};
use crate::protocol::ZoneProfile;
use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

#[derive(Deserialize)]
pub struct SelectZoneRequest {
    pub zone_id: String,
}

#[derive(Deserialize)]
pub struct RenameZoneRequest {
    pub name: String,
}

#[derive(Deserialize)]
pub struct ZoneSettingsRequest {
    #[serde(default)]
    pub airplay_default_volume_enabled: Option<bool>,
    #[serde(default)]
    pub airplay_default_volume: Option<f32>,
    #[serde(default)]
    pub qobuz_hires_enabled: Option<bool>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub device_type: Option<String>,
    #[serde(default)]
    pub hegel: Option<ZoneHegelSettings>,
    #[serde(default)]
    pub upnp_capabilities: Option<ZoneUpnpCapabilities>,
    #[serde(default)]
    pub browser_stream: Option<BrowserStreamSettings>,
}

#[derive(Deserialize)]
pub struct ZoneHegelRequest {
    pub host: String,
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ZoneCalibrationResponse {
    pub zone: Option<ZoneProfile>,
    pub message: String,
}

pub fn routes() -> Router<AppState> {
    let router = common_routes()
        .route("/api/zones/:zone_id/settings", post(update_zone_settings))
        .route("/api/zones/:zone_id/status", get(get_zone_status));
    #[cfg(feature = "hegel")]
    let router = router.route("/api/zones/:zone_id/hegel/status", post(zone_hegel_status));
    router
}

/// Zone routes safe for the remote listener. Device-target settings and Hegel
/// status are LAN-only because they can select and contact arbitrary TCP
/// targets from the server's network.
pub fn remote_routes() -> Router<AppState> {
    common_routes()
        .route(
            "/api/zones/:zone_id/settings",
            post(update_remote_browser_zone_settings),
        )
        .route("/api/zones/:zone_id/status", get(get_zone_status))
}

fn common_routes() -> Router<AppState> {
    Router::new()
        .route("/api/zones", get(list_zones))
        .route("/api/zones/select", post(select_zone))
        .route("/api/zones/:zone_id/enable", post(enable_zone))
        .route("/api/zones/:zone_id/disable", post(disable_zone))
        .route("/api/zones/:zone_id/rename", post(rename_zone))
        .route("/api/zones/:zone_id/calibrate", post(calibrate_zone))
}

async fn list_zones(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let _activity = state
        .diagnostics()
        .begin_activity(DiagnosticActivity::ApiZonesRefresh);
    let owner = crate::app::auth::browser_zone_header(&headers);
    let mut zones = refresh_playback_zones(&state);
    // Browser zones are visible only to the browser session that owns them
    // (browser zone ids double as the owning agent id).
    zones.retain(|zone| !zone.browser || owner.as_deref() == Some(zone.id.as_str()));
    Json(zones)
}

async fn select_zone(
    State(state): State<AppState>,
    Json(req): Json<SelectZoneRequest>,
) -> Result<StatusCode, ApiError> {
    select_playback_zone(&state, &req.zone_id)?;
    Ok(StatusCode::OK)
}

async fn enable_zone(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    enable_playback_zone(&state, &zone_id)?;
    Ok(StatusCode::OK)
}

async fn disable_zone(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    disable_playback_zone(&state, &zone_id)?;
    Ok(StatusCode::OK)
}

async fn rename_zone(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    Json(req): Json<RenameZoneRequest>,
) -> Result<StatusCode, ApiError> {
    rename_playback_zone(&state, &zone_id, &req.name)?;
    Ok(StatusCode::OK)
}

async fn update_zone_settings(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    Json(req): Json<ZoneSettingsRequest>,
) -> Result<Json<ZoneSettings>, ApiError> {
    Ok(Json(update_playback_zone_settings(
        &state,
        &zone_id,
        req.into(),
    )?))
}

/// Remote Access may change delivery preferences for the authenticated
/// browser's own private output. Server-device settings remain LAN-only: in
/// particular this route cannot configure Hegel targets, UPnP capabilities,
/// default volume, device type, or Qobuz behavior.
async fn update_remote_browser_zone_settings(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    Json(req): Json<ZoneSettingsRequest>,
) -> Result<Json<ZoneSettings>, ApiError> {
    if state.zones().browser_zone_agent_id(&zone_id).is_none() {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "Remote settings can only change this browser's output",
        ));
    }
    if req.airplay_default_volume_enabled.is_some()
        || req.airplay_default_volume.is_some()
        || req.qobuz_hires_enabled.is_some()
        || req.device_type.is_some()
        || req.hegel.is_some()
        || req.upnp_capabilities.is_some()
    {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "Remote browser settings may only change the icon and stream format",
        ));
    }
    Ok(Json(update_playback_zone_settings(
        &state,
        &zone_id,
        ZoneSettingsUpdate {
            icon: req.icon,
            browser_stream: req.browser_stream,
            ..ZoneSettingsUpdate::default()
        },
    )?))
}

async fn calibrate_zone(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
) -> Result<Json<ZoneCalibrationResponse>, ApiError> {
    if !zone_id.starts_with("upnp-") {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "Capability calibration is only available for UPnP zones",
        ));
    }
    let calibration = state
        .upnp()
        .calibrate_renderer_capabilities(&zone_id)
        .await
        .map_err(|message| ApiError::new(StatusCode::NOT_FOUND, message))?;
    persist_calibrated_upnp_capabilities(&state, &zone_id, &calibration.target)
        .map_err(ApiError::from)?;
    state.zones().sync_upnp_renderers(state.upnp().renderers());
    let zones = refresh_playback_zones(&state);
    let zone = zones.into_iter().find(|zone| zone.id == zone_id);
    Ok(Json(ZoneCalibrationResponse {
        zone,
        message: calibration.message,
    }))
}

async fn zone_hegel_status(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    Json(req): Json<ZoneHegelRequest>,
) -> Result<Json<crate::services::hegel::HegelStatus>, ApiError> {
    Ok(Json(
        query_hegel_status_for_zone_target(&state, &zone_id, &req.host, req.port).await?,
    ))
}

async fn get_zone_status(
    State(state): State<AppState>,
    Path(zone_id): Path<String>,
    surface: Option<Extension<RequestSurface>>,
) -> Result<Json<StatusResponse>, (StatusCode, String)> {
    build_status_response_for_zone(&state, &zone_id)
        .map(|mut response| {
            response.surface = match surface.map(|Extension(surface)| surface) {
                Some(RequestSurface::Remote) => "remote".to_string(),
                _ => "local".to_string(),
            };
            Json(response)
        })
        .map_err(|e| (StatusCode::NOT_FOUND, e))
}

impl From<ZoneSettingsRequest> for ZoneSettingsUpdate {
    fn from(req: ZoneSettingsRequest) -> Self {
        Self {
            airplay_default_volume_enabled: req.airplay_default_volume_enabled,
            airplay_default_volume: req.airplay_default_volume,
            qobuz_hires_enabled: req.qobuz_hires_enabled,
            icon: req.icon,
            device_type: req.device_type,
            hegel: req.hegel,
            upnp_capabilities: req.upnp_capabilities,
            browser_stream: req.browser_stream,
        }
    }
}
