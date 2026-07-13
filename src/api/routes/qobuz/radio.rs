use super::{REMOTE_GRID_ARTWORK_SIZE, artwork_json, internal_error};
use crate::app::auth::RequestSurface;
use crate::app::state::AppState;
use axum::{
    Json,
    extract::{Extension, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;

#[derive(Deserialize)]
pub(in crate::api::routes) struct QobuzRadioNextRequest {
    seed_track_id: Option<u64>,
    seed_artist_name: Option<String>,
    #[serde(default)]
    exclude_track_ids: Vec<u64>,
    limit: Option<u32>,
}

#[derive(Debug)]
pub(in crate::api::routes) enum QobuzRadioSeed {
    Track(u64),
    ArtistName(String),
}

impl QobuzRadioNextRequest {
    pub(in crate::api::routes) fn seed(&self) -> Result<QobuzRadioSeed, (StatusCode, String)> {
        if let Some(seed_track_id) = self.seed_track_id
            && seed_track_id > 0
        {
            return Ok(QobuzRadioSeed::Track(seed_track_id));
        }
        if let Some(seed_artist_name) = self
            .seed_artist_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            return Ok(QobuzRadioSeed::ArtistName(seed_artist_name.to_string()));
        }
        Err((
            StatusCode::BAD_REQUEST,
            "seed_track_id or seed_artist_name is required".to_string(),
        ))
    }
}

pub(super) async fn qobuz_radio_next(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
    Json(req): Json<QobuzRadioNextRequest>,
) -> Result<Response, (StatusCode, String)> {
    let limit = req.limit.unwrap_or(10);
    let recommendation = match req.seed()? {
        QobuzRadioSeed::Track(seed_track_id) => {
            state
                .qobuz()
                .radio_next(seed_track_id, &req.exclude_track_ids, limit)
                .await
        }
        QobuzRadioSeed::ArtistName(seed_artist_name) => {
            state
                .qobuz()
                .radio_next_for_artist_name(&seed_artist_name, &req.exclude_track_ids, limit)
                .await
        }
    }
    .map_err(internal_error)?;

    match recommendation {
        Some(recommendation) => Ok(
            artwork_json(recommendation, surface, REMOTE_GRID_ARTWORK_SIZE)
                .map_err(internal_error)?
                .into_response(),
        ),
        None => Ok(StatusCode::NO_CONTENT.into_response()),
    }
}
