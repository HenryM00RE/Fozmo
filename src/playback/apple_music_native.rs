//! Product Apple Music playback on macOS.
//!
//! MusicKit remains the catalog/authorization plane. The audio plane is the
//! native Music.app lossless decoder routed through Fozmo Capture, then through
//! the normal local Player/DSP/output selected for the zone.

use crate::app::state::AppState;
use crate::audio::player::Player;
use crate::playback::error::PlaybackError;
use crate::playback::intent::{PlaybackGuard, PlaybackIntent, PlaybackOutcome};
use crate::playback::qobuz::qobuz_stream_queue_item_for_request;
use crate::playback::resolver::local_player_queue_items_from_sources;
use crate::playback::router::PlaybackRouter;
use crate::playback::service::{
    apply_playback_settings_for_zone, prepare_airplay_volume_for_zone, prepare_hegel_for_zone,
};
use crate::playback::source::qobuz_play_request_from_source_ref;
use crate::protocol::SourceRef;
use crate::services::apple_music::{APPLE_MUSIC_LIVE_DISPLAY_NAME, AppleMusicPlaybackSnapshot};
use crate::services::apple_music_musickit::{
    AppleMusicMvpError, MusicAppSnapshot, activate_catalog_track, music_app_status,
    pause_music_app, play_music_app, play_music_app_current_in_context,
    play_music_app_current_once, prepare_music_app, set_music_app_position,
};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tracing::{debug, info, warn};

const TRACK_ACTIVATION_TIMEOUT: Duration = Duration::from_secs(10);
const TRACK_ACTIVATION_POLL: Duration = Duration::from_millis(75);
const TRACK_TRANSPORT_SETTLE_TIMEOUT: Duration = Duration::from_secs(3);
const TRACK_START_POSITION_TOLERANCE_SECS: f64 = 1.0;
const TRACK_PARK_POSITION_TOLERANCE_SECS: f64 = 0.050;
const START_PREFILL_TARGET_SECS: f64 = 0.500;
const START_PREFILL_MIN_SECS: f64 = 0.250;
const START_PREFILL_TIMEOUT: Duration = Duration::from_millis(2_500);
const RESUME_PREFILL_TARGET_SECS: f64 = 0.080;
const RESUME_PREFILL_MIN_SECS: f64 = 0.030;
const RESUME_PREFILL_TIMEOUT: Duration = Duration::from_millis(1_000);
const MONITOR_INTERVAL: Duration = Duration::from_millis(350);
const TERMINAL_STATE_STARTUP_GRACE: Duration = Duration::from_secs(1);
const TERMINAL_STATE_CONFIRMATIONS: u8 = 3;
const STARTUP_TRANSPORT_STATUS_POLL: Duration = Duration::from_millis(350);
const STARTUP_TRANSPORT_RECOVERY_TIMEOUT: Duration = Duration::from_secs(3);
const STARTUP_STALL_POSITION_MAX_SECS: f64 = 1.0;
const COMPLETION_TAIL_SECS: f64 = 4.0;
const PLAYER_PAUSE_TIMEOUT: Duration = Duration::from_secs(4);
const PLAYER_OUTPUT_START_TIMEOUT: Duration = Duration::from_secs(25);
const PLAYER_EOF_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const PLAYER_AUTO_ADVANCE_START_GRACE: Duration = Duration::from_millis(750);

#[allow(clippy::too_many_arguments)]
pub(crate) async fn play_apple_music_source(
    state: &AppState,
    zone_id: &str,
    profile_id: String,
    source: SourceRef,
    queue: Vec<SourceRef>,
    radio_auto: bool,
    guard: PlaybackGuard,
) -> Result<PlaybackOutcome, PlaybackError> {
    let source = hydrate_catalog_source(state, source).await?;
    let (song_id, storefront, album_id) = catalog_identity(&source)?;
    if !guard.is_current(state) {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    let player = native_local_player(state, zone_id).ok_or(PlaybackError::ZoneNotAvailable)?;

    let _playback_switch = state.apple_music().lock_playback_switch().await;
    ensure_guard_current(state, &guard)?;
    pause_music_blocking().await?;
    if let Some(previous) = state.apple_music_playback().playback_snapshot() {
        clear_prefetched_player_queue(state, &previous);
    }
    if state.apple_music_playback().capture_running() {
        state.apple_music_playback().stop_runtime(false);
    }
    ensure_guard_current(state, &guard)?;

    apply_playback_settings_for_zone(state, zone_id);
    prepare_airplay_volume_for_zone(state, zone_id, &player);
    prepare_hegel_for_zone(state, zone_id).await?;
    prepare_music_blocking().await?;

    let settings = state.settings().apple_music_playback_settings();
    let initial_epoch = state
        .apple_music_playback()
        .start_playback_capture(player.clone(), &settings)
        .map_err(PlaybackError::integration)?;
    let playback = state.apple_music_playback().activate_playback(
        zone_id.to_string(),
        initial_epoch,
        source.clone(),
    );
    let use_contextual_transport = queue
        .first()
        .is_some_and(|next| native_apple_music_gapless_pair(&source, next));

    let start_result = async {
        hold_player_paused(&player, initial_epoch).await?;
        ensure_owned(state, &guard, &playback)?;
        let format_boundary = SystemTime::now();
        let activation_storefront = storefront.clone();
        let activation_album_id = album_id.clone();
        let activation_song_id = song_id.clone();
        tokio::task::spawn_blocking(move || {
            activate_catalog_track(
                &activation_storefront,
                &activation_album_id,
                &activation_song_id,
            )
        })
        .await
        .map_err(|error| {
            PlaybackError::internal_invariant(format!(
                "Music.app catalog activation task stopped: {error}"
            ))
        })?
        .map_err(PlaybackError::integration)?;

        let selection = async {
            let selected = wait_for_selected_track(state, &guard, &playback).await?;
            // Music.app can carry its global player position across an
            // Accessibility-selected catalog row. Reset only after the
            // requested track is current so a shorter track cannot inherit a
            // position at or beyond its EOF.
            set_music_position_blocking(0.0).await?;
            Ok::<MusicAppSnapshot, PlaybackError>(selected)
        };
        // Music must still be decoding while we inspect its fresh decoder-log
        // event. Pausing here can make the app briefly disappear from Core
        // Audio and can prevent the ALAC event from being emitted at all. The
        // local Player remains paused, so these probe samples stay private;
        // the verified-rate restart below destroys them before playback is
        // restarted from zero.
        //
        // Music.app starts decoding as soon as the activation click lands, so
        // the Unified Log query runs alongside the selection poll rather than
        // after it. `log show` costs the best part of a second, and both halves
        // still measure against the same pre-activation boundary, so the probe
        // cannot accept a record left over from the previous track.
        let apple_music = state.apple_music();
        let (selected, source_format) = tokio::join!(
            selection,
            apple_music.probe_music_app_source_format(format_boundary)
        );
        let selected = selected?;
        let source_format = source_format
            .map_err(playback_error)?
            .ok_or_else(|| {
                PlaybackError::integration(
                    "Music.app did not expose a fresh Apple Lossless decoder format. Fozmo kept the Hegel muted and did not release unverified audio to the DSP.",
                )
            })?;
        pause_music_blocking().await?;
        ensure_owned(state, &guard, &playback)?;
        let source_rate_hz = source_format.sample_rate_hz;
        let source_bits = source_format.source_bit_depth_bits;

        // Establish Music.app's single-track transport before creating the
        // capture session that Fozmo will actually play. Starting a catalog
        // track is asynchronous: setting player position to zero in the same
        // AppleScript call is not enough, because the transport can restore
        // its previous global position a moment later.
        if use_contextual_transport {
            play_current_in_context_blocking().await?;
        } else {
            play_current_once_blocking().await?;
        }
        wait_for_selected_track_transport(
            &source,
            "playing",
            None,
            "start its single-track transport",
        )
        .await?;
        ensure_owned(state, &guard, &playback)?;
        pause_music_blocking().await?;
        ensure_owned(state, &guard, &playback)?;
        set_music_position_blocking(0.0).await?;
        wait_for_selected_track_transport(
            &source,
            "paused",
            Some(TRACK_PARK_POSITION_TOLERANCE_SECS),
            "park at 0:00",
        )
        .await?;
        ensure_owned(state, &guard, &playback)?;

        // Recreate the capture ring only after Music.app is confirmed paused
        // at zero. This guarantees that PCM from its former seek position
        // cannot survive into the new Fozmo timeline.
        let verified_epoch = state
            .apple_music_playback()
            .restart_at_verified_source_format(source_rate_hz, source_bits)
            .map_err(PlaybackError::integration)?;
        if !state
            .apple_music_playback()
            .replace_player_epoch(playback.generation, verified_epoch)
        {
            return Err(PlaybackError::conflict("Playback changed"));
        }
        hold_player_paused(&player, verified_epoch).await?;
        player.flush_live_output();
        ensure_owned(state, &guard, &playback)?;

        play_music_blocking().await?;
        wait_for_selected_track_transport(
            &source,
            "playing",
            Some(TRACK_START_POSITION_TOLERANCE_SECS),
            "resume from 0:00",
        )
        .await?;
        ensure_owned(state, &guard, &playback)?;
        wait_for_prefill(
            state,
            &guard,
            &playback,
            START_PREFILL_TARGET_SECS,
            START_PREFILL_MIN_SECS,
            START_PREFILL_TIMEOUT,
        )
        .await?;
        ensure_selected_track_still_playing(&source).await?;
        ensure_owned(state, &guard, &playback)?;
        prepare_hegel_for_zone(state, zone_id).await?;
        player.resume();
        wait_for_player_output_ready(state, &guard, &playback, &player, verified_epoch).await?;
        state.apple_music_playback().update_playback(
            playback.generation,
            "playing",
            Some(0.0),
            selected.track.duration_secs,
        );
        info!(
            event = "apple_music_native_lossless_started",
            zone_id,
            song_id,
            album_id,
            source_rate_hz,
            source_bits = source_bits.unwrap_or_default(),
            player_epoch = verified_epoch,
            "Music.app lossless capture is feeding the selected local DSP/output"
        );
        Ok::<(u32, Option<u32>), PlaybackError>((source_rate_hz, source_bits))
    }
    .await;

    let (verified_rate_hz, verified_bits) = match start_result {
        Ok(format) => format,
        Err(error) => {
            cleanup_failed_start(state, playback.generation, &player).await;
            return Err(error);
        }
    };

    // Apple's catalog only advertises "lossless" or "hi-res lossless", so this
    // verified decoder format is the sole source of an exact Apple Music rate
    // and depth. The strict start path rejects every non-ALAC decoder, so a
    // format that reaches here is always Apple Lossless.
    if let Err(error) = state.library().record_apple_music_track_format(
        &song_id,
        Some(&album_id),
        Some(&storefront),
        "ALAC",
        verified_rate_hz,
        verified_bits,
    ) {
        warn!(
            event = "apple_music_verified_format_not_recorded",
            zone_id, song_id, error, "Could not remember the verified Apple Music decoder format"
        );
    }

    state
        .library()
        .set_zone_queue(zone_id, &queue)
        .map_err(PlaybackError::library)?;
    state.listening().start_with_radio(
        state.library(),
        zone_id.to_string(),
        state.zones().zone_name(zone_id),
        profile_id,
        source,
        queue,
        radio_auto,
    );
    spawn_music_app_monitor(state.clone(), zone_id.to_string(), playback.generation);
    spawn_native_next_prefetch(state.clone(), zone_id.to_string());
    Ok(PlaybackOutcome::Completed)
}

pub(crate) fn active_snapshot(
    state: &AppState,
    zone_id: &str,
) -> Option<AppleMusicPlaybackSnapshot> {
    let snapshot = state
        .apple_music_playback()
        .playback_snapshot_for_zone(zone_id)?;
    let player = native_local_player(state, zone_id)?;
    (player.playback_epoch() == snapshot.player_epoch).then_some(snapshot)
}

/// Refresh the Player-owned item directly behind the native live capture.
/// Local files and already-open Qobuz streams can then begin at live EOF
/// without tearing down the DSP/output. Apple entries stay out of the engine
/// queue: sequential tracks from the same catalog album advance inside
/// Music.app's continuous transport, while other Apple boundaries are routed
/// through the normal verified start path.
pub(crate) fn spawn_native_next_prefetch(state: AppState, zone_id: String) {
    let Some(snapshot) = active_snapshot(&state, &zone_id) else {
        return;
    };
    let Some(revision) = state
        .apple_music_playback()
        .reserve_prefetch(snapshot.generation)
    else {
        return;
    };
    tokio::spawn(async move {
        if let Err(error) =
            arm_native_next_player_queue(&state, &zone_id, &snapshot, revision).await
        {
            debug!(
                event = "apple_music_native_next_prefetch_skipped",
                zone_id,
                generation = snapshot.generation,
                error,
                "Could not pre-arm the next mixed-provider queue entry"
            );
        }
    });
}

async fn arm_native_next_player_queue(
    state: &AppState,
    zone_id: &str,
    snapshot: &AppleMusicPlaybackSnapshot,
    revision: u64,
) -> Result<(), String> {
    let player =
        native_local_player(state, zone_id).ok_or_else(|| "Zone not available".to_string())?;
    if player.playback_epoch() != snapshot.player_epoch {
        return Err("Playback changed".to_string());
    }
    let upcoming = zone_queue_sources(state, zone_id);
    let Some(next) = upcoming.first().cloned() else {
        if native_prefetch_is_current(state, zone_id, snapshot, revision, None) {
            player.set_queue_if_epoch(Vec::new(), Some(snapshot.player_epoch));
        }
        return Ok(());
    };
    let next_key = next.key();
    match &next {
        SourceRef::LocalTrack { .. } => {
            // Load the entire contiguous local prefix. Player can then retain
            // the same output and DSP graph for Apple -> local -> local.
            let queue = local_player_queue_items_from_sources(state, &upcoming);
            if queue.is_empty() {
                return Err(format!("Could not resolve queued local source {next_key}"));
            }
            if native_prefetch_is_current(state, zone_id, snapshot, revision, Some(&next_key)) {
                player.set_queue_if_epoch(queue, Some(snapshot.player_epoch));
                info!(
                    event = "apple_music_native_next_prefetched",
                    zone_id,
                    next_source_key = next_key,
                    provider = "local",
                    "Armed a gapless Player handoff after native Apple Music"
                );
            }
        }
        SourceRef::QobuzTrack { .. } => {
            let request = qobuz_play_request_from_source_ref(&next, &[], next.is_radio())
                .ok_or_else(|| format!("Queued Qobuz source {next_key} was not playable"))?;
            let item = qobuz_stream_queue_item_for_request(state, &request).await?;
            if native_prefetch_is_current(state, zone_id, snapshot, revision, Some(&next_key)) {
                player.set_stream_queue_if_epoch(
                    vec![item],
                    player.current_file_name(),
                    Some(snapshot.player_epoch),
                );
                info!(
                    event = "apple_music_native_next_prefetched",
                    zone_id,
                    next_source_key = next_key,
                    provider = "qobuz",
                    "Armed a gapless Player handoff after native Apple Music"
                );
            }
        }
        SourceRef::AppleMusicTrack { .. } => {
            if native_prefetch_is_current(state, zone_id, snapshot, revision, Some(&next_key)) {
                player.set_queue_if_epoch(Vec::new(), Some(snapshot.player_epoch));
            }
        }
    }
    Ok(())
}

fn native_prefetch_is_current(
    state: &AppState,
    zone_id: &str,
    snapshot: &AppleMusicPlaybackSnapshot,
    revision: u64,
    expected_next_key: Option<&str>,
) -> bool {
    state
        .apple_music_playback()
        .prefetch_is_current(snapshot.generation, revision)
        && active_snapshot(state, zone_id)
            .is_some_and(|active| active.player_epoch == snapshot.player_epoch)
        && expected_next_key.is_none_or(|expected| {
            zone_queue_sources(state, zone_id)
                .first()
                .is_some_and(|source| source.key() == expected)
        })
}

fn clear_prefetched_player_queue(state: &AppState, snapshot: &AppleMusicPlaybackSnapshot) {
    if let Some(player) = native_local_player(state, &snapshot.zone_id) {
        player.set_queue_if_epoch(Vec::new(), Some(snapshot.player_epoch));
    }
}

pub(crate) async fn stop_replaced_session_if_current(
    state: &AppState,
    zone_id: &str,
    guard: &PlaybackGuard,
) -> Result<(), PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    ensure_guard_current(state, guard)?;
    if let Some(snapshot) = active_snapshot(state, zone_id) {
        // A natural EOF may have a Local/Qobuz item pre-armed in Player. An
        // explicit provider switch owns the boundary instead, so remove that
        // item before closing the live producer.
        clear_prefetched_player_queue(state, &snapshot);
        let _ = pause_music_blocking().await;
        state.apple_music_playback().stop_runtime(false);
    } else {
        // This command may have replaced the Apple Music successor that a
        // finished track retained the capture route for.
        state.apple_music_playback().release_retained_route();
    }
    Ok(())
}

pub(crate) async fn pause(state: &AppState, zone_id: &str) -> Result<bool, PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    let Some(snapshot) = active_snapshot(state, zone_id) else {
        return Ok(false);
    };
    pause_music_blocking().await?;
    let player = native_local_player(state, zone_id).ok_or(PlaybackError::ZoneNotAvailable)?;
    player.pause();
    state
        .apple_music_playback()
        .update_playback(snapshot.generation, "paused", None, None);
    Ok(true)
}

pub(crate) async fn resume(state: &AppState, zone_id: &str) -> Result<bool, PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    let Some(snapshot) = active_snapshot(state, zone_id) else {
        return Ok(false);
    };
    let player = native_local_player(state, zone_id).ok_or(PlaybackError::ZoneNotAvailable)?;
    play_music_blocking().await?;
    wait_for_prefill_without_guard(
        state,
        &snapshot,
        RESUME_PREFILL_TARGET_SECS,
        RESUME_PREFILL_MIN_SECS,
        RESUME_PREFILL_TIMEOUT,
    )
    .await?;
    prepare_hegel_for_zone(state, zone_id).await?;
    player.resume();
    state
        .apple_music_playback()
        .update_playback(snapshot.generation, "playing", None, None);
    Ok(true)
}

pub(crate) async fn seek(
    state: &AppState,
    zone_id: &str,
    seconds: f64,
) -> Result<bool, PlaybackError> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(PlaybackError::bad_request(
            "Apple Music seek position must be non-negative.",
        ));
    }
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    let Some(snapshot) = active_snapshot(state, zone_id) else {
        return Ok(false);
    };
    let player = native_local_player(state, zone_id).ok_or(PlaybackError::ZoneNotAvailable)?;
    pause_music_blocking().await?;
    player.pause();
    state.apple_music_playback().update_playback(
        snapshot.generation,
        "paused",
        Some(seconds),
        None,
    );
    let replacement_epoch = state
        .apple_music_playback()
        .restart_current_managed_session()
        .map_err(PlaybackError::integration)?;
    if !state
        .apple_music_playback()
        .set_timeline_origin(snapshot.generation, seconds)
    {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    hold_player_paused(&player, replacement_epoch).await?;
    set_music_position_blocking(seconds).await?;
    play_music_blocking().await?;
    wait_for_prefill_without_guard(
        state,
        &snapshot,
        RESUME_PREFILL_TARGET_SECS,
        RESUME_PREFILL_MIN_SECS,
        RESUME_PREFILL_TIMEOUT,
    )
    .await?;
    prepare_hegel_for_zone(state, zone_id).await?;
    player.resume();
    state.apple_music_playback().update_playback(
        snapshot.generation,
        "playing",
        Some(seconds),
        None,
    );
    spawn_native_next_prefetch(state.clone(), zone_id.to_string());
    Ok(true)
}

pub(crate) async fn stop(state: &AppState, zone_id: &str) -> Result<bool, PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    if active_snapshot(state, zone_id).is_none() {
        return Ok(false);
    }
    let _ = pause_music_blocking().await;
    state.apple_music_playback().stop_runtime(true);
    Ok(true)
}

pub(crate) async fn next(state: &AppState, zone_id: &str) -> Result<bool, PlaybackError> {
    let playback_switch = state.apple_music().lock_playback_switch().await;
    let Some(snapshot) = active_snapshot(state, zone_id) else {
        return Ok(false);
    };
    let queue = zone_queue_sources(state, zone_id);
    let profile_id = state
        .listening()
        .profile_id(zone_id)
        .unwrap_or_else(|| crate::settings::DEFAULT_PROFILE_ID.to_string());
    clear_prefetched_player_queue(state, &snapshot);
    let _ = pause_music_blocking().await;
    let detached_epoch = if retains_capture_route(true, &queue) {
        state.apple_music_playback().stop_playback_retaining_route()
    } else {
        state.apple_music_playback().stop_runtime(false)
    }
    .unwrap_or(snapshot.player_epoch);
    let Some((next, rest)) = queue.split_first() else {
        if let Some(player) = native_local_player(state, zone_id) {
            player.stop();
        }
        state.listening().stop(state.library(), zone_id);
        return Ok(true);
    };
    let next = next.clone();
    let rest = rest.to_vec();
    drop(playback_switch);
    route_after_native_boundary(
        state.clone(),
        zone_id.to_string(),
        profile_id,
        next,
        rest,
        detached_epoch,
        "manual_next",
    )
    .await;
    Ok(true)
}

async fn hydrate_catalog_source(
    state: &AppState,
    source: SourceRef,
) -> Result<SourceRef, PlaybackError> {
    let SourceRef::AppleMusicTrack {
        song_id,
        storefront,
        album_id,
        title,
        ..
    } = &source
    else {
        return Err(PlaybackError::bad_request("Expected an Apple Music source"));
    };
    if !song_id.trim().is_empty()
        && album_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && title
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(source);
    }
    state
        .apple_music()
        .lookup_song(song_id.clone(), storefront.clone())
        .await
        .map(|song| song.source_ref())
        .map_err(playback_error)
}

fn catalog_identity(source: &SourceRef) -> Result<(String, String, String), PlaybackError> {
    let SourceRef::AppleMusicTrack {
        song_id,
        storefront,
        album_id,
        ..
    } = source
    else {
        return Err(PlaybackError::bad_request("Expected an Apple Music source"));
    };
    let normalize = |value: Option<&str>, label: &str| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                PlaybackError::bad_request(format!(
                    "Apple Music {label} is required for native lossless playback."
                ))
            })
    };
    Ok((
        normalize(Some(song_id), "song ID")?,
        normalize(storefront.as_deref(), "storefront")?,
        normalize(album_id.as_deref(), "album ID")?,
    ))
}

async fn wait_for_selected_track(
    state: &AppState,
    guard: &PlaybackGuard,
    playback: &AppleMusicPlaybackSnapshot,
) -> Result<MusicAppSnapshot, PlaybackError> {
    let deadline = tokio::time::Instant::now() + TRACK_ACTIVATION_TIMEOUT;
    let mut last_track = None;
    loop {
        ensure_owned(state, guard, playback)?;
        let snapshot = music_status_blocking().await?;
        if snapshot.has_current_track() {
            last_track = snapshot.track.title.clone();
            if music_track_matches_source(&snapshot, &playback.source) {
                return Ok(snapshot);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(PlaybackError::integration(format!(
                "Music.app did not select the requested catalog track{}.",
                last_track
                    .as_deref()
                    .map(|title| format!("; it remained on “{title}”"))
                    .unwrap_or_default()
            )));
        }
        tokio::time::sleep(TRACK_ACTIVATION_POLL).await;
    }
}

async fn ensure_selected_track_still_playing(source: &SourceRef) -> Result<(), PlaybackError> {
    let snapshot = music_status_blocking().await?;
    if !music_track_matches_source(&snapshot, source)
        || snapshot.player_state.as_deref() != Some("playing")
    {
        return Err(PlaybackError::integration(
            "Music.app did not remain on the selected track while Fozmo prebuffered it.",
        ));
    }
    Ok(())
}

async fn wait_for_selected_track_transport(
    source: &SourceRef,
    expected_state: &str,
    maximum_position_secs: Option<f64>,
    operation: &str,
) -> Result<MusicAppSnapshot, PlaybackError> {
    let deadline = tokio::time::Instant::now() + TRACK_TRANSPORT_SETTLE_TIMEOUT;
    loop {
        let snapshot = music_status_blocking().await?;
        let position_matches = maximum_position_secs
            .is_none_or(|maximum| track_position_is_at_start(&snapshot, maximum));
        if music_track_matches_source(&snapshot, source)
            && snapshot.player_state.as_deref() == Some(expected_state)
            && position_matches
        {
            return Ok(snapshot);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(PlaybackError::integration(format!(
                "Music.app did not {operation} (state={}, position={}).",
                snapshot.player_state.as_deref().unwrap_or("unknown"),
                snapshot
                    .track
                    .position_secs
                    .map(|position| format!("{position:.3}s"))
                    .unwrap_or_else(|| "unknown".to_string())
            )));
        }
        tokio::time::sleep(TRACK_ACTIVATION_POLL).await;
    }
}

fn track_position_is_at_start(snapshot: &MusicAppSnapshot, maximum_position_secs: f64) -> bool {
    snapshot
        .track
        .position_secs
        .is_some_and(|position| position <= maximum_position_secs)
}

fn native_apple_music_gapless_pair(current: &SourceRef, next: &SourceRef) -> bool {
    let (
        SourceRef::AppleMusicTrack {
            storefront: current_storefront,
            album_id: current_album_id,
            track_number: current_track,
            disc_number: current_disc,
            ..
        },
        SourceRef::AppleMusicTrack {
            storefront: next_storefront,
            album_id: next_album_id,
            track_number: next_track,
            disc_number: next_disc,
            ..
        },
    ) = (current, next)
    else {
        return false;
    };
    let same_album = current_album_id
        .as_deref()
        .map(str::trim)
        .filter(|album_id| !album_id.is_empty())
        .is_some_and(|album_id| {
            next_album_id
                .as_deref()
                .map(str::trim)
                .is_some_and(|next_album_id| next_album_id == album_id)
        });
    let same_storefront = current_storefront
        .as_deref()
        .map(str::trim)
        .zip(next_storefront.as_deref().map(str::trim))
        .is_some_and(|(current, next)| current.eq_ignore_ascii_case(next));
    let consecutive = match (current_disc, current_track, next_disc, next_track) {
        (Some(current_disc), Some(current_track), Some(next_disc), Some(next_track)) => {
            (current_disc == next_disc && current_track.checked_add(1) == Some(*next_track))
                || (current_disc.checked_add(1) == Some(*next_disc) && *next_track == 1)
        }
        _ => false,
    };
    same_album && same_storefront && consecutive
}

fn music_track_matches_source(snapshot: &MusicAppSnapshot, source: &SourceRef) -> bool {
    let Some(actual_title) = snapshot.track.title.as_deref() else {
        return false;
    };
    let title_matches = source
        .title()
        .is_none_or(|expected| normalize_metadata(expected) == normalize_metadata(actual_title));
    let artist_matches = source.artist().is_none_or(|expected| {
        snapshot.track.artist.as_deref().is_some_and(|actual| {
            let expected = normalize_metadata(expected);
            let actual = normalize_metadata(actual);
            expected == actual || expected.contains(&actual) || actual.contains(&expected)
        })
    });
    title_matches && artist_matches
}

fn normalize_metadata(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

pub(crate) fn native_local_player(state: &AppState, zone_id: &str) -> Option<Arc<Player>> {
    state
        .zones()
        .player_for_zone(zone_id)
        .or_else(|| state.zones().player_for_enabled_local_zone(zone_id))
}

fn ensure_guard_current(state: &AppState, guard: &PlaybackGuard) -> Result<(), PlaybackError> {
    guard
        .is_current(state)
        .then_some(())
        .ok_or_else(|| PlaybackError::conflict("Playback changed"))
}

fn ensure_owned(
    state: &AppState,
    guard: &PlaybackGuard,
    expected: &AppleMusicPlaybackSnapshot,
) -> Result<(), PlaybackError> {
    if !guard.sequence_is_current(state) {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    let Some(current) = state
        .apple_music_playback()
        .playback_snapshot_for_zone(&expected.zone_id)
    else {
        return Err(PlaybackError::conflict("Playback changed"));
    };
    let current_player_epoch =
        native_local_player(state, &expected.zone_id).map(|player| player.playback_epoch());
    if current.generation != expected.generation
        || current_player_epoch != Some(current.player_epoch)
    {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    Ok(())
}

async fn hold_player_paused(
    player: &std::sync::Arc<crate::audio::player::Player>,
    expected_epoch: u64,
) -> Result<(), PlaybackError> {
    use crate::audio::player::PlaybackState;

    let deadline = tokio::time::Instant::now() + PLAYER_PAUSE_TIMEOUT;
    loop {
        if player.playback_epoch() != expected_epoch {
            return Err(PlaybackError::conflict("Playback changed"));
        }
        match player.playback_state() {
            PlaybackState::Paused => return Ok(()),
            PlaybackState::Starting | PlaybackState::Playing => player.pause(),
            PlaybackState::Stopped => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(PlaybackError::integration(
                "Fozmo's local Player did not enter its paused prebuffer state in time.",
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_player_output_ready(
    state: &AppState,
    guard: &PlaybackGuard,
    playback: &AppleMusicPlaybackSnapshot,
    player: &std::sync::Arc<crate::audio::player::Player>,
    expected_epoch: u64,
) -> Result<(), PlaybackError> {
    use crate::audio::player::PlaybackState;

    let deadline = tokio::time::Instant::now() + PLAYER_OUTPUT_START_TIMEOUT;
    let mut next_music_status = tokio::time::Instant::now();
    let mut consecutive_music_stopped = 0_u8;
    let mut startup_recovery_attempted = false;
    loop {
        ensure_owned(state, guard, playback)?;
        if player.playback_epoch() != expected_epoch {
            return Err(PlaybackError::conflict("Playback changed"));
        }
        let snapshot = player.snapshot_no_cover();
        if local_player_output_is_ready(snapshot.state) {
            debug!(
                event = "apple_music_native_output_ready",
                output_mode = snapshot.signal_path.active_output_mode.as_name(),
                output_transport = snapshot.signal_path.output_transport.as_name(),
                "The local output path is ready for native Apple Music playback"
            );
            return Ok(());
        }
        if tokio::time::Instant::now() >= next_music_status {
            let music = music_status_blocking().await?;
            if !music.running {
                return Err(PlaybackError::integration(
                    "Music.app quit while Fozmo was opening the local output.",
                ));
            }
            if music.has_current_track() && !music_track_matches_source(&music, &playback.source) {
                return Err(PlaybackError::integration(
                    "Music.app changed tracks while Fozmo was opening the local output.",
                ));
            }
            match music.player_state.as_deref() {
                Some("playing") => consecutive_music_stopped = 0,
                Some("paused" | "stopped") => {
                    consecutive_music_stopped = consecutive_music_stopped.saturating_add(1);
                    if consecutive_music_stopped >= 2 {
                        if startup_recovery_attempted {
                            return Err(PlaybackError::integration(
                                "Music.app repeatedly stopped while Fozmo was opening the local output.",
                            ));
                        }
                        startup_recovery_attempted = true;
                        warn!(
                            event = "apple_music_native_startup_transport_recovery",
                            zone_id = playback.zone_id,
                            generation = playback.generation,
                            "Music.app stopped during output startup; retrying the selected track with its normal transport"
                        );
                        restart_selected_track_with_normal_transport(&playback.source).await?;
                        ensure_owned(state, guard, playback)?;
                        consecutive_music_stopped = 0;
                    }
                }
                _ => {}
            }
            next_music_status = tokio::time::Instant::now() + STARTUP_TRANSPORT_STATUS_POLL;
        }
        if snapshot.state == PlaybackState::Stopped {
            return Err(PlaybackError::integration(
                snapshot.output_notice.unwrap_or_else(|| {
                    "Fozmo's local Player stopped while opening the Apple Music output path."
                        .to_string()
                }),
            ));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(PlaybackError::integration(format!(
                "Fozmo's local Player did not open the Apple Music output path in time (state={}, requested={}, active={}, transport={}{}).",
                snapshot.state.as_name(),
                snapshot.signal_path.output_mode.as_name(),
                snapshot.signal_path.active_output_mode.as_name(),
                snapshot.signal_path.output_transport.as_name(),
                snapshot
                    .output_notice
                    .as_deref()
                    .map(|notice| format!(", notice={notice}"))
                    .unwrap_or_default(),
            )));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn local_player_output_is_ready(state: crate::audio::player::PlaybackState) -> bool {
    use crate::audio::player::PlaybackState;

    // A live session resumes in Starting. The audio worker only promotes it to
    // Playing after an ActiveOutput exists and its pre-roll/warmup is ready.
    // Signal-path transport is diagnostic metadata and can briefly lag when
    // the worker deliberately retains a compatible CoreAudio/DoP stream.
    state == PlaybackState::Playing
}

async fn wait_for_prefill(
    state: &AppState,
    guard: &PlaybackGuard,
    playback: &AppleMusicPlaybackSnapshot,
    target_secs: f64,
    minimum_secs: f64,
    timeout: Duration,
) -> Result<(), PlaybackError> {
    let deadline = tokio::time::Instant::now() + timeout;
    let started = tokio::time::Instant::now();
    loop {
        ensure_owned(state, guard, playback)?;
        let buffered = state
            .apple_music_playback()
            .buffered_audio_secs()
            .map_err(PlaybackError::integration)?;
        if buffered >= target_secs && started.elapsed().as_secs_f64() >= minimum_secs {
            debug!(
                event = "apple_music_native_prefill_ready",
                buffered_ms = buffered * 1_000.0,
                target_ms = target_secs * 1_000.0,
                "Native Apple Music capture prebuffer is ready"
            );
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            if buffered >= minimum_secs && started.elapsed().as_secs_f64() >= minimum_secs {
                warn!(
                    event = "apple_music_native_prefill_short",
                    buffered_ms = buffered * 1_000.0,
                    minimum_ms = minimum_secs * 1_000.0,
                    "Native Apple Music prebuffer timed out above the safe minimum"
                );
                return Ok(());
            }
            return Err(PlaybackError::integration(format!(
                "Music.app delivered only {:.0} ms of the required {:.0} ms startup prebuffer.",
                buffered * 1_000.0,
                minimum_secs * 1_000.0
            )));
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn wait_for_prefill_without_guard(
    state: &AppState,
    snapshot: &AppleMusicPlaybackSnapshot,
    target_secs: f64,
    minimum_secs: f64,
    timeout: Duration,
) -> Result<(), PlaybackError> {
    wait_for_prefill(
        state,
        &PlaybackGuard::none(),
        snapshot,
        target_secs,
        minimum_secs,
        timeout,
    )
    .await
}

async fn cleanup_failed_start(
    state: &AppState,
    generation: u64,
    player: &std::sync::Arc<crate::audio::player::Player>,
) {
    let _ = pause_music_blocking().await;
    if state
        .apple_music_playback()
        .playback_snapshot()
        .is_some_and(|snapshot| snapshot.generation == generation)
    {
        state.apple_music_playback().stop_runtime(true);
    } else {
        player.stop();
    }
}

fn spawn_music_app_monitor(state: AppState, zone_id: String, generation: u64) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(MONITOR_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_position = 0.0_f64;
        let mut last_duration = 0.0_f64;
        let mut consecutive_errors = 0_u8;
        let mut consecutive_stopped = 0_u8;
        let mut startup_recovery_attempted = false;
        let monitor_started = tokio::time::Instant::now();
        loop {
            ticker.tick().await;
            let Some(snapshot) = active_snapshot(&state, &zone_id) else {
                break;
            };
            if snapshot.generation != generation {
                break;
            }
            let music = match music_status_blocking().await {
                Ok(status) => {
                    consecutive_errors = 0;
                    status
                }
                Err(error) => {
                    consecutive_errors = consecutive_errors.saturating_add(1);
                    if consecutive_errors < 3 {
                        continue;
                    }
                    warn!(
                        event = "apple_music_native_monitor_failed",
                        zone_id,
                        error = %error,
                        "Music.app playback monitoring failed"
                    );
                    finish_native_playback(
                        state.clone(),
                        zone_id.clone(),
                        generation,
                        false,
                        "monitor_failed",
                    )
                    .await;
                    break;
                }
            };
            if !music.running {
                finish_native_playback(
                    state.clone(),
                    zone_id.clone(),
                    generation,
                    false,
                    "music_app_quit",
                )
                .await;
                break;
            }
            if music.has_current_track() && !music_track_matches_source(&music, &snapshot.source) {
                let duration = if last_duration > 0.0 {
                    last_duration
                } else {
                    snapshot.duration_secs
                };
                let completed = native_track_completed(last_position, duration);
                if completed
                    && let Some(next) = promote_continuous_apple_music_boundary(
                        &state, &zone_id, generation, &snapshot, &music,
                    )
                {
                    last_position = music.track.position_secs.unwrap_or(0.0);
                    last_duration = music
                        .track
                        .duration_secs
                        .or_else(|| next.duration_secs())
                        .unwrap_or(0.0);
                    consecutive_stopped = 0;
                    startup_recovery_attempted = false;
                    continue;
                }
                warn!(
                    event = "apple_music_native_track_interrupted",
                    zone_id,
                    expected = snapshot.source.key(),
                    actual_title = music.track.title.as_deref().unwrap_or_default(),
                    last_position_secs = last_position,
                    duration_secs = duration,
                    completed,
                    "Music.app changed away from Fozmo's selected catalog track"
                );
                if completed {
                    let _ = pause_music_blocking().await;
                }
                finish_native_playback(
                    state.clone(),
                    zone_id.clone(),
                    generation,
                    completed,
                    if completed {
                        "completed_track_transition"
                    } else {
                        "track_interrupted"
                    },
                )
                .await;
                break;
            }
            if let Some(position) = music.track.position_secs {
                last_position = position;
            }
            if let Some(duration) = music.track.duration_secs {
                last_duration = duration;
            }
            match music.player_state.as_deref() {
                Some("playing") => {
                    consecutive_stopped = 0;
                    state.apple_music_playback().update_playback(
                        generation,
                        "playing",
                        music.track.position_secs,
                        music.track.duration_secs,
                    );
                }
                Some("paused") => {
                    consecutive_stopped = 0;
                    state.apple_music_playback().update_playback(
                        generation,
                        "paused",
                        music.track.position_secs,
                        music.track.duration_secs,
                    );
                }
                Some("stopped") => {
                    consecutive_stopped = consecutive_stopped.saturating_add(1);
                    let elapsed = monitor_started.elapsed();
                    if !stopped_state_is_terminal(consecutive_stopped, elapsed) {
                        debug!(
                            event = "apple_music_native_terminal_state_deferred",
                            zone_id,
                            generation,
                            consecutive_stopped,
                            elapsed_ms = elapsed.as_millis(),
                            "Deferring a transient Music.app stopped state"
                        );
                        continue;
                    }
                    if should_recover_startup_stall(last_position, startup_recovery_attempted) {
                        startup_recovery_attempted = true;
                        match restart_selected_track_with_normal_transport(&snapshot.source).await {
                            Ok(recovered) => {
                                consecutive_stopped = 0;
                                if let Some(position) = recovered.track.position_secs {
                                    last_position = position;
                                }
                                if let Some(duration) = recovered.track.duration_secs {
                                    last_duration = duration;
                                }
                                state.apple_music_playback().update_playback(
                                    generation,
                                    "playing",
                                    recovered.track.position_secs,
                                    recovered.track.duration_secs,
                                );
                                info!(
                                    event = "apple_music_native_startup_transport_recovered",
                                    zone_id,
                                    generation,
                                    "Restarted a track-specific Music.app startup stall without reopening the output"
                                );
                                continue;
                            }
                            Err(error) => {
                                warn!(
                                    event = "apple_music_native_startup_transport_recovery_failed",
                                    zone_id,
                                    generation,
                                    error = %error,
                                    "Music.app did not recover from its startup stall"
                                );
                            }
                        }
                    }
                    let duration = if last_duration > 0.0 {
                        last_duration
                    } else {
                        snapshot.duration_secs
                    };
                    let completed = native_track_completed(last_position, duration);
                    info!(
                        event = "apple_music_native_terminal_state",
                        zone_id,
                        generation,
                        last_position_secs = last_position,
                        duration_secs = duration,
                        completed,
                        queued_count = zone_queue_sources(&state, &zone_id).len(),
                        "Observed Music.app's terminal state"
                    );
                    state.apple_music_playback().update_playback(
                        generation,
                        "stopped",
                        Some(last_position),
                        Some(duration),
                    );
                    finish_native_playback(
                        state.clone(),
                        zone_id.clone(),
                        generation,
                        completed,
                        if completed {
                            "completed"
                        } else {
                            "stopped_early"
                        },
                    )
                    .await;
                    break;
                }
                _ => {
                    consecutive_stopped = 0;
                }
            }
        }
    });
}

fn promote_continuous_apple_music_boundary(
    state: &AppState,
    zone_id: &str,
    generation: u64,
    previous: &AppleMusicPlaybackSnapshot,
    music: &MusicAppSnapshot,
) -> Option<SourceRef> {
    let next = zone_queue_sources(state, zone_id).into_iter().next()?;
    if !native_apple_music_gapless_pair(&previous.source, &next)
        || !music_track_matches_source(music, &next)
    {
        return None;
    }
    let player = native_local_player(state, zone_id)?;
    if player.playback_epoch() != previous.player_epoch {
        return None;
    }
    let player_position_secs = native_player_position_secs(&player);
    if !state.apple_music_playback().promote_continuous_playback(
        generation,
        next.clone(),
        player_position_secs,
        music.track.position_secs,
        music.track.duration_secs,
    ) {
        return None;
    }
    state.listening().completed_next(state.library(), zone_id);
    spawn_native_next_prefetch(state.clone(), zone_id.to_string());
    info!(
        event = "apple_music_native_gapless_album_transition",
        zone_id,
        previous_source_key = previous.source.key(),
        next_source_key = next.key(),
        player_epoch = previous.player_epoch,
        "Music.app advanced to the queued album track without reopening capture or output"
    );
    Some(next)
}

fn native_player_position_secs(player: &Player) -> f64 {
    let snapshot = player.snapshot_no_cover();
    let target_rate = snapshot.signal_path.target_rate;
    if target_rate == 0 {
        0.0
    } else {
        snapshot.metrics.position_samples as f64 / f64::from(target_rate)
    }
}

fn stopped_state_is_terminal(consecutive_stopped: u8, elapsed: Duration) -> bool {
    elapsed >= TERMINAL_STATE_STARTUP_GRACE && consecutive_stopped >= TERMINAL_STATE_CONFIRMATIONS
}

fn should_recover_startup_stall(last_position_secs: f64, already_attempted: bool) -> bool {
    !already_attempted
        && (!last_position_secs.is_finite()
            || last_position_secs <= STARTUP_STALL_POSITION_MAX_SECS)
}

fn native_track_completed(last_position_secs: f64, duration_secs: f64) -> bool {
    duration_secs.is_finite()
        && duration_secs > 0.0
        && last_position_secs.is_finite()
        && last_position_secs > 0.0
        && (last_position_secs >= duration_secs - COMPLETION_TAIL_SECS
            || last_position_secs / duration_secs >= 0.97)
}

async fn finish_native_playback(
    state: AppState,
    zone_id: String,
    generation: u64,
    completed: bool,
    reason: &'static str,
) {
    let playback_switch = state.apple_music().lock_playback_switch().await;
    let Some(snapshot) = active_snapshot(&state, &zone_id) else {
        return;
    };
    if snapshot.generation != generation {
        return;
    }
    let queue = zone_queue_sources(&state, &zone_id);
    let profile_id = state
        .listening()
        .profile_id(&zone_id)
        .unwrap_or_else(|| crate::settings::DEFAULT_PROFILE_ID.to_string());
    let detached_epoch = if retains_capture_route(completed, &queue) {
        state.apple_music_playback().stop_playback_retaining_route()
    } else {
        state.apple_music_playback().stop_runtime(false)
    }
    .unwrap_or(snapshot.player_epoch);
    if !completed {
        if let Some(player) = native_local_player(&state, &zone_id) {
            player.stop();
        }
        state.listening().stop(state.library(), &zone_id);
        warn!(
            event = "apple_music_native_finished",
            zone_id, reason, "Native Apple Music playback ended without queue advancement"
        );
        return;
    }
    let next = queue.split_first().map(|(next, rest)| {
        let next = next.clone();
        let rest = rest.to_vec();
        (next, rest)
    });
    // Dropping the capture session closes the producer but intentionally leaves
    // the live source's ring intact. Let Player consume that final captured PCM,
    // flush the DSP/output, and reach EOF before replacing the provider.
    drop(playback_switch);
    let expect_engine_handoff = next.as_ref().is_some_and(|(next, _)| {
        matches!(
            next,
            SourceRef::LocalTrack { .. } | SourceRef::QobuzTrack { .. }
        )
    });
    let Some(boundary) =
        wait_for_live_eof_drain(&state, &zone_id, detached_epoch, expect_engine_handoff).await
    else {
        state.apple_music_playback().release_retained_route();
        return;
    };
    let Some((next, rest)) = next else {
        state.listening().stop(state.library(), &zone_id);
        debug!(
            event = "apple_music_native_queue_finished",
            zone_id, "Native Apple Music queue reached its end"
        );
        return;
    };
    if matches!(boundary, LiveEofBoundary::AutoAdvanced(_)) && expect_engine_handoff {
        promote_gapless_listening_boundary(&state, &zone_id, &snapshot.source, &next);
        info!(
            event = "apple_music_native_gapless_handoff",
            zone_id,
            next_source_key = next.key(),
            player_epoch = boundary.epoch(),
            "Player promoted the prefetched source without reopening the output"
        );
        return;
    }
    route_after_native_boundary(
        state,
        zone_id,
        profile_id,
        next,
        rest,
        boundary.epoch(),
        reason,
    )
    .await;
}

fn promote_gapless_listening_boundary(
    state: &AppState,
    zone_id: &str,
    previous: &SourceRef,
    next: &SourceRef,
) {
    let previous_key = previous.key();
    let next_key = next.key();
    match state.listening().active_source(zone_id) {
        Some(active) if active.key() == previous_key => {
            state.listening().completed_next(state.library(), zone_id);
        }
        // The global status observer can see Player's new metadata in the few
        // microseconds before this task runs. It already promotes the same
        // queued source in that case; advancing again would skip a track.
        Some(active) if active.key() == next_key => {}
        active => {
            warn!(
                event = "apple_music_native_listening_handoff_mismatch",
                zone_id,
                previous_source_key = previous_key,
                next_source_key = next_key,
                active_source_key = active.map(|source| source.key()),
                "Player advanced, but listening state no longer owned the expected queue boundary"
            );
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveEofBoundary {
    Stopped(u64),
    AutoAdvanced(u64),
}

impl LiveEofBoundary {
    fn epoch(self) -> u64 {
        match self {
            Self::Stopped(epoch) | Self::AutoAdvanced(epoch) => epoch,
        }
    }
}

async fn wait_for_live_eof_drain(
    state: &AppState,
    zone_id: &str,
    expected_epoch: u64,
    expect_engine_handoff: bool,
) -> Option<LiveEofBoundary> {
    use crate::audio::player::PlaybackState;

    let deadline = tokio::time::Instant::now() + PLAYER_EOF_DRAIN_TIMEOUT;
    let mut stopped_since = None;
    loop {
        let Some(player) = native_local_player(state, zone_id) else {
            return None;
        };
        if player.playback_epoch() != expected_epoch {
            return None;
        }
        let playback_state = player.playback_state();
        let auto_advanced = expect_engine_handoff
            && playback_state != PlaybackState::Stopped
            && player
                .current_file_name()
                .as_deref()
                .is_some_and(|name| name != APPLE_MUSIC_LIVE_DISPLAY_NAME);
        if auto_advanced {
            return Some(LiveEofBoundary::AutoAdvanced(expected_epoch));
        }
        if playback_state == PlaybackState::Stopped {
            if !expect_engine_handoff {
                return Some(LiveEofBoundary::Stopped(expected_epoch));
            }
            let stopped_at = stopped_since.get_or_insert_with(tokio::time::Instant::now);
            if stopped_at.elapsed() >= PLAYER_AUTO_ADVANCE_START_GRACE {
                return Some(LiveEofBoundary::Stopped(expected_epoch));
            }
        } else {
            stopped_since = None;
        }
        if tokio::time::Instant::now() >= deadline {
            warn!(
                event = "apple_music_native_eof_drain_timeout",
                zone_id,
                expected_epoch,
                "Captured Apple Music tail did not finish draining before the next provider handoff"
            );
            player.stop();
            return Some(LiveEofBoundary::Stopped(player.playback_epoch()));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn route_after_native_boundary(
    state: AppState,
    zone_id: String,
    profile_id: String,
    source: SourceRef,
    queue: Vec<SourceRef>,
    expected_epoch: u64,
    reason: &'static str,
) {
    if native_local_player(&state, &zone_id)
        .is_none_or(|player| player.playback_epoch() != expected_epoch)
    {
        state.apple_music_playback().release_retained_route();
        return;
    }
    let source_key = source.key();
    let result = Box::pin(PlaybackRouter::new(&state).execute(
        &zone_id,
        PlaybackIntent::Play {
            profile_id,
            radio_auto: source.is_radio(),
            source,
            queue,
            guard: PlaybackGuard::from_expected_player_epoch(zone_id.clone(), expected_epoch),
            qobuz_request: None,
        },
    ))
    .await;
    if let Err(error) = result {
        warn!(
            event = "apple_music_native_queue_advance_failed",
            zone_id,
            reason,
            source_key,
            error = %error,
            "Could not route the next mixed-provider queue entry"
        );
        if let Some(player) = native_local_player(&state, &zone_id)
            && player.playback_epoch() == expected_epoch
        {
            player.stop();
        }
        // The successor never opened its capture session, so nothing else will
        // hand the macOS default output back to the DAC.
        state.apple_music_playback().release_retained_route();
        state.listening().stop(state.library(), &zone_id);
    } else {
        debug!(
            event = "apple_music_native_queue_advance",
            zone_id, reason, source_key, "Routed the next mixed-provider queue entry"
        );
    }
}

/// Whether the boundary hands the Fozmo Capture route straight to another
/// Apple Music track rather than to the DAC.
fn retains_capture_route(completed: bool, queue: &[SourceRef]) -> bool {
    completed
        && queue
            .first()
            .is_some_and(|next| matches!(next, SourceRef::AppleMusicTrack { .. }))
}

fn zone_queue_sources(state: &AppState, zone_id: &str) -> Vec<SourceRef> {
    state
        .library()
        .zone_queue(zone_id)
        .map(|entries| entries.into_iter().map(|entry| entry.source).collect())
        .unwrap_or_default()
}

async fn music_status_blocking() -> Result<MusicAppSnapshot, PlaybackError> {
    tokio::task::spawn_blocking(music_app_status)
        .await
        .map_err(|error| {
            PlaybackError::internal_invariant(format!("Music.app status task stopped: {error}"))
        })?
        .map_err(PlaybackError::integration)
}

async fn restart_selected_track_with_normal_transport(
    source: &SourceRef,
) -> Result<MusicAppSnapshot, PlaybackError> {
    play_music_blocking().await?;
    // `play` can restore Music.app's previous global player position while it
    // creates the transport. Apply the start position after that transition.
    set_music_position_blocking(0.0).await?;
    let deadline = tokio::time::Instant::now() + STARTUP_TRANSPORT_RECOVERY_TIMEOUT;
    loop {
        let snapshot = music_status_blocking().await?;
        if snapshot.running
            && snapshot.player_state.as_deref() == Some("playing")
            && music_track_matches_source(&snapshot, source)
        {
            return Ok(snapshot);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(PlaybackError::integration(
                "Music.app did not restart the selected track with its normal transport.",
            ));
        }
        tokio::time::sleep(TRACK_ACTIVATION_POLL).await;
    }
}

async fn prepare_music_blocking() -> Result<(), PlaybackError> {
    tokio::task::spawn_blocking(prepare_music_app)
        .await
        .map_err(|error| {
            PlaybackError::internal_invariant(format!(
                "Music.app preparation task stopped: {error}"
            ))
        })?
        .map_err(PlaybackError::integration)
}

async fn pause_music_blocking() -> Result<(), PlaybackError> {
    tokio::task::spawn_blocking(pause_music_app)
        .await
        .map_err(|error| {
            PlaybackError::internal_invariant(format!("Music.app pause task stopped: {error}"))
        })?
        .map_err(PlaybackError::integration)
}

async fn play_music_blocking() -> Result<(), PlaybackError> {
    tokio::task::spawn_blocking(play_music_app)
        .await
        .map_err(|error| {
            PlaybackError::internal_invariant(format!("Music.app play task stopped: {error}"))
        })?
        .map_err(PlaybackError::integration)
}

async fn play_current_once_blocking() -> Result<(), PlaybackError> {
    tokio::task::spawn_blocking(play_music_app_current_once)
        .await
        .map_err(|error| {
            PlaybackError::internal_invariant(format!(
                "Music.app single-track play task stopped: {error}"
            ))
        })?
        .map_err(PlaybackError::integration)
}

async fn play_current_in_context_blocking() -> Result<(), PlaybackError> {
    tokio::task::spawn_blocking(play_music_app_current_in_context)
        .await
        .map_err(|error| {
            PlaybackError::internal_invariant(format!(
                "Music.app contextual play task stopped: {error}"
            ))
        })?
        .map_err(PlaybackError::integration)
}

async fn set_music_position_blocking(seconds: f64) -> Result<(), PlaybackError> {
    tokio::task::spawn_blocking(move || set_music_app_position(seconds))
        .await
        .map_err(|error| {
            PlaybackError::internal_invariant(format!("Music.app seek task stopped: {error}"))
        })?
        .map_err(PlaybackError::integration)
}

fn playback_error(error: AppleMusicMvpError) -> PlaybackError {
    match error.code.as_str() {
        "music_authorization_not_determined"
        | "music_authorization_denied"
        | "subscription_required"
        | "musickit_capability_unavailable" => PlaybackError::forbidden(error.message),
        "song_not_found" | "album_not_found" | "helper_missing" => {
            PlaybackError::not_found(error.message)
        }
        "apple_music_active_segment_requires_reprepare" => PlaybackError::conflict(error.message),
        "apple_music_storefront_invalid" | "apple_music_seek_invalid" | "queue_prepare_failed" => {
            PlaybackError::bad_request(error.message)
        }
        _ if error.retryable => PlaybackError::retryable_network(error.message),
        _ => PlaybackError::integration(error.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apple_source(title: Option<&str>, artist: Option<&str>) -> SourceRef {
        SourceRef::AppleMusicTrack {
            song_id: "635770203".to_string(),
            storefront: Some("nz".to_string()),
            title: title.map(str::to_string),
            artist: artist.map(str::to_string),
            album: Some("Hotel California".to_string()),
            album_artist: artist.map(str::to_string),
            album_id: Some("635770200".to_string()),
            artwork_url: None,
            duration_secs: Some(300.0),
            track_number: Some(2),
            disc_number: Some(1),
            isrc: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }

    #[test]
    fn music_metadata_match_is_case_and_punctuation_tolerant() {
        let mut snapshot = MusicAppSnapshot::default();
        snapshot.running = true;
        snapshot.track.title = Some("New Kid In Town".to_string());
        snapshot.track.artist = Some("Eagles".to_string());

        assert!(music_track_matches_source(
            &snapshot,
            &apple_source(Some("New Kid in Town"), Some("EAGLES"))
        ));
    }

    #[test]
    fn native_gapless_pair_requires_the_next_track_from_the_same_apple_album() {
        let current = apple_source(Some("New Kid in Town"), Some("Eagles"));
        let mut next = apple_source(Some("Life in the Fast Lane"), Some("Eagles"));
        if let SourceRef::AppleMusicTrack {
            song_id,
            track_number,
            ..
        } = &mut next
        {
            *song_id = "635770204".to_string();
            *track_number = Some(3);
        }
        assert!(native_apple_music_gapless_pair(&current, &next));

        let mut skipped = next.clone();
        if let SourceRef::AppleMusicTrack { track_number, .. } = &mut skipped {
            *track_number = Some(4);
        }
        assert!(!native_apple_music_gapless_pair(&current, &skipped));

        let mut other_album = next.clone();
        if let SourceRef::AppleMusicTrack { album_id, .. } = &mut other_album {
            *album_id = Some("different-album".to_string());
        }
        assert!(!native_apple_music_gapless_pair(&current, &other_album));
    }

    #[test]
    fn native_gapless_pair_accepts_the_first_track_of_the_next_disc() {
        let mut current = apple_source(Some("Disc One Finale"), Some("Artist"));
        let mut next = apple_source(Some("Disc Two Opener"), Some("Artist"));
        if let SourceRef::AppleMusicTrack {
            track_number,
            disc_number,
            ..
        } = &mut current
        {
            *track_number = Some(10);
            *disc_number = Some(1);
        }
        if let SourceRef::AppleMusicTrack {
            song_id,
            track_number,
            disc_number,
            ..
        } = &mut next
        {
            *song_id = "635770204".to_string();
            *track_number = Some(1);
            *disc_number = Some(2);
        }
        assert!(native_apple_music_gapless_pair(&current, &next));
    }

    #[test]
    fn catalog_identity_requires_album_for_exact_native_selection() {
        let mut source = apple_source(Some("Track"), Some("Artist"));
        if let SourceRef::AppleMusicTrack { album_id, .. } = &mut source {
            *album_id = None;
        }
        assert!(catalog_identity(&source).is_err());
    }

    #[test]
    fn terminal_music_state_uses_retained_near_end_position_as_completion() {
        assert!(native_track_completed(296.5, 300.0));
        assert!(native_track_completed(291.0, 300.0));
        assert!(!native_track_completed(280.0, 300.0));
        assert!(!native_track_completed(0.0, 300.0));
        assert!(!native_track_completed(296.5, 0.0));
    }

    #[test]
    fn native_track_start_requires_a_position_near_zero() {
        let mut snapshot = MusicAppSnapshot::default();
        snapshot.track.position_secs = Some(0.75);
        assert!(track_position_is_at_start(
            &snapshot,
            TRACK_START_POSITION_TOLERANCE_SECS
        ));

        snapshot.track.position_secs = Some(42.0);
        assert!(!track_position_is_at_start(
            &snapshot,
            TRACK_START_POSITION_TOLERANCE_SECS
        ));
    }

    #[test]
    fn native_output_is_ready_when_the_audio_worker_enters_playing() {
        use crate::audio::player::PlaybackState;

        assert!(!local_player_output_is_ready(PlaybackState::Starting));
        assert!(local_player_output_is_ready(PlaybackState::Playing));
        assert!(!local_player_output_is_ready(PlaybackState::Paused));
        assert!(!local_player_output_is_ready(PlaybackState::Stopped));
    }

    #[test]
    fn terminal_music_state_requires_grace_and_consecutive_observations() {
        assert!(!stopped_state_is_terminal(
            TERMINAL_STATE_CONFIRMATIONS,
            TERMINAL_STATE_STARTUP_GRACE - Duration::from_millis(1)
        ));
        assert!(!stopped_state_is_terminal(
            TERMINAL_STATE_CONFIRMATIONS - 1,
            TERMINAL_STATE_STARTUP_GRACE
        ));
        assert!(stopped_state_is_terminal(
            TERMINAL_STATE_CONFIRMATIONS,
            TERMINAL_STATE_STARTUP_GRACE
        ));
    }

    #[test]
    fn only_a_completed_apple_music_successor_keeps_the_capture_route() {
        let apple = apple_source(Some("Track"), Some("Artist"));
        let local = SourceRef::LocalTrack {
            track_id: 7,
            file_name: None,
            title: Some("Local".to_string()),
            artist: Some("Artist".to_string()),
            album: None,
            album_artist: None,
            album_id: None,
            art_id: None,
            duration_secs: Some(180.0),
            ext_hint: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        };

        assert!(retains_capture_route(true, std::slice::from_ref(&apple)));
        // A local or Qobuz successor needs the DAC back as the macOS default.
        assert!(!retains_capture_route(true, std::slice::from_ref(&local)));
        // Nothing follows, so the route must be handed back.
        assert!(!retains_capture_route(true, &[]));
        // An interrupted track is not handing off to anything.
        assert!(!retains_capture_route(false, std::slice::from_ref(&apple)));
    }

    #[test]
    fn startup_stall_recovery_is_limited_to_an_unadvanced_first_attempt() {
        assert!(should_recover_startup_stall(0.0, false));
        assert!(should_recover_startup_stall(
            STARTUP_STALL_POSITION_MAX_SECS,
            false
        ));
        assert!(!should_recover_startup_stall(
            STARTUP_STALL_POSITION_MAX_SECS + 0.001,
            false
        ));
        assert!(!should_recover_startup_stall(0.0, true));
    }
}
