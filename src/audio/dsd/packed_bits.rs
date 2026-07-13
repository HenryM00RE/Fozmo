//! Packed MSB-first DSD bit buffers used for renderer migration experiments.
//!
//! The production renderer still moves legacy `Vec<u8>` bit flags today. This
//! type provides a byte-packed representation that can coexist with that path
//! and be compared byte-for-byte before changing the worker contract.

// Staged packed-bit migration helpers compile while the worker contract remains byte-flag based.
#![allow(dead_code)]

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackedDsdBits {
    bytes: Vec<u8>,
    len_bits: usize,
}

impl PackedDsdBits {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity_bits(bits: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(bits.div_ceil(8)),
            len_bits: 0,
        }
    }

    pub fn from_bit_bytes(bits: &[u8]) -> Self {
        let mut packed = Self::with_capacity_bits(bits.len());
        for bit in bits {
            packed.push_bit(*bit);
        }
        packed
    }

    #[inline]
    pub fn push_bit(&mut self, bit: u8) {
        let byte_idx = self.len_bits / 8;
        let shift = 7 - (self.len_bits % 8);
        if byte_idx == self.bytes.len() {
            self.bytes.push(0);
        }
        self.bytes[byte_idx] |= (bit & 1) << shift;
        self.len_bits += 1;
    }

    pub fn len_bits(&self) -> usize {
        self.len_bits
    }

    pub fn is_empty(&self) -> bool {
        self.len_bits == 0
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[inline]
    pub fn bit(&self, idx: usize) -> u8 {
        assert!(idx < self.len_bits);
        (self.bytes[idx / 8] >> (7 - (idx % 8))) & 1
    }

    pub fn unpack_to_bit_bytes(&self) -> Vec<u8> {
        (0..self.len_bits).map(|idx| self.bit(idx)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn packed_bits_round_trip_legacy_bit_bytes() {
        let bits: Vec<u8> = (0..257)
            .map(|i| (((i * 17) ^ (i >> 1) ^ 0x5a) & 1) as u8)
            .collect();
        let packed = PackedDsdBits::from_bit_bytes(&bits);

        assert_eq!(packed.len_bits(), bits.len());
        assert_eq!(packed.unpack_to_bit_bytes(), bits);
    }

    #[test]
    fn packed_bits_are_msb_first_with_zero_tail_padding() {
        let packed = PackedDsdBits::from_bit_bytes(&[1, 0, 1, 0, 0, 1, 1, 0, 1]);

        assert_eq!(packed.bytes(), &[0xa6, 0x80]);
        assert_eq!(packed.bit(8), 1);
    }

    proptest! {
        #[test]
        fn property_arbitrary_legacy_bits_round_trip(bits in proptest::collection::vec(any::<u8>(), 0..4096)) {
            let expected = bits.iter().map(|bit| bit & 1).collect::<Vec<_>>();
            let packed = PackedDsdBits::from_bit_bytes(&bits);
            prop_assert_eq!(packed.len_bits(), bits.len());
            prop_assert_eq!(packed.unpack_to_bit_bytes(), expected);
        }
    }
}
