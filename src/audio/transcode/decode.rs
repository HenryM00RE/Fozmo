//! Shared Symphonia decode front-end for playback derivatives.
//!
//! Any local source becomes blocks of stereo f64 planes, with the zone's
//! parametric EQ optionally applied in line at the source sample rate. Both
//! derivative encoders (Ogg Opus and FLAC) consume this so EQ behaves
//! identically regardless of the delivery format.

use crate::audio::eq::{EqConfig, EqProcessor};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use symphonia::core::audio::{AudioBuffer, AudioBufferRef, Signal};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Decode `source` into stereo f64 blocks and hand each to `on_block` along
/// with the (stable) source sample rate. Mono sources play on both sides.
/// `cancel` is polled between packets so an abandoned job can stop early.
pub(super) fn decode_stereo_blocks(
    source: &Path,
    eq: Option<&EqConfig>,
    cancel: &Arc<AtomicBool>,
    mut on_block: impl FnMut(&[f64], &[f64], u32) -> Result<(), String>,
) -> Result<(), String> {
    let file =
        std::fs::File::open(source).map_err(|e| format!("open source for transcode: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = source.extension().and_then(|ext| ext.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe source format: {e}"))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| "source has no default audio track".to_string())?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("create source decoder: {e}"))?;

    let mut sample_buffer = AudioBuffer::<f64>::unused();
    let mut eq_proc: Option<EqProcessor> = None;
    let mut stream_rate: Option<u32> = None;
    let mut left_buf = Vec::new();
    let mut right_buf = Vec::new();

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("transcode cancelled".to_string());
        }
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(ref err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(format!("read source packet: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // Match the player's tolerance for isolated bad packets.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decode source packet: {e}")),
        };
        let source_rate = decoded.spec().rate;
        match stream_rate {
            None => stream_rate = Some(source_rate),
            Some(rate) if rate != source_rate => {
                return Err(format!(
                    "source sample rate changed mid-stream ({rate} -> {source_rate})"
                ));
            }
            Some(_) => {}
        }
        convert_to_f64(&mut sample_buffer, &decoded);
        let (left, right) = stereo_planes(&sample_buffer);
        left_buf.clear();
        right_buf.clear();
        left_buf.extend_from_slice(left);
        right_buf.extend_from_slice(right);
        let frames = left_buf.len().min(right_buf.len());
        left_buf.truncate(frames);
        right_buf.truncate(frames);
        if let Some(config) = eq {
            let proc = eq_proc.get_or_insert_with(|| EqProcessor::new(source_rate, config));
            proc.process_planar_stereo(&mut left_buf, &mut right_buf);
        }
        on_block(&left_buf, &right_buf, source_rate)?;
    }
    Ok(())
}

fn convert_to_f64(sample_buffer: &mut AudioBuffer<f64>, decoded: &AudioBufferRef<'_>) {
    let spec = *decoded.spec();
    let capacity = decoded.capacity();
    if sample_buffer.spec() != &spec || sample_buffer.capacity() < capacity {
        *sample_buffer = AudioBuffer::<f64>::new(capacity as u64, spec);
    } else {
        sample_buffer.clear();
    }
    decoded.convert(sample_buffer);
}

/// Left/right planes for the decoded block; mono sources play on both sides.
fn stereo_planes(buffer: &AudioBuffer<f64>) -> (&[f64], &[f64]) {
    let left = buffer.chan(0);
    let right = if buffer.spec().channels.count() > 1 {
        buffer.chan(1)
    } else {
        left
    };
    (left, right)
}
