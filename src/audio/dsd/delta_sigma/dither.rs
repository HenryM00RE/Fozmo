/// Lightweight xorshift64* PRNG used by the standard modulator's TPDF dither.
/// Cryptographic quality is unnecessary; the stream only needs to be flat and
/// deterministic for a given channel seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    pub(super) fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    #[inline]
    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// TPDF dither in `(-1, 1)` from two independent uniforms.
    #[inline]
    pub(super) fn next_tpdf(&mut self) -> f64 {
        self.next_uniform_half() + self.next_uniform_half()
    }

    #[inline]
    fn next_uniform_half(&mut self) -> f64 {
        let bits = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        bits - 0.5
    }
}
