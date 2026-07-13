use crate::app::state::AppState;
use crate::settings::ListeningProfile;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post, put},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, JsonSchema)]
pub struct ProfilesResponse {
    pub profiles: Vec<ListeningProfile>,
    pub active_profile_id: String,
}

#[derive(Deserialize)]
pub struct CreateProfileRequest {
    pub name: String,
}

#[derive(Deserialize)]
pub struct SelectProfileRequest {
    pub profile_id: String,
}

#[derive(Deserialize)]
pub struct UpdateProfileRequest {
    pub name: String,
    pub color: String,
    pub image: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateRecentSearchesRequest {
    pub searches: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct RecentSearchesResponse {
    pub searches: Vec<String>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/profiles", get(profiles).post(create_profile))
        .route(
            "/api/profiles/:profile_id",
            put(update_profile).delete(delete_profile),
        )
        .route(
            "/api/profiles/:profile_id/recent-searches",
            get(recent_searches).put(update_recent_searches),
        )
        .route("/api/profiles/select", post(select_profile))
}

async fn profiles(State(state): State<AppState>) -> impl IntoResponse {
    Json(ProfilesResponse {
        profiles: state.settings().profiles(),
        active_profile_id: state.settings().active_profile_id(),
    })
}

async fn create_profile(
    State(state): State<AppState>,
    Json(req): Json<CreateProfileRequest>,
) -> impl IntoResponse {
    match state.settings().create_profile(&req.name) {
        Ok(profile) => Json(ProfilesResponse {
            profiles: state.settings().profiles(),
            active_profile_id: profile.id,
        })
        .into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

async fn select_profile(
    State(state): State<AppState>,
    Json(req): Json<SelectProfileRequest>,
) -> impl IntoResponse {
    match state.settings().select_profile(&req.profile_id) {
        Ok(profile) => Json(ProfilesResponse {
            profiles: state.settings().profiles(),
            active_profile_id: profile.id,
        })
        .into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e).into_response(),
    }
}

async fn update_profile(
    State(state): State<AppState>,
    Path(profile_id): Path<String>,
    Json(req): Json<UpdateProfileRequest>,
) -> impl IntoResponse {
    match state
        .settings()
        .update_profile(&profile_id, &req.name, &req.color, req.image.as_deref())
    {
        Ok(_) => Json(ProfilesResponse {
            profiles: state.settings().profiles(),
            active_profile_id: state.settings().active_profile_id(),
        })
        .into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

async fn recent_searches(
    State(state): State<AppState>,
    Path(profile_id): Path<String>,
) -> impl IntoResponse {
    let profiles = state.settings().profiles();
    let Some(profile) = profiles
        .into_iter()
        .find(|profile| profile.id == profile_id)
    else {
        return (StatusCode::NOT_FOUND, "Profile not found").into_response();
    };
    Json(RecentSearchesResponse {
        searches: profile.recent_searches,
    })
    .into_response()
}

async fn update_recent_searches(
    State(state): State<AppState>,
    Path(profile_id): Path<String>,
    Json(req): Json<UpdateRecentSearchesRequest>,
) -> impl IntoResponse {
    match state
        .settings()
        .update_profile_recent_searches(&profile_id, &req.searches)
    {
        Ok(searches) => Json(RecentSearchesResponse { searches }).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e).into_response(),
    }
}

async fn delete_profile(
    State(state): State<AppState>,
    Path(profile_id): Path<String>,
) -> impl IntoResponse {
    match state.settings().delete_profile(&profile_id) {
        Ok(active_profile_id) => Json(ProfilesResponse {
            profiles: state.settings().profiles(),
            active_profile_id,
        })
        .into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}
