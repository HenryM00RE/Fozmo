//! DSD-over-PCM (DoP) frame packer.
//!
//! Spec: <https://dsd-guide.com/sites/default/files/white-papers/DoP_open_Standard_1v1.pdf>
//!
//! Sixteen 1-bit DSD samples are packed MSB-first into the low 16 bits of a 24-bit
//! PCM container. The top 8 bits carry an alternating marker — 0x05 on odd DoP frames,
//! 0xFA on even — which a DoP-capable DAC recognises to switch into DSD mode.
//!
//! The wire output is stereo interleaved `i32` (24-bit data in a 32-bit container,
//! left-justified to bit 23). The DoP frame rate is the DSD rate / 16 — so DSD128 at
//! 5.6448 MHz becomes a 352.8 kHz / 24-bit / 2-channel PCM stream, which the existing
//! WASAPI exclusive backend can carry without modification to the transport layer.

// Staged DSD transports compile before every platform wires them into playback.
#![allow(dead_code)]

use crate::audio::dsd::packed_bits::PackedDsdBits;

const MARKER_A: u32 = 0x05;
const MARKER_B: u32 = 0xFA;
const DSD_SILENCE: u32 = 0x6969;

pub struct DopPacker {
    /// `false` → next emitted frame uses MARKER_A; `true` → MARKER_B. Toggles per frame.
    marker_phase_b: bool,
    /// Accumulated DSD bits, MSB-first. Bit (15 - bits_in_buf) is the next slot.
    bit_buf_l: u16,
    bit_buf_r: u16,
    bits_in_buf: u8,
}

impl DopPacker {
    pub fn new() -> Self {
        Self {
            marker_phase_b: false,
            bit_buf_l: 0,
            bit_buf_r: 0,
            bits_in_buf: 0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Push one stereo DSD bit pair. After every 16 calls the packer emits one DoP
    /// frame: two `i32` samples (L then R) appended to `out`, each containing the
    /// marker in the top byte of the 24-bit field.
    #[inline]
    pub fn push(&mut self, l_bit: u8, r_bit: u8, out: &mut Vec<i32>) {
        let shift = 15 - self.bits_in_buf;
        self.bit_buf_l |= ((l_bit & 1) as u16) << shift;
        self.bit_buf_r |= ((r_bit & 1) as u16) << shift;
        self.bits_in_buf += 1;

        if self.bits_in_buf == 16 {
            self.emit_frame(self.bit_buf_l, self.bit_buf_r, out);
            self.clear_partial_frame();
        }
    }

    /// Push two equal-length 1-bit streams. The streams must be a multiple of 16 long
    /// for a clean flush; any residue is held in the internal buffer for the next call.
    pub fn push_stream(&mut self, l_bits: &[u8], r_bits: &[u8], out: &mut Vec<i32>) {
        debug_assert_eq!(l_bits.len(), r_bits.len());
        let len = l_bits.len().min(r_bits.len());
        out.reserve((len + self.bits_in_buf as usize) / 8); // 2 i32s per 16 bit-pairs

        let mut offset = 0;
        if self.bits_in_buf != 0 {
            let needed = (16 - self.bits_in_buf) as usize;
            let partial = needed.min(len);
            self.push_partial_bits(&l_bits[..partial], &r_bits[..partial]);
            offset = partial;
            if self.bits_in_buf == 16 {
                self.emit_frame(self.bit_buf_l, self.bit_buf_r, out);
                self.clear_partial_frame();
            } else {
                return;
            }
        }

        let full_len = ((len - offset) / 16) * 16;
        let full_l = &l_bits[offset..offset + full_len];
        let full_r = &r_bits[offset..offset + full_len];
        for (l_chunk, r_chunk) in full_l.chunks_exact(16).zip(full_r.chunks_exact(16)) {
            self.emit_frame(pack_bits_16(l_chunk), pack_bits_16(r_chunk), out);
        }

        offset += full_len;
        self.push_partial_bits(&l_bits[offset..len], &r_bits[offset..len]);
    }

    pub fn push_packed_stream(
        &mut self,
        l_bits: &PackedDsdBits,
        r_bits: &PackedDsdBits,
        out: &mut Vec<i32>,
    ) {
        debug_assert_eq!(l_bits.len_bits(), r_bits.len_bits());
        let len = l_bits.len_bits().min(r_bits.len_bits());
        out.reserve((len + self.bits_in_buf as usize) / 8);

        let mut offset = 0;
        if self.bits_in_buf != 0 {
            let needed = (16 - self.bits_in_buf) as usize;
            let partial = needed.min(len);
            for idx in 0..partial {
                self.push(l_bits.bit(idx), r_bits.bit(idx), out);
            }
            offset = partial;
            if self.bits_in_buf != 0 {
                return;
            }
        }

        while offset + 16 <= len && offset % 8 == 0 {
            self.emit_frame(
                packed_bits_16(l_bits.bytes(), offset),
                packed_bits_16(r_bits.bytes(), offset),
                out,
            );
            offset += 16;
        }

        while offset < len {
            self.push(l_bits.bit(offset), r_bits.bit(offset), out);
            offset += 1;
        }
    }

    #[inline]
    fn emit_frame(&mut self, l_bits: u16, r_bits: u16, out: &mut Vec<i32>) {
        let marker = if self.marker_phase_b {
            MARKER_B
        } else {
            MARKER_A
        };
        // 24-bit value with marker in bits [23:16], DSD bits in [15:0].
        // Left-justify into a 32-bit container (shift up by 8) so it lands in
        // the high 24 bits of the i32, matching WASAPI's 24-in-32 layout.
        let frame_l = ((marker << 16) | l_bits as u32) << 8;
        let frame_r = ((marker << 16) | r_bits as u32) << 8;
        out.push(frame_l as i32);
        out.push(frame_r as i32);
        self.marker_phase_b = !self.marker_phase_b;
    }

    #[inline]
    fn clear_partial_frame(&mut self) {
        self.bit_buf_l = 0;
        self.bit_buf_r = 0;
        self.bits_in_buf = 0;
    }

    #[inline]
    fn push_partial_bits(&mut self, l_bits: &[u8], r_bits: &[u8]) {
        debug_assert_eq!(l_bits.len(), r_bits.len());
        for (l, r) in l_bits.iter().zip(r_bits.iter()) {
            self.push_partial_bit_pair(*l, *r);
        }
    }

    #[inline]
    fn push_partial_bit_pair(&mut self, l: u8, r: u8) {
        let shift = 15 - self.bits_in_buf;
        self.bit_buf_l |= ((l & 1) as u16) << shift;
        self.bit_buf_r |= ((r & 1) as u16) << shift;
        self.bits_in_buf += 1;
    }
}

#[inline]
fn pack_bits_16(bits: &[u8]) -> u16 {
    debug_assert_eq!(bits.len(), 16);
    bits.iter()
        .fold(0_u16, |packed, bit| (packed << 1) | ((*bit & 1) as u16))
}

#[inline]
fn packed_bits_16(bytes: &[u8], bit_offset: usize) -> u16 {
    debug_assert_eq!(bit_offset % 8, 0);
    let byte_idx = bit_offset / 8;
    let hi = bytes[byte_idx] as u16;
    let lo = bytes[byte_idx + 1] as u16;
    (hi << 8) | lo
}

impl Default for DopPacker {
    fn default() -> Self {
        Self::new()
    }
}

/// Marker-correct DoP idle frames for underruns, pause, and startup gaps.
///
/// A DoP DAC needs the 0x05/0xFA marker cadence even when there is no program
/// audio. Emitting literal zero PCM can make some receivers leave or fail to
/// enter DSD mode until the stream is flushed.
pub struct DopIdlePattern {
    marker_phase_b: bool,
}

impl DopIdlePattern {
    pub fn new() -> Self {
        Self {
            marker_phase_b: false,
        }
    }

    pub fn with_next_marker_phase_b(marker_phase_b: bool) -> Self {
        Self { marker_phase_b }
    }

    pub fn reset(&mut self) {
        self.marker_phase_b = false;
    }

    pub fn fill_interleaved_i32(&mut self, out: &mut [i32], channels: usize) {
        let channels = channels.max(1);
        debug_assert_eq!(
            out.len() % channels,
            0,
            "DoP idle fill must cover whole frames"
        );
        for frame in out.chunks_mut(channels) {
            let sample = self.next_sample();
            for value in frame {
                *value = sample;
            }
        }
    }

    fn next_sample(&mut self) -> i32 {
        let marker = if self.marker_phase_b {
            MARKER_B
        } else {
            MARKER_A
        };
        self.marker_phase_b = !self.marker_phase_b;
        (((marker << 16) | DSD_SILENCE) << 8) as i32
    }
}

impl Default for DopIdlePattern {
    fn default() -> Self {
        Self::new()
    }
}

/// Single marker-phase authority for an output stream.
///
/// `DopPacker` (program audio) and `DopIdlePattern` (underrun/pause fill) each track
/// their own marker phase, so at any splice between the two — startup, pause/resume,
/// underrun recovery — nothing guarantees the 0x05/0xFA alternation continues. Two
/// consecutive identical markers break the DoP detection cadence and can knock the
/// DAC out of DSD mode.
///
/// The output thread owns one of these for the lifetime of the stream and re-stamps
/// the marker byte of every outgoing sample just before it hits the device, making
/// marker continuity unconditional regardless of which source produced the frame.
/// Markers carry no data — they are purely positional — so overwriting them is safe.
pub struct DopMarkerStamper {
    marker_phase_b: bool,
}

impl DopMarkerStamper {
    pub fn new() -> Self {
        Self {
            marker_phase_b: false,
        }
    }

    pub fn with_next_marker_phase_b(marker_phase_b: bool) -> Self {
        Self { marker_phase_b }
    }

    /// Overwrite the marker byte (bits [31:24] of the left-justified 24-in-32 layout)
    /// of every frame in `out`, advancing the alternation once per frame.
    pub fn restamp_interleaved_i32(&mut self, out: &mut [i32], channels: usize) {
        let channels = channels.max(1);
        debug_assert_eq!(
            out.len() % channels,
            0,
            "DoP restamp must cover whole frames"
        );
        for frame in out.chunks_mut(channels) {
            let marker = if self.marker_phase_b {
                MARKER_B
            } else {
                MARKER_A
            };
            self.marker_phase_b = !self.marker_phase_b;
            for value in frame {
                *value = (((*value as u32) & 0x00FF_FFFF) | (marker << 24)) as i32;
            }
        }
    }
}

impl Default for DopMarkerStamper {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extract the 24-bit DoP value from the 32-bit container (right-shift by 8).
    fn unpack_24(sample: i32) -> u32 {
        ((sample as u32) >> 8) & 0x00FF_FFFF
    }

    #[test]
    fn marker_alternates_per_frame() {
        let mut packer = DopPacker::new();
        let mut out = Vec::new();
        // 32 bit-pairs → 2 DoP frames → 4 i32 samples (L, R, L, R).
        for _ in 0..32 {
            packer.push(0, 0, &mut out);
        }
        assert_eq!(out.len(), 4);
        let frame0_l = unpack_24(out[0]) >> 16;
        let frame0_r = unpack_24(out[1]) >> 16;
        let frame1_l = unpack_24(out[2]) >> 16;
        let frame1_r = unpack_24(out[3]) >> 16;
        assert_eq!(frame0_l, MARKER_A);
        assert_eq!(frame0_r, MARKER_A);
        assert_eq!(frame1_l, MARKER_B);
        assert_eq!(frame1_r, MARKER_B);
    }

    #[test]
    fn bits_are_msb_first() {
        let mut packer = DopPacker::new();
        let mut out = Vec::new();
        // Pattern: 1,0,0,0,0,0,0,0, 0,0,0,0,0,0,0,0 (left channel)
        //          0,0,0,0,0,0,0,0, 0,0,0,0,0,0,0,1 (right channel)
        let l = [1u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let r = [0u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        packer.push_stream(&l, &r, &mut out);
        assert_eq!(out.len(), 2);
        let lbits = unpack_24(out[0]) & 0xFFFF;
        let rbits = unpack_24(out[1]) & 0xFFFF;
        assert_eq!(lbits, 0x8000, "left should have only MSB set");
        assert_eq!(rbits, 0x0001, "right should have only LSB set");
    }

    #[test]
    fn marker_phase_persists_across_calls() {
        let mut packer = DopPacker::new();
        let mut out = Vec::new();
        // First call: 16 bits → 1 frame (MARKER_A).
        packer.push_stream(&[0u8; 16], &[0u8; 16], &mut out);
        // Second call: 16 bits → 1 frame (should be MARKER_B).
        packer.push_stream(&[0u8; 16], &[0u8; 16], &mut out);
        assert_eq!(out.len(), 4);
        assert_eq!(unpack_24(out[0]) >> 16, MARKER_A);
        assert_eq!(unpack_24(out[2]) >> 16, MARKER_B);
    }

    #[test]
    fn partial_frame_is_buffered() {
        let mut packer = DopPacker::new();
        let mut out = Vec::new();
        packer.push_stream(&[1u8; 8], &[1u8; 8], &mut out);
        assert!(
            out.is_empty(),
            "only 8 bits in, no frame should be emitted yet"
        );
        packer.push_stream(&[1u8; 8], &[1u8; 8], &mut out);
        assert_eq!(out.len(), 2, "16 bits total should emit exactly one frame");
        // All bits were 1 → low 16 bits = 0xFFFF.
        assert_eq!(unpack_24(out[0]) & 0xFFFF, 0xFFFF);
    }

    #[test]
    fn push_stream_matches_push_for_varied_boundaries() {
        let mut seed = 0x1234_5678_9abc_def0_u64;
        let mut l_bits = Vec::new();
        let mut r_bits = Vec::new();
        for _ in 0..4097 {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            l_bits.push((seed >> 63) as u8);
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            r_bits.push((seed >> 62) as u8);
        }

        let mut expected_packer = DopPacker::new();
        let mut expected = Vec::new();
        for (l, r) in l_bits.iter().zip(&r_bits) {
            expected_packer.push(*l, *r, &mut expected);
        }

        for split_seed in 0..64 {
            let mut actual_packer = DopPacker::new();
            let mut actual = Vec::new();
            let mut pos = 0;
            while pos < l_bits.len() {
                let chunk = ((pos * 17 + split_seed * 13) % 73 + 1).min(l_bits.len() - pos);
                actual_packer.push_stream(
                    &l_bits[pos..pos + chunk],
                    &r_bits[pos..pos + chunk],
                    &mut actual,
                );
                pos += chunk;
            }
            assert_eq!(actual, expected, "split seed {split_seed}");
        }
    }

    #[test]
    fn packed_stream_matches_legacy_stream_for_varied_boundaries() {
        let l_bits: Vec<u8> = (0..4097)
            .map(|i| (((i * 13) ^ (i >> 2) ^ 0x21) & 1) as u8)
            .collect();
        let r_bits: Vec<u8> = (0..4097)
            .map(|i| (((i * 31) ^ (i >> 1) ^ 0x42) & 1) as u8)
            .collect();

        for prefix in 0..16 {
            let mut legacy = DopPacker::new();
            let mut legacy_out = Vec::new();
            legacy.push_stream(&l_bits[..prefix], &r_bits[..prefix], &mut legacy_out);
            legacy.push_stream(&l_bits[prefix..], &r_bits[prefix..], &mut legacy_out);

            let mut packed = DopPacker::new();
            let mut actual = Vec::new();
            packed.push_stream(&l_bits[..prefix], &r_bits[..prefix], &mut actual);
            let suffix_l = PackedDsdBits::from_bit_bytes(&l_bits[prefix..]);
            let suffix_r = PackedDsdBits::from_bit_bytes(&r_bits[prefix..]);
            packed.push_packed_stream(&suffix_l, &suffix_r, &mut actual);

            assert_eq!(actual, legacy_out, "prefix {prefix}");
        }
    }

    #[test]
    fn idle_pattern_preserves_markers() {
        let mut idle = DopIdlePattern::new();
        let mut out = [0i32; 6];
        idle.fill_interleaved_i32(&mut out, 2);

        assert_eq!(unpack_24(out[0]) >> 16, MARKER_A);
        assert_eq!(unpack_24(out[1]) >> 16, MARKER_A);
        assert_eq!(unpack_24(out[2]) >> 16, MARKER_B);
        assert_eq!(unpack_24(out[3]) >> 16, MARKER_B);
        assert_eq!(unpack_24(out[4]) & 0xFFFF, DSD_SILENCE);
        assert_eq!(unpack_24(out[5]) & 0xFFFF, DSD_SILENCE);

        idle.reset();
        idle.fill_interleaved_i32(&mut out[..2], 2);
        assert_eq!(unpack_24(out[0]) >> 16, MARKER_A);
    }

    #[test]
    fn stamper_repairs_marker_splice_between_program_and_idle() {
        // Simulate the underrun splice: packer frames followed by idle frames,
        // where both sources happen to sit at the same phase.
        let mut packer = DopPacker::new();
        let mut buf = Vec::new();
        packer.push_stream(&[1u8; 32], &[0u8; 32], &mut buf); // 2 frames: A, B
        // Splice in idle frames starting at the idle generator's B phase, so the
        // wire sequence is A, B, B, A — a broken cadence.
        let mut idle = DopIdlePattern::new();
        let mut idle_buf = [0i32; 4];
        idle.fill_interleaved_i32(&mut idle_buf, 2); // frames: A, B
        buf.extend_from_slice(&idle_buf[2..]);
        buf.extend_from_slice(&idle_buf[..2]);

        // Confirm the splice is broken before re-stamping.
        let markers: Vec<u32> = buf.chunks(2).map(|f| unpack_24(f[0]) >> 16).collect();
        assert_eq!(
            markers[1], markers[2],
            "test setup must create a bad splice"
        );

        let mut stamper = DopMarkerStamper::new();
        stamper.restamp_interleaved_i32(&mut buf, 2);

        // Markers now strictly alternate, both channels match, payload untouched.
        for (i, frame) in buf.chunks(2).enumerate() {
            let expect = if i % 2 == 0 { MARKER_A } else { MARKER_B };
            assert_eq!(unpack_24(frame[0]) >> 16, expect);
            assert_eq!(unpack_24(frame[1]) >> 16, expect);
        }
        assert_eq!(unpack_24(buf[0]) & 0xFFFF, 0xFFFF, "payload must survive");
        assert_eq!(unpack_24(buf[1]) & 0xFFFF, 0x0000, "payload must survive");
        assert_eq!(unpack_24(buf[4]) & 0xFFFF, DSD_SILENCE);
    }

    #[test]
    fn stamper_phase_persists_across_buffers() {
        let mut stamper = DopMarkerStamper::new();
        let mut a = [0i32; 2]; // one frame
        let mut b = [0i32; 2]; // next buffer, one frame
        stamper.restamp_interleaved_i32(&mut a, 2);
        stamper.restamp_interleaved_i32(&mut b, 2);
        assert_eq!(unpack_24(a[0]) >> 16, MARKER_A);
        assert_eq!(unpack_24(b[0]) >> 16, MARKER_B);
    }
}
