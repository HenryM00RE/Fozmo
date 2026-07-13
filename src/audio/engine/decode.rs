use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::audio::dsp::eq::EqProcessor;

use super::buffers::{
    AudioProducer, DsdWorkerState, output_has_room, write_audio_blocking, write_dsd_output_blocking,
};
use super::render::{
    output_headroom_gain, render_dsd_block, render_dsd_upsampler_tail, render_pcm_block,
    render_pcm_eof_tail,
};
use super::session::PlaybackSession;
use super::state::AtomicPlayerState;
use symphonia::core::audio::AudioBuffer;
use symphonia::core::errors::Error as SymphoniaError;

const SOURCE_READ_STALL_NS: u64 = 10_000_000;
const DECODER_DECODE_STALL_NS: u64 = 10_000_000;

pub(super) enum DecodePumpResult {
    Progress,
    Backpressured,
    EndOfStream,
    FatalError,
}

pub(super) fn pump_active_session(
    sess: &mut PlaybackSession,
    dsd_state: Option<&mut DsdWorkerState>,
    prod: &mut AudioProducer,
    state: &AtomicPlayerState,
    eq_processor: &mut EqProcessor,
    target_rate: u32,
    mut should_continue: impl FnMut() -> bool,
) -> DecodePumpResult {
    if !should_continue() {
        return DecodePumpResult::Progress;
    }
    if !output_has_room(dsd_state.as_deref(), prod) {
        return DecodePumpResult::Backpressured;
    }

    let source_read_start = Instant::now();
    let packet_result = sess.format.next_packet();
    state.record_source_read_ns(
        source_read_start.elapsed().as_nanos() as u64,
        SOURCE_READ_STALL_NS,
    );
    let packet = match packet_result {
        Ok(packet) => packet,
        Err(SymphoniaError::IoError(ref err))
            if err.kind() == std::io::ErrorKind::UnexpectedEof =>
        {
            return DecodePumpResult::EndOfStream;
        }
        Err(e) => {
            state.decoder_starved_count.fetch_add(1, Ordering::Relaxed);
            eprintln!("AudioWorker: Symphonia error: {:?}", e);
            return DecodePumpResult::FatalError;
        }
    };

    if packet.track_id() != sess.track_id {
        return DecodePumpResult::Progress;
    }
    if !should_continue() {
        return DecodePumpResult::Progress;
    }

    let decoded = {
        let decoder = &mut sess.decoder;
        let decode_start = Instant::now();
        let decode_result = decoder.decode(&packet);
        state.record_decoder_decode_ns(
            decode_start.elapsed().as_nanos() as u64,
            DECODER_DECODE_STALL_NS,
        );
        match decode_result {
            Ok(decoded) => decoded,
            Err(e) => {
                state.decoder_starved_count.fetch_add(1, Ordering::Relaxed);
                eprintln!("AudioWorker: Decoding error: {:?}", e);
                return DecodePumpResult::Progress;
            }
        }
    };
    if !should_continue() {
        return DecodePumpResult::Progress;
    }
    PlaybackSession::convert_decoded_buffer(&mut sess.sample_buffer, &decoded);
    drop(decoded);
    let sample_buf = std::mem::replace(&mut sess.sample_buffer, AudioBuffer::<f64>::unused());

    {
        let planes = sample_buf.planes();
        let samples_l = planes.planes()[0];
        let samples_r = if planes.planes().len() > 1 {
            planes.planes()[1]
        } else {
            samples_l
        };
        if samples_l.is_empty() || samples_r.is_empty() {
            state.decoder_starved_count.fetch_add(1, Ordering::Relaxed);
        }

        if let Some(ds) = dsd_state {
            let volume = f32::from_bits(state.volume.load(Ordering::Relaxed)) as f64;
            let headroom_gain =
                output_headroom_gain(f32::from_bits(state.headroom_db.load(Ordering::Relaxed)));
            let input_gain = volume * headroom_gain;
            let mut input_frame = 0;
            while let Some((chunk_l, chunk_r)) =
                ds.take_render_quantum_from_pcm(samples_l, samples_r, &mut input_frame)
            {
                render_dsd_block(ds, &chunk_l, &chunk_r, input_gain, state, eq_processor);
                write_dsd_output_blocking(ds, || {
                    should_continue() && !state.flush_buffer.load(Ordering::Relaxed)
                });
                ds.recycle_render_quantum_buffers(chunk_l, chunk_r);
                if !should_continue() {
                    break;
                }
            }
        } else {
            let volume = f32::from_bits(state.volume.load(Ordering::Relaxed)) as f64;
            let headroom_gain =
                output_headroom_gain(f32::from_bits(state.headroom_db.load(Ordering::Relaxed)));
            if render_pcm_block(
                sess,
                samples_l,
                samples_r,
                headroom_gain,
                volume,
                target_rate,
                state,
                eq_processor,
            ) {
                write_audio_blocking(prod, &sess.output_buffer, || {
                    should_continue() && !state.flush_buffer.load(Ordering::Relaxed)
                });
            }
        }
    }
    sess.sample_buffer = sample_buf;

    DecodePumpResult::Progress
}

pub(super) fn flush_dsd_staged_pcm_at_eof(
    ds: &mut DsdWorkerState,
    state: &AtomicPlayerState,
    eq_processor: &mut EqProcessor,
    mut should_continue: impl FnMut() -> bool,
) {
    if !should_continue() {
        return;
    }
    let Some((chunk_l, chunk_r)) = ds.take_all_staged_pcm() else {
        return;
    };
    let volume = f32::from_bits(state.volume.load(Ordering::Relaxed)) as f64;
    let headroom_gain =
        output_headroom_gain(f32::from_bits(state.headroom_db.load(Ordering::Relaxed)));
    render_dsd_block(
        ds,
        &chunk_l,
        &chunk_r,
        volume * headroom_gain,
        state,
        eq_processor,
    );
    write_dsd_output_blocking(ds, || {
        should_continue() && !state.flush_buffer.load(Ordering::Relaxed)
    });
    ds.recycle_render_quantum_buffers(chunk_l, chunk_r);
}

pub(super) fn flush_dsd_upsampler_tail_at_eof(
    ds: &mut DsdWorkerState,
    state: &AtomicPlayerState,
    mut should_continue: impl FnMut() -> bool,
) {
    if !should_continue() {
        return;
    }
    let volume = f32::from_bits(state.volume.load(Ordering::Relaxed)) as f64;
    let headroom_gain =
        output_headroom_gain(f32::from_bits(state.headroom_db.load(Ordering::Relaxed)));
    if render_dsd_upsampler_tail(ds, volume * headroom_gain, state) {
        write_dsd_output_blocking(ds, || {
            should_continue() && !state.flush_buffer.load(Ordering::Relaxed)
        });
    }
}

pub(super) fn flush_pcm_resampler_tail_at_eof(
    sess: &mut PlaybackSession,
    prod: &mut AudioProducer,
    state: &AtomicPlayerState,
    eq_processor: &mut EqProcessor,
    target_rate: u32,
    mut should_continue: impl FnMut() -> bool,
) {
    if !should_continue() {
        return;
    }
    let volume = f32::from_bits(state.volume.load(Ordering::Relaxed)) as f64;
    let headroom_gain =
        output_headroom_gain(f32::from_bits(state.headroom_db.load(Ordering::Relaxed)));
    if render_pcm_eof_tail(
        sess,
        headroom_gain,
        volume,
        target_rate,
        state,
        eq_processor,
    ) {
        write_audio_blocking(prod, &sess.output_buffer, || {
            should_continue() && !state.flush_buffer.load(Ordering::Relaxed)
        });
    }
}
