use crate::api::error::{ApiError, ApiResult};
use crate::app::state::AppState;
#[cfg(all(target_os = "windows", feature = "asio"))]
use crate::audio::asio_output;
use crate::audio::device_caps;
use crate::audio::player::read_track_metadata;
use crate::diagnostics::status::DiagnosticActivity;
use crate::playback::service::select_active_output_device;
use crate::protocol::system_audio_backend;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use cpal::traits::{DeviceTrait, HostTrait};
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Deserialize)]
pub struct SelectDeviceRequest {
    pub name: Option<String>,
}

#[derive(Serialize)]
pub struct DeviceInfo {
    pub name: String,
    pub is_default: bool,
    pub backend: String,
    pub max_sample_rate: u32,
    pub max_bit_depth: u8,
}

#[derive(Serialize)]
pub struct FileInfo {
    pub name: String,
    pub size_bytes: u64,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub has_cover: bool,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/devices", get(list_devices))
        .route("/api/select-device", post(select_device))
        .route("/api/files", get(list_files))
}

async fn list_devices(State(state): State<AppState>) -> ApiResult<Json<Vec<DeviceInfo>>> {
    let _activity = state
        .diagnostics()
        .begin_activity(DiagnosticActivity::ApiDevicesList);
    let host = cpal::default_host();
    let mut devices_info = Vec::new();

    let default_device_name = host.default_output_device().and_then(|d| d.name().ok());

    let devices = host
        .output_devices()
        .map_err(|e| ApiError::internal(format!("Failed to list output devices: {:?}", e)))?;

    for dev in devices {
        if let Ok(name) = dev.name() {
            let is_default = Some(name.clone()) == default_device_name;
            let caps = {
                let _probe = state
                    .diagnostics()
                    .begin_activity(DiagnosticActivity::LocalAudioDeviceCapabilityProbe);
                device_caps::apply_known_device_capability(
                    Some(&name),
                    device_caps::capabilities_for_cpal_device(&dev).unwrap_or_default(),
                )
            };
            state.zones().cache_local_device_capabilities(&name, caps);
            devices_info.push(DeviceInfo {
                name,
                is_default,
                backend: system_audio_backend().to_string(),
                max_sample_rate: caps.max_sample_rate,
                max_bit_depth: caps.max_bit_depth,
            });
        }
    }

    #[cfg(all(target_os = "windows", feature = "asio"))]
    for name in asio_output::list_devices() {
        let name = format!("ASIO: {name}");
        let caps = {
            let _probe = state
                .diagnostics()
                .begin_activity(DiagnosticActivity::LocalAudioDeviceCapabilityProbe);
            device_caps::output_device_capabilities(Some(&name))
        };
        state.zones().cache_local_device_capabilities(&name, caps);
        devices_info.push(DeviceInfo {
            name,
            is_default: false,
            backend: "asio".to_string(),
            max_sample_rate: caps.max_sample_rate,
            max_bit_depth: caps.max_bit_depth,
        });
    }

    Ok(Json(devices_info))
}

async fn select_device(
    State(state): State<AppState>,
    Json(req): Json<SelectDeviceRequest>,
) -> Result<StatusCode, ApiError> {
    select_active_output_device(&state, req.name)?;
    Ok(StatusCode::OK)
}

async fn list_files(State(state): State<AppState>) -> ApiResult<Json<Vec<FileInfo>>> {
    let mut files = Vec::new();
    let music_dir = state.music_dir();
    if !music_dir.exists() {
        fs::create_dir_all(music_dir).map_err(|e| {
            ApiError::internal(format!("Failed to create music directory: {:?}", e))
        })?;
    }

    let entries = fs::read_dir(music_dir)
        .map_err(|e| ApiError::internal(format!("Failed to read music directory: {:?}", e)))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let extension = path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("")
                .to_lowercase();
            if ["wav", "flac", "mp3", "m4a", "ogg", "caf"].contains(&extension.as_str())
                && let (Some(name), Ok(metadata)) =
                    (path.file_name().and_then(|n| n.to_str()), entry.metadata())
            {
                let (tags, cover) = read_track_metadata(&path);
                files.push(FileInfo {
                    name: name.to_string(),
                    size_bytes: metadata.len(),
                    title: tags.title,
                    artist: tags.artist,
                    album: tags.album,
                    has_cover: cover.is_some(),
                });
            }
        }
    }

    files.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Json(files))
}
