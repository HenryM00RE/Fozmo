#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DitherMode {
    Off,
    Tpdf,
    /// TPDF dither with mild 2nd-order error-feedback noise shaping
    /// (noise transfer function `(1 - z^-1)^2`). Intended for 16-bit
    /// endpoint conversions only; other bit depths fall back to flat TPDF.
    Shaped16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DitherPreference {
    Auto,
    Off,
    Tpdf,
}

impl DitherPreference {
    pub fn as_id(self) -> u32 {
        match self {
            DitherPreference::Auto => 0,
            DitherPreference::Off => 1,
            DitherPreference::Tpdf => 2,
        }
    }

    pub fn from_id(id: u32) -> Option<Self> {
        match id {
            0 => Some(DitherPreference::Auto),
            1 => Some(DitherPreference::Off),
            2 => Some(DitherPreference::Tpdf),
            _ => None,
        }
    }

    pub fn as_name(self) -> &'static str {
        match self {
            DitherPreference::Auto => "Auto",
            DitherPreference::Off => "Off",
            DitherPreference::Tpdf => "TPDF",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "Auto" => Some(DitherPreference::Auto),
            "Off" => Some(DitherPreference::Off),
            "TPDF" | "Tpdf" => Some(DitherPreference::Tpdf),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DitherState {
    rng_l: u64,
    rng_r: u64,
    shape_err_l: [f64; 2],
    shape_err_r: [f64; 2],
}

impl DitherState {
    pub fn new(seed: u64) -> Self {
        let seed = if seed == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seed
        };
        Self {
            rng_l: splitmix64(seed),
            rng_r: splitmix64(seed ^ 0xd1b5_4a32_d192_ed03),
            shape_err_l: [0.0; 2],
            shape_err_r: [0.0; 2],
        }
    }

    #[cfg(test)]
    fn seeded_for_test() -> Self {
        Self::new(0x1234_5678_9abc_def0)
    }

    fn rand01(&mut self, channel: usize) -> f64 {
        let state = if channel.is_multiple_of(2) {
            &mut self.rng_l
        } else {
            &mut self.rng_r
        };
        *state = splitmix64(*state);
        let mantissa = *state >> 11;
        mantissa as f64 * (1.0 / ((1u64 << 53) as f64))
    }

    fn tpdf_lsb(&mut self, channel: usize) -> f64 {
        self.rand01(channel) - self.rand01(channel)
    }

    /// Quantize `target` (in LSB units) with TPDF dither and 2nd-order
    /// error feedback shaping the total quantization noise by
    /// `(1 - z^-1)^2`. The fed-back error is taken before the caller's
    /// range clamp so the state stays bounded (|err| <= 1.5 LSB) even
    /// during sustained full-scale input.
    fn shaped16_round(&mut self, target: f64, channel: usize) -> f64 {
        let dither = self.tpdf_lsb(channel);
        let errors = if channel.is_multiple_of(2) {
            &mut self.shape_err_l
        } else {
            &mut self.shape_err_r
        };
        let shaped = target - 2.0 * errors[0] + errors[1];
        let rounded = (shaped + dither).round();
        errors[1] = errors[0];
        errors[0] = rounded - shaped;
        rounded
    }
}

pub fn quantize_signed_pcm(
    sample: f64,
    bits: usize,
    channel: usize,
    state: &mut DitherState,
    mode: DitherMode,
) -> i32 {
    debug_assert!((1..=32).contains(&bits));
    let full_scale = (1i64 << (bits - 1)) as f64;
    let min_code = -(1i64 << (bits - 1));
    let max_code = (1i64 << (bits - 1)) - 1;
    let finite_sample = if sample.is_finite() { sample } else { 0.0 };
    let rounded = match mode {
        DitherMode::Off => (finite_sample * full_scale).round(),
        DitherMode::Tpdf => (finite_sample * full_scale + state.tpdf_lsb(channel)).round(),
        DitherMode::Shaped16 if bits == 16 => {
            state.shaped16_round(finite_sample * full_scale, channel)
        }
        DitherMode::Shaped16 => (finite_sample * full_scale + state.tpdf_lsb(channel)).round(),
    };
    rounded.clamp(min_code as f64, max_code as f64) as i32
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scale_uses_asymmetric_pcm_range() {
        let mut dither = DitherState::seeded_for_test();

        assert_eq!(
            quantize_signed_pcm(-1.0, 16, 0, &mut dither, DitherMode::Off),
            -32768
        );
        assert_eq!(
            quantize_signed_pcm(1.0, 16, 0, &mut dither, DitherMode::Off),
            32767
        );
        assert_eq!(
            quantize_signed_pcm(-1.0, 24, 0, &mut dither, DitherMode::Off),
            -8_388_608
        );
        assert_eq!(
            quantize_signed_pcm(1.0, 24, 0, &mut dither, DitherMode::Off),
            8_388_607
        );
    }

    #[test]
    fn tpdf_is_in_integer_lsb_units() {
        let mut dither = DitherState::seeded_for_test();
        let mut nonzero = 0;
        for _ in 0..4096 {
            let code = quantize_signed_pcm(0.0, 16, 0, &mut dither, DitherMode::Tpdf);
            assert!((-1..=1).contains(&code), "silence dither code was {code}");
            if code != 0 {
                nonzero += 1;
            }
        }
        assert!(
            nonzero > 200,
            "TPDF appears too small; silence rarely crossed an LSB: {nonzero}"
        );
    }

    #[test]
    fn sub_lsb_signal_is_not_collapsed_by_tpdf() {
        let mut off = DitherState::seeded_for_test();
        let mut tpdf = DitherState::seeded_for_test();
        let mut off_nonzero = 0;
        let mut tpdf_nonzero = 0;

        for n in 0..4096 {
            let phase = 2.0 * std::f64::consts::PI * 997.0 * n as f64 / 44_100.0;
            let sample = 10f64.powf(-100.0 / 20.0) * phase.sin();
            if quantize_signed_pcm(sample, 16, 0, &mut off, DitherMode::Off) != 0 {
                off_nonzero += 1;
            }
            if quantize_signed_pcm(sample, 16, 0, &mut tpdf, DitherMode::Tpdf) != 0 {
                tpdf_nonzero += 1;
            }
        }

        assert_eq!(off_nonzero, 0);
        assert!(
            tpdf_nonzero > 200,
            "TPDF should make sub-LSB energy noise-like rather than silently zero: {tpdf_nonzero}"
        );
    }

    #[test]
    fn dither_and_rounding_never_wrap_full_scale() {
        let mut dither = DitherState::seeded_for_test();
        for _ in 0..4096 {
            let positive = quantize_signed_pcm(1.0, 16, 0, &mut dither, DitherMode::Tpdf);
            let negative = quantize_signed_pcm(-1.0, 16, 1, &mut dither, DitherMode::Tpdf);
            assert!((..=32767).contains(&positive));
            assert!((-32768..).contains(&negative));
        }
    }

    #[test]
    fn integer_valued_samples_stay_within_one_lsb() {
        let mut dither = DitherState::seeded_for_test();
        let target_code = 1234;
        let sample = target_code as f64 / 32768.0;

        for _ in 0..4096 {
            let code = quantize_signed_pcm(sample, 16, 0, &mut dither, DitherMode::Tpdf);
            assert!(
                (target_code - 1..=target_code + 1).contains(&code),
                "dithered integer-valued sample moved too far: {code}"
            );
        }
    }

    #[test]
    fn tpdf_has_near_zero_mean() {
        let mut dither = DitherState::seeded_for_test();
        let mut sum = 0i64;
        let frames = 100_000;

        for _ in 0..frames {
            sum += quantize_signed_pcm(0.0, 16, 0, &mut dither, DitherMode::Tpdf) as i64;
        }

        let mean = sum as f64 / frames as f64;
        assert!(mean.abs() < 0.02, "dither mean drifted: {mean}");
    }

    #[test]
    fn shaped_silence_stays_bounded() {
        let mut dither = DitherState::seeded_for_test();
        let mut nonzero = 0;
        for _ in 0..8192 {
            let code = quantize_signed_pcm(0.0, 16, 0, &mut dither, DitherMode::Shaped16);
            assert!(
                (-8..=8).contains(&code),
                "shaped silence code out of bounds: {code}"
            );
            if code != 0 {
                nonzero += 1;
            }
        }
        assert!(
            nonzero > 200,
            "shaped dither appears too small on silence: {nonzero}"
        );
    }

    #[test]
    fn shaped_has_near_zero_mean() {
        let mut dither = DitherState::seeded_for_test();
        let mut sum = 0i64;
        let frames = 100_000;

        for _ in 0..frames {
            sum += quantize_signed_pcm(0.0, 16, 0, &mut dither, DitherMode::Shaped16) as i64;
        }

        let mean = sum as f64 / frames as f64;
        assert!(mean.abs() < 0.03, "shaped dither mean drifted: {mean}");
    }

    #[test]
    fn shaped_is_deterministic_for_a_given_seed() {
        let mut a = DitherState::new(0xfeed_beef);
        let mut b = DitherState::new(0xfeed_beef);
        for n in 0..4096 {
            let phase = 2.0 * std::f64::consts::PI * 441.0 * n as f64 / 44_100.0;
            let sample = 0.25 * phase.sin();
            let code_a = quantize_signed_pcm(sample, 16, n % 2, &mut a, DitherMode::Shaped16);
            let code_b = quantize_signed_pcm(sample, 16, n % 2, &mut b, DitherMode::Shaped16);
            assert_eq!(code_a, code_b, "shaped dither diverged at sample {n}");
        }
    }

    #[test]
    fn shaped_never_wraps_full_scale() {
        let mut dither = DitherState::seeded_for_test();
        for _ in 0..4096 {
            let positive = quantize_signed_pcm(1.0, 16, 0, &mut dither, DitherMode::Shaped16);
            let negative = quantize_signed_pcm(-1.0, 16, 1, &mut dither, DitherMode::Shaped16);
            assert!((..=32767).contains(&positive));
            assert!((-32768..).contains(&negative));
        }
    }

    #[test]
    fn shaped_falls_back_to_flat_tpdf_for_non_16_bit() {
        let mut shaped = DitherState::seeded_for_test();
        let mut tpdf = DitherState::seeded_for_test();
        for n in 0..1024 {
            let sample = (n as f64 / 1024.0) - 0.5;
            assert_eq!(
                quantize_signed_pcm(sample, 24, n % 2, &mut shaped, DitherMode::Shaped16),
                quantize_signed_pcm(sample, 24, n % 2, &mut tpdf, DitherMode::Tpdf),
            );
        }
    }

    #[test]
    fn shaped_keeps_low_level_signal_and_lowers_in_band_noise() {
        const FRAMES: usize = 8192;
        const SAMPLE_RATE: f64 = 44_100.0;

        // A -100 dBFS tone is far below one 16-bit LSB; it must survive as
        // noise-like output rather than collapsing to digital silence.
        let mut shaped = DitherState::seeded_for_test();
        let mut shaped_nonzero = 0;
        for n in 0..FRAMES {
            let phase = 2.0 * std::f64::consts::PI * 997.0 * n as f64 / SAMPLE_RATE;
            let sample = 10f64.powf(-100.0 / 20.0) * phase.sin();
            if quantize_signed_pcm(sample, 16, 0, &mut shaped, DitherMode::Shaped16) != 0 {
                shaped_nonzero += 1;
            }
        }
        assert!(
            shaped_nonzero > 200,
            "shaped dither collapsed a sub-LSB signal to silence: {shaped_nonzero}"
        );

        // Quantizing silence isolates the noise floor of each mode; compare
        // energy below 3 kHz where shaping should push noise out of band.
        let mut shaped = DitherState::seeded_for_test();
        let mut tpdf = DitherState::seeded_for_test();
        let shaped_noise: Vec<f64> = (0..FRAMES)
            .map(|_| quantize_signed_pcm(0.0, 16, 0, &mut shaped, DitherMode::Shaped16) as f64)
            .collect();
        let tpdf_noise: Vec<f64> = (0..FRAMES)
            .map(|_| quantize_signed_pcm(0.0, 16, 0, &mut tpdf, DitherMode::Tpdf) as f64)
            .collect();

        let shaped_band = band_energy(&shaped_noise, SAMPLE_RATE, 20.0, 3_000.0);
        let tpdf_band = band_energy(&tpdf_noise, SAMPLE_RATE, 20.0, 3_000.0);
        assert!(
            shaped_band < tpdf_band * 0.5,
            "shaped low/mid-band noise not below flat TPDF: shaped={shaped_band} tpdf={tpdf_band}"
        );
    }

    fn band_energy(samples: &[f64], sample_rate: f64, low_hz: f64, high_hz: f64) -> f64 {
        let len = samples.len();
        let bin_hz = sample_rate / len as f64;
        let mut energy = 0.0;
        let mut bin = (low_hz / bin_hz).ceil() as usize;
        while (bin as f64) * bin_hz <= high_hz {
            let omega = 2.0 * std::f64::consts::PI * bin as f64 / len as f64;
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (n, sample) in samples.iter().enumerate() {
                let angle = omega * n as f64;
                re += sample * angle.cos();
                im -= sample * angle.sin();
            }
            energy += re * re + im * im;
            bin += 1;
        }
        energy
    }
}
