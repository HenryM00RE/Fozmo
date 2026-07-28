use super::playback_sequence::playback_request_sequence_from_headers;
use crate::app::auth::ProfileContext;
use crate::app::state::AppState;
use crate::library::{
    AlbumDetail, AlbumVersionSummary, AppleMusicAlbumMatchPreview, AppleMusicVersionDetail,
    QobuzAppleMusicLink, QobuzAppleMusicMatch, apple_music_match_for_qobuz_album,
};
use crate::playback::commands::accept_playback_request_sequence;
use crate::playback::intent::{PlaybackGuard, PlaybackIntent};
use crate::playback::resolver::{QueueRequestItem, source_ref_from_queue_request};
use crate::playback::router::PlaybackRouter;
use crate::protocol::SourceRef;
use crate::services::apple_music_musickit::{
    AppleCatalogAlbum, AppleCatalogSearchResult, AppleCatalogSong, AppleMusicAlbumVersionRequest,
    AppleMusicAuthorizeRequest, AppleMusicCatalogQuery, AppleMusicCatalogSearchQuery,
    AppleMusicMvpError, AppleMusicMvpStatus, AppleMusicPlayRequest,
    AppleQueuePlaylistCleanupRequest, AppleQueuePlaylistInventory,
};
use axum::{
    Json, Router,
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, collections::HashSet};

type AppleMusicApiResult =
    Result<Json<AppleMusicMvpStatus>, (StatusCode, Json<AppleMusicMvpError>)>;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/apple-music/status", get(status))
        .route("/api/apple-music/launch", post(launch))
        .route("/api/apple-music/authorize", post(authorize))
        .route("/api/apple-music/play", post(play))
        .route("/api/apple-music/queue-playlists", get(queue_playlists))
        .route(
            "/api/apple-music/queue-playlists/cleanup",
            post(cleanup_queue_playlists),
        )
        .route("/api/apple-music/catalog/search", get(search_catalog))
        .route("/api/apple-music/catalog/songs/:id", get(lookup_song))
        .route("/api/apple-music/catalog/albums/:id", get(lookup_album))
        .route(
            "/api/library/albums/:id/apple-music/preview",
            get(preview_album_version),
        )
        .route(
            "/api/library/albums/:id/apple-music/match",
            post(match_album_version),
        )
        .route(
            "/api/library/albums/:id/apple-music/link",
            post(link_album_version),
        )
        .route(
            "/api/library/albums/:id/apple-music/unlink",
            post(unlink_album_version),
        )
        .route(
            "/api/library/albums/:id/apple-music/versions/:version_id",
            get(album_version_detail),
        )
        .route(
            "/api/library/apple-music-albums/:id",
            get(linked_library_album),
        )
        .route(
            "/api/apple-music/qobuz-albums/:id/version",
            get(qobuz_album_version),
        )
        .route("/api/apple-music/shutdown", post(shutdown))
}

#[derive(Debug, Serialize)]
struct AppleMusicAlbumMatchResponse {
    status: String,
    linked_version: Option<AlbumVersionSummary>,
    candidates: Vec<AppleMusicAlbumMatchPreview>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AppleMusicAlbumMatchQuery {
    storefront: Option<String>,
    #[serde(default)]
    review: bool,
}

async fn launch(State(state): State<AppState>) -> AppleMusicApiResult {
    state
        .apple_music()
        .launch()
        .await
        .map(Json)
        .map_err(api_error)
}

async fn status(State(state): State<AppState>) -> AppleMusicApiResult {
    state
        .apple_music()
        .refresh_status()
        .await
        .map(Json)
        .map_err(api_error)
}

async fn queue_playlists(
    State(state): State<AppState>,
) -> Result<Json<AppleQueuePlaylistInventory>, (StatusCode, Json<AppleMusicMvpError>)> {
    state
        .apple_music()
        .queue_playlist_inventory()
        .await
        .map(Json)
        .map_err(api_error)
}

async fn cleanup_queue_playlists(
    State(state): State<AppState>,
    Json(request): Json<AppleQueuePlaylistCleanupRequest>,
) -> Result<Json<AppleQueuePlaylistInventory>, (StatusCode, Json<AppleMusicMvpError>)> {
    state
        .apple_music()
        .cleanup_queue_playlists(0, &request.legacy_web_playlist_ids)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn authorize(
    State(state): State<AppState>,
    Json(request): Json<AppleMusicAuthorizeRequest>,
) -> AppleMusicApiResult {
    state
        .apple_music()
        .authorize(request.present_ui)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn lookup_song(
    State(state): State<AppState>,
    Path(song_id): Path<String>,
    Query(query): Query<AppleMusicCatalogQuery>,
) -> Result<Json<AppleCatalogSong>, (StatusCode, Json<AppleMusicMvpError>)> {
    state
        .apple_music()
        .lookup_song(song_id, query.storefront)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn search_catalog(
    State(state): State<AppState>,
    Query(query): Query<AppleMusicCatalogSearchQuery>,
) -> Result<Json<AppleCatalogSearchResult>, (StatusCode, Json<AppleMusicMvpError>)> {
    state
        .apple_music()
        .search_songs(query.term, query.storefront, query.limit)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn lookup_album(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    Path(album_id): Path<String>,
    Query(query): Query<AppleMusicCatalogQuery>,
) -> Result<Json<AppleCatalogAlbum>, (StatusCode, Json<AppleMusicMvpError>)> {
    let album = state
        .apple_music()
        .lookup_album(album_id, query.storefront)
        .await
        .map_err(api_error)?;
    let profile_id = profile
        .map(|Extension(profile)| profile.id)
        .unwrap_or_else(|| state.settings().active_profile_id());
    let album = with_verified_formats(&state, album);
    Ok(Json(
        with_playback_summaries(&state, profile_id, album).await,
    ))
}

/// Apple reports nothing about how often this listener has played a track, so
/// carry Fozmo's own history onto the catalog album the same way the Qobuz
/// album detail does. Plays recorded against a linked local or Qobuz edition
/// roll up here too, because the summary lookup resolves by recording.
async fn with_playback_summaries(
    state: &AppState,
    profile_id: String,
    mut album: AppleCatalogAlbum,
) -> AppleCatalogAlbum {
    let keys = album
        .tracks
        .iter()
        .map(|track| format!("apple_music:{}", track.song_id))
        .collect::<Vec<_>>();
    if keys.is_empty() {
        return album;
    }
    let Ok(summaries) = state
        .library()
        .run_blocking(move |library| {
            library.playback_summaries_for_keys_for_profile(&profile_id, &keys)
        })
        .await
    else {
        return album;
    };
    for track in &mut album.tracks {
        if let Some(summary) = summaries.get(&format!("apple_music:{}", track.song_id)) {
            track.play_count = summary.play_count;
            track.last_played_at = summary.last_played_at;
            track.listened_secs = summary.listened_secs;
        }
    }
    album
}

/// Apple's catalog cannot report a track's real rate, so replace the advertised
/// tier with the decoder format Fozmo verified the last time each track played.
/// Only this response is enriched: the payload stored when an album version is
/// linked must stay a faithful copy of the catalog.
fn with_verified_formats(state: &AppState, mut album: AppleCatalogAlbum) -> AppleCatalogAlbum {
    let library = state.library();
    album.verified_format = library
        .apple_music_album_verified_format(&album.album_id)
        .ok()
        .flatten()
        .map(verified_format_payload);
    let Ok(by_song) = library.apple_music_track_verified_formats(&album.album_id) else {
        return album;
    };
    for track in &mut album.tracks {
        track.verified_format = by_song
            .get(&track.song_id)
            .cloned()
            .map(verified_format_payload);
    }
    album
}

fn verified_format_payload(
    format: crate::library::AppleMusicVerifiedFormat,
) -> crate::services::apple_music_musickit::AppleVerifiedFormat {
    crate::services::apple_music_musickit::AppleVerifiedFormat {
        codec: format.codec,
        sample_rate: format.sample_rate,
        bit_depth: format.bit_depth,
    }
}

async fn preview_album_version(
    State(state): State<AppState>,
    Path(local_album_id): Path<i64>,
    Query(request): Query<AppleMusicAlbumVersionRequest>,
) -> Result<Json<AppleMusicAlbumMatchPreview>, (StatusCode, Json<AppleMusicMvpError>)> {
    let album = state
        .apple_music()
        .lookup_album(request.album_id, request.storefront)
        .await
        .map_err(api_error)?;
    state
        .library()
        .run_blocking(move |library| {
            library.preview_apple_music_album_version(local_album_id, album)
        })
        .await
        .map_err(library_api_error)?
        .map(Json)
        .ok_or_else(album_not_found)
}

async fn match_album_version(
    State(state): State<AppState>,
    Path(local_album_id): Path<i64>,
    Query(query): Query<AppleMusicAlbumMatchQuery>,
) -> Result<Json<AppleMusicAlbumMatchResponse>, (StatusCode, Json<AppleMusicMvpError>)> {
    let local_detail = state
        .library()
        .run_blocking(move |library| library.album_detail(local_album_id))
        .await
        .map_err(library_api_error)?
        .ok_or_else(album_not_found)?;
    if let Some(version) = local_detail
        .versions
        .iter()
        .find(|version| version.provider == "apple_music")
    {
        if !query.review {
            return Ok(Json(AppleMusicAlbumMatchResponse {
                status: "already_linked".to_string(),
                linked_version: Some(version.clone()),
                candidates: Vec::new(),
                message: None,
            }));
        }
    }

    let artist = local_detail
        .album
        .album_artist
        .as_deref()
        .unwrap_or("")
        .trim();
    let term = if artist.is_empty() {
        local_detail.album.title.clone()
    } else {
        format!("{artist} {}", local_detail.album.title)
    };
    let search = match state
        .apple_music()
        .search_songs(term, query.storefront.clone(), 10)
        .await
    {
        Ok(search) => search,
        Err(error) => {
            return Ok(Json(AppleMusicAlbumMatchResponse {
                status: "unavailable".to_string(),
                linked_version: None,
                candidates: Vec::new(),
                message: Some(error.message),
            }));
        }
    };

    let mut candidates = Vec::new();
    let mut seen_album_ids = HashSet::new();
    for candidate in search.albums.into_iter().take(6) {
        if !seen_album_ids.insert(candidate.album_id.clone()) {
            continue;
        }
        let album = match state
            .apple_music()
            .lookup_album(
                candidate.album_id,
                Some(candidate.storefront).filter(|value| !value.trim().is_empty()),
            )
            .await
        {
            Ok(album) => album,
            Err(_) => continue,
        };
        let preview = state
            .library()
            .run_blocking(move |library| {
                library.preview_apple_music_album_version(local_album_id, album)
            })
            .await
            .map_err(library_api_error)?;
        if let Some(preview) = preview {
            candidates.push(preview);
        }
    }
    candidates.sort_by(compare_apple_match_candidates);

    // Apple can return multiple catalog IDs for recording-identical editions.
    // They are one grouped Apple version in Fozmo, so choose the strongest
    // safe candidate deterministically instead of exposing an unresolvable
    // review state after the candidate UI was removed.
    if !query.review
        && let Some(safe_candidate) = candidates.iter().find(|candidate| candidate.safe_to_link)
    {
        let album = safe_candidate.apple_album.clone();
        let linked_version = state
            .library()
            .run_blocking(move |library| library.link_apple_music_album(local_album_id, &album))
            .await
            .map_err(library_api_error)?
            .ok_or_else(album_not_found)?;
        return Ok(Json(AppleMusicAlbumMatchResponse {
            status: "linked".to_string(),
            linked_version: Some(linked_version),
            candidates,
            message: None,
        }));
    }

    Ok(Json(AppleMusicAlbumMatchResponse {
        status: if candidates.is_empty() {
            "no_match".to_string()
        } else {
            "needs_review".to_string()
        },
        linked_version: None,
        candidates,
        message: None,
    }))
}

fn compare_apple_match_candidates(
    left: &AppleMusicAlbumMatchPreview,
    right: &AppleMusicAlbumMatchPreview,
) -> Ordering {
    apple_match_preference(right)
        .cmp(&apple_match_preference(left))
        .then_with(|| left.apple_album.album_id.cmp(&right.apple_album.album_id))
}

fn apple_match_preference(
    candidate: &AppleMusicAlbumMatchPreview,
) -> (bool, i64, bool, bool, bool, bool, usize) {
    apple_match_preference_parts(
        candidate.safe_to_link,
        candidate.confidence,
        &candidate.evidence,
        &candidate.apple_album,
        candidate.pairings.len(),
    )
}

/// Rank one Apple Music candidate against the others found for the same album,
/// whether it was judged against a local album or a standalone Qobuz one.
fn apple_match_preference_parts(
    safe_to_link: bool,
    confidence: i64,
    evidence: &[String],
    apple_album: &AppleCatalogAlbum,
    paired_track_count: usize,
) -> (bool, i64, bool, bool, bool, bool, usize) {
    let has_evidence = |expected: &str| evidence.iter().any(|evidence| evidence == expected);
    let advertises_lossless = apple_album.audio_variants.iter().any(|variant| {
        matches!(
            normalized_audio_variant(variant).as_str(),
            "lossless" | "highresolutionlossless"
        )
    });
    (
        safe_to_link,
        confidence,
        has_evidence("upc_match"),
        has_evidence("exact_normalized_title"),
        has_evidence("equal_track_count"),
        advertises_lossless,
        paired_track_count,
    )
}

fn normalized_audio_variant(variant: &str) -> String {
    variant
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

async fn link_album_version(
    State(state): State<AppState>,
    Path(local_album_id): Path<i64>,
    Json(request): Json<AppleMusicAlbumVersionRequest>,
) -> Result<Json<AlbumVersionSummary>, (StatusCode, Json<AppleMusicMvpError>)> {
    let album = state
        .apple_music()
        .lookup_album(request.album_id, request.storefront)
        .await
        .map_err(api_error)?;
    state
        .library()
        .run_blocking(move |library| library.link_apple_music_album(local_album_id, &album))
        .await
        .map_err(library_api_error)?
        .map(Json)
        .ok_or_else(album_not_found)
}

async fn unlink_album_version(
    State(state): State<AppState>,
    Path(local_album_id): Path<i64>,
) -> Result<Json<Vec<AlbumVersionSummary>>, (StatusCode, Json<AppleMusicMvpError>)> {
    state
        .library()
        .run_blocking(move |library| library.unlink_apple_music_album(local_album_id))
        .await
        .map_err(library_api_error)?
        .map(Json)
        .ok_or_else(album_not_found)
}

async fn album_version_detail(
    State(state): State<AppState>,
    profile: Option<Extension<ProfileContext>>,
    Path((local_album_id, version_id)): Path<(i64, i64)>,
) -> Result<Json<AppleMusicVersionDetail>, (StatusCode, Json<AppleMusicMvpError>)> {
    let mut detail = state
        .library()
        .run_blocking(move |library| library.apple_music_version_detail(local_album_id, version_id))
        .await
        .map_err(library_api_error)?
        .ok_or_else(|| {
            api_error(apple_music_error(
                "apple_music_version_not_found",
                "The linked Apple Music album version was not found.",
                false,
                "resolving_album_version",
                true,
            ))
        })?;
    // The stored version payload is a frozen copy of the catalog, so the play
    // history has to be attached on the way out — otherwise the Apple edition
    // of an album reads zero next to the local edition's counts.
    let profile_id = profile
        .map(|Extension(profile)| profile.id)
        .unwrap_or_else(|| state.settings().active_profile_id());
    detail.apple_album = with_playback_summaries(&state, profile_id, detail.apple_album).await;
    Ok(Json(detail))
}

/// A Qobuz album Apple had nothing for is worth re-checking eventually — Apple
/// adds catalog editions — but not on every visit.
const QOBUZ_APPLE_MUSIC_MISS_RETRY_SECS: i64 = 14 * 24 * 60 * 60;

/// How many search hits are looked up in full before the best is chosen. Each
/// lookup is a helper round trip, and Apple orders search results well enough
/// that the match is in the first handful when it exists at all.
const QOBUZ_APPLE_MUSIC_CANDIDATE_LIMIT: usize = 6;

#[derive(Debug, Deserialize)]
struct QobuzAppleMusicVersionQuery {
    storefront: Option<String>,
}

#[derive(Debug, Serialize)]
struct QobuzAppleMusicVersionResponse {
    status: String,
    version: Option<QobuzAppleMusicVersion>,
    message: Option<String>,
}

/// The Apple Music row a standalone Qobuz album shows under Versions.
///
/// A linked local album stores its Apple edition in `album_versions` and serves
/// it as an [`AlbumVersionSummary`]; a Qobuz album with no local counterpart has
/// no album row to key one to, so the same fields are assembled straight from
/// the catalog copy with a catalog-derived string ID.
#[derive(Debug, Serialize)]
struct QobuzAppleMusicVersion {
    id: String,
    provider: &'static str,
    provider_id: String,
    source_label: &'static str,
    title: String,
    artist: Option<String>,
    year: Option<i32>,
    track_count: i64,
    format: Option<String>,
    sample_rate: Option<i64>,
    bit_depth: Option<i64>,
    image_url: Option<String>,
    storefront: Option<String>,
    audio_variants: Vec<String>,
    is_primary: bool,
}

/// Offer the Apple Music edition of a Qobuz album that no local album covers.
///
/// A Qobuz album linked to a local album already inherits that album's Apple
/// version through the local album's version list, so only the standalone case
/// reaches here. The answer is remembered per Qobuz album because finding it
/// costs a catalog search plus a lookup per candidate.
async fn qobuz_album_version(
    State(state): State<AppState>,
    Path(qobuz_album_id): Path<String>,
    Query(query): Query<QobuzAppleMusicVersionQuery>,
) -> Result<Json<QobuzAppleMusicVersionResponse>, (StatusCode, Json<AppleMusicMvpError>)> {
    let qobuz_album_id = qobuz_album_id.trim().to_string();
    if qobuz_album_id.is_empty() {
        return Ok(Json(no_match_response()));
    }

    let cached: Option<QobuzAppleMusicLink> = {
        let qobuz_album_id = qobuz_album_id.clone();
        state
            .library()
            .run_blocking(move |library| library.qobuz_apple_music_link(&qobuz_album_id))
            .await
            .map_err(library_api_error)?
    };
    if let Some(link) = cached
        && link.is_current(QOBUZ_APPLE_MUSIC_MISS_RETRY_SECS)
    {
        return Ok(Json(match link.apple_album {
            Some(album) => linked_response(&state, &album),
            None => no_match_response(),
        }));
    }

    let detail = match state.qobuz().album_detail(&qobuz_album_id).await {
        Ok(detail) => detail,
        Err(message) => return Ok(Json(unavailable_response(message))),
    };
    let artist = detail.album.artist.trim();
    let term = if artist.is_empty() {
        detail.album.title.clone()
    } else {
        format!("{artist} {}", detail.album.title)
    };
    let search = match state
        .apple_music()
        .search_songs(term, query.storefront.clone(), 10)
        .await
    {
        Ok(search) => search,
        Err(error) => return Ok(Json(unavailable_response(error.message))),
    };

    let mut best: Option<(AppleCatalogAlbum, QobuzAppleMusicMatch)> = None;
    let mut seen_album_ids = HashSet::new();
    for candidate in search
        .albums
        .into_iter()
        .take(QOBUZ_APPLE_MUSIC_CANDIDATE_LIMIT)
    {
        if !seen_album_ids.insert(candidate.album_id.clone()) {
            continue;
        }
        let Ok(album) = state
            .apple_music()
            .lookup_album(
                candidate.album_id,
                Some(candidate.storefront).filter(|value| !value.trim().is_empty()),
            )
            .await
        else {
            continue;
        };
        let assessment = apple_music_match_for_qobuz_album(&detail, &album);
        if best.as_ref().is_none_or(|(best_album, best_match)| {
            compare_qobuz_apple_candidates(&album, &assessment, best_album, best_match)
                == Ordering::Less
        }) {
            best = Some((album, assessment));
        }
    }

    let linked = best.filter(|(_, assessment)| assessment.safe_to_link);
    {
        let qobuz_album_id = qobuz_album_id.clone();
        let stored = linked
            .as_ref()
            .map(|(album, assessment)| (album.clone(), assessment.confidence));
        state
            .library()
            .run_blocking(move |library| {
                let (album, confidence) = match stored.as_ref() {
                    Some((album, confidence)) => (Some(album), *confidence),
                    None => (None, 0),
                };
                library.save_qobuz_apple_music_link(&qobuz_album_id, album, confidence)
            })
            .await
            .map_err(library_api_error)?;
    }
    Ok(Json(match linked {
        Some((album, _)) => linked_response(&state, &album),
        None => no_match_response(),
    }))
}

fn compare_qobuz_apple_candidates(
    left_album: &AppleCatalogAlbum,
    left: &QobuzAppleMusicMatch,
    right_album: &AppleCatalogAlbum,
    right: &QobuzAppleMusicMatch,
) -> Ordering {
    let preference = |album: &AppleCatalogAlbum, candidate: &QobuzAppleMusicMatch| {
        apple_match_preference_parts(
            candidate.safe_to_link,
            candidate.confidence,
            &candidate.evidence,
            album,
            candidate.paired_track_count,
        )
    };
    preference(right_album, right)
        .cmp(&preference(left_album, left))
        .then_with(|| left_album.album_id.cmp(&right_album.album_id))
}

fn linked_response(state: &AppState, album: &AppleCatalogAlbum) -> QobuzAppleMusicVersionResponse {
    QobuzAppleMusicVersionResponse {
        status: "linked".to_string(),
        version: Some(qobuz_apple_music_version(state, album)),
        message: None,
    }
}

fn no_match_response() -> QobuzAppleMusicVersionResponse {
    QobuzAppleMusicVersionResponse {
        status: "no_match".to_string(),
        version: None,
        message: None,
    }
}

fn unavailable_response(message: String) -> QobuzAppleMusicVersionResponse {
    QobuzAppleMusicVersionResponse {
        status: "unavailable".to_string(),
        version: None,
        message: Some(message),
    }
}

fn qobuz_apple_music_version(
    state: &AppState,
    album: &AppleCatalogAlbum,
) -> QobuzAppleMusicVersion {
    // Apple's catalog cannot report a track's real rate, so the row advertises
    // its variant tier until playback verifies Music.app's decoder — the same
    // rule a linked local album's Apple version follows.
    let verified = state
        .library()
        .apple_music_album_verified_format(&album.album_id)
        .ok()
        .flatten();
    let storefront = album.storefront.trim();
    let mut audio_variants = album
        .audio_variants
        .iter()
        .chain(album.tracks.iter().flat_map(|track| &track.audio_variants))
        .map(|variant| variant.trim().to_string())
        .filter(|variant| !variant.is_empty())
        .collect::<Vec<_>>();
    audio_variants.sort();
    audio_variants.dedup();
    QobuzAppleMusicVersion {
        id: format!(
            "apple_music:{}:{}",
            if storefront.is_empty() {
                "default"
            } else {
                storefront
            },
            album.album_id
        ),
        provider: "apple_music",
        provider_id: album.album_id.clone(),
        source_label: "Apple Music",
        title: album.title.clone(),
        artist: Some(album.artist.clone()),
        year: album
            .release_date
            .as_deref()
            .and_then(|date| date.get(..4))
            .and_then(|year| year.parse::<i32>().ok()),
        track_count: album.tracks.len() as i64,
        format: Some(
            verified
                .as_ref()
                .map(|format| format.codec.clone())
                .unwrap_or_else(|| "Apple Music".to_string()),
        ),
        sample_rate: verified.as_ref().map(|format| format.sample_rate),
        bit_depth: verified.as_ref().and_then(|format| format.bit_depth),
        image_url: album.artwork_url.clone(),
        storefront: (!storefront.is_empty()).then(|| storefront.to_string()),
        audio_variants,
        is_primary: false,
    }
}

async fn linked_library_album(
    State(state): State<AppState>,
    Path(apple_album_id): Path<String>,
) -> Result<Json<Option<AlbumDetail>>, (StatusCode, Json<AppleMusicMvpError>)> {
    state
        .library()
        .run_blocking(move |library| library.album_by_apple_music_id(&apple_album_id))
        .await
        .map_err(library_api_error)
        .map(Json)
}

async fn play(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AppleMusicPlayRequest>,
) -> AppleMusicApiResult {
    let startup_id = state.apple_music().begin_startup();
    let result = play_inner(&state, headers, request, &startup_id).await;
    state
        .apple_music()
        .complete_startup(&startup_id, result.is_ok());
    result
}

async fn play_inner(
    state: &AppState,
    headers: HeaderMap,
    request: AppleMusicPlayRequest,
    startup_id: &str,
) -> AppleMusicApiResult {
    let sequence = playback_request_sequence_from_headers(&headers);
    if !accept_playback_request_sequence(state, sequence.as_ref()) {
        return Err(playback_api_error(
            crate::playback::error::PlaybackError::conflict("Playback changed"),
        ));
    }
    let zone_id = request
        .zone_id
        .as_deref()
        .map(str::trim)
        .filter(|zone_id| !zone_id.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| state.zones().active_zone_id());
    let source = match request.source {
        Some(source) => resolve_scenario_source(state, source)
            .await
            .map_err(api_error)?,
        None => {
            let song_id = request.song_id.ok_or_else(|| {
                api_error(apple_music_error(
                    "apple_music_song_id_invalid",
                    "Choose a source or enter an Apple Music song ID.",
                    false,
                    "validating_request",
                    true,
                ))
            })?;
            state
                .apple_music()
                .lookup_song(song_id, request.storefront)
                .await
                .map_err(api_error)?
                .source_ref()
        }
    };
    let mut queue = Vec::with_capacity(request.queue.len());
    for queued in request.queue {
        queue.push(
            resolve_scenario_source(state, queued)
                .await
                .map_err(api_error)?,
        );
    }
    let profile_id = state.settings().active_profile_id();
    state
        .apple_music()
        .mark_startup_phase(startup_id, "request_resolution");
    PlaybackRouter::new(state)
        .execute(
            &zone_id,
            PlaybackIntent::Play {
                profile_id,
                source,
                queue,
                radio_auto: false,
                guard: PlaybackGuard::from_expected_sequence(sequence),
                qobuz_request: None,
                startup_id: Some(startup_id.to_string()),
            },
        )
        .await
        .map_err(playback_api_error)?;
    Ok(Json(state.apple_music().status()))
}

async fn resolve_scenario_source(
    state: &AppState,
    source: SourceRef,
) -> Result<SourceRef, AppleMusicMvpError> {
    match source {
        // Catalog search results already carry a canonical MusicKit source.
        // The helper validates and resolves each ID while preparing its queue,
        // so looking it up again here only doubles startup latency and adds
        // another network failure point.
        source @ SourceRef::AppleMusicTrack { .. } => Ok(source),
        source => source_ref_from_queue_request(state, &QueueRequestItem::Source(source))
            .map_err(|error| {
                apple_music_error(
                    "queue_source_invalid",
                    error.to_string(),
                    false,
                    "resolving_queue",
                    true,
                )
            })?
            .ok_or_else(|| {
                apple_music_error(
                    "queue_source_invalid",
                    "The queue item did not resolve to a playable source.",
                    false,
                    "resolving_queue",
                    true,
                )
            }),
    }
}

async fn shutdown(State(state): State<AppState>) -> AppleMusicApiResult {
    state
        .apple_music()
        .shutdown()
        .await
        .map(Json)
        .map_err(api_error)
}

fn apple_music_error(
    code: impl Into<String>,
    message: impl Into<String>,
    retryable: bool,
    stage: impl Into<String>,
    cleanup_complete: bool,
) -> AppleMusicMvpError {
    AppleMusicMvpError {
        code: code.into(),
        message: message.into(),
        retryable,
        stage: stage.into(),
        cleanup_complete,
    }
}

fn api_error(error: AppleMusicMvpError) -> (StatusCode, Json<AppleMusicMvpError>) {
    let status = match error.code.as_str() {
        "helper_missing" | "song_not_found" | "album_not_found" => StatusCode::NOT_FOUND,
        "music_authorization_not_determined"
        | "music_authorization_denied"
        | "subscription_required" => StatusCode::FORBIDDEN,
        "session_limit_reached" => StatusCode::CONFLICT,
        "helper_launch_failed"
        | "helper_exited"
        | "apple_music_helper_connect_timeout"
        | "musickit_capability_unavailable" => StatusCode::SERVICE_UNAVAILABLE,
        "catalog_search_failed" | "helper_protocol_mismatch" => StatusCode::BAD_GATEWAY,
        _ => StatusCode::BAD_REQUEST,
    };
    (status, Json(error))
}

fn library_api_error(message: String) -> (StatusCode, Json<AppleMusicMvpError>) {
    tracing::error!(
        event = "apple_music_album_version_persistence_failed",
        error = %message,
        "Apple Music album-version persistence failed"
    );
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(apple_music_error(
            "apple_music_version_persistence_failed",
            "Fozmo could not update the Apple Music album version.",
            true,
            "persisting_album_version",
            true,
        )),
    )
}

fn album_not_found() -> (StatusCode, Json<AppleMusicMvpError>) {
    (
        StatusCode::NOT_FOUND,
        Json(apple_music_error(
            "local_album_not_found",
            "The local Fozmo album was not found.",
            false,
            "resolving_local_album",
            true,
        )),
    )
}

fn playback_api_error(
    error: crate::playback::error::PlaybackError,
) -> (StatusCode, Json<AppleMusicMvpError>) {
    use crate::error::ErrorCategory;

    let status = match error.category() {
        ErrorCategory::Authentication => StatusCode::FORBIDDEN,
        ErrorCategory::NotFound => StatusCode::NOT_FOUND,
        ErrorCategory::Conflict => StatusCode::CONFLICT,
        ErrorCategory::Unavailable | ErrorCategory::RetryableNetwork => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        ErrorCategory::Validation => StatusCode::BAD_REQUEST,
        ErrorCategory::Persistence | ErrorCategory::InternalInvariant => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    (
        status,
        Json(apple_music_error(
            error.message(),
            error.message(),
            matches!(
                error.category(),
                ErrorCategory::Unavailable | ErrorCategory::RetryableNetwork
            ),
            "playback_router",
            true,
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::PlaybackHistoryInput;
    use crate::playback::test_support::app_state;
    use crate::settings::DEFAULT_PROFILE_ID;

    #[tokio::test]
    async fn catalog_album_reports_the_listener_s_own_play_history() {
        let state = app_state("apple-music-catalog-play-counts");
        for played_secs in [300.0, 280.0] {
            state
                .library()
                .record_playback_history(PlaybackHistoryInput {
                    profile_id: Some(DEFAULT_PROFILE_ID.to_string()),
                    source: SourceRef::AppleMusicTrack {
                        song_id: "1440857781".to_string(),
                        storefront: Some("nz".to_string()),
                        title: Some("Hyperballad".to_string()),
                        artist: Some("Björk".to_string()),
                        album: Some("Post".to_string()),
                        album_artist: Some("Björk".to_string()),
                        album_id: Some("1440857780".to_string()),
                        artwork_url: None,
                        duration_secs: Some(315.0),
                        track_number: Some(3),
                        disc_number: Some(1),
                        isrc: None,
                        radio: false,
                        radio_context: None,
                        playlist_context: None,
                    },
                    zone_id: "local-core".to_string(),
                    zone_name: "Local".to_string(),
                    played_secs: Some(played_secs),
                    duration_secs: Some(315.0),
                    completed: true,
                    counted: true,
                    radio: false,
                })
                .unwrap();
        }
        let album = AppleCatalogAlbum {
            album_id: "1440857780".to_string(),
            title: "Post".to_string(),
            artist: "Björk".to_string(),
            tracks: vec![
                AppleCatalogSong {
                    song_id: "1440857781".to_string(),
                    title: "Hyperballad".to_string(),
                    ..AppleCatalogSong::default()
                },
                AppleCatalogSong {
                    song_id: "1440857782".to_string(),
                    title: "The Modern Things".to_string(),
                    ..AppleCatalogSong::default()
                },
            ],
            ..AppleCatalogAlbum::default()
        };

        let enriched = with_playback_summaries(&state, DEFAULT_PROFILE_ID.to_string(), album).await;

        assert_eq!(enriched.tracks[0].play_count, 2);
        assert!((enriched.tracks[0].listened_secs - 580.0).abs() < f64::EPSILON);
        assert!(enriched.tracks[0].last_played_at.is_some());
        assert_eq!(
            enriched.tracks[1].play_count, 0,
            "an unplayed catalog track keeps a zero count"
        );
    }

    fn apple_match_candidate(
        album_id: &str,
        evidence: &[&str],
        safe_to_link: bool,
    ) -> AppleMusicAlbumMatchPreview {
        AppleMusicAlbumMatchPreview {
            local_album_id: 7,
            apple_album: AppleCatalogAlbum {
                album_id: album_id.to_string(),
                title: "Homogenic".to_string(),
                artist: "Björk".to_string(),
                audio_variants: vec!["lossless".to_string()],
                ..AppleCatalogAlbum::default()
            },
            confidence: 100,
            evidence: evidence.iter().map(|value| (*value).to_string()).collect(),
            pairings: Vec::new(),
            unmatched_local_track_ids: Vec::new(),
            unmatched_apple_song_ids: Vec::new(),
            safe_to_link,
            resulting_version: None,
        }
    }

    #[test]
    fn grouped_safe_editions_prefer_the_exact_base_album() {
        let mut candidates = vec![
            apple_match_candidate(
                "expanded",
                &["edition_compatible_title", "equal_track_count"],
                true,
            ),
            apple_match_candidate(
                "base",
                &["exact_normalized_title", "equal_track_count"],
                true,
            ),
        ];

        candidates.sort_by(compare_apple_match_candidates);

        assert_eq!(candidates[0].apple_album.album_id, "base");
    }

    #[test]
    fn grouped_safe_editions_use_the_album_id_as_a_stable_final_tiebreaker() {
        let mut candidates = vec![
            apple_match_candidate(
                "200",
                &["exact_normalized_title", "equal_track_count"],
                true,
            ),
            apple_match_candidate(
                "100",
                &["exact_normalized_title", "equal_track_count"],
                true,
            ),
        ];

        candidates.sort_by(compare_apple_match_candidates);

        assert_eq!(candidates[0].apple_album.album_id, "100");
    }

    #[test]
    fn unsafe_candidates_never_outrank_a_groupable_apple_edition() {
        let mut candidates = vec![
            apple_match_candidate(
                "unsafe-upc",
                &["upc_match", "exact_normalized_title", "equal_track_count"],
                false,
            ),
            apple_match_candidate(
                "safe",
                &["exact_normalized_title", "equal_track_count"],
                true,
            ),
        ];

        candidates.sort_by(compare_apple_match_candidates);

        assert_eq!(candidates[0].apple_album.album_id, "safe");
    }

    #[test]
    fn provider_failures_map_to_structured_http_statuses() {
        for (code, expected) in [
            ("album_not_found", StatusCode::NOT_FOUND),
            ("subscription_required", StatusCode::FORBIDDEN),
            (
                "musickit_capability_unavailable",
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            ("catalog_search_failed", StatusCode::BAD_GATEWAY),
            ("catalog_search_term_invalid", StatusCode::BAD_REQUEST),
            ("helper_protocol_mismatch", StatusCode::BAD_GATEWAY),
            ("apple_music_storefront_invalid", StatusCode::BAD_REQUEST),
        ] {
            let failure = apple_music_error(code, "message", false, "test", true);
            let (status, Json(body)) = api_error(failure);
            assert_eq!(status, expected, "{code}");
            assert_eq!(body.code, code);
            assert_eq!(body.stage, "test");
            assert!(body.cleanup_complete);
        }
    }
}
