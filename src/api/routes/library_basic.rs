use super::{auth_token_from_headers, internal_error};
use crate::app::auth::ProfileContext;
use crate::app::state::AppState;
use crate::library::{FavoriteAlbumRemoveRequest, FavoriteAlbumRequest, LibraryBrowseQuery};
use crate::services::qobuz::QobuzArtistImageResponse;
use axum::{
    Json, Router,
    extract::{ConnectInfo, Extension, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use futures_util::stream::{FuturesUnordered, StreamExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path as StdPath, PathBuf};
use std::sync::Arc;

const RECENT_QOBUZ_ALBUM_CACHE_WARM_LIMIT: usize = 10;
const RECENT_QOBUZ_ALBUM_STREAM_WARM_TRACKS: usize = 8;
const ARTIST_PROFILE_IMAGE_WARM_DEFAULT_LIMIT: i64 = 24;
const ARTIST_PROFILE_IMAGE_WARM_MAX_LIMIT: i64 = 48;
const ARTIST_PROFILE_IMAGE_WARM_CONCURRENCY: usize = 4;

#[derive(Deserialize, JsonSchema)]
pub struct LibraryFolderRequest {
    pub path: String,
}

#[derive(Serialize, JsonSchema)]
pub struct LibraryFoldersResponse {
    pub folders: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct LibraryFolderPickResponse {
    pub path: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct RecentAlbumsQuery {
    pub limit: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct LibrarySearchQuery {
    q: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct LibraryBrowseQueryParams {
    q: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
    sort: Option<String>,
    direction: Option<String>,
    genre: Option<String>,
    decade: Option<i32>,
    quality: Option<String>,
    source: Option<String>,
    include_facets: Option<bool>,
}

#[derive(Deserialize)]
struct ArtistProfileImageCacheWarmRequest {
    limit: Option<i64>,
}

#[derive(Serialize)]
struct ArtistProfileImageCacheWarmResponse {
    requested: usize,
    resolved: usize,
    failed: usize,
    images: Vec<QobuzArtistImageResponse>,
}

/// Local-only routes composed with the remote-safe set. Folder management
/// exposes and mutates local filesystem paths and must never be reachable on
/// the remote listener.
pub fn routes() -> Router<AppState> {
    remote_routes()
        .route(
            "/api/library/folders",
            get(library_folders)
                .post(library_add_folder)
                .delete(library_remove_folder),
        )
        .route("/api/library/folders/pick", post(library_pick_folder))
}

/// Library browse routes that are safe to expose on the authenticated remote
/// listener. Composing `routes()` from this set avoids allowlist drift.
pub fn remote_routes() -> Router<AppState> {
    Router::new()
        .route("/api/library/summary", get(library_summary))
        .route("/api/library/albums", get(library_albums))
        .route("/api/library/browse/albums", get(library_browse_albums))
        .route("/api/library/browse/tracks", get(library_browse_tracks))
        .route("/api/library/browse/artists", get(library_browse_artists))
        .route("/api/library/recent-albums", get(recent_albums))
        .route(
            "/api/library/favorite-albums",
            get(library_favorite_albums)
                .post(library_add_favorite_album)
                .delete(library_remove_favorite_album),
        )
        .route("/api/library/tracks", get(library_tracks))
        .route("/api/library/artists", get(library_artists))
        .route(
            "/api/library/artists/profile-image-cache/warm",
            post(library_warm_artist_profile_image_cache),
        )
        .route("/api/library/search", get(library_search))
}

async fn recent_albums(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    Query(query): Query<RecentAlbumsQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let limit = query.limit.unwrap_or(50);
    let profile_id = request_profile_id(&state, profile);
    let albums = state
        .library()
        .run_blocking(move |library| library.recent_albums_for_profile(&profile_id, limit))
        .await
        .map_err(internal_error)?;
    let mut qobuz_album_ids: Vec<(String, Option<u64>)> = Vec::new();
    for album in &albums {
        let Some(album_id) = album.qobuz_album_id.as_deref() else {
            continue;
        };
        if album_id.trim().is_empty()
            || qobuz_album_ids
                .iter()
                .any(|(existing, _)| existing.as_str() == album_id)
        {
            continue;
        }
        let source_track_id = album
            .source_track_id
            .as_deref()
            .and_then(|value| value.parse::<u64>().ok());
        qobuz_album_ids.push((album_id.to_string(), source_track_id));
        if qobuz_album_ids.len() >= RECENT_QOBUZ_ALBUM_CACHE_WARM_LIMIT {
            break;
        }
    }
    if !qobuz_album_ids.is_empty() {
        let qobuz = Arc::clone(state.qobuz());
        tokio::spawn(async move {
            for (album_id, source_track_id) in qobuz_album_ids {
                if let Err(err) = qobuz
                    .warm_album_stream_link_cache(
                        &album_id,
                        source_track_id,
                        RECENT_QOBUZ_ALBUM_STREAM_WARM_TRACKS,
                    )
                    .await
                {
                    eprintln!(
                        "qobuz: recent album playback cache warm failed for {album_id}: {err}"
                    );
                }
            }
        });
    }
    Ok(Json(albums))
}

async fn library_summary(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .library()
        .run_blocking(|library| library.summary())
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn library_albums(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .library()
        .run_blocking(|library| library.albums())
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn library_browse_albums(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    Query(query): Query<LibraryBrowseQueryParams>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let query = query.into();
    let profile_id = request_profile_id(&state, profile);
    state
        .library()
        .run_blocking(move |library| library.browse_albums_for_profile(&profile_id, query))
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn library_browse_tracks(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    Query(query): Query<LibraryBrowseQueryParams>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let query = query.into();
    let profile_id = request_profile_id(&state, profile);
    state
        .library()
        .run_blocking(move |library| library.browse_tracks_for_profile(&profile_id, query))
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn library_browse_artists(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    Query(query): Query<LibraryBrowseQueryParams>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let query = query.into();
    let profile_id = request_profile_id(&state, profile);
    state
        .library()
        .run_blocking(move |library| library.browse_artists_for_profile(&profile_id, query))
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn library_favorite_albums(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .library()
        .run_blocking(|library| library.favorite_albums())
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn library_add_favorite_album(
    State(state): State<AppState>,
    Json(req): Json<FavoriteAlbumRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .library()
        .run_blocking(move |library| library.add_favorite_album(req))
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn library_remove_favorite_album(
    State(state): State<AppState>,
    Json(req): Json<FavoriteAlbumRemoveRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .library()
        .run_blocking(move |library| library.remove_favorite_album(req))
        .await
        .map(|removed| Json(serde_json::json!({ "removed": removed })))
        .map_err(internal_error)
}

async fn library_tracks(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let profile_id = request_profile_id(&state, profile);
    state
        .library()
        .run_blocking(move |library| library.tracks_for_profile(&profile_id))
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn library_artists(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let profile_id = request_profile_id(&state, profile);
    state
        .library()
        .run_blocking(move |library| library.artists_for_profile(&profile_id))
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn library_warm_artist_profile_image_cache(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    Json(req): Json<ArtistProfileImageCacheWarmRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let limit = req
        .limit
        .unwrap_or(ARTIST_PROFILE_IMAGE_WARM_DEFAULT_LIMIT)
        .clamp(1, ARTIST_PROFILE_IMAGE_WARM_MAX_LIMIT);
    let profile_id = request_profile_id(&state, profile);
    let names = artist_profile_image_warm_names(&state, &profile_id, limit)
        .await
        .map_err(internal_error)?;
    let requested = names.len();
    let mut pending = FuturesUnordered::new();
    let mut images = Vec::new();
    let mut failed = 0_usize;
    let qobuz = Arc::clone(state.qobuz());

    for name in names {
        let qobuz = Arc::clone(&qobuz);
        pending.push(async move { qobuz.cache_artist_portrait(&name).await });
        if pending.len() >= ARTIST_PROFILE_IMAGE_WARM_CONCURRENCY {
            match pending.next().await {
                Some(Ok(image)) => images.push(image),
                Some(Err(err)) => {
                    failed += 1;
                    eprintln!("qobuz: artist portrait cache warm failed: {err}");
                }
                None => {}
            }
        }
    }

    while let Some(result) = pending.next().await {
        match result {
            Ok(image) => images.push(image),
            Err(err) => {
                failed += 1;
                eprintln!("qobuz: artist portrait cache warm failed: {err}");
            }
        }
    }

    let resolved = images
        .iter()
        .filter(|image| image.local_image_url.is_some())
        .count();
    Ok(Json(ArtistProfileImageCacheWarmResponse {
        requested,
        resolved,
        failed,
        images,
    }))
}

async fn library_search(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    Query(query): Query<LibrarySearchQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let query = query.q.unwrap_or_default();
    let profile_id = request_profile_id(&state, profile);
    state
        .library()
        .run_blocking(move |library| library.search_for_profile(&profile_id, &query))
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn artist_profile_image_warm_names(
    state: &AppState,
    profile_id: &str,
    limit: i64,
) -> Result<Vec<String>, String> {
    let profile_id = profile_id.to_string();
    state
        .library()
        .run_blocking(move |library| {
            artist_profile_image_warm_names_blocking(library, &profile_id, limit)
        })
        .await
}

fn artist_profile_image_warm_names_blocking(
    library: &crate::library::Library,
    profile_id: &str,
    limit: i64,
) -> Result<Vec<String>, String> {
    let presets = [
        ("popularity", "desc"),
        ("name", "desc"),
        ("albums", "desc"),
        ("songs", "desc"),
    ];
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for (sort, direction) in presets {
        let page = library.browse_artists_for_profile(
            profile_id,
            LibraryBrowseQuery {
                q: None,
                limit,
                offset: 0,
                sort: Some(sort.to_string()),
                direction: Some(direction.to_string()),
                genre: None,
                decade: None,
                quality: None,
                source: None,
                include_facets: false,
            },
        )?;
        for artist in page.items {
            let name = artist.name.trim();
            if name.is_empty() {
                continue;
            }
            let key = name.to_lowercase();
            if seen.insert(key) {
                names.push(name.to_string());
            }
        }
    }
    Ok(names)
}

fn request_profile_id(state: &AppState, profile: Option<Extension<ProfileContext>>) -> String {
    profile
        .map(|Extension(profile)| profile.id)
        .unwrap_or_else(|| state.settings().active_profile_id())
}

async fn library_folders(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
) -> Result<Json<LibraryFoldersResponse>, (StatusCode, String)> {
    require_local_filesystem_access(
        &state,
        &headers,
        peer.as_ref().map(|ConnectInfo(addr)| *addr),
    )?;
    Ok(library_folders_response(&state))
}

fn library_folders_response(state: &AppState) -> Json<LibraryFoldersResponse> {
    Json(LibraryFoldersResponse {
        folders: state
            .library()
            .music_dirs()
            .into_iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect(),
    })
}

async fn library_add_folder(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
    Json(req): Json<LibraryFolderRequest>,
) -> Result<Json<LibraryFoldersResponse>, (StatusCode, String)> {
    require_local_filesystem_access(
        &state,
        &headers,
        peer.as_ref().map(|ConnectInfo(addr)| *addr),
    )?;
    let path = normalize_music_folder(&req.path)?;
    let mut folders = state.library().music_dirs();
    if !folders.iter().any(|existing| same_path(existing, &path)) {
        folders.push(path);
        persist_music_folders(&state, folders)?;
    }
    Ok(library_folders_response(&state))
}

async fn library_pick_folder(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
) -> Result<Json<LibraryFolderPickResponse>, (StatusCode, String)> {
    require_local_filesystem_access(
        &state,
        &headers,
        peer.as_ref().map(|ConnectInfo(addr)| *addr),
    )?;
    Ok(Json(LibraryFolderPickResponse {
        path: pick_music_folder()?,
    }))
}

async fn library_remove_folder(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
    Json(req): Json<LibraryFolderRequest>,
) -> Result<Json<LibraryFoldersResponse>, (StatusCode, String)> {
    require_local_filesystem_access(
        &state,
        &headers,
        peer.as_ref().map(|ConnectInfo(addr)| *addr),
    )?;
    let target = normalize_music_folder_for_remove(&req.path)?;
    let mut folders = state.library().music_dirs();
    folders.retain(|existing| !same_path(existing, &target));
    persist_music_folders(&state, folders)?;
    Ok(library_folders_response(&state))
}

#[cfg(target_os = "macos")]
fn pick_music_folder() -> Result<Option<String>, (StatusCode, String)> {
    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg(r#"POSIX path of (choose folder with prompt "Choose a music folder")"#)
        .output()
        .map_err(|e| internal_error(format!("Could not open Finder folder picker: {e}")))?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Ok((!path.is_empty()).then_some(path));
    }
    let error = String::from_utf8_lossy(&output.stderr);
    if error.contains("User canceled") {
        return Ok(None);
    }
    Err(internal_error(format!(
        "Finder folder picker failed: {}",
        error.trim()
    )))
}

#[cfg(not(target_os = "macos"))]
fn pick_music_folder() -> Result<Option<String>, (StatusCode, String)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        "Folder picker is only available on macOS".to_string(),
    ))
}

fn require_local_filesystem_access(
    state: &AppState,
    headers: &HeaderMap,
    peer_addr: Option<SocketAddr>,
) -> Result<(), (StatusCode, String)> {
    let token = auth_token_from_headers(headers);
    if state.pairing().verify_control_token(token.as_deref())
        || crate::app::auth::local_filesystem_request_allowed(headers, peer_addr)
    {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            "Music folder management is only available from the local control UI".to_string(),
        ))
    }
}

fn normalize_music_folder(raw: &str) -> Result<PathBuf, (StatusCode, String)> {
    let trimmed = raw.trim().trim_matches('"');
    if trimmed.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Folder path is required".to_string(),
        ));
    }
    let path = PathBuf::from(trimmed);
    let canonical = std::fs::canonicalize(&path)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Folder not found".to_string()))?;
    if !canonical.is_dir() {
        return Err((StatusCode::BAD_REQUEST, "Path must be a folder".to_string()));
    }
    Ok(clean_windows_verbatim(canonical))
}

fn normalize_music_folder_for_remove(raw: &str) -> Result<PathBuf, (StatusCode, String)> {
    let trimmed = raw.trim().trim_matches('"');
    if trimmed.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Folder path is required".to_string(),
        ));
    }
    let path = PathBuf::from(trimmed);
    Ok(std::fs::canonicalize(&path)
        .map(clean_windows_verbatim)
        .unwrap_or_else(|_| clean_windows_verbatim(path)))
}

fn same_path(left: &StdPath, right: &StdPath) -> bool {
    same_path_text(
        &normalized_path_string(left),
        &normalized_path_string(right),
    )
}

impl From<LibraryBrowseQueryParams> for LibraryBrowseQuery {
    fn from(value: LibraryBrowseQueryParams) -> Self {
        Self {
            q: value.q,
            limit: value.limit.unwrap_or(0),
            offset: value.offset.unwrap_or(0),
            sort: value.sort,
            direction: value.direction,
            genre: value.genre,
            decade: value.decade,
            quality: value.quality,
            source: value.source,
            include_facets: value.include_facets.unwrap_or(true),
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn same_path_text(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn same_path_text(left: &str, right: &str) -> bool {
    left == right
}

fn persist_music_folders(
    state: &AppState,
    folders: Vec<PathBuf>,
) -> Result<(), (StatusCode, String)> {
    let folders: Vec<PathBuf> = folders.into_iter().map(clean_windows_verbatim).collect();
    state
        .settings()
        .try_update(|settings| {
            settings.music_dirs = Some(
                folders
                    .iter()
                    .map(|path| path.to_string_lossy().to_string())
                    .collect(),
            );
        })
        .map_err(internal_error)?;
    state.library().set_music_dirs(folders);
    Ok(())
}

fn normalized_path_string(path: &StdPath) -> String {
    clean_windows_verbatim(path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

fn clean_windows_verbatim(path: PathBuf) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(stripped) = raw.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{stripped}"));
    }
    if let Some(stripped) = raw.strip_prefix(r"\\?\") {
        return PathBuf::from(stripped);
    }
    path
}
