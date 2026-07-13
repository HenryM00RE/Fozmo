use crate::api::error::ApiError;
use crate::api::routes::remote_artwork::{
    REMOTE_LIBRARY_ARTWORK_SIZE, REMOTE_NOW_PLAYING_ARTWORK_SIZE, is_remote,
};
use crate::app::auth::RequestSurface;
use crate::app::state::AppState;
use crate::audio::player::read_track_metadata;
use crate::playback::resolver::resolve_music_file_name;
use crate::protocol::SourceRef;
use axum::{
    Router,
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use image::codecs::jpeg::JpegEncoder;
use serde::Deserialize;

const ARTWORK_CACHE_CONTROL: &str = "public, max-age=86400, stale-while-revalidate=604800";
// For responses we can't tie to the requested source: never let the browser
// cache them under a per-track URL.
const ARTWORK_NO_STORE: &str = "no-store";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/library/art/:id", get(library_art))
        .route("/api/cover", get(get_cover))
        .route("/api/zones/:zone_id/cover", get(get_zone_cover))
        .route(
            "/api/zones/:zone_id/now-playing-art",
            get(get_zone_now_playing_art),
        )
        .route("/api/files/:name/cover", get(get_file_cover))
}

#[derive(Deserialize)]
struct LibraryArtQuery {
    size: Option<u32>,
}

async fn library_art(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
    Path(id): Path<i64>,
    Query(query): Query<LibraryArtQuery>,
) -> impl IntoResponse {
    let size = query
        .size
        .or_else(|| is_remote(&surface).then_some(REMOTE_LIBRARY_ARTWORK_SIZE));
    library_art_response(&state, id, size).await
}

async fn library_art_response(
    state: &AppState,
    id: i64,
    size: Option<u32>,
) -> axum::response::Response {
    let art = state
        .library()
        .run_blocking(move |library| match size {
            Some(size) => library.art_thumbnail(id, size),
            None => library.art(id),
        })
        .await;
    match art {
        Ok(Some((mime, data))) => bytes_response(&mime, data, ARTWORK_CACHE_CONTROL),
        Ok(None) => (StatusCode::NOT_FOUND, "No artwork").into_response(),
        Err(e) => ApiError::internal(e).into_response(),
    }
}

async fn get_cover(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
) -> impl IntoResponse {
    let player = state.zones().active_player();
    cover_response(player.current_cover(), is_remote(&surface))
}

async fn get_zone_cover(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
    Path(zone_id): Path<String>,
) -> impl IntoResponse {
    let cover = state
        .zones()
        .player_for_zone(&zone_id)
        .and_then(|player| player.current_cover());
    cover_response(cover, is_remote(&surface))
}

#[derive(Deserialize)]
struct NowPlayingArtQuery {
    source: Option<String>,
}

async fn get_zone_now_playing_art(
    State(state): State<AppState>,
    surface: Option<Extension<RequestSurface>>,
    Path(zone_id): Path<String>,
    Query(query): Query<NowPlayingArtQuery>,
) -> impl IntoResponse {
    let remote = is_remote(&surface);
    // When the client names a source, resolve artwork for that source rather
    // than whatever the player holds right now. The response is cached by URL
    // for a day, so serving the live cover during a track change would pin the
    // wrong album art to this track's URL.
    if let Some(key) = query.source.as_deref().filter(|key| !key.is_empty()) {
        let source = source_for_art_key(&state, &zone_id, key).await;
        match source {
            Some(SourceRef::LocalTrack {
                art_id: Some(art_id),
                ..
            }) => {
                return library_art_response(
                    &state,
                    art_id,
                    remote.then_some(REMOTE_NOW_PLAYING_ARTWORK_SIZE),
                )
                .await;
            }
            Some(SourceRef::QobuzTrack {
                image_url: Some(url),
                ..
            }) => return qobuz_cover_response(&state, &url, remote).await,
            _ => {}
        }

        // The source has no artwork reference of its own. The live player
        // cover is only trustworthy if that source is what's playing (or
        // nothing is being tracked), and even then it must not be cached
        // against this URL.
        if let Some(cover) = state.upnp().current_art_for_key(&zone_id, key) {
            return cover_bytes_response(&cover.mime, cover.data, ARTWORK_NO_STORE, remote);
        }
        let active = state.listening().active_source(&zone_id);
        let cover = active
            .is_none_or(|active| active.key() == key)
            .then(|| {
                state
                    .zones()
                    .player_for_zone(&zone_id)
                    .and_then(|player| player.current_cover())
            })
            .flatten();
        return match cover {
            Some(cover) => cover_bytes_response(&cover.mime, cover.data, ARTWORK_NO_STORE, remote),
            None => not_found_no_store(),
        };
    }

    if let Some(cover) = state
        .zones()
        .player_for_zone(&zone_id)
        .and_then(|player| player.current_cover())
    {
        return cover_response(Some(cover), remote);
    }

    match state.listening().active_source(&zone_id) {
        Some(SourceRef::LocalTrack {
            art_id: Some(art_id),
            ..
        }) => {
            library_art_response(
                &state,
                art_id,
                remote.then_some(REMOTE_NOW_PLAYING_ARTWORK_SIZE),
            )
            .await
        }
        Some(SourceRef::QobuzTrack {
            image_url: Some(url),
            ..
        }) => qobuz_cover_response(&state, &url, remote).await,
        _ => not_found_no_store(),
    }
}

async fn source_for_art_key(state: &AppState, zone_id: &str, key: &str) -> Option<SourceRef> {
    // Local tracks resolve through the library, which also picks up
    // album-level artwork the in-flight source ref may be missing.
    if let Some(track_id) = key
        .strip_prefix("local:")
        .and_then(|id| id.parse::<i64>().ok())
        && let Ok(Some(source)) = state
            .library()
            .run_blocking(move |library| library.source_ref_for_track_id(track_id))
            .await
    {
        return Some(source);
    }
    if let Some(source) = state.upnp().current_source_for_key(zone_id, key) {
        return Some(source);
    }
    state.listening().source_for_key(zone_id, key)
}

fn not_found_no_store() -> axum::response::Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(ARTWORK_NO_STORE),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    (StatusCode::NOT_FOUND, headers, "No now-playing artwork").into_response()
}

// Live player covers are volatile state, not content addressed by the URL.
// The `?v=<cover_version>` cache-buster the UI appends restarts from zero with
// the process, so a cached `?v=N` from a previous run would pin another
// track's artwork for a day.
fn cover_response(
    cover: Option<crate::audio::player::TrackCover>,
    remote: bool,
) -> axum::response::Response {
    match cover {
        Some(cover) => cover_bytes_response(&cover.mime, cover.data, ARTWORK_NO_STORE, remote),
        None => not_found_no_store(),
    }
}

async fn qobuz_cover_response(
    state: &AppState,
    url: &str,
    remote: bool,
) -> axum::response::Response {
    let sized_url;
    let url = if remote {
        sized_url = crate::services::qobuz::sized_cover_url(url, REMOTE_NOW_PLAYING_ARTWORK_SIZE);
        sized_url.as_str()
    } else {
        url
    };
    match state.qobuz().fetch_cover_public(url).await {
        Ok((Some(mime), Some(data))) => bytes_response(&mime, data, ARTWORK_CACHE_CONTROL),
        Ok(_) => (StatusCode::NOT_FOUND, "No Qobuz artwork").into_response(),
        Err(e) => ApiError::upstream(e).into_response(),
    }
}

fn bytes_response(
    mime: &str,
    data: Vec<u8>,
    cache_control: &'static str,
) -> axum::response::Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    let Some(safe_mime) = crate::library::safe_raster_artwork_mime(&data, mime) else {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            headers,
            "Unsupported artwork",
        )
            .into_response();
    };
    let mime = HeaderValue::from_str(safe_mime)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    headers.insert(header::CONTENT_TYPE, mime);
    (StatusCode::OK, headers, data).into_response()
}

fn cover_bytes_response(
    mime: &str,
    data: Vec<u8>,
    cache_control: &'static str,
    remote: bool,
) -> axum::response::Response {
    if !remote {
        return bytes_response(mime, data, cache_control);
    }
    match thumbnail_bytes(data, REMOTE_NOW_PLAYING_ARTWORK_SIZE) {
        Some((mime, data)) => bytes_response(&mime, data, cache_control),
        None => {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            );
            headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static(cache_control),
            );
            (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                headers,
                "Unsupported artwork",
            )
                .into_response()
        }
    }
}

fn thumbnail_bytes(data: Vec<u8>, size: u32) -> Option<(String, Vec<u8>)> {
    let Ok(image) = image::load_from_memory(&data) else {
        return None;
    };
    let thumb = image.thumbnail(size, size).to_rgb8();
    let mut encoded = Vec::new();
    if JpegEncoder::new_with_quality(&mut encoded, 84)
        .encode_image(&thumb)
        .is_err()
    {
        return None;
    }
    Some(("image/jpeg".to_string(), encoded))
}

async fn get_file_cover(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let path = match resolve_music_file_name(state.music_dir(), &name) {
        Ok(path) => path,
        Err(error) => return ApiError::from(error).into_response(),
    };
    if !path.is_file() {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }

    let (_, cover) = read_track_metadata(&path);
    match cover {
        Some(cover) => bytes_response(&cover.mime, cover.data, "public, max-age=3600"),
        None => (StatusCode::NOT_FOUND, "No cover").into_response(),
    }
}
