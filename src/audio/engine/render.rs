use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::audio::dsd::dsd_render::DsdLimiterTelemetry;
use crate::audio::dsp::eq::EqProcessor;

use super::buffers::DsdWorkerState;
use super::session::{DspPath, PlaybackSession};
use super::signal_path::OutputMode;
use super::state::AtomicPlayerState;

const SIGNAL_CLIP_THRESHOLD: f64 = 1.0;

pub(super) fn clamp_headroom_db(db: f32) -> f32 {
    if db.is_finite() {
        db.clamp(-24.0, 0.0)
    } else {
        0.0
    }
}

pub(super) fn output_headroom_gain(db: f32) -> f64 {
    10.0f64.powf(clamp_headroom_db(db) as f64 / 20.0)
}

pub(super) fn eq_processing_rate(
    output_mode: OutputMode,
    source_rate: u32,
    target_rate: u32,
) -> u32 {
    if output_mode.is_dsd() && source_rate > 0 {
        source_rate
    } else {
        target_rate
    }
}

fn publish_signal_level_metrics(samples: &[f64], gain: f64, state: &AtomicPlayerState) {
    let gain = if gain.is_finite() { gain.abs() } else { 0.0 };
    let mut peak = 0.0f64;
    let mut clipped_samples = 0_u64;

    for sample in samples {
        let value = *sample * gain;
        if !value.is_finite() {
            continue;
        }
        let abs = value.abs();
        peak = peak.max(abs);
        if abs >= SIGNAL_CLIP_THRESHOLD {
            clipped_samples += 1;
        }
    }

    let peak = peak.min(f32::MAX as f64) as f32;
    state.signal_peak.store(peak.to_bits(), Ordering::Relaxed);
    update_signal_peak_max(state, peak);
    state
        .signal_clipping
        .store(clipped_samples > 0, Ordering::Relaxed);
    if clipped_samples > 0 {
        state.signal_clip_events.fetch_add(1, Ordering::Relaxed);
        state
            .signal_clip_samples
            .fetch_add(clipped_samples, Ordering::Relaxed);
    }
}

fn update_signal_peak_max(state: &AtomicPlayerState, peak: f32) {
    let mut current_bits = state.signal_peak_max.load(Ordering::Relaxed);
    loop {
        let current = f32::from_bits(current_bits);
        if peak <= current {
            return;
        }
        match state.signal_peak_max.compare_exchange_weak(
            current_bits,
            peak.to_bits(),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(next_bits) => current_bits = next_bits,
        }
    }
}

fn publish_dsd_limiter_metrics(telemetry: DsdLimiterTelemetry, state: &AtomicPlayerState) {
    state.dsd_limiter_peak_ratio.store(
        telemetry.current_block_peak_ratio.to_bits(),
        Ordering::Relaxed,
    );
    update_dsd_limiter_peak_ratio_max(state, telemetry.current_block_peak_ratio);
    state.dsd_limiter_active.store(
        telemetry.current_block_limited_samples > 0 || telemetry.current_block_gain < 1.0,
        Ordering::Relaxed,
    );
    state
        .dsd_limiter_events
        .store(telemetry.limited_events, Ordering::Relaxed);
    state
        .dsd_limiter_samples
        .store(telemetry.limited_samples, Ordering::Relaxed);
}

fn update_dsd_limiter_peak_ratio_max(state: &AtomicPlayerState, peak: f32) {
    let mut current_bits = state.dsd_limiter_peak_ratio_max.load(Ordering::Relaxed);
    loop {
        let current = f32::from_bits(current_bits);
        if peak <= current {
            return;
        }
        match state.dsd_limiter_peak_ratio_max.compare_exchange_weak(
            current_bits,
            peak.to_bits(),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(next_bits) => current_bits = next_bits,
        }
    }
}

fn effective_dsd_render_elapsed_ns(
    caller_elapsed_ns: u64,
    collected_worker_elapsed_ns: u64,
) -> u64 {
    // DSD modulation is pipelined on two channel workers. File sources usually
    // reach the next render call before those workers finish, so their time is
    // naturally visible while collecting the previous block. A paced live
    // source can leave enough time between calls for the workers to finish,
    // hiding that same work from the caller timer. Pipeline throughput is
    // bounded by the slower stage, so use the larger duration rather than
    // adding two stages that execute concurrently.
    caller_elapsed_ns.max(collected_worker_elapsed_ns)
}

pub(super) fn render_dsd_block(
    ds: &mut DsdWorkerState,
    samples_l: &[f64],
    samples_r: &[f64],
    input_gain: f64,
    state: &AtomicPlayerState,
    eq_processor: &mut EqProcessor,
) {
    let start = Instant::now();
    let upsample_start = Instant::now();
    {
        let fade_in_total_frames = ds.fade_in_total_frames;
        let fade_in_remaining_frames = &mut ds.fade_in_remaining_frames;
        let (source_l, source_r) = if eq_processor.is_processing_active() {
            ds.eq_scratch_l.clear();
            ds.eq_scratch_r.clear();
            ds.eq_scratch_l.extend_from_slice(samples_l);
            ds.eq_scratch_r.extend_from_slice(samples_r);
            eq_processor.process_planar_stereo(&mut ds.eq_scratch_l, &mut ds.eq_scratch_r);
            (&ds.eq_scratch_l[..], &ds.eq_scratch_r[..])
        } else {
            (samples_l, samples_r)
        };
        let upsampled = ds.renderer.upsample(source_l, source_r);
        apply_dsd_boundary_fade_in(upsampled, fade_in_total_frames, fade_in_remaining_frames);
        publish_signal_level_metrics(upsampled, input_gain, state);
    }
    let upsample_elapsed = upsample_start.elapsed().as_nanos() as u64;

    let modulate_start = Instant::now();
    #[cfg(all(target_os = "windows", feature = "asio"))]
    if let Some(native) = ds.native.as_mut() {
        native.output_l.clear();
        native.output_r.clear();
        ds.renderer.modulate_and_pack_native(
            input_gain,
            &mut native.output_l,
            &mut native.output_r,
        );
    } else {
        ds.output_buf.clear();
        ds.renderer
            .modulate_and_pack(input_gain, &mut ds.output_buf);
    }
    #[cfg(not(all(target_os = "windows", feature = "asio")))]
    {
        ds.output_buf.clear();
        ds.renderer
            .modulate_and_pack(input_gain, &mut ds.output_buf);
    }
    let modulate_caller_elapsed = modulate_start.elapsed().as_nanos() as u64;
    let worker_modulation_elapsed = ds.renderer.last_collected_modulation_time().as_nanos() as u64;
    let modulate_elapsed = modulate_caller_elapsed.max(worker_modulation_elapsed);

    let caller_elapsed = start.elapsed().as_nanos() as u64;
    let elapsed = effective_dsd_render_elapsed_ns(caller_elapsed, worker_modulation_elapsed);
    state.record_render_block_ns(elapsed);
    state.resample_time_ns.store(elapsed, Ordering::Relaxed);
    state
        .dsd_upsample_time_ns
        .store(upsample_elapsed, Ordering::Relaxed);
    state
        .dsd_modulate_time_ns
        .store(modulate_elapsed, Ordering::Relaxed);
    state
        .dsd_output_pending_samples
        .store(ds.output_pending_len() as u64, Ordering::Relaxed);
    let block_duration_ns = if !samples_l.is_empty() && ds.source_rate > 0 {
        (samples_l.len() as f64 / ds.source_rate as f64 * 1e9) as u64
    } else {
        0
    };
    if block_duration_ns > 0 {
        state
            .block_duration_ns
            .store(block_duration_ns, Ordering::Relaxed);
        let render_load = elapsed as f32 / block_duration_ns as f32;
        state
            .dsd_last_load
            .store(render_load.to_bits(), Ordering::Relaxed);
        ds.record_render_load(render_load, state);
        let over_budget = elapsed > block_duration_ns;
        state.record_startup_render_block_ns(elapsed, over_budget);
        if over_budget {
            state.dsd_overbudget_blocks.fetch_add(1, Ordering::Relaxed);
        }
    } else {
        state.record_startup_render_block_ns(elapsed, false);
        state
            .dsd_last_load
            .store(0.0f32.to_bits(), Ordering::Relaxed);
        state
            .dsd_recent_load_p95
            .store(0.0f32.to_bits(), Ordering::Relaxed);
        state
            .dsd_recent_load_p99
            .store(0.0f32.to_bits(), Ordering::Relaxed);
    }
    state
        .dsd_stability_resets
        .store(ds.renderer.stability_resets(), Ordering::Relaxed);
    publish_dsd_limiter_metrics(ds.renderer.limiter_telemetry(), state);
    let output_len = ds.staged_output_len();
    let pending_before_write = ds.output_pending_len();
    let resets = ds.renderer.stability_resets();
    let clamps = ds.renderer.state_clamps();
    let modulator = ds.renderer.dsd_modulator().as_name();
    ds.debug.log_render_block(
        ds.mode,
        modulator,
        samples_l.len().min(samples_r.len()),
        output_len,
        pending_before_write,
        upsample_elapsed,
        modulate_elapsed,
        elapsed,
        block_duration_ns,
        resets,
        clamps,
    );
}

pub(super) fn render_dsd_upsampler_tail(
    ds: &mut DsdWorkerState,
    input_gain: f64,
    state: &AtomicPlayerState,
) -> bool {
    let start = Instant::now();
    let upsample_start = Instant::now();
    let tail_frames = {
        let fade_in_total_frames = ds.fade_in_total_frames;
        let fade_in_remaining_frames = &mut ds.fade_in_remaining_frames;
        let upsampled = ds.renderer.drain_resampler_eof();
        let frames = upsampled.len() / 2;
        if frames > 0 {
            apply_dsd_boundary_fade_in(upsampled, fade_in_total_frames, fade_in_remaining_frames);
            publish_signal_level_metrics(upsampled, input_gain, state);
        }
        frames
    };
    let upsample_elapsed = upsample_start.elapsed().as_nanos() as u64;

    if tail_frames == 0 {
        return false;
    }

    let modulate_start = Instant::now();
    #[cfg(all(target_os = "windows", feature = "asio"))]
    if let Some(native) = ds.native.as_mut() {
        native.output_l.clear();
        native.output_r.clear();
        ds.renderer.modulate_and_pack_native(
            input_gain,
            &mut native.output_l,
            &mut native.output_r,
        );
    } else {
        ds.output_buf.clear();
        ds.renderer
            .modulate_and_pack(input_gain, &mut ds.output_buf);
    }
    #[cfg(not(all(target_os = "windows", feature = "asio")))]
    {
        ds.output_buf.clear();
        ds.renderer
            .modulate_and_pack(input_gain, &mut ds.output_buf);
    }
    let modulate_elapsed = modulate_start.elapsed().as_nanos() as u64;

    let elapsed = start.elapsed().as_nanos() as u64;
    state.record_render_block_ns(elapsed);
    state.resample_time_ns.store(elapsed, Ordering::Relaxed);
    state
        .dsd_upsample_time_ns
        .store(upsample_elapsed, Ordering::Relaxed);
    state
        .dsd_modulate_time_ns
        .store(modulate_elapsed, Ordering::Relaxed);
    state
        .dsd_output_pending_samples
        .store(ds.output_pending_len() as u64, Ordering::Relaxed);
    let block_duration_ns = if ds.wire_rate > 0 {
        (tail_frames as f64 / ds.wire_rate as f64 * 1e9) as u64
    } else {
        0
    };
    state
        .block_duration_ns
        .store(block_duration_ns, Ordering::Relaxed);
    state.record_startup_render_block_ns(
        elapsed,
        block_duration_ns > 0 && elapsed > block_duration_ns,
    );
    state
        .dsd_stability_resets
        .store(ds.renderer.stability_resets(), Ordering::Relaxed);
    publish_dsd_limiter_metrics(ds.renderer.limiter_telemetry(), state);

    ds.staged_output_len() > 0
}

fn apply_dsd_boundary_fade_in(
    samples: &mut [f64],
    total_frames: usize,
    remaining_frames: &mut usize,
) {
    if total_frames == 0 || *remaining_frames == 0 {
        return;
    }

    for frame in samples.chunks_exact_mut(2) {
        if *remaining_frames == 0 {
            break;
        }
        let completed = total_frames.saturating_sub(*remaining_frames);
        let gain = (completed + 1) as f64 / total_frames as f64;
        frame[0] *= gain;
        frame[1] *= gain;
        *remaining_frames -= 1;
    }
}

// PCM rendering is a tight audio-engine boundary; explicit buffers and processors are clearer here.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_pcm_block(
    sess: &mut PlaybackSession,
    samples_l: &[f64],
    samples_r: &[f64],
    headroom_gain: f64,
    output_volume: f64,
    target_rate: u32,
    state: &AtomicPlayerState,
    eq_processor: &mut EqProcessor,
) -> bool {
    let start = Instant::now();
    let frames = sess
        .dsp_path
        .render(samples_l, samples_r, &mut sess.output_buffer);
    let elapsed = start.elapsed().as_nanos() as u64;
    finish_pcm_render(
        &mut sess.output_buffer,
        frames,
        elapsed,
        headroom_gain,
        output_volume,
        target_rate,
        state,
        eq_processor,
    )
}

pub(super) fn render_pcm_eof_tail(
    sess: &mut PlaybackSession,
    headroom_gain: f64,
    output_volume: f64,
    target_rate: u32,
    state: &AtomicPlayerState,
    eq_processor: &mut EqProcessor,
) -> bool {
    let start = Instant::now();
    let frames = sess.dsp_path.drain_eof(&mut sess.output_buffer);
    let elapsed = start.elapsed().as_nanos() as u64;
    finish_pcm_render(
        &mut sess.output_buffer,
        frames,
        elapsed,
        headroom_gain,
        output_volume,
        target_rate,
        state,
        eq_processor,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_pcm_dsp_path_eof_tail(
    dsp_path: &mut DspPath,
    output_buffer: &mut Vec<f64>,
    headroom_gain: f64,
    output_volume: f64,
    target_rate: u32,
    state: &AtomicPlayerState,
    eq_processor: &mut EqProcessor,
) -> bool {
    let start = Instant::now();
    let frames = dsp_path.drain_eof(output_buffer);
    let elapsed = start.elapsed().as_nanos() as u64;
    finish_pcm_render(
        output_buffer,
        frames,
        elapsed,
        headroom_gain,
        output_volume,
        target_rate,
        state,
        eq_processor,
    )
}

// Shared render finalization receives the same boundary data from normal and EOF-tail rendering.
#[allow(clippy::too_many_arguments)]
fn finish_pcm_render(
    output_buffer: &mut [f64],
    frames: usize,
    elapsed: u64,
    headroom_gain: f64,
    output_volume: f64,
    target_rate: u32,
    state: &AtomicPlayerState,
    eq_processor: &mut EqProcessor,
) -> bool {
    state.record_render_block_ns(elapsed);
    let block_dur = (frames as f64 / target_rate as f64 * 1e9) as u64;
    state.record_startup_render_block_ns(elapsed, block_dur > 0 && elapsed > block_dur);

    eq_processor.process_interleaved_stereo(output_buffer);
    if headroom_gain < 1.0 {
        for sample in output_buffer.iter_mut() {
            *sample *= headroom_gain;
        }
    }
    publish_signal_level_metrics(output_buffer, output_volume, state);

    if frames == 0 {
        return false;
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
    state.resample_time_ns.store(elapsed, Ordering::Relaxed);
    state.block_duration_ns.store(block_dur, Ordering::Relaxed);
    true
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::{
        apply_dsd_boundary_fade_in, clamp_headroom_db, effective_dsd_render_elapsed_ns,
        output_headroom_gain, publish_signal_level_metrics,
    };
    use crate::audio::engine::state::AtomicPlayerState;

    #[test]
    fn headroom_attenuation_is_clamped_and_converted_to_linear_gain() {
        assert_eq!(clamp_headroom_db(3.0), 0.0);
        assert_eq!(clamp_headroom_db(-30.0), -24.0);
        assert_eq!(clamp_headroom_db(f32::NAN), 0.0);
        assert!((output_headroom_gain(0.0) - 1.0).abs() < 1e-12);
        assert!((output_headroom_gain(-6.0) - 0.501_187).abs() < 1e-6);
    }

    #[test]
    fn paced_dsd_render_accounts_for_work_completed_between_calls() {
        assert_eq!(
            effective_dsd_render_elapsed_ns(15_000_000, 40_000_000),
            40_000_000
        );
        assert_eq!(
            effective_dsd_render_elapsed_ns(45_000_000, 40_000_000),
            45_000_000
        );
    }

    #[test]
    fn signal_level_metrics_track_current_peak_peak_hold_and_clips() {
        let state = AtomicPlayerState::new();

        publish_signal_level_metrics(&[0.25, -0.5, 0.75], 1.0, &state);

        assert!((f32::from_bits(state.signal_peak.load(Ordering::Relaxed)) - 0.75).abs() < 1e-6);
        assert!(
            (f32::from_bits(state.signal_peak_max.load(Ordering::Relaxed)) - 0.75).abs() < 1e-6
        );
        assert!(!state.signal_clipping.load(Ordering::Relaxed));
        assert_eq!(state.signal_clip_events.load(Ordering::Relaxed), 0);
        assert_eq!(state.signal_clip_samples.load(Ordering::Relaxed), 0);

        publish_signal_level_metrics(&[0.8, -1.1, 0.4], 1.0, &state);

        assert!((f32::from_bits(state.signal_peak.load(Ordering::Relaxed)) - 1.1).abs() < 1e-6);
        assert!((f32::from_bits(state.signal_peak_max.load(Ordering::Relaxed)) - 1.1).abs() < 1e-6);
        assert!(state.signal_clipping.load(Ordering::Relaxed));
        assert_eq!(state.signal_clip_events.load(Ordering::Relaxed), 1);
        assert_eq!(state.signal_clip_samples.load(Ordering::Relaxed), 1);

        publish_signal_level_metrics(&[0.2, -0.3], 1.0, &state);

        assert!((f32::from_bits(state.signal_peak.load(Ordering::Relaxed)) - 0.3).abs() < 1e-6);
        assert!((f32::from_bits(state.signal_peak_max.load(Ordering::Relaxed)) - 1.1).abs() < 1e-6);
        assert!(!state.signal_clipping.load(Ordering::Relaxed));
    }

    #[test]
    fn dsd_boundary_fade_in_scales_initial_frames_and_expires() {
        let mut samples = vec![1.0; 8];
        let mut remaining = 3;

        apply_dsd_boundary_fade_in(&mut samples, 3, &mut remaining);

        assert!(samples[0] > 0.0);
        assert!(samples[0] < samples[2]);
        assert!(samples[2] < samples[4]);
        assert_eq!(samples[6], 1.0);
        assert_eq!(remaining, 0);
    }
}
