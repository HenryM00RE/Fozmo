use super::internal_error;
use crate::api::error::ApiError;
use crate::app::state::AppState;
use crate::playback::lastfm::{
    LastFmResolveOptions, LastFmSeedContext, RadioExclusions, merge_seed_context,
    resolve_lastfm_radio, resolve_lastfm_radio_with_context, seed_context_from_current_status,
    seed_from_current_status,
};
use crate::protocol::RadioContext;
use crate::secrets::{SecretKey, SecretValue};
use crate::services::lastfm::LastFmSeed;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

/// Local-only routes composed with the remote-safe set. Settings accept an
/// API key and the radio test drives outbound requests; both stay local.
pub fn routes() -> Router<AppState> {
    remote_routes()
        .route("/api/lastfm/settings", post(update_lastfm_settings))
        .route("/api/lastfm/radio/test", post(lastfm_radio_test))
}

/// Read-only status is the only Last.fm surface exposed remotely.
pub fn remote_routes() -> Router<AppState> {
    Router::new().route("/api/lastfm/status", get(lastfm_status))
}

#[derive(Deserialize)]
struct LastFmSettingsRequest {
    #[serde(default)]
    api_key: Option<Value>,
    #[serde(default)]
    radio_enabled: Option<bool>,
}

#[derive(Deserialize)]
struct LastFmRadioTestRequest {
    seed: Option<LastFmSeed>,
    #[serde(default)]
    seed_context: Option<LastFmSeedContext>,
    limit: Option<u32>,
    resolve_limit: Option<u32>,
    qobuz_resolve_limit: Option<u32>,
    #[serde(default)]
    radio_context: Option<RadioContext>,
}

async fn lastfm_status(State(state): State<AppState>) -> impl IntoResponse {
    Json(lastfm_status_payload(&state))
}

async fn update_lastfm_settings(
    State(state): State<AppState>,
    Json(req): Json<LastFmSettingsRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let mut saved_api_key_configured = None;
    if let Some(api_key) = req.api_key.as_ref() {
        if let Some(value) = normalize_secret_value(api_key) {
            state
                .secrets()
                .put(SecretKey::LastFmApiKey, SecretValue::new(value))
                .map_err(|e| internal_error(e.to_string()))?;
            saved_api_key_configured = Some(true);
        } else {
            state
                .secrets()
                .delete(SecretKey::LastFmApiKey)
                .map_err(|e| internal_error(e.to_string()))?;
            saved_api_key_configured = Some(false);
        }
    }
    state
        .settings()
        .try_update(move |settings| {
            if let Some(enabled) = req.radio_enabled {
                settings.lastfm_radio_enabled = Some(enabled);
                if enabled {
                    settings.qobuz_radio_enabled = Some(false);
                }
            } else if let Some(configured) = saved_api_key_configured {
                settings.lastfm_radio_enabled = Some(configured);
                if configured {
                    settings.qobuz_radio_enabled = Some(false);
                }
            }
        })
        .map_err(internal_error)?;
    Ok(Json(lastfm_status_payload(&state)))
}

async fn lastfm_radio_test(
    State(state): State<AppState>,
    Json(req): Json<LastFmRadioTestRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let seed = radio_seed_for_request(&state, req.seed)?;
    if state.lastfm_api_key().is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Last.fm API key is not configured".to_string(),
        ));
    }
    let context = radio_seed_context_for_request(&state, req.seed_context);
    let limit = req.limit.unwrap_or(20).clamp(1, 50);
    let options = LastFmResolveOptions::new(
        limit,
        req.resolve_limit.unwrap_or(limit),
        req.qobuz_resolve_limit.unwrap_or(3),
    );
    let resolution = if req.radio_context.is_some() {
        resolve_lastfm_radio_with_context(
            &state,
            seed,
            context,
            req.radio_context,
            options,
            RadioExclusions::default(),
        )
        .await
        .map_err(lastfm_radio_error)?
    } else {
        resolve_lastfm_radio(&state, seed, context, options, RadioExclusions::default())
            .await
            .map_err(lastfm_radio_error)?
    };

    Ok(Json(resolution))
}

fn lastfm_radio_error(error: String) -> (StatusCode, String) {
    let error = ApiError::upstream(error);
    (error.status(), error.message().to_string())
}

fn lastfm_status_payload(state: &AppState) -> serde_json::Value {
    let source = state.lastfm_api_key_source();
    json!({
        "configured": source.is_some(),
        "source": source,
        "radio_enabled": state.settings().lastfm_radio_enabled(),
        "radio_active": state.lastfm_radio_active(),
    })
}

fn radio_seed_for_request(
    state: &AppState,
    seed: Option<LastFmSeed>,
) -> Result<LastFmSeed, (StatusCode, String)> {
    let zone_id = state.zones().active_zone_id();
    let seed = seed
        .or_else(|| seed_from_current_status(state, &zone_id))
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "Last.fm seed requires title and artist, or a track MBID".to_string(),
            )
        })?;
    seed.normalized()
        .map_err(|err| (StatusCode::BAD_REQUEST, err))
}

fn radio_seed_context_for_request(
    state: &AppState,
    context: Option<LastFmSeedContext>,
) -> LastFmSeedContext {
    let zone_id = state.zones().active_zone_id();
    merge_seed_context(seed_context_from_current_status(state, &zone_id), context)
}

fn normalize_secret(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn normalize_secret_value(value: &Value) -> Option<String> {
    value.as_str().and_then(|text| normalize_secret(Some(text)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::test_support::app_state;

    #[test]
    fn oversized_lastfm_seed_is_a_controlled_bad_request() {
        let state = app_state("lastfm-oversized-seed");
        let error = radio_seed_for_request(
            &state,
            Some(LastFmSeed {
                title: Some("t".repeat(513)),
                artist: Some("Artist".to_string()),
                mbid: None,
            }),
        )
        .unwrap_err();

        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert_eq!(error.1, "Last.fm seed title exceeds the 512 byte limit");
    }

    #[test]
    fn lastfm_upstream_errors_use_bad_gateway_status() {
        assert_eq!(
            lastfm_radio_error("Last.fm similar tracks request failed".to_string()),
            (
                StatusCode::BAD_GATEWAY,
                "Upstream service error".to_string()
            )
        );
    }
}
