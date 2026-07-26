//! Non-seekable WAV `MediaSource` backed by Fozmo Capture's live PCM ring.

use ringbuf::{Consumer, HeapRb, Producer, SharedRb};
use std::io::{Read, Seek, SeekFrom};
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use symphonia::core::io::MediaSource;

pub(super) type CaptureProducer = Producer<f32, Arc<SharedRb<f32, Vec<MaybeUninit<f32>>>>>;
pub(super) type CaptureConsumer = Consumer<f32, Arc<SharedRb<f32, Vec<MaybeUninit<f32>>>>>;

pub(super) const LIVE_CHANNELS: u16 = 2;
const BYTES_PER_SAMPLE: usize = 4;
const STAGE_SAMPLES: usize = 4096;
const EMPTY_RING_POLL: Duration = Duration::from_millis(2);
const EMPTY_RING_UNDERRUN_THRESHOLD: Duration = Duration::from_millis(25);

pub(super) struct CaptureFlow {
    enqueued_frames: AtomicU64,
    consumed_frames: AtomicU64,
    capture_gate_open: AtomicBool,
    dropped_frames: AtomicU64,
    underrun_count: AtomicU64,
}

impl Default for CaptureFlow {
    fn default() -> Self {
        Self {
            enqueued_frames: AtomicU64::new(0),
            consumed_frames: AtomicU64::new(0),
            capture_gate_open: AtomicBool::new(true),
            dropped_frames: AtomicU64::new(0),
            underrun_count: AtomicU64::new(0),
        }
    }
}

impl CaptureFlow {
    pub(super) fn record_enqueued(&self, frames: usize) {
        self.enqueued_frames
            .fetch_add(frames as u64, Ordering::Relaxed);
    }

    fn record_consumed_samples(&self, samples: usize) {
        self.consumed_frames.fetch_add(
            (samples / usize::from(LIVE_CHANNELS)) as u64,
            Ordering::Relaxed,
        );
    }

    pub(super) fn buffered_frames(&self) -> u64 {
        self.enqueued_frames
            .load(Ordering::Relaxed)
            .saturating_sub(self.consumed_frames.load(Ordering::Relaxed))
    }

    pub(super) fn capture_gate_open(&self) -> bool {
        self.capture_gate_open.load(Ordering::Acquire)
    }

    pub(super) fn set_capture_gate_open(&self, open: bool) {
        self.capture_gate_open.store(open, Ordering::Release);
    }

    pub(super) fn record_dropped(&self, frames: usize) {
        self.dropped_frames
            .fetch_add(frames as u64, Ordering::Relaxed);
    }

    pub(super) fn dropped_frames(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }

    fn record_underrun(&self) {
        self.underrun_count.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn underrun_count(&self) -> u64 {
        self.underrun_count.load(Ordering::Relaxed)
    }
}

pub(super) fn ring_capacity_samples(rate_hz: u32, buffer_ms: u32) -> usize {
    let samples = (rate_hz as u64 * u64::from(LIVE_CHANNELS) * u64::from(buffer_ms)).div_ceil(1000);
    (samples as usize).max(STAGE_SAMPLES * 2)
}

pub(super) fn live_capture_ring(capacity_samples: usize) -> (CaptureProducer, CaptureConsumer) {
    HeapRb::<f32>::new(capacity_samples).split()
}

/// WAV header for a stereo IEEE-float stream with an effectively unbounded
/// data chunk.
pub(super) fn wav_header_ieee_f32(rate_hz: u32) -> Vec<u8> {
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
    let channels = u32::from(LIVE_CHANNELS);
    let byte_rate = rate_hz * channels * BYTES_PER_SAMPLE as u32;
    let block_align = (channels * BYTES_PER_SAMPLE as u32) as u16;

    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(u32::MAX - 8).to_le_bytes());
    header.extend_from_slice(b"WAVE");
    header.extend_from_slice(b"fmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&WAVE_FORMAT_IEEE_FLOAT.to_le_bytes());
    header.extend_from_slice(&LIVE_CHANNELS.to_le_bytes());
    header.extend_from_slice(&rate_hz.to_le_bytes());
    header.extend_from_slice(&byte_rate.to_le_bytes());
    header.extend_from_slice(&block_align.to_le_bytes());
    header.extend_from_slice(&(BYTES_PER_SAMPLE as u16 * 8).to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&(u32::MAX - 44).to_le_bytes());
    header
}

pub(super) struct LiveCaptureSource {
    header: Vec<u8>,
    header_pos: usize,
    consumer: CaptureConsumer,
    shutdown: Arc<AtomicBool>,
    stage: Vec<f32>,
    pending: Vec<u8>,
    pending_pos: usize,
    flow: Arc<CaptureFlow>,
    received_audio: bool,
    empty_since: Option<Instant>,
    underrun_reported: bool,
}

impl LiveCaptureSource {
    #[cfg(test)]
    fn new(rate_hz: u32, consumer: CaptureConsumer, shutdown: Arc<AtomicBool>) -> Self {
        Self::new_with_flow(
            rate_hz,
            consumer,
            shutdown,
            Arc::new(CaptureFlow::default()),
        )
    }

    pub(super) fn new_with_flow(
        rate_hz: u32,
        consumer: CaptureConsumer,
        shutdown: Arc<AtomicBool>,
        flow: Arc<CaptureFlow>,
    ) -> Self {
        Self {
            header: wav_header_ieee_f32(rate_hz),
            header_pos: 0,
            consumer,
            shutdown,
            stage: vec![0.0; STAGE_SAMPLES],
            pending: Vec::new(),
            pending_pos: 0,
            flow,
            received_audio: false,
            empty_since: None,
            underrun_reported: false,
        }
    }

    fn drain_pending(&mut self, buf: &mut [u8]) -> usize {
        let available = self.pending.len() - self.pending_pos;
        if available == 0 {
            return 0;
        }
        let count = available.min(buf.len());
        buf[..count].copy_from_slice(&self.pending[self.pending_pos..self.pending_pos + count]);
        self.pending_pos += count;
        if self.pending_pos == self.pending.len() {
            self.pending.clear();
            self.pending_pos = 0;
        }
        count
    }

    fn stage_from_ring(&mut self) -> usize {
        let popped = self.consumer.pop_slice(&mut self.stage);
        if popped > 0 {
            self.flow.record_consumed_samples(popped);
            self.pending.reserve(popped * BYTES_PER_SAMPLE);
            for sample in &self.stage[..popped] {
                self.pending.extend_from_slice(&sample.to_le_bytes());
            }
        }
        popped
    }
}

impl Read for LiveCaptureSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.header_pos < self.header.len() {
            let count = (self.header.len() - self.header_pos).min(buf.len());
            buf[..count].copy_from_slice(&self.header[self.header_pos..self.header_pos + count]);
            self.header_pos += count;
            return Ok(count);
        }
        loop {
            let drained = self.drain_pending(buf);
            if drained > 0 {
                return Ok(drained);
            }
            if self.stage_from_ring() > 0 {
                self.received_audio = true;
                self.empty_since = None;
                self.underrun_reported = false;
                continue;
            }
            if self.shutdown.load(Ordering::Acquire) {
                return Ok(0);
            }
            if self.received_audio && self.empty_since.is_none() {
                self.empty_since = Some(Instant::now());
            }
            if !self.underrun_reported
                && self
                    .empty_since
                    .is_some_and(|started| started.elapsed() >= EMPTY_RING_UNDERRUN_THRESHOLD)
            {
                self.underrun_reported = true;
                self.flow.record_underrun();
                tracing::warn!(
                    event = "apple_music_capture_ring_underrun",
                    capture_gate_open = self.flow.capture_gate_open(),
                    dropped_frames = self.flow.dropped_frames(),
                    "Apple Music capture lead drained; live input is stalling until PCM resumes"
                );
            }
            std::thread::sleep(EMPTY_RING_POLL);
        }
    }
}

impl Seek for LiveCaptureSource {
    fn seek(&mut self, _pos: SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "live capture stream is not seekable",
        ))
    }
}

impl MediaSource for LiveCaptureSource {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symphonia::core::codecs::{CODEC_TYPE_PCM_F32LE, DecoderOptions};
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    fn ramp(frames: usize) -> Vec<f32> {
        (0..frames * usize::from(LIVE_CHANNELS))
            .map(|index| (index as f32) / 100_000.0)
            .collect()
    }

    #[test]
    fn symphonia_probes_unbounded_nonseekable_live_wav() {
        let rate_hz = 96_000;
        let samples = ramp(4096);
        let (mut producer, consumer) = live_capture_ring(samples.len() * 2);
        assert_eq!(producer.push_slice(&samples), samples.len());
        let source = LiveCaptureSource::new(rate_hz, consumer, Arc::new(AtomicBool::new(true)));

        let stream = MediaSourceStream::new(Box::new(source), Default::default());
        let mut hint = Hint::new();
        hint.with_extension("wav");
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                stream,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .expect("symphonia should probe the live WAV header");
        let mut format = probed.format;
        let track = format.default_track().expect("live WAV track").clone();
        assert_eq!(track.codec_params.codec, CODEC_TYPE_PCM_F32LE);
        assert_eq!(track.codec_params.sample_rate, Some(rate_hz));

        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .expect("decoder for live WAV");
        let mut decoded_samples = Vec::new();
        while decoded_samples.len() < samples.len() {
            let packet = match format.next_packet() {
                Ok(packet) => packet,
                Err(_) => break,
            };
            let decoded = decoder.decode(&packet).expect("decode live packet");
            let mut buffer = decoded.make_equivalent::<f32>();
            decoded.convert(&mut buffer);
            let planes = buffer.planes();
            let channel_planes = planes.planes();
            let frames = channel_planes.first().map_or(0, |plane| plane.len());
            for frame in 0..frames {
                for plane in channel_planes {
                    decoded_samples.push(plane[frame]);
                }
            }
        }
        assert_eq!(decoded_samples, samples);
    }

    #[test]
    fn shutdown_with_drained_ring_reaches_eof() {
        let (mut producer, consumer) = live_capture_ring(1024);
        let samples = ramp(8);
        producer.push_slice(&samples);
        let mut source = LiveCaptureSource::new(44_100, consumer, Arc::new(AtomicBool::new(true)));

        let mut everything = Vec::new();
        source.read_to_end(&mut everything).expect("live read");
        assert_eq!(everything.len(), 44 + samples.len() * BYTES_PER_SAMPLE);
    }

    #[test]
    fn ring_capacity_scales_with_rate_and_buffer() {
        assert_eq!(ring_capacity_samples(44_100, 1000), 88_200);
        assert_eq!(ring_capacity_samples(192_000, 250), 96_000);
        assert_eq!(ring_capacity_samples(44_100, 1), STAGE_SAMPLES * 2);
    }

    #[test]
    fn float32_capture_exactly_preserves_integer_pcm_through_24_bits() {
        for bits in [16_u32, 24] {
            let scale = 1_i32 << (bits - 1);
            for sample in [
                -scale,
                -scale + 1,
                -1,
                0,
                1,
                scale / 3,
                scale - 2,
                scale - 1,
            ] {
                let captured = sample as f32 / scale as f32;
                let recovered = (f64::from(captured) * f64::from(scale)).round() as i32;
                assert_eq!(recovered, sample);
            }
        }
    }

    #[test]
    fn capture_flow_gate_starts_open_and_tracks_drops_and_underruns() {
        let flow = CaptureFlow::default();
        assert!(flow.capture_gate_open());
        flow.set_capture_gate_open(false);
        assert!(!flow.capture_gate_open());
        flow.record_dropped(192);
        flow.record_underrun();
        assert_eq!(flow.dropped_frames(), 192);
        assert_eq!(flow.underrun_count(), 1);
    }
}
