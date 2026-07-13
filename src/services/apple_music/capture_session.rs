//! CPAL capture streams for the Fozmo Capture device and the live playback
//! session that feeds captured PCM into the existing player/DSP engine.
//!
//! Two paths share the metrics plumbing:
//! - the diagnostic stream (format-tolerant, conversion allowed) used to prove
//!   the driver is passing audio, and
//! - the real live path, which requires the exact F32/stereo/driver-rate
//!   configuration and errors instead of coercing formats.

use super::live_source::{
    CaptureProducer, LIVE_CHANNELS, LiveCaptureSource, live_capture_ring, ring_capacity_samples,
};
use crate::audio::player::{Player, TrackTags};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) const LIVE_DISPLAY_NAME: &str = "Apple Music (Live)";

#[derive(Debug, Default)]
pub(super) struct DiagnosticMetrics {
    pub frames_received: AtomicU64,
    pub callbacks_received: AtomicU64,
    pub dropouts: AtomicU64,
    pub ring_overruns: AtomicU64,
    pub rms_l_bits: AtomicU32,
    pub rms_r_bits: AtomicU32,
    pub observed_rate_hz: AtomicU32,
    pub last_callback_unix_ms: AtomicU64,
}

impl DiagnosticMetrics {
    pub(super) fn snapshot(&self) -> DiagnosticMetricsSnapshot {
        DiagnosticMetricsSnapshot {
            frames_received: self.frames_received.load(Ordering::Relaxed),
            callbacks_received: self.callbacks_received.load(Ordering::Relaxed),
            dropouts: self.dropouts.load(Ordering::Relaxed),
            ring_overruns: self.ring_overruns.load(Ordering::Relaxed),
            rms_l: f32::from_bits(self.rms_l_bits.load(Ordering::Relaxed)),
            rms_r: f32::from_bits(self.rms_r_bits.load(Ordering::Relaxed)),
            observed_rate_hz: nonzero_u32(self.observed_rate_hz.load(Ordering::Relaxed)),
            last_callback_unix_ms: nonzero_u64(self.last_callback_unix_ms.load(Ordering::Relaxed)),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct DiagnosticMetricsSnapshot {
    pub frames_received: u64,
    pub callbacks_received: u64,
    pub dropouts: u64,
    pub ring_overruns: u64,
    pub rms_l: f32,
    pub rms_r: f32,
    pub observed_rate_hz: Option<u32>,
    pub last_callback_unix_ms: Option<u64>,
}

/// Holds a capture stream open on a worker thread (cpal streams are not Send).
pub(super) struct CaptureWorker {
    stop_tx: Option<mpsc::Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Drop for CaptureWorker {
    fn drop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn spawn_capture_worker<F>(thread_name: &str, open_stream: F) -> Result<CaptureWorker, String>
where
    F: FnOnce() -> Result<cpal::Stream, String> + Send + 'static,
{
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (stop_tx, stop_rx) = mpsc::channel();
    let worker = thread::Builder::new()
        .name(thread_name.to_string())
        .spawn(move || match open_stream() {
            Ok(_stream) => {
                let _ = ready_tx.send(Ok(()));
                let _ = stop_rx.recv();
            }
            Err(err) => {
                let _ = ready_tx.send(Err(err));
            }
        })
        .map_err(|err| format!("Could not start capture thread: {err}"))?;

    match ready_rx
        .recv()
        .map_err(|_| "Capture thread stopped before opening the stream.".to_string())?
    {
        Ok(()) => Ok(CaptureWorker {
            stop_tx: Some(stop_tx),
            worker: Some(worker),
        }),
        Err(err) => {
            let _ = worker.join();
            Err(err)
        }
    }
}

fn find_input_device(device_name: &str) -> Result<cpal::Device, String> {
    let host = cpal::default_host();
    host.input_devices()
        .map_err(|err| format!("Could not enumerate input devices: {err}"))?
        .find(|device| device.name().is_ok_and(|name| name == device_name))
        .ok_or_else(|| {
            format!(
                "{device_name} is not visible as an input device. Route macOS output to Fozmo Capture after installing the HAL driver."
            )
        })
}

// ---------------------------------------------------------------------------
// Live capture session
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(super) struct LiveSessionParams {
    pub device_name: String,
    pub rate_hz: u32,
    pub buffer_ms: u32,
}

/// A running live capture: CPAL stream on a worker thread plus the player
/// session consuming the live source. Dropping it signals the source to EOF
/// and closes the stream; the player runs its normal end-of-stream path.
pub(super) struct LiveSession {
    shutdown: Arc<AtomicBool>,
    _worker: CaptureWorker,
    rate_hz: u32,
}

impl LiveSession {
    pub(super) fn rate_hz(&self) -> u32 {
        self.rate_hz
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
    }
}

pub(super) fn start_live_session(
    player: &Arc<Player>,
    params: &LiveSessionParams,
    metrics: Arc<DiagnosticMetrics>,
) -> Result<LiveSession, String> {
    let capacity = ring_capacity_samples(params.rate_hz, params.buffer_ms);
    let (producer, consumer) = live_capture_ring(capacity);
    let shutdown = Arc::new(AtomicBool::new(false));

    let device_name = params.device_name.clone();
    let rate_hz = params.rate_hz;
    let worker_metrics = Arc::clone(&metrics);
    let worker = spawn_capture_worker("fozmo-capture-live", move || {
        open_fozmo_capture_stream(&device_name, rate_hz, worker_metrics, producer)
    })?;

    let source = LiveCaptureSource::new(params.rate_hz, consumer, Arc::clone(&shutdown));
    let tags = TrackTags {
        title: Some(LIVE_DISPLAY_NAME.to_string()),
        artist: Some("Apple Music".to_string()),
        sample_rate: Some(params.rate_hz),
        channels: Some(LIVE_CHANNELS),
        bits_per_sample: Some(32),
        ..TrackTags::default()
    };
    let epoch = player.reserve_playback_change();
    let started = player.play_stream_if_epoch(
        epoch,
        Box::new(source),
        Some("wav".to_string()),
        LIVE_DISPLAY_NAME.to_string(),
        None,
        Some(tags),
        Vec::new(),
    );
    if !started {
        shutdown.store(true, Ordering::Release);
        return Err("Playback changed while starting Apple Music capture.".to_string());
    }
    Ok(LiveSession {
        shutdown,
        _worker: worker,
        rate_hz: params.rate_hz,
    })
}

/// Real capture path: the stream must match the driver exactly — F32, stereo,
/// at the driver's current nominal rate. Anything else is an error; silent
/// format conversion would break the bit-transparent contract.
pub(super) fn open_fozmo_capture_stream(
    device_name: &str,
    rate_hz: u32,
    metrics: Arc<DiagnosticMetrics>,
    mut producer: CaptureProducer,
) -> Result<cpal::Stream, String> {
    let device = find_input_device(device_name)?;
    let supported = device
        .supported_input_configs()
        .map_err(|err| format!("Could not read input configurations: {err}"))?
        .find(|config| {
            config.sample_format() == SampleFormat::F32
                && config.channels() == LIVE_CHANNELS
                && config.min_sample_rate().0 <= rate_hz
                && config.max_sample_rate().0 >= rate_hz
        });
    if supported.is_none() {
        return Err(format!(
            "{device_name} does not expose an F32/{LIVE_CHANNELS}ch input configuration at {rate_hz} Hz. The live capture path does not convert formats; check the driver's nominal rate."
        ));
    }
    let config = StreamConfig {
        channels: LIVE_CHANNELS,
        sample_rate: cpal::SampleRate(rate_hz),
        buffer_size: cpal::BufferSize::Default,
    };
    let error_metrics = Arc::clone(&metrics);
    let channels = usize::from(LIVE_CHANNELS);
    let stream = device
        .build_input_stream(
            &config,
            move |data: &[f32], _| {
                let pushed = producer.push_slice(data);
                if pushed < data.len() {
                    // Drop-on-full: the consumer is behind; never block the callback.
                    metrics.ring_overruns.fetch_add(1, Ordering::Relaxed);
                }
                update_metrics(data, channels, rate_hz, &metrics, |sample: f32| sample);
            },
            move |_| {
                error_metrics.dropouts.fetch_add(1, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|err| format!("Could not open live capture stream: {err}"))?;
    stream
        .play()
        .map_err(|err| format!("Could not start live capture stream: {err}"))?;
    Ok(stream)
}

// ---------------------------------------------------------------------------
// Diagnostic capture (format-tolerant; not used for the live audio path)
// ---------------------------------------------------------------------------

pub(super) fn start_fozmo_diagnostic_capture(
    device_name: &str,
) -> Result<(CaptureWorker, Arc<DiagnosticMetrics>), String> {
    let metrics = Arc::new(DiagnosticMetrics::default());
    let thread_metrics = Arc::clone(&metrics);
    let device_name = device_name.to_string();
    let worker = spawn_capture_worker("fozmo-capture-diagnostic", move || {
        open_fozmo_capture_diagnostic_stream(&device_name, thread_metrics)
    })?;
    Ok((worker, metrics))
}

fn open_fozmo_capture_diagnostic_stream(
    device_name: &str,
    metrics: Arc<DiagnosticMetrics>,
) -> Result<cpal::Stream, String> {
    let device = find_input_device(device_name)?;
    let supported = supported_or_default_input_config(&device)?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.config();
    let stream =
        build_input_stream_for_format(&device, &config, sample_format, Arc::clone(&metrics))?;
    stream
        .play()
        .map_err(|err| format!("Could not start diagnostic input stream: {err}"))?;
    Ok(stream)
}

fn supported_or_default_input_config(
    device: &cpal::Device,
) -> Result<cpal::SupportedStreamConfig, String> {
    let mut fallback = None;
    if let Ok(configs) = device.supported_input_configs() {
        for config in configs {
            if fallback.is_none() {
                fallback = Some(config.with_max_sample_rate());
            }
            let min_rate = config.min_sample_rate().0;
            let max_rate = config.max_sample_rate().0;
            if config.sample_format() == SampleFormat::F32
                && config.channels() >= 2
                && min_rate <= 48_000
                && max_rate >= 48_000
            {
                return Ok(config.with_sample_rate(cpal::SampleRate(48_000)));
            }
        }
    }
    fallback
        .or_else(|| device.default_input_config().ok())
        .ok_or_else(|| "Fozmo Capture has no usable input stream configuration.".to_string())
}

fn build_input_stream_for_format(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    metrics: Arc<DiagnosticMetrics>,
) -> Result<cpal::Stream, String> {
    let channels = usize::from(config.channels.max(1));
    let sample_rate = config.sample_rate.0;
    match sample_format {
        SampleFormat::F32 => build_typed_input_stream(
            device,
            config,
            metrics,
            move |sample: f32| sample,
            channels,
            sample_rate,
        ),
        SampleFormat::F64 => build_typed_input_stream(
            device,
            config,
            metrics,
            move |sample: f64| sample as f32,
            channels,
            sample_rate,
        ),
        SampleFormat::I8 => build_typed_input_stream(
            device,
            config,
            metrics,
            move |sample: i8| sample as f32 / 128.0,
            channels,
            sample_rate,
        ),
        SampleFormat::I16 => build_typed_input_stream(
            device,
            config,
            metrics,
            move |sample: i16| sample as f32 / 32768.0,
            channels,
            sample_rate,
        ),
        SampleFormat::I32 => build_typed_input_stream(
            device,
            config,
            metrics,
            move |sample: i32| sample as f32 / 2147483648.0,
            channels,
            sample_rate,
        ),
        SampleFormat::U8 => build_typed_input_stream(
            device,
            config,
            metrics,
            move |sample: u8| (sample as f32 - 128.0) / 128.0,
            channels,
            sample_rate,
        ),
        SampleFormat::U16 => build_typed_input_stream(
            device,
            config,
            metrics,
            move |sample: u16| (sample as f32 - 32768.0) / 32768.0,
            channels,
            sample_rate,
        ),
        SampleFormat::U32 => build_typed_input_stream(
            device,
            config,
            metrics,
            move |sample: u32| (sample as f32 - 2147483648.0) / 2147483648.0,
            channels,
            sample_rate,
        ),
        other => Err(format!(
            "Diagnostic capture does not support {other:?} input samples yet."
        )),
    }
}

fn build_typed_input_stream<T, F>(
    device: &cpal::Device,
    config: &StreamConfig,
    metrics: Arc<DiagnosticMetrics>,
    convert: F,
    channels: usize,
    sample_rate: u32,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + Copy + Send + 'static,
    F: Fn(T) -> f32 + Copy + Send + 'static,
{
    let data_metrics = Arc::clone(&metrics);
    let error_metrics = Arc::clone(&metrics);
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                update_metrics(data, channels, sample_rate, &data_metrics, convert);
            },
            move |_| {
                error_metrics.dropouts.fetch_add(1, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|err| format!("Could not open diagnostic input stream: {err}"))
}

fn update_metrics<T, F>(
    data: &[T],
    channels: usize,
    sample_rate: u32,
    metrics: &DiagnosticMetrics,
    convert: F,
) where
    T: Copy,
    F: Fn(T) -> f32,
{
    if channels == 0 {
        return;
    }
    let frames = data.len() / channels;
    if frames == 0 {
        return;
    }

    let mut left_sum = 0.0_f64;
    let mut right_sum = 0.0_f64;
    for frame in 0..frames {
        let base = frame * channels;
        let left = convert(data[base]);
        let right = if channels > 1 {
            convert(data[base + 1])
        } else {
            left
        };
        left_sum += f64::from(left * left);
        right_sum += f64::from(right * right);
    }

    let rms_l = (left_sum / frames as f64).sqrt() as f32;
    let rms_r = (right_sum / frames as f64).sqrt() as f32;
    metrics
        .frames_received
        .fetch_add(frames as u64, Ordering::Relaxed);
    metrics.callbacks_received.fetch_add(1, Ordering::Relaxed);
    metrics.rms_l_bits.store(rms_l.to_bits(), Ordering::Relaxed);
    metrics.rms_r_bits.store(rms_r.to_bits(), Ordering::Relaxed);
    metrics
        .observed_rate_hz
        .store(sample_rate, Ordering::Relaxed);
    metrics
        .last_callback_unix_ms
        .store(now_unix_ms(), Ordering::Relaxed);
}

pub(super) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn nonzero_u32(value: u32) -> Option<u32> {
    (value != 0).then_some(value)
}

fn nonzero_u64(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

// Session control state shared between the service, the Music poller, and
// status snapshots.
#[derive(Debug, Default)]
pub(super) struct SessionControl {
    /// 0 = unknown / not playing.
    pub detected_track_rate_hz: AtomicU32,
    pub rate_switch_pending: AtomicBool,
    /// u32::MAX = unknown.
    pub music_sound_volume: AtomicU32,
    pub last_poll_error: Mutex<Option<String>>,
}

impl SessionControl {
    pub(super) fn new_unknown() -> Self {
        let control = Self::default();
        control
            .music_sound_volume
            .store(u32::MAX, Ordering::Relaxed);
        control
    }

    pub(super) fn detected_rate(&self) -> Option<u32> {
        nonzero_u32(self.detected_track_rate_hz.load(Ordering::Relaxed))
    }

    pub(super) fn music_volume(&self) -> Option<u32> {
        let value = self.music_sound_volume.load(Ordering::Relaxed);
        (value != u32::MAX).then_some(value)
    }

    pub(super) fn rate_switch_pending(&self) -> bool {
        self.rate_switch_pending.load(Ordering::Relaxed)
    }

    pub(super) fn set_rate_switch_pending(&self, pending: bool) {
        self.rate_switch_pending.store(pending, Ordering::Relaxed);
    }

    pub(super) fn poll_error(&self) -> Option<String> {
        self.last_poll_error.lock().unwrap().clone()
    }

    pub(super) fn set_poll_error(&self, error: Option<String>) {
        *self.last_poll_error.lock().unwrap() = error;
    }

    pub(super) fn observe_track_info(
        &self,
        track_rate_hz: Option<u32>,
        sound_volume: Option<u32>,
        _playing: bool,
    ) {
        self.detected_track_rate_hz
            .store(track_rate_hz.unwrap_or(0), Ordering::Relaxed);
        self.music_sound_volume
            .store(sound_volume.unwrap_or(u32::MAX), Ordering::Relaxed);
        self.set_poll_error(None);
    }

    pub(super) fn observe_music_gone(&self) {
        self.detected_track_rate_hz.store(0, Ordering::Relaxed);
        self.music_sound_volume.store(u32::MAX, Ordering::Relaxed);
    }
}
