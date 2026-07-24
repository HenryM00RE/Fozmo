//! Live Symphonia `MediaSource` for Apple Music capture.
//!
//! The capture callback produces interleaved F32 frames into a lock-free SPSC
//! ring; this source serves a WAV (IEEE float) header followed by the ring
//! contents as an effectively unbounded, non-seekable stream. On shutdown the
//! reader returns EOF so the player's normal end-of-stream path runs.

use ringbuf::{Consumer, HeapRb, Producer, SharedRb};
use std::io::{Read, Seek, SeekFrom};
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use symphonia::core::io::MediaSource;

pub(super) type CaptureProducer = Producer<f32, Arc<SharedRb<f32, Vec<MaybeUninit<f32>>>>>;
pub(super) type CaptureConsumer = Consumer<f32, Arc<SharedRb<f32, Vec<MaybeUninit<f32>>>>>;

pub(super) const LIVE_CHANNELS: u16 = 2;
const BYTES_PER_SAMPLE: usize = 4;
pub(super) const LIVE_SAMPLE_CONTAINER_BITS: u32 = (BYTES_PER_SAMPLE * 8) as u32;
pub(super) const LIVE_SAMPLE_PRECISION_BITS: u32 = f32::MANTISSA_DIGITS;
const STAGE_SAMPLES: usize = 4096;
const EMPTY_RING_POLL: Duration = Duration::from_millis(2);

#[derive(Default)]
pub(super) struct CaptureFlow {
    enqueued_frames: AtomicU64,
    consumed_frames: AtomicU64,
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
}

/// Ring capacity in samples for a given rate and buffer size, with a floor so
/// tiny settings cannot starve the capture callback.
pub(super) fn ring_capacity_samples(rate_hz: u32, buffer_ms: u32) -> usize {
    let samples = (rate_hz as u64 * u64::from(LIVE_CHANNELS) * u64::from(buffer_ms)).div_ceil(1000);
    (samples as usize).max(STAGE_SAMPLES * 2)
}

pub(super) fn live_capture_ring(capacity_samples: usize) -> (CaptureProducer, CaptureConsumer) {
    HeapRb::<f32>::new(capacity_samples).split()
}

pub(super) fn discard_capture_buffer(consumer: &mut CaptureConsumer, flow: &CaptureFlow) -> u64 {
    let mut discarded_samples = 0_usize;
    while consumer.pop().is_some() {
        discarded_samples += 1;
    }
    flow.record_consumed_samples(discarded_samples);
    (discarded_samples / usize::from(LIVE_CHANNELS)) as u64
}

/// WAV header for a stereo IEEE-float stream with an effectively unbounded
/// data chunk. Sizes use the streaming-WAV convention of `u32::MAX`-based
/// lengths, which Symphonia accepts for non-seekable input (verified by the
/// probe tests below).
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
    /// Bytes converted from staged samples but not yet handed to the reader
    /// (Read calls are not always multiples of the sample size).
    pending: Vec<u8>,
    pending_pos: usize,
    flow: Arc<CaptureFlow>,
}

impl LiveCaptureSource {
    // The legacy capture backend uses this constructor; the process-tap build
    // supplies shared flow accounting through `new_with_flow`.
    #[allow(dead_code)]
    pub(super) fn new(rate_hz: u32, consumer: CaptureConsumer, shutdown: Arc<AtomicBool>) -> Self {
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
                continue;
            }
            if self.shutdown.load(Ordering::Acquire) {
                // Unblock the engine: EOF runs the normal tail/stop path.
                return Ok(0);
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
            .map(|i| (i as f32) / 100_000.0)
            .collect()
    }

    // The spike from issue #65: Symphonia's WAV reader must accept a
    // non-seekable source with an effectively unbounded data chunk. If this
    // test ever fails after a symphonia upgrade, the fallback plan is a
    // minimal custom FormatReader emitting CODEC_TYPE_PCM_F32LE packets.
    #[test]
    fn symphonia_probes_unbounded_nonseekable_live_wav() {
        let rate_hz = 96_000;
        let samples = ramp(4096);
        let (mut producer, consumer) = live_capture_ring(samples.len() * 2);
        assert_eq!(producer.push_slice(&samples), samples.len());
        let shutdown = Arc::new(AtomicBool::new(true));
        let source = LiveCaptureSource::new(rate_hz, consumer, Arc::clone(&shutdown));

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
        assert_eq!(
            track.codec_params.channels.map(|channels| channels.count()),
            Some(usize::from(LIVE_CHANNELS))
        );

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
        assert_eq!(decoded_samples.len(), samples.len());
        assert_eq!(decoded_samples, samples);
    }

    #[test]
    fn shutdown_with_drained_ring_reaches_eof() {
        let (mut producer, consumer) = live_capture_ring(1024);
        let samples = ramp(8);
        producer.push_slice(&samples);
        let shutdown = Arc::new(AtomicBool::new(true));
        let mut source = LiveCaptureSource::new(44_100, consumer, shutdown);

        let mut everything = Vec::new();
        let mut buf = [0u8; 97]; // odd size to exercise partial-sample handling
        loop {
            let n = source.read(&mut buf).expect("live read");
            if n == 0 {
                break;
            }
            everything.extend_from_slice(&buf[..n]);
        }
        assert_eq!(everything.len(), 44 + samples.len() * BYTES_PER_SAMPLE);
        let payload: Vec<f32> = everything[44..]
            .chunks_exact(BYTES_PER_SAMPLE)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();
        assert_eq!(payload, samples);
    }

    #[test]
    fn ring_capacity_scales_with_rate_and_buffer() {
        assert_eq!(ring_capacity_samples(44_100, 1000), 88_200);
        assert_eq!(ring_capacity_samples(192_000, 250), 96_000);
        // Floor keeps tiny buffers usable.
        assert_eq!(ring_capacity_samples(44_100, 1), STAGE_SAMPLES * 2);
    }

    #[test]
    fn capture_flow_tracks_and_discards_only_complete_stereo_frames() {
        let (mut producer, mut consumer) = live_capture_ring(64);
        let flow = CaptureFlow::default();
        let samples = ramp(12);
        let pushed = producer.push_slice(&samples);
        flow.record_enqueued(pushed / usize::from(LIVE_CHANNELS));

        assert_eq!(flow.buffered_frames(), 12);
        assert_eq!(discard_capture_buffer(&mut consumer, &flow), 12);
        assert_eq!(flow.buffered_frames(), 0);
        assert_eq!(consumer.len(), 0);
    }

    #[test]
    fn float32_capture_exactly_preserves_integer_pcm_through_24_bits() {
        for bits in [16_u32, 24] {
            let scale = 1_i32 << (bits - 1);
            let samples = [
                -scale,
                -scale + 1,
                -1,
                0,
                1,
                scale / 3,
                scale - 2,
                scale - 1,
            ];

            for sample in samples {
                let captured = sample as f32 / scale as f32;
                let widened_for_dsp = f64::from(captured);
                let recovered = (widened_for_dsp * f64::from(scale)).round() as i32;
                assert_eq!(
                    recovered, sample,
                    "{bits}-bit PCM value {sample} did not round-trip through F32"
                );
            }
        }
    }
}
