//! Native DSD byte packing for planar hardware transports such as ASIO.

// Staged DSD transports compile before every platform wires them into playback.
#![allow(dead_code)]

use crate::audio::dsd::packed_bits::PackedDsdBits;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeDsdOrder {
    MsbFirst,
    LsbFirst,
}

impl NativeDsdOrder {
    pub fn idle_byte(self) -> u8 {
        match self {
            NativeDsdOrder::MsbFirst => 0x69,
            NativeDsdOrder::LsbFirst => 0x96,
        }
    }
}

pub struct NativeDsdPacker {
    order: NativeDsdOrder,
    byte_l: u8,
    byte_r: u8,
    bits_in_byte: u8,
}

impl NativeDsdPacker {
    pub fn new(order: NativeDsdOrder) -> Self {
        Self {
            order,
            byte_l: 0,
            byte_r: 0,
            bits_in_byte: 0,
        }
    }

    pub fn set_order(&mut self, order: NativeDsdOrder) {
        debug_assert_eq!(self.bits_in_byte, 0, "native DSD order changed mid-byte");
        self.order = order;
    }

    pub fn reset(&mut self) {
        self.byte_l = 0;
        self.byte_r = 0;
        self.bits_in_byte = 0;
    }

    #[inline]
    pub fn push(&mut self, l_bit: u8, r_bit: u8, out_l: &mut Vec<u8>, out_r: &mut Vec<u8>) {
        let shift = 7 - self.bits_in_byte;
        self.byte_l |= (l_bit & 1) << shift;
        self.byte_r |= (r_bit & 1) << shift;
        self.bits_in_byte += 1;

        if self.bits_in_byte == 8 {
            let (l, r) = match self.order {
                NativeDsdOrder::MsbFirst => (self.byte_l, self.byte_r),
                NativeDsdOrder::LsbFirst => {
                    (self.byte_l.reverse_bits(), self.byte_r.reverse_bits())
                }
            };
            out_l.push(l);
            out_r.push(r);
            self.byte_l = 0;
            self.byte_r = 0;
            self.bits_in_byte = 0;
        }
    }

    pub fn push_stream(
        &mut self,
        l_bits: &[u8],
        r_bits: &[u8],
        out_l: &mut Vec<u8>,
        out_r: &mut Vec<u8>,
    ) {
        debug_assert_eq!(l_bits.len(), r_bits.len());
        out_l.reserve(l_bits.len() / 8);
        out_r.reserve(r_bits.len() / 8);
        let len = l_bits.len().min(r_bits.len());
        let mut offset = 0;

        if self.bits_in_byte != 0 {
            let needed = (8 - self.bits_in_byte) as usize;
            let partial = needed.min(len);
            for (l, r) in l_bits[..partial].iter().zip(&r_bits[..partial]) {
                self.push(*l, *r, out_l, out_r);
            }
            offset = partial;
            if self.bits_in_byte != 0 {
                return;
            }
        }

        let full_len = ((len - offset) / 8) * 8;
        let full_l = &l_bits[offset..offset + full_len];
        let full_r = &r_bits[offset..offset + full_len];
        for (l_chunk, r_chunk) in full_l.chunks_exact(8).zip(full_r.chunks_exact(8)) {
            self.push_packed_byte(pack_bits_8(l_chunk), pack_bits_8(r_chunk), out_l, out_r);
        }

        offset += full_len;
        for (l, r) in l_bits[offset..len].iter().zip(&r_bits[offset..len]) {
            self.push(*l, *r, out_l, out_r);
        }
    }

    pub fn push_packed_stream(
        &mut self,
        l_bits: &PackedDsdBits,
        r_bits: &PackedDsdBits,
        out_l: &mut Vec<u8>,
        out_r: &mut Vec<u8>,
    ) {
        debug_assert_eq!(l_bits.len_bits(), r_bits.len_bits());
        let len = l_bits.len_bits().min(r_bits.len_bits());
        out_l.reserve(len / 8);
        out_r.reserve(len / 8);

        let mut offset = 0;
        if self.bits_in_byte != 0 {
            let needed = (8 - self.bits_in_byte) as usize;
            let partial = needed.min(len);
            for idx in 0..partial {
                self.push(l_bits.bit(idx), r_bits.bit(idx), out_l, out_r);
            }
            offset = partial;
            if self.bits_in_byte != 0 {
                return;
            }
        }

        while offset + 8 <= len && offset % 8 == 0 {
            let byte_idx = offset / 8;
            self.push_packed_byte(
                l_bits.bytes()[byte_idx],
                r_bits.bytes()[byte_idx],
                out_l,
                out_r,
            );
            offset += 8;
        }

        while offset < len {
            self.push(l_bits.bit(offset), r_bits.bit(offset), out_l, out_r);
            offset += 1;
        }
    }

    /// Flush any partial byte by padding it with the DSD idle pattern.
    ///
    /// The packer always accumulates bits MSB-first internally. For LSB-first
    /// transports, the completed byte is reversed by `push`, so the internal
    /// padding pattern is still the MSB-first idle byte.
    pub fn flush_with_idle(&mut self, out_l: &mut Vec<u8>, out_r: &mut Vec<u8>) {
        if self.bits_in_byte == 0 {
            return;
        }

        let internal_idle = NativeDsdOrder::MsbFirst.idle_byte();
        while self.bits_in_byte != 0 {
            let shift = 7 - self.bits_in_byte;
            let idle_bit = (internal_idle >> shift) & 1;
            self.push(idle_bit, idle_bit, out_l, out_r);
        }
    }

    #[inline]
    fn push_packed_byte(&mut self, l: u8, r: u8, out_l: &mut Vec<u8>, out_r: &mut Vec<u8>) {
        debug_assert_eq!(self.bits_in_byte, 0);
        match self.order {
            NativeDsdOrder::MsbFirst => {
                out_l.push(l);
                out_r.push(r);
            }
            NativeDsdOrder::LsbFirst => {
                out_l.push(l.reverse_bits());
                out_r.push(r.reverse_bits());
            }
        }
    }
}

#[inline]
fn pack_bits_8(bits: &[u8]) -> u8 {
    debug_assert_eq!(bits.len(), 8);
    let mut packed = 0_u8;
    for bit in bits {
        packed = (packed << 1) | (*bit & 1);
    }
    packed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_output_is_planar_and_msb_first() {
        let mut packer = NativeDsdPacker::new(NativeDsdOrder::MsbFirst);
        let mut l = Vec::new();
        let mut r = Vec::new();
        packer.push_stream(
            &[1, 0, 0, 0, 0, 0, 0, 1],
            &[0, 1, 0, 0, 0, 0, 1, 0],
            &mut l,
            &mut r,
        );
        assert_eq!(l, vec![0x81]);
        assert_eq!(r, vec![0x42]);
    }

    #[test]
    fn lsb_transport_reverses_bytes_before_output() {
        let mut packer = NativeDsdPacker::new(NativeDsdOrder::LsbFirst);
        let mut l = Vec::new();
        let mut r = Vec::new();
        packer.push_stream(
            &[1, 0, 0, 0, 0, 0, 0, 0],
            &[0, 0, 0, 0, 0, 0, 0, 1],
            &mut l,
            &mut r,
        );
        assert_eq!(l, vec![0x01]);
        assert_eq!(r, vec![0x80]);
    }

    #[test]
    fn native_partial_byte_is_buffered() {
        let mut packer = NativeDsdPacker::new(NativeDsdOrder::MsbFirst);
        let mut l = Vec::new();
        let mut r = Vec::new();
        packer.push_stream(&[1; 4], &[0; 4], &mut l, &mut r);
        assert!(l.is_empty());
        packer.push_stream(&[1; 4], &[0; 4], &mut l, &mut r);
        assert_eq!(l, vec![0xff]);
        assert_eq!(r, vec![0x00]);
    }

    #[test]
    fn flush_with_idle_pads_partial_msb_byte() {
        let mut packer = NativeDsdPacker::new(NativeDsdOrder::MsbFirst);
        let mut l = Vec::new();
        let mut r = Vec::new();
        packer.push_stream(&[1, 0, 1, 0], &[0, 1, 0, 1], &mut l, &mut r);
        packer.flush_with_idle(&mut l, &mut r);

        assert_eq!(l, vec![0xa9]);
        assert_eq!(r, vec![0x59]);

        packer.push_stream(&[1; 8], &[0; 8], &mut l, &mut r);
        assert_eq!(l, vec![0xa9, 0xff]);
        assert_eq!(r, vec![0x59, 0x00]);
    }

    #[test]
    fn flush_with_idle_pads_partial_lsb_byte() {
        let mut packer = NativeDsdPacker::new(NativeDsdOrder::LsbFirst);
        let mut l = Vec::new();
        let mut r = Vec::new();
        packer.push_stream(&[1, 0, 1, 0], &[0, 1, 0, 1], &mut l, &mut r);
        packer.flush_with_idle(&mut l, &mut r);

        assert_eq!(l, vec![0x95]);
        assert_eq!(r, vec![0x9a]);
    }

    #[test]
    fn flush_with_idle_is_noop_on_byte_boundary() {
        let mut packer = NativeDsdPacker::new(NativeDsdOrder::MsbFirst);
        let mut l = Vec::new();
        let mut r = Vec::new();
        packer.push_stream(&[1; 8], &[0; 8], &mut l, &mut r);
        packer.flush_with_idle(&mut l, &mut r);

        assert_eq!(l, vec![0xff]);
        assert_eq!(r, vec![0x00]);
    }

    #[test]
    fn push_stream_matches_bit_push_for_varied_boundaries_and_orders() {
        let mut seed = 0x2345_6789_abcd_ef01_u64;
        let mut l_bits = Vec::new();
        let mut r_bits = Vec::new();
        for _ in 0..1027 {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            l_bits.push((seed >> 63) as u8);
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            r_bits.push((seed >> 62) as u8);
        }

        for order in [NativeDsdOrder::MsbFirst, NativeDsdOrder::LsbFirst] {
            let mut expected_packer = NativeDsdPacker::new(order);
            let mut expected_l = Vec::new();
            let mut expected_r = Vec::new();
            for (l, r) in l_bits.iter().zip(&r_bits) {
                expected_packer.push(*l, *r, &mut expected_l, &mut expected_r);
            }
            expected_packer.flush_with_idle(&mut expected_l, &mut expected_r);

            for split_seed in 0..64 {
                let mut actual_packer = NativeDsdPacker::new(order);
                let mut actual_l = Vec::new();
                let mut actual_r = Vec::new();
                let mut pos = 0;
                while pos < l_bits.len() {
                    let chunk = ((pos * 19 + split_seed * 11) % 71 + 1).min(l_bits.len() - pos);
                    actual_packer.push_stream(
                        &l_bits[pos..pos + chunk],
                        &r_bits[pos..pos + chunk],
                        &mut actual_l,
                        &mut actual_r,
                    );
                    pos += chunk;
                }
                actual_packer.flush_with_idle(&mut actual_l, &mut actual_r);
                assert_eq!(actual_l, expected_l, "{order:?}, split seed {split_seed}");
                assert_eq!(actual_r, expected_r, "{order:?}, split seed {split_seed}");
            }
        }
    }

    #[test]
    fn packed_stream_matches_legacy_stream_for_varied_boundaries() {
        let l_bits: Vec<u8> = (0..1027)
            .map(|i| (((i * 17) ^ (i >> 2) ^ 0x33) & 1) as u8)
            .collect();
        let r_bits: Vec<u8> = (0..1027)
            .map(|i| (((i * 29) ^ (i >> 1) ^ 0x55) & 1) as u8)
            .collect();

        for order in [NativeDsdOrder::MsbFirst, NativeDsdOrder::LsbFirst] {
            for prefix in 0..8 {
                let mut legacy = NativeDsdPacker::new(order);
                let mut legacy_l = Vec::new();
                let mut legacy_r = Vec::new();
                legacy.push_stream(
                    &l_bits[..prefix],
                    &r_bits[..prefix],
                    &mut legacy_l,
                    &mut legacy_r,
                );
                legacy.push_stream(
                    &l_bits[prefix..],
                    &r_bits[prefix..],
                    &mut legacy_l,
                    &mut legacy_r,
                );
                legacy.flush_with_idle(&mut legacy_l, &mut legacy_r);

                let mut packed = NativeDsdPacker::new(order);
                let mut actual_l = Vec::new();
                let mut actual_r = Vec::new();
                packed.push_stream(
                    &l_bits[..prefix],
                    &r_bits[..prefix],
                    &mut actual_l,
                    &mut actual_r,
                );
                let suffix_l = PackedDsdBits::from_bit_bytes(&l_bits[prefix..]);
                let suffix_r = PackedDsdBits::from_bit_bytes(&r_bits[prefix..]);
                packed.push_packed_stream(&suffix_l, &suffix_r, &mut actual_l, &mut actual_r);
                packed.flush_with_idle(&mut actual_l, &mut actual_r);

                assert_eq!(actual_l, legacy_l, "{order:?}, prefix {prefix}");
                assert_eq!(actual_r, legacy_r, "{order:?}, prefix {prefix}");
            }
        }
    }

    #[test]
    fn negotiated_order_defines_safe_idle_byte() {
        assert_eq!(NativeDsdOrder::MsbFirst.idle_byte(), 0x69);
        assert_eq!(NativeDsdOrder::LsbFirst.idle_byte(), 0x96);
    }
}
