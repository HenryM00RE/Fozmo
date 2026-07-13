use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::audio::sinks::airplay;

use super::commands::{PlayerCommand, QueueItem, StreamQueueItem};
use super::output_stream::{
    ActiveOutput, drop_active_stream_for_reopen, reset_output_pipeline_for_reopen,
};
use super::render::eq_processing_rate;
use super::session::{
    apply_seek_to_session, reconfigure_current_session, restart_current_file_session,
};
use super::signal_path::{OutputMode, dsd_policy_for_source};
use super::state::{
    PLAYBACK_PAUSED, PLAYBACK_PLAYING, PLAYBACK_STARTING, PLAYBACK_STOPPED, REOPEN_REASON_DSD_ISI,
    REOPEN_REASON_DSD_MODULATOR, REOPEN_REASON_DSD_RULES, REOPEN_REASON_EXTERNAL_DEVICE_READY,
    REOPEN_REASON_SEEK, REOPEN_REASON_SELECT_DEVICE, REOPEN_REASON_SET_OUTPUT_MODE,
    REOPEN_REASON_UPDATE_CONFIG,
};
use super::worker_state::WorkerRuntime;
use super::worker_status::{
    full_stop_and_clear_now_playing, publish_config_status, publish_output_notice,
    stop_and_clear_now_playing,
};

pub(super) fn handle_worker_command(cmd: PlayerCommand, runtime: &mut WorkerRuntime) {
    let shared = &runtime.shared;
    let playback = &mut runtime.playback;
    let output = &mut runtime.output;
    let buffers = &mut runtime.buffers;
    let config = &mut runtime.config;
    let queues = &mut playback.queues;
    let repeat_one = &mut playback.repeat_one;
    let pending_start = &mut playback.pending_start;
    let reopen_output_for_pending_start = &mut playback.reopen_output_for_pending_start;
    let use_transition_preroll = &mut playback.use_transition_preroll;
    let pending_start_gapless = &mut playback.pending_start_gapless;
    let gapless_dsp_path = &mut playback.gapless_dsp_path;
    let session = &mut playback.session;
    let current_file_path = &mut playback.current_file_path;
    let current_fallback_tags = &mut playback.current_fallback_tags;
    let active_device_name = &mut output.active_device_name;
    let active_stream = &mut output.active_stream;
    let active_stream_opened_at = &mut output.active_stream_opened_at;
    let dsd_state = &mut buffers.dsd_state;
    let dsd_fallback_key = &mut output.dsd_fallback_key;
    let next_stream_retry = &mut output.next_stream_retry;
    let filter_type = &mut config.filter_type;
    let configured_target_rate = &mut config.configured_target_rate;
    let upsampling_enabled = &mut config.upsampling_enabled;
    let target_rate = &mut config.target_rate;
    let exclusive_mode = &mut config.exclusive_mode;
    let dsp_buffer_ms = &mut config.dsp_buffer_ms;
    let output_mode = &mut config.output_mode;
    let dsd_modulator = &mut config.dsd_modulator;
    let dsd_isi_penalty = &mut config.dsd_isi_penalty;
    let current_eq_config = &mut config.current_eq_config;
    let eq_processor = &mut config.eq_processor;
    let dsd_rules = &mut config.dsd_rules;
    #[cfg(all(target_os = "windows", feature = "asio"))]
    let native_dsd_failed_attempts = &mut output.native_dsd_failed_attempts;

    match cmd {
        PlayerCommand::Play {
            epoch,
            file_path,
            fallback_cover,
            fallback_tags,
            queue: new_queue,
        } => {
            if epoch != shared.playback_epoch.load(Ordering::Relaxed) {
                return;
            }
            *pending_start = Some(queues.replace_for_file_start(
                QueueItem {
                    file_path,
                    fallback_cover,
                    fallback_tags,
                },
                new_queue,
                epoch,
            ));
            *reopen_output_for_pending_start = true;
            *pending_start_gapless = false;
            *gapless_dsp_path = None;
        }
        PlayerCommand::PlayStream {
            epoch,
            source,
            ext_hint,
            display_name,
            fallback_cover,
            fallback_tags,
            queue: new_queue,
        } => {
            // Streams replace the file queue and publish their own follow-up stream queue.
            if epoch != shared.playback_epoch.load(Ordering::Relaxed) {
                return;
            }
            *pending_start = Some(queues.replace_for_stream_start(
                StreamQueueItem {
                    source,
                    ext_hint,
                    display_name,
                    fallback_cover,
                    fallback_tags,
                },
                new_queue,
                epoch,
            ));
            *reopen_output_for_pending_start = true;
            *pending_start_gapless = false;
            *gapless_dsp_path = None;
        }
        PlayerCommand::Next { epoch } => {
            if epoch != shared.playback_epoch.load(Ordering::Relaxed) {
                return;
            }
            if let Some(start) = queues.pop_next_start(epoch) {
                *pending_start = Some(start);
                *reopen_output_for_pending_start = true;
                *pending_start_gapless = false;
                *gapless_dsp_path = None;
            } else {
                if let Some(ds) = dsd_state.as_mut() {
                    ds.reset_for_playback_boundary_with_diagnostics(&shared.state);
                }
                *session = None;
                *pending_start_gapless = false;
                *gapless_dsp_path = None;
                *current_file_path = None;
                *current_fallback_tags = None;
                stop_and_clear_now_playing(
                    &shared.file_name,
                    &shared.track_tags,
                    &shared.track_cover,
                    &shared.cover_version,
                    &shared.state,
                );
            }
        }
        PlayerCommand::SetQueue {
            queue: new_queue,
            expected_epoch,
        } => {
            if expected_epoch
                .is_some_and(|epoch| shared.playback_epoch.load(Ordering::Relaxed) != epoch)
            {
                return;
            }
            // Wholesale replace; caller already computed the correct upcoming list.
            queues.replace_file_queue(new_queue);
        }
        PlayerCommand::SetStreamQueue {
            queue: new_queue,
            expected_current,
            expected_epoch,
        } => {
            let matches_current = expected_current
                .as_ref()
                .map(|expected| {
                    shared.file_name.lock().unwrap().as_deref() == Some(expected.as_str())
                })
                .unwrap_or(true);
            let matches_epoch = expected_epoch
                .map(|epoch| shared.playback_epoch.load(Ordering::Relaxed) == epoch)
                .unwrap_or(true);
            if matches_current && matches_epoch {
                queues.replace_stream_queue(new_queue);
            }
        }
        PlayerCommand::SetRepeatOne {
            repeat_one: new_repeat_one,
        } => {
            *repeat_one = new_repeat_one;
        }
        PlayerCommand::Pause => {
            if matches!(
                shared.state.state.load(Ordering::Relaxed),
                PLAYBACK_PLAYING | PLAYBACK_STARTING
            ) {
                shared.state.state.store(PLAYBACK_PAUSED, Ordering::Relaxed);
                if dsd_state.is_some()
                    && let Some(sess) = session
                {
                    let seconds = current_playback_seconds(&shared.state, *target_rate);
                    if apply_seek_to_session(
                        sess,
                        seconds,
                        &shared.state,
                        dsd_state.as_mut(),
                        eq_processor,
                        *target_rate,
                    ) {
                        shared.state.state.store(PLAYBACK_PAUSED, Ordering::Relaxed);
                        if active_stream
                            .as_ref()
                            .map(ActiveOutput::should_reopen_on_interrupted_track_change)
                            .unwrap_or(false)
                        {
                            shared.state.record_reopen_reason(REOPEN_REASON_SEEK);
                            reset_output_pipeline_for_reopen(
                                active_stream,
                                dsd_state,
                                dsd_fallback_key,
                                &shared.state,
                                true,
                            );
                            *next_stream_retry = Instant::now();
                        }
                    }
                }
            }
        }
        PlayerCommand::Resume => {
            if shared.state.state.load(Ordering::Relaxed) == PLAYBACK_PAUSED {
                let next_state = if session.is_some() {
                    PLAYBACK_STARTING
                } else {
                    PLAYBACK_PLAYING
                };
                shared.state.state.store(next_state, Ordering::Relaxed);
            }
        }
        PlayerCommand::Stop { epoch } => {
            if epoch != shared.playback_epoch.load(Ordering::Relaxed) {
                return;
            }
            queues.clear_all();
            *pending_start = None;
            *pending_start_gapless = false;
            *gapless_dsp_path = None;
            if let Some(ds) = dsd_state.as_mut() {
                ds.reset_for_playback_boundary_with_diagnostics(&shared.state);
            }
            *session = None;
            *current_file_path = None;
            *current_fallback_tags = None;
            full_stop_and_clear_now_playing(
                &shared.file_name,
                &shared.track_tags,
                &shared.track_cover,
                &shared.cover_version,
                &shared.state,
            );
        }
        PlayerCommand::Seek { seconds } => {
            if let Some(sess) = session {
                let reopen_dsd_boundary = dsd_state.is_some()
                    && active_stream
                        .as_ref()
                        .map(ActiveOutput::should_reopen_on_interrupted_track_change)
                        .unwrap_or(false);
                if shared.state.state.load(Ordering::Relaxed) == PLAYBACK_PAUSED
                    || reopen_dsd_boundary
                {
                    let seek_ok = apply_seek_to_session(
                        sess,
                        seconds,
                        &shared.state,
                        dsd_state.as_mut(),
                        eq_processor,
                        *target_rate,
                    );
                    if seek_ok && reopen_dsd_boundary {
                        shared.state.record_reopen_reason(REOPEN_REASON_SEEK);
                        reset_output_pipeline_for_reopen(
                            active_stream,
                            dsd_state,
                            dsd_fallback_key,
                            &shared.state,
                            true,
                        );
                        *next_stream_retry = Instant::now();
                    }
                } else {
                    sess.seek_request = Some(seconds);
                }
            }
        }
        PlayerCommand::UpdateConfig {
            filter_type: new_filter,
            target_rate: new_rate,
            upsampling_enabled: new_upsampling_enabled,
            exclusive: new_excl,
            dsp_buffer_ms: new_dsp_buffer_ms,
        } => {
            if *filter_type == new_filter
                && *configured_target_rate == new_rate
                && *upsampling_enabled == new_upsampling_enabled
                && *exclusive_mode == new_excl
                && *dsp_buffer_ms == new_dsp_buffer_ms
            {
                return;
            }

            println!("AudioWorker: Config changed. Re-opening output device.");
            let resume_state = shared.state.state.load(Ordering::Relaxed);
            let resume_seconds = current_playback_seconds(&shared.state, *target_rate);
            *filter_type = new_filter;
            *configured_target_rate = new_rate;
            *upsampling_enabled = new_upsampling_enabled;
            *exclusive_mode = new_excl;
            *dsp_buffer_ms = new_dsp_buffer_ms;

            publish_config_status(
                &shared.state,
                *filter_type,
                *configured_target_rate,
                *upsampling_enabled,
                *exclusive_mode,
                *dsp_buffer_ms,
                *output_mode,
            );

            // Force stream re-initialization.
            shared
                .state
                .record_reopen_reason(REOPEN_REASON_UPDATE_CONFIG);
            drop_active_stream_for_reopen(active_stream, &shared.state);
            *dsd_fallback_key = None;

            match restart_current_file_session(
                current_file_path.as_deref(),
                *filter_type,
                *configured_target_rate,
                *upsampling_enabled,
                active_device_name.as_deref(),
                current_fallback_tags.clone(),
                &shared.state,
                &shared.track_tags,
                &shared.track_cover,
                &shared.cover_version,
                eq_processor,
                current_eq_config,
                true,
                Some(resume_seconds),
            ) {
                Ok(Some((new_session, new_target_rate))) => {
                    *session = Some(new_session);
                    *target_rate = new_target_rate;
                    if resume_state == PLAYBACK_PAUSED {
                        shared.state.state.store(PLAYBACK_PAUSED, Ordering::Relaxed);
                    }
                }
                Ok(None) => {
                    if let Some(sess) = session {
                        *target_rate = reconfigure_current_session(
                            sess,
                            *filter_type,
                            *configured_target_rate,
                            *upsampling_enabled,
                            active_device_name.as_deref(),
                            &shared.state,
                            eq_processor,
                            current_eq_config,
                        );
                    } else if *configured_target_rate != 0 {
                        *target_rate = *configured_target_rate;
                        shared
                            .state
                            .target_rate
                            .store(*target_rate, Ordering::Relaxed);
                    }
                }
                Err(_) => {}
            }
        }
        PlayerCommand::ApplyPlaybackConfig { config: new_config } => {
            let config_changed = *filter_type != new_config.filter_type
                || *configured_target_rate != new_config.target_rate
                || *upsampling_enabled != new_config.upsampling_enabled
                || *exclusive_mode != new_config.exclusive
                || *dsp_buffer_ms != new_config.dsp_buffer_ms;
            let output_mode_changed =
                output_mode_change_requires_reopen(*output_mode, new_config.output_mode);
            let dsd_renderer_changed = *dsd_modulator != new_config.dsd_modulator
                || (*dsd_isi_penalty - new_config.dsd_isi_penalty).abs() >= f32::EPSILON
                || *dsd_rules != new_config.dsd_rules;
            let should_reopen = config_changed
                || output_mode_changed
                || (new_config.output_mode.is_dsd() && dsd_renderer_changed);
            let eq_changed = new_config.eq.is_some();

            if !should_reopen && !eq_changed && !dsd_renderer_changed {
                return;
            }

            let previous_state = shared.state.state.load(Ordering::Relaxed);
            let resume_seconds = current_playback_seconds(&shared.state, *target_rate);

            *filter_type = new_config.filter_type;
            *configured_target_rate = new_config.target_rate;
            *upsampling_enabled = new_config.upsampling_enabled;
            *exclusive_mode = new_config.exclusive;
            *dsp_buffer_ms = new_config.dsp_buffer_ms;
            *output_mode = new_config.output_mode;
            *dsd_modulator = new_config.dsd_modulator;
            *dsd_isi_penalty = new_config.dsd_isi_penalty;
            *dsd_rules = new_config.dsd_rules;
            if let Some(new_eq) = new_config.eq {
                *current_eq_config = new_eq;
            }

            shared
                .state
                .output_mode
                .store(output_mode.as_id(), Ordering::Relaxed);
            shared
                .state
                .dsd_modulator
                .store(dsd_modulator.as_id(), Ordering::Relaxed);
            shared
                .state
                .dsd_isi_penalty
                .store(dsd_isi_penalty.to_bits(), Ordering::Relaxed);
            publish_config_status(
                &shared.state,
                *filter_type,
                *configured_target_rate,
                *upsampling_enabled,
                *exclusive_mode,
                *dsp_buffer_ms,
                *output_mode,
            );

            if !should_reopen {
                let source_rate = shared.state.source_rate.load(Ordering::Relaxed);
                let active_output_mode =
                    OutputMode::from_id(shared.state.active_output_mode.load(Ordering::Relaxed));
                eq_processor.update(
                    eq_processing_rate(active_output_mode, source_rate, *target_rate),
                    current_eq_config,
                );
                return;
            }

            println!("AudioWorker: Playback config changed. Re-opening output device.");
            let reopening_active_playback =
                should_use_live_config_transition_preroll(previous_state);
            *use_transition_preroll = reopening_active_playback;
            if reopening_active_playback {
                shared
                    .state
                    .state
                    .store(PLAYBACK_STARTING, Ordering::Relaxed);
            }
            #[cfg(all(target_os = "windows", feature = "asio"))]
            native_dsd_failed_attempts.clear();
            shared
                .state
                .record_reopen_reason(REOPEN_REASON_UPDATE_CONFIG);
            reset_output_pipeline_for_reopen(
                active_stream,
                dsd_state,
                dsd_fallback_key,
                &shared.state,
                true,
            );
            *active_stream_opened_at = None;
            *next_stream_retry = Instant::now();

            match restart_current_file_session(
                current_file_path.as_deref(),
                *filter_type,
                *configured_target_rate,
                *upsampling_enabled,
                active_device_name.as_deref(),
                current_fallback_tags.clone(),
                &shared.state,
                &shared.track_tags,
                &shared.track_cover,
                &shared.cover_version,
                eq_processor,
                current_eq_config,
                active_reopen_state(previous_state).is_some(),
                Some(resume_seconds),
            ) {
                Ok(Some((new_session, new_target_rate))) => {
                    let source_rate = new_session.dsp_path.source_rate();
                    let position_rate = effective_position_rate_for_session(
                        *output_mode,
                        *filter_type,
                        source_rate,
                        new_target_rate,
                        dsd_rules,
                    );
                    *target_rate = position_rate;
                    shared
                        .state
                        .target_rate
                        .store(*target_rate, Ordering::Relaxed);
                    eq_processor.update(
                        eq_processing_rate(*output_mode, source_rate, *target_rate),
                        current_eq_config,
                    );
                    eq_processor.reset();
                    restore_position_seconds(&shared.state, resume_seconds, position_rate);
                    *session = Some(new_session);
                    restore_transition_state(&shared.state, previous_state);
                }
                Ok(None) => {
                    if let Some(sess) = session {
                        *target_rate = reconfigure_current_session(
                            sess,
                            *filter_type,
                            *configured_target_rate,
                            *upsampling_enabled,
                            active_device_name.as_deref(),
                            &shared.state,
                            eq_processor,
                            current_eq_config,
                        );
                        let source_rate = sess.dsp_path.source_rate();
                        let position_rate = effective_position_rate_for_session(
                            *output_mode,
                            *filter_type,
                            source_rate,
                            *target_rate,
                            dsd_rules,
                        );
                        *target_rate = position_rate;
                        shared
                            .state
                            .target_rate
                            .store(*target_rate, Ordering::Relaxed);
                        eq_processor.update(
                            eq_processing_rate(*output_mode, source_rate, *target_rate),
                            current_eq_config,
                        );
                        eq_processor.reset();
                        restore_position_seconds(&shared.state, resume_seconds, position_rate);
                        restore_transition_state(&shared.state, previous_state);
                    } else if *configured_target_rate != 0 {
                        *target_rate = *configured_target_rate;
                        shared
                            .state
                            .target_rate
                            .store(*target_rate, Ordering::Relaxed);
                        restore_transition_state(&shared.state, previous_state);
                    }
                }
                Err(err) => {
                    let message =
                        format!("Failed to rebuild playback session after DSP change: {err}");
                    eprintln!("AudioWorker: {message}");
                    publish_output_notice(&shared.state, &shared.output_notice, message);
                    shared
                        .state
                        .state
                        .store(PLAYBACK_STOPPED, Ordering::Relaxed);
                }
            }
        }
        PlayerCommand::SelectDevice { name } => {
            if *shared.device_name.lock().unwrap() == name {
                return;
            }
            println!("AudioWorker: Device selection changed to {:?}", name);
            let was_playing = shared.state.state.load(Ordering::Relaxed) == PLAYBACK_PLAYING;
            *shared.device_name.lock().unwrap() = name.clone();
            *active_device_name = name;
            #[cfg(all(target_os = "windows", feature = "asio"))]
            native_dsd_failed_attempts.clear();
            shared
                .state
                .record_reopen_reason(REOPEN_REASON_SELECT_DEVICE);
            reset_output_pipeline_for_reopen(
                active_stream,
                dsd_state,
                dsd_fallback_key,
                &shared.state,
                true,
            );
            *active_stream_opened_at = None;
            *next_stream_retry = Instant::now();

            if *configured_target_rate == 0
                && *upsampling_enabled
                && !output_mode.is_dsd()
                && was_playing
            {
                let resume_seconds = current_playback_seconds(&shared.state, *target_rate);
                match restart_current_file_session(
                    current_file_path.as_deref(),
                    *filter_type,
                    *configured_target_rate,
                    *upsampling_enabled,
                    active_device_name.as_deref(),
                    current_fallback_tags.clone(),
                    &shared.state,
                    &shared.track_tags,
                    &shared.track_cover,
                    &shared.cover_version,
                    eq_processor,
                    current_eq_config,
                    true,
                    Some(resume_seconds),
                ) {
                    Ok(Some((new_session, new_target_rate))) => {
                        *session = Some(new_session);
                        *target_rate = new_target_rate;
                    }
                    Ok(None) => {
                        if let Some(sess) = session {
                            *target_rate = reconfigure_current_session(
                                sess,
                                *filter_type,
                                *configured_target_rate,
                                *upsampling_enabled,
                                active_device_name.as_deref(),
                                &shared.state,
                                eq_processor,
                                current_eq_config,
                            );
                        }
                    }
                    Err(_) => {}
                }
            }
        }
        PlayerCommand::ReopenOutput => {
            if crate::audio::debug::audio_debug_enabled() {
                eprintln!("AudioWorker DEBUG: external device ready; reopening output");
            }
            shared
                .state
                .record_reopen_reason(REOPEN_REASON_EXTERNAL_DEVICE_READY);
            reset_output_pipeline_for_reopen(
                active_stream,
                dsd_state,
                dsd_fallback_key,
                &shared.state,
                true,
            );
            *active_stream_opened_at = None;
            *next_stream_retry = Instant::now();
        }
        PlayerCommand::UpdateEq(new_eq) => {
            *current_eq_config = new_eq;
            let source_rate = shared.state.source_rate.load(Ordering::Relaxed);
            let active_output_mode =
                OutputMode::from_id(shared.state.active_output_mode.load(Ordering::Relaxed));
            eq_processor.update(
                eq_processing_rate(active_output_mode, source_rate, *target_rate),
                current_eq_config,
            );
        }
        PlayerCommand::SetOutputMode { mode } => {
            println!(
                "AudioWorker: SetOutputMode received: current={:?} requested={:?}",
                output_mode, mode
            );
            if !output_mode_change_requires_reopen(*output_mode, mode) {
                return;
            }
            let leaving_dsd = output_mode.is_dsd() && !mode.is_dsd();
            *output_mode = mode;
            shared
                .state
                .output_mode
                .store(mode.as_id(), Ordering::Relaxed);
            #[cfg(all(target_os = "windows", feature = "asio"))]
            native_dsd_failed_attempts.clear();
            shared
                .state
                .record_reopen_reason(REOPEN_REASON_SET_OUTPUT_MODE);
            reset_output_pipeline_for_reopen(
                active_stream,
                dsd_state,
                dsd_fallback_key,
                &shared.state,
                true,
            );
            *next_stream_retry = Instant::now();

            if leaving_dsd && let Some(sess) = session {
                *target_rate = sess.dsp_path.target_rate();
                shared
                    .state
                    .target_rate
                    .store(*target_rate, Ordering::Relaxed);
                shared
                    .state
                    .active_filter_type
                    .store(filter_type.as_id(), Ordering::Relaxed);
                eq_processor.update(*target_rate, current_eq_config);
                eq_processor.reset();
            }
        }
        PlayerCommand::SetDsdRules { rules } => {
            if *dsd_rules == rules {
                return;
            }
            if crate::audio::debug::audio_debug_enabled() {
                eprintln!(
                    "AudioWorker DEBUG: SetDsdRules received: {} rule(s); output_mode={}",
                    rules.len(),
                    output_mode.as_name(),
                );
            }
            *dsd_rules = rules;
            if output_mode.is_dsd() {
                #[cfg(all(target_os = "windows", feature = "asio"))]
                native_dsd_failed_attempts.clear();
                if crate::audio::debug::audio_debug_enabled() {
                    eprintln!(
                        "AudioWorker DEBUG: DSD rules changed while DSD active; reopening output"
                    );
                }
                shared.state.record_reopen_reason(REOPEN_REASON_DSD_RULES);
                reset_output_pipeline_for_reopen(
                    active_stream,
                    dsd_state,
                    dsd_fallback_key,
                    &shared.state,
                    true,
                );
                *next_stream_retry = Instant::now();
            }
        }
        PlayerCommand::SetDsdModulator { modulator } => {
            if *dsd_modulator == modulator {
                return;
            }
            if crate::audio::debug::audio_debug_enabled() {
                eprintln!(
                    "AudioWorker DEBUG: SetDsdModulator received: current={} requested={} lookahead={}",
                    dsd_modulator.as_name(),
                    modulator.as_name(),
                    modulator.lookahead_depth(),
                );
            }
            *dsd_modulator = modulator;
            shared
                .state
                .dsd_modulator
                .store(modulator.as_id(), Ordering::Relaxed);
            if output_mode.is_dsd() {
                #[cfg(all(target_os = "windows", feature = "asio"))]
                native_dsd_failed_attempts.clear();
                if crate::audio::debug::audio_debug_enabled() {
                    eprintln!(
                        "AudioWorker DEBUG: DSD modulator changed while DSD active; reopening output"
                    );
                }
                shared
                    .state
                    .record_reopen_reason(REOPEN_REASON_DSD_MODULATOR);
                reset_output_pipeline_for_reopen(
                    active_stream,
                    dsd_state,
                    dsd_fallback_key,
                    &shared.state,
                    true,
                );
                *next_stream_retry = Instant::now();
            }
        }
        PlayerCommand::SetDsdIsiPenalty { penalty } => {
            if (*dsd_isi_penalty - penalty).abs() < f32::EPSILON {
                return;
            }
            if crate::audio::debug::audio_debug_enabled() {
                eprintln!(
                    "AudioWorker DEBUG: SetDsdIsiPenalty received: current={:.5} requested={:.5}",
                    *dsd_isi_penalty, penalty,
                );
            }
            *dsd_isi_penalty = penalty;
            shared
                .state
                .dsd_isi_penalty
                .store(penalty.to_bits(), Ordering::Relaxed);
            if output_mode.is_dsd() {
                #[cfg(all(target_os = "windows", feature = "asio"))]
                native_dsd_failed_attempts.clear();
                if crate::audio::debug::audio_debug_enabled() {
                    eprintln!(
                        "AudioWorker DEBUG: DSD ISI penalty changed while DSD active; reopening output"
                    );
                }
                shared.state.record_reopen_reason(REOPEN_REASON_DSD_ISI);
                reset_output_pipeline_for_reopen(
                    active_stream,
                    dsd_state,
                    dsd_fallback_key,
                    &shared.state,
                    true,
                );
                *next_stream_retry = Instant::now();
            }
        }
        PlayerCommand::SetAirPlayVolume { volume } => {
            let Some(volume) = airplay::normalize_device_volume(volume) else {
                return;
            };
            shared
                .airplay_device_volume
                .store(volume.to_bits(), Ordering::Relaxed);
            if let Some(ActiveOutput::AirPlayRaop(stream)) = active_stream.as_ref() {
                stream.set_volume(volume);
            }
            if let Some(ActiveOutput::AirPlay2(stream)) = active_stream.as_ref() {
                stream.set_volume(volume);
            }
        }
    }
}

fn output_mode_change_requires_reopen(
    current: super::signal_path::OutputMode,
    requested: super::signal_path::OutputMode,
) -> bool {
    current != requested
}

fn active_reopen_state(state: u32) -> Option<u32> {
    match state {
        PLAYBACK_PLAYING | PLAYBACK_STARTING => Some(PLAYBACK_STARTING),
        _ => None,
    }
}

fn should_use_live_config_transition_preroll(previous_state: u32) -> bool {
    active_reopen_state(previous_state).is_some()
}

fn restore_transition_state(state: &super::state::AtomicPlayerState, previous_state: u32) {
    if let Some(next_state) = active_reopen_state(previous_state) {
        state.state.store(next_state, Ordering::Relaxed);
    } else if previous_state == PLAYBACK_PAUSED {
        state.state.store(PLAYBACK_PAUSED, Ordering::Relaxed);
    }
}

fn restore_position_seconds(
    state: &super::state::AtomicPlayerState,
    seconds: f64,
    position_rate: u32,
) {
    if seconds.is_finite() && seconds >= 0.0 {
        let samples = (seconds * position_rate.max(1) as f64) as u64;
        state.position_samples.store(samples, Ordering::Relaxed);
    }
}

fn effective_position_rate_for_session(
    output_mode: OutputMode,
    filter_type: crate::audio::resampler::FilterType,
    source_rate: u32,
    pcm_target_rate: u32,
    dsd_rules: &[crate::settings::DsdSourceRule],
) -> u32 {
    if output_mode.is_dsd() {
        let policy = dsd_policy_for_source(output_mode, filter_type, source_rate, dsd_rules);
        policy
            .mode
            .dsd_wire_rate(source_rate)
            .unwrap_or(pcm_target_rate)
    } else {
        pcm_target_rate
    }
}

fn current_playback_seconds(state: &super::state::AtomicPlayerState, target_rate: u32) -> f64 {
    let rate = target_rate.max(1) as f64;
    state.position_samples.load(Ordering::Relaxed) as f64 / rate
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::{
        active_reopen_state, current_playback_seconds, effective_position_rate_for_session,
        output_mode_change_requires_reopen, restore_position_seconds, restore_transition_state,
        should_use_live_config_transition_preroll,
    };
    use crate::audio::dsp::resampler::FilterType;
    use crate::audio::engine::signal_path::OutputMode;
    use crate::audio::engine::state::{
        AtomicPlayerState, PLAYBACK_PAUSED, PLAYBACK_PLAYING, PLAYBACK_STARTING, PLAYBACK_STOPPED,
    };
    use crate::settings::DsdSourceRule;

    #[test]
    fn same_output_mode_command_is_noop_even_for_dsd() {
        assert!(!output_mode_change_requires_reopen(
            OutputMode::Dsd256,
            OutputMode::Dsd256
        ));
        assert!(!output_mode_change_requires_reopen(
            OutputMode::Pcm,
            OutputMode::Pcm
        ));
        assert!(output_mode_change_requires_reopen(
            OutputMode::Dsd256,
            OutputMode::Pcm
        ));
        assert!(output_mode_change_requires_reopen(
            OutputMode::Pcm,
            OutputMode::Dsd256
        ));
    }

    #[test]
    fn current_playback_seconds_uses_active_target_rate() {
        let state = AtomicPlayerState::new();
        state.position_samples.store(1_411_200, Ordering::Relaxed);

        assert_eq!(current_playback_seconds(&state, 11_289_600), 0.125);
        assert_eq!(current_playback_seconds(&state, 0), 1_411_200.0);
    }

    #[test]
    fn active_reopen_moves_running_playback_to_starting() {
        assert_eq!(
            active_reopen_state(PLAYBACK_PLAYING),
            Some(PLAYBACK_STARTING)
        );
        assert_eq!(
            active_reopen_state(PLAYBACK_STARTING),
            Some(PLAYBACK_STARTING)
        );
        assert_eq!(active_reopen_state(PLAYBACK_PAUSED), None);
        assert_eq!(active_reopen_state(PLAYBACK_STOPPED), None);
    }

    #[test]
    fn active_live_config_changes_use_transition_preroll() {
        assert!(should_use_live_config_transition_preroll(PLAYBACK_PLAYING));
        assert!(should_use_live_config_transition_preroll(PLAYBACK_STARTING));
        assert!(!should_use_live_config_transition_preroll(PLAYBACK_PAUSED));
        assert!(!should_use_live_config_transition_preroll(PLAYBACK_STOPPED));
    }

    #[test]
    fn restore_transition_state_keeps_paused_paused() {
        let state = AtomicPlayerState::new();

        restore_transition_state(&state, PLAYBACK_PAUSED);
        assert_eq!(state.state.load(Ordering::Relaxed), PLAYBACK_PAUSED);

        restore_transition_state(&state, PLAYBACK_PLAYING);
        assert_eq!(state.state.load(Ordering::Relaxed), PLAYBACK_STARTING);
    }

    #[test]
    fn restore_position_seconds_rebases_to_new_rate() {
        let state = AtomicPlayerState::new();

        restore_position_seconds(&state, 12.5, 96_000);

        assert_eq!(state.position_samples.load(Ordering::Relaxed), 1_200_000);
        assert_eq!(current_playback_seconds(&state, 96_000), 12.5);
    }

    #[test]
    fn dsd_position_rate_uses_rule_selected_wire_rate() {
        let rate = effective_position_rate_for_session(
            OutputMode::Dsd256,
            FilterType::Minimum16k,
            44_100,
            176_400,
            &[DsdSourceRule {
                source_rate: 44_100,
                filter_type: "Split128k".to_string(),
                output_mode: "Dsd128".to_string(),
            }],
        );

        assert_eq!(rate, 5_644_800);
    }
}
