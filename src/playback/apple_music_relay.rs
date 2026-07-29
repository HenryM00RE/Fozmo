//! Apple Music through an output that is not this Mac.
//!
//! The audio plane is unchanged: Music.app decodes the catalog track into
//! Fozmo Capture exactly as it does for a local output. What changes is who
//! consumes that PCM. Instead of the local Player, DSP, and a CoreAudio
//! device, the capture ring is drained by an HTTP response that the zone's own
//! agent — a Windows or macOS agent, or a browser — pulls and renders through
//! its own DSP and hardware. That keeps a relayed zone identical to every
//! other source it plays: it fetches bytes from Fozmo and does its own signal
//! path, rather than receiving audio Fozmo has already processed for someone
//! else's hardware.
//!
//! Exactly one zone can do this at a time. Fozmo captures Music.app by taking
//! over the Mac's default output, so there is one Apple stream in existence —
//! not one per listener. [`PlaybackRouter`] refuses a second zone before
//! anything here runs, and the reader itself is single-take so a stray fetch
//! cannot split one stream's frames across two consumers.
//!
//! Music.app still advances a safe island by itself, and the relay carries that
//! straight through: an island's internal track changes never interrupt the
//! byte stream, so an album plays gaplessly on a remote output too. Only the
//! end of an island closes the stream, which the remote agent sees as an
//! ordinary end of track.

use crate::app::state::AppState;
use crate::diagnostics::logging::sanitize_error;
use crate::playback::apple_music_native::{
    MUSIC_NOTIFICATION_FALLBACK, MUSIC_STOP_CONFIRMATION_DELAY,
    MUSIC_TRACK_CHANGE_CONFIRMATION_DELAY, TRACK_ACTIVATION_POLL, TRACK_ACTIVATION_TIMEOUT,
    apple_music_startup_prefill_secs, catalog_identity, hydrate_catalog_source,
    music_status_blocking, music_track_definitively_differs, native_track_completed,
    pause_music_blocking, play_music_blocking, play_queue_playlist_blocking, playback_error,
    projected_music_position, promote_gapless_listening_boundary, queue_playlist_song_ids,
    restart_queue_playlist_from_start, schedule_queue_cleanup, set_music_position_blocking,
    stage_queue_generation_blocking, wait_for_music_notification_blocking, zone_queue_sources,
};
use crate::playback::error::PlaybackError;
use crate::playback::intent::{PlaybackGuard, PlaybackIntent, PlaybackOutcome};
use crate::playback::router::PlaybackRouter;
use crate::playback::service::playback_config_for_zone;
use crate::protocol::{CoreToAgentCommand, SourceRef};
use crate::services::apple_music::{AppleMusicDelivery, AppleMusicPlaybackSnapshot};
use crate::services::apple_music_musickit::MusicAppSnapshot;
use std::time::{Duration, SystemTime};
use tracing::{debug, info, warn};

/// How long the remote zone has to open the live stream before the start is
/// treated as failed. A zone that never fetches would otherwise leave Music.app
/// playing into a ring nobody drains.
const RELAY_STREAM_CLAIM_TIMEOUT: Duration = Duration::from_secs(12);
const RELAY_STREAM_CLAIM_POLL: Duration = Duration::from_millis(100);
/// Ceiling on the lead Fozmo builds before handing the stream over. The ring
/// itself is far larger, but every buffered second is a second the remote
/// output lags Music.app's transport.
const RELAY_PREFILL_MAX_SECS: f64 = 2.0;
const RELAY_PREFILL_POLL: Duration = Duration::from_millis(20);

/// The Apple Music session for `zone_id`, when that zone is the one being
/// relayed.
pub(crate) fn active_relay_snapshot(
    state: &AppState,
    zone_id: &str,
) -> Option<AppleMusicPlaybackSnapshot> {
    state
        .apple_music_playback()
        .playback_snapshot_for_zone(zone_id)
        .filter(|snapshot| snapshot.delivery == AppleMusicDelivery::Relay)
        .filter(|_| state.apple_music_playback().relay_running())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn play_apple_music_source_relayed(
    state: &AppState,
    zone_id: &str,
    profile_id: String,
    source: SourceRef,
    queue: Vec<SourceRef>,
    radio_auto: bool,
    guard: PlaybackGuard,
    startup_id: Option<String>,
) -> Result<PlaybackOutcome, PlaybackError> {
    let source = hydrate_catalog_source(state, source).await?;
    if let Some(startup_id) = startup_id.as_deref() {
        state
            .apple_music()
            .mark_startup_phase(startup_id, "catalog_resolution");
    }
    let (song_id, storefront, album_id) = catalog_identity(&source)?;
    ensure_guard_current(state, &guard)?;

    let _playback_switch = state.apple_music().lock_playback_switch().await;
    ensure_guard_current(state, &guard)?;
    pause_music_blocking().await?;
    if state.apple_music_playback().capture_running() {
        state.apple_music_playback().stop_runtime(false);
    }
    ensure_guard_current(state, &guard)?;

    let settings = state.settings().apple_music_playback_settings();
    let playlist_song_ids = queue_playlist_song_ids(state, &source, &queue, None);
    let stage_playlist =
        stage_queue_generation_blocking(state, playlist_song_ids, startup_id.as_deref(), zone_id);
    let (playlist_target, ()) =
        tokio::try_join!(stage_playlist, async { prepare_music_for_relay().await })?;
    let playlist_keys = playlist_target.database_ids.clone();
    state
        .apple_music()
        .prepare_music_app_decoder_observer()
        .await
        .map_err(playback_error)?;
    if let Some(startup_id) = startup_id.as_deref() {
        state
            .apple_music()
            .mark_startup_phase(startup_id, "parallel_preparation");
    }
    ensure_guard_current(state, &guard)?;

    let cached_verified_format = cached_verified_format(state, &song_id, storefront.as_deref());
    // Capture opens gated shut, so Music.app's catalog navigation and its
    // restored global position cannot reach the listener even though the
    // stream is technically live from this point.
    let session_rate_hz = state
        .apple_music_playback()
        .start_relay_capture(&settings, cached_verified_format)
        .map_err(PlaybackError::integration)?;
    let playback = state
        .apple_music_playback()
        .activate_playback_with_delivery(
            zone_id.to_string(),
            AppleMusicDelivery::Relay,
            0,
            source.clone(),
        );
    state
        .apple_music()
        .mark_current_queue_active(zone_id)
        .await
        .map_err(playback_error)?;

    let start_result = async {
        let expected_key = playlist_keys.first().cloned();
        let format_boundary = SystemTime::now();
        play_queue_playlist_blocking(&playlist_target).await?;
        let selected =
            wait_for_relayed_track(state, &playback, expected_key.as_deref()).await?;
        let source_format = state
            .apple_music()
            .probe_music_app_source_format(format_boundary)
            .await
            .map_err(playback_error)?
            .ok_or_else(|| {
                PlaybackError::integration(
                    "Music.app did not expose a fresh Apple Lossless decoder format. Fozmo did not release unverified audio to the remote output.",
                )
            })?;
        ensure_owned(state, &guard, &playback)?;

        // The relay has no consumer yet, so a rate correction here costs
        // nothing: the session is simply reopened at the verified rate before
        // any byte reaches the remote zone.
        if source_format.sample_rate_hz != session_rate_hz {
            debug!(
                event = "apple_music_relay_rate_corrected",
                zone_id,
                song_id,
                session_rate_hz,
                verified_rate_hz = source_format.sample_rate_hz,
                "Reopening the relayed capture session at Music.app's verified rate"
            );
            let reopened_rate_hz = state
                .apple_music_playback()
                .restart_relay_capture(
                    playback.generation,
                    &settings,
                    Some((
                        source_format.sample_rate_hz,
                        source_format.source_bit_depth_bits,
                    )),
                )
                .map_err(PlaybackError::integration)?;
            // Capture running at anything but the decoder's own rate means
            // macOS is resampling Music.app on the way in, which is exactly
            // what this path exists to avoid.
            if reopened_rate_hz != source_format.sample_rate_hz {
                return Err(PlaybackError::integration(format!(
                    "Fozmo Capture stayed at {reopened_rate_hz} Hz for a {} Hz Apple Lossless decoder, so the stream would not be bit-exact.",
                    source_format.sample_rate_hz
                )));
            }
        }

        // Re-enter the playlist so the stream the remote zone receives starts
        // at the track's true beginning rather than wherever verification left
        // the transport.
        pause_music_blocking().await?;
        ensure_owned(state, &guard, &playback)?;
        state.apple_music_playback().set_relay_gate_open(false);
        restart_queue_playlist_from_start(state, &source, expected_key.as_deref()).await?;
        ensure_owned(state, &guard, &playback)?;
        state.apple_music_playback().set_relay_gate_open(true);

        let prefill_target_secs =
            apple_music_startup_prefill_secs(settings.startup_prefill_ms, settings.buffer_ms)
                .min(RELAY_PREFILL_MAX_SECS);
        wait_for_relay_prefill(state, &playback, prefill_target_secs).await?;
        if let Some(startup_id) = startup_id.as_deref() {
            state
                .apple_music()
                .mark_startup_phase(startup_id, "capture_routing_and_prefill");
        }
        ensure_owned(state, &guard, &playback)?;

        send_relay_play_command(state, zone_id, &source)?;
        wait_for_relay_stream_claimed(state, &playback).await?;
        if let Some(startup_id) = startup_id.as_deref() {
            state
                .apple_music()
                .mark_startup_phase(startup_id, "output_opening");
            state.apple_music().complete_startup(startup_id, true);
        }

        state.apple_music_playback().update_playback(
            playback.generation,
            "playing",
            Some(0.0),
            selected.track.duration_secs,
        );
        Ok::<(u32, Option<u32>), PlaybackError>((
            source_format.sample_rate_hz,
            source_format.source_bit_depth_bits,
        ))
    }
    .await;

    let (verified_rate_hz, verified_bits) = match start_result {
        Ok(format) => format,
        Err(error) => {
            state.apple_music_playback().stop_runtime(false);
            let _ = state
                .zones()
                .send_to_zone(zone_id, CoreToAgentCommand::Stop);
            let _ = state.apple_music().release_queue_owner(zone_id).await;
            schedule_queue_cleanup(state);
            if let Some(startup_id) = startup_id.as_deref() {
                state.apple_music().complete_startup(startup_id, false);
            }
            return Err(error);
        }
    };

    let format_context = state.apple_music().format_context_fingerprint();
    if let Err(error) = state.library().record_apple_music_track_format_in_context(
        &song_id,
        album_id.as_deref(),
        storefront.as_deref(),
        "ALAC",
        verified_rate_hz,
        verified_bits,
        Some(&format_context),
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
    info!(
        event = "apple_music_relayed_lossless_started",
        zone_id,
        song_id,
        album_id,
        source_rate_hz = verified_rate_hz,
        source_bits = verified_bits.unwrap_or_default(),
        "Music.app lossless capture is streaming to a remote output"
    );
    spawn_relay_monitor(state.clone(), zone_id.to_string(), playback.generation);
    Ok(PlaybackOutcome::Completed)
}

pub(crate) async fn pause(state: &AppState, zone_id: &str) -> Result<bool, PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    let Some(snapshot) = active_relay_snapshot(state, zone_id) else {
        return Ok(false);
    };
    pause_music_blocking().await?;
    // Nothing may enter the ring while the transport is parked: the stream is
    // a bare byte sequence, so system alert audio landing in it would be heard
    // as part of the track when playback resumes.
    state.apple_music_playback().set_relay_gate_open(false);
    state
        .zones()
        .send_to_zone(zone_id, CoreToAgentCommand::Pause)
        .map_err(PlaybackError::integration)?;
    if !state.apple_music_playback().pause_playback_at(
        snapshot.generation,
        relayed_audible_position(state, zone_id, &snapshot),
    ) {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    state
        .apple_music()
        .mark_current_queue_paused(zone_id)
        .await
        .map_err(playback_error)?;
    Ok(true)
}

pub(crate) async fn resume(state: &AppState, zone_id: &str) -> Result<bool, PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    let Some(snapshot) = active_relay_snapshot(state, zone_id) else {
        return Ok(false);
    };
    state.apple_music_playback().set_relay_gate_open(true);
    play_music_blocking().await?;
    state
        .zones()
        .send_to_zone(zone_id, CoreToAgentCommand::Resume)
        .map_err(PlaybackError::integration)?;
    state
        .apple_music_playback()
        .update_playback(snapshot.generation, "playing", None, None);
    state
        .apple_music()
        .mark_current_queue_active(zone_id)
        .await
        .map_err(playback_error)?;
    Ok(true)
}

/// Seek by restarting the stream at the requested position.
///
/// A live capture ring has no history to rewind through, so the only honest
/// seek is a new session: park the transport, reopen capture so no pre-seek
/// PCM survives, and hand the remote zone a fresh stream that begins at the
/// new position.
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
    let Some(snapshot) = active_relay_snapshot(state, zone_id) else {
        return Ok(false);
    };
    let settings = state.settings().apple_music_playback_settings();
    let verified_format = state.apple_music_playback().session_format();
    pause_music_blocking().await?;
    state
        .zones()
        .send_to_zone(zone_id, CoreToAgentCommand::Stop)
        .map_err(PlaybackError::integration)?;
    state
        .apple_music_playback()
        .restart_relay_capture(snapshot.generation, &settings, verified_format)
        .map_err(PlaybackError::integration)?;
    if !state
        .apple_music_playback()
        .set_timeline_origin(snapshot.generation, seconds)
    {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    set_music_position_blocking(seconds).await?;
    play_music_blocking().await?;
    state.apple_music_playback().set_relay_gate_open(true);
    wait_for_relay_prefill(
        state,
        &snapshot,
        apple_music_startup_prefill_secs(settings.startup_prefill_ms, settings.buffer_ms)
            .min(RELAY_PREFILL_MAX_SECS),
    )
    .await?;
    send_relay_play_command(state, zone_id, &snapshot.source)?;
    wait_for_relay_stream_claimed(state, &snapshot).await?;
    state.apple_music_playback().update_playback(
        snapshot.generation,
        "playing",
        Some(seconds),
        None,
    );
    Ok(true)
}

pub(crate) async fn stop(state: &AppState, zone_id: &str) -> Result<bool, PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    if active_relay_snapshot(state, zone_id).is_none() {
        return Ok(false);
    }
    let _ = pause_music_blocking().await;
    state.apple_music_playback().stop_runtime(false);
    let _ = state
        .zones()
        .send_to_zone(zone_id, CoreToAgentCommand::Stop);
    state
        .apple_music()
        .release_queue_owner(zone_id)
        .await
        .map_err(playback_error)?;
    schedule_queue_cleanup(state);
    Ok(true)
}

pub(crate) async fn next(state: &AppState, zone_id: &str) -> Result<bool, PlaybackError> {
    let playback_switch = state.apple_music().lock_playback_switch().await;
    if active_relay_snapshot(state, zone_id).is_none() {
        return Ok(false);
    }
    let queue = zone_queue_sources(state, zone_id);
    let profile_id = state
        .listening()
        .profile_id(zone_id)
        .unwrap_or_else(|| crate::settings::DEFAULT_PROFILE_ID.to_string());
    let _ = pause_music_blocking().await;
    state.apple_music_playback().stop_runtime(false);
    state
        .apple_music()
        .release_queue_owner(zone_id)
        .await
        .map_err(playback_error)?;
    schedule_queue_cleanup(state);
    let Some((next, rest)) = queue.split_first() else {
        let _ = state
            .zones()
            .send_to_zone(zone_id, CoreToAgentCommand::Stop);
        state.listening().stop(state.library(), zone_id);
        return Ok(true);
    };
    let next = next.clone();
    let rest = rest.to_vec();
    drop(playback_switch);
    route_after_relay_boundary(
        state.clone(),
        zone_id.to_string(),
        profile_id,
        next,
        rest,
        "manual_next",
    )
    .await;
    Ok(true)
}

/// Tear down a relayed session that a non-Apple command has replaced.
pub(crate) async fn stop_replaced_relay_if_current(
    state: &AppState,
    zone_id: &str,
    guard: &PlaybackGuard,
) -> Result<(), PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    ensure_guard_current(state, guard)?;
    if active_relay_snapshot(state, zone_id).is_none() {
        return Ok(());
    }
    let _ = pause_music_blocking().await;
    state.apple_music_playback().stop_runtime(false);
    state
        .apple_music()
        .release_queue_owner(zone_id)
        .await
        .map_err(playback_error)?;
    schedule_queue_cleanup(state);
    Ok(())
}

/// Listener-facing position for a relayed session.
///
/// The remote agent's own timeline is the audible one: its stream began at the
/// track's first frame, so its position is the position being heard, offset by
/// whatever earlier tracks of the same island it has already played.
fn relayed_audible_position(
    state: &AppState,
    zone_id: &str,
    snapshot: &AppleMusicPlaybackSnapshot,
) -> f64 {
    let agent_position_secs = state
        .zones()
        .remote_snapshot_for_zone(zone_id)
        .and_then(|remote| remote.playback)
        .map(|playback| playback.position_secs)
        .unwrap_or(0.0);
    snapshot.audible_position_secs(agent_position_secs)
}

async fn prepare_music_for_relay() -> Result<(), PlaybackError> {
    crate::playback::apple_music_native::prepare_music_blocking().await
}

fn cached_verified_format(
    state: &AppState,
    song_id: &str,
    storefront: Option<&str>,
) -> Option<(u32, Option<u32>)> {
    let format_context = state.apple_music().format_context_fingerprint();
    state
        .library()
        .apple_music_track_verified_format_in_context(song_id, storefront, &format_context)
        .ok()
        .flatten()
        .filter(|format| format.codec.eq_ignore_ascii_case("ALAC"))
        .and_then(|format| {
            Some((
                u32::try_from(format.sample_rate).ok()?,
                format.bit_depth.and_then(|bits| u32::try_from(bits).ok()),
            ))
        })
}

fn send_relay_play_command(
    state: &AppState,
    zone_id: &str,
    source: &SourceRef,
) -> Result<(), PlaybackError> {
    let player = state.zones().active_player();
    state
        .zones()
        .send_to_zone(
            zone_id,
            CoreToAgentCommand::PlaySource {
                source_ref: source.clone(),
                // Deliberately empty. A relayed queue must not be prefetched:
                // the successor's audio does not exist as a fetchable resource
                // until Fozmo hands Music.app the next island, and a second
                // fetch of the live stream would split one stream's frames
                // between two readers.
                queue: Vec::new(),
                playback_config: playback_config_for_zone(state, zone_id, &player),
                stream_base_url: state.public_base_url().clone(),
            },
        )
        .map_err(PlaybackError::integration)
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
    state
        .apple_music_playback()
        .playback_snapshot()
        .filter(|current| current.generation == expected.generation)
        .map(|_| ())
        .ok_or_else(|| PlaybackError::conflict("Playback changed"))
}

/// Wait for Music.app to actually be playing the track Fozmo selected.
async fn wait_for_relayed_track(
    state: &AppState,
    playback: &AppleMusicPlaybackSnapshot,
    expected_key: Option<&str>,
) -> Result<MusicAppSnapshot, PlaybackError> {
    let deadline = tokio::time::Instant::now() + TRACK_ACTIVATION_TIMEOUT;
    loop {
        if state
            .apple_music_playback()
            .playback_snapshot()
            .is_none_or(|current| current.generation != playback.generation)
        {
            return Err(PlaybackError::conflict("Playback changed"));
        }
        let snapshot = music_status_blocking().await?;
        if snapshot.has_current_track()
            && crate::playback::apple_music_native::music_track_matches_expected(
                &snapshot,
                &playback.source,
                expected_key,
            )
        {
            return Ok(snapshot);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(PlaybackError::integration(
                "Music.app did not start the selected catalog track for the remote output.",
            ));
        }
        tokio::time::sleep(TRACK_ACTIVATION_POLL).await;
    }
}

/// Build a small lead before the remote zone starts pulling, so its first
/// seconds do not depend on Music.app filling the ring in real time.
async fn wait_for_relay_prefill(
    state: &AppState,
    playback: &AppleMusicPlaybackSnapshot,
    target_secs: f64,
) -> Result<(), PlaybackError> {
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs_f64((target_secs + 2.0).max(2.0));
    loop {
        if state
            .apple_music_playback()
            .playback_snapshot()
            .is_none_or(|current| current.generation != playback.generation)
        {
            return Err(PlaybackError::conflict("Playback changed"));
        }
        let buffered = state
            .apple_music_playback()
            .relay_buffered_audio_secs()
            .ok_or_else(|| {
                PlaybackError::integration("The Apple Music relay closed before it filled.")
            })?;
        if buffered >= target_secs {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            // A short lead is not fatal — the remote zone buffers too — so this
            // proceeds rather than failing a start that would have worked.
            warn!(
                event = "apple_music_relay_prefill_short",
                zone_id = playback.zone_id,
                buffered_secs = buffered,
                target_secs,
                "Handing the Apple Music relay over with less lead than requested"
            );
            return Ok(());
        }
        tokio::time::sleep(RELAY_PREFILL_POLL).await;
    }
}

async fn wait_for_relay_stream_claimed(
    state: &AppState,
    playback: &AppleMusicPlaybackSnapshot,
) -> Result<(), PlaybackError> {
    let deadline = tokio::time::Instant::now() + RELAY_STREAM_CLAIM_TIMEOUT;
    loop {
        if state
            .apple_music_playback()
            .playback_snapshot()
            .is_none_or(|current| current.generation != playback.generation)
        {
            return Err(PlaybackError::conflict("Playback changed"));
        }
        if state.apple_music_playback().relay_stream_claimed() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(PlaybackError::integration(
                "The selected output did not open Fozmo's Apple Music stream.",
            ));
        }
        tokio::time::sleep(RELAY_STREAM_CLAIM_POLL).await;
    }
}

/// Track Music.app for a relayed session.
///
/// This mirrors the local monitor's job without a Player to consult: Music.app
/// is the only transport, and the remote zone reports the audible timeline.
fn spawn_relay_monitor(state: AppState, zone_id: String, generation: u64) {
    tokio::spawn(async move {
        let mut last_position = 0.0_f64;
        let mut last_duration = 0.0_f64;
        let mut consecutive_errors = 0_u8;
        let mut first_snapshot = true;
        let mut last_playing_observed_at: Option<tokio::time::Instant> = None;
        loop {
            if first_snapshot {
                first_snapshot = false;
            } else {
                wait_for_music_notification_blocking(MUSIC_NOTIFICATION_FALLBACK).await;
            }
            let Some(snapshot) = active_relay_snapshot(&state, &zone_id) else {
                break;
            };
            if snapshot.generation != generation {
                break;
            }
            let mut music = match music_status_blocking().await {
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
                        event = "apple_music_relay_monitor_failed",
                        zone_id,
                        error = %sanitize_error(&error.to_string()),
                        "Music.app monitoring failed for the relayed session"
                    );
                    finish_relayed_playback(
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
                finish_relayed_playback(
                    state.clone(),
                    zone_id.clone(),
                    generation,
                    false,
                    "music_app_quit",
                )
                .await;
                break;
            }
            if music_track_definitively_differs(&music, &snapshot.source) {
                tokio::time::sleep(MUSIC_TRACK_CHANGE_CONFIRMATION_DELAY).await;
                let Ok(confirmed) = music_status_blocking().await else {
                    continue;
                };
                if !music_track_definitively_differs(&confirmed, &snapshot.source) {
                    continue;
                }
                music = confirmed;
                let duration = if last_duration > 0.0 {
                    last_duration
                } else {
                    snapshot.duration_secs
                };
                let completion_position = projected_music_position(
                    last_position,
                    last_playing_observed_at
                        .map(|observed| tokio::time::Instant::now().duration_since(observed)),
                );
                let completed = native_track_completed(completion_position, duration);
                // Music.app advanced inside the island it owns. The byte
                // stream never stopped, so the remote output hears a gapless
                // boundary and only Fozmo's identity has to move with it.
                if completed
                    && let Some(next) =
                        promote_relayed_boundary(&state, &zone_id, generation, &snapshot, &music)
                {
                    last_position = music.track.position_secs.unwrap_or(0.0);
                    last_duration = music
                        .track
                        .duration_secs
                        .or_else(|| next.duration_secs())
                        .unwrap_or(0.0);
                    last_playing_observed_at = Some(tokio::time::Instant::now());
                    continue;
                }
                warn!(
                    event = "apple_music_relay_track_interrupted",
                    zone_id,
                    expected = snapshot.source.key(),
                    completed,
                    "Music.app changed away from the track Fozmo was relaying"
                );
                if completed {
                    let _ = pause_music_blocking().await;
                }
                finish_relayed_playback(
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
                    last_playing_observed_at = Some(tokio::time::Instant::now());
                    state.apple_music_playback().update_playback(
                        generation,
                        "playing",
                        music.track.position_secs,
                        music.track.duration_secs,
                    );
                }
                Some("paused") => {
                    last_playing_observed_at = None;
                    state.apple_music_playback().update_playback(
                        generation,
                        "paused",
                        music.track.position_secs,
                        music.track.duration_secs,
                    );
                }
                Some("stopped") => {
                    // Music.app publishes a transient stop while a pause
                    // settles, so a listener-initiated pause must not be read
                    // as the end of the track.
                    tokio::time::sleep(MUSIC_STOP_CONFIRMATION_DELAY).await;
                    let fozmo_paused = active_relay_snapshot(&state, &zone_id)
                        .filter(|current| current.generation == generation)
                        .is_some_and(|current| current.playback_state == "paused");
                    if fozmo_paused {
                        last_playing_observed_at = None;
                        continue;
                    }
                    let Ok(confirmed) = music_status_blocking().await else {
                        continue;
                    };
                    if confirmed.player_state.as_deref() != Some("stopped") {
                        continue;
                    }
                    let duration = if last_duration > 0.0 {
                        last_duration
                    } else {
                        snapshot.duration_secs
                    };
                    let completion_position = projected_music_position(
                        last_position,
                        last_playing_observed_at
                            .map(|observed| tokio::time::Instant::now().duration_since(observed)),
                    );
                    let completed = native_track_completed(completion_position, duration);
                    info!(
                        event = "apple_music_relay_terminal_state",
                        zone_id,
                        generation,
                        last_position_secs = last_position,
                        duration_secs = duration,
                        completed,
                        "Observed Music.app's terminal state for the relayed session"
                    );
                    state.apple_music_playback().update_playback(
                        generation,
                        "stopped",
                        Some(last_position),
                        Some(duration),
                    );
                    finish_relayed_playback(
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

/// Move Fozmo's identity onto the island's next track without touching the
/// stream the remote output is playing.
fn promote_relayed_boundary(
    state: &AppState,
    zone_id: &str,
    generation: u64,
    previous: &AppleMusicPlaybackSnapshot,
    music: &MusicAppSnapshot,
) -> Option<SourceRef> {
    let queue = zone_queue_sources(state, zone_id);
    let next = queue.first()?.clone();
    if !crate::playback::apple_music_native::music_track_matches_expected(music, &next, None) {
        return None;
    }
    let agent_position_secs = state
        .zones()
        .remote_snapshot_for_zone(zone_id)
        .and_then(|remote| remote.playback)
        .map(|playback| playback.position_secs)
        .unwrap_or(0.0);
    if !state.apple_music_playback().promote_continuous_playback(
        generation,
        next.clone(),
        agent_position_secs,
        music.track.position_secs,
        music.track.duration_secs,
    ) {
        return None;
    }
    promote_gapless_listening_boundary(state, zone_id, &previous.source, &next);
    debug!(
        event = "apple_music_relay_gapless_boundary",
        zone_id,
        next = next.key(),
        "Music.app advanced its island while the relayed stream continued"
    );
    Some(next)
}

async fn finish_relayed_playback(
    state: AppState,
    zone_id: String,
    generation: u64,
    completed: bool,
    reason: &'static str,
) {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    let Some(snapshot) = active_relay_snapshot(&state, &zone_id) else {
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
    state.apple_music_playback().stop_runtime(false);
    if let Err(error) = state.apple_music().release_queue_owner(&zone_id).await {
        debug!(
            event = "apple_music_relay_queue_owner_release_failed",
            zone_id,
            error = %error.message,
            "The relayed session's queue playlist reference will be reclaimed by reconciliation"
        );
    }
    schedule_queue_cleanup(&state);

    let next = completed.then(|| queue.split_first()).flatten();
    let Some((next, rest)) = next else {
        let _ = state
            .zones()
            .send_to_zone(&zone_id, CoreToAgentCommand::Stop);
        state.listening().stop(state.library(), &zone_id);
        return;
    };
    let next = next.clone();
    let rest = rest.to_vec();
    promote_gapless_listening_boundary(&state, &zone_id, &snapshot.source, &next);
    drop(_playback_switch);
    route_after_relay_boundary(state, zone_id, profile_id, next, rest, reason).await;
}

async fn route_after_relay_boundary(
    state: AppState,
    zone_id: String,
    profile_id: String,
    source: SourceRef,
    queue: Vec<SourceRef>,
    reason: &'static str,
) {
    let source_key = source.key();
    let result = Box::pin(PlaybackRouter::new(&state).execute(
        &zone_id,
        PlaybackIntent::Play {
            profile_id,
            radio_auto: source.is_radio(),
            source,
            queue,
            guard: PlaybackGuard::none(),
            qobuz_request: None,
            startup_id: None,
        },
    ))
    .await;
    if let Err(error) = result {
        warn!(
            event = "apple_music_relay_queue_advance_failed",
            zone_id,
            reason,
            source_key,
            error = %error,
            "Could not route the next queue entry for the relayed zone"
        );
        let _ = state
            .zones()
            .send_to_zone(&zone_id, CoreToAgentCommand::Stop);
        state.listening().stop(state.library(), &zone_id);
    } else {
        debug!(
            event = "apple_music_relay_queue_advance",
            zone_id, reason, source_key, "Routed the next queue entry for the relayed zone"
        );
    }
}
