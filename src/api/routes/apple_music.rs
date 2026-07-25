use super::playback_sequence::playback_request_sequence_from_headers;
use crate::app::state::AppState;
use crate::library::{
    AlbumDetail, AlbumVersionSummary, AppleMusicAlbumMatchPreview, AppleMusicVersionDetail,
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
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
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
    Path(album_id): Path<String>,
    Query(query): Query<AppleMusicCatalogQuery>,
) -> Result<Json<AppleCatalogAlbum>, (StatusCode, Json<AppleMusicMvpError>)> {
    state
        .apple_music()
        .lookup_album(album_id, query.storefront)
        .await
        .map(Json)
        .map_err(api_error)
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
    let has_evidence = |expected: &str| {
        candidate
            .evidence
            .iter()
            .any(|evidence| evidence == expected)
    };
    let advertises_lossless = candidate.apple_album.audio_variants.iter().any(|variant| {
        let normalized = variant
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>();
        normalized == "lossless" || normalized == "highresolutionlossless"
    });
    (
        candidate.safe_to_link,
        candidate.confidence,
        has_evidence("upc_match"),
        has_evidence("exact_normalized_title"),
        has_evidence("equal_track_count"),
        advertises_lossless,
        candidate.pairings.len(),
    )
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
    Path((local_album_id, version_id)): Path<(i64, i64)>,
) -> Result<Json<AppleMusicVersionDetail>, (StatusCode, Json<AppleMusicMvpError>)> {
    state
        .library()
        .run_blocking(move |library| library.apple_music_version_detail(local_album_id, version_id))
        .await
        .map_err(library_api_error)?
        .map(Json)
        .ok_or_else(|| {
            api_error(apple_music_error(
                "apple_music_version_not_found",
                "The linked Apple Music album version was not found.",
                false,
                "resolving_album_version",
                true,
            ))
        })
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
    let sequence = playback_request_sequence_from_headers(&headers);
    if !accept_playback_request_sequence(&state, sequence.as_ref()) {
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
        Some(source) => resolve_scenario_source(&state, source)
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
            resolve_scenario_source(&state, queued)
                .await
                .map_err(api_error)?,
        );
    }
    let profile_id = state.settings().active_profile_id();
    PlaybackRouter::new(&state)
        .execute(
            &zone_id,
            PlaybackIntent::Play {
                profile_id,
                source,
                queue,
                radio_auto: false,
                guard: PlaybackGuard::from_expected_sequence(sequence),
                qobuz_request: None,
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
