//! Ogg Opus encoding of local library tracks for browser playback.
//!
//! This is a playback derivative pipeline, not an export path: it decodes a
//! source file through the same Symphonia stack the player uses, bridges to
//! 48 kHz stereo with the existing sinc resampler, and encodes RFC 7845 Ogg
//! Opus suitable for `<audio>` elements (including Safari 17+ on iOS/macOS).

use super::decode::decode_stereo_blocks;
use crate::audio::dsp::resampler::{FilterType, SincResampler};
use crate::audio::eq::EqConfig;
use ogg::{PacketWriteEndInfo, PacketWriter};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Opus only accepts 8/12/16/24/48 kHz input; everything is bridged to 48 kHz.
pub const OPUS_SAMPLE_RATE: u32 = 48_000;
/// 20 ms at 48 kHz, the container's default and most compatible frame size.
const OPUS_FRAME_LEN: usize = 960;
/// Recommended maximum packet buffer from the libopus documentation.
const MAX_OPUS_PACKET_BYTES: usize = 4000;
/// Force an Ogg page boundary at least every second of audio so browsers can
/// start playback while the derivative is still being generated.
const PACKETS_PER_PAGE: usize = 50;
/// Browser-selectable stream bitrates; requests outside this set are
/// rejected so arbitrary values cannot fragment the derivative cache.
pub const ALLOWED_BITRATE_KBPS: [u32; 3] = [128, 256, 320];
pub const DEFAULT_BITRATE_KBPS: u32 = 256;
pub const MIN_BITRATE_KBPS: u32 = 48;
pub const MAX_BITRATE_KBPS: u32 = 320;

/// Server-wide default Opus bitrate: `FOZMO_OPUS_STREAM_KBPS` when it names
/// one of [`ALLOWED_BITRATE_KBPS`], otherwise [`DEFAULT_BITRATE_KBPS`].
pub fn configured_bitrate_kbps() -> u32 {
    std::env::var(crate::app::identity::env_key("OPUS_STREAM_KBPS"))
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|kbps| ALLOWED_BITRATE_KBPS.contains(kbps))
        .unwrap_or(DEFAULT_BITRATE_KBPS)
}

/// Encode `source` to Ogg Opus, writing container bytes to `out` as they are
/// produced. EQ (when given) is applied at the source rate before the 48 kHz
/// bridge. `cancel` is polled between packets so an abandoned job can stop
/// without finishing the track.
pub fn encode_ogg_opus(
    source: &Path,
    out: &mut dyn Write,
    bitrate_kbps: u32,
    eq: Option<&EqConfig>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), String> {
    let mut encoder = OggOpusStream::new(out, bitrate_kbps)?;
    let mut resampler: Option<SincResampler> = None;
    let mut resampled = Vec::new();

    decode_stereo_blocks(source, eq, cancel, |left, right, source_rate| {
        if source_rate == OPUS_SAMPLE_RATE {
            encoder.push_planar(left, right)
        } else {
            let bridge = resampler.get_or_insert_with(|| {
                SincResampler::new(FilterType::Minimum16k, source_rate, OPUS_SAMPLE_RATE)
            });
            bridge.input(left, right);
            resampled.clear();
            bridge.process(&mut resampled);
            encoder.push_interleaved(&resampled)
        }
    })?;

    if let Some(bridge) = resampler.as_mut() {
        resampled.clear();
        bridge.drain_eof(&mut resampled);
        encoder.push_interleaved(&resampled)?;
    }
    encoder.finish()
}

/// Incremental RFC 7845 Ogg Opus stream: identification + comment headers up
/// front, then fixed 20 ms packets with granule positions that let players
/// trim both the encoder pre-skip and the zero padding of the final frame.
struct OggOpusStream<'w> {
    writer: PacketWriter<'w, &'w mut dyn Write>,
    encoder: *mut unsafe_libopus::OpusEncoder,
    serial: u32,
    pre_skip: u16,
    /// Interleaved stereo f32 samples not yet forming a whole Opus frame.
    pending: Vec<f32>,
    /// Total 48 kHz frames accepted from the source (excludes padding).
    input_frames: u64,
    /// Total 48 kHz frames pushed into the encoder (includes padding).
    encoded_frames: u64,
    packets_in_page: usize,
}

impl<'w> OggOpusStream<'w> {
    fn new(out: &'w mut dyn Write, bitrate_kbps: u32) -> Result<Self, String> {
        let mut error = 0_i32;
        let encoder = unsafe {
            unsafe_libopus::opus_encoder_create(
                OPUS_SAMPLE_RATE as i32,
                2,
                unsafe_libopus::OPUS_APPLICATION_AUDIO,
                &mut error,
            )
        };
        if error != unsafe_libopus::OPUS_OK || encoder.is_null() {
            return Err(format!("create opus encoder: error {error}"));
        }
        let bitrate_bps = (bitrate_kbps.clamp(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS) * 1000) as i32;
        let mut lookahead = 0_i32;
        let (bitrate_ret, lookahead_ret) = unsafe {
            (
                unsafe_libopus::opus_encoder_ctl!(
                    encoder,
                    unsafe_libopus::OPUS_SET_BITRATE_REQUEST,
                    bitrate_bps
                ),
                unsafe_libopus::opus_encoder_ctl!(
                    encoder,
                    unsafe_libopus::OPUS_GET_LOOKAHEAD_REQUEST,
                    &mut lookahead
                ),
            )
        };
        if bitrate_ret != unsafe_libopus::OPUS_OK || lookahead_ret != unsafe_libopus::OPUS_OK {
            unsafe { unsafe_libopus::opus_encoder_destroy(encoder) };
            return Err("configure opus encoder failed".to_string());
        }
        let mut stream = Self {
            writer: PacketWriter::new(out),
            encoder,
            serial: rand::random::<u32>(),
            pre_skip: lookahead.clamp(0, u16::MAX as i32) as u16,
            pending: Vec::with_capacity(OPUS_FRAME_LEN * 2),
            input_frames: 0,
            encoded_frames: 0,
            packets_in_page: 0,
        };
        stream.write_headers()?;
        Ok(stream)
    }

    fn write_headers(&mut self) -> Result<(), String> {
        let mut head = Vec::with_capacity(19);
        head.extend_from_slice(b"OpusHead");
        head.push(1); // version
        head.push(2); // channel count
        head.extend_from_slice(&self.pre_skip.to_le_bytes());
        head.extend_from_slice(&OPUS_SAMPLE_RATE.to_le_bytes());
        head.extend_from_slice(&0_u16.to_le_bytes()); // output gain
        head.push(0); // mapping family: stereo
        self.write_packet(head, PacketWriteEndInfo::EndPage, 0)?;

        let vendor = b"fozmo";
        let mut tags = Vec::with_capacity(8 + 4 + vendor.len() + 4);
        tags.extend_from_slice(b"OpusTags");
        tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        tags.extend_from_slice(vendor);
        tags.extend_from_slice(&0_u32.to_le_bytes()); // no user comments
        self.write_packet(tags, PacketWriteEndInfo::EndPage, 0)
    }

    fn write_packet(
        &mut self,
        data: Vec<u8>,
        end: PacketWriteEndInfo,
        granule: u64,
    ) -> Result<(), String> {
        self.writer
            .write_packet(data, self.serial, end, granule)
            .map_err(|e| format!("write ogg packet: {e}"))
    }

    fn push_planar(&mut self, left: &[f64], right: &[f64]) -> Result<(), String> {
        let frames = left.len().min(right.len());
        self.input_frames += frames as u64;
        self.pending.reserve(frames * 2);
        for i in 0..frames {
            self.pending.push(left[i] as f32);
            self.pending.push(right[i] as f32);
        }
        self.encode_ready_frames()
    }

    fn push_interleaved(&mut self, samples: &[f64]) -> Result<(), String> {
        let samples = &samples[..samples.len() - samples.len() % 2];
        self.input_frames += (samples.len() / 2) as u64;
        self.pending.extend(samples.iter().map(|s| *s as f32));
        self.encode_ready_frames()
    }

    /// Encode whole frames but always hold at least one frame's worth back:
    /// the stream needs a genuine final packet to carry the end-trim granule.
    fn encode_ready_frames(&mut self) -> Result<(), String> {
        while self.pending.len() > OPUS_FRAME_LEN * 2 {
            let rest = self.pending.split_off(OPUS_FRAME_LEN * 2);
            let frame = std::mem::replace(&mut self.pending, rest);
            self.encode_frame(&frame, false)?;
        }
        Ok(())
    }

    fn encode_frame(&mut self, frame: &[f32], last: bool) -> Result<(), String> {
        debug_assert_eq!(frame.len(), OPUS_FRAME_LEN * 2);
        let mut packet = vec![0_u8; MAX_OPUS_PACKET_BYTES];
        let written = unsafe {
            unsafe_libopus::opus_encode_float(
                self.encoder,
                frame.as_ptr(),
                OPUS_FRAME_LEN as i32,
                packet.as_mut_ptr(),
                packet.len() as i32,
            )
        };
        if written < 0 {
            return Err(format!("opus encode failed: error {written}"));
        }
        packet.truncate(written as usize);
        self.encoded_frames += OPUS_FRAME_LEN as u64;
        // Granule positions count 48 kHz samples including pre-skip; the final
        // packet's position reflects only real input so players trim padding.
        let granule = if last {
            u64::from(self.pre_skip) + self.input_frames
        } else {
            u64::from(self.pre_skip) + self.encoded_frames
        };
        self.packets_in_page += 1;
        let end = if last {
            PacketWriteEndInfo::EndStream
        } else if self.packets_in_page >= PACKETS_PER_PAGE {
            self.packets_in_page = 0;
            PacketWriteEndInfo::EndPage
        } else {
            PacketWriteEndInfo::NormalPacket
        };
        self.write_packet(packet, end, granule)
    }

    fn finish(mut self) -> Result<(), String> {
        // Pad the tail to a whole frame; granule math trims it on playback.
        // An empty source still emits one silent frame so the stream is valid.
        let mut frame = std::mem::take(&mut self.pending);
        frame.resize(OPUS_FRAME_LEN * 2, 0.0);
        self.encode_frame(&frame, true)
    }
}

impl Drop for OggOpusStream<'_> {
    fn drop(&mut self) {
        unsafe { unsafe_libopus::opus_encoder_destroy(self.encoder) };
    }
}
