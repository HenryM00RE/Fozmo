use super::playback_sequence::playback_request_sequence_from_headers;
use crate::app::state::AppState;
use crate::library::{
    AlbumDetail, AlbumVersionSummary, AppleMusicAlbumMatchPreview, AppleMusicVersionDetail,
};
use crate::playback::commands::accept_playback_request_sequence;
use crate::playback::intent::{PlaybackGuard, PlaybackIntent};
use crate::playback::queue::now_playing_queue_for_zone;
use crate::playback::resolver::{QueueRequestItem, source_ref_from_queue_request};
use crate::playback::router::PlaybackRouter;
use crate::playback::status::build_status_response_for_zone;
use crate::protocol::{SinkProtocol, SourceRef};
use crate::services::apple_music_musickit::{
    AppleCatalogAlbum, AppleCatalogSearchResult, AppleCatalogSong, AppleMusicAlbumVersionRequest,
    AppleMusicAuthorizeRequest, AppleMusicCaptureConfirmationRequest, AppleMusicCatalogQuery,
    AppleMusicCatalogSearchQuery, AppleMusicComparisonReferenceState,
    AppleMusicComparisonSwitchRequest, AppleMusicDevPlaySongRequest, AppleMusicMvpError,
    AppleMusicMvpStatus, AppleMusicPlayRequest, AppleMusicProcessTapStartRequest,
    AppleMusicTransportRequest, MusicAppSnapshot, music_app_status, pause_music_app,
    pause_music_app_and_status, play_music_app, set_music_app_position,
    set_music_app_position_and_play,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, collections::HashSet, time::Duration};

const APPLE_HANDOFF_PREFILL_SECS: f64 = 0.060;
const APPLE_HANDOFF_PREFILL_TIMEOUT: Duration = Duration::from_millis(750);
const APPLE_HANDOFF_MIN_CUSHION_MS: f64 = 180.0;
const APPLE_TAIL_SETTLE_TIMEOUT: Duration = Duration::from_millis(180);
const APPLE_TAIL_DRAIN_STABLE: Duration = Duration::from_millis(60);

type AppleMusicApiResult =
    Result<Json<AppleMusicMvpStatus>, (StatusCode, Json<AppleMusicMvpError>)>;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/apple-music/status", get(status))
        .route("/api/apple-music/launch", post(launch))
        .route("/api/apple-music/authorize", post(authorize))
        .route(
            "/api/apple-music/capture/confirm",
            post(confirm_system_audio_capture),
        )
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
        .route("/api/apple-music/dev/play-song", post(play_song))
        .route("/api/apple-music/transport", post(transport))
        .route("/api/apple-music/stop", post(stop))
        .route("/api/apple-music/shutdown", post(shutdown))
        .route(
            "/api/apple-music/process-tap/start",
            post(start_process_tap),
        )
        .route("/api/apple-music/process-tap/stop", post(stop_process_tap))
        .route(
            "/api/apple-music/comparison/switch",
            post(switch_comparison),
        )
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

async fn confirm_system_audio_capture(
    State(state): State<AppState>,
    Json(request): Json<AppleMusicCaptureConfirmationRequest>,
) -> AppleMusicApiResult {
    if !request.confirm_system_audio_capture {
        return Err(api_error(comparison_error(
            "process_tap_confirmation_required",
            "Confirm macOS system-audio capture before queueing Apple Music playback.",
            false,
            "permission",
            true,
        )));
    }
    state.apple_music().confirm_system_audio_capture(true);
    Ok(Json(state.apple_music().status()))
}

async fn play_song(
    State(state): State<AppState>,
    Json(request): Json<AppleMusicDevPlaySongRequest>,
) -> AppleMusicApiResult {
    state
        .apple_music()
        .play_song(request.song_id, request.storefront)
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
            api_error(comparison_error(
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
    state
        .apple_music()
        .confirm_system_audio_capture(request.confirm_system_audio_capture);
    let source = match request.source {
        Some(source) => resolve_scenario_source(&state, source)
            .await
            .map_err(api_error)?,
        None => {
            let song_id = request.song_id.ok_or_else(|| {
                api_error(comparison_error(
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
                comparison_error(
                    "queue_source_invalid",
                    error.to_string(),
                    false,
                    "resolving_queue",
                    true,
                )
            })?
            .ok_or_else(|| {
                comparison_error(
                    "queue_source_invalid",
                    "The queue item did not resolve to a playable source.",
                    false,
                    "resolving_queue",
                    true,
                )
            }),
    }
}

async fn transport(
    State(state): State<AppState>,
    Json(request): Json<AppleMusicTransportRequest>,
) -> AppleMusicApiResult {
    state
        .apple_music()
        .transport(&request.command)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn stop(State(state): State<AppState>) -> AppleMusicApiResult {
    state
        .apple_music()
        .transport("stop")
        .await
        .map(Json)
        .map_err(api_error)
}

async fn shutdown(State(state): State<AppState>) -> AppleMusicApiResult {
    state
        .apple_music()
        .transport("shutdown")
        .await
        .map(Json)
        .map_err(api_error)
}

async fn start_process_tap(
    State(state): State<AppState>,
    Json(request): Json<AppleMusicProcessTapStartRequest>,
) -> AppleMusicApiResult {
    state
        .apple_music()
        .start_process_tap(
            state.zones().active_player(),
            request.confirm_system_audio_capture,
            request.mute_original_audio,
        )
        .map(Json)
        .map_err(api_error)
}

async fn stop_process_tap(State(state): State<AppState>) -> AppleMusicApiResult {
    Ok(Json(state.apple_music().stop_process_tap()))
}

async fn switch_comparison(
    State(state): State<AppState>,
    Json(request): Json<AppleMusicComparisonSwitchRequest>,
) -> AppleMusicApiResult {
    let apple_music = state.apple_music().clone();
    let _switch_guard = apple_music.lock_comparison_switch().await;
    match request.target.trim().to_ascii_lowercase().as_str() {
        "apple_music" => {
            switch_to_apple_music(
                &state,
                request.confirm_system_audio_capture,
                request.match_position,
            )
            .await
        }
        "fozmo" => switch_to_fozmo(&state, request.match_position).await,
        _ => Err(comparison_error(
            "comparison_target_invalid",
            "Choose either Apple Music or Fozmo playback.",
            false,
            "validating_comparison",
            true,
        )),
    }
    .map(Json)
    .map_err(api_error)
}

async fn switch_to_apple_music(
    state: &AppState,
    confirm_system_audio_capture: bool,
    match_position: bool,
) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
    if state.apple_music().status().process_tap.state == "running" {
        return Ok(state.apple_music().status());
    }

    let zone_id = state.zones().active_zone_id();
    if state.zones().zone_protocol(&zone_id) != Some(SinkProtocol::LocalCoreAudio) {
        return Err(comparison_error(
            "comparison_local_zone_required",
            "Choose a local Core Audio output before starting the Apple Music comparison.",
            false,
            "capturing_fozmo_reference",
            true,
        ));
    }
    let player = state.zones().player_for_zone(&zone_id).ok_or_else(|| {
        comparison_error(
            "comparison_output_unavailable",
            "The selected local output is not currently available.",
            true,
            "capturing_fozmo_reference",
            true,
        )
    })?;
    let queue = now_playing_queue_for_zone(state, &zone_id).map_err(|error| {
        comparison_error(
            "comparison_reference_unavailable",
            error.message(),
            false,
            "capturing_fozmo_reference",
            true,
        )
    })?;
    let source = queue.current_source.ok_or_else(|| {
        comparison_error(
            "comparison_reference_missing",
            "Play the Qobuz or local version in Fozmo first, then switch to Apple Music.",
            false,
            "capturing_fozmo_reference",
            true,
        )
    })?;
    let playback = build_status_response_for_zone(state, &zone_id).map_err(|message| {
        comparison_error(
            "comparison_reference_unavailable",
            message,
            true,
            "capturing_fozmo_reference",
            true,
        )
    })?;
    let mut reference = AppleMusicComparisonReferenceState {
        zone_id: zone_id.clone(),
        zone_name: state.zones().zone_name(&zone_id),
        profile_id: state
            .listening()
            .profile_id(&zone_id)
            .unwrap_or_else(|| state.settings().active_profile_id()),
        source,
        queue: queue.queued_sources,
        position_secs: valid_position(playback.position_secs),
    };

    let music = music_app_snapshot().await?;
    if !music.running {
        return Err(comparison_error(
            "music_app_not_running",
            "Open the Music app and select the Apple Music version before switching.",
            false,
            "checking_music_app",
            true,
        ));
    }
    if !music.has_current_track() {
        return Err(comparison_error(
            "music_app_track_missing",
            "Select the matching track in the Music app before switching.",
            false,
            "checking_music_app",
            true,
        ));
    }

    let prepared_tap = state.apple_music().prepare_process_tap(
        player.clone(),
        confirm_system_audio_capture,
        true,
    )?;

    let handoff_boundary = match player
        .begin_seamless_handoff(prepared_tap.process_tap.sample_rate_hz)
        .await
    {
        Ok(boundary) if boundary.output_cushion_secs * 1_000.0 >= APPLE_HANDOFF_MIN_CUSHION_MS => {
            Some(boundary)
        }
        Ok(boundary) => {
            player.cancel_seamless_handoff(boundary.epoch);
            None
        }
        Err(_) => None,
    };
    let latest_reference_position = if let Some(boundary) = handoff_boundary {
        boundary.position_secs
    } else {
        build_status_response_for_zone(state, &zone_id)
            .map(|status| valid_position(status.position_secs))
            .unwrap_or(reference.position_secs)
    };
    reference.position_secs = latest_reference_position;
    let apple_position = if match_position {
        matched_position(latest_reference_position, music.track.duration_secs)
    } else {
        music.track.position_secs.unwrap_or(0.0)
    };

    if let Err(mut error) = state.apple_music().discard_process_tap_buffer() {
        error.cleanup_complete =
            abort_prepared_apple_handoff(state, &player, handoff_boundary).await;
        return Err(error);
    }
    if let Err(mut error) = run_music_action(
        move || set_music_app_position_and_play(apple_position),
        "starting_music_app",
    )
    .await
    {
        error.cleanup_complete =
            abort_prepared_apple_handoff(state, &player, handoff_boundary).await;
        return Err(error);
    }
    if let Err(mut error) = wait_for_apple_handoff_prefill(state).await {
        error.cleanup_complete =
            abort_prepared_apple_handoff(state, &player, handoff_boundary).await;
        return Err(error);
    }
    if let Err(mut error) = state.apple_music().prepare_process_tap_stream() {
        error.cleanup_complete =
            abort_prepared_apple_handoff(state, &player, handoff_boundary).await;
        return Err(error);
    }
    if let Err(mut error) = state
        .apple_music()
        .commit_process_tap(handoff_boundary.is_some())
    {
        error.cleanup_complete =
            abort_prepared_apple_handoff(state, &player, handoff_boundary).await;
        return Err(error);
    }

    let mut apple_track = music.track;
    apple_track.position_secs = Some(apple_position);
    state
        .apple_music()
        .comparison_switched_to_apple(reference, apple_track, match_position);
    Ok(state.apple_music().status())
}

async fn switch_to_fozmo(
    state: &AppState,
    match_position: bool,
) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
    let reference = state
        .apple_music()
        .comparison_reference()
        .ok_or_else(|| {
            comparison_error(
                "comparison_reference_missing",
                "Play the Qobuz or local version in Fozmo, then switch to Apple Music once so it can be remembered.",
                false,
                "loading_fozmo_reference",
                true,
            )
        })?;
    let music = music_app_snapshot().await?;
    if reference.source.qobuz_track_id().is_some() {
        return switch_to_prepared_qobuz(state, reference, music, match_position).await;
    }
    let fozmo_position = if match_position {
        music.track.position_secs.unwrap_or(reference.position_secs)
    } else {
        reference.position_secs
    };

    if music.running {
        run_music_action(pause_music_app, "pausing_music_app").await?;
    }
    state.apple_music().stop_process_tap();
    if let Err(error) = play_reference(state, &reference, fozmo_position).await {
        let rollback_complete = restore_apple_after_failed_switch(state, &reference, &music).await;
        return Err(comparison_error(
            "comparison_playback_failed",
            format!("Could not restore the Fozmo reference: {}", error.message()),
            true,
            "restoring_fozmo_reference",
            rollback_complete,
        ));
    }

    state.apple_music().comparison_switched_to_fozmo(
        Some(music.track),
        match_position,
        fozmo_position,
    );
    Ok(state.apple_music().status())
}

async fn switch_to_prepared_qobuz(
    state: &AppState,
    reference: AppleMusicComparisonReferenceState,
    music: MusicAppSnapshot,
    match_position: bool,
) -> Result<AppleMusicMvpStatus, AppleMusicMvpError> {
    let initial_position = if match_position {
        music.track.position_secs.unwrap_or(reference.position_secs)
    } else {
        reference.position_secs
    };
    let router = PlaybackRouter::new(state);
    let mut handoff = router
        .prepare_local_playback_handoff(
            &reference.zone_id,
            reference.profile_id.clone(),
            reference.source.clone(),
            reference.queue.clone(),
            reference.source.is_radio(),
            initial_position,
        )
        .await
        .map_err(|error| {
            comparison_error(
                "comparison_playback_failed",
                format!("Could not prepare the Fozmo reference: {}", error.message()),
                true,
                "preparing_fozmo_reference",
                true,
            )
        })?;

    let paused_music = if music.running {
        pause_music_and_snapshot().await?
    } else {
        music
    };
    let buffered_audio_secs = settle_apple_capture_tail(state).await?;
    let mut fozmo_position = if match_position {
        aligned_fozmo_position(
            paused_music.track.position_secs,
            buffered_audio_secs,
            reference.position_secs,
        )
    } else {
        reference.position_secs
    };

    handoff = match router
        .retarget_local_playback_handoff(handoff, fozmo_position)
        .await
    {
        Ok(handoff) => handoff,
        Err(error) => {
            let cleanup_complete = run_music_action(play_music_app, "restoring_music_app")
                .await
                .is_ok();
            return Err(comparison_error(
                "comparison_playback_failed",
                format!("Could not seek the Fozmo reference: {}", error.message()),
                true,
                "preparing_fozmo_reference",
                cleanup_complete,
            ));
        }
    };

    if match_position {
        let latest_buffered = state
            .apple_music()
            .process_tap_buffered_audio_secs()
            .unwrap_or(buffered_audio_secs);
        let latest_position = aligned_fozmo_position(
            paused_music.track.position_secs,
            latest_buffered,
            fozmo_position,
        );
        if (latest_position - fozmo_position).abs() >= 0.015 {
            handoff = match router
                .retarget_local_playback_handoff(handoff, latest_position)
                .await
            {
                Ok(handoff) => handoff,
                Err(error) => {
                    let cleanup_complete = run_music_action(play_music_app, "restoring_music_app")
                        .await
                        .is_ok();
                    return Err(comparison_error(
                        "comparison_playback_failed",
                        format!("Could not align the Fozmo reference: {}", error.message()),
                        true,
                        "preparing_fozmo_reference",
                        cleanup_complete,
                    ));
                }
            };
            fozmo_position = latest_position;
        }
    }

    if let Err(error) = router.commit_local_playback_handoff(handoff) {
        let cleanup_complete = run_music_action(play_music_app, "restoring_music_app")
            .await
            .is_ok();
        return Err(comparison_error(
            "comparison_playback_failed",
            format!("Could not start the Fozmo reference: {}", error.message()),
            true,
            "restoring_fozmo_reference",
            cleanup_complete,
        ));
    }

    state.apple_music().stop_process_tap();
    state.apple_music().comparison_switched_to_fozmo(
        Some(paused_music.track),
        match_position,
        fozmo_position,
    );
    Ok(state.apple_music().status())
}

async fn wait_for_apple_handoff_prefill(state: &AppState) -> Result<(), AppleMusicMvpError> {
    let deadline = tokio::time::Instant::now() + APPLE_HANDOFF_PREFILL_TIMEOUT;
    loop {
        let buffered = state.apple_music().process_tap_buffered_audio_secs()?;
        if buffered >= APPLE_HANDOFF_PREFILL_SECS {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return if buffered >= 0.010 {
                Ok(())
            } else {
                Err(comparison_error(
                    "process_tap_prefill_timeout",
                    "Music.app did not deliver enough audio for a continuous handoff.",
                    true,
                    "preparing_dsp_handoff",
                    false,
                ))
            };
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn settle_apple_capture_tail(state: &AppState) -> Result<f64, AppleMusicMvpError> {
    let deadline = tokio::time::Instant::now() + APPLE_TAIL_SETTLE_TIMEOUT;
    let mut drained_since = None;
    loop {
        let buffered = state.apple_music().process_tap_buffered_audio_secs()?;
        let now = tokio::time::Instant::now();
        if buffered <= 0.005 {
            let drained_at = drained_since.get_or_insert(now);
            if now.duration_since(*drained_at) >= APPLE_TAIL_DRAIN_STABLE {
                return Ok(buffered);
            }
        } else {
            drained_since = None;
        }
        if now >= deadline {
            return Ok(buffered);
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn pause_music_and_snapshot() -> Result<MusicAppSnapshot, AppleMusicMvpError> {
    tokio::task::spawn_blocking(pause_music_app_and_status)
        .await
        .map_err(|error| {
            comparison_error(
                "music_app_control_failed",
                format!("Music app pause task failed: {error}"),
                true,
                "pausing_music_app",
                false,
            )
        })?
        .map_err(|message| {
            comparison_error(
                "music_app_control_failed",
                message,
                true,
                "pausing_music_app",
                false,
            )
        })
}

async fn abort_prepared_apple_handoff(
    state: &AppState,
    player: &std::sync::Arc<crate::audio::player::Player>,
    boundary: Option<crate::audio::player::SeamlessHandoffBoundary>,
) -> bool {
    if let Some(boundary) = boundary {
        player.cancel_seamless_handoff(boundary.epoch);
    }
    let music_paused = run_music_action(pause_music_app, "pausing_music_app")
        .await
        .is_ok();
    state.apple_music().stop_process_tap();
    music_paused
}

async fn play_reference(
    state: &AppState,
    reference: &AppleMusicComparisonReferenceState,
    position_secs: f64,
) -> Result<(), crate::playback::error::PlaybackError> {
    PlaybackRouter::new(state)
        .execute(
            &reference.zone_id,
            PlaybackIntent::Play {
                profile_id: reference.profile_id.clone(),
                source: reference.source.clone(),
                queue: reference.queue.clone(),
                radio_auto: reference.source.is_radio(),
                guard: PlaybackGuard::none(),
                qobuz_request: None,
            },
        )
        .await?;
    if position_secs > 0.0 {
        PlaybackRouter::new(state)
            .execute(
                &reference.zone_id,
                PlaybackIntent::Seek {
                    seconds: position_secs,
                },
            )
            .await?;
    }
    Ok(())
}

async fn restore_apple_after_failed_switch(
    state: &AppState,
    reference: &AppleMusicComparisonReferenceState,
    music: &MusicAppSnapshot,
) -> bool {
    if !music.running || !music.has_current_track() {
        return false;
    }
    let Some(player) = state.zones().player_for_zone(&reference.zone_id) else {
        return false;
    };
    if state
        .apple_music()
        .start_process_tap(player, true, true)
        .is_err()
    {
        return false;
    }
    if let Some(position) = music.track.position_secs {
        let _ = set_music_position(position).await;
    }
    run_music_action(play_music_app, "restoring_music_app")
        .await
        .is_ok()
}

async fn music_app_snapshot() -> Result<MusicAppSnapshot, AppleMusicMvpError> {
    tokio::task::spawn_blocking(music_app_status)
        .await
        .map_err(|error| {
            comparison_error(
                "music_app_control_failed",
                format!("Music app status task failed: {error}"),
                true,
                "checking_music_app",
                true,
            )
        })?
        .map_err(|message| {
            comparison_error(
                "music_app_control_failed",
                message,
                true,
                "checking_music_app",
                true,
            )
        })
}

async fn set_music_position(seconds: f64) -> Result<(), AppleMusicMvpError> {
    run_music_action(
        move || set_music_app_position(seconds),
        "matching_music_position",
    )
    .await
}

async fn run_music_action<F>(action: F, stage: &'static str) -> Result<(), AppleMusicMvpError>
where
    F: FnOnce() -> Result<(), String> + Send + 'static,
{
    tokio::task::spawn_blocking(action)
        .await
        .map_err(|error| {
            comparison_error(
                "music_app_control_failed",
                format!("Music app control task failed: {error}"),
                true,
                stage,
                true,
            )
        })?
        .map_err(|message| comparison_error("music_app_control_failed", message, true, stage, true))
}

fn valid_position(position_secs: f64) -> f64 {
    if position_secs.is_finite() && position_secs >= 0.0 {
        position_secs
    } else {
        0.0
    }
}

fn aligned_fozmo_position(
    paused_music_position_secs: Option<f64>,
    unconsumed_capture_secs: f64,
    fallback_position_secs: f64,
) -> f64 {
    let buffered = if unconsumed_capture_secs.is_finite() {
        unconsumed_capture_secs.max(0.0)
    } else {
        0.0
    };
    paused_music_position_secs
        .filter(|position| position.is_finite() && *position >= 0.0)
        .map(|position| (position - buffered).max(0.0))
        .unwrap_or_else(|| valid_position(fallback_position_secs))
}

fn matched_position(position_secs: f64, duration_secs: Option<f64>) -> f64 {
    let position = valid_position(position_secs);
    duration_secs
        .filter(|duration| duration.is_finite() && *duration > 0.5)
        .map(|duration| position.min(duration - 0.25))
        .unwrap_or(position)
}

fn comparison_error(
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
        | "subscription_required"
        | "process_tap_confirmation_required" => StatusCode::FORBIDDEN,
        "session_limit_reached"
        | "process_tap_playback_changed"
        | "apple_music_active_segment_requires_reprepare"
        | "comparison_reference_missing"
        | "music_app_track_missing" => StatusCode::CONFLICT,
        "music_app_not_running" | "comparison_output_unavailable" => StatusCode::NOT_FOUND,
        "helper_launch_failed"
        | "helper_exited"
        | "apple_music_helper_connect_timeout"
        | "musickit_capability_unavailable"
        | "process_tap_format_unsupported"
        | "process_tap_prefill_timeout"
        | "process_tap_prepare_failed"
        | "process_tap_stalled"
        | "process_tap_start_failed"
        | "process_tap_unsupported" => StatusCode::SERVICE_UNAVAILABLE,
        "catalog_search_failed"
        | "comparison_playback_failed"
        | "helper_protocol_mismatch"
        | "music_app_control_failed" => StatusCode::BAD_GATEWAY,
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
        Json(comparison_error(
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
        Json(comparison_error(
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
        Json(comparison_error(
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
    fn matched_position_clamps_to_the_apple_track_duration() {
        assert_eq!(matched_position(90.0, Some(80.0)), 79.75);
        assert_eq!(matched_position(45.0, Some(80.0)), 45.0);
    }

    #[test]
    fn invalid_timeline_values_never_reach_a_transport() {
        assert_eq!(valid_position(f64::NAN), 0.0);
        assert_eq!(valid_position(f64::INFINITY), 0.0);
        assert_eq!(valid_position(-1.0), 0.0);
    }

    #[test]
    fn fozmo_handoff_accounts_for_the_unconsumed_capture_tail() {
        assert!((aligned_fozmo_position(Some(90.0), 0.040, 12.0) - 89.960).abs() < 1e-9);
        assert_eq!(aligned_fozmo_position(Some(0.020), 0.050, 12.0), 0.0);
        assert_eq!(aligned_fozmo_position(None, 0.050, 12.0), 12.0);
    }

    #[test]
    fn missing_reference_is_reported_as_a_conflict() {
        let failure = comparison_error(
            "comparison_reference_missing",
            "missing",
            false,
            "test",
            true,
        );

        assert_eq!(api_error(failure).0, StatusCode::CONFLICT);
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
            let failure = comparison_error(code, "message", false, "test", true);
            let (status, Json(body)) = api_error(failure);
            assert_eq!(status, expected, "{code}");
            assert_eq!(body.code, code);
            assert_eq!(body.stage, "test");
            assert!(body.cleanup_complete);
        }
    }
}
