use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use crate::audio::sinks::airplay::{self, sender::AirPlayMetadata};

use super::output_open::open_audio_stream;
use super::session::{reconfigure_current_session, restart_current_file_session};
use super::state::{PLAYBACK_STARTING, PLAYBACK_STOPPED};
use super::worker_state::WorkerRuntime;
use super::worker_status::{
    clear_dsd_buffer_health, publish_active_pcm_status, publish_output_notice,
    reset_pcm_buffer_watermark,
};

pub(super) enum PcmOutputOpenResult {
    Ready,
    RetryLater,
}

pub(super) fn ensure_pcm_output_stream(runtime: &mut WorkerRuntime) -> PcmOutputOpenResult {
    let shared = &runtime.shared;
    let playback = &mut runtime.playback;
    let output = &mut runtime.output;
    let buffers = &mut runtime.buffers;
    let config = &mut runtime.config;
    let active_stream = &mut output.active_stream;
    let active_stream_opened_at = &mut output.active_stream_opened_at;
    let active_device_name = &output.active_device_name;
    let target_rate = &mut config.target_rate;
    let exclusive_mode = config.exclusive_mode;
    let dsp_buffer_ms = config.dsp_buffer_ms;
    let state = &shared.state;
    let output_notice = &shared.output_notice;
    let next_stream_retry = &mut output.next_stream_retry;
    let airplay_device_volume = &shared.airplay_device_volume;
    let file_name = &shared.file_name;
    let track_tags = &shared.track_tags;
    let track_cover = &shared.track_cover;
    let cover_version = &shared.cover_version;
    let filter_type = config.filter_type;
    let current_file_path = playback.current_file_path.as_deref();
    let current_fallback_tags = playback.current_fallback_tags.clone();
    let eq_processor = &mut config.eq_processor;
    let current_eq_config = &config.current_eq_config;
    let session = &mut playback.session;

    // Dropping any previous stream releases its OS resources
    // (including macOS Hog Mode via CpalOutput::drop).
    *active_stream = None;
    *active_stream_opened_at = None;
    // Releasing any stale DSD state keeps only one output path live at a time.
    buffers.clear_dsd_state();
    if config.output_mode.is_dsd()
        && let Some(sess) = session.as_ref()
    {
        *target_rate = sess.dsp_path.target_rate();
        state.target_rate.store(*target_rate, Ordering::Relaxed);
        eq_processor.update(*target_rate, current_eq_config);
        eq_processor.reset();
    }

    if let Some(reason) = airplay::unsupported_target_reason(active_device_name.as_deref()) {
        eprintln!("AudioWorker: Error opening audio stream: {reason}");
        publish_output_notice(state, output_notice, reason.to_string());
        state.state.store(PLAYBACK_STOPPED, Ordering::Relaxed);
        return PcmOutputOpenResult::RetryLater;
    }

    if buffers.ensure_pcm_ring_capacity(*target_rate, dsp_buffer_ms) {
        println!(
            "AudioWorker: Ring buffer resized to {} samples ({:.0}ms at {}Hz stereo)",
            buffers.ring_capacity,
            buffers.ring_capacity as f64 / 2.0 / *target_rate as f64 * 1000.0,
            *target_rate,
        );
    }

    if state.state.load(Ordering::Relaxed) == PLAYBACK_STARTING
        && active_stream.is_none()
        && state.flush_buffer.swap(false, Ordering::Relaxed)
    {
        buffers.reset_pcm_ring(*target_rate, dsp_buffer_ms);
    }

    match open_audio_stream(
        active_device_name,
        *target_rate,
        exclusive_mode,
        buffers.take_pcm_consumer_after_ring_setup(),
        Arc::clone(state),
        AirPlayMetadata::from_player(
            file_name.lock().unwrap().clone(),
            track_tags.lock().unwrap().clone(),
            track_cover.lock().unwrap().clone(),
        ),
        Arc::clone(airplay_device_volume),
        airplay::normalize_device_volume(f32::from_bits(
            airplay_device_volume.load(Ordering::Relaxed),
        )),
    ) {
        Ok((stream, actual_output_rate)) => {
            *active_stream = Some(stream);
            *active_stream_opened_at = Some(Instant::now());
            publish_active_pcm_status(state, filter_type);
            clear_dsd_buffer_health(state);
            reset_pcm_buffer_watermark(state);
            *next_stream_retry = Instant::now();

            if actual_output_rate != *target_rate {
                println!(
                    "AudioWorker: Output device fell back to {}Hz; rebuilding DSP session to match.",
                    actual_output_rate
                );
                let resume_seconds = state.position_samples.load(Ordering::Relaxed) as f64
                    / (*target_rate).max(1) as f64;
                *target_rate = actual_output_rate;
                state.target_rate.store(*target_rate, Ordering::Relaxed);
                eq_processor.update(*target_rate, current_eq_config);
                eq_processor.reset();

                match restart_current_file_session(
                    current_file_path,
                    filter_type,
                    actual_output_rate,
                    true,
                    active_device_name.as_deref(),
                    current_fallback_tags,
                    state,
                    track_tags,
                    track_cover,
                    cover_version,
                    eq_processor,
                    current_eq_config,
                    false,
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
                                filter_type,
                                actual_output_rate,
                                true,
                                active_device_name.as_deref(),
                                state,
                                eq_processor,
                                current_eq_config,
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "AudioWorker: Failed to rebuild session for fallback output rate: {:?}",
                            e
                        );
                        state.state.store(PLAYBACK_STOPPED, Ordering::Relaxed);
                    }
                }
            }

            PcmOutputOpenResult::Ready
        }
        Err(e) => {
            eprintln!("AudioWorker: Error opening audio stream: {:?}", e);
            let message = e.to_string();
            if airplay::is_permanent_open_error(active_device_name.as_deref(), &message) {
                publish_output_notice(state, output_notice, message);
                state.state.store(PLAYBACK_STOPPED, Ordering::Relaxed);
                return PcmOutputOpenResult::RetryLater;
            }
            *next_stream_retry = Instant::now() + Duration::from_secs(2);
            thread::sleep(Duration::from_millis(50));
            PcmOutputOpenResult::RetryLater
        }
    }
}
