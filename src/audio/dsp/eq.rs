//! Parametric EQ — 10 band SVF cascade per channel, with preamp.
//!
//! Filters use Andrew Simper's Trapezoidal-integrated State Variable Filter
//! (Cytomic). Compared to a DF2T biquad cascade this is more numerically
//! well-behaved at low cutoffs running at high sample rates — exactly the
//! regime hit when the upsampler pushes the EQ stage to 192/384 kHz, where
//! a biquad's poles sit very close to the unit circle. Coefficient ramps on
//! parameter changes still avoid zipper noise.
//!
//! References:
//!   Simper, "Linear Trapezoidal Integrated State Variable Filter"
//!   <https://cytomic.com/files/dsp/SvfLinearTrapOptimised2.pdf>
//!   Filter-type → mix-coefficient mappings follow that paper.

use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

pub const NUM_BANDS: usize = 10;
const SMOOTHING_TIME_MS: f64 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum BandType {
    #[default]
    Peaking,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    Notch,
    AllPass,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EqBand {
    pub enabled: bool,
    #[serde(rename = "type")]
    pub band_type: BandType,
    pub freq_hz: f32,
    pub gain_db: f32,
    pub q: f32,
}

impl EqBand {
    pub fn default_at(freq_hz: f32) -> Self {
        Self {
            enabled: false,
            band_type: BandType::Peaking,
            freq_hz,
            gain_db: 0.0,
            q: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EqConfig {
    pub enabled: bool,
    pub preamp_db: f32,
    pub bands: [EqBand; NUM_BANDS],
}

impl Default for EqConfig {
    fn default() -> Self {
        // Logarithmically-spaced default centers covering 20 Hz – 20 kHz.
        let default_centers: [f32; NUM_BANDS] = [
            31.0, 62.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
        ];
        let mut bands = [EqBand::default_at(1000.0); NUM_BANDS];
        for (i, center) in default_centers.iter().enumerate() {
            bands[i] = EqBand::default_at(*center);
        }
        // The first and last band default to shelving filters to feel Roon-like.
        bands[0].band_type = BandType::LowShelf;
        bands[NUM_BANDS - 1].band_type = BandType::HighShelf;

        Self {
            enabled: false,
            preamp_db: 0.0,
            bands,
        }
    }
}

/// Trapezoidal SVF coefficients — three integrator-step coefficients (a1/a2/a3
/// derived from the prewarped cutoff `g` and damping `k`) plus the three output
/// mix coefficients (m0/m1/m2) that select filter type (LP, HP, shelf, etc.).
/// Stored in f64 because both the state variables and the mix sum accumulate
/// recursive error that's audible in f32 at low cutoffs.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SvfCoeffs {
    a1: f64,
    a2: f64,
    a3: f64,
    m0: f64,
    m1: f64,
    m2: f64,
}

impl Default for SvfCoeffs {
    fn default() -> Self {
        Self::identity()
    }
}

impl SvfCoeffs {
    /// Pass-through (y = x) regardless of state — `g = 0` freezes the
    /// integrators and `m0 = 1, m1 = m2 = 0` selects the direct input.
    fn identity() -> Self {
        Self {
            a1: 1.0,
            a2: 0.0,
            a3: 0.0,
            m0: 1.0,
            m1: 0.0,
            m2: 0.0,
        }
    }

    fn step_toward(&mut self, target: Self, remaining_samples: usize) {
        let t = 1.0 / remaining_samples as f64;
        self.a1 += (target.a1 - self.a1) * t;
        self.a2 += (target.a2 - self.a2) * t;
        self.a3 += (target.a3 - self.a3) * t;
        self.m0 += (target.m0 - self.m0) * t;
        self.m1 += (target.m1 - self.m1) * t;
        self.m2 += (target.m2 - self.m2) * t;
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct SvfState {
    ic1eq: f64,
    ic2eq: f64,
}

impl SvfState {
    #[inline(always)]
    fn process(&mut self, x: f64, c: &SvfCoeffs) -> f64 {
        let v3 = x - self.ic2eq;
        let v1 = c.a1 * self.ic1eq + c.a2 * v3;
        let v2 = self.ic2eq + c.a2 * self.ic1eq + c.a3 * v3;
        self.ic1eq = 2.0 * v1 - self.ic1eq;
        self.ic2eq = 2.0 * v2 - self.ic2eq;
        c.m0 * x + c.m1 * v1 + c.m2 * v2
    }

    #[inline(always)]
    fn is_finite(&self) -> bool {
        self.ic1eq.is_finite() && self.ic2eq.is_finite()
    }
}

/// Stereo 10-band EQ processor. Holds per-channel filter state and per-band coefficients.
/// All filter math and sample buffers run in f64 until the final device-format boundary.
pub struct EqProcessor {
    coeffs: [SvfCoeffs; NUM_BANDS],
    target_coeffs: [SvfCoeffs; NUM_BANDS],
    active_flags: [bool; NUM_BANDS],
    target_active_flags: [bool; NUM_BANDS],
    state_l: [SvfState; NUM_BANDS],
    state_r: [SvfState; NUM_BANDS],
    preamp_linear: f64,
    target_preamp_linear: f64,
    processing_active: bool,
    ramp_remaining: usize,
    sample_rate: u32,
}

impl EqProcessor {
    pub fn new(sample_rate: u32, config: &EqConfig) -> Self {
        let mut proc = Self {
            coeffs: [SvfCoeffs::default(); NUM_BANDS],
            target_coeffs: [SvfCoeffs::default(); NUM_BANDS],
            active_flags: [false; NUM_BANDS],
            target_active_flags: [false; NUM_BANDS],
            state_l: [SvfState::default(); NUM_BANDS],
            state_r: [SvfState::default(); NUM_BANDS],
            preamp_linear: 1.0,
            target_preamp_linear: 1.0,
            processing_active: false,
            ramp_remaining: 0,
            sample_rate,
        };
        proc.apply_config(sample_rate, config, false);
        proc
    }

    /// Recompute target coefficients from the config.
    ///
    /// Runtime parameter changes are smoothed by linearly ramping the active coefficients and
    /// preamp gain toward their new targets. Filter state is preserved during the ramp so knob
    /// moves do not produce hard coefficient steps.
    pub fn update(&mut self, sample_rate: u32, config: &EqConfig) {
        let smooth = sample_rate == self.sample_rate;
        self.apply_config(sample_rate, config, smooth);
    }

    fn apply_config(&mut self, sample_rate: u32, config: &EqConfig, smooth: bool) {
        self.sample_rate = sample_rate;
        let target_preamp = if config.enabled {
            db_to_linear(config.preamp_db)
        } else {
            1.0
        };

        let mut target_coeffs = [SvfCoeffs::identity(); NUM_BANDS];
        let mut target_active_flags = [false; NUM_BANDS];
        for (i, band) in config.bands.iter().enumerate() {
            if config.enabled && band.enabled {
                target_active_flags[i] = true;
                target_coeffs[i] = compute_svf(sample_rate, band);
            }
        }

        let changed = self.target_preamp_linear != target_preamp
            || self.target_coeffs != target_coeffs
            || self.target_active_flags != target_active_flags;

        self.target_preamp_linear = target_preamp;
        self.target_coeffs = target_coeffs;
        self.target_active_flags = target_active_flags;

        if !smooth {
            self.preamp_linear = self.target_preamp_linear;
            self.coeffs = self.target_coeffs;
            self.active_flags = self.target_active_flags;
            self.ramp_remaining = 0;
            self.reset_inactive_band_state();
        } else if changed {
            for i in 0..NUM_BANDS {
                self.active_flags[i] |= self.target_active_flags[i];
            }
            self.ramp_remaining = smoothing_samples(sample_rate);
        }
        self.processing_active = self.should_process();
    }

    /// Reset filter state — useful on seek/track-change to avoid ringing artifacts.
    pub fn reset(&mut self) {
        for s in self.state_l.iter_mut() {
            *s = SvfState::default();
        }
        for s in self.state_r.iter_mut() {
            *s = SvfState::default();
        }
    }

    fn should_process(&self) -> bool {
        self.ramp_remaining > 0
            || self.active_flags.iter().any(|enabled| *enabled)
            || (self.preamp_linear - 1.0).abs() > f64::EPSILON
            || (self.target_preamp_linear - 1.0).abs() > f64::EPSILON
    }

    pub fn is_processing_active(&self) -> bool {
        self.processing_active
    }

    fn advance_ramp(&mut self) {
        if self.ramp_remaining == 0 {
            return;
        }

        let remaining = self.ramp_remaining;
        self.preamp_linear += (self.target_preamp_linear - self.preamp_linear) / remaining as f64;
        for i in 0..NUM_BANDS {
            if self.active_flags[i] || self.target_active_flags[i] {
                self.coeffs[i].step_toward(self.target_coeffs[i], remaining);
            }
        }
        self.ramp_remaining -= 1;

        if self.ramp_remaining == 0 {
            self.preamp_linear = self.target_preamp_linear;
            self.coeffs = self.target_coeffs;
            self.active_flags = self.target_active_flags;
            self.reset_inactive_band_state();
            self.processing_active = self.should_process();
        }
    }

    fn reset_inactive_band_state(&mut self) {
        for i in 0..NUM_BANDS {
            if !self.active_flags[i] {
                self.state_l[i] = SvfState::default();
                self.state_r[i] = SvfState::default();
            }
        }
    }

    /// Process an interleaved stereo buffer in-place.
    ///
    /// Any non-finite output is forced to zero and the offending band state is reset. That keeps
    /// invalid values away from DAC drivers and lets playback recover on the next sample instead
    /// of letting NaN state poison the cascade indefinitely.
    pub fn process_interleaved_stereo(&mut self, samples: &mut [f64]) {
        if !self.processing_active {
            return;
        }
        for frame in samples.chunks_exact_mut(2) {
            let (l, r) = self.process_frame(frame[0], frame[1]);
            frame[0] = l;
            frame[1] = r;
        }
    }

    pub fn process_planar_stereo(&mut self, samples_l: &mut [f64], samples_r: &mut [f64]) {
        if !self.processing_active {
            return;
        }
        for (l_sample, r_sample) in samples_l.iter_mut().zip(samples_r.iter_mut()) {
            let (l, r) = self.process_frame(*l_sample, *r_sample);
            *l_sample = l;
            *r_sample = r;
        }
    }

    fn process_frame(&mut self, left: f64, right: f64) -> (f64, f64) {
        self.advance_ramp();

        let mut l = left * self.preamp_linear;
        let mut r = right * self.preamp_linear;
        if !l.is_finite() {
            l = 0.0;
        }
        if !r.is_finite() {
            r = 0.0;
        }

        for i in 0..NUM_BANDS {
            if self.active_flags[i] {
                l = self.state_l[i].process(l, &self.coeffs[i]);
                r = self.state_r[i].process(r, &self.coeffs[i]);

                if !l.is_finite() || !self.state_l[i].is_finite() {
                    self.state_l[i] = SvfState::default();
                    l = 0.0;
                }
                if !r.is_finite() || !self.state_r[i].is_finite() {
                    self.state_r[i] = SvfState::default();
                    r = 0.0;
                }
            }
        }

        (
            if l.is_finite() { l } else { 0.0 },
            if r.is_finite() { r } else { 0.0 },
        )
    }
}

#[inline]
fn db_to_linear(db: f32) -> f64 {
    10.0f64.powf(db as f64 / 20.0)
}

/// Continuous-time SVF parameters before integrator-coefficient derivation: the
/// prewarped cutoff `g = tan(π·f/fs)`, the damping `k = 1/Q`, and the output
/// mix `m0/m1/m2` that selects filter type. Sharing this between `compute_svf`
/// and `band_magnitude_db` keeps the audio path and the analytical response in
/// lockstep.
struct SvfParams {
    g: f64,
    k: f64,
    m0: f64,
    m1: f64,
    m2: f64,
}

fn compute_svf_params(sample_rate: u32, band: &EqBand) -> Option<SvfParams> {
    if sample_rate == 0 {
        return None;
    }

    let fs = sample_rate as f64;
    let f0 = (band.freq_hz as f64).clamp(10.0, fs * 0.49);
    let q = (band.q as f64).max(0.01);
    let gain_db = band.gain_db as f64;
    let a_amp = 10.0f64.powf(gain_db / 40.0); // sqrt of linear gain

    let mut g = (PI * f0 / fs).tan();
    let k;
    let m0;
    let m1;
    let m2;

    match band.band_type {
        BandType::Peaking => {
            // Constant-Q bell: damping is scaled by A so the -3 dB bandwidth
            // (relative to the boost peak) is set by Q regardless of gain.
            k = 1.0 / (q * a_amp);
            m0 = 1.0;
            m1 = k * (a_amp * a_amp - 1.0);
            m2 = 0.0;
        }
        BandType::LowShelf => {
            // Shifting g by 1/sqrt(A) places the half-gain point at f0,
            // matching RBJ shelf semantics.
            g /= a_amp.sqrt();
            k = 1.0 / q;
            m0 = 1.0;
            m1 = k * (a_amp - 1.0);
            m2 = a_amp * a_amp - 1.0;
        }
        BandType::HighShelf => {
            g *= a_amp.sqrt();
            k = 1.0 / q;
            m0 = a_amp * a_amp;
            m1 = k * (1.0 - a_amp) * a_amp;
            m2 = 1.0 - a_amp * a_amp;
        }
        BandType::LowPass => {
            k = 1.0 / q;
            m0 = 0.0;
            m1 = 0.0;
            m2 = 1.0;
        }
        BandType::HighPass => {
            k = 1.0 / q;
            m0 = 1.0;
            m1 = -k;
            m2 = -1.0;
        }
        BandType::Notch => {
            k = 1.0 / q;
            m0 = 1.0;
            m1 = -k;
            m2 = 0.0;
        }
        BandType::AllPass => {
            k = 1.0 / q;
            m0 = 1.0;
            m1 = -2.0 * k;
            m2 = 0.0;
        }
    }

    Some(SvfParams { g, k, m0, m1, m2 })
}

fn compute_svf(sample_rate: u32, band: &EqBand) -> SvfCoeffs {
    let Some(p) = compute_svf_params(sample_rate, band) else {
        return SvfCoeffs::identity();
    };
    let a1 = 1.0 / (1.0 + p.g * (p.g + p.k));
    let a2 = p.g * a1;
    let a3 = p.g * a2;
    SvfCoeffs {
        a1,
        a2,
        a3,
        m0: p.m0,
        m1: p.m1,
        m2: p.m2,
    }
}

fn smoothing_samples(sample_rate: u32) -> usize {
    ((sample_rate as f64 * SMOOTHING_TIME_MS / 1000.0).round() as usize).max(1)
}

/// Evaluate the magnitude response of a band at a given frequency (Hz).
/// Exposed for tests / parity checks with the JS-side curve drawing.
///
/// The closed form follows directly from the bilinear-prewarped SVF:
/// with `G = tan(π·f/fs) / g` the response is
///   `H(jω) = m0 + (m1·jG + m2) / ((1 − G²) + j·k·G)`.
// Retained for curve parity checks even when no Rust-side caller is compiled.
#[allow(dead_code)]
pub fn band_magnitude_db(sample_rate: u32, band: &EqBand, freq_hz: f32) -> f32 {
    if !band.enabled {
        return 0.0;
    }
    let Some(p) = compute_svf_params(sample_rate, band) else {
        return 0.0;
    };
    if p.g.abs() < 1e-30 {
        return 0.0;
    }

    let fs = sample_rate as f64;
    let test = (PI * freq_hz as f64 / fs).tan();
    let big_g = test / p.g;

    let denom_re = 1.0 - big_g * big_g;
    let denom_im = p.k * big_g;
    let num_re = p.m0 * denom_re + p.m2;
    let num_im = p.m0 * denom_im + p.m1 * big_g;

    let num_mag = (num_re * num_re + num_im * num_im).sqrt();
    let den_mag = (denom_re * denom_re + denom_im * denom_im).sqrt();
    if den_mag < 1e-12 {
        return 0.0;
    }
    (20.0 * (num_mag / den_mag).log10()) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_peaking_config(gain_db: f32) -> EqConfig {
        let mut config = EqConfig {
            enabled: true,
            ..EqConfig::default()
        };
        config.bands[0] = EqBand {
            enabled: true,
            band_type: BandType::Peaking,
            freq_hz: 100.0,
            gain_db,
            q: 1.0,
        };
        config
    }

    #[test]
    fn update_ramps_coefficients_instead_of_swapping_immediately() {
        let sample_rate = 1_000;
        let mut proc = EqProcessor::new(sample_rate, &enabled_peaking_config(0.0));

        proc.update(sample_rate, &enabled_peaking_config(12.0));

        assert_eq!(proc.ramp_remaining, smoothing_samples(sample_rate));
        assert_ne!(proc.coeffs[0], proc.target_coeffs[0]);

        let mut one_frame = [0.25, -0.25];
        proc.process_interleaved_stereo(&mut one_frame);
        assert_eq!(proc.ramp_remaining, smoothing_samples(sample_rate) - 1);
        assert_ne!(proc.coeffs[0], proc.target_coeffs[0]);

        let mut rest = vec![0.0; proc.ramp_remaining * 2];
        proc.process_interleaved_stereo(&mut rest);
        assert_eq!(proc.ramp_remaining, 0);
        assert_eq!(proc.coeffs[0], proc.target_coeffs[0]);
    }

    #[test]
    fn disabling_eq_ramps_back_to_bypass() {
        let sample_rate = 1_000;
        let mut proc = EqProcessor::new(sample_rate, &enabled_peaking_config(6.0));

        proc.update(sample_rate, &EqConfig::default());

        assert!(proc.processing_active);
        assert!(proc.active_flags[0]);
        assert_eq!(proc.target_coeffs[0], SvfCoeffs::identity());

        let mut buffer = vec![0.0; smoothing_samples(sample_rate) * 2];
        proc.process_interleaved_stereo(&mut buffer);

        assert!(!proc.processing_active);
        assert!(!proc.active_flags[0]);
        assert_eq!(proc.coeffs[0], SvfCoeffs::identity());
    }

    #[test]
    fn duplicate_update_does_not_snap_active_ramp_to_target() {
        let sample_rate = 1_000;
        let target = enabled_peaking_config(12.0);
        let mut proc = EqProcessor::new(sample_rate, &enabled_peaking_config(0.0));
        proc.update(sample_rate, &target);

        let mut one_frame = [0.25, -0.25];
        proc.process_interleaved_stereo(&mut one_frame);
        let ramp_remaining = proc.ramp_remaining;
        let coeff_after_one_frame = proc.coeffs[0];

        proc.update(sample_rate, &target);

        assert_eq!(proc.ramp_remaining, ramp_remaining);
        assert_eq!(proc.coeffs[0], coeff_after_one_frame);
        assert_ne!(proc.coeffs[0], proc.target_coeffs[0]);
    }

    #[test]
    fn non_finite_band_state_is_reset() {
        let mut proc = EqProcessor::new(48_000, &enabled_peaking_config(6.0));
        proc.state_l[0].ic1eq = f64::NAN;
        proc.state_r[0].ic2eq = f64::INFINITY;

        let mut frame = [1.0, 1.0];
        proc.process_interleaved_stereo(&mut frame);

        assert!(frame[0].is_finite());
        assert!(frame[1].is_finite());
        assert!(proc.state_l[0].is_finite());
        assert!(proc.state_r[0].is_finite());
    }

    /// Sanity-check the closed-form SVF magnitude against RBJ targets for the
    /// filter types where the two formulations should agree analytically.
    #[test]
    fn magnitude_response_matches_known_targets() {
        let fs = 192_000u32;

        let peak = EqBand {
            enabled: true,
            band_type: BandType::Peaking,
            freq_hz: 1_000.0,
            gain_db: 6.0,
            q: 1.0,
        };
        let peak_db = band_magnitude_db(fs, &peak, peak.freq_hz);
        assert!(
            (peak_db - 6.0).abs() < 0.05,
            "peak band gain at center should be ~+6 dB, got {peak_db}"
        );

        let low_shelf = EqBand {
            enabled: true,
            band_type: BandType::LowShelf,
            freq_hz: 100.0,
            gain_db: 8.0,
            q: 1.0 / std::f32::consts::SQRT_2,
        };
        let dc = band_magnitude_db(fs, &low_shelf, 1.0);
        assert!(
            (dc - 8.0).abs() < 0.05,
            "low shelf should asymptote to +8 dB at DC, got {dc}"
        );
        let above = band_magnitude_db(fs, &low_shelf, 10_000.0);
        assert!(
            above.abs() < 0.05,
            "low shelf should asymptote to 0 dB well above f0, got {above}"
        );
        let knee = band_magnitude_db(fs, &low_shelf, low_shelf.freq_hz);
        assert!(
            (knee - 4.0).abs() < 0.05,
            "low shelf half-gain should sit at f0, got {knee}"
        );

        let high_shelf = EqBand {
            enabled: true,
            band_type: BandType::HighShelf,
            freq_hz: 8_000.0,
            gain_db: -6.0,
            q: 1.0 / std::f32::consts::SQRT_2,
        };
        let very_high = band_magnitude_db(fs, &high_shelf, 60_000.0);
        assert!(
            (very_high - (-6.0)).abs() < 0.1,
            "high shelf should asymptote to -6 dB above f0, got {very_high}"
        );
        let very_low = band_magnitude_db(fs, &high_shelf, 50.0);
        assert!(
            very_low.abs() < 0.05,
            "high shelf should asymptote to 0 dB at DC, got {very_low}"
        );
    }

    /// Regression check for the original motivation: a low-Q, low-frequency
    /// peak at a high sample rate. The biquad version of this filter has poles
    /// extremely close to the unit circle; the SVF should produce a clean
    /// finite output with no state explosion when driven by a normal signal.
    #[test]
    fn low_freq_band_stays_stable_at_high_sample_rate() {
        let fs = 384_000u32;
        let mut config = EqConfig {
            enabled: true,
            ..EqConfig::default()
        };
        config.bands[0] = EqBand {
            enabled: true,
            band_type: BandType::LowShelf,
            freq_hz: 25.0,
            gain_db: 6.0,
            q: 0.7,
        };
        let mut proc = EqProcessor::new(fs, &config);

        let mut buf = vec![0.0; (fs as usize) * 2 / 10]; // 100 ms stereo
        for (i, frame) in buf.chunks_exact_mut(2).enumerate() {
            let t = i as f64 / fs as f64;
            let s = (2.0 * std::f64::consts::PI * 50.0 * t).sin() * 0.5;
            frame[0] = s;
            frame[1] = s;
        }
        proc.process_interleaved_stereo(&mut buf);
        let peak = buf.iter().cloned().fold(0.0f64, |a, b| a.max(b.abs()));
        assert!(
            peak.is_finite() && peak < 4.0,
            "unstable output peak {peak}"
        );
        assert!(proc.state_l[0].is_finite() && proc.state_r[0].is_finite());
    }
}
