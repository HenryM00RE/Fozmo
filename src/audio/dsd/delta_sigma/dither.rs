use super::modulator::{DitherPrng, DitherShape};

/// Lightweight xorshift64* PRNG. We don't need cryptographic quality — just a
/// statistically flat stream of f64s for the dither generator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    pub(super) fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    #[inline]
    pub(super) fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform f64 in (-0.5, 0.5).
    #[inline]
    pub(super) fn next_uniform_half(&mut self) -> f64 {
        // Take the top 53 bits, map to [0,1), then shift to (-0.5, 0.5).
        let bits = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        bits - 0.5
    }

    /// TPDF dither: sum of two independent uniforms in (-0.5, 0.5).
    /// Output range is (-1, 1), triangular distribution centered at 0.
    #[inline]
    pub(super) fn next_tpdf(&mut self) -> f64 {
        self.next_uniform_half() + self.next_uniform_half()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub(super) fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    #[inline]
    pub(super) fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        splitmix64_mix(self.state)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Xoshiro256StarStar {
    state: [u64; 4],
}

impl Xoshiro256StarStar {
    pub(super) fn new(seed: u64) -> Self {
        let mut sm = SplitMix64::new(seed);
        Self {
            state: [sm.next_u64(), sm.next_u64(), sm.next_u64(), sm.next_u64()],
        }
    }

    #[inline]
    pub(super) fn next_u64(&mut self) -> u64 {
        let result = self.state[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.state[1] << 17;
        self.state[2] ^= self.state[0];
        self.state[3] ^= self.state[1];
        self.state[1] ^= self.state[2];
        self.state[0] ^= self.state[3];
        self.state[2] ^= t;
        self.state[3] = self.state[3].rotate_left(45);
        result
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DitherRng {
    XorShift64(XorShift64),
    Xoshiro256StarStar(Xoshiro256StarStar),
    SplitMix64(SplitMix64),
}

impl DitherRng {
    pub(super) fn new(seed: u64, prng: DitherPrng) -> Self {
        match prng {
            DitherPrng::XorShift64 => Self::XorShift64(XorShift64::new(seed)),
            DitherPrng::Xoshiro256StarStar => {
                Self::Xoshiro256StarStar(Xoshiro256StarStar::new(seed))
            }
            DitherPrng::SplitMix64 => Self::SplitMix64(SplitMix64::new(seed)),
        }
    }

    #[inline]
    pub(super) fn next_u64(&mut self) -> u64 {
        match self {
            Self::XorShift64(rng) => rng.next_u64(),
            Self::Xoshiro256StarStar(rng) => rng.next_u64(),
            Self::SplitMix64(rng) => rng.next_u64(),
        }
    }

    #[inline]
    pub(super) fn next_uniform_half(&mut self) -> f64 {
        let bits = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        bits - 0.5
    }

    #[inline]
    pub(super) fn next_tpdf(&mut self) -> f64 {
        self.next_uniform_half() + self.next_uniform_half()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct CommonSideDitherState {
    common_seed: u64,
    side_seed: u64,
    common_rng: DitherRng,
    side_rng: DitherRng,
    common_prev_tpdf: f64,
    side_prev_tpdf: f64,
    beta: f64,
    side_sign: f64,
    inv_norm: f64,
}

impl CommonSideDitherState {
    pub(super) fn new(
        common_seed: u64,
        side_seed: u64,
        prng: DitherPrng,
        beta: f64,
        side_sign: f64,
    ) -> Option<Self> {
        if !beta.is_finite() || beta < 0.0 || !side_sign.is_finite() {
            return None;
        }
        let side_sign = if side_sign < 0.0 { -1.0 } else { 1.0 };
        Some(Self {
            common_seed,
            side_seed,
            common_rng: DitherRng::new(common_seed, prng),
            side_rng: DitherRng::new(side_seed, prng),
            common_prev_tpdf: 0.0,
            side_prev_tpdf: 0.0,
            beta,
            side_sign,
            inv_norm: (1.0 / (1.0 + beta * beta)).sqrt(),
        })
    }

    pub(super) fn reset_history(&mut self) {
        self.common_prev_tpdf = 0.0;
        self.side_prev_tpdf = 0.0;
    }

    pub(super) fn reseed(&mut self, prng: DitherPrng) {
        self.common_rng = DitherRng::new(self.common_seed, prng);
        self.side_rng = DitherRng::new(self.side_seed, prng);
        self.reset_history();
    }

    #[inline(always)]
    pub(super) fn next(
        &mut self,
        shape: DitherShape,
        leak_alpha: f64,
        lf_floor_gamma: f64,
        high_pass_norm: f64,
    ) -> f64 {
        let common = next_dither_from(
            &mut self.common_rng,
            shape,
            &mut self.common_prev_tpdf,
            leak_alpha,
            lf_floor_gamma,
            high_pass_norm,
        );
        if self.beta == 0.0 {
            return common;
        }
        let side = next_dither_from(
            &mut self.side_rng,
            shape,
            &mut self.side_prev_tpdf,
            leak_alpha,
            lf_floor_gamma,
            high_pass_norm,
        );
        self.inv_norm * (common + self.side_sign * self.beta * side)
    }
}

#[inline]
pub(super) fn splitmix64_mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

#[inline(always)]
pub(super) fn next_white_tpdf_from(rng: &mut DitherRng) -> f64 {
    rng.next_tpdf()
}

/// First-difference (leaky) high-pass TPDF, normalized to the variance of
/// white TPDF at the same scale: the raw difference has variance
/// `(1 + α² + γ²)/6`, so dividing by `sqrt(1 + α² + γ²)` restores `1/6` for
/// every leak/floor setting. Shape comparisons at a fixed `dither_scale` are
/// therefore power-matched and differ in spectrum only.
#[inline(always)]
pub(super) fn next_high_pass_tpdf_from(
    rng: &mut DitherRng,
    prev_tpdf: &mut f64,
    leak_alpha: f64,
    lf_floor_gamma: f64,
    norm: f64,
) -> f64 {
    let tpdf = rng.next_tpdf();
    let floor = if lf_floor_gamma == 0.0 {
        0.0
    } else {
        lf_floor_gamma * rng.next_tpdf()
    };
    let high_pass_tpdf = norm * (tpdf - leak_alpha * *prev_tpdf + floor);
    *prev_tpdf = tpdf;
    high_pass_tpdf
}

#[inline(always)]
pub(super) fn high_pass_tpdf_norm(leak_alpha: f64, lf_floor_gamma: f64) -> f64 {
    (1.0 / (1.0 + leak_alpha * leak_alpha + lf_floor_gamma * lf_floor_gamma)).sqrt()
}

#[inline(always)]
pub(super) fn next_dither_from(
    rng: &mut DitherRng,
    shape: DitherShape,
    prev_tpdf: &mut f64,
    leak_alpha: f64,
    lf_floor_gamma: f64,
    high_pass_norm: f64,
) -> f64 {
    match shape {
        DitherShape::WhiteTpdf => next_white_tpdf_from(rng),
        DitherShape::HighPassTpdf => {
            next_high_pass_tpdf_from(rng, prev_tpdf, leak_alpha, lf_floor_gamma, high_pass_norm)
        }
    }
}
