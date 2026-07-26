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
use crate::playback::status::StatusResponse;
use crate::protocol::{SinkProtocol, SourceRef};
use crate::services::apple_music::{
    APPLE_MUSIC_LIVE_DISPLAY_NAME, AppleMusicPlaybackSnapshot, PreparedAppleMusicControl,
};
use crate::services::apple_music_musickit::{
    AppleMusicMvpError, MusicAppSnapshot, delete_queue_playlist, music_app_status, pause_music_app,
    play_music_app, play_queue_playlist, prepare_music_app, queue_playlist_track_count,
    queue_playlist_track_keys, set_music_app_position, wait_for_music_app_notification,
};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tracing::{debug, info, warn};

const TRACK_ACTIVATION_TIMEOUT: Duration = Duration::from_secs(10);
const TRACK_ACTIVATION_POLL: Duration = Duration::from_millis(75);
const TRACK_TRANSPORT_SETTLE_TIMEOUT: Duration = Duration::from_secs(3);
const TRACK_START_POSITION_TOLERANCE_SECS: f64 = 1.0;
const TRACK_PARK_POSITION_TOLERANCE_SECS: f64 = 0.250;
const START_PREFILL_MIN_SECS: f64 = 0.250;
const RESUME_PREFILL_TARGET_SECS: f64 = 0.080;
const RESUME_PREFILL_MIN_SECS: f64 = 0.030;
const RESUME_PREFILL_TIMEOUT: Duration = Duration::from_millis(1_000);
const MUSIC_NOTIFICATION_FALLBACK: Duration = Duration::from_secs(2);
const STARTUP_TRANSPORT_STATUS_POLL: Duration = Duration::from_millis(350);
const STARTUP_TRANSPORT_RECOVERY_TIMEOUT: Duration = Duration::from_secs(3);
const STARTUP_STALL_POSITION_MAX_SECS: f64 = 1.0;
const PLAYER_PAUSE_TIMEOUT: Duration = Duration::from_secs(4);
const PLAYER_OUTPUT_START_TIMEOUT: Duration = Duration::from_secs(25);
const PLAYER_EOF_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const PLAYER_AUTO_ADVANCE_START_GRACE: Duration = Duration::from_millis(750);
/// Music.app has to import each queued catalog song into the library before it
/// can appear in the playlist, and Apple gives no completion signal, so the
/// only option is a bounded poll.
const QUEUE_PLAYLIST_VISIBLE_TIMEOUT: Duration = Duration::from_secs(8);
const QUEUE_PLAYLIST_VISIBLE_POLL: Duration = Duration::from_millis(100);
/// Matches the helper's own cap. A longer queue only delays the start, since
/// Fozmo re-syncs whenever the upcoming run changes.
const MAX_QUEUE_PLAYLIST_TRACKS: usize = 100;
const CROSS_PROVIDER_PREWARM_SECS: f64 = 8.0;
const CROSS_PROVIDER_HANDOFF_WINDOW_SECS: f64 = 1.5;
const CROSS_PROVIDER_ENDPOINT_TOLERANCE_SECS: f64 = 0.100;

fn apple_music_boundary_lead_secs(configured: f64) -> f64 {
    if configured.is_finite() {
        configured.clamp(0.5, 5.0)
    } else {
        2.0
    }
}

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
    let prepared_control = state
        .apple_music_playback()
        .take_prepared_control(&source.key());
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

    // Build Music.app's queue before opening capture. This is a network round
    // trip plus a library import, and it must not run with the capture route
    // already stolen from the DAC. The run holds this track and everything
    // Music.app can advance to on its own.
    let playlist_song_ids = queue_playlist_song_ids(state, &source, &queue, None);
    let playlist_keys = sync_queue_playlist_blocking(state, playlist_song_ids.clone()).await?;
    ensure_guard_current(state, &guard)?;
    info!(
        event = "apple_music_queue_playlist_synced",
        zone_id,
        song_id,
        playlist = crate::services::apple_music_musickit::QUEUE_PLAYLIST_NAME,
        tracks = playlist_keys.len(),
        "Music.app is holding Fozmo's queue playlist"
    );
    let cached_verified_format = prepared_control
        .as_ref()
        .map(|prepared| (prepared.rate_hz, prepared.source_bit_depth))
        .or_else(|| {
            state
                .library()
                .apple_music_track_verified_format(&song_id)
                .ok()
                .flatten()
                .filter(|format| format.codec.eq_ignore_ascii_case("ALAC"))
                .and_then(|format| {
                    Some((
                        u32::try_from(format.sample_rate).ok()?,
                        format.bit_depth.and_then(|bits| u32::try_from(bits).ok()),
                    ))
                })
        });
    let initial_epoch = match cached_verified_format {
        Some((rate_hz, source_bits)) => state
            .apple_music_playback()
            .start_playback_capture_at_format(player.clone(), &settings, rate_hz, source_bits),
        None => state
            .apple_music_playback()
            .start_playback_capture(player.clone(), &settings),
    }
    .map_err(PlaybackError::integration)?;
    // The ring starts empty and closed. Catalog navigation and Music.app's
    // restored global position cannot leak into the listener's timeline.
    if !state.apple_music_playback().set_capture_gate_open(false) {
        return Err(PlaybackError::integration(
            "Apple Music capture started without a controllable input gate.",
        ));
    }
    let playback = state.apple_music_playback().activate_playback(
        zone_id.to_string(),
        initial_epoch,
        source.clone(),
    );
    let start_result = async {
        hold_player_paused(&player, initial_epoch).await?;
        ensure_owned(state, &guard, &playback)?;
        let format_boundary = SystemTime::now();
        if let Some(prepared) = prepared_control.as_ref() {
            debug!(
                event = "apple_music_prepared_control_fallback",
                song_id,
                rate_hz = prepared.rate_hz,
                "The prepared boundary missed its seamless endpoint; reusing its verified format"
            );
        }
        // Entering the playlist container is what makes Music.app advance the
        // rest of the run internally, which is the only gapless Apple-to-Apple
        // boundary available. It also replaces the Accessibility click: no
        // foregrounding, no synthetic input, and a deterministic start at 0:00.
        let expected_key = playlist_keys.first().cloned();
        let selection = async {
            play_queue_playlist_blocking().await?;
            let selected = wait_for_selected_track(
                state,
                &guard,
                &playback,
                expected_key.as_deref(),
            )
            .await?;
            Ok::<MusicAppSnapshot, PlaybackError>(selected)
        };
        let apple_music = state.apple_music();
        // A cached per-song format removes `log show` from the audible start
        // path. Keep the verification query running concurrently and inspect
        // it only after Player output is flowing.
        let cached_probe = cached_verified_format.is_some().then(|| {
            let probe_state = state.clone();
            tokio::spawn(async move {
                probe_state
                    .apple_music()
                    .probe_music_app_source_format(format_boundary)
                    .await
            })
        });
        let (selected, mut source_rate_hz, mut source_bits) =
            if let Some((rate_hz, bits)) = cached_verified_format {
                (selection.await?, rate_hz, bits)
            } else {
                let (selected, source_format) = tokio::join!(
                    selection,
                    apple_music.probe_music_app_source_format(format_boundary)
                );
                let source_format = source_format
                    .map_err(playback_error)?
                    .ok_or_else(|| {
                        PlaybackError::integration(
                            "Music.app did not expose a fresh Apple Lossless decoder format. Fozmo kept the Hegel muted and did not release unverified audio to the DSP.",
                        )
                    })?;
                (
                    selected?,
                    source_format.sample_rate_hz,
                    source_format.source_bit_depth_bits,
                )
            };
        // Entering the playlist already established the transport at 0:00, so
        // there is no single-track dance to run here. Pause only so the capture
        // session can be rebuilt without Music.app decoding into a ring that is
        // about to be discarded.
        pause_music_blocking().await?;
        ensure_owned(state, &guard, &playback)?;

        // This is a no-op when the live session already has the verified rate.
        // A rate change rebuilds only the capture/Player source; the system
        // output route remains on Fozmo Capture.
        let mut verified_epoch = state
            .apple_music_playback()
            .restart_at_verified_source_format(source_rate_hz, source_bits)
            .map_err(PlaybackError::integration)?;
        if !state
            .apple_music_playback()
            .replace_player_epoch(playback.generation, verified_epoch)
        {
            return Err(PlaybackError::conflict("Playback changed"));
        }
        state
            .apple_music_playback()
            .set_capture_gate_open(false);
        hold_player_paused(&player, verified_epoch).await?;
        player.flush_live_output();
        ensure_owned(state, &guard, &playback)?;

        // Re-enter the playlist so the verified capture session records the
        // track from its true start, and so Music.app keeps the container
        // context that carries the rest of the run gaplessly.
        play_queue_playlist_blocking().await?;
        wait_for_selected_track_transport(
            &source,
            expected_key.as_deref(),
            "playing",
            Some(TRACK_START_POSITION_TOLERANCE_SECS),
            "restart the queue playlist at 0:00",
        )
        .await?;
        ensure_owned(state, &guard, &playback)?;
        state
            .apple_music_playback()
            .set_capture_gate_open(true);
        let prefill_target_secs = apple_music_boundary_lead_secs(settings.boundary_lead_secs);
        wait_for_prefill(
            state,
            &guard,
            &playback,
            prefill_target_secs,
            (prefill_target_secs * 0.75).max(START_PREFILL_MIN_SECS),
            Duration::from_secs_f64(prefill_target_secs + 2.0),
        )
        .await?;
        ensure_selected_track_still_playing(&source).await?;
        ensure_owned(state, &guard, &playback)?;
        prepare_hegel_for_zone(state, zone_id).await?;
        player.resume();
        wait_for_player_output_ready(state, &guard, &playback, &player, verified_epoch).await?;

        if let Some(probe) = cached_probe {
            match probe.await {
                Ok(Ok(Some(actual)))
                    if actual.sample_rate_hz != source_rate_hz
                        || actual.source_bit_depth_bits != source_bits =>
                {
                    warn!(
                        event = "apple_music_cached_format_mismatch",
                        song_id,
                        cached_rate_hz = source_rate_hz,
                        actual_rate_hz = actual.sample_rate_hz,
                        "The cached Apple Music decoder format changed; correcting the live session"
                    );
                    player.pause();
                    pause_music_blocking().await?;
                    state
                        .apple_music_playback()
                        .set_capture_gate_open(false);
                    set_music_position_blocking(0.0).await?;
                    source_rate_hz = actual.sample_rate_hz;
                    source_bits = actual.source_bit_depth_bits;
                    verified_epoch = state
                        .apple_music_playback()
                        .restart_at_verified_source_format(source_rate_hz, source_bits)
                        .map_err(PlaybackError::integration)?;
                    if !state
                        .apple_music_playback()
                        .replace_player_epoch(playback.generation, verified_epoch)
                    {
                        return Err(PlaybackError::conflict("Playback changed"));
                    }
                    state
                        .apple_music_playback()
                        .set_capture_gate_open(false);
                    hold_player_paused(&player, verified_epoch).await?;
                    player.flush_live_output();
                    restart_queue_playlist_from_start(&source, expected_key.as_deref()).await?;
                    state
                        .apple_music_playback()
                        .set_capture_gate_open(true);
                    wait_for_prefill(
                        state,
                        &guard,
                        &playback,
                        prefill_target_secs,
                        (prefill_target_secs * 0.75).max(START_PREFILL_MIN_SECS),
                        Duration::from_secs_f64(prefill_target_secs + 2.0),
                    )
                    .await?;
                    player.resume();
                    wait_for_player_output_ready(
                        state,
                        &guard,
                        &playback,
                        &player,
                        verified_epoch,
                    )
                    .await?;
                }
                Ok(Ok(Some(_))) => {}
                Ok(Ok(None)) => warn!(
                    event = "apple_music_cached_format_not_reverified",
                    song_id,
                    "Music.app exposed no fresh decoder event; continuing with the last verified per-track format"
                ),
                Ok(Err(error)) if error.code == "lossy_source_format_selected" => {
                    return Err(playback_error(error));
                }
                Ok(Err(error)) => warn!(
                    event = "apple_music_cached_format_verification_failed",
                    song_id,
                    error = %error.message,
                    "Could not asynchronously reverify the cached Apple Music format"
                ),
                Err(error) => warn!(
                    event = "apple_music_cached_format_verification_stopped",
                    song_id,
                    error = %error,
                    "Cached Apple Music format verification task stopped"
                ),
            }
        }
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
        album_id.as_deref(),
        storefront.as_deref(),
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

/// Prepare and promote a Qobuz/local -> Apple Music boundary before the
/// ordinary stopped-state auto-advance sees it. Returning true tells the
/// monitor that this boundary owns auto-advance for the current tick.
pub(crate) fn maybe_spawn_cross_provider_apple_music_prewarm(
    state: &AppState,
    zone_id: &str,
    status: &StatusResponse,
    pending: &Arc<Mutex<HashSet<String>>>,
) -> bool {
    if matches!(
        state.zones().zone_protocol(zone_id),
        Some(SinkProtocol::RemoteAgent | SinkProtocol::SonosUpnp | SinkProtocol::UpnpAvRenderer)
    ) || state.apple_music_playback().capture_running()
    {
        return false;
    }
    let Some(active_source) = state.listening().active_source(zone_id) else {
        return false;
    };
    if !matches!(
        active_source,
        SourceRef::QobuzTrack { .. } | SourceRef::LocalTrack { .. }
    ) || status
        .current_source
        .as_ref()
        .is_some_and(|source| source.key() != active_source.key())
    {
        return false;
    }
    let Some(next_source) = zone_queue_sources(state, zone_id)
        .first()
        .filter(|source| matches!(source, SourceRef::AppleMusicTrack { .. }))
        .cloned()
    else {
        return false;
    };
    let next_key = next_source.key();
    let prepared = state.apple_music_playback().prepared_boundary(&next_key);
    let already_pending = pending.lock().unwrap().contains(zone_id);
    if already_pending {
        return true;
    }

    let has_reliable_timeline = status.duration_secs.is_finite()
        && status.duration_secs > 0.0
        && status.position_secs.is_finite()
        && status.position_secs >= 0.0;
    let remaining_secs = if has_reliable_timeline {
        (status.duration_secs - status.position_secs).max(0.0)
    } else {
        f64::INFINITY
    };
    let should_promote = prepared.is_some()
        && (status.state == "Stopped"
            || (status.state == "Playing" && remaining_secs <= CROSS_PROVIDER_HANDOFF_WINDOW_SECS));
    if should_promote {
        if !pending.lock().unwrap().insert(zone_id.to_string()) {
            return true;
        }
        let state = state.clone();
        let zone_id = zone_id.to_string();
        let pending = Arc::clone(pending);
        let expected_source = active_source;
        let outgoing_duration_secs = status.duration_secs;
        let stopped = status.state == "Stopped";
        tokio::spawn(async move {
            let result = promote_cross_provider_apple_music_boundary(
                &state,
                &zone_id,
                &expected_source,
                next_source,
                outgoing_duration_secs,
                stopped,
            )
            .await;
            if let Err(error) = result {
                debug!(
                    event = "apple_music_cross_provider_handoff_deferred",
                    zone_id,
                    error = %error,
                    "The prepared Apple Music boundary was not at a safe endpoint yet"
                );
            }
            pending.lock().unwrap().remove(&zone_id);
        });
        return true;
    }

    if prepared.is_some() {
        return true;
    }
    if status.state != "Playing" || remaining_secs > CROSS_PROVIDER_PREWARM_SECS {
        // If playback has already stopped without a prepared ring, let the
        // existing queue router perform its reliable fallback immediately.
        return false;
    }
    if !pending.lock().unwrap().insert(zone_id.to_string()) {
        return true;
    }
    let Some(player) = native_local_player(state, zone_id) else {
        pending.lock().unwrap().remove(zone_id);
        return false;
    };
    let expected_epoch = player.playback_epoch();
    let expected_source = active_source;
    let state = state.clone();
    let zone_id = zone_id.to_string();
    let pending = Arc::clone(pending);
    tokio::spawn(async move {
        let result = prepare_cross_provider_apple_music_boundary(
            &state,
            &zone_id,
            &expected_source,
            expected_epoch,
            next_source,
        )
        .await;
        if let Err(error) = result {
            warn!(
                event = "apple_music_cross_provider_prewarm_failed",
                zone_id,
                error = %error,
                "Could not prepare Apple Music during the outgoing track"
            );
            state.apple_music_playback().release_retained_route();
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        pending.lock().unwrap().remove(&zone_id);
    });
    true
}

async fn prepare_cross_provider_apple_music_boundary(
    state: &AppState,
    zone_id: &str,
    expected_source: &SourceRef,
    expected_epoch: u64,
    next_source: SourceRef,
) -> Result<(), PlaybackError> {
    let next_source = hydrate_catalog_source(state, next_source).await?;
    let (song_id, storefront, album_id) = catalog_identity(&next_source)?;
    let player = native_local_player(state, zone_id).ok_or(PlaybackError::ZoneNotAvailable)?;
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    ensure_cross_provider_boundary_current(
        state,
        zone_id,
        expected_source,
        &next_source,
        &player,
        expected_epoch,
    )?;
    prepare_music_blocking().await?;
    state
        .apple_music_playback()
        .prepare_capture_route(&player)
        .map_err(PlaybackError::integration)?;
    let format_boundary = SystemTime::now();
    // The outgoing provider is still playing, so this whole prewarm — the
    // library import included — happens off the audible path.
    let prewarm_queue = zone_queue_sources(state, zone_id);
    let playlist_song_ids =
        queue_playlist_song_ids(state, &next_source, prewarm_queue.get(1..).unwrap_or(&[]), None);
    let playlist_keys = sync_queue_playlist_blocking(state, playlist_song_ids).await?;
    let expected_key = playlist_keys.first().cloned();
    let apple_music = state.apple_music();
    let selection = async {
        play_queue_playlist_blocking().await?;
        wait_for_music_track(&next_source, expected_key.as_deref(), TRACK_ACTIVATION_TIMEOUT).await
    };
    let (selected, source_format) = tokio::join!(
        selection,
        apple_music.probe_music_app_source_format(format_boundary)
    );
    let selected = selected?;
    let source_format = source_format.map_err(playback_error)?.ok_or_else(|| {
        PlaybackError::integration(
            "Music.app exposed no fresh Apple Lossless format during cross-provider prewarm.",
        )
    })?;
    ensure_cross_provider_boundary_current(
        state,
        zone_id,
        expected_source,
        &next_source,
        &player,
        expected_epoch,
    )?;
    pause_music_blocking().await?;
    set_music_position_blocking(0.0).await?;
    wait_for_selected_track_transport(
        &next_source,
        expected_key.as_deref(),
        "paused",
        Some(TRACK_PARK_POSITION_TOLERANCE_SECS),
        "park the prewarmed track at 0:00",
    )
    .await?;

    let settings = state.settings().apple_music_playback_settings();
    let prepared = PreparedAppleMusicControl {
        source_key: next_source.key(),
        rate_hz: source_format.sample_rate_hz,
        source_bit_depth: source_format.source_bit_depth_bits,
        duration_secs: selected
            .track
            .duration_secs
            .or_else(|| next_source.duration_secs()),
    };
    state
        .apple_music_playback()
        .prepare_boundary_capture(player.clone(), &settings, prepared)
        .map_err(PlaybackError::integration)?;
    play_queue_playlist_blocking().await?;
    wait_for_selected_track_transport(
        &next_source,
        expected_key.as_deref(),
        "playing",
        Some(TRACK_START_POSITION_TOLERANCE_SECS),
        "restart the prewarmed queue playlist at 0:00",
    )
    .await?;
    if !state
        .apple_music_playback()
        .set_prepared_capture_gate_open(true)
    {
        return Err(PlaybackError::integration(
            "The prepared Apple Music capture gate disappeared.",
        ));
    }
    let prefill_target = apple_music_boundary_lead_secs(settings.boundary_lead_secs);
    let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(prefill_target + 2.0);
    loop {
        let buffered = state
            .apple_music_playback()
            .prepared_boundary(&next_source.key())
            .map(|(_, buffered)| buffered)
            .ok_or_else(|| PlaybackError::conflict("Prepared Apple Music boundary changed"))?;
        if buffered >= prefill_target {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(PlaybackError::integration(format!(
                "Prepared Apple Music captured only {buffered:.3}s of the {prefill_target:.3}s boundary lead."
            )));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    ensure_cross_provider_boundary_current(
        state,
        zone_id,
        expected_source,
        &next_source,
        &player,
        expected_epoch,
    )?;
    if let Err(error) = state.library().record_apple_music_track_format(
        &song_id,
        album_id.as_deref(),
        storefront.as_deref(),
        "ALAC",
        source_format.sample_rate_hz,
        source_format.source_bit_depth_bits,
    ) {
        warn!(
            event = "apple_music_verified_format_not_recorded",
            zone_id, song_id, error, "Could not remember the prewarmed Apple Music decoder format"
        );
    }
    info!(
        event = "apple_music_cross_provider_prewarm_ready",
        zone_id,
        next_source_key = next_source.key(),
        source_rate_hz = source_format.sample_rate_hz,
        prefill_ms = prefill_target * 1_000.0,
        "Apple Music capture and DSP source are ready behind the outgoing track"
    );
    Ok(())
}

fn ensure_cross_provider_boundary_current(
    state: &AppState,
    zone_id: &str,
    expected_source: &SourceRef,
    next_source: &SourceRef,
    player: &Player,
    expected_epoch: u64,
) -> Result<(), PlaybackError> {
    if player.playback_epoch() != expected_epoch
        || state
            .listening()
            .active_source(zone_id)
            .is_none_or(|source| source.key() != expected_source.key())
        || zone_queue_sources(state, zone_id)
            .first()
            .is_none_or(|source| source.key() != next_source.key())
    {
        return Err(PlaybackError::conflict("Playback or queue changed"));
    }
    Ok(())
}

async fn promote_cross_provider_apple_music_boundary(
    state: &AppState,
    zone_id: &str,
    expected_source: &SourceRef,
    next_source: SourceRef,
    outgoing_duration_secs: f64,
    outgoing_stopped: bool,
) -> Result<(), PlaybackError> {
    let player = native_local_player(state, zone_id).ok_or(PlaybackError::ZoneNotAvailable)?;
    let expected_epoch = player.playback_epoch();
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    ensure_cross_provider_boundary_current(
        state,
        zone_id,
        expected_source,
        &next_source,
        &player,
        expected_epoch,
    )?;
    let (_, buffered_secs) = state
        .apple_music_playback()
        .prepared_boundary(&next_source.key())
        .ok_or_else(|| PlaybackError::conflict("Prepared Apple Music boundary changed"))?;
    let (handoff_epoch, output_cushion_secs, preserve_output) = if outgoing_stopped {
        (expected_epoch, 0.0, false)
    } else {
        let boundary = player
            .begin_seamless_handoff(
                state
                    .apple_music_playback()
                    .prepared_boundary(&next_source.key())
                    .map(|(prepared, _)| prepared.rate_hz),
            )
            .await
            .map_err(PlaybackError::integration)?;
        if outgoing_duration_secs.is_finite()
            && outgoing_duration_secs > 0.0
            && boundary.position_secs
                < outgoing_duration_secs - CROSS_PROVIDER_ENDPOINT_TOLERANCE_SECS
        {
            player.cancel_seamless_handoff(boundary.epoch);
            return Err(PlaybackError::conflict(format!(
                "Outgoing endpoint {:.3}s has not reached duration {:.3}s",
                boundary.position_secs, outgoing_duration_secs
            )));
        }
        (boundary.epoch, boundary.output_cushion_secs, true)
    };
    let playback = match state.apple_music_playback().promote_prepared_boundary(
        zone_id.to_string(),
        next_source.clone(),
        handoff_epoch,
        preserve_output,
    ) {
        Ok(playback) => playback,
        Err(error) => {
            if preserve_output {
                player.cancel_seamless_handoff(handoff_epoch);
            }
            return Err(PlaybackError::integration(error));
        }
    };
    state.listening().completed_next(state.library(), zone_id);
    spawn_music_app_monitor(state.clone(), zone_id.to_string(), playback.generation);
    spawn_native_next_prefetch(state.clone(), zone_id.to_string());
    info!(
        event = "apple_music_cross_provider_handoff_completed",
        zone_id,
        previous_source_key = expected_source.key(),
        next_source_key = next_source.key(),
        buffered_ms = buffered_secs * 1_000.0,
        output_cushion_ms = output_cushion_secs * 1_000.0,
        preserve_output,
        player_epoch = playback.player_epoch,
        "Promoted the prefilled Apple Music ring without an audible start gap"
    );
    Ok(())
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
    state.apple_music_playback().set_capture_gate_open(false);
    if !state
        .apple_music_playback()
        .set_timeline_origin(snapshot.generation, seconds)
    {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    hold_player_paused(&player, replacement_epoch).await?;
    player.flush_live_output();
    set_music_position_blocking(seconds).await?;
    play_music_blocking().await?;
    state.apple_music_playback().set_capture_gate_open(true);
    let settings = state.settings().apple_music_playback_settings();
    let prefill_target_secs = apple_music_boundary_lead_secs(settings.boundary_lead_secs);
    wait_for_prefill_without_guard(
        state,
        &snapshot,
        prefill_target_secs,
        (prefill_target_secs * 0.75).max(START_PREFILL_MIN_SECS),
        Duration::from_secs_f64(prefill_target_secs + 2.0),
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

/// Catalog identity for playback: the song ID, plus storefront and album for
/// bookkeeping.
///
/// Only the song ID is required. The queue playlist is built from catalog song
/// IDs alone, so a track with no album metadata — a single, or a radio pick —
/// is still playable. Storefront and album are used for the verified-format
/// record and for logging.
fn catalog_identity(
    source: &SourceRef,
) -> Result<(String, Option<String>, Option<String>), PlaybackError> {
    let SourceRef::AppleMusicTrack {
        song_id,
        storefront,
        album_id,
        ..
    } = source
    else {
        return Err(PlaybackError::bad_request("Expected an Apple Music source"));
    };
    let normalize = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let song_id = normalize(Some(song_id)).ok_or_else(|| {
        PlaybackError::bad_request(
            "Apple Music song ID is required for native lossless playback.",
        )
    })?;
    Ok((
        song_id,
        normalize(storefront.as_deref()),
        normalize(album_id.as_deref()),
    ))
}

async fn wait_for_selected_track(
    state: &AppState,
    guard: &PlaybackGuard,
    playback: &AppleMusicPlaybackSnapshot,
    expected_key: Option<&str>,
) -> Result<MusicAppSnapshot, PlaybackError> {
    let deadline = tokio::time::Instant::now() + TRACK_ACTIVATION_TIMEOUT;
    let mut last_track = None;
    loop {
        ensure_owned(state, guard, playback)?;
        let snapshot = music_status_blocking().await?;
        if snapshot.has_current_track() {
            last_track = snapshot.track.title.clone();
            if music_track_matches_expected(&snapshot, &playback.source, expected_key) {
                return Ok(snapshot);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(PlaybackError::integration(format!(
                "Music.app did not start Fozmo's queue playlist{}.",
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
    expected_key: Option<&str>,
    expected_state: &str,
    maximum_position_secs: Option<f64>,
    operation: &str,
) -> Result<MusicAppSnapshot, PlaybackError> {
    let deadline = tokio::time::Instant::now() + TRACK_TRANSPORT_SETTLE_TIMEOUT;
    loop {
        let snapshot = music_status_blocking().await?;
        let position_matches = maximum_position_secs
            .is_none_or(|maximum| track_position_is_at_start(&snapshot, maximum));
        if music_track_matches_expected(&snapshot, source, expected_key)
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

/// Whether an internal Music.app advance between these two sources is one
/// Fozmo can adopt without restarting capture.
///
/// Both must be Apple Music tracks on the same storefront: only such tracks can
/// be in the Fozmo playlist, so only they can be reached by Music.app advancing
/// on its own. This deliberately no longer requires the same album or
/// consecutive track numbers — the playlist supplies the ordering that the
/// album context used to.
fn native_apple_music_playlist_pair(current: &SourceRef, next: &SourceRef) -> bool {
    let (
        SourceRef::AppleMusicTrack {
            storefront: current_storefront,
            ..
        },
        SourceRef::AppleMusicTrack {
            storefront: next_storefront,
            ..
        },
    ) = (current, next)
    else {
        return false;
    };
    match (
        current_storefront.as_deref().map(str::trim),
        next_storefront.as_deref().map(str::trim),
    ) {
        (Some(current), Some(next)) => current.eq_ignore_ascii_case(next),
        // A missing storefront resolves to the account default for both.
        _ => true,
    }
}

/// Whether Music.app is on the queue-playlist entry Fozmo expects.
///
/// `database ID` is exact and is the reason the queue playlist exists: before
/// the tracks were library items, AppleScript could not report a catalog track
/// at all and Fozmo had to guess from title and artist. Metadata comparison
/// stays only as a fallback for the moment before a sync has produced keys.
fn music_track_matches_expected(
    snapshot: &MusicAppSnapshot,
    source: &SourceRef,
    expected_key: Option<&str>,
) -> bool {
    match (expected_key, snapshot.track.track_key.as_deref()) {
        (Some(expected), Some(actual)) => expected == actual,
        (Some(_), None) => false,
        (None, _) => music_track_matches_source(snapshot, source),
    }
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
    let duration_matches = source.duration_secs().is_none_or(|expected| {
        snapshot
            .track
            .duration_secs
            .is_none_or(|actual| track_durations_match(expected, actual))
    });
    title_matches && artist_matches && duration_matches
}

fn track_durations_match(expected: f64, actual: f64) -> bool {
    if !expected.is_finite() || !actual.is_finite() || expected <= 0.0 || actual <= 0.0 {
        return false;
    }
    let tolerance = (expected * 0.005).clamp(0.75, 2.0);
    (expected - actual).abs() <= tolerance
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
                        restart_queue_playlist_from_start(&playback.source, None).await?;
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
        let mut last_position = 0.0_f64;
        let mut last_duration = 0.0_f64;
        let mut consecutive_errors = 0_u8;
        let mut startup_recovery_attempted = false;
        let mut first_snapshot = true;
        let mut last_playing_observed_at: Option<tokio::time::Instant> = None;
        loop {
            if first_snapshot {
                first_snapshot = false;
            } else {
                wait_for_music_notification_blocking(MUSIC_NOTIFICATION_FALLBACK).await;
            }
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
                let completion_position = projected_music_position(
                    last_position,
                    last_playing_observed_at
                        .map(|observed| tokio::time::Instant::now().duration_since(observed)),
                );
                let completed = native_track_completed(completion_position, duration);
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
                    startup_recovery_attempted = false;
                    last_playing_observed_at = Some(tokio::time::Instant::now());
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
                    if should_recover_startup_stall(last_position, startup_recovery_attempted) {
                        startup_recovery_attempted = true;
                        match restart_queue_playlist_from_start(&snapshot.source, None).await {
                            Ok(recovered) => {
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
                                last_playing_observed_at = Some(tokio::time::Instant::now());
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
                    let completion_position = projected_music_position(
                        last_position,
                        last_playing_observed_at
                            .map(|observed| tokio::time::Instant::now().duration_since(observed)),
                    );
                    let completed = native_track_completed(completion_position, duration);
                    if completed {
                        match switch_buffered_apple_music_boundary(
                            &state, &zone_id, generation, &snapshot,
                        )
                        .await
                        {
                            Ok(Some(next_music)) => {
                                last_position = next_music.track.position_secs.unwrap_or(0.0);
                                last_duration = next_music.track.duration_secs.unwrap_or_default();
                                startup_recovery_attempted = false;
                                continue;
                            }
                            Ok(None) => {}
                            Err(error) => warn!(
                                event = "apple_music_buffered_boundary_fallback",
                                zone_id,
                                generation,
                                error = %error,
                                "Buffered Apple Music switch failed; falling back to normal queue routing"
                            ),
                        }
                    }
                    info!(
                        event = "apple_music_native_terminal_state",
                        zone_id,
                        generation,
                        last_position_secs = last_position,
                        completion_position_secs = completion_position,
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
                _ => {}
            }
        }
    });
}

async fn switch_buffered_apple_music_boundary(
    state: &AppState,
    zone_id: &str,
    generation: u64,
    previous: &AppleMusicPlaybackSnapshot,
) -> Result<Option<MusicAppSnapshot>, PlaybackError> {
    let queue = zone_queue_sources(state, zone_id);
    let Some(next) = queue.first().cloned() else {
        return Ok(None);
    };
    if !matches!(next, SourceRef::AppleMusicTrack { .. }) {
        return Ok(None);
    }
    let (song_id, _storefront, _album_id) = catalog_identity(&next)?;
    let Some((live_rate_hz, _)) = state.apple_music_playback().session_format() else {
        return Ok(None);
    };
    let next_format = state
        .library()
        .apple_music_track_verified_format(&song_id)
        .map_err(PlaybackError::library)?
        .filter(|format| format.codec.eq_ignore_ascii_case("ALAC"))
        .and_then(|format| {
            Some((
                u32::try_from(format.sample_rate).ok()?,
                format.bit_depth.and_then(|bits| u32::try_from(bits).ok()),
            ))
        });
    let Some((next_rate_hz, _)) = next_format else {
        debug!(
            event = "apple_music_buffered_boundary_format_unknown",
            zone_id, song_id, "The next Apple Music track has no cached verified format"
        );
        return Ok(None);
    };
    if next_rate_hz != live_rate_hz {
        info!(
            event = "apple_music_buffered_boundary_rate_change",
            zone_id,
            song_id,
            live_rate_hz,
            next_rate_hz,
            "The next Apple Music track needs a capture-rate change"
        );
        return Ok(None);
    }

    let _playback_switch = state.apple_music().lock_playback_switch().await;
    let Some(active) = active_snapshot(state, zone_id) else {
        return Ok(None);
    };
    if active.generation != generation || active.source.key() != previous.source.key() {
        return Ok(None);
    }
    let player = native_local_player(state, zone_id).ok_or(PlaybackError::ZoneNotAvailable)?;
    let buffered_before = state
        .apple_music_playback()
        .buffered_audio_secs()
        .map_err(PlaybackError::integration)?;
    let underruns_before = state
        .apple_music_playback()
        .capture_underrun_count()
        .unwrap_or_default();
    if !state.apple_music_playback().set_capture_gate_open(false) {
        return Ok(None);
    }
    info!(
        event = "apple_music_buffered_boundary_started",
        zone_id,
        previous_source_key = previous.source.key(),
        next_source_key = next.key(),
        buffered_ms = buffered_before * 1_000.0,
        "Switching Music.app while the listener consumes the capture tail"
    );

    // Rebuild the playlist so it starts at the successor. This runs while the
    // listener is still consuming the capture tail, so the library import and
    // the transport swap stay under the buffer.
    let playlist_song_ids =
        queue_playlist_song_ids(state, &next, queue.get(1..).unwrap_or(&[]), None);
    let playlist_keys = sync_queue_playlist_blocking(state, playlist_song_ids).await?;
    let expected_key = playlist_keys.first().cloned();
    play_queue_playlist_blocking().await?;
    let selected =
        wait_for_music_track(&next, expected_key.as_deref(), TRACK_ACTIVATION_TIMEOUT).await?;
    pause_music_blocking().await?;
    set_music_position_blocking(0.0).await?;
    let playing = restart_queue_playlist_from_start(&next, expected_key.as_deref()).await?;
    state.apple_music_playback().set_capture_gate_open(true);

    let player_position_secs = native_player_position_secs(&player);
    if !state.apple_music_playback().promote_continuous_playback(
        generation,
        next.clone(),
        player_position_secs,
        playing.track.position_secs,
        playing.track.duration_secs.or(selected.track.duration_secs),
    ) {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    state.listening().completed_next(state.library(), zone_id);
    spawn_native_next_prefetch(state.clone(), zone_id.to_string());
    let underruns_after = state
        .apple_music_playback()
        .capture_underrun_count()
        .unwrap_or_default();
    if underruns_after > underruns_before {
        warn!(
            event = "apple_music_buffered_boundary_underrun",
            zone_id,
            previous_source_key = previous.source.key(),
            next_source_key = next.key(),
            buffered_ms = buffered_before * 1_000.0,
            "Music.app switch outran the capture lead; live input resumed after the current-behaviour gap"
        );
    } else {
        info!(
            event = "apple_music_buffered_boundary_completed",
            zone_id,
            previous_source_key = previous.source.key(),
            next_source_key = next.key(),
            "Promoted an arbitrary same-rate Apple Music boundary without reopening Player or the DAC"
        );
    }
    Ok(Some(playing))
}

async fn wait_for_music_track(
    source: &SourceRef,
    expected_key: Option<&str>,
    timeout: Duration,
) -> Result<MusicAppSnapshot, PlaybackError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let snapshot = music_status_blocking().await?;
        if music_track_matches_expected(&snapshot, source, expected_key) {
            return Ok(snapshot);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(PlaybackError::integration(
                "Music.app did not select the buffered Apple Music successor.",
            ));
        }
        tokio::time::sleep(TRACK_ACTIVATION_POLL).await;
    }
}

fn promote_continuous_apple_music_boundary(
    state: &AppState,
    zone_id: &str,
    generation: u64,
    previous: &AppleMusicPlaybackSnapshot,
    music: &MusicAppSnapshot,
) -> Option<SourceRef> {
    let next = zone_queue_sources(state, zone_id).into_iter().next()?;
    // Music.app advanced on its own, which it can only do inside the Fozmo
    // playlist. Any successor it reached is therefore one Fozmo queued, so the
    // old same-album/consecutive-track restriction no longer applies: this is
    // what extends the gapless boundary to arbitrary Apple-to-Apple pairs.
    if !native_apple_music_playlist_pair(&previous.source, &next)
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

fn should_recover_startup_stall(last_position_secs: f64, already_attempted: bool) -> bool {
    !already_attempted
        && (!last_position_secs.is_finite()
            || last_position_secs <= STARTUP_STALL_POSITION_MAX_SECS)
}

fn projected_music_position(last_position_secs: f64, elapsed: Option<Duration>) -> f64 {
    if !last_position_secs.is_finite() || last_position_secs < 0.0 {
        return 0.0;
    }
    last_position_secs
        + elapsed
            .map(|duration| duration.as_secs_f64())
            .filter(|seconds| seconds.is_finite())
            .unwrap_or_default()
}

fn native_track_completed(last_position_secs: f64, duration_secs: f64) -> bool {
    duration_secs.is_finite()
        && duration_secs > 0.0
        && last_position_secs.is_finite()
        && last_position_secs > 0.0
        && (last_position_secs >= duration_secs - 0.5
            || last_position_secs / duration_secs >= 0.995)
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

async fn wait_for_music_notification_blocking(timeout: Duration) {
    let _ = tokio::task::spawn_blocking(move || wait_for_music_app_notification(timeout)).await;
}

/// Re-enter the queue playlist and confirm it is playing its first track.
///
/// Music.app can restore its previous global player position while it builds a
/// transport, so the position is reasserted until the reported timeline agrees.
async fn restart_queue_playlist_from_start(
    source: &SourceRef,
    expected_key: Option<&str>,
) -> Result<MusicAppSnapshot, PlaybackError> {
    play_queue_playlist_blocking().await?;
    set_music_position_blocking(0.0).await?;
    let deadline = tokio::time::Instant::now() + STARTUP_TRANSPORT_RECOVERY_TIMEOUT;
    let mut next_position_reset = tokio::time::Instant::now() + Duration::from_millis(250);
    loop {
        let snapshot = music_status_blocking().await?;
        let on_expected_track = snapshot.running
            && snapshot.player_state.as_deref() == Some("playing")
            && music_track_matches_expected(&snapshot, source, expected_key);
        if on_expected_track
            && track_position_is_at_start(&snapshot, TRACK_START_POSITION_TOLERANCE_SECS)
        {
            return Ok(snapshot);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(PlaybackError::integration(
                "Music.app did not restart Fozmo's queue playlist from its first track.",
            ));
        }
        if on_expected_track && tokio::time::Instant::now() >= next_position_reset {
            warn!(
                event = "apple_music_start_position_corrected",
                position_secs = snapshot.track.position_secs.unwrap_or_default(),
                "Music.app restored its global position after play; resetting the queue playlist to 0:00"
            );
            set_music_position_blocking(0.0).await?;
            next_position_reset = tokio::time::Instant::now() + Duration::from_millis(250);
        }
        tokio::time::sleep(TRACK_ACTIVATION_POLL).await;
    }
}

/// Contiguous run of Apple Music songs starting at `source`, as catalog IDs.
///
/// The run stops at the first non-Apple source because Music.app can only
/// advance through its own playlist; a Qobuz or local successor is handed to
/// the engine instead. It also stops at a sample-rate change, since a new rate
/// needs a fresh capture session and cannot be crossed inside one playlist.
fn queue_playlist_song_ids(
    state: &AppState,
    source: &SourceRef,
    queue: &[SourceRef],
    capture_rate_hz: Option<u32>,
) -> Vec<String> {
    let mut song_ids = Vec::new();
    for candidate in std::iter::once(source).chain(queue.iter()) {
        let SourceRef::AppleMusicTrack { song_id, .. } = candidate else {
            break;
        };
        if song_id.trim().is_empty() {
            break;
        }
        // The first entry is the track being started, so its rate defines the
        // session rather than having to match it.
        if let (Some(capture_rate_hz), false) = (capture_rate_hz, song_ids.is_empty())
            && cached_track_rate_hz(state, candidate).is_none_or(|rate| rate != capture_rate_hz)
        {
            break;
        }
        song_ids.push(song_id.clone());
        if song_ids.len() >= MAX_QUEUE_PLAYLIST_TRACKS {
            break;
        }
    }
    song_ids
}

/// Verified sample rate Fozmo has already recorded for a catalog song.
fn cached_track_rate_hz(state: &AppState, source: &SourceRef) -> Option<u32> {
    let SourceRef::AppleMusicTrack {
        song_id, album_id, ..
    } = source
    else {
        return None;
    };
    let album_id = album_id.as_deref()?;
    state
        .library()
        .apple_music_track_verified_formats(album_id)
        .ok()?
        .into_iter()
        .find(|(id, _)| id == song_id)
        .and_then(|(_, format)| {
            format
                .codec
                .eq_ignore_ascii_case("ALAC")
                .then(|| u32::try_from(format.sample_rate).ok())
                .flatten()
        })
}

/// Rebuild the Fozmo playlist for `song_ids` and wait until Music.app can play it.
///
/// Returns the playlist's `database ID`s in playback order. Those are the
/// identities the verification loops match against: Music.app's `current track`
/// only became readable at all because these are now library items.
async fn sync_queue_playlist_blocking(
    state: &AppState,
    song_ids: Vec<String>,
) -> Result<Vec<String>, PlaybackError> {
    let expected = song_ids.len();
    if expected == 0 {
        return Err(PlaybackError::integration(
            "Fozmo had no Apple Music tracks to place in its queue playlist.",
        ));
    }
    tokio::task::spawn_blocking(delete_queue_playlist)
        .await
        .map_err(|error| {
            PlaybackError::internal_invariant(format!("Queue playlist reset stopped: {error}"))
        })?
        .map_err(PlaybackError::integration)?;
    if let Err(sync_error) = state.apple_music().sync_queue_playlist(song_ids).await {
        // A refused library write is almost always Sync Library being off, and
        // neither MusicKit nor AppleScript exposes that setting. Ask Apple
        // directly so the user gets the actionable message instead of a
        // generic failure.
        if let Ok(status) = state.apple_music().library_status().await
            && !status.can_write_library
        {
            return Err(PlaybackError::integration(
                status.blocked_reason.unwrap_or_else(|| {
                    "Apple Music will not let Fozmo build its queue playlist.".to_string()
                }),
            ));
        }
        return Err(playback_error(sync_error));
    }

    // Apple documents that "there may be a delay before a new resource appears
    // in a user's library", so the playlist is not playable the moment the Web
    // API returns.
    let deadline = tokio::time::Instant::now() + QUEUE_PLAYLIST_VISIBLE_TIMEOUT;
    loop {
        let count = tokio::task::spawn_blocking(queue_playlist_track_count)
            .await
            .map_err(|error| {
                PlaybackError::internal_invariant(format!("Queue playlist poll stopped: {error}"))
            })?
            .map_err(PlaybackError::integration)?;
        if count == Some(expected) {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(PlaybackError::integration(format!(
                "Music.app did not receive Fozmo's queue playlist in time ({} of {expected} tracks). Apple Music may still be syncing the library.",
                count.unwrap_or_default()
            )));
        }
        tokio::time::sleep(QUEUE_PLAYLIST_VISIBLE_POLL).await;
    }

    let keys = tokio::task::spawn_blocking(queue_playlist_track_keys)
        .await
        .map_err(|error| {
            PlaybackError::internal_invariant(format!("Queue playlist read stopped: {error}"))
        })?
        .map_err(PlaybackError::integration)?;
    if keys.len() != expected {
        return Err(PlaybackError::integration(format!(
            "Music.app exposed {} of {expected} queue-playlist tracks.",
            keys.len()
        )));
    }
    Ok(keys)
}

async fn play_queue_playlist_blocking() -> Result<(), PlaybackError> {
    tokio::task::spawn_blocking(play_queue_playlist)
        .await
        .map_err(|error| {
            PlaybackError::internal_invariant(format!("Queue playlist start stopped: {error}"))
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

    fn local_source() -> SourceRef {
        SourceRef::LocalTrack {
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
    fn music_metadata_match_rejects_a_same_named_track_with_wrong_duration() {
        let mut snapshot = MusicAppSnapshot::default();
        snapshot.running = true;
        snapshot.track.title = Some("New Kid In Town".to_string());
        snapshot.track.artist = Some("Eagles".to_string());
        snapshot.track.duration_secs = Some(201.8);

        assert!(!music_track_matches_source(
            &snapshot,
            &apple_source(Some("New Kid in Town"), Some("EAGLES"))
        ));
        snapshot.track.duration_secs = Some(299.2);
        assert!(music_track_matches_source(
            &snapshot,
            &apple_source(Some("New Kid in Town"), Some("EAGLES"))
        ));
    }

    #[test]
    fn boundary_lead_uses_two_seconds_for_invalid_values_and_safe_bounds() {
        assert_eq!(apple_music_boundary_lead_secs(f64::NAN), 2.0);
        assert_eq!(apple_music_boundary_lead_secs(0.1), 0.5);
        assert_eq!(apple_music_boundary_lead_secs(2.0), 2.0);
        assert_eq!(apple_music_boundary_lead_secs(12.0), 5.0);
    }

    #[test]
    fn terminal_completion_projects_the_last_playing_observation_to_the_stop_event() {
        let projected = projected_music_position(198.9, Some(Duration::from_secs_f64(1.1)));
        assert!(native_track_completed(projected, 200.0));
        assert!(!native_track_completed(
            projected_music_position(80.0, Some(Duration::from_secs_f64(1.1))),
            200.0
        ));
    }

    /// The playlist supplies the ordering that the album context used to, so an
    /// internal advance is adoptable for any two Apple tracks — not only
    /// consecutive ones from one album.
    #[test]
    fn playlist_pair_accepts_apple_successors_from_any_album() {
        let current = apple_source(Some("New Kid in Town"), Some("Eagles"));
        let mut other_album = apple_source(Some("Jóga"), Some("Björk"));
        if let SourceRef::AppleMusicTrack {
            song_id,
            album_id,
            track_number,
            ..
        } = &mut other_album
        {
            *song_id = "1726654449".to_string();
            *album_id = Some("different-album".to_string());
            *track_number = Some(9);
        }
        assert!(native_apple_music_playlist_pair(&current, &other_album));
    }

    /// Only Apple Music tracks can sit in the Fozmo playlist, so Music.app can
    /// never advance to another provider on its own.
    #[test]
    fn playlist_pair_rejects_other_providers_and_foreign_storefronts() {
        let current = apple_source(Some("New Kid in Town"), Some("Eagles"));
        assert!(!native_apple_music_playlist_pair(
            &current,
            &local_source()
        ));

        let mut foreign = apple_source(Some("Jóga"), Some("Björk"));
        if let SourceRef::AppleMusicTrack {
            song_id,
            storefront,
            ..
        } = &mut foreign
        {
            *song_id = "1726654449".to_string();
            *storefront = Some("jp".to_string());
        }
        assert!(!native_apple_music_playlist_pair(&current, &foreign));
    }

    #[test]
    /// The queue playlist is built from catalog song IDs alone, so a track with
    /// no album — a single, or a radio pick — must stay playable. Only a
    /// missing song ID is fatal.
    fn catalog_identity_needs_only_the_song_id() {
        let mut source = apple_source(Some("Track"), Some("Artist"));
        if let SourceRef::AppleMusicTrack {
            album_id,
            storefront,
            ..
        } = &mut source
        {
            *album_id = None;
            *storefront = None;
        }
        let (song_id, storefront, album_id) =
            catalog_identity(&source).expect("a song ID alone is enough to queue a track");
        assert_eq!(song_id, "635770203");
        assert_eq!(storefront, None);
        assert_eq!(album_id, None);

        if let SourceRef::AppleMusicTrack { song_id, .. } = &mut source {
            *song_id = "   ".to_string();
        }
        assert!(catalog_identity(&source).is_err());
    }

    #[test]
    fn terminal_music_state_uses_retained_near_end_position_as_completion() {
        assert!(native_track_completed(299.5, 300.0));
        assert!(native_track_completed(299.0, 300.0));
        assert!(!native_track_completed(296.5, 300.0));
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
