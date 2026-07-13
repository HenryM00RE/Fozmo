//! Incremental FLAC encoding for lossless (EQ'd) browser streams.
//!
//! Unlike the Sonos/UPnP sinks, which encode whole tracks in memory, this
//! writes the stream progressively — `fLaC` magic + STREAMINFO first, then
//! byte-aligned frames as they are produced — so browsers can start playback
//! while the derivative is still encoding. The header initially reports an
//! unknown total-sample count; [`encode_flac`] returns the finalized header
//! so callers can patch the file prefix once the encode completes, giving
//! fully cached derivatives an accurate duration for seeking.

use super::decode::decode_stereo_blocks;
use crate::audio::eq::EqConfig;
use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, Stream, StreamInfo};
use flacenc::config;
use flacenc::error::{Verified, Verify};
use flacenc::source::{Fill, FrameBuf};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Frames per FLAC block, matching the sinks' fixed block size.
const FLAC_BLOCK_LEN: usize = 4096;
/// The smallest block `flacenc` accepts (`constant::MIN_BLOCK_SIZE`, stricter
/// than the FLAC spec's 16); shorter tails are zero-padded up to this.
const MIN_FLAC_BLOCK_LEN: usize = 32;
/// EQ output is re-quantized to 24-bit regardless of source depth.
const FLAC_BITS_PER_SAMPLE: usize = 24;

/// Encode `source` (with EQ applied at the source rate) to a FLAC stream on
/// `out`. Returns the finalized stream header (identical length to the one
/// already written) so the caller can patch the file prefix in place.
pub fn encode_flac(
    source: &Path,
    out: &mut dyn Write,
    eq: Option<&EqConfig>,
    cancel: &Arc<AtomicBool>,
) -> Result<Vec<u8>, String> {
    let config = config::Encoder::default()
        .into_verified()
        .map_err(|e| format!("verify FLAC encoder config: {e:?}"))?;

    let mut encoder: Option<FlacStreamEncoder> = None;
    decode_stereo_blocks(source, eq, cancel, |left, right, source_rate| {
        if encoder.is_none() {
            encoder = Some(FlacStreamEncoder::start(out, source_rate)?);
        }
        let encoder = encoder.as_mut().expect("flac encoder started");
        encoder.push(out, &config, left, right)
    })?;
    let Some(encoder) = encoder else {
        return Err("source produced no audio".to_string());
    };
    encoder.finish(out, &config)
}

struct FlacStreamEncoder {
    stream_info: StreamInfo,
    /// Interleaved i32 (24-bit range) samples not yet forming a whole block.
    pending: Vec<i32>,
    frame_number: usize,
}

impl FlacStreamEncoder {
    fn start(out: &mut dyn Write, sample_rate: u32) -> Result<Self, String> {
        let mut stream_info = StreamInfo::new(sample_rate as usize, 2, FLAC_BITS_PER_SAMPLE)
            .map_err(|e| format!("create FLAC stream info: {e:?}"))?;
        stream_info
            .set_block_sizes(FLAC_BLOCK_LEN, FLAC_BLOCK_LEN)
            .map_err(|e| format!("set FLAC block sizes: {e:?}"))?;
        // 0 = unknown per the FLAC spec; patched via `update_frame_info`.
        stream_info
            .set_frame_sizes(0, 0)
            .map_err(|e| format!("set FLAC frame sizes: {e:?}"))?;
        let header = header_bytes(&stream_info)?;
        out.write_all(&header)
            .map_err(|e| format!("write FLAC header: {e}"))?;
        Ok(Self {
            stream_info,
            pending: Vec::with_capacity(FLAC_BLOCK_LEN * 2),
            frame_number: 0,
        })
    }

    fn push(
        &mut self,
        out: &mut dyn Write,
        config: &Verified<config::Encoder>,
        left: &[f64],
        right: &[f64],
    ) -> Result<(), String> {
        let frames = left.len().min(right.len());
        self.pending.reserve(frames * 2);
        for i in 0..frames {
            self.pending.push(float_to_i24(left[i]));
            self.pending.push(float_to_i24(right[i]));
        }
        while self.pending.len() >= FLAC_BLOCK_LEN * 2 {
            let rest = self.pending.split_off(FLAC_BLOCK_LEN * 2);
            let block = std::mem::replace(&mut self.pending, rest);
            self.encode_block(out, config, &block)?;
        }
        Ok(())
    }

    fn encode_block(
        &mut self,
        out: &mut dyn Write,
        config: &Verified<config::Encoder>,
        interleaved: &[i32],
    ) -> Result<(), String> {
        let block_len = interleaved.len() / 2;
        let mut framebuf = FrameBuf::with_size(2, block_len)
            .map_err(|e| format!("create FLAC frame buffer: {e:?}"))?;
        framebuf
            .fill_interleaved(interleaved)
            .map_err(|e| format!("fill FLAC frame buffer: {e:?}"))?;
        let frame = flacenc::encode_fixed_size_frame(
            config,
            &framebuf,
            self.frame_number,
            &self.stream_info,
        )
        .map_err(|e| format!("encode FLAC frame: {e:?}"))?;
        self.stream_info.update_frame_info(&frame);
        self.frame_number += 1;
        let mut sink = ByteSink::new();
        frame
            .write(&mut sink)
            .map_err(|e| format!("write FLAC frame: {e:?}"))?;
        out.write_all(&sink.into_inner())
            .map_err(|e| format!("write FLAC frame bytes: {e}"))
    }

    fn finish(
        mut self,
        out: &mut dyn Write,
        config: &Verified<config::Encoder>,
    ) -> Result<Vec<u8>, String> {
        if !self.pending.is_empty() {
            // The final frame may be shorter than the fixed block size, but
            // not shorter than the spec minimum: pad sub-16-frame tails.
            while self.pending.len() < MIN_FLAC_BLOCK_LEN * 2 {
                self.pending.push(0);
            }
            let block = std::mem::take(&mut self.pending);
            self.encode_block(out, config, &block)?;
        }
        // `update_frame_info` folds the shorter tail frame into STREAMINFO's
        // min block size, which declares the stream variable-blocksize even
        // though every frame uses the fixed-blocksize strategy. The spec
        // exempts the last frame from the minimum, so keep min == max: Apple's
        // CoreMedia demuxer (iOS Safari) otherwise fails to packetize the
        // stream and playback never starts.
        self.stream_info
            .set_block_sizes(FLAC_BLOCK_LEN, FLAC_BLOCK_LEN)
            .map_err(|e| format!("finalize FLAC block sizes: {e:?}"))?;
        header_bytes(&self.stream_info)
    }
}

/// `fLaC` magic + STREAMINFO block for `stream_info`. Always the same length
/// for a given stream, so the finalized header can overwrite the provisional
/// one in place.
fn header_bytes(stream_info: &StreamInfo) -> Result<Vec<u8>, String> {
    let stream = Stream::with_stream_info(stream_info.clone());
    let mut sink = ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| format!("write FLAC stream header: {e:?}"))?;
    Ok(sink.into_inner())
}

fn float_to_i24(sample: f64) -> i32 {
    (sample.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32
}
