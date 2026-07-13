use crate::api::error::ApiError;
use crate::app::auth::ProfileContext;
use crate::app::state::AppState;
use crate::library::{ListeningTopSongItem, PlaybackHistoryDataEntry, PlaybackHistoryInput};
use axum::{
    Json, Router,
    extract::{Extension, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, JsonSchema)]
pub struct RecentHistoryQuery {
    pub limit: Option<i64>,
    pub exclude_radio: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct HistoryStatsQuery {
    pub range: Option<String>,
}

#[derive(Deserialize)]
pub struct HistoryTopQuery {
    pub kind: Option<String>,
    pub range: Option<String>,
    pub limit: Option<i64>,
    pub profile_id: Option<String>,
    pub exclude_radio: Option<bool>,
}

#[derive(Deserialize)]
pub struct PlaybackHistoryImportRequest {
    pub mode: Option<String>,
    pub entries: Vec<PlaybackHistoryDataEntry>,
}

#[derive(Serialize)]
pub struct HistoryTopProfile {
    pub id: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct HistoryTopResponse {
    pub profile: HistoryTopProfile,
    pub range: String,
    pub kind: String,
    pub items: Vec<ListeningTopSongItem>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/history/recent", get(recent_history))
        .route("/api/history/stats", get(history_stats))
        .route("/api/history/top", get(history_top))
        .route("/api/history/export", get(export_history))
        .route(
            "/api/history/import",
            post(import_history).layer(axum::extract::DefaultBodyLimit::max(25 * 1024 * 1024)),
        )
        .route("/api/history/record", post(record_history))
}

async fn recent_history(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    Query(query): Query<RecentHistoryQuery>,
) -> impl IntoResponse {
    let profile_id = request_profile_id(&state, profile);
    let limit = query.limit.unwrap_or(50);
    let live = state.listening().active_history_inputs();
    let include_radio = !query.exclude_radio.unwrap_or(false);
    match state
        .library()
        .run_blocking(move |library| {
            library.recent_playback_history_with_live_for_profile(
                &profile_id,
                limit,
                &live,
                include_radio,
            )
        })
        .await
    {
        Ok(history) => Json(history).into_response(),
        Err(e) => ApiError::internal(e).into_response(),
    }
}

async fn history_stats(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    Query(query): Query<HistoryStatsQuery>,
) -> impl IntoResponse {
    let profile_id = request_profile_id(&state, profile);
    let range = query.range.unwrap_or_else(|| "4w".to_string());
    let live = state.listening().active_history_inputs();
    match state
        .library()
        .run_blocking(move |library| {
            library.listening_history_stats_with_live_for_profile(&profile_id, &range, &live)
        })
        .await
    {
        Ok(stats) => Json(stats).into_response(),
        Err(e) => ApiError::internal(e).into_response(),
    }
}

async fn history_top(
    State(state): State<AppState>,
    Query(query): Query<HistoryTopQuery>,
) -> impl IntoResponse {
    let kind = query.kind.as_deref().unwrap_or("songs");
    if kind != "songs" {
        return (StatusCode::BAD_REQUEST, "History kind must be songs").into_response();
    }
    let range = query.range.as_deref().unwrap_or("week");
    if !matches!(range, "week" | "month" | "year" | "all" | "4w") {
        return (
            StatusCode::BAD_REQUEST,
            "History range must be week, month, year, all, or 4w",
        )
            .into_response();
    }

    let profiles = state.settings().profiles();
    let profile_id = query
        .profile_id
        .clone()
        .unwrap_or_else(|| state.settings().active_profile_id());
    let Some(profile) = profiles.iter().find(|profile| profile.id == profile_id) else {
        return (StatusCode::NOT_FOUND, "Profile not found").into_response();
    };

    let profile_id = profile.id.clone();
    let range_owned = range.to_string();
    let limit = query.limit.unwrap_or(25);
    let exclude_radio = query.exclude_radio.unwrap_or(false);
    match state
        .library()
        .run_blocking(move |library| {
            library.top_history_songs_for_profile(&profile_id, &range_owned, limit, exclude_radio)
        })
        .await
    {
        Ok(top) => Json(HistoryTopResponse {
            profile: HistoryTopProfile {
                id: profile.id.clone(),
                name: profile.name.clone(),
            },
            range: top.range,
            kind: "songs".to_string(),
            items: top.items,
        })
        .into_response(),
        Err(e) => ApiError::internal(e).into_response(),
    }
}

async fn export_history(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
) -> impl IntoResponse {
    let profile_id = request_profile_id(&state, profile);
    match state
        .library()
        .run_blocking(move |library| library.export_playback_history_for_profile(&profile_id))
        .await
    {
        Ok(export) => Json(export).into_response(),
        Err(e) => ApiError::internal(e).into_response(),
    }
}

async fn import_history(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    Json(req): Json<PlaybackHistoryImportRequest>,
) -> impl IntoResponse {
    let profile_id = request_profile_id(&state, profile);
    let replace = match req.mode.as_deref().unwrap_or("merge") {
        "merge" => false,
        "replace" => true,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "Import mode must be merge or replace",
            )
                .into_response();
        }
    };
    match state
        .library()
        .run_blocking(move |library| {
            library.import_playback_history_for_profile(&profile_id, &req.entries, replace)
        })
        .await
    {
        Ok(result) => Json(result).into_response(),
        Err(e) => ApiError::internal(e).into_response(),
    }
}

async fn record_history(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    Json(mut req): Json<PlaybackHistoryInput>,
) -> impl IntoResponse {
    if let Some(Extension(profile)) = profile {
        req.profile_id = Some(profile.id);
    } else if req.profile_id.is_none() {
        // Compatibility for direct in-process callers that do not install the
        // production profile middleware. Network requests always have context.
        req.profile_id = Some(state.settings().active_profile_id());
    }
    match state
        .library()
        .run_blocking(move |library| library.record_playback_history(req))
        .await
    {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e) => ApiError::internal(e).into_response(),
    }
}

fn request_profile_id(state: &AppState, profile: Option<Extension<ProfileContext>>) -> String {
    profile
        .map(|Extension(profile)| profile.id)
        .unwrap_or_else(|| state.settings().active_profile_id())
}
