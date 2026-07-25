use crate::app::state::AppState;
use crate::playback::error::PlaybackError;
use crate::playback::intent::{PlaybackGuard, PlaybackIntent, PlaybackOutcome};
use crate::playback::router::PlaybackRouter;
use crate::playback::service::{
    apply_playback_settings_for_zone, prepare_airplay_volume_for_zone, prepare_hegel_for_zone,
};
use crate::protocol::SourceRef;
use crate::services::apple_music_musickit::{
    AppleMusicMvpError, ApplePlaybackSnapshot, HelperMessage, ProcessTapRateTransition,
};
use std::time::{Duration, SystemTime};
use tokio::sync::broadcast;
use tracing::{debug, warn};

// A new DSP/output session needs enough captured program audio to absorb
// decoder probing, high-rate DSP warmup, and the first output-ring fill. The
// shorter cushion remains appropriate for resume/seek, where that machinery
// is already initialized.
const APPLE_START_PREFILL_SECS: f64 = 0.500;
const APPLE_START_PREFILL_MIN_SECS: f64 = 0.250;
const APPLE_START_PREFILL_TIMEOUT: Duration = Duration::from_secs(2);
const APPLE_RESUME_PREFILL_SECS: f64 = 0.060;
const APPLE_RESUME_PREFILL_MIN_SECS: f64 = 0.010;
const APPLE_RESUME_PREFILL_TIMEOUT: Duration = Duration::from_millis(900);
const APPLE_AUDIO_PROCESS_TIMEOUT: Duration = Duration::from_secs(8);
const APPLE_AUDIO_PROCESS_RETRY_INTERVAL: Duration = Duration::from_millis(50);
const APPLE_TAP_STALL_TIMEOUT_MS: u64 = 3_000;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProviderRun {
    pub(crate) current: SourceRef,
    /// Includes `current` at index zero.
    pub(crate) contiguous: Vec<SourceRef>,
    /// The complete Fozmo-owned queue after `current`.
    pub(crate) remaining: Vec<SourceRef>,
    /// The queue after the contiguous provider segment.
    pub(crate) after_contiguous: Vec<SourceRef>,
}

pub(crate) fn provider_run(current: SourceRef, queue: &[SourceRef]) -> ProviderRun {
    let provider = current.provider();
    let contiguous_tail_len = queue
        .iter()
        .take_while(|source| source.provider() == provider)
        .count();
    let mut contiguous = Vec::with_capacity(contiguous_tail_len + 1);
    contiguous.push(current.clone());
    contiguous.extend(queue.iter().take(contiguous_tail_len).cloned());
    ProviderRun {
        current,
        contiguous,
        remaining: queue.to_vec(),
        after_contiguous: queue.iter().skip(contiguous_tail_len).cloned().collect(),
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
    if source.apple_music_song_id().is_none() {
        return Err(PlaybackError::bad_request("Expected an Apple Music source"));
    }
    if !guard.is_current(state) {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    let Some(player) = state.zones().player_for_zone(zone_id) else {
        return Err(PlaybackError::ZoneNotAvailable);
    };
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    ensure_current(state, &guard)?;

    apply_playback_settings_for_zone(state, zone_id);
    prepare_airplay_volume_for_zone(state, zone_id, &player);
    prepare_hegel_for_zone(state, zone_id).await?;

    stop_replaced_session_locked(state, zone_id).await;
    ensure_current(state, &guard)?;
    let run = provider_run(source.clone(), &queue);
    // Keep the helper queue to exactly one item. Every Apple Music track must
    // independently pass the catalog + active AudioVariant lossless gates
    // before a process tap can reach Fozmo's DSP. Letting MusicKit advance a
    // multi-item internal queue could switch the live tap to AAC before Fozmo
    // has a chance to validate the new entry.
    let helper_segment = vec![run.current.clone()];
    let starting_epoch = player.playback_epoch();
    let queue_revision = state
        .apple_music()
        .prepare_queue(&helper_segment, 0)
        .await
        .map_err(playback_error)?;
    if let Err(stale) = ensure_current(state, &guard) {
        let _ = state.apple_music().transport("stop").await;
        return Err(stale);
    }
    let helper_session_id = state
        .apple_music()
        .helper_session_id()
        .await
        .map_err(playback_error)?;
    let receiver = state
        .apple_music()
        .subscribe_events()
        .await
        .map_err(playback_error)?;

    let start_result = async {
        let preexisting_renderer_pids = state
            .apple_music()
            .active_musickit_renderer_pids()
            .map_err(playback_error)?;
        ensure_current(state, &guard)?;
        let source_format_boundary = state.apple_music().source_format_probe_boundary();
        state
            .apple_music()
            .play_prepared()
            .await
            .map_err(playback_error)?;
        ensure_current(state, &guard)?;
        prepare_musickit_process_tap_after_playback(
            state,
            player.clone(),
            &preexisting_renderer_pids,
            &guard,
        )
        .await?;
        prepare_initial_source_rate(state, &player, &guard, source_format_boundary).await?;
        commit_initial_process_tap(state, &player, starting_epoch, &guard).await?;
        Ok::<(), PlaybackError>(())
    }
    .await;

    if let Err(failure) = start_result {
        cleanup_failed_start(state, &player, starting_epoch).await;
        return Err(failure);
    }

    let player_epoch = state
        .apple_music()
        .process_tap_playback_epoch()
        .ok_or_else(|| {
            PlaybackError::internal_invariant(
                "Apple Music tap committed without owning a Player epoch",
            )
        })?;
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
    state.apple_music().activate_playback(
        zone_id.to_string(),
        player_epoch,
        helper_session_id,
        queue_revision,
        helper_segment,
    );
    // Arm the next entry after the current decoder/tap handoff has settled.
    // This predates any normal automatic queue transition, unlike the helper's
    // observed `entry_changed` event, which can arrive after decoding begins.
    let next_source_format_boundary = state.apple_music().source_format_probe_boundary();
    spawn_event_monitor(
        state.clone(),
        receiver,
        zone_id.to_string(),
        player_epoch,
        next_source_format_boundary,
    );
    Ok(PlaybackOutcome::Completed)
}

pub(crate) fn active_snapshot(state: &AppState, zone_id: &str) -> Option<ApplePlaybackSnapshot> {
    let snapshot = state.apple_music().playback_snapshot_for_zone(zone_id)?;
    let player = state.zones().player_for_zone(zone_id)?;
    (player.playback_epoch() == snapshot.player_epoch).then_some(snapshot)
}

pub(crate) async fn pause(state: &AppState, zone_id: &str) -> Result<bool, PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    if active_snapshot(state, zone_id).is_none() {
        return Ok(false);
    }
    state
        .apple_music()
        .transport("pause")
        .await
        .map_err(playback_error)?;
    let player = state
        .zones()
        .player_for_zone(zone_id)
        .ok_or(PlaybackError::ZoneNotAvailable)?;
    player.pause();
    state.apple_music().update_playback_state(zone_id, "paused");
    Ok(true)
}

pub(crate) async fn resume(state: &AppState, zone_id: &str) -> Result<bool, PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    if active_snapshot(state, zone_id).is_none() {
        return Ok(false);
    }
    let player = state
        .zones()
        .player_for_zone(zone_id)
        .ok_or(PlaybackError::ZoneNotAvailable)?;
    state
        .apple_music()
        .discard_process_tap_buffer()
        .map_err(playback_error)?;
    player.flush_live_output();
    state
        .apple_music()
        .transport("resume")
        .await
        .map_err(playback_error)?;
    wait_for_prefill(
        state,
        APPLE_RESUME_PREFILL_SECS,
        APPLE_RESUME_PREFILL_MIN_SECS,
        APPLE_RESUME_PREFILL_TIMEOUT,
        None,
    )
    .await
    .map_err(playback_error)?;
    prepare_hegel_for_zone(state, zone_id).await?;
    player.resume();
    state
        .apple_music()
        .update_playback_state(zone_id, "playing");
    Ok(true)
}

pub(crate) async fn seek(
    state: &AppState,
    zone_id: &str,
    seconds: f64,
) -> Result<bool, PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    if active_snapshot(state, zone_id).is_none() {
        return Ok(false);
    }
    let player = state
        .zones()
        .player_for_zone(zone_id)
        .ok_or(PlaybackError::ZoneNotAvailable)?;
    state
        .apple_music()
        .transport("pause")
        .await
        .map_err(playback_error)?;
    player.pause();
    state
        .apple_music()
        .discard_process_tap_buffer()
        .map_err(playback_error)?;
    player.flush_live_output();
    state
        .apple_music()
        .seek_helper(seconds)
        .await
        .map_err(playback_error)?;
    state
        .apple_music()
        .transport("resume")
        .await
        .map_err(playback_error)?;
    wait_for_prefill(
        state,
        APPLE_RESUME_PREFILL_SECS,
        APPLE_RESUME_PREFILL_MIN_SECS,
        APPLE_RESUME_PREFILL_TIMEOUT,
        None,
    )
    .await
    .map_err(playback_error)?;
    player.resume();
    state
        .apple_music()
        .update_playback_state(zone_id, "playing");
    Ok(true)
}

pub(crate) async fn stop(state: &AppState, zone_id: &str) -> Result<bool, PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    let Some(snapshot) = active_snapshot(state, zone_id) else {
        let _ = state.apple_music().transport("stop").await;
        state.apple_music().stop_process_tap();
        return Ok(false);
    };
    let helper_result = state.apple_music().transport("stop").await;
    state.apple_music().stop_process_tap();
    if let Some(player) = state.zones().player_for_zone(zone_id)
        && player.playback_epoch() == snapshot.player_epoch
    {
        player.stop();
    }
    state
        .apple_music()
        .clear_playback(zone_id, Some(snapshot.player_epoch));
    helper_result.map_err(playback_error)?;
    Ok(true)
}

pub(crate) async fn skip_next_if_internal(
    state: &AppState,
    zone_id: &str,
) -> Result<bool, PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    let Some(snapshot) = active_snapshot(state, zone_id) else {
        return Ok(false);
    };
    if snapshot.current_segment_index + 1 >= snapshot.segment.len() {
        return Ok(false);
    }
    state
        .apple_music()
        .skip_next_helper()
        .await
        .map_err(playback_error)?;
    Ok(true)
}

async fn prepare_musickit_process_tap_after_playback(
    state: &AppState,
    player: std::sync::Arc<crate::audio::player::Player>,
    preexisting_renderer_pids: &[u32],
    guard: &PlaybackGuard,
) -> Result<(), PlaybackError> {
    let deadline = tokio::time::Instant::now() + APPLE_AUDIO_PROCESS_TIMEOUT;
    loop {
        ensure_current(state, guard)?;
        match state
            .apple_music()
            .prepare_musickit_process_tap(player.clone(), preexisting_renderer_pids)
        {
            Ok(_) => return Ok(()),
            Err(error)
                if should_retry_audio_process_visibility(&error)
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(APPLE_AUDIO_PROCESS_RETRY_INTERVAL).await;
            }
            Err(error) => return Err(playback_error(error)),
        }
    }
}

async fn prepare_initial_source_rate(
    state: &AppState,
    player: &std::sync::Arc<crate::audio::player::Player>,
    guard: &PlaybackGuard,
    source_format_boundary: SystemTime,
) -> Result<(), PlaybackError> {
    // MusicKit has to begin decoding before macOS exposes both its renderer
    // process and the exact lossless decoder format. Pause immediately after
    // attaching the tap so probing cannot consume the start of the song.
    state
        .apple_music()
        .transport("pause")
        .await
        .map_err(playback_error)?;
    ensure_current(state, guard)?;

    let source_format = match state
        .apple_music()
        .probe_process_tap_source_format(source_format_boundary)
        .await
    {
        Ok(Some(source_format)) => source_format,
        Ok(None) => {
            return Err(playback_error(AppleMusicMvpError {
                code: "lossless_source_format_unverified".to_string(),
                message:
                    "MusicKit reported Lossless playback, but macOS did not expose a fresh Apple Lossless decoder format. Fozmo will not label the fixed process-tap mix as the source rate."
                        .to_string(),
                retryable: true,
                stage: "source_format".to_string(),
                cleanup_complete: false,
            }));
        }
        Err(error) => return Err(playback_error(error)),
    };
    let sample_rate_hz = source_format.sample_rate_hz;
    match state
        .apple_music()
        .prepare_process_tap_rate_transition(player.clone(), source_format)
        .map_err(playback_error)?
    {
        ProcessTapRateTransition::Unchanged { .. } => {
            debug!(
                event = "apple_music_source_rate_unchanged",
                sample_rate_hz, "MusicKit tap already matches the verified lossless source rate"
            );
        }
        ProcessTapRateTransition::Prepared {
            previous_stream_rate_hz,
            source_sample_rate_hz,
        } => {
            debug!(
                event = "apple_music_source_rate_prepared",
                previous_stream_rate_hz,
                source_sample_rate_hz,
                "Prepared MusicKit PCM at the verified lossless source rate"
            );
        }
    }
    ensure_current(state, guard)?;

    restart_captured_song_from_start(state, guard).await?;
    Ok(())
}

async fn restart_captured_song_from_start(
    state: &AppState,
    guard: &PlaybackGuard,
) -> Result<(), PlaybackError> {
    state
        .apple_music()
        .transport("pause")
        .await
        .map_err(playback_error)?;
    ensure_current(state, guard)?;
    state
        .apple_music()
        .seek_helper(0.0)
        .await
        .map_err(playback_error)?;
    state
        .apple_music()
        .discard_process_tap_buffer()
        .map_err(playback_error)?;
    state
        .apple_music()
        .transport("resume")
        .await
        .map_err(playback_error)?;
    ensure_current(state, guard)
}

async fn commit_initial_process_tap(
    state: &AppState,
    player: &std::sync::Arc<crate::audio::player::Player>,
    starting_epoch: u64,
    guard: &PlaybackGuard,
) -> Result<(), PlaybackError> {
    state
        .apple_music()
        .prepare_process_tap_stream()
        .map_err(playback_error)?;
    // Preparing the decoder can consume capture-ring data while probing the
    // live WAV. Measure the cushion afterwards so commit never starts with a
    // nominal prefill that probing has already drained.
    wait_for_prefill(
        state,
        APPLE_START_PREFILL_SECS,
        APPLE_START_PREFILL_MIN_SECS,
        APPLE_START_PREFILL_TIMEOUT,
        Some(guard),
    )
    .await
    .map_err(playback_error)?;
    if !guard.is_current(state) || player.playback_epoch() != starting_epoch {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    state
        .apple_music()
        .commit_process_tap(false)
        .map_err(playback_error)?;
    Ok(())
}

fn should_retry_audio_process_visibility(error: &AppleMusicMvpError) -> bool {
    error.retryable && error.code == "process_tap_start_failed" && error.stage == "audio_process"
}

pub(crate) async fn stop_replaced_session(state: &AppState, zone_id: &str) {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    stop_replaced_session_locked(state, zone_id).await;
}

pub(crate) async fn stop_replaced_session_if_current(
    state: &AppState,
    zone_id: &str,
    guard: &PlaybackGuard,
) -> Result<(), PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    ensure_current(state, guard)?;
    stop_replaced_session_locked(state, zone_id).await;
    Ok(())
}

pub(crate) async fn stop_replaced_session_locked(state: &AppState, zone_id: &str) {
    let Some(snapshot) = state.apple_music().playback_snapshot_for_zone(zone_id) else {
        return;
    };
    let _ = state.apple_music().transport("stop").await;
    state.apple_music().stop_process_tap();
    state
        .apple_music()
        .clear_playback(zone_id, Some(snapshot.player_epoch));
}

async fn wait_for_prefill(
    state: &AppState,
    target_secs: f64,
    minimum_secs: f64,
    timeout: Duration,
    guard: Option<&PlaybackGuard>,
) -> Result<(), AppleMusicMvpError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if guard.is_some_and(|guard| !guard.is_current(state)) {
            return Err(AppleMusicMvpError {
                code: "process_tap_playback_changed".to_string(),
                message: "Playback changed while Apple Music was preparing.".to_string(),
                retryable: true,
                stage: "preparing_dsp_handoff".to_string(),
                cleanup_complete: false,
            });
        }
        let buffered = state.apple_music().process_tap_buffered_audio_secs()?;
        if buffered >= target_secs {
            debug!(
                event = "apple_music_process_tap_prefill_ready",
                buffered_ms = buffered * 1000.0,
                target_ms = target_secs * 1000.0,
                "Apple Music capture prebuffer is ready"
            );
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return if buffered >= minimum_secs {
                warn!(
                    event = "apple_music_process_tap_prefill_short",
                    buffered_ms = buffered * 1000.0,
                    target_ms = target_secs * 1000.0,
                    minimum_ms = minimum_secs * 1000.0,
                    "Apple Music capture prebuffer timed out above its safe minimum"
                );
                Ok(())
            } else {
                Err(AppleMusicMvpError {
                    code: "process_tap_prefill_timeout".to_string(),
                    message: format!(
                        "The MusicKit helper delivered only {:.0} ms of the required {:.0} ms audio prebuffer.",
                        buffered * 1000.0,
                        minimum_secs * 1000.0
                    ),
                    retryable: true,
                    stage: "preparing_dsp_handoff".to_string(),
                    cleanup_complete: false,
                })
            };
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn ensure_current(state: &AppState, guard: &PlaybackGuard) -> Result<(), PlaybackError> {
    guard
        .is_current(state)
        .then_some(())
        .ok_or_else(|| PlaybackError::conflict("Playback changed"))
}

async fn cleanup_failed_start(
    state: &AppState,
    player: &std::sync::Arc<crate::audio::player::Player>,
    starting_epoch: u64,
) {
    let owned_epoch = state.apple_music().process_tap_playback_epoch();
    let _ = state.apple_music().transport("stop").await;
    state.apple_music().stop_process_tap();
    if player.playback_epoch() != starting_epoch && owned_epoch == Some(player.playback_epoch()) {
        player.stop();
    }
}

fn spawn_event_monitor(
    state: AppState,
    mut receiver: broadcast::Receiver<HelperMessage>,
    zone_id: String,
    mut player_epoch: u64,
    mut next_source_format_boundary: SystemTime,
) {
    tokio::spawn(async move {
        let mut tap_watchdog = tokio::time::interval(Duration::from_secs(1));
        tap_watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let event = tokio::select! {
                _ = tap_watchdog.tick() => {
                    let tap = state.apple_music().status().process_tap;
                    let owns_playback = state
                        .apple_music()
                        .playback_snapshot_for_zone(&zone_id)
                        .is_some_and(|snapshot| {
                            snapshot.player_epoch == player_epoch
                                && snapshot.playback_state == "playing"
                        });
                    if owns_playback
                        && tap.state == "running"
                        && tap.metrics
                            .last_callback_age_ms
                            .is_some_and(|age| age > APPLE_TAP_STALL_TIMEOUT_MS)
                    {
                        warn!(
                            event = "apple_music_process_tap_stalled",
                            zone_id,
                            player_epoch,
                            "Apple Music process-tap callbacks stopped"
                        );
                        state.apple_music().mark_playback_failed(
                            &zone_id,
                            player_epoch,
                            AppleMusicMvpError {
                                code: "process_tap_stalled".to_string(),
                                message: "The Apple Music audio capture stopped delivering PCM."
                                    .to_string(),
                                retryable: true,
                                stage: "capturing_audio".to_string(),
                                cleanup_complete: false,
                            },
                        );
                        handle_finished(
                            state.clone(),
                            zone_id.clone(),
                            player_epoch,
                            "failed".to_string(),
                        )
                        .await;
                        break;
                    }
                    continue;
                }
                received = receiver.recv() => match received {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        handle_finished(
                            state.clone(),
                            zone_id.clone(),
                            player_epoch,
                            "failed".to_string(),
                        )
                        .await;
                        break;
                    }
                }
            };
            let effect = state.apple_music().apply_playback_event(&event);
            if effect.stale {
                continue;
            }
            for _ in 0..effect.advance_by {
                state.listening().next(state.library(), &zone_id);
            }
            if event.message_type == "entry_changed" && effect.advance_by > 0 {
                match reconfigure_for_entry_change(
                    &state,
                    &zone_id,
                    player_epoch,
                    next_source_format_boundary,
                )
                .await
                {
                    Ok(Some(replacement_epoch)) => {
                        player_epoch = replacement_epoch;
                        next_source_format_boundary =
                            state.apple_music().source_format_probe_boundary();
                    }
                    Ok(None) => {
                        next_source_format_boundary =
                            state.apple_music().source_format_probe_boundary();
                    }
                    Err(failure) => {
                        warn!(
                            event = "apple_music_entry_reconfigure_failed",
                            zone_id,
                            player_epoch,
                            error = %failure,
                            "Apple Music could not safely restart the new queue entry"
                        );
                        state.apple_music().mark_playback_failed(
                            &zone_id,
                            player_epoch,
                            AppleMusicMvpError {
                                code: "apple_music_entry_reconfigure_failed".to_string(),
                                message: failure.to_string(),
                                retryable: true,
                                stage: "source_format".to_string(),
                                cleanup_complete: false,
                            },
                        );
                        handle_finished(
                            state.clone(),
                            zone_id.clone(),
                            player_epoch,
                            "failed".to_string(),
                        )
                        .await;
                        break;
                    }
                }
            }
            if let Some(reason) = effect.finished_reason {
                handle_finished(state.clone(), zone_id.clone(), player_epoch, reason).await;
                break;
            }
        }
    });
}

async fn reconfigure_for_entry_change(
    state: &AppState,
    zone_id: &str,
    player_epoch: u64,
    source_format_boundary: SystemTime,
) -> Result<Option<u64>, PlaybackError> {
    let _playback_switch = state.apple_music().lock_playback_switch().await;
    let Some(snapshot) = state.apple_music().playback_snapshot_for_zone(zone_id) else {
        return Ok(None);
    };
    if snapshot.player_epoch != player_epoch {
        return Ok(None);
    }
    let player = state
        .zones()
        .player_for_zone(zone_id)
        .ok_or(PlaybackError::ZoneNotAvailable)?;

    state
        .apple_music()
        .transport("pause")
        .await
        .map_err(playback_error)?;
    player.pause();
    player.flush_live_output();

    let mut rate_transition_prepared = false;
    match state
        .apple_music()
        .probe_process_tap_source_format(source_format_boundary)
        .await
    {
        Ok(Some(source_format)) => {
            let sample_rate_hz = source_format.sample_rate_hz;
            match state
                .apple_music()
                .prepare_process_tap_rate_transition(player.clone(), source_format)
            {
                Ok(ProcessTapRateTransition::Prepared {
                    previous_stream_rate_hz,
                    source_sample_rate_hz,
                }) => {
                    rate_transition_prepared = true;
                    debug!(
                        event = "apple_music_entry_source_rate_prepared",
                        previous_stream_rate_hz,
                        source_sample_rate_hz,
                        "Prepared the next MusicKit entry at its native rate"
                    );
                }
                Ok(ProcessTapRateTransition::Unchanged { .. }) => {}
                Err(error) => {
                    warn!(
                        event = "apple_music_entry_source_rate_transition_skipped",
                        sample_rate_hz,
                        error = %error.message,
                        "Retaining the current tap rate for the next MusicKit entry"
                    );
                }
            }
        }
        Ok(None) => {}
        Err(error) => {
            warn!(
                event = "apple_music_entry_source_rate_probe_failed",
                error = %error.message,
                "Retaining the current tap rate for the next MusicKit entry"
            );
        }
    }

    state
        .apple_music()
        .seek_helper(0.0)
        .await
        .map_err(playback_error)?;
    state
        .apple_music()
        .discard_process_tap_buffer()
        .map_err(playback_error)?;
    state
        .apple_music()
        .transport("resume")
        .await
        .map_err(playback_error)?;

    if !rate_transition_prepared {
        wait_for_prefill(
            state,
            APPLE_RESUME_PREFILL_SECS,
            APPLE_RESUME_PREFILL_MIN_SECS,
            APPLE_RESUME_PREFILL_TIMEOUT,
            None,
        )
        .await
        .map_err(playback_error)?;
        player.resume();
        return Ok(None);
    }

    let replacement_result = async {
        state
            .apple_music()
            .prepare_process_tap_stream()
            .map_err(playback_error)?;
        wait_for_prefill(
            state,
            APPLE_START_PREFILL_SECS,
            APPLE_START_PREFILL_MIN_SECS,
            APPLE_START_PREFILL_TIMEOUT,
            None,
        )
        .await
        .map_err(playback_error)?;
        state
            .apple_music()
            .commit_process_tap(true)
            .map_err(playback_error)?;
        state
            .apple_music()
            .process_tap_playback_epoch()
            .ok_or_else(|| {
                PlaybackError::internal_invariant(
                    "Apple Music rate replacement committed without a Player epoch",
                )
            })
    }
    .await;

    let replacement_epoch = match replacement_result {
        Ok(epoch) => epoch,
        Err(first_failure) => {
            state.apple_music().cancel_process_tap_rate_transition();
            warn!(
                event = "apple_music_entry_source_rate_fallback",
                error = %first_failure,
                "Exact-rate entry handoff failed; resuming the existing MusicKit tap"
            );
            state
                .apple_music()
                .transport("pause")
                .await
                .map_err(playback_error)?;
            state
                .apple_music()
                .seek_helper(0.0)
                .await
                .map_err(playback_error)?;
            state
                .apple_music()
                .discard_process_tap_buffer()
                .map_err(playback_error)?;
            state
                .apple_music()
                .transport("resume")
                .await
                .map_err(playback_error)?;
            wait_for_prefill(
                state,
                APPLE_RESUME_PREFILL_SECS,
                APPLE_RESUME_PREFILL_MIN_SECS,
                APPLE_RESUME_PREFILL_TIMEOUT,
                None,
            )
            .await
            .map_err(playback_error)?;
            player.resume();
            return Ok(None);
        }
    };
    if !state
        .apple_music()
        .replace_playback_player_epoch(zone_id, player_epoch, replacement_epoch)
    {
        return Err(PlaybackError::conflict("Playback changed"));
    }
    Ok(Some(replacement_epoch))
}

async fn handle_finished(state: AppState, zone_id: String, player_epoch: u64, reason: String) {
    let playback_switch = state.apple_music().lock_playback_switch().await;
    let Some(snapshot) = state.apple_music().playback_snapshot_for_zone(&zone_id) else {
        return;
    };
    if snapshot.player_epoch != player_epoch {
        return;
    }
    state.apple_music().stop_process_tap();
    state
        .apple_music()
        .clear_playback(&zone_id, Some(player_epoch));

    if reason != "completed" {
        if reason == "failed" || reason == "interrupted" {
            if let Some(player) = state.zones().player_for_zone(&zone_id)
                && player.playback_epoch() == player_epoch
            {
                player.stop();
            }
            state.listening().stop(state.library(), &zone_id);
        }
        return;
    }

    let queued = state
        .library()
        .zone_queue(&zone_id)
        .map(|entries| {
            entries
                .into_iter()
                .map(|entry| entry.source)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let Some((next, rest)) = queued.split_first() else {
        if let Some(player) = state.zones().player_for_zone(&zone_id)
            && player.playback_epoch() == player_epoch
        {
            player.stop();
        }
        state.listening().stop(state.library(), &zone_id);
        return;
    };
    let profile_id = state
        .listening()
        .profile_id(&zone_id)
        .unwrap_or_else(|| crate::settings::DEFAULT_PROFILE_ID.to_string());
    let next = next.clone();
    let rest = rest.to_vec();
    // `PlaybackRouter` may acquire the same switch while stopping the old
    // provider, so release it after atomically validating/clearing this epoch.
    drop(playback_switch);
    if state
        .zones()
        .player_for_zone(&zone_id)
        .is_some_and(|player| player.playback_epoch() != player_epoch)
    {
        return;
    }
    if let Err(error) = PlaybackRouter::new(&state)
        .execute(
            &zone_id,
            PlaybackIntent::Play {
                profile_id,
                source: next,
                queue: rest,
                radio_auto: false,
                guard: PlaybackGuard::from_expected_player_epoch(zone_id.clone(), player_epoch),
                qobuz_request: None,
            },
        )
        .await
    {
        warn!(
            event = "apple_music_boundary_auto_advance_failed",
            zone_id,
            reason,
            error = %error,
            "Apple Music provider-boundary auto-advance failed"
        );
    } else {
        debug!(
            event = "apple_music_boundary_auto_advance",
            zone_id, "Apple Music provider-boundary auto-advance completed"
        );
    }
}

fn playback_error(error: AppleMusicMvpError) -> PlaybackError {
    match error.code.as_str() {
        "music_authorization_not_determined"
        | "music_authorization_denied"
        | "subscription_required"
        | "musickit_capability_unavailable"
        | "process_tap_confirmation_required" => PlaybackError::forbidden(error.code),
        "song_not_found" | "album_not_found" | "helper_missing" => {
            PlaybackError::not_found(error.code)
        }
        "process_tap_playback_changed" | "apple_music_active_segment_requires_reprepare" => {
            PlaybackError::conflict(error.code)
        }
        "apple_music_storefront_invalid" | "apple_music_seek_invalid" | "queue_prepare_failed" => {
            PlaybackError::bad_request(error.code)
        }
        _ if error.retryable => PlaybackError::retryable_network(error.message),
        _ => PlaybackError::integration(error.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::test_support::qobuz_source;

    fn apple(song_id: &str) -> SourceRef {
        SourceRef::AppleMusicTrack {
            song_id: song_id.to_string(),
            storefront: Some("nz".to_string()),
            title: Some(format!("Song {song_id}")),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            album_artist: Some("Artist".to_string()),
            album_id: Some("album-1".to_string()),
            artwork_url: None,
            duration_secs: Some(180.0),
            track_number: None,
            disc_number: None,
            isrc: None,
            radio: false,
            radio_context: None,
            playlist_context: None,
        }
    }

    #[test]
    fn provider_run_extracts_only_the_contiguous_apple_segment() {
        let current = apple("1");
        let queue = vec![apple("2"), qobuz_source(3, false), apple("4")];
        let run = provider_run(current.clone(), &queue);

        assert_eq!(run.current, current);
        assert_eq!(run.contiguous, vec![apple("1"), apple("2")]);
        assert_eq!(run.remaining, queue);
        assert_eq!(
            run.after_contiguous,
            vec![qobuz_source(3, false), apple("4")]
        );
    }

    #[test]
    fn duplicate_apple_song_ids_remain_distinct_queue_occurrences() {
        let repeated = apple("1");
        let run = provider_run(repeated.clone(), std::slice::from_ref(&repeated));
        assert_eq!(run.contiguous.len(), 2);
        assert_eq!(run.contiguous[0].key(), run.contiguous[1].key());
    }

    #[test]
    fn retries_only_transient_audio_process_visibility_failures() {
        let transient = AppleMusicMvpError {
            code: "process_tap_start_failed".to_string(),
            message: "not visible yet".to_string(),
            retryable: true,
            stage: "audio_process".to_string(),
            cleanup_complete: true,
        };
        assert!(should_retry_audio_process_visibility(&transient));

        let permission = AppleMusicMvpError {
            stage: "create_tap".to_string(),
            ..transient.clone()
        };
        assert!(!should_retry_audio_process_visibility(&permission));

        let permanent = AppleMusicMvpError {
            retryable: false,
            ..transient
        };
        assert!(!should_retry_audio_process_visibility(&permanent));
    }
}
