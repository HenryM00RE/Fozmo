//! CPAL capture stream and live Player session for the Fozmo Capture device.

use super::live_source::{
    CaptureFlow, CaptureProducer, LIVE_CHANNELS, LiveCaptureSource, live_capture_ring,
    ring_capacity_samples,
};
use crate::audio::player::{Player, TrackTags};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;

pub(super) const LIVE_DISPLAY_NAME: &str = "Apple Music (Live)";

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
    /// Original decoded source precision, when Apple exposes it. The CoreAudio
    /// transport remains F32; this value is metadata for the DSP/UI.
    pub source_bit_depth: Option<u32>,
}

/// A running live capture: CPAL stream on a worker thread plus the player
/// session consuming the live source. Dropping it signals the source to EOF
/// and closes the stream; the player runs its normal end-of-stream path.
pub(super) struct LiveSession {
    shutdown: Arc<AtomicBool>,
    _worker: CaptureWorker,
    rate_hz: u32,
    flow: Arc<CaptureFlow>,
    player_epoch: u64,
}

impl LiveSession {
    pub(super) fn player_epoch(&self) -> u64 {
        self.player_epoch
    }

    pub(super) fn buffered_audio_secs(&self) -> f64 {
        self.flow.buffered_frames() as f64 / f64::from(self.rate_hz.max(1))
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
    start_paused: bool,
) -> Result<LiveSession, String> {
    let capacity = ring_capacity_samples(params.rate_hz, params.buffer_ms);
    let (producer, consumer) = live_capture_ring(capacity);
    let shutdown = Arc::new(AtomicBool::new(false));
    let flow = Arc::new(CaptureFlow::default());

    let device_name = params.device_name.clone();
    let rate_hz = params.rate_hz;
    let worker_flow = Arc::clone(&flow);
    let worker = spawn_capture_worker("fozmo-capture-live", move || {
        open_fozmo_capture_stream(&device_name, rate_hz, producer, worker_flow)
    })?;

    let source = LiveCaptureSource::new_with_flow(
        params.rate_hz,
        consumer,
        Arc::clone(&shutdown),
        Arc::clone(&flow),
    );
    let source_bit_depth = source_bit_depth_for_tags(params.source_bit_depth);
    let tags = TrackTags {
        title: Some(LIVE_DISPLAY_NAME.to_string()),
        artist: Some("Apple Music".to_string()),
        sample_rate: Some(params.rate_hz),
        channels: Some(LIVE_CHANNELS),
        bits_per_sample: Some(source_bit_depth),
        ..TrackTags::default()
    };
    let epoch = player.reserve_playback_change();
    let started = if start_paused {
        player.play_stream_paused_if_epoch(
            epoch,
            Box::new(source),
            Some("wav".to_string()),
            LIVE_DISPLAY_NAME.to_string(),
            None,
            Some(tags),
            Vec::new(),
        )
    } else {
        player.play_stream_if_epoch(
            epoch,
            Box::new(source),
            Some("wav".to_string()),
            LIVE_DISPLAY_NAME.to_string(),
            None,
            Some(tags),
            Vec::new(),
        )
    };
    if !started {
        shutdown.store(true, Ordering::Release);
        return Err("Playback changed while starting Apple Music capture.".to_string());
    }
    Ok(LiveSession {
        shutdown,
        _worker: worker,
        rate_hz: params.rate_hz,
        flow,
        player_epoch: player.playback_epoch(),
    })
}

fn source_bit_depth_for_tags(source_bit_depth: Option<u32>) -> u32 {
    source_bit_depth
        .filter(|bits| matches!(bits, 16 | 24 | 32))
        .unwrap_or(32)
}

/// Real capture path: the stream must match the driver exactly — F32, stereo,
/// at the driver's current nominal rate. Anything else is an error; silent
/// format conversion would break the bit-transparent contract.
pub(super) fn open_fozmo_capture_stream(
    device_name: &str,
    rate_hz: u32,
    mut producer: CaptureProducer,
    flow: Arc<CaptureFlow>,
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
    let channels = usize::from(LIVE_CHANNELS);
    let stream = device
        .build_input_stream(
            &config,
            move |data: &[f32], _| {
                let pushed = producer.push_slice(data);
                flow.record_enqueued(pushed / channels);
            },
            move |error| {
                tracing::warn!(
                    event = "apple_music_playback_stream_error",
                    %error,
                    "Fozmo Capture input stream reported an error"
                );
            },
            None,
        )
        .map_err(|err| format!("Could not open live capture stream: {err}"))?;
    stream
        .play()
        .map_err(|err| format!("Could not start live capture stream: {err}"))?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::source_bit_depth_for_tags;

    #[test]
    fn live_source_reports_detected_precision_not_float_container_width() {
        assert_eq!(source_bit_depth_for_tags(Some(24)), 24);
        assert_eq!(source_bit_depth_for_tags(Some(16)), 16);
        assert_eq!(source_bit_depth_for_tags(None), 32);
        assert_eq!(source_bit_depth_for_tags(Some(20)), 32);
    }
}
