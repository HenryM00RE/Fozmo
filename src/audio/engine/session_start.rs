use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::audio::sinks::airplay::sender::AirPlayMetadata;

use super::buffers::{flush_dsd_tail_at_eof, write_audio_blocking};
use super::decode::{flush_dsd_staged_pcm_at_eof, flush_dsd_upsampler_tail_at_eof};
use super::dsd_path::{DsdFallbackKey, build_renderer, force_44k_family_for_existing_carrier};
#[cfg(any(target_os = "macos", target_os = "windows", test))]
use super::dsd_path::{dop_wire_rate_for_mode, should_force_44k_family_dsd256};
use super::output_stream::{ActiveOutput, drop_active_stream_for_reopen};
use super::render::{eq_processing_rate, output_headroom_gain, render_pcm_dsp_path_eof_tail};
use super::session::{init_pending_start_session, publish_started_session_metadata};
use super::signal_path::{OutputMode, dsd_policy_for_source, effective_dsd_target_rate};
use super::state::{FLUSH_REASON_PENDING_START, PLAYBACK_STARTING, REOPEN_REASON_PENDING_START};
use super::worker_state::WorkerRuntime;
use super::worker_status::{
    publish_start_failure, reset_dsd_buffer_watermark, stop_after_failed_start,
};

pub(super) enum PendingStartResult {
    Handled,
    StaleEpoch,
}

pub(super) fn install_pending_start(runtime: &mut WorkerRuntime) -> PendingStartResult {
    let shared = &runtime.shared;
    let playback = &mut runtime.playback;
    let output = &mut runtime.output;
    let buffers = &mut runtime.buffers;
    let config = &mut runtime.config;
    let pending_start = &mut playback.pending_start;
    let reopen_output_for_pending_start = &mut playback.reopen_output_for_pending_start;
    let use_transition_preroll = &mut playback.use_transition_preroll;
    let pending_start_gapless = &mut playback.pending_start_gapless;
    let gapless_dsp_path = &mut playback.gapless_dsp_path;
    let active_stream = &mut output.active_stream;
    let audio_prod = &mut buffers.prod;
    let dsd_state = &mut buffers.dsd_state;
    let dsd_fallback_key = &mut output.dsd_fallback_key;
    let next_stream_retry = &mut output.next_stream_retry;
    let queues = &mut playback.queues;
    let filter_type = config.filter_type;
    let configured_target_rate = config.configured_target_rate;
    let upsampling_enabled = config.upsampling_enabled;
    let active_device_name = output.active_device_name.as_deref();
    let output_mode = config.output_mode;
    let dsd_modulator = config.dsd_modulator;
    let dsd_isi_penalty = config.dsd_isi_penalty;
    let dsd_rules = &config.dsd_rules;
    let target_rate = &mut config.target_rate;
    let eq_processor = &mut config.eq_processor;
    let current_eq_config = &config.current_eq_config;
    let session = &mut playback.session;
    let session_epoch = &mut playback.session_epoch;
    let current_file_path = &mut playback.current_file_path;
    let current_fallback_tags = &mut playback.current_fallback_tags;

    let Some(start) = pending_start.take() else {
        return PendingStartResult::Handled;
    };

    let start_epoch = start.epoch();
    if start_epoch != shared.playback_epoch.load(Ordering::Relaxed) {
        // The dropped start may have been a stream item popped at EOF, which
        // set the auto-advance-in-flight flag; leaving it set would block the
        // monitor's queue auto-advance forever if no newer command lands.
        *pending_start_gapless = false;
        *gapless_dsp_path = None;
        queues.clear_stream_auto_advance_pending();
        return PendingStartResult::StaleEpoch;
    }

    let active_stream_ready_for_gapless = active_stream
        .as_ref()
        .map(|stream| stream.reset_notice().is_none())
        .unwrap_or(false);
    let (requested_gapless_start, reopen_output) = pending_start_boundary_plan(
        *pending_start_gapless,
        *reopen_output_for_pending_start,
        active_stream_ready_for_gapless,
    );
    reset_dsd_fallback_for_pending_start(reopen_output, dsd_fallback_key);
    *reopen_output_for_pending_start = false;
    *pending_start_gapless = false;
    *use_transition_preroll = false;
    if !requested_gapless_start {
        prepare_pending_start_boundary(
            reopen_output,
            active_stream,
            dsd_state,
            next_stream_retry,
            &shared.state,
        );
    }

    let pending_init = init_pending_start_session(
        start,
        filter_type,
        configured_target_rate,
        upsampling_enabled,
        active_device_name,
    );
    *current_file_path = pending_init.current_file_path;
    *current_fallback_tags = pending_init.current_fallback_tags;
    *shared.file_name.lock().unwrap() = Some(pending_init.display_name.clone());
    let start_position_secs = pending_init.start_position_secs;

    match pending_init.result {
        Ok((mut new_session, tags, cover)) => {
            let mut continuous_start = requested_gapless_start;
            let mut dsp_path_reused = false;
            let mut previous_pcm_dsp_tail = None;
            if requested_gapless_start {
                if let Some(previous_dsp_path) = gapless_dsp_path.take() {
                    if previous_dsp_path.is_gapless_compatible_with(&new_session.dsp_path) {
                        new_session.dsp_path = previous_dsp_path;
                        dsp_path_reused = true;
                    } else {
                        previous_pcm_dsp_tail = Some(previous_dsp_path);
                    }
                }
            } else {
                *gapless_dsp_path = None;
            }

            let session_rates = publish_started_session_metadata(
                &new_session,
                tags,
                cover,
                pending_init.fallback_cover,
                pending_init.fallback_tags,
                &shared.state,
                &shared.track_tags,
                &shared.track_cover,
                &shared.cover_version,
            );
            let new_source = session_rates.source_rate;
            let new_target = session_rates.target_rate;
            println!(
                "AudioWorker: Session ready — source {} Hz → target {} Hz",
                new_source, new_target,
            );
            if crate::audio::debug::audio_debug_enabled() {
                eprintln!(
                    "AudioWorker DEBUG: session config: output_mode={} filter={} configured_target={} upsampling={} device={:?} dsd_modulator={} lookahead={} isi_penalty={:.5} dsd_rules={}",
                    output_mode.as_name(),
                    filter_type.as_name(),
                    configured_target_rate,
                    upsampling_enabled,
                    active_device_name,
                    dsd_modulator.as_name(),
                    dsd_modulator.lookahead_depth(),
                    dsd_isi_penalty,
                    dsd_rules.len(),
                );
            }
            if output_mode.is_dsd() {
                let dsd_policy =
                    dsd_policy_for_source(output_mode, filter_type, new_source, dsd_rules);
                let mut dsd_state_match = dsd_state
                    .as_ref()
                    .map(|state| {
                        dsd_state_matches_pending_start(state, dsd_policy.mode, new_source)
                    })
                    .unwrap_or(false);
                if !dsd_state_match
                    && continuous_start
                    && let Some((wire_rate, force_44k_family)) = dsd_state
                        .as_ref()
                        .filter(|state| {
                            state.mode == dsd_policy.mode
                                && active_stream.as_ref().is_some_and(|stream| {
                                    stream.supports_continuous_dsd_renderer_swap()
                                        && stream.reset_notice().is_none()
                                })
                        })
                        .and_then(|state| {
                            force_44k_family_for_existing_carrier(
                                dsd_policy.mode,
                                new_source,
                                state.wire_rate,
                            )
                            .map(|force| (state.wire_rate, force))
                        })
                    && let Ok(renderer) = build_renderer(
                        dsd_policy.filter_type,
                        new_source,
                        dsd_policy.mode,
                        force_44k_family,
                        dsd_modulator,
                        dsd_isi_penalty,
                    )
                    && let Some(state) = dsd_state.as_mut()
                {
                    let mut output_can_continue = || {
                        !shared.shutdown_requested()
                            && shared.state.state.load(Ordering::Relaxed)
                                == super::state::PLAYBACK_PLAYING
                            && active_stream
                                .as_ref()
                                .is_some_and(|stream| stream.reset_notice().is_none())
                    };
                    flush_dsd_staged_pcm_at_eof(
                        state,
                        &shared.state,
                        eq_processor,
                        &mut output_can_continue,
                    );
                    flush_dsd_upsampler_tail_at_eof(state, &shared.state, &mut output_can_continue);
                    flush_dsd_tail_at_eof(state, &mut output_can_continue);
                    if output_can_continue() {
                        state.reconfigure_renderer_for_continuous_carrier(
                            renderer,
                            new_source,
                            wire_rate,
                            dsd_policy.mode,
                        );
                        shared
                            .state
                            .modulator_reset_count
                            .fetch_add(1, Ordering::Relaxed);
                        eq_processor.reset();
                        dsd_state_match = true;
                        if crate::audio::debug::audio_debug_enabled() {
                            eprintln!(
                                "AudioWorker DEBUG: retained {} Hz DSD carrier across {} Hz source-rate handoff",
                                wire_rate, new_source,
                            );
                        }
                    }
                }
                if !dsd_state_match {
                    if continuous_start {
                        prepare_pending_start_boundary(
                            reopen_output,
                            active_stream,
                            dsd_state,
                            next_stream_retry,
                            &shared.state,
                        );
                        continuous_start = false;
                    }
                    shared
                        .state
                        .record_reopen_reason(REOPEN_REASON_PENDING_START);
                    drop_active_stream_for_reopen(active_stream, &shared.state);
                    *dsd_state = None;
                    *dsd_fallback_key = None;
                    shared.state.request_flush(FLUSH_REASON_PENDING_START);
                }
                let active_dsd = dsd_state.as_ref().filter(|state| {
                    state.source_rate == new_source && state.mode == dsd_policy.mode
                });
                *target_rate = effective_dsd_target_rate(
                    dsd_policy.mode,
                    active_dsd.map(|state| state.mode),
                    active_dsd.map(|state| state.wire_rate),
                    new_source,
                    new_target,
                );
            } else {
                if continuous_start
                    && new_target == *target_rate
                    && let Some(mut previous_dsp_path) = previous_pcm_dsp_tail.take()
                {
                    let mut tail = Vec::new();
                    let volume = f32::from_bits(shared.state.volume.load(Ordering::Relaxed)) as f64;
                    let headroom = output_headroom_gain(f32::from_bits(
                        shared.state.headroom_db.load(Ordering::Relaxed),
                    ));
                    if render_pcm_dsp_path_eof_tail(
                        &mut previous_dsp_path,
                        &mut tail,
                        headroom,
                        volume,
                        *target_rate,
                        &shared.state,
                        eq_processor,
                    ) {
                        write_audio_blocking(audio_prod, &tail, || {
                            !shared.shutdown_requested()
                                && shared.state.state.load(Ordering::Relaxed)
                                    == super::state::PLAYBACK_PLAYING
                                && active_stream
                                    .as_ref()
                                    .is_some_and(|stream| stream.reset_notice().is_none())
                                && !shared.state.flush_buffer.load(Ordering::Relaxed)
                        });
                    }
                }
                if new_target != *target_rate {
                    if continuous_start {
                        prepare_pending_start_boundary(
                            reopen_output,
                            active_stream,
                            dsd_state,
                            next_stream_retry,
                            &shared.state,
                        );
                        continuous_start = false;
                    }
                    shared
                        .state
                        .record_reopen_reason(REOPEN_REASON_PENDING_START);
                    drop_active_stream_for_reopen(active_stream, &shared.state);
                    shared.state.request_flush(FLUSH_REASON_PENDING_START);
                }
                *target_rate = new_target;
                shared
                    .state
                    .active_filter_type
                    .store(filter_type.as_id(), Ordering::Relaxed);
            }
            eq_processor.update(
                eq_processing_rate(output_mode, new_source, *target_rate),
                current_eq_config,
            );
            if !continuous_start {
                eq_processor.reset();
            }
            record_pending_start_dsp_graph_install(&shared.state, dsp_path_reused);
            shared
                .state
                .target_rate
                .store(*target_rate, Ordering::Relaxed);
            if let Some(position_secs) = start_position_secs {
                shared.state.position_samples.store(
                    (position_secs.max(0.0) * f64::from(*target_rate)) as u64,
                    Ordering::Relaxed,
                );
            }

            *use_transition_preroll =
                should_use_transition_preroll(continuous_start, active_stream.is_some());
            *session = Some(new_session);
            *session_epoch = start_epoch;
            if continuous_start {
                shared
                    .state
                    .state
                    .store(super::state::PLAYBACK_PLAYING, Ordering::Relaxed);
                *use_transition_preroll = false;
                if crate::audio::debug::audio_debug_enabled() {
                    eprintln!(
                        "AudioWorker DEBUG: installed continuous session without draining output"
                    );
                }
            } else {
                shared
                    .state
                    .state
                    .store(PLAYBACK_STARTING, Ordering::Relaxed);
            }
            queues.clear_stream_auto_advance_pending();
            if let Some(ActiveOutput::AirPlayRaop(stream)) = active_stream.as_ref() {
                stream.set_metadata(AirPlayMetadata::from_player(
                    shared.file_name.lock().unwrap().clone(),
                    shared.track_tags.lock().unwrap().clone(),
                    shared.track_cover.lock().unwrap().clone(),
                ));
            }
        }
        Err(e) => {
            if requested_gapless_start {
                prepare_pending_start_boundary(
                    reopen_output,
                    active_stream,
                    dsd_state,
                    next_stream_retry,
                    &shared.state,
                );
            }
            *gapless_dsp_path = None;
            *pending_start_gapless = false;
            eprintln!("AudioWorker: Failed to initialize session: {:?}", e);
            publish_start_failure(
                &shared.file_name,
                &shared.track_tags,
                &shared.track_cover,
                &shared.cover_version,
                e.as_ref(),
            );
            // Try the next queued track so a single bad file doesn't halt the queue.
            if start_epoch == shared.playback_epoch.load(Ordering::Relaxed) {
                *pending_start = queues.pop_next_start(start_epoch);
                *gapless_dsp_path = None;
                *pending_start_gapless = false;
                if pending_start.is_none() {
                    stop_after_failed_start(&shared.state);
                }
            } else {
                queues.clear_stream_auto_advance_pending();
                stop_after_failed_start(&shared.state);
            }
        }
    }

    PendingStartResult::Handled
}

fn pending_start_boundary_plan(
    pending_start_gapless: bool,
    reopen_output_for_pending_start: bool,
    active_stream_ready_for_gapless: bool,
) -> (bool, bool) {
    let requested_gapless_start = pending_start_gapless && active_stream_ready_for_gapless;
    let reopen_output = if requested_gapless_start {
        false
    } else {
        reopen_output_for_pending_start
    };
    (requested_gapless_start, reopen_output)
}

fn reset_dsd_fallback_for_pending_start(
    reopen_output: bool,
    dsd_fallback_key: &mut Option<DsdFallbackKey>,
) {
    // A fallback records one failed open attempt, not a permanent device
    // capability decision. An explicit track start is a fresh recovery
    // boundary (notably after a USB DAC wakes or reconnects), so retry the
    // requested DSD path instead of pinning the new track to PCM.
    if reopen_output {
        *dsd_fallback_key = None;
    }
}

fn dsd_state_matches_pending_start(
    state: &super::buffers::DsdWorkerState,
    mode: OutputMode,
    source_rate: u32,
) -> bool {
    state.source_rate == source_rate
        && state.mode == mode
        && dsd_wire_rate_for_pending_start(mode, source_rate) == Some(state.wire_rate)
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn dsd_wire_rate_for_pending_start(mode: OutputMode, source_rate: u32) -> Option<u32> {
    dop_wire_rate_for_mode(
        mode,
        source_rate,
        should_force_44k_family_dsd256(mode, source_rate),
    )
}

#[cfg(not(any(target_os = "macos", target_os = "windows", test)))]
fn dsd_wire_rate_for_pending_start(mode: OutputMode, source_rate: u32) -> Option<u32> {
    mode.dsd_wire_rate(source_rate)
}

fn prepare_pending_start_boundary(
    reopen_output: bool,
    active_stream: &mut Option<ActiveOutput>,
    dsd_state: &mut Option<super::buffers::DsdWorkerState>,
    next_stream_retry: &mut Instant,
    state: &super::state::AtomicPlayerState,
) {
    if crate::audio::debug::audio_debug_enabled() {
        eprintln!(
            "AudioWorker DEBUG: pending-start boundary before: reopen_output={} {}",
            reopen_output,
            state.diagnostics_debug_summary()
        );
    }
    // Stop the callback from consuming program audio while the old output ring
    // is being drained and the new session is being prepared.
    state.state.store(PLAYBACK_STARTING, Ordering::Relaxed);
    state.begin_startup_diagnostics();
    // Drop any tail samples from the previous track still sitting in the ring buffer.
    state.request_flush(FLUSH_REASON_PENDING_START);
    if reopen_output
        && active_stream
            .as_ref()
            .map(ActiveOutput::should_reopen_on_interrupted_track_change)
            .unwrap_or(false)
    {
        // Some backends may already have device buffers filled with the previous track.
        state.record_reopen_reason(REOPEN_REASON_PENDING_START);
        drop_active_stream_for_reopen(active_stream, state);
        *dsd_state = None;
        *next_stream_retry = Instant::now();
    }
    state.underrun_events.store(0, Ordering::Relaxed);
    state.underrun_samples.store(0, Ordering::Relaxed);
    state.reset_signal_level_metrics();
    reset_dsd_buffer_watermark(state);
    if let Some(ds) = dsd_state.as_mut() {
        ds.reset_for_playback_boundary_with_diagnostics(state);
    }
    state.dsd_overbudget_blocks.store(0, Ordering::Relaxed);
    state
        .dsd_last_load
        .store(0.0f32.to_bits(), Ordering::Relaxed);
    state
        .dsd_recent_load_p95
        .store(0.0f32.to_bits(), Ordering::Relaxed);
    state
        .dsd_recent_load_p99
        .store(0.0f32.to_bits(), Ordering::Relaxed);
    if crate::audio::debug::audio_debug_enabled() {
        eprintln!(
            "AudioWorker DEBUG: pending-start boundary after: reopen_output={} {}",
            reopen_output,
            state.diagnostics_debug_summary()
        );
    }
}

fn record_pending_start_dsp_graph_install(
    state: &super::state::AtomicPlayerState,
    dsp_path_reused: bool,
) {
    if !dsp_path_reused {
        state
            .dsp_graph_rebuild_count
            .fetch_add(1, Ordering::Relaxed);
    }
}

fn should_use_transition_preroll(gapless_start: bool, has_active_stream: bool) -> bool {
    !gapless_start && has_active_stream
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dsd::dsd_render::{DsdRate, DsdRenderer};
    use crate::audio::dsp::resampler::FilterType;
    use crate::audio::engine::dsd_path::DsdFallbackKey;

    fn test_dsd_state(source_rate: u32, mode: OutputMode) -> super::super::buffers::DsdWorkerState {
        let dsd_rate = match mode {
            OutputMode::Dsd64 => DsdRate::Dsd64,
            OutputMode::Dsd128 => DsdRate::Dsd128,
            OutputMode::Dsd256 => DsdRate::Dsd256,
            OutputMode::Pcm => panic!("test helper requires DSD mode"),
        };
        test_dsd_state_with_wire_rate(
            source_rate,
            mode,
            dsd_rate.wire_rate_for_source(source_rate).unwrap(),
        )
    }

    fn test_dsd_state_with_wire_rate(
        source_rate: u32,
        mode: OutputMode,
        wire_rate: u32,
    ) -> super::super::buffers::DsdWorkerState {
        let dsd_rate = match mode {
            OutputMode::Dsd64 => DsdRate::Dsd64,
            OutputMode::Dsd128 => DsdRate::Dsd128,
            OutputMode::Dsd256 => DsdRate::Dsd256,
            OutputMode::Pcm => panic!("test helper requires DSD mode"),
        };
        let renderer = DsdRenderer::new(FilterType::Minimum16k, source_rate, dsd_rate)
            .expect("calibrated test renderer");
        super::super::dsd_path::new_dop_worker_state(renderer, source_rate, wire_rate, mode, 0)
    }

    #[test]
    fn dsd_pending_start_match_keeps_compatible_carrier() {
        let state = test_dsd_state(44_100, OutputMode::Dsd128);

        assert!(dsd_state_matches_pending_start(
            &state,
            OutputMode::Dsd128,
            44_100
        ));
    }

    #[test]
    fn dsd_pending_start_match_rejects_different_source_or_mode() {
        let state = test_dsd_state(44_100, OutputMode::Dsd128);

        assert!(!dsd_state_matches_pending_start(
            &state,
            OutputMode::Dsd128,
            48_000
        ));
        assert!(!dsd_state_matches_pending_start(
            &state,
            OutputMode::Dsd256,
            44_100
        ));
    }

    #[test]
    fn dsd_pending_start_match_accepts_forced_44k_family_dsd256_carrier() {
        let state = test_dsd_state_with_wire_rate(48_000, OutputMode::Dsd256, 11_289_600);

        assert!(dsd_state_matches_pending_start(
            &state,
            OutputMode::Dsd256,
            48_000
        ));
    }

    #[test]
    fn gapless_pending_start_ignores_stale_reopen_latch() {
        assert_eq!(pending_start_boundary_plan(true, true, true), (true, false));
        assert_eq!(
            pending_start_boundary_plan(true, true, false),
            (false, true)
        );
        assert_eq!(
            pending_start_boundary_plan(false, true, true),
            (false, true)
        );
    }

    #[test]
    fn explicit_pending_start_retries_a_previous_dsd_fallback() {
        let mut fallback = Some(DsdFallbackKey::new(
            Some("Hegel USB".to_string()),
            OutputMode::Dsd128,
            44_100,
        ));

        reset_dsd_fallback_for_pending_start(true, &mut fallback);

        assert!(fallback.is_none());
    }

    #[test]
    fn gapless_pending_start_preserves_dsd_fallback_state() {
        let expected =
            DsdFallbackKey::new(Some("Hegel USB".to_string()), OutputMode::Dsd128, 44_100);
        let mut fallback = Some(expected.clone());

        reset_dsd_fallback_for_pending_start(false, &mut fallback);

        assert_eq!(fallback.as_ref(), Some(&expected));
    }

    #[test]
    fn diagnostics_count_new_dsp_graph_installs_only() {
        let state = super::super::state::AtomicPlayerState::new();

        record_pending_start_dsp_graph_install(&state, true);
        assert_eq!(
            state.dsp_graph_rebuild_count.load(Ordering::Relaxed),
            0,
            "DSP-path reuse should not count as a graph rebuild"
        );

        record_pending_start_dsp_graph_install(&state, false);
        assert_eq!(state.dsp_graph_rebuild_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn transition_preroll_is_used_for_non_gapless_boundaries_on_open_stream() {
        assert!(should_use_transition_preroll(false, true));
        assert!(!should_use_transition_preroll(true, true));
        assert!(!should_use_transition_preroll(false, false));
    }
}
