use super::internal_error;
use crate::app::state::AppState;
use crate::settings::AppearanceSettings;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path as StdPath;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CUSTOM_DISPLAY_FONT_FILENAME: &str = "custom-display.ttf";
const MAX_FONT_UPLOAD_BYTES: usize = 8 * 1024 * 1024;
const FONT_VALIDATION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Serialize)]
struct AppearanceResponse {
    custom_display_font_enabled: bool,
    custom_display_font_scale_percent: u16,
    custom_display_font_name: Option<String>,
    custom_display_font_url: Option<String>,
    custom_display_font_version: u64,
    custom_display_font_supported_ranges: Vec<[u32; 2]>,
}

#[derive(Debug, Deserialize)]
struct AppearanceUpdateRequest {
    #[serde(default)]
    custom_display_font_enabled: bool,
    #[serde(default = "default_custom_display_font_scale_percent")]
    custom_display_font_scale_percent: u16,
}

fn default_custom_display_font_scale_percent() -> u16 {
    100
}

/// Local-only routes composed with the remote-safe set. Appearance updates
/// and font uploads write to the workspace and stay local.
pub fn routes() -> Router<AppState> {
    remote_routes()
        .route("/api/appearance", post(update_appearance))
        .route(
            "/api/appearance/display-font",
            post(upload_display_font).layer(DefaultBodyLimit::max(MAX_FONT_UPLOAD_BYTES + 1024)),
        )
}

/// Read-only appearance state needed by the remote app shell for theming.
pub fn remote_routes() -> Router<AppState> {
    Router::new().route("/api/appearance", get(get_appearance))
}

async fn get_appearance(State(state): State<AppState>) -> Json<AppearanceResponse> {
    Json(appearance_response(&state.settings().appearance_settings()))
}

async fn update_appearance(
    State(state): State<AppState>,
    Json(req): Json<AppearanceUpdateRequest>,
) -> Result<Json<AppearanceResponse>, (StatusCode, String)> {
    state
        .settings()
        .try_update(|settings| {
            let has_font = settings
                .appearance
                .custom_display_font_filename
                .as_deref()
                .is_some_and(|name| !name.trim().is_empty());
            settings.appearance.custom_display_font_user_configured = true;
            settings.appearance.custom_display_font_enabled =
                req.custom_display_font_enabled && has_font;
            settings.appearance.custom_display_font_scale_percent =
                clamp_scale_percent(req.custom_display_font_scale_percent);
        })
        .map_err(internal_error)?;
    Ok(Json(appearance_response(
        &state.settings().appearance_settings(),
    )))
}

async fn upload_display_font(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<AppearanceResponse>, (StatusCode, String)> {
    let mut original_filename = None;
    let mut data = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (e.status(), format!("Multipart error: {e:?}")))?
    {
        if field.name() == Some("font") || field.file_name().is_some() {
            let filename = field
                .file_name()
                .and_then(|name| StdPath::new(name).file_name())
                .and_then(|name| name.to_str())
                .map(str::to_string);
            original_filename = filename;
            let bytes = field
                .bytes()
                .await
                .map_err(|e| (e.status(), format!("Read font bytes: {e:?}")))?;
            if bytes.len() > MAX_FONT_UPLOAD_BYTES {
                return Err((
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Display font must be 8 MB or smaller".to_string(),
                ));
            }
            data = Some(bytes.to_vec());
            break;
        }
    }

    let Some(data) = data else {
        return Err((StatusCode::BAD_REQUEST, "Missing font file".to_string()));
    };
    let original_filename =
        original_filename.unwrap_or_else(|| CUSTOM_DISPLAY_FONT_FILENAME.into());
    if !original_filename.to_ascii_lowercase().ends_with(".ttf") {
        return Err((
            StatusCode::BAD_REQUEST,
            "Display font upload must be a .ttf file".to_string(),
        ));
    }

    let validation_data = data.clone();
    let validation = tokio::task::spawn_blocking(move || validate_font(&validation_data));
    let supported_ranges = tokio::time::timeout(FONT_VALIDATION_TIMEOUT, validation)
        .await
        .map_err(|_| {
            (
                StatusCode::REQUEST_TIMEOUT,
                "Display font validation timed out".to_string(),
            )
        })?
        .map_err(|error| internal_error(format!("Display font validator stopped: {error}")))??;
    if supported_ranges.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Font file does not expose any supported characters".to_string(),
        ));
    }

    let assets_dir = state.appearance_assets_dir();
    tokio::fs::create_dir_all(assets_dir)
        .await
        .map_err(|e| internal_error(format!("Create display font directory: {e:?}")))?;
    tokio::fs::write(assets_dir.join(CUSTOM_DISPLAY_FONT_FILENAME), &data)
        .await
        .map_err(|e| internal_error(format!("Save display font: {e:?}")))?;

    let version = font_version();
    state
        .settings()
        .try_update(|settings| {
            settings.appearance.custom_display_font_filename =
                Some(CUSTOM_DISPLAY_FONT_FILENAME.to_string());
            settings.appearance.custom_display_font_original_filename = Some(original_filename);
            settings.appearance.custom_display_font_version = version;
            settings.appearance.custom_display_font_supported_ranges = supported_ranges;
            settings.appearance.custom_display_font_scale_percent =
                clamp_scale_percent(settings.appearance.custom_display_font_scale_percent);
        })
        .map_err(internal_error)?;

    Ok(Json(appearance_response(
        &state.settings().appearance_settings(),
    )))
}

fn appearance_response(settings: &AppearanceSettings) -> AppearanceResponse {
    let font_url = settings
        .custom_display_font_filename
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .map(|name| {
            format!(
                "/user-fonts/{name}?v={}",
                settings.custom_display_font_version
            )
        });

    AppearanceResponse {
        custom_display_font_enabled: settings.custom_display_font_user_configured
            && settings.custom_display_font_enabled
            && font_url.is_some(),
        custom_display_font_scale_percent: clamp_scale_percent(
            settings.custom_display_font_scale_percent,
        ),
        custom_display_font_name: settings.custom_display_font_original_filename.clone(),
        custom_display_font_url: font_url,
        custom_display_font_version: settings.custom_display_font_version,
        custom_display_font_supported_ranges: settings.custom_display_font_supported_ranges.clone(),
    }
}

fn clamp_scale_percent(value: u16) -> u16 {
    value.clamp(70, 140)
}

fn font_version() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn validate_font(data: &[u8]) -> Result<Vec<[u32; 2]>, (StatusCode, String)> {
    let face = ttf_parser::Face::parse(data, 0).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "Font file is not a valid TTF".to_string(),
        )
    })?;
    Ok(supported_codepoint_ranges(&face))
}

fn supported_codepoint_ranges(face: &ttf_parser::Face<'_>) -> Vec<[u32; 2]> {
    let mut codepoints = BTreeSet::new();
    if let Some(cmap) = face.tables().cmap {
        for subtable in cmap
            .subtables
            .into_iter()
            .filter(|table| table.is_unicode())
        {
            subtable.codepoints(|codepoint| {
                if codepoint <= 0x10ffff
                    && !(0xd800..=0xdfff).contains(&codepoint)
                    && subtable.glyph_index(codepoint).is_some()
                {
                    codepoints.insert(codepoint);
                }
            });
        }
    }
    let mut ranges = Vec::new();
    let mut start = None;
    let mut previous = 0_u32;

    for codepoint in codepoints {
        if start.is_none() {
            start = Some(codepoint);
        } else if codepoint != previous.saturating_add(1) {
            let range_start = start.replace(codepoint).expect("range start");
            ranges.push([range_start, previous]);
        }
        previous = codepoint;
    }

    if let Some(range_start) = start {
        ranges.push([range_start, previous]);
    }

    ranges
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn property_arbitrary_font_input_is_rejected_without_panicking(
            bytes in proptest::collection::vec(any::<u8>(), 0..16_384)
        ) {
            if let Ok(ranges) = validate_font(&bytes) {
                prop_assert!(ranges.iter().all(|[start, end]| start <= end && *end <= 0x10ffff));
            }
        }
    }
}
