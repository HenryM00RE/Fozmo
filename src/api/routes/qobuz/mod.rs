use crate::app::state::AppState;
use axum::{
    Router,
    routing::{get, post},
};

mod albums;
mod artists;
mod auth;
mod cache;
mod home;
mod playback;
mod playlists;
mod radio;
mod search;
mod settings;

pub(super) use super::internal_error;
pub(super) use super::remote_artwork::{
    REMOTE_DETAIL_ARTWORK_SIZE, REMOTE_GRID_ARTWORK_SIZE, artwork_json,
};
pub use auth::QobuzStatusResponse;
#[cfg(test)]
pub(super) use radio::{QobuzRadioNextRequest, QobuzRadioSeed};
pub(crate) use search::QobuzSearchQuery;

/// Local-only routes composed with the remote-safe set. Account/session
/// management, OAuth, settings, and cache administration must never be
/// reachable on the remote listener.
pub fn routes() -> Router<AppState> {
    remote_routes()
        .route("/api/qobuz/init", post(auth::qobuz_init))
        .route("/api/qobuz/login", post(auth::qobuz_login))
        .route("/api/qobuz/logout", post(auth::qobuz_logout))
        .route("/api/qobuz/oauth/start", get(auth::qobuz_oauth_start))
        .route("/api/qobuz/oauth/callback", get(auth::qobuz_oauth_callback))
        .route(
            "/api/qobuz/settings",
            get(settings::qobuz_settings).post(settings::update_qobuz_settings),
        )
        .route("/api/qobuz/cache", get(cache::qobuz_cache_info))
        .route("/api/qobuz/cache/clear", post(cache::qobuz_cache_clear))
}

/// Read/browse/playback routes that are safe to expose on the authenticated
/// remote listener. Composing `routes()` from this set avoids allowlist drift.
pub fn remote_routes() -> Router<AppState> {
    Router::new()
        .route("/api/qobuz/status", get(auth::qobuz_status))
        .route("/api/qobuz/play", post(playback::qobuz_play))
        .route("/api/qobuz/prefetch", post(playback::qobuz_prefetch))
        .route("/api/qobuz/home", get(home::qobuz_home))
        .route("/api/qobuz/home/section", get(home::qobuz_home_section))
        .route(
            "/api/qobuz/playlists/featured",
            get(playlists::qobuz_featured_playlists),
        )
        .route(
            "/api/qobuz/playlists/tags",
            get(playlists::qobuz_playlist_tags),
        )
        .route("/api/qobuz/genres", get(playlists::qobuz_genres))
        .route(
            "/api/qobuz/playlists/:id",
            get(playlists::qobuz_playlist_detail),
        )
        .route(
            "/api/qobuz/home/album-of-the-week",
            get(home::qobuz_home_album_of_the_week),
        )
        .route("/api/qobuz/search", get(search::qobuz_search))
        .route("/api/qobuz/search/albums", get(search::qobuz_search_albums))
        .route("/api/qobuz/albums", get(albums::qobuz_favorite_albums))
        .route("/api/qobuz/albums/:id", get(albums::qobuz_album_detail))
        .route("/api/qobuz/tracks/:id", get(albums::qobuz_track_detail))
        .route(
            "/api/qobuz/artists/search",
            get(artists::qobuz_search_artists),
        )
        .route("/api/qobuz/artists/image", get(artists::qobuz_artist_image))
        .route(
            "/api/qobuz/artists/image-cache",
            get(artists::qobuz_artist_image_cache),
        )
        .route("/api/qobuz/artists/:id", get(artists::qobuz_artist_detail))
        .route(
            "/api/qobuz/artists/:id/core",
            get(artists::qobuz_artist_core),
        )
        .route(
            "/api/qobuz/artists/:id/top-tracks",
            get(artists::qobuz_artist_top_tracks),
        )
        .route(
            "/api/qobuz/artists/:id/similar",
            get(artists::qobuz_artist_similar),
        )
        .route("/api/qobuz/radio/next", post(radio::qobuz_radio_next))
}
