use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
#[cfg(target_os = "windows")]
use std::thread;
#[cfg(target_os = "windows")]
use std::time::Duration;
use std::time::Instant;

use crate::audio::dsp::resampler::FilterType;
#[cfg(all(target_os = "windows", feature = "asio"))]
use crate::audio::output::asio_output;
#[cfg(target_os = "windows")]
use crate::audio::output::wasapi_exclusive;

#[cfg(all(target_os = "windows", feature = "asio"))]
use super::buffers::DsdDebugState;
use super::buffers::DsdWorkerState;
#[cfg(all(target_os = "windows", feature = "asio"))]
use super::buffers::NativeDsdWorkerSink;
#[cfg(all(target_os = "windows", feature = "asio"))]
use super::buffers::dsd_boundary_fade_in_frames;
#[cfg(all(target_os = "windows", feature = "asio"))]
use super::buffers::new_dop_ring;
use super::buffers::output_start_preroll_ready;
use super::dsd_path::{DsdFallbackKey, build_renderer, new_dop_worker_state};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use super::dsd_path::{dop_wire_rate_for_mode, should_force_44k_family_dsd256};
#[cfg(target_os = "macos")]
use super::output_open::open_coreaudio_dop_stream;
use super::output_stream::ActiveOutput;
use super::signal_path::{OutputTransport, dsd_policy_for_source};
use super::state::AtomicPlayerState;
use super::worker_state::WorkerRuntime;
use super::worker_status::{
    OutputSignalStatus, install_output_signal_status, publish_output_notice,
    publish_pcm_fallback_status,
};

pub(super) enum DsdOutputOpenResult {
    Ready,
    // Non-Windows transports currently retry through the ready/fallback path.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    RetryLater,
}

pub(super) fn ensure_dsd_output_stream(runtime: &mut WorkerRuntime) -> DsdOutputOpenResult {
    let shared = &runtime.shared;
    let playback = &mut runtime.playback;
    let output = &mut runtime.output;
    let buffers = &mut runtime.buffers;
    let config = &mut runtime.config;
    let active_stream = &mut output.active_stream;
    let dsd_state = &mut buffers.dsd_state;
    let dsd_fallback_key = &mut output.dsd_fallback_key;
    let active_device_name = &output.active_device_name;
    let output_mode = config.output_mode;
    let dsp_buffer_ms = config.dsp_buffer_ms;
    let dsd_modulator = config.dsd_modulator;
    let dsd_isi_penalty = config.dsd_isi_penalty;
    let filter_type = config.filter_type;
    let dsd_rules = &config.dsd_rules;
    let session = playback.session.as_ref();
    let target_rate = &mut config.target_rate;
    let state = &shared.state;
    let output_notice = &shared.output_notice;
    let next_stream_retry = &mut output.next_stream_retry;
    let eq_processor = &mut config.eq_processor;
    let current_eq_config = &config.current_eq_config;
    #[cfg(target_os = "macos")]
    let exclusive_mode = config.exclusive_mode;
    #[cfg(all(target_os = "windows", feature = "asio"))]
    let native_dsd_failed_attempts = &mut output.native_dsd_failed_attempts;

    let source_rate = session.map(|s| s.dsp_path.source_rate()).unwrap_or(44_100);
    let dsd_policy = dsd_policy_for_source(output_mode, filter_type, source_rate, dsd_rules);
    let fallback_key =
        DsdFallbackKey::new(active_device_name.clone(), dsd_policy.mode, source_rate);
    if dsd_fallback_key.as_ref() == Some(&fallback_key) {
        return DsdOutputOpenResult::Ready;
    }

    let wire_rate = dsd_policy.mode.dsd_wire_rate(source_rate);
    let is_asio_target = active_device_name
        .as_deref()
        .map(|name| name.starts_with("ASIO: "))
        .unwrap_or(false);
    if crate::audio::debug::audio_debug_enabled() {
        eprintln!(
            "AudioWorker DEBUG: DSD output attempt: device={:?} requested_mode={} policy_mode={} source={}Hz wire_rate={:?} filter={} modulator={} lookahead={} isi_penalty={:.5} asio_target={}",
            active_device_name,
            output_mode.as_name(),
            dsd_policy.mode.as_name(),
            source_rate,
            wire_rate,
            dsd_policy.filter_type.as_name(),
            dsd_modulator.as_name(),
            dsd_modulator.lookahead_depth(),
            dsd_isi_penalty,
            is_asio_target,
        );
    }
    let mut fallback_reason = wire_rate.is_none().then(|| {
        format!("DSD output is unavailable for a {source_rate} Hz source family; using PCM.")
    });

    #[cfg(target_os = "macos")]
    if fallback_reason.is_none() && !is_asio_target {
        let force_44k_family = should_force_44k_family_dsd256(dsd_policy.mode, source_rate);
        let expected_wire_rate =
            dop_wire_rate_for_mode(dsd_policy.mode, source_rate, force_44k_family)
                .expect("DSD wire rate checked");
        let primed_state_matches = primed_dop_state_matches(
            dsd_state.as_ref(),
            source_rate,
            dsd_policy.mode,
            expected_wire_rate,
        );
        if primed_state_matches {
            if !dsd_preopen_preroll_ready(
                dsd_state.as_mut(),
                &buffers.prod,
                *target_rate,
                dsp_buffer_ms,
                state,
                playback.use_transition_preroll,
            ) {
                return DsdOutputOpenResult::Ready;
            }
            let ds = dsd_state.as_mut().expect("primed DoP state present");
            let ring_capacity_samples = ds.prod.len() + ds.prod.free_len();
            let dop_cons = ds.cons_opt.take().expect("DoP consumer present");
            match open_coreaudio_dop_stream(
                active_device_name,
                ds.dop_frame_rate,
                ring_capacity_samples,
                exclusive_mode,
                dop_cons,
                Arc::clone(state),
            ) {
                Ok(stream) => {
                    println!(
                        "AudioWorker: CoreAudio DoP stream opened at {} Hz ({}) after startup preroll",
                        ds.dop_frame_rate,
                        dsd_policy.mode.as_name().to_uppercase()
                    );
                    state.flush_buffer.store(false, Ordering::Relaxed);
                    *active_stream = Some(ActiveOutput::CoreAudioDop(stream));
                    output.active_stream_opened_at = Some(Instant::now());
                    *target_rate = install_output_signal_status(
                        state,
                        eq_processor,
                        current_eq_config,
                        OutputSignalStatus {
                            target_rate: expected_wire_rate,
                            eq_rate: source_rate,
                            target_bits: 24,
                            active_mode: dsd_policy.mode,
                            active_filter: dsd_policy.filter_type,
                            transport: OutputTransport::DopCoreAudio,
                            exclusive: None,
                        },
                    );
                    *next_stream_retry = Instant::now();
                    return DsdOutputOpenResult::Ready;
                }
                Err((e, returned_cons)) => {
                    eprintln!("AudioWorker: CoreAudio DoP open failed: {e:?}");
                    ds.cons_opt = Some(returned_cons);
                    fallback_reason =
                        Some(format!("CoreAudio DoP open failed ({e:?}); using PCM."));
                }
            }
        }
    }

    if fallback_reason.is_none() && is_asio_target {
        #[cfg(all(target_os = "windows", feature = "asio"))]
        {
            let driver_name = active_device_name
                .as_deref()
                .and_then(|name| name.strip_prefix("ASIO: "))
                .expect("ASIO device prefix checked");
            let mut native_failures = Vec::new();
            let mut pcm_fallback_after = None;

            for attempt in dsd_policy.mode.native_dsd_attempts(source_rate) {
                let attempt_mode = attempt.mode;
                let attempt_wire_rate = attempt.wire_rate;
                if let Some(description) = native_dsd_failed_attempts.active_failure_description(
                    active_device_name.clone(),
                    attempt_mode,
                    attempt_wire_rate,
                    Instant::now(),
                ) {
                    native_failures.push(description);
                    continue;
                }
                let mut renderer = match build_renderer(
                    dsd_policy.filter_type,
                    source_rate,
                    attempt_mode,
                    attempt.force_44k_family,
                    dsd_modulator,
                    dsd_isi_penalty,
                ) {
                    Ok(renderer) => renderer,
                    Err(e) => {
                        native_failures.push(format!(
                            "cannot build {} renderer ({e})",
                            attempt_mode.as_name().to_uppercase()
                        ));
                        continue;
                    }
                };
                match asio_output::open_native_dsd(
                    driver_name,
                    attempt_wire_rate,
                    dsp_buffer_ms,
                    Arc::clone(state),
                ) {
                    Ok(opened) => {
                        println!(
                            "AudioWorker: Native ASIO {} stream opened at {} Hz{}",
                            attempt_mode.as_name().to_uppercase(),
                            attempt_wire_rate,
                            if attempt.force_44k_family {
                                " (44.1-family compatibility)"
                            } else {
                                ""
                            }
                        );
                        renderer.set_native_order(opened.order);
                        let (dop_prod, dop_cons) = new_dop_ring(1, dsp_buffer_ms);
                        let fade_in_frames = dsd_boundary_fade_in_frames(attempt_wire_rate);
                        *dsd_state = Some(DsdWorkerState {
                            renderer,
                            prod: dop_prod,
                            cons_opt: Some(dop_cons),
                            output_buf: Vec::new(),
                            staged_pcm_l: Vec::with_capacity(
                                super::buffers::dsd_render_quantum_frames(
                                    source_rate,
                                    attempt_mode,
                                ),
                            ),
                            staged_pcm_r: Vec::with_capacity(
                                super::buffers::dsd_render_quantum_frames(
                                    source_rate,
                                    attempt_mode,
                                ),
                            ),
                            render_quantum_l: Vec::with_capacity(
                                super::buffers::dsd_render_quantum_frames(
                                    source_rate,
                                    attempt_mode,
                                ),
                            ),
                            render_quantum_r: Vec::with_capacity(
                                super::buffers::dsd_render_quantum_frames(
                                    source_rate,
                                    attempt_mode,
                                ),
                            ),
                            eq_scratch_l: Vec::with_capacity(
                                super::buffers::dsd_render_quantum_frames(
                                    source_rate,
                                    attempt_mode,
                                ),
                            ),
                            eq_scratch_r: Vec::with_capacity(
                                super::buffers::dsd_render_quantum_frames(
                                    source_rate,
                                    attempt_mode,
                                ),
                            ),
                            render_quantum_frames: super::buffers::dsd_render_quantum_frames(
                                source_rate,
                                attempt_mode,
                            ),
                            recent_render_loads: Vec::new(),
                            recent_render_load_cursor: 0,
                            dop_frame_rate: 0,
                            source_rate,
                            wire_rate: attempt_wire_rate,
                            mode: attempt_mode,
                            dsp_buffer_ms,
                            fade_in_total_frames: fade_in_frames,
                            fade_in_remaining_frames: fade_in_frames,
                            debug: DsdDebugState::new(),
                            native: Some(NativeDsdWorkerSink {
                                prod_l: opened.producer_l,
                                prod_r: opened.producer_r,
                                output_l: Vec::with_capacity(opened.callback_bytes * 8),
                                output_r: Vec::with_capacity(opened.callback_bytes * 8),
                            }),
                        });
                        *active_stream = Some(ActiveOutput::AsioNativeDsd(opened.stream));
                        *target_rate = install_output_signal_status(
                            state,
                            eq_processor,
                            current_eq_config,
                            OutputSignalStatus {
                                target_rate: attempt_wire_rate,
                                eq_rate: source_rate,
                                target_bits: 1,
                                active_mode: attempt_mode,
                                active_filter: dsd_policy.filter_type,
                                transport: OutputTransport::NativeDsdAsio,
                                exclusive: Some(true),
                            },
                        );
                        *next_stream_retry = Instant::now();
                        let requested_wire_rate = wire_rate.unwrap_or(attempt_wire_rate);
                        if attempt_mode != dsd_policy.mode
                            || attempt_wire_rate != requested_wire_rate
                        {
                            let requested_failure = if native_failures.is_empty() {
                                "requested native DSD rate failed".to_string()
                            } else {
                                native_failures.join("; ")
                            };
                            let compatibility_label = if attempt.force_44k_family {
                                " 44.1-family"
                            } else {
                                ""
                            };
                            publish_output_notice(
                                state,
                                output_notice,
                                format!(
                                    "Native ASIO {requested_failure}; using {}{} at {} Hz.",
                                    attempt_mode.as_name().to_uppercase(),
                                    compatibility_label,
                                    attempt_wire_rate
                                ),
                            );
                        }
                        break;
                    }
                    Err(e) => {
                        let timed_out = e.timed_out();
                        let now = Instant::now();
                        if timed_out {
                            pcm_fallback_after = Some(native_dsd_failed_attempts.record_timeout(
                                active_device_name.clone(),
                                attempt_mode,
                                attempt_wire_rate,
                                now,
                            ));
                        } else {
                            native_dsd_failed_attempts.record_permanent(
                                active_device_name.clone(),
                                attempt_mode,
                                attempt_wire_rate,
                            );
                        }
                        native_failures.push(format!(
                            "{} at {} Hz failed ({})",
                            attempt_mode.as_name().to_uppercase(),
                            attempt_wire_rate,
                            e,
                        ));
                        if timed_out {
                            break;
                        }
                    }
                }
            }

            if active_stream.is_none() {
                if let Some(retry_at) = pcm_fallback_after {
                    *next_stream_retry = retry_at;
                }
                fallback_reason = Some(format!(
                    "Native ASIO {}; using PCM.",
                    native_failures.join("; ")
                ));
            }
        }
        #[cfg(not(all(target_os = "windows", feature = "asio")))]
        {
            fallback_reason = Some("This build does not include ASIO; using PCM.".to_string());
        }
    } else if fallback_reason.is_none() {
        #[cfg(target_os = "windows")]
        {
            let force_44k_family = should_force_44k_family_dsd256(dsd_policy.mode, source_rate);
            let wire_rate = dop_wire_rate_for_mode(dsd_policy.mode, source_rate, force_44k_family)
                .expect("DSD wire rate checked");
            if crate::audio::debug::audio_debug_enabled() {
                eprintln!(
                    "AudioWorker DEBUG: WASAPI DoP plan: force_44k_family={} wire={}Hz dop_frame={}Hz",
                    force_44k_family,
                    wire_rate,
                    wire_rate / 16,
                );
            }
            match build_renderer(
                dsd_policy.filter_type,
                source_rate,
                dsd_policy.mode,
                force_44k_family,
                dsd_modulator,
                dsd_isi_penalty,
            ) {
                Ok(renderer) => {
                    *dsd_state = Some(new_dop_worker_state(
                        renderer,
                        source_rate,
                        wire_rate,
                        dsd_policy.mode,
                        dsp_buffer_ms,
                    ));
                    let ds = dsd_state.as_mut().expect("DoP state present");
                    let dop_cons = ds.cons_opt.take().expect("DoP consumer present");
                    match wasapi_exclusive::open_dop(
                        active_device_name.as_deref(),
                        ds.dop_frame_rate,
                        dop_cons,
                        Arc::clone(state),
                    ) {
                        Ok((stream, actual_rate)) => {
                            println!(
                                "AudioWorker: WASAPI DoP stream opened at {} Hz ({})",
                                actual_rate,
                                dsd_policy.mode.as_name().to_uppercase()
                            );
                            state.flush_buffer.store(false, Ordering::Relaxed);
                            *active_stream = Some(ActiveOutput::WasapiExclusiveDop(stream));
                            *target_rate = install_output_signal_status(
                                state,
                                eq_processor,
                                current_eq_config,
                                OutputSignalStatus {
                                    target_rate: wire_rate,
                                    eq_rate: source_rate,
                                    target_bits: 24,
                                    active_mode: dsd_policy.mode,
                                    active_filter: dsd_policy.filter_type,
                                    transport: OutputTransport::DopWasapi,
                                    exclusive: Some(true),
                                },
                            );
                            *next_stream_retry = Instant::now();
                        }
                        Err((e, returned_cons)) => {
                            eprintln!("AudioWorker: WASAPI DoP open failed: {e:?}");
                            let message = e.to_string();
                            ds.cons_opt = Some(returned_cons);
                            if wasapi_dop_failure_is_permanent(&message) {
                                fallback_reason =
                                    Some(format!("WASAPI DoP open failed ({message}); using PCM."));
                            } else {
                                *next_stream_retry = Instant::now() + Duration::from_secs(2);
                                thread::sleep(Duration::from_millis(50));
                                return DsdOutputOpenResult::RetryLater;
                            }
                        }
                    }
                }
                Err(e) => {
                    fallback_reason = Some(format!("Cannot build DSD renderer ({e}); using PCM."));
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            let force_44k_family = should_force_44k_family_dsd256(dsd_policy.mode, source_rate);
            let wire_rate = dop_wire_rate_for_mode(dsd_policy.mode, source_rate, force_44k_family)
                .expect("DSD wire rate checked");
            if crate::audio::debug::audio_debug_enabled() {
                eprintln!(
                    "AudioWorker DEBUG: CoreAudio DoP plan: exclusive={} force_44k_family={} wire={}Hz dop_frame={}Hz",
                    exclusive_mode,
                    force_44k_family,
                    wire_rate,
                    wire_rate / 16,
                );
            }
            match build_renderer(
                dsd_policy.filter_type,
                source_rate,
                dsd_policy.mode,
                force_44k_family,
                dsd_modulator,
                dsd_isi_penalty,
            ) {
                Ok(renderer) => {
                    *dsd_state = Some(new_dop_worker_state(
                        renderer,
                        source_rate,
                        wire_rate,
                        dsd_policy.mode,
                        dsp_buffer_ms,
                    ));
                    if !dsd_preopen_preroll_ready(
                        dsd_state.as_mut(),
                        &buffers.prod,
                        *target_rate,
                        dsp_buffer_ms,
                        state,
                        playback.use_transition_preroll,
                    ) {
                        return DsdOutputOpenResult::Ready;
                    }
                    let ds = dsd_state.as_mut().expect("DoP state present");
                    let ring_capacity_samples = ds.prod.len() + ds.prod.free_len();
                    let dop_cons = ds.cons_opt.take().expect("DoP consumer present");
                    match open_coreaudio_dop_stream(
                        active_device_name,
                        ds.dop_frame_rate,
                        ring_capacity_samples,
                        exclusive_mode,
                        dop_cons,
                        Arc::clone(state),
                    ) {
                        Ok(stream) => {
                            println!(
                                "AudioWorker: CoreAudio DoP stream opened at {} Hz ({})",
                                ds.dop_frame_rate,
                                dsd_policy.mode.as_name().to_uppercase()
                            );
                            state.flush_buffer.store(false, Ordering::Relaxed);
                            *active_stream = Some(ActiveOutput::CoreAudioDop(stream));
                            output.active_stream_opened_at = Some(Instant::now());
                            *target_rate = install_output_signal_status(
                                state,
                                eq_processor,
                                current_eq_config,
                                OutputSignalStatus {
                                    target_rate: wire_rate,
                                    eq_rate: source_rate,
                                    target_bits: 24,
                                    active_mode: dsd_policy.mode,
                                    active_filter: dsd_policy.filter_type,
                                    transport: OutputTransport::DopCoreAudio,
                                    exclusive: None,
                                },
                            );
                            *next_stream_retry = Instant::now();
                        }
                        Err((e, returned_cons)) => {
                            eprintln!("AudioWorker: CoreAudio DoP open failed: {e:?}");
                            ds.cons_opt = Some(returned_cons);
                            fallback_reason =
                                Some(format!("CoreAudio DoP open failed ({e:?}); using PCM."));
                        }
                    }
                }
                Err(e) => {
                    fallback_reason = Some(format!("Cannot build DSD renderer ({e}); using PCM."));
                }
            }
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            fallback_reason =
                Some("DSD DoP output is not supported on this platform; using PCM.".to_string());
        }
    }

    if let Some(reason) = fallback_reason {
        eprintln!("AudioWorker: {reason}");
        publish_dsd_pcm_fallback(
            dsd_state,
            dsd_fallback_key,
            target_rate,
            DsdPcmFallback {
                session_target_rate: session.map(|s| s.dsp_path.target_rate()),
                state,
                output_notice,
                filter_type,
                fallback_key,
                reason,
            },
        );
    }

    DsdOutputOpenResult::Ready
}

fn primed_dop_state_matches(
    dsd_state: Option<&DsdWorkerState>,
    source_rate: u32,
    mode: super::signal_path::OutputMode,
    wire_rate: u32,
) -> bool {
    dsd_state
        .map(|ds| {
            ds.source_rate == source_rate
                && ds.mode == mode
                && ds.wire_rate == wire_rate
                && ds.cons_opt.is_some()
        })
        .unwrap_or(false)
}

fn dsd_preopen_preroll_ready(
    dsd_state: Option<&mut DsdWorkerState>,
    audio_prod: &super::buffers::AudioProducer,
    target_rate: u32,
    dsp_buffer_ms: u32,
    state: &AtomicPlayerState,
    transition_preroll: bool,
) -> bool {
    if transition_preroll {
        return true;
    }
    let dsd_state = if let Some(ds) = dsd_state {
        if state.flush_buffer.swap(false, Ordering::Relaxed)
            && let Some(cons) = ds.cons_opt.as_mut()
        {
            cons.clear();
        }
        Some(&*ds)
    } else {
        None
    };
    if dsd_state.is_none() {
        state.flush_buffer.store(false, Ordering::Relaxed);
    }
    let p99 = f32::from_bits(state.dsd_recent_load_p99.load(Ordering::Relaxed));
    let protective_preroll =
        p99 >= 0.85 || state.startup_overbudget_blocks.load(Ordering::Relaxed) > 0;
    output_start_preroll_ready(
        dsd_state,
        audio_prod,
        target_rate,
        dsp_buffer_ms,
        false,
        false,
        protective_preroll,
    )
}

struct DsdPcmFallback<'a> {
    session_target_rate: Option<u32>,
    state: &'a AtomicPlayerState,
    output_notice: &'a Mutex<Option<String>>,
    filter_type: FilterType,
    fallback_key: DsdFallbackKey,
    reason: String,
}

fn publish_dsd_pcm_fallback(
    dsd_state: &mut Option<DsdWorkerState>,
    dsd_fallback_key: &mut Option<DsdFallbackKey>,
    target_rate: &mut u32,
    fallback: DsdPcmFallback<'_>,
) {
    *dsd_state = None;
    *dsd_fallback_key = Some(fallback.fallback_key);
    *target_rate = fallback.session_target_rate.unwrap_or(*target_rate);
    publish_pcm_fallback_status(fallback.state, *target_rate, fallback.filter_type);
    publish_output_notice(fallback.state, fallback.output_notice, fallback.reason);
}

#[cfg(any(test, target_os = "windows"))]
fn wasapi_dop_failure_is_permanent(message: &str) -> bool {
    [
        "DAC does not accept Int24 exclusive",
        "Probed format is not Int24",
        "Probed format rate",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::audio::dsp::resampler::FilterType;
    use crate::audio::engine::signal_path::{OutputMode, OutputTransport};

    #[test]
    fn dsd_pcm_fallback_clears_dsd_state_and_publishes_pcm_status() {
        let state = AtomicPlayerState::new();
        let output_notice = Mutex::new(None);
        let mut dsd_state = None;
        let mut fallback_key = None;
        let expected_key = DsdFallbackKey::new(Some("DAC".to_string()), OutputMode::Dsd256, 48_000);
        let mut target_rate = 192_000;

        publish_dsd_pcm_fallback(
            &mut dsd_state,
            &mut fallback_key,
            &mut target_rate,
            DsdPcmFallback {
                session_target_rate: Some(96_000),
                state: &state,
                output_notice: &output_notice,
                filter_type: FilterType::LinearPhase128k,
                fallback_key: expected_key.clone(),
                reason: "DSD unavailable; using PCM.".to_string(),
            },
        );

        assert!(dsd_state.is_none());
        assert_eq!(fallback_key.as_ref(), Some(&expected_key));
        assert_eq!(target_rate, 96_000);
        assert_eq!(state.target_rate.load(Ordering::Relaxed), 96_000);
        assert_eq!(
            state.active_output_mode.load(Ordering::Relaxed),
            OutputMode::Pcm.as_id()
        );
        assert_eq!(
            state.active_filter_type.load(Ordering::Relaxed),
            FilterType::LinearPhase128k.as_id()
        );
        assert_eq!(
            state.output_transport.load(Ordering::Relaxed),
            OutputTransport::None.as_id()
        );
        assert_eq!(
            output_notice.lock().unwrap().as_deref(),
            Some("DSD unavailable; using PCM.")
        );
        assert_eq!(state.output_notice_id.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn dsd_pcm_fallback_preserves_current_target_without_session_rate() {
        let state = AtomicPlayerState::new();
        let output_notice = Mutex::new(None);
        let mut dsd_state = None;
        let mut fallback_key = None;
        let mut target_rate = 176_400;

        publish_dsd_pcm_fallback(
            &mut dsd_state,
            &mut fallback_key,
            &mut target_rate,
            DsdPcmFallback {
                session_target_rate: None,
                state: &state,
                output_notice: &output_notice,
                filter_type: FilterType::LinearPhase128k,
                fallback_key: DsdFallbackKey::new(None, OutputMode::Dsd128, 44_100),
                reason: "Fallback".to_string(),
            },
        );

        assert_eq!(target_rate, 176_400);
        assert_eq!(state.target_rate.load(Ordering::Relaxed), 176_400);
    }

    #[test]
    fn primed_dop_state_accepts_forced_44k_family_dsd256_wire_rate() {
        let renderer = build_renderer(
            FilterType::Minimum16k,
            48_000,
            OutputMode::Dsd256,
            true,
            crate::audio::dsd::delta_sigma::DsdModulator::default(),
            0.0,
        )
        .expect("forced 48k-family DSD256 renderer");
        let state = new_dop_worker_state(renderer, 48_000, 11_289_600, OutputMode::Dsd256, 0);

        assert!(primed_dop_state_matches(
            Some(&state),
            48_000,
            OutputMode::Dsd256,
            11_289_600
        ));
        assert!(!primed_dop_state_matches(
            Some(&state),
            48_000,
            OutputMode::Dsd256,
            12_288_000
        ));
    }

    #[test]
    fn wasapi_dop_probe_rejections_are_permanent_fallbacks() {
        assert!(wasapi_dop_failure_is_permanent(
            "DAC does not accept Int24 exclusive format"
        ));
        assert!(wasapi_dop_failure_is_permanent(
            "Probed format is not Int24"
        ));
        assert!(wasapi_dop_failure_is_permanent(
            "Probed format rate 192000 did not match requested 176400"
        ));
        assert!(!wasapi_dop_failure_is_permanent(
            "IAudioClient::Initialize failed with AUDCLNT_E_DEVICE_IN_USE"
        ));
    }
}
