use crate::audio::engine::signal_path::OutputMode;
use crate::audio::engine::state::{AtomicPlayerState, PLAYBACK_PLAYING};
#[cfg(all(target_os = "windows", feature = "asio"))]
use crate::audio::output::asio_output;
use ringbuf::{Consumer, HeapRb, Producer, SharedRb};
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

const PCM_OUTPUT_ROOM_SAMPLES: usize = 4096;
const DOP_OUTPUT_ROOM_MIN_SAMPLES: usize = 131_072;
const DOP_OUTPUT_ROOM_MS: usize = 250;
#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
const NATIVE_DSD_OUTPUT_ROOM_MIN_BYTES: usize = 131_072;
#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
const NATIVE_DSD_OUTPUT_ROOM_MS: usize = 250;
const PCM_START_PREROLL_MS: u32 = 50;
const HIGH_RATE_START_PREROLL_MS: u32 = 500;
const HIGH_RATE_PROTECTIVE_START_PREROLL_MS: u32 = 1000;
const HIGH_RATE_PCM_THRESHOLD_HZ: u32 = 176_400;
const PCM_TRANSITION_PREROLL_MS: u32 = 10;
#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
const DOP_TRANSITION_PREROLL_MS: u32 = 10;
const DSD_BOUNDARY_FADE_IN_MS: usize = 5;
const DSD_RECENT_RENDER_LOAD_WINDOW: usize = 64;
const EOF_OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const MAX_DSP_BUFFER_MS: u32 = 1000;

pub(crate) type AudioConsumer = Consumer<f64, Arc<SharedRb<f64, Vec<MaybeUninit<f64>>>>>;
pub(super) type AudioProducer = Producer<f64, Arc<SharedRb<f64, Vec<MaybeUninit<f64>>>>>;
pub(super) type DopConsumer = Consumer<i32, Arc<SharedRb<i32, Vec<MaybeUninit<i32>>>>>;
pub(super) type DopProducer = Producer<i32, Arc<SharedRb<i32, Vec<MaybeUninit<i32>>>>>;

/// Worker-thread state for the DSD output path. Present only while
/// `output_mode` is Dsd128 or Dsd256.
// DSD state is compiled on all platforms, even when the active transport falls back to PCM.
#[allow(dead_code)]
pub(super) struct DsdWorkerState {
    pub(super) renderer: crate::audio::dsd::dsd_render::DsdRenderer,
    pub(super) prod: DopProducer,
    pub(super) cons_opt: Option<DopConsumer>,
    pub(super) output_buf: Vec<i32>,
    pub(super) staged_pcm_l: Vec<f64>,
    pub(super) staged_pcm_r: Vec<f64>,
    pub(super) render_quantum_l: Vec<f64>,
    pub(super) render_quantum_r: Vec<f64>,
    pub(super) eq_scratch_l: Vec<f64>,
    pub(super) eq_scratch_r: Vec<f64>,
    pub(super) render_quantum_frames: usize,
    pub(super) recent_render_loads: Vec<f32>,
    pub(super) recent_render_load_cursor: usize,
    pub(super) dop_frame_rate: u32,
    pub(super) source_rate: u32,
    pub(super) wire_rate: u32,
    pub(super) mode: OutputMode,
    pub(super) dsp_buffer_ms: u32,
    pub(super) fade_in_total_frames: usize,
    pub(super) fade_in_remaining_frames: usize,
    pub(super) debug: DsdDebugState,
    #[cfg(all(target_os = "windows", feature = "asio"))]
    pub(super) native: Option<NativeDsdWorkerSink>,
}

impl DsdWorkerState {
    pub(super) fn output_pending_len(&self) -> usize {
        dsd_pending_len(self)
    }

    pub(super) fn staged_output_len(&self) -> usize {
        #[cfg(all(target_os = "windows", feature = "asio"))]
        {
            if let Some(native) = &self.native {
                return native.output_l.len().min(native.output_r.len());
            }
        }
        self.output_buf.len()
    }

    pub(super) fn append_staged_pcm(&mut self, samples_l: &[f64], samples_r: &[f64]) {
        let frames = samples_l.len().min(samples_r.len());
        self.staged_pcm_l.extend_from_slice(&samples_l[..frames]);
        self.staged_pcm_r.extend_from_slice(&samples_r[..frames]);
    }

    pub(super) fn staged_pcm_frames(&self) -> usize {
        self.staged_pcm_l.len().min(self.staged_pcm_r.len())
    }

    pub(super) fn take_render_quantum_from_pcm(
        &mut self,
        samples_l: &[f64],
        samples_r: &[f64],
        input_frame: &mut usize,
    ) -> Option<(Vec<f64>, Vec<f64>)> {
        let input_frames = samples_l.len().min(samples_r.len());
        if *input_frame >= input_frames {
            return None;
        }

        let quantum_frames = self.render_quantum_frames;
        if self.staged_pcm_frames() == 0
            && input_frames.saturating_sub(*input_frame) >= quantum_frames
        {
            let end = *input_frame + quantum_frames;
            let mut left = std::mem::take(&mut self.render_quantum_l);
            let mut right = std::mem::take(&mut self.render_quantum_r);
            left.clear();
            right.clear();
            left.extend_from_slice(&samples_l[*input_frame..end]);
            right.extend_from_slice(&samples_r[*input_frame..end]);
            *input_frame = end;
            return Some((left, right));
        }

        let staged_frames = self.staged_pcm_frames();
        if staged_frames < quantum_frames {
            let needed = quantum_frames - staged_frames;
            let available = input_frames - *input_frame;
            let to_stage = needed.min(available);
            let end = *input_frame + to_stage;
            self.append_staged_pcm(&samples_l[*input_frame..end], &samples_r[*input_frame..end]);
            *input_frame = end;
        }

        if self.staged_pcm_frames() == quantum_frames {
            self.take_all_staged_pcm()
        } else {
            None
        }
    }

    pub(super) fn take_all_staged_pcm(&mut self) -> Option<(Vec<f64>, Vec<f64>)> {
        let frames = self.staged_pcm_frames();
        if frames == 0 {
            return None;
        }
        let mut left = std::mem::take(&mut self.render_quantum_l);
        let mut right = std::mem::take(&mut self.render_quantum_r);
        left.clear();
        right.clear();
        std::mem::swap(&mut left, &mut self.staged_pcm_l);
        std::mem::swap(&mut right, &mut self.staged_pcm_r);
        left.truncate(frames);
        right.truncate(frames);
        Some((left, right))
    }

    pub(super) fn recycle_render_quantum_buffers(
        &mut self,
        mut left: Vec<f64>,
        mut right: Vec<f64>,
    ) {
        left.clear();
        right.clear();
        self.render_quantum_l = left;
        self.render_quantum_r = right;
    }

    pub(super) fn reset_for_playback_boundary(&mut self) {
        self.staged_pcm_l.clear();
        self.staged_pcm_r.clear();
        self.render_quantum_l.clear();
        self.render_quantum_r.clear();
        self.eq_scratch_l.clear();
        self.eq_scratch_r.clear();
        self.output_buf.clear();
        #[cfg(all(target_os = "windows", feature = "asio"))]
        if let Some(native) = self.native.as_mut() {
            native.output_l.clear();
            native.output_r.clear();
        }
        self.recent_render_loads.clear();
        self.recent_render_load_cursor = 0;
        self.reset_boundary_fade_in();
        self.renderer.reset();
    }

    pub(super) fn reset_for_playback_boundary_with_diagnostics(
        &mut self,
        state: &AtomicPlayerState,
    ) {
        state.modulator_reset_count.fetch_add(1, Ordering::Relaxed);
        self.reset_for_playback_boundary();
    }

    pub(super) fn reset_boundary_fade_in(&mut self) {
        let frames = dsd_boundary_fade_in_frames(self.wire_rate);
        self.fade_in_total_frames = frames;
        self.fade_in_remaining_frames = frames;
    }

    pub(super) fn record_render_load(&mut self, load: f32, state: &AtomicPlayerState) {
        if !load.is_finite() || load < 0.0 {
            return;
        }
        if self.recent_render_loads.len() < DSD_RECENT_RENDER_LOAD_WINDOW {
            self.recent_render_loads.push(load);
        } else {
            self.recent_render_loads[self.recent_render_load_cursor] = load;
            self.recent_render_load_cursor =
                (self.recent_render_load_cursor + 1) % DSD_RECENT_RENDER_LOAD_WINDOW;
        }
        let p95 = percentile_f32(&self.recent_render_loads, 0.95);
        let p99 = percentile_f32(&self.recent_render_loads, 0.99);
        state
            .dsd_recent_load_p95
            .store(p95.to_bits(), Ordering::Relaxed);
        state
            .dsd_recent_load_p99
            .store(p99.to_bits(), Ordering::Relaxed);
    }
}

pub(super) fn dsd_boundary_fade_in_frames(wire_rate: u32) -> usize {
    let rate = wire_rate.max(176_400) as usize;
    div_ceil_usize(rate * DSD_BOUNDARY_FADE_IN_MS, 1000).max(1)
}

pub(super) fn dsd_render_quantum_frames(source_rate: u32, mode: OutputMode) -> usize {
    let target_ms = match mode {
        OutputMode::Dsd256 => 85,
        OutputMode::Dsd128 | OutputMode::Dsd64 => 60,
        OutputMode::Pcm => unreachable!("PCM has no DSD render quantum"),
    };
    let source_rate = source_rate.max(1) as usize;
    let raw = (source_rate * target_ms).div_ceil(1000);
    raw.next_power_of_two().clamp(4096, 16384)
}

fn percentile_f32(values: &[f32], percentile: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let rank = ((sorted.len() - 1) as f32 * percentile).ceil() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

// Debug counters are enabled through diagnostic probes before every field is read in release paths.
#[allow(dead_code)]
pub(super) struct DsdDebugState {
    render_blocks: u64,
    write_blocks: u64,
    zero_output_blocks: u64,
    saw_nonzero_output: bool,
    last_render_log: Instant,
    last_write_log: Instant,
    last_resets: u64,
    last_clamps: u64,
}

impl DsdDebugState {
    pub(super) fn new() -> Self {
        let now = Instant::now();
        Self {
            render_blocks: 0,
            write_blocks: 0,
            zero_output_blocks: 0,
            saw_nonzero_output: false,
            last_render_log: now,
            last_write_log: now,
            last_resets: 0,
            last_clamps: 0,
        }
    }

    // Render telemetry records a fixed set of counters from the audio callback boundary.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn log_render_block(
        &mut self,
        mode: OutputMode,
        modulator: &'static str,
        source_frames: usize,
        output_len: usize,
        pending_before_write: usize,
        upsample_ns: u64,
        modulate_ns: u64,
        elapsed_ns: u64,
        block_duration_ns: u64,
        resets: u64,
        clamps: u64,
    ) {
        self.render_blocks += 1;
        if output_len == 0 {
            self.zero_output_blocks += 1;
        }
        let first_nonzero = output_len > 0 && !self.saw_nonzero_output;
        if output_len > 0 {
            self.saw_nonzero_output = true;
        }
        let health_changed = resets != self.last_resets || clamps != self.last_clamps;
        let now = Instant::now();
        let periodic = now.duration_since(self.last_render_log) >= Duration::from_secs(2);
        let first_blocks = self.render_blocks <= 6;
        let slow_ratio = if block_duration_ns > 0 {
            elapsed_ns as f64 / block_duration_ns as f64
        } else {
            0.0
        };
        let slow_block = block_duration_ns > 0
            && slow_ratio >= 0.95
            && now.duration_since(self.last_render_log) >= Duration::from_millis(500);

        if crate::audio::debug::audio_debug_enabled()
            && (first_blocks || first_nonzero || health_changed || periodic || slow_block)
        {
            eprintln!(
                "AudioWorker DEBUG: DSD render block #{} mode={} modulator={} source_frames={} staged_output={} pending_before_write={} zero_outputs={} upsample={:.2}ms modulate={:.2}ms total={:.2}ms block_budget={:.2}ms load={:.2}x resets={} clamps={}",
                self.render_blocks,
                mode.as_name(),
                modulator,
                source_frames,
                output_len,
                pending_before_write,
                self.zero_output_blocks,
                upsample_ns as f64 / 1_000_000.0,
                modulate_ns as f64 / 1_000_000.0,
                elapsed_ns as f64 / 1_000_000.0,
                block_duration_ns as f64 / 1_000_000.0,
                slow_ratio,
                resets,
                clamps,
            );
            self.last_render_log = now;
        }

        self.last_resets = resets;
        self.last_clamps = clamps;
    }

    pub(super) fn log_write_block(
        &mut self,
        label: &str,
        staged_len: usize,
        written: usize,
        stalls: u64,
        pending_after_write: usize,
    ) {
        self.write_blocks += 1;
        let now = Instant::now();
        let periodic = now.duration_since(self.last_write_log) >= Duration::from_secs(2);
        let partial = written < staged_len;
        let severe_stall = stalls >= 50;

        if crate::audio::debug::audio_debug_enabled()
            && (self.write_blocks <= 6 || periodic || partial || severe_stall)
        {
            eprintln!(
                "AudioWorker DEBUG: DSD write #{} path={} staged={} written={} pending_after_write={} ring_stalls={}",
                self.write_blocks, label, staged_len, written, pending_after_write, stalls,
            );
            self.last_write_log = now;
        }
    }
}

#[cfg(all(target_os = "windows", feature = "asio"))]
pub(super) struct NativeDsdWorkerSink {
    pub(super) prod_l: asio_output::NativeProducer,
    pub(super) prod_r: asio_output::NativeProducer,
    pub(super) output_l: Vec<u8>,
    pub(super) output_r: Vec<u8>,
}

fn configured_buffer_ms(configured_ms: u32, auto_ms: u32) -> u32 {
    if configured_ms == 0 {
        auto_ms
    } else {
        configured_ms.min(MAX_DSP_BUFFER_MS)
    }
}

fn high_rate_start_preroll_ms(configured_ms: u32, protective: bool) -> u32 {
    let minimum = if protective {
        HIGH_RATE_PROTECTIVE_START_PREROLL_MS
    } else {
        HIGH_RATE_START_PREROLL_MS
    };
    if configured_ms == 0 {
        minimum
    } else {
        configured_ms.min(MAX_DSP_BUFFER_MS).max(minimum)
    }
}

fn pcm_start_preroll_ms(target_rate: u32, dsp_buffer_ms: u32, protective: bool) -> u32 {
    if target_rate >= HIGH_RATE_PCM_THRESHOLD_HZ {
        high_rate_start_preroll_ms(dsp_buffer_ms, protective)
    } else {
        configured_buffer_ms(dsp_buffer_ms, PCM_START_PREROLL_MS)
    }
}

fn dsd_start_preroll_ms(dsp_buffer_ms: u32, protective: bool) -> u32 {
    high_rate_start_preroll_ms(dsp_buffer_ms, protective)
}

fn div_ceil_usize(n: usize, d: usize) -> usize {
    n.div_ceil(d)
}

fn base_ring_buffer_capacity_samples(target_rate: u32) -> usize {
    let rate = target_rate.max(48_000) as usize;

    // Interleaved stereo samples. At 384kHz this is about 500ms of cushion,
    // while lower-rate modes stay at or above the previous 96k-sample buffer.
    rate.clamp(96_000, 768_000)
}

#[allow(dead_code)]
pub(super) fn pcm_start_preroll_samples(target_rate: u32, dsp_buffer_ms: u32) -> usize {
    pcm_start_preroll_samples_with_protection(target_rate, dsp_buffer_ms, false)
}

fn pcm_start_preroll_samples_with_protection(
    target_rate: u32,
    dsp_buffer_ms: u32,
    protective: bool,
) -> usize {
    let rate = target_rate.max(48_000) as usize;
    let ms = pcm_start_preroll_ms(target_rate, dsp_buffer_ms, protective) as usize;
    let samples = div_ceil_usize(rate * 2 * ms, 1000);
    if dsp_buffer_ms == 0 {
        samples.max(PCM_OUTPUT_ROOM_SAMPLES)
    } else {
        samples
    }
}

#[allow(dead_code)]
pub(super) fn dop_start_preroll_samples(dop_frame_rate: u32, dsp_buffer_ms: u32) -> usize {
    dop_start_preroll_samples_with_protection(dop_frame_rate, dsp_buffer_ms, false)
}

fn dop_start_preroll_samples_with_protection(
    dop_frame_rate: u32,
    dsp_buffer_ms: u32,
    protective: bool,
) -> usize {
    let rate = dop_frame_rate.max(176_400) as usize;
    let ms = dsd_start_preroll_ms(dsp_buffer_ms, protective) as usize;
    let samples = div_ceil_usize(rate * 2 * ms, 1000);
    if dsp_buffer_ms == 0 {
        samples.max(dop_output_room_samples(dop_frame_rate))
    } else {
        samples
    }
}

fn pcm_transition_preroll_samples(target_rate: u32) -> usize {
    let rate = target_rate.max(48_000) as usize;
    div_ceil_usize(rate * 2 * PCM_TRANSITION_PREROLL_MS as usize, 1000).max(1)
}

#[cfg(test)]
fn dop_transition_preroll_samples(dop_frame_rate: u32) -> usize {
    let rate = dop_frame_rate.max(176_400) as usize;
    div_ceil_usize(rate * 2 * DOP_TRANSITION_PREROLL_MS as usize, 1000).max(1)
}

pub(super) fn dop_output_room_samples(dop_frame_rate: u32) -> usize {
    let rate = dop_frame_rate.max(176_400) as usize;
    div_ceil_usize(rate * 2 * DOP_OUTPUT_ROOM_MS, 1000).max(DOP_OUTPUT_ROOM_MIN_SAMPLES)
}

#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
pub(super) fn native_dsd_output_room_bytes(wire_rate: u32) -> usize {
    let bytes_per_sec = div_ceil_usize(wire_rate.max(176_400) as usize, 8);
    div_ceil_usize(bytes_per_sec * NATIVE_DSD_OUTPUT_ROOM_MS, 1000)
        .max(NATIVE_DSD_OUTPUT_ROOM_MIN_BYTES)
}

#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
fn native_dsd_start_preroll_bytes_unclamped(wire_rate: u32, dsp_buffer_ms: u32) -> usize {
    native_dsd_start_preroll_bytes_with_protection(wire_rate, dsp_buffer_ms, false)
}

#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
fn native_dsd_start_preroll_bytes_with_protection(
    wire_rate: u32,
    dsp_buffer_ms: u32,
    protective: bool,
) -> usize {
    let bytes_per_sec = div_ceil_usize(wire_rate.max(176_400) as usize, 8);
    let ms = dsd_start_preroll_ms(dsp_buffer_ms, protective) as usize;
    div_ceil_usize(bytes_per_sec * ms, 1000)
}

#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
fn native_dsd_transition_preroll_bytes(wire_rate: u32) -> usize {
    let bytes_per_sec = div_ceil_usize(wire_rate.max(176_400) as usize, 8);
    div_ceil_usize(bytes_per_sec * DOP_TRANSITION_PREROLL_MS as usize, 1000).max(1)
}

#[cfg(any(test, all(target_os = "windows", feature = "asio")))]
pub(crate) fn native_dsd_ring_capacity_bytes(
    wire_rate: u32,
    callback_bytes: usize,
    dsp_buffer_ms: u32,
) -> usize {
    let base = (wire_rate as usize / 16).max(callback_bytes * 8);
    let target = native_dsd_start_preroll_bytes_with_protection(wire_rate, dsp_buffer_ms, true);
    let desired = base.max(target * 2).max(target + callback_bytes * 8);
    desired.div_ceil(callback_bytes) * callback_bytes
}

pub(super) fn ring_buffer_capacity_samples(target_rate: u32, dsp_buffer_ms: u32) -> usize {
    let base = base_ring_buffer_capacity_samples(target_rate);
    let target = pcm_start_preroll_samples_with_protection(target_rate, dsp_buffer_ms, true);
    if dsp_buffer_ms == 0 && target_rate < HIGH_RATE_PCM_THRESHOLD_HZ {
        return base;
    }
    base.max(target * 2).max(target + PCM_OUTPUT_ROOM_SAMPLES)
}

pub(super) fn new_audio_ring(
    target_rate: u32,
    dsp_buffer_ms: u32,
) -> (AudioProducer, AudioConsumer) {
    HeapRb::<f64>::new(ring_buffer_capacity_samples(target_rate, dsp_buffer_ms)).split()
}

pub(super) fn ensure_audio_ring_capacity(
    target_rate: u32,
    dsp_buffer_ms: u32,
    prod: &mut AudioProducer,
    cons_opt: &mut Option<AudioConsumer>,
    ring_capacity: &mut usize,
) -> bool {
    let desired_ring_capacity = ring_buffer_capacity_samples(target_rate, dsp_buffer_ms);
    if cons_opt.is_none() || *ring_capacity != desired_ring_capacity {
        let (new_prod, new_cons) = new_audio_ring(target_rate, dsp_buffer_ms);
        *prod = new_prod;
        *cons_opt = Some(new_cons);
        *ring_capacity = desired_ring_capacity;
        return true;
    }
    false
}

pub(super) fn output_has_room(
    dsd_state: Option<&DsdWorkerState>,
    audio_prod: &AudioProducer,
) -> bool {
    match dsd_state {
        Some(ds) => dsd_has_room(ds),
        None => audio_prod.free_len() > PCM_OUTPUT_ROOM_SAMPLES,
    }
}

fn dsd_has_room(ds: &DsdWorkerState) -> bool {
    #[cfg(all(target_os = "windows", feature = "asio"))]
    {
        if let Some(native) = &ds.native {
            let required = native_dsd_output_room_bytes(ds.wire_rate).max(ds.staged_output_len());
            return native.prod_l.free_len().min(native.prod_r.free_len()) >= required;
        }
    }
    ds.prod.free_len() > dop_output_room_samples(ds.dop_frame_rate)
}

pub(super) fn output_start_preroll_ready(
    dsd_state: Option<&DsdWorkerState>,
    audio_prod: &AudioProducer,
    target_rate: u32,
    dsp_buffer_ms: u32,
    flush_pending: bool,
    transition_preroll: bool,
    protective_preroll: bool,
) -> bool {
    if flush_pending {
        return false;
    }
    match dsd_state {
        Some(ds) => dsd_start_preroll_ready(ds, transition_preroll, protective_preroll),
        None => {
            let capacity = audio_prod.len() + audio_prod.free_len();
            let target = if transition_preroll {
                pcm_transition_preroll_samples(target_rate)
            } else {
                pcm_start_preroll_samples_with_protection(
                    target_rate,
                    dsp_buffer_ms,
                    protective_preroll,
                )
            }
            .min(capacity / 2);
            audio_prod.len() >= target
        }
    }
}

fn dsd_start_preroll_ready(
    ds: &DsdWorkerState,
    transition_preroll: bool,
    protective_preroll: bool,
) -> bool {
    #[cfg(all(target_os = "windows", feature = "asio"))]
    {
        if let Some(native) = &ds.native {
            let pending = native.prod_l.len().min(native.prod_r.len());
            let capacity = pending + native.prod_l.free_len().min(native.prod_r.free_len());
            let target = if transition_preroll {
                native_dsd_transition_preroll_bytes(ds.wire_rate)
            } else {
                native_dsd_start_preroll_bytes_with_protection(
                    ds.wire_rate,
                    ds.dsp_buffer_ms,
                    protective_preroll,
                )
            }
            .min(capacity / 2);
            return pending >= target;
        }
    }

    let _ = transition_preroll;
    let capacity = ds.prod.len() + ds.prod.free_len();
    let target = dop_start_preroll_samples_with_protection(
        ds.dop_frame_rate,
        ds.dsp_buffer_ms,
        protective_preroll,
    )
    .min(capacity / 2);
    ds.prod.len() >= target
}

pub(super) fn write_audio_blocking(
    prod: &mut AudioProducer,
    samples: &[f64],
    mut should_continue: impl FnMut() -> bool,
) {
    let mut written = 0;
    while written < samples.len() {
        if !should_continue() {
            break;
        }
        let n = prod.push_slice(&samples[written..]);
        written += n;
        if n == 0 {
            thread::sleep(Duration::from_millis(1));
        }
    }
}

pub(super) fn write_dsd_output_blocking(
    ds: &mut DsdWorkerState,
    mut should_continue: impl FnMut() -> bool,
) {
    #[cfg(all(target_os = "windows", feature = "asio"))]
    if ds.native.is_some() {
        let (staged_len, written, stalls) = {
            let native = ds.native.as_mut().expect("native DSD sink present");
            let staged_len = native.output_l.len().min(native.output_r.len());
            let mut stalls = 0_u64;
            let mut written = 0;
            while written < staged_len {
                if !should_continue() {
                    break;
                }
                let writable = native
                    .prod_l
                    .free_len()
                    .min(native.prod_r.free_len())
                    .min(staged_len - written);
                if writable == 0 {
                    stalls += 1;
                    thread::sleep(Duration::from_millis(1));
                    continue;
                }
                let n_l = native
                    .prod_l
                    .push_slice(&native.output_l[written..written + writable]);
                let n_r = native
                    .prod_r
                    .push_slice(&native.output_r[written..written + writable]);
                debug_assert_eq!(n_l, n_r);
                written += n_l.min(n_r);
            }
            (staged_len, written, stalls)
        };
        let pending = ds.output_pending_len();
        ds.debug
            .log_write_block("native-dsd", staged_len, written, stalls, pending);
        return;
    }

    let staged_len = ds.output_buf.len();
    let mut stalls = 0_u64;
    let mut written = 0;
    while written < ds.output_buf.len() {
        if !should_continue() {
            break;
        }
        let n = ds.prod.push_slice(&ds.output_buf[written..]);
        written += n;
        if n == 0 {
            stalls += 1;
            thread::sleep(Duration::from_millis(1));
        }
    }
    let pending = ds.output_pending_len();
    ds.debug
        .log_write_block("dop", staged_len, written, stalls, pending);
}

/// At end of stream the EC modulators still hold `lookahead_depth - 1` samples per
/// channel; emit that tail (and, on the native path, idle padding to a byte boundary)
/// before draining the output ring.
pub(super) fn flush_dsd_tail_at_eof(
    ds: &mut DsdWorkerState,
    mut should_continue: impl FnMut() -> bool,
) {
    if !should_continue() {
        return;
    }
    #[cfg(all(target_os = "windows", feature = "asio"))]
    if let Some(native) = ds.native.as_mut() {
        native.output_l.clear();
        native.output_r.clear();
        ds.renderer
            .flush_modulators_and_pack_native(&mut native.output_l, &mut native.output_r);
        ds.renderer
            .flush_native_with_idle(&mut native.output_l, &mut native.output_r);
        write_dsd_output_blocking(ds, &mut should_continue);
        return;
    }

    ds.output_buf.clear();
    ds.renderer.flush_modulators_and_pack(&mut ds.output_buf);
    if crate::audio::debug::audio_debug_enabled() {
        eprintln!(
            "AudioWorker DEBUG: DSD EOF flush staged {} DoP samples",
            ds.output_buf.len()
        );
    }
    write_dsd_output_blocking(ds, &mut should_continue);
}

pub(super) fn output_pending_len(
    dsd_state: Option<&DsdWorkerState>,
    audio_prod: &AudioProducer,
) -> usize {
    match dsd_state {
        Some(ds) => dsd_pending_len(ds),
        None => audio_prod.len(),
    }
}

fn dsd_pending_len(ds: &DsdWorkerState) -> usize {
    #[cfg(all(target_os = "windows", feature = "asio"))]
    {
        if let Some(native) = &ds.native {
            return native.prod_l.len().max(native.prod_r.len());
        }
    }
    ds.prod.len()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OutputDrainResult {
    Drained,
    Interrupted,
    TimedOut,
}

pub(super) fn wait_for_output_drain(
    dsd_state: Option<&DsdWorkerState>,
    audio_prod: &AudioProducer,
    state: &AtomicPlayerState,
    mut should_continue: impl FnMut() -> bool,
) -> OutputDrainResult {
    let started = Instant::now();
    loop {
        if output_pending_len(dsd_state, audio_prod) == 0 {
            return OutputDrainResult::Drained;
        }
        if state.state.load(std::sync::atomic::Ordering::Relaxed) != PLAYBACK_PLAYING {
            return OutputDrainResult::Interrupted;
        }
        if !should_continue() {
            return OutputDrainResult::Interrupted;
        }
        if started.elapsed() >= EOF_OUTPUT_DRAIN_TIMEOUT {
            eprintln!(
                "AudioWorker: EOF output drain timed out with {} samples pending",
                output_pending_len(dsd_state, audio_prod)
            );
            return OutputDrainResult::TimedOut;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

pub(super) fn flush_and_wait_for_output_at_eof(
    dsd_state: &mut Option<DsdWorkerState>,
    audio_prod: &AudioProducer,
    state: &AtomicPlayerState,
    mut should_continue: impl FnMut() -> bool,
) -> OutputDrainResult {
    state
        .eof_drain_requested
        .store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(ds) = dsd_state.as_mut() {
        flush_dsd_tail_at_eof(ds, &mut should_continue);
    }
    let result = wait_for_output_drain(dsd_state.as_ref(), audio_prod, state, should_continue);
    state
        .eof_drain_requested
        .store(false, std::sync::atomic::Ordering::Relaxed);
    result
}

/// DoP ring buffer sized in `i32` samples. Interleaved stereo i32 at the DoP frame
/// rate (= DSD rate / 16). Capacity follows the same time-cushion rule as PCM.
// DoP buffering is staged for platform transports that are not active on every build target.
#[allow(dead_code)]
pub(super) fn new_dop_ring(dop_frame_rate: u32, dsp_buffer_ms: u32) -> (DopProducer, DopConsumer) {
    let base = base_ring_buffer_capacity_samples(dop_frame_rate);
    let target = dop_start_preroll_samples_with_protection(dop_frame_rate, dsp_buffer_ms, true);
    let capacity = if dsp_buffer_ms == 0 && dop_frame_rate < HIGH_RATE_PCM_THRESHOLD_HZ {
        base
    } else {
        base.max(target * 2)
            .max(target + dop_output_room_samples(dop_frame_rate))
    };
    HeapRb::<i32>::new(capacity).split()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn pcm_output_room_uses_threshold_cushion() {
        let (mut prod, _cons) = new_audio_ring(48_000, 0);
        assert!(output_has_room(None, &prod));

        let fill = ring_buffer_capacity_samples(48_000, 0) - 4096;
        let samples = vec![0.0; fill];
        assert_eq!(prod.push_slice(&samples), fill);

        assert!(!output_has_room(None, &prod));
        assert_eq!(output_pending_len(None, &prod), fill);
    }

    #[test]
    fn write_audio_blocking_publishes_pcm_samples() {
        let (mut prod, _cons) = new_audio_ring(48_000, 0);

        write_audio_blocking(&mut prod, &[0.25, -0.5, 0.75], || true);

        assert_eq!(prod.len(), 3);
    }

    #[test]
    fn eof_drain_stops_when_cancelled() {
        let (mut prod, _cons) = new_audio_ring(48_000, 0);
        assert_eq!(prod.push_slice(&[0.25, -0.25]), 2);
        let state = AtomicPlayerState::new();
        state.state.store(PLAYBACK_PLAYING, Ordering::Relaxed);
        let mut polls = 0;

        let result = wait_for_output_drain(None, &prod, &state, || {
            polls += 1;
            polls < 2
        });

        assert_eq!(result, OutputDrainResult::Interrupted);
        assert_eq!(prod.len(), 2);
        assert_eq!(state.state.load(Ordering::Relaxed), PLAYBACK_PLAYING);
        assert_eq!(polls, 2);
    }

    #[test]
    fn shutdown_signal_releases_backpressured_audio_write() {
        let (mut prod, _cons) = new_audio_ring(48_000, 0);
        let capacity = prod.free_len();
        let fill = vec![0.0; capacity];
        assert_eq!(prod.push_slice(&fill), capacity);

        let shutdown = Arc::new(AtomicBool::new(false));
        let entered_wait = Arc::new(AtomicBool::new(false));
        let writer_shutdown = Arc::clone(&shutdown);
        let writer_entered_wait = Arc::clone(&entered_wait);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let writer = thread::spawn(move || {
            write_audio_blocking(&mut prod, &[1.0], || {
                writer_entered_wait.store(true, Ordering::Release);
                !writer_shutdown.load(Ordering::Acquire)
            });
            done_tx.send(()).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        while !entered_wait.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(entered_wait.load(Ordering::Acquire));
        assert!(
            done_rx.recv_timeout(Duration::from_millis(10)).is_err(),
            "the full output ring should keep the writer backpressured"
        );

        shutdown.store(true, Ordering::Release);
        done_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("shutdown should promptly release the backpressured write");
        writer.join().unwrap();
    }

    #[test]
    fn auto_pcm_start_preroll_preserves_existing_target_cushion() {
        let (mut prod, _cons) = new_audio_ring(48_000, 0);
        assert!(!output_start_preroll_ready(
            None, &prod, 48_000, 0, false, false, false
        ));

        let almost_ready = (48_000 * 2 * PCM_START_PREROLL_MS as usize / 1000) - 1;
        let samples = vec![0.0; almost_ready];
        assert_eq!(prod.push_slice(&samples), almost_ready);
        assert!(!output_start_preroll_ready(
            None, &prod, 48_000, 0, false, false, false
        ));

        assert_eq!(prod.push(0.0), Ok(()));
        assert!(output_start_preroll_ready(
            None, &prod, 48_000, 0, false, false, false
        ));
    }

    #[test]
    fn explicit_pcm_start_preroll_uses_configured_milliseconds() {
        let (mut prod, _cons) = new_audio_ring(48_000, 200);
        let target = 48_000 * 2 * 200 / 1000;
        assert_eq!(pcm_start_preroll_samples(48_000, 200), target);

        let samples = vec![0.0; target - 1];
        assert_eq!(prod.push_slice(&samples), target - 1);
        assert!(!output_start_preroll_ready(
            None, &prod, 48_000, 200, false, false, false
        ));

        assert_eq!(prod.push(0.0), Ok(()));
        assert!(output_start_preroll_ready(
            None, &prod, 48_000, 200, false, false, false
        ));
    }

    #[test]
    fn transition_pcm_start_preroll_uses_short_handoff_cushion() {
        let (mut prod, _cons) = new_audio_ring(48_000, 1000);
        let transition_target = pcm_transition_preroll_samples(48_000);
        assert!(transition_target < pcm_start_preroll_samples(48_000, 1000));

        let samples = vec![0.0; transition_target - 1];
        assert_eq!(prod.push_slice(&samples), transition_target - 1);
        assert!(!output_start_preroll_ready(
            None, &prod, 48_000, 1000, false, true, false
        ));

        assert_eq!(prod.push(0.0), Ok(()));
        assert!(output_start_preroll_ready(
            None, &prod, 48_000, 1000, false, true, false
        ));
        assert!(!output_start_preroll_ready(
            None, &prod, 48_000, 1000, false, false, false
        ));
    }

    #[test]
    fn pending_flush_blocks_preroll_readiness() {
        let (mut prod, _cons) = new_audio_ring(48_000, 1000);
        let target = pcm_start_preroll_samples(48_000, 1000);
        let samples = vec![0.0; target];
        assert_eq!(prod.push_slice(&samples), target);

        assert!(!output_start_preroll_ready(
            None, &prod, 48_000, 1000, true, false, false
        ));
        assert!(output_start_preroll_ready(
            None, &prod, 48_000, 1000, false, false, false
        ));
    }

    #[test]
    fn dsd_render_quantum_tracks_source_rate_and_mode() {
        assert_eq!(dsd_render_quantum_frames(44_100, OutputMode::Dsd256), 4096);
        assert_eq!(dsd_render_quantum_frames(48_000, OutputMode::Dsd256), 4096);
        assert_eq!(dsd_render_quantum_frames(96_000, OutputMode::Dsd256), 8192);
        assert_eq!(
            dsd_render_quantum_frames(192_000, OutputMode::Dsd256),
            16384
        );
        assert_eq!(dsd_render_quantum_frames(96_000, OutputMode::Dsd128), 8192);
    }

    #[test]
    fn dsd_staging_accumulates_full_quantum_and_keeps_tail() {
        use crate::audio::dsd::dsd_render::{DsdRate, DsdRenderer};
        use crate::audio::dsp::resampler::FilterType;

        let renderer = DsdRenderer::new(FilterType::Minimum16k, 96_000, DsdRate::Dsd128)
            .expect("calibrated test renderer");
        let mut ds = super::super::dsd_path::new_dop_worker_state(
            renderer,
            96_000,
            DsdRate::Dsd128.wire_rate_for_source(96_000).unwrap(),
            OutputMode::Dsd128,
            0,
        );
        let quantum = ds.render_quantum_frames;
        let almost = vec![0.0; quantum - 1];
        let mut input_frame = 0;

        assert!(
            ds.take_render_quantum_from_pcm(&almost, &almost, &mut input_frame)
                .is_none()
        );
        assert_eq!(input_frame, quantum - 1);
        assert_eq!(ds.staged_pcm_frames(), quantum - 1);

        input_frame = 0;
        let input_l = [1.0, 2.0];
        let input_r = [3.0, 4.0];
        let (left, right) = ds
            .take_render_quantum_from_pcm(&input_l, &input_r, &mut input_frame)
            .expect("full quantum");

        assert_eq!(left.len(), quantum);
        assert_eq!(right.len(), quantum);
        assert_eq!(input_frame, 1);
        ds.recycle_render_quantum_buffers(left, right);

        assert!(
            ds.take_render_quantum_from_pcm(&input_l, &input_r, &mut input_frame)
                .is_none()
        );
        assert_eq!(ds.staged_pcm_frames(), 1);
        assert_eq!(ds.take_all_staged_pcm().expect("tail").0, vec![2.0]);
    }

    #[test]
    fn dsd_playback_boundary_discards_staged_pcm() {
        use crate::audio::dsd::dsd_render::{DsdRate, DsdRenderer};
        use crate::audio::dsp::resampler::FilterType;

        let renderer = DsdRenderer::new(FilterType::Minimum16k, 44_100, DsdRate::Dsd128)
            .expect("calibrated test renderer");
        let mut ds = super::super::dsd_path::new_dop_worker_state(
            renderer,
            44_100,
            DsdRate::Dsd128.wire_rate_for_source(44_100).unwrap(),
            OutputMode::Dsd128,
            0,
        );

        ds.append_staged_pcm(&[0.1, 0.2], &[0.3, 0.4]);
        ds.output_buf.extend_from_slice(&[1, 2, 3]);
        ds.record_render_load(1.25, &AtomicPlayerState::new());
        ds.reset_for_playback_boundary();

        assert_eq!(ds.staged_pcm_frames(), 0);
        assert_eq!(ds.staged_output_len(), 0);
        assert!(ds.recent_render_loads.is_empty());
    }

    #[test]
    fn diagnostics_count_external_modulator_resets() {
        use crate::audio::dsd::dsd_render::{DsdRate, DsdRenderer};
        use crate::audio::dsp::resampler::FilterType;

        let renderer = DsdRenderer::new(FilterType::Minimum16k, 44_100, DsdRate::Dsd128)
            .expect("calibrated test renderer");
        let mut ds = super::super::dsd_path::new_dop_worker_state(
            renderer,
            44_100,
            DsdRate::Dsd128.wire_rate_for_source(44_100).unwrap(),
            OutputMode::Dsd128,
            0,
        );
        let state = AtomicPlayerState::new();

        ds.reset_for_playback_boundary_with_diagnostics(&state);

        assert_eq!(state.modulator_reset_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn explicit_dsd_preroll_targets_use_path_native_units() {
        assert_eq!(dop_start_preroll_samples(352_800, 200), 352_800);
        let native_target = 5_644_800 / 8 * HIGH_RATE_START_PREROLL_MS as usize / 1000;
        assert!(native_dsd_ring_capacity_bytes(5_644_800, 512, 200) >= native_target * 2);
        assert!(
            native_dsd_transition_preroll_bytes(5_644_800)
                < native_dsd_start_preroll_bytes_unclamped(5_644_800, 0)
        );
    }

    #[test]
    fn auto_native_dsd_preroll_uses_bytes_not_bits() {
        assert_eq!(
            native_dsd_start_preroll_bytes_unclamped(5_644_800, 0),
            352_800
        );
        assert_eq!(
            native_dsd_start_preroll_bytes_unclamped(2_822_400, 0),
            176_400
        );
    }

    #[test]
    fn native_dsd_output_room_uses_bytes_with_legacy_floor() {
        assert_eq!(native_dsd_output_room_bytes(2_822_400), 131_072);
        assert_eq!(native_dsd_output_room_bytes(5_644_800), 176_400);
        assert_eq!(native_dsd_output_room_bytes(11_289_600), 352_800);
    }

    #[test]
    fn transition_dop_preroll_uses_full_start_cushion() {
        use crate::audio::dsd::dsd_render::{DsdRate, DsdRenderer};
        use crate::audio::dsp::resampler::FilterType;

        let renderer = DsdRenderer::new(FilterType::Minimum16k, 44_100, DsdRate::Dsd128)
            .expect("calibrated test renderer");
        let mut ds = super::super::dsd_path::new_dop_worker_state(
            renderer,
            44_100,
            DsdRate::Dsd128.wire_rate_for_source(44_100).unwrap(),
            OutputMode::Dsd128,
            0,
        );
        let (audio_prod, _audio_cons) = new_audio_ring(48_000, 0);
        let capacity = ds.prod.len() + ds.prod.free_len();
        let full_target =
            dop_start_preroll_samples(ds.dop_frame_rate, ds.dsp_buffer_ms).min(capacity / 2);
        let short_target = dop_transition_preroll_samples(ds.dop_frame_rate);
        assert!(short_target < full_target);

        let samples = vec![0; full_target - 1];
        assert_eq!(ds.prod.push_slice(&samples), full_target - 1);
        assert!(!output_start_preroll_ready(
            Some(&ds),
            &audio_prod,
            48_000,
            0,
            false,
            true,
            false
        ));

        assert_eq!(ds.prod.push(0), Ok(()));
        assert!(output_start_preroll_ready(
            Some(&ds),
            &audio_prod,
            48_000,
            0,
            false,
            true,
            false
        ));
    }

    #[test]
    fn dop_output_room_scales_by_rate_with_legacy_floor() {
        assert_eq!(
            dop_output_room_samples(176_400),
            DOP_OUTPUT_ROOM_MIN_SAMPLES
        );
        assert_eq!(dop_output_room_samples(352_800), 176_400);
        assert_eq!(dop_output_room_samples(705_600), 352_800);
    }

    #[test]
    fn explicit_dop_ring_capacity_grows_for_rate_scaled_room() {
        let target = dop_start_preroll_samples(705_600, 1000);
        let (prod, _cons) = new_dop_ring(705_600, 1000);
        let capacity = prod.len() + prod.free_len();

        assert!(capacity >= target * 2);
        assert!(capacity >= target + dop_output_room_samples(705_600));
    }

    #[test]
    fn ensuring_audio_ring_capacity_recreates_missing_consumer() {
        let (mut prod, _cons) = new_audio_ring(48_000, 0);
        let mut cons_opt = None;
        let mut ring_capacity = ring_buffer_capacity_samples(48_000, 0);

        assert!(ensure_audio_ring_capacity(
            48_000,
            0,
            &mut prod,
            &mut cons_opt,
            &mut ring_capacity,
        ));

        assert!(cons_opt.is_some());
        assert_eq!(ring_capacity, ring_buffer_capacity_samples(48_000, 0));
    }

    #[test]
    fn ensuring_audio_ring_capacity_recreates_mismatched_ring() {
        let (mut prod, cons) = new_audio_ring(48_000, 0);
        let mut cons_opt = Some(cons);
        let mut ring_capacity = ring_buffer_capacity_samples(48_000, 0);
        write_audio_blocking(&mut prod, &[0.25, -0.5, 0.75], || true);

        assert!(ensure_audio_ring_capacity(
            384_000,
            0,
            &mut prod,
            &mut cons_opt,
            &mut ring_capacity,
        ));

        assert!(cons_opt.is_some());
        assert_eq!(ring_capacity, ring_buffer_capacity_samples(384_000, 0));
        assert_eq!(output_pending_len(None, &prod), 0);
    }

    #[test]
    fn ensuring_audio_ring_capacity_keeps_matching_ring() {
        let (mut prod, cons) = new_audio_ring(96_000, 0);
        let mut cons_opt = Some(cons);
        let mut ring_capacity = ring_buffer_capacity_samples(96_000, 0);

        assert!(!ensure_audio_ring_capacity(
            96_000,
            0,
            &mut prod,
            &mut cons_opt,
            &mut ring_capacity,
        ));

        assert!(cons_opt.is_some());
        assert_eq!(ring_capacity, ring_buffer_capacity_samples(96_000, 0));
    }

    #[test]
    fn explicit_pcm_ring_capacity_grows_for_preroll_and_output_room() {
        let target = pcm_start_preroll_samples(384_000, 1000);
        let capacity = ring_buffer_capacity_samples(384_000, 1000);

        assert!(capacity >= target * 2);
        assert!(capacity >= target + PCM_OUTPUT_ROOM_SAMPLES);
    }
}
