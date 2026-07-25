//! Product Apple Music playback on macOS.
//!
//! MusicKit remains the catalog/authorization plane. The audio plane is the
//! native Music.app lossless decoder routed through Fozmo Capture, then through
//! the normal local Player/DSP/output selected for the zone.

use crate::app::state::AppState;
use crate::playback::error::PlaybackError;
use crate::playback::intent::{PlaybackGuard, PlaybackIntent, PlaybackOutcome};
use crate::playback::router::PlaybackRouter;
use crate::playback::service::{
    apply_playback_settings_for_zone, prepare_airplay_volume_for_zone, prepare_hegel_for_zone,
};
use crate::protocol::SourceRef;
use crate::services::apple_music::NativeAppleMusicPlaybackSnapshot;
use crate::services::apple_music_musickit::{
    AppleMusicMvpError, MusicAppSnapshot, activate_catalog_track, music_app_status,
    pause_music_app, play_music_app, play_music_app_current_once, prepare_music_app,
    set_music_app_position,
};
use std::time::{Duration, SystemTime};
use tracing::{debug, info, warn};

const TRACK_ACTIVATION_TIMEOUT: Duration = Duration::from_secs(10);
const TRACK_ACTIVATION_POLL: Duration = Duration::from_millis(75);
const START_PREFILL_TARGET_SECS: f64 = 0.500;
const START_PREFILL_MIN_SECS: f64 = 0.250;
const START_PREFILL_TIMEOUT: Duration = Duration::from_millis(2_500);
const RESUME_PREFILL_TARGET_SECS: f64 = 0.080;
const RESUME_PREFILL_MIN_SECS: f64 = 0.030;
const RESUME_PREFILL_TIMEOUT: Duration = Duration::from_millis(1_000);
const MONITOR_INTERVAL: Duration = Duration::from_millis(350);
const COMPLETION_TAIL_SECS: f64 = 4.0;
const PLAYER_PAUSE_TIMEOUT: Duration = Duration::from_secs(4);
const PLAYER_EOF_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

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
    if !state.apple_music().system_audio_capture_confirmed() {
        return Err(PlaybackError::forbidden(
            "Confirm macOS system-audio capture in Apple Music settings before playback.",
        ));
    }
    if !guard.is_current(state) {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    let player = state
        .zones()
        .player_for_zone(zone_id)
        .ok_or(PlaybackError::ZoneNotAvailable)?;

    let _playback_switch = state.apple_music().lock_playback_switch().await;
    ensure_guard_current(state, &guard)?;
    pause_music_blocking().await?;
    if state.apple_music_capture().capture_running() {
        state.apple_music_capture().stop_runtime(false);
    }
    crate::playback::apple_music::stop_replaced_session_locked(state, zone_id).await;
    ensure_guard_current(state, &guard)?;

    apply_playback_settings_for_zone(state, zone_id);
    prepare_airplay_volume_for_zone(state, zone_id, &player);
    prepare_hegel_for_zone(state, zone_id).await?;
    prepare_music_blocking().await?;

    let settings = state.settings().apple_music_capture_settings();
    let initial_epoch = state
        .apple_music_capture()
        .start_managed_playback(
            player.clone(),
            &settings,
            zone_id,
            state.apple_music().system_audio_capture_confirmed(),
        )
        .map_err(PlaybackError::integration)?;
    let playback = state.apple_music_capture().activate_managed_playback(
        zone_id.to_string(),
        initial_epoch,
        source.clone(),
    );

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

        let selected = wait_for_selected_track(state, &guard, &playback).await?;
        pause_music_blocking().await?;
        ensure_owned(state, &guard, &playback)?;

        let source_format = state
            .apple_music()
            .probe_music_app_source_format(format_boundary)
            .await
            .map_err(playback_error)?
            .ok_or_else(|| {
                PlaybackError::integration(
                    "Music.app did not expose a fresh Apple Lossless decoder format. Fozmo kept the Hegel muted and did not release unverified audio to the DSP.",
                )
            })?;
        let source_rate_hz = source_format.sample_rate_hz;
        let source_bits = source_format.source_bit_depth_bits;
        let verified_epoch = state
            .apple_music_capture()
            .restart_at_verified_source_format(source_rate_hz, source_bits)
            .map_err(PlaybackError::integration)?;
        if !state
            .apple_music_capture()
            .replace_managed_player_epoch(playback.generation, verified_epoch)
        {
            return Err(PlaybackError::conflict("Playback changed"));
        }
        hold_player_paused(&player, verified_epoch).await?;
        player.flush_live_output();
        ensure_owned(state, &guard, &playback)?;

        play_current_once_blocking().await?;
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
        state.apple_music_capture().update_managed_playback(
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
        Ok::<(), PlaybackError>(())
    }
    .await;

    match start_result {
        Ok(()) => {}
        Err(error) => {
            cleanup_failed_start(state, playback.generation, &player).await;
            return Err(error);
        }
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
    Ok(PlaybackOutcome::Completed)
}

pub(crate) fn active_snapshot(
    state: &AppState,
    zone_id: &str,
) -> Option<NativeAppleMusicPlaybackSnapshot> {
    let snapshot = state
        .apple_music_capture()
        .playback_snapshot_for_zone(zone_id)?;
    let player = state.zones().player_for_zone(zone_id)?;
    (player.playback_epoch() == snapshot.player_epoch).then_some(snapshot)
}

pub(crate) async fn stop_replaced_session_if_current(
    state: &AppState,
    zone_id: &str,
    guard: &PlaybackGuard,
) -> Result<(), PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    ensure_guard_current(state, guard)?;
    if active_snapshot(state, zone_id).is_some() {
        let _ = pause_music_blocking().await;
        state.apple_music_capture().stop_runtime(false);
    }
    crate::playback::apple_music::stop_replaced_session_locked(state, zone_id).await;
    Ok(())
}

pub(crate) async fn pause(state: &AppState, zone_id: &str) -> Result<bool, PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    let Some(snapshot) = active_snapshot(state, zone_id) else {
        return Ok(false);
    };
    pause_music_blocking().await?;
    let player = state
        .zones()
        .player_for_zone(zone_id)
        .ok_or(PlaybackError::ZoneNotAvailable)?;
    player.pause();
    state
        .apple_music_capture()
        .update_managed_playback(snapshot.generation, "paused", None, None);
    Ok(true)
}

pub(crate) async fn resume(state: &AppState, zone_id: &str) -> Result<bool, PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    let Some(snapshot) = active_snapshot(state, zone_id) else {
        return Ok(false);
    };
    let player = state
        .zones()
        .player_for_zone(zone_id)
        .ok_or(PlaybackError::ZoneNotAvailable)?;
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
        .apple_music_capture()
        .update_managed_playback(snapshot.generation, "playing", None, None);
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
    let player = state
        .zones()
        .player_for_zone(zone_id)
        .ok_or(PlaybackError::ZoneNotAvailable)?;
    pause_music_blocking().await?;
    player.pause();
    state.apple_music_capture().update_managed_playback(
        snapshot.generation,
        "paused",
        Some(seconds),
        None,
    );
    let replacement_epoch = state
        .apple_music_capture()
        .restart_current_managed_session()
        .map_err(PlaybackError::integration)?;
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
    state.apple_music_capture().update_managed_playback(
        snapshot.generation,
        "playing",
        Some(seconds),
        None,
    );
    Ok(true)
}

pub(crate) async fn stop(state: &AppState, zone_id: &str) -> Result<bool, PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    if active_snapshot(state, zone_id).is_none() {
        return Ok(false);
    }
    let _ = pause_music_blocking().await;
    state.apple_music_capture().stop_runtime(true);
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
    let _ = pause_music_blocking().await;
    let detached_epoch = state
        .apple_music_capture()
        .stop_runtime(false)
        .unwrap_or(snapshot.player_epoch);
    let Some((next, rest)) = queue.split_first() else {
        if let Some(player) = state.zones().player_for_zone(zone_id) {
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
    playback: &NativeAppleMusicPlaybackSnapshot,
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

fn ensure_guard_current(state: &AppState, guard: &PlaybackGuard) -> Result<(), PlaybackError> {
    guard
        .is_current(state)
        .then_some(())
        .ok_or_else(|| PlaybackError::conflict("Playback changed"))
}

fn ensure_owned(
    state: &AppState,
    guard: &PlaybackGuard,
    expected: &NativeAppleMusicPlaybackSnapshot,
) -> Result<(), PlaybackError> {
    if !guard.sequence_is_current(state) {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    let Some(current) = state
        .apple_music_capture()
        .playback_snapshot_for_zone(&expected.zone_id)
    else {
        return Err(PlaybackError::conflict("Playback changed"));
    };
    let current_player_epoch = state
        .zones()
        .player_for_zone(&expected.zone_id)
        .map(|player| player.playback_epoch());
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

async fn wait_for_prefill(
    state: &AppState,
    guard: &PlaybackGuard,
    playback: &NativeAppleMusicPlaybackSnapshot,
    target_secs: f64,
    minimum_secs: f64,
    timeout: Duration,
) -> Result<(), PlaybackError> {
    let deadline = tokio::time::Instant::now() + timeout;
    let started = tokio::time::Instant::now();
    loop {
        ensure_owned(state, guard, playback)?;
        let buffered = state
            .apple_music_capture()
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
    snapshot: &NativeAppleMusicPlaybackSnapshot,
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
        .apple_music_capture()
        .managed_playback_snapshot()
        .is_some_and(|snapshot| snapshot.generation == generation)
    {
        state.apple_music_capture().stop_runtime(true);
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
                warn!(
                    event = "apple_music_native_track_interrupted",
                    zone_id,
                    expected = snapshot.source.key(),
                    actual_title = music.track.title.as_deref().unwrap_or_default(),
                    "Music.app changed away from Fozmo's selected catalog track"
                );
                finish_native_playback(
                    state.clone(),
                    zone_id.clone(),
                    generation,
                    false,
                    "track_interrupted",
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
                    state.apple_music_capture().update_managed_playback(
                        generation,
                        "playing",
                        music.track.position_secs,
                        music.track.duration_secs,
                    );
                }
                Some("paused") => {
                    state.apple_music_capture().update_managed_playback(
                        generation,
                        "paused",
                        music.track.position_secs,
                        music.track.duration_secs,
                    );
                }
                Some("stopped") => {
                    let duration = if last_duration > 0.0 {
                        last_duration
                    } else {
                        snapshot.duration_secs
                    };
                    let completed = duration > 0.0
                        && last_position > 0.0
                        && (last_position >= duration - COMPLETION_TAIL_SECS
                            || last_position / duration >= 0.97);
                    state.apple_music_capture().update_managed_playback(
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
                _ => {}
            }
        }
    });
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
    let detached_epoch = state
        .apple_music_capture()
        .stop_runtime(false)
        .unwrap_or(snapshot.player_epoch);
    if !completed {
        if let Some(player) = state.zones().player_for_zone(&zone_id) {
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
    let Some(handoff_epoch) = wait_for_live_eof_drain(&state, &zone_id, detached_epoch).await
    else {
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
    route_after_native_boundary(
        state,
        zone_id,
        profile_id,
        next,
        rest,
        handoff_epoch,
        reason,
    )
    .await;
}

async fn wait_for_live_eof_drain(
    state: &AppState,
    zone_id: &str,
    expected_epoch: u64,
) -> Option<u64> {
    use crate::audio::player::PlaybackState;

    let deadline = tokio::time::Instant::now() + PLAYER_EOF_DRAIN_TIMEOUT;
    loop {
        let Some(player) = state.zones().player_for_zone(zone_id) else {
            return None;
        };
        if player.playback_epoch() != expected_epoch {
            return None;
        }
        if player.playback_state() == PlaybackState::Stopped {
            return Some(expected_epoch);
        }
        if tokio::time::Instant::now() >= deadline {
            warn!(
                event = "apple_music_native_eof_drain_timeout",
                zone_id,
                expected_epoch,
                "Captured Apple Music tail did not finish draining before the next provider handoff"
            );
            player.stop();
            return Some(player.playback_epoch());
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
    if state
        .zones()
        .player_for_zone(&zone_id)
        .is_none_or(|player| player.playback_epoch() != expected_epoch)
    {
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
        if let Some(player) = state.zones().player_for_zone(&zone_id)
            && player.playback_epoch() == expected_epoch
        {
            player.stop();
        }
        state.listening().stop(state.library(), &zone_id);
    } else {
        debug!(
            event = "apple_music_native_queue_advance",
            zone_id, reason, source_key, "Routed the next mixed-provider queue entry"
        );
    }
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
        | "musickit_capability_unavailable"
        | "process_tap_confirmation_required" => PlaybackError::forbidden(error.message),
        "song_not_found" | "album_not_found" | "helper_missing" => {
            PlaybackError::not_found(error.message)
        }
        "process_tap_playback_changed" | "apple_music_active_segment_requires_reprepare" => {
            PlaybackError::conflict(error.message)
        }
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
    fn catalog_identity_requires_album_for_exact_native_selection() {
        let mut source = apple_source(Some("Track"), Some("Artist"));
        if let SourceRef::AppleMusicTrack { album_id, .. } = &mut source {
            *album_id = None;
        }
        assert!(catalog_identity(&source).is_err());
    }
}
