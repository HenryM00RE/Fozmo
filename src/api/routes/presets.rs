use super::internal_error;
use crate::app::state::AppState;
use crate::audio::eq::EqConfig;
use crate::playback::service::apply_active_eq_config;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path as FsPath, PathBuf};

#[derive(Serialize)]
struct PresetSummary {
    name: String,
}

#[derive(Deserialize, Serialize)]
struct PresetFile {
    name: String,
    #[serde(flatten)]
    config: EqConfig,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/eq/presets", get(list_presets).post(save_preset))
        .route(
            "/api/eq/presets/:name",
            get(load_preset).delete(delete_preset),
        )
}

async fn list_presets(
    State(state): State<AppState>,
) -> Result<Json<Vec<PresetSummary>>, (StatusCode, String)> {
    let mut names = BTreeSet::new();
    for directory in [state.built_in_presets_dir(), state.presets_dir()] {
        collect_preset_names(directory, &mut names)?;
    }
    let presets = names
        .into_iter()
        .map(|name| PresetSummary { name })
        .collect();
    Ok(Json(presets))
}

async fn save_preset(
    State(state): State<AppState>,
    Json(req): Json<PresetFile>,
) -> Result<StatusCode, (StatusCode, String)> {
    let name = sanitize_preset_name(&req.name)
        .ok_or((StatusCode::BAD_REQUEST, "Invalid preset name".to_string()))?;

    let presets_dir = state.presets_dir();
    if !presets_dir.exists() {
        fs::create_dir_all(presets_dir)
            .map_err(|e| internal_error(format!("Failed to create presets directory: {e:?}")))?;
    }

    let path = presets_dir.join(format!("{name}.json"));
    let body = PresetFile {
        name: name.clone(),
        config: req.config,
    };
    let json = serde_json::to_string_pretty(&body)
        .map_err(|e| internal_error(format!("Serialize preset: {e:?}")))?;
    fs::write(&path, json)
        .map_err(|e| internal_error(format!("Failed to write preset file: {e:?}")))?;
    Ok(StatusCode::CREATED)
}

async fn load_preset(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<EqConfig>, (StatusCode, String)> {
    let name = sanitize_preset_name(&name)
        .ok_or((StatusCode::BAD_REQUEST, "Invalid preset name".to_string()))?;
    let path = resolved_preset_path(&state, &name)
        .ok_or((StatusCode::NOT_FOUND, format!("Preset '{name}' not found")))?;
    let data = fs::read_to_string(&path)
        .map_err(|_| (StatusCode::NOT_FOUND, format!("Preset '{name}' not found")))?;
    let file: PresetFile = serde_json::from_str(&data)
        .map_err(|e| internal_error(format!("Failed to parse preset: {e:?}")))?;

    apply_active_eq_config(&state, file.config.clone())
        .map_err(|error| internal_error(error.message().to_string()))?;
    Ok(Json(file.config))
}

async fn delete_preset(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let name = sanitize_preset_name(&name)
        .ok_or((StatusCode::BAD_REQUEST, "Invalid preset name".to_string()))?;
    let path = state.presets_dir().join(format!("{name}.json"));
    if !path.exists() {
        if state.built_in_presets_dir() != state.presets_dir()
            && state
                .built_in_presets_dir()
                .join(format!("{name}.json"))
                .exists()
        {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Preset '{name}' is built in and cannot be deleted"),
            ));
        }
        return Err((StatusCode::NOT_FOUND, format!("Preset '{name}' not found")));
    }
    fs::remove_file(&path)
        .map_err(|e| internal_error(format!("Failed to delete preset: {e:?}")))?;
    Ok(StatusCode::OK)
}

fn collect_preset_names(
    directory: &FsPath,
    names: &mut BTreeSet<String>,
) -> Result<(), (StatusCode, String)> {
    if !directory.exists() {
        return Ok(());
    }
    let entries = fs::read_dir(directory)
        .map_err(|error| internal_error(format!("Failed to read presets directory: {error:?}")))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("json")
            && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
        {
            names.insert(stem.to_string());
        }
    }
    Ok(())
}

fn resolved_preset_path(state: &AppState, name: &str) -> Option<PathBuf> {
    let user = state.presets_dir().join(format!("{name}.json"));
    if user.is_file() {
        return Some(user);
    }
    let built_in = state.built_in_presets_dir().join(format!("{name}.json"));
    built_in.is_file().then_some(built_in)
}

fn sanitize_preset_name(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return None;
    }
    let ok = trimmed
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '));
    if ok { Some(trimmed.to_string()) } else { None }
}
