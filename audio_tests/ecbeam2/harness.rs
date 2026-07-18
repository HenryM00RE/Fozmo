#![allow(dead_code)]

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::env;
use std::f64::consts::PI;
use std::fs;
use std::hint::black_box;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use fozmo::audio::dsd::delta_sigma::{
    AdaptiveDecisionTraceSnapshot, DitherPrng, DitherShape, DsdModulator, Ec2DecisionTraceSnapshot,
    Ec2LongFilterPolicy, Ec2PolicyWeights, EcBeam2DiagnosticWindow, EcBeam2ExperimentConfig,
    EcFutureScorer,
};
use fozmo::audio::dsd::dsd_coeffs::{ALL_VARIANTS, CALIBRATED, ModulatorCoeffs};
use fozmo::audio::dsd::dsd_render::{
    DSD64_EC_BEAM_A1_DEFAULT_EXPECTED_GAIN_DB, DSD64_EC_BEAM_A1_DEFAULT_INPUT_GAIN_DB,
    DSD128_EC4A_DITHER_SCALE_MULTIPLIER, DsdCommonSideDither, DsdExperimentTweaks, DsdRate,
    DsdRenderer, dsd_source_window_to_modulator_samples,
};
use fozmo::audio::dsd::native_dsd::NativeDsdOrder;
use fozmo::audio::dsp::dither::{DitherMode, DitherState, quantize_signed_pcm};
use fozmo::audio::dsp::resampler::{FilterType, SincResampler};
use realfft::RealFftPlanner;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CHUNK_FRAMES: usize = 1024;
const PCM_SECONDS_QUICK: f64 = 0.16;
const PCM_SECONDS_FULL: f64 = 0.8;
const DSD_SECONDS_QUICK: f64 = 0.04;
const DSD_SECONDS_FULL: f64 = 0.9;
const DSD_STABILITY_PRECHECK_SECONDS: f64 = 0.16;
const DSD_EC_DEPTH_SECONDS_FULL: f64 = DSD_SECONDS_FULL;
const DSD_EC4_SECONDS_FULL: f64 = DSD_EC_DEPTH_SECONDS_FULL;
const DSD_ANALYSIS_FFT_BITS_QUICK: usize = 1 << 16;
const DSD_ANALYSIS_FFT_BITS_FULL: usize = 1 << 21;
const DSD_TONE_WINDOW_FFT_BITS_FULL: usize = 1 << 20;
const DSD_ANALYSIS_SETTLE_SECONDS_FULL: f64 = 0.30;
const DSD_DISTRIBUTION_HOP_DIVISOR_FULL: usize = 4;
const DSD_SPUR_PEAK_HIGH_HZ: f64 = 19_000.0;
/// Carrier for the optional low-frequency SINAD probe (Workstream D). Chosen so
/// the fixed ~30 Hz SINAD guard band (`dsd_inband_tone_metrics_from_spectrum`)
/// lands at ~70–130 Hz — comfortably above the 20 Hz in-band floor at the
/// standard 2^20 tone FFT, so it needs no larger capture — while still sitting
/// well inside the DC-bias tracker's servo reach (≥ ~225 Hz legacy corner), so
/// it can see servo damage the 1 kHz SINAD tone misses.
const DSD_LF_SINAD_TONE_HZ: f64 = 100.0;
const DSD_INBAND_PREFILTER_PASS_HZ: f64 = 24_000.0;
const DSD_INBAND_PREFILTER_STOP_HZ: f64 = 32_000.0;
const DSD_INBAND_PREFILTER_EDGE_DISCARD_SECONDS: f64 = 0.050;
const DSD_TRIAGE_REFERENCE_LOWPASS_PASS_HZ: f64 = 19_000.0;
const DSD_TRIAGE_REFERENCE_LOWPASS_STOP_HZ: f64 = 20_500.0;
const DSD_TRIAGE_GAIN_GATE_DB: f64 = 1.0;
const DSD_TRIAGE_DECODED_PEAK_GATE_DB: f64 = 1.0;
const MEASUREMENT_VERSION: &str = "dsd-ultrasonic-bands-v3-20260704";
const SCORING_VERSION: &str = "dsd-sectioned-score-v9";
const GATE_TABLE_VERSION: &str = "dsd-rate-gates-v2";
const FIXTURE_SET_VERSION: &str = "dsd-fixtures-v3";
const CANDIDATE_SCHEMA_VERSION: &str = "dsd-candidate-schema-v1";
const DSD_ROUNDTRIP_PINK_NOISE_SEEDS: [(u64, u64); 3] = [
    (0x5049_4e4b_4c45_4654, 0x5049_4e4b_5249_4748),
    (0x5049_4e4b_5331_4c46, 0x5049_4e4b_5331_5254),
    (0x5049_4e4b_5332_4c46, 0x5049_4e4b_5332_5254),
];
pub const DSD_ROUNDTRIP_FIXTURE_COUNT: usize = 5 + DSD_ROUNDTRIP_PINK_NOISE_SEEDS.len();
pub const EC4A_CRACKLE_TORTURE_SECONDS: f64 = 60.0;
const CRACKLE_RESIDUAL_FLOOR: f64 = 0.18;
const CRACKLE_SCORE_LIMIT: f64 = 10.0;
const CRACKLE_ANALYSIS_SETTLE_SECONDS: f64 = 0.50;

type DecodedProbeChannel = (&'static str, Vec<f64>, Vec<f64>);
type StereoProbeOutput = ((Vec<f64>, Vec<f64>), (Vec<f64>, Vec<f64>));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuiteMode {
    Quick,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuiteScope {
    All,
    DsdOnly,
    EcDepthOnly,
    Ec3Tuning,
}

#[derive(Debug, Clone, Copy)]
struct DsdPcmPath {
    path_variant: &'static str,
    origin_source_rate: u32,
    renderer_source_rate: u32,
    intermediate_rate: Option<u32>,
    intermediate_bits: Option<u32>,
    intermediate_filter: Option<FilterType>,
}

impl DsdPcmPath {
    fn direct(source_rate: u32) -> Self {
        Self {
            path_variant: "direct",
            origin_source_rate: source_rate,
            renderer_source_rate: source_rate,
            intermediate_rate: None,
            intermediate_bits: None,
            intermediate_filter: None,
        }
    }

    fn direct48_dsd128() -> Self {
        Self {
            path_variant: "direct48_dsd128",
            origin_source_rate: 48_000,
            renderer_source_rate: 48_000,
            intermediate_rate: None,
            intermediate_bits: None,
            intermediate_filter: None,
        }
    }

    fn pcm1536k32_dsd128(intermediate_filter: FilterType) -> Self {
        Self {
            path_variant: "pcm1536k32_dsd128",
            origin_source_rate: 48_000,
            renderer_source_rate: 1_536_000,
            intermediate_rate: Some(1_536_000),
            intermediate_bits: Some(32),
            intermediate_filter: Some(intermediate_filter),
        }
    }

    fn intermediate_filter_name(self) -> Option<&'static str> {
        self.intermediate_filter.map(FilterType::as_name)
    }

    fn prepare_renderer_input(self, filter: FilterType, input: &[f64]) -> Result<Vec<f64>, String> {
        let Some(intermediate_rate) = self.intermediate_rate else {
            return Ok(input.to_vec());
        };
        if self.renderer_source_rate != intermediate_rate {
            return Err(format!(
                "path {} renderer source {} does not match intermediate {}",
                self.path_variant, self.renderer_source_rate, intermediate_rate
            ));
        }
        let intermediate_filter = self.intermediate_filter.unwrap_or(filter);
        let mut resampler = SincResampler::new(
            intermediate_filter,
            self.origin_source_rate,
            intermediate_rate,
        );
        resampler.input(input, input);
        let mut interleaved = Vec::new();
        resampler.process(&mut interleaved);
        resampler.drain_eof(&mut interleaved);
        let mut dither = DitherState::new(0x5043_4d31_3533_364b);
        let full_scale = (1u64 << 31) as f64;
        Ok(interleaved
            .chunks_exact(2)
            .enumerate()
            .map(|(idx, frame)| {
                let code = quantize_signed_pcm(frame[0], 32, idx % 2, &mut dither, DitherMode::Off);
                code as f64 / full_scale
            })
            .collect())
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DsdExperimentConfig {
    ec_obg: Option<f64>,
    expected_gain_db: Option<f64>,
    dsd64_tweaks: DsdExperimentTweaks,
    dsd128_tweaks: DsdExperimentTweaks,
    dsd256_tweaks: DsdExperimentTweaks,
    dsd64_input_gain_explicit: bool,
    ec4a_decision_trace_window_bits: Option<usize>,
}

impl DsdExperimentConfig {
    pub fn ec_obg(obg: f64) -> Result<Self, String> {
        Self::default().with_ec_obg(obg)
    }

    /// Select an EC coefficient-table variant by out-of-band gain. The variant
    /// must exist in `ALL_VARIANTS` for every measured rate;
    /// [`Self::validate_for_rates`] enforces that.
    pub fn with_ec_obg(mut self, obg: f64) -> Result<Self, String> {
        if !obg.is_finite() {
            return Err("EC OBG must be finite".to_string());
        }
        self.ec_obg = Some(obg);
        Ok(self)
    }

    pub fn with_expected_gain_db(mut self, gain_db: f64) -> Result<Self, String> {
        if !gain_db.is_finite() {
            return Err("expected gain dB must be finite".to_string());
        }
        self.expected_gain_db = Some(gain_db);
        Ok(self)
    }

    fn expected_gain_db(self) -> Option<f64> {
        self.expected_gain_db
    }

    pub fn with_dsd64_ecbeam2_config(
        mut self,
        config: EcBeam2ExperimentConfig,
    ) -> Result<Self, String> {
        validate_ecbeam2_experiment_config(config)?;
        self.dsd64_tweaks.ecbeam2_config = Some(config);
        Ok(self)
    }

    pub fn with_dsd64_dither_scale_multiplier(mut self, multiplier: f64) -> Result<Self, String> {
        if !multiplier.is_finite() || multiplier < 0.0 {
            return Err("DSD64 EC dither scale multiplier must be finite and non-negative".into());
        }
        self.dsd64_tweaks.ec_dither_scale_multiplier = Some(multiplier);
        Ok(self)
    }

    pub fn with_dsd64_dither_shape(mut self, shape: DitherShape) -> Self {
        self.dsd64_tweaks.ec_dither_shape = Some(shape);
        self
    }

    pub fn with_dsd64_dither_prng(mut self, prng: DitherPrng) -> Self {
        self.dsd64_tweaks.ec_dither_prng = Some(prng);
        self
    }

    pub fn with_dsd64_dither_leak_alpha(mut self, alpha: f64) -> Result<Self, String> {
        if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
            return Err("DSD64 EC dither leak alpha must be finite and between 0 and 1".into());
        }
        self.dsd64_tweaks.ec_dither_leak_alpha = Some(alpha);
        Ok(self)
    }

    pub fn with_dsd64_dither_lf_floor_gamma(mut self, gamma: f64) -> Result<Self, String> {
        if !gamma.is_finite() || gamma < 0.0 {
            return Err("DSD64 EC dither LF floor gamma must be finite and non-negative".into());
        }
        self.dsd64_tweaks.ec_dither_lf_floor_gamma = Some(gamma);
        Ok(self)
    }

    pub fn with_dsd64_ec_dc_corner_hz(mut self, corner_hz: f64) -> Result<Self, String> {
        validate_ec_dc_corner_hz(corner_hz, "DSD64")?;
        self.dsd64_tweaks.ec_dc_bias_corner_hz = Some(corner_hz);
        Ok(self)
    }

    pub fn with_dsd64_future_scorer(mut self, scorer: EcFutureScorer) -> Self {
        self.dsd64_tweaks.ec_future_scorer = Some(scorer);
        self
    }

    pub fn with_dsd64_ec2_long_filter_policy(mut self, policy: Ec2LongFilterPolicy) -> Self {
        self.dsd64_tweaks.ec2_long_filter_policy = Some(policy);
        self
    }

    pub fn with_dsd64_ec2_policy_weights(
        mut self,
        weights: Ec2PolicyWeights,
    ) -> Result<Self, String> {
        validate_ec2_policy_weights(weights)?;
        self.dsd64_tweaks.ec2_policy_weights = Some(weights);
        Ok(self)
    }

    pub fn with_dsd64_ec2_pressure_stage_weights(
        mut self,
        weights: [f64; 7],
    ) -> Result<Self, String> {
        validate_ec2_pressure_stage_weights(&weights, "DSD64")?;
        self.dsd64_tweaks.ec2_pressure_stage_weights = Some(weights);
        Ok(self)
    }

    pub fn with_dsd64_gated_dither(mut self, margin: f64, scale: f64) -> Result<Self, String> {
        validate_gated_dither(margin, scale, "DSD64")?;
        self.dsd64_tweaks.ec_gated_dither_margin = Some(margin);
        self.dsd64_tweaks.ec_gated_dither_scale = Some(scale);
        Ok(self)
    }

    pub fn with_dsd64_ec2_decision_trace_window_bits(mut self, window_bits: usize) -> Self {
        self.dsd64_tweaks.ec2_decision_trace_window_bits = Some(window_bits.max(1024));
        self
    }

    pub fn with_dsd64_ec_beam_search(mut self, m: usize, n: usize) -> Result<Self, String> {
        validate_ec_beam_search(m, n, "DSD64")?;
        self.dsd64_tweaks.ec_beam_search = Some((m, n));
        self.dsd64_tweaks = self.dsd64_tweaks.with_ec_beam_a1_defaults();
        if self.expected_gain_db.is_none() {
            self.expected_gain_db = Some(DSD64_EC_BEAM_A1_DEFAULT_EXPECTED_GAIN_DB);
        }
        if !self.dsd64_input_gain_explicit {
            self.dsd64_tweaks.input_gain_db = DSD64_EC_BEAM_A1_DEFAULT_INPUT_GAIN_DB;
        }
        Ok(self)
    }

    pub fn with_dsd64_ec_beam_terminal_weight(mut self, weight: f64) -> Result<Self, String> {
        validate_ec_beam_terminal_weight(weight, "DSD64")?;
        self.dsd64_tweaks.ec_beam_terminal_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd64_ec_beam_alternation_weight(mut self, weight: f64) -> Result<Self, String> {
        validate_ec_beam_alternation_weight(weight, "DSD64")?;
        self.dsd64_tweaks.ec_beam_alternation_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd64_ec_beam_alternation_rank_weight(
        mut self,
        weight: f64,
    ) -> Result<Self, String> {
        validate_ec_beam_alternation_weight(weight, "DSD64")?;
        self.dsd64_tweaks.ec_beam_alternation_rank_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd64_ec_beam_alternation_threshold(
        mut self,
        threshold: f64,
    ) -> Result<Self, String> {
        validate_ec_beam_alternation_threshold(threshold, "DSD64")?;
        self.dsd64_tweaks.ec_beam_alternation_threshold = Some(threshold);
        Ok(self)
    }

    pub fn with_dsd64_ec_beam_periodicity_weight(mut self, weight: f64) -> Result<Self, String> {
        validate_ec_beam_periodicity_weight(weight, "DSD64")?;
        self.dsd64_tweaks.ec_beam_periodicity_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd64_ec_beam_periodicity_lags(mut self, lags: &[u8]) -> Result<Self, String> {
        let (lags, count) = validate_ec_beam_periodicity_lags(lags, "DSD64")?;
        self.dsd64_tweaks.ec_beam_periodicity_lags = Some(lags);
        self.dsd64_tweaks.ec_beam_periodicity_lag_count = Some(count);
        Ok(self)
    }

    pub fn with_dsd64_ec_beam_periodicity_window(mut self, window: usize) -> Result<Self, String> {
        validate_ec_beam_periodicity_window(window, "DSD64")?;
        self.dsd64_tweaks.ec_beam_periodicity_window = Some(window);
        Ok(self)
    }

    pub fn with_dsd64_ec_beam_filtered_error_weight(mut self, weight: f64) -> Result<Self, String> {
        validate_ec_beam_filtered_error_weight(weight, "DSD64")?;
        self.dsd64_tweaks.ec_beam_filtered_error_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd64_ec_beam_filtered_error_rank_weight(
        mut self,
        weight: f64,
    ) -> Result<Self, String> {
        validate_ec_beam_filtered_error_weight(weight, "DSD64")?;
        self.dsd64_tweaks.ec_beam_filtered_error_rank_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd64_ec_beam_reconstruction_error_weight(
        mut self,
        weight: f64,
    ) -> Result<Self, String> {
        validate_ec_beam_reconstruction_error_weight(weight, "DSD64")?;
        self.dsd64_tweaks.ec_beam_reconstruction_error_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd64_ec_beam_pressure_deadzone(mut self, deadzone: f64) -> Result<Self, String> {
        validate_ec_beam_pressure_deadzone(deadzone, "DSD64")?;
        self.dsd64_tweaks.ec_beam_pressure_deadzone = Some(deadzone);
        Ok(self)
    }

    pub fn with_dsd64_ec_beam_metric_diagnostics(mut self, enabled: bool) -> Self {
        self.dsd64_tweaks.ec_beam_metric_diagnostics = Some(enabled);
        self
    }

    pub fn with_dsd64_ec_beam_auxiliary_metric_scales(
        mut self,
        pressure_accum_scale: Option<f64>,
        pressure_rank_scale: Option<f64>,
        dc_accum_scale: Option<f64>,
        dc_rank_scale: Option<f64>,
    ) -> Result<Self, String> {
        for (name, value) in [
            ("pressure_accum_scale", pressure_accum_scale),
            ("pressure_rank_scale", pressure_rank_scale),
            ("dc_accum_scale", dc_accum_scale),
            ("dc_rank_scale", dc_rank_scale),
        ] {
            if let Some(value) = value
                && (!value.is_finite() || value < 0.0)
            {
                return Err(format!(
                    "DSD64 EcBeam {name} must be finite and non-negative"
                ));
            }
        }
        self.dsd64_tweaks.ec_beam_pressure_accum_scale = pressure_accum_scale;
        self.dsd64_tweaks.ec_beam_pressure_rank_scale = pressure_rank_scale;
        self.dsd64_tweaks.ec_beam_dc_accum_scale = dc_accum_scale;
        self.dsd64_tweaks.ec_beam_dc_rank_scale = dc_rank_scale;
        Ok(self)
    }

    pub fn with_dsd64_seed_pair(mut self, left: Option<u64>, right: Option<u64>) -> Self {
        self.dsd64_tweaks.seed_left = left;
        self.dsd64_tweaks.seed_right = right;
        self
    }

    pub fn with_dsd64_input_gain_db(mut self, gain_db: f64) -> Result<Self, String> {
        if !gain_db.is_finite() || !(-12.0..=3.0).contains(&gain_db) {
            return Err("DSD64 input gain trim must be finite and between -12 and +3 dB".into());
        }
        self.dsd64_tweaks.input_gain_db = gain_db;
        self.dsd64_input_gain_explicit = true;
        Ok(self)
    }

    pub fn with_dsd128_dither_scale_multiplier(mut self, multiplier: f64) -> Result<Self, String> {
        if !multiplier.is_finite() || multiplier < 0.0 {
            return Err("DSD128 EC dither scale multiplier must be finite and non-negative".into());
        }
        self.dsd128_tweaks.ec_dither_scale_multiplier = Some(multiplier);
        Ok(self)
    }

    pub fn with_dsd128_dither_shape(mut self, shape: DitherShape) -> Self {
        self.dsd128_tweaks.ec_dither_shape = Some(shape);
        self
    }

    pub fn with_dsd128_dither_prng(mut self, prng: DitherPrng) -> Self {
        self.dsd128_tweaks.ec_dither_prng = Some(prng);
        self
    }

    pub fn with_dsd128_dither_leak_alpha(mut self, alpha: f64) -> Result<Self, String> {
        if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
            return Err("DSD128 EC dither leak alpha must be finite and between 0 and 1".into());
        }
        self.dsd128_tweaks.ec_dither_leak_alpha = Some(alpha);
        Ok(self)
    }

    pub fn with_dsd128_dither_lf_floor_gamma(mut self, gamma: f64) -> Result<Self, String> {
        if !gamma.is_finite() || gamma < 0.0 {
            return Err("DSD128 EC dither LF floor gamma must be finite and non-negative".into());
        }
        self.dsd128_tweaks.ec_dither_lf_floor_gamma = Some(gamma);
        Ok(self)
    }

    pub fn with_dsd128_common_side_dither(
        mut self,
        beta: f64,
        common_seed: u64,
        side_seed: u64,
    ) -> Result<Self, String> {
        if !beta.is_finite() || beta < 0.0 {
            return Err("DSD128 common/side dither beta must be finite and non-negative".into());
        }
        self.dsd128_tweaks.ec_common_side_dither = Some(DsdCommonSideDither {
            beta,
            common_seed,
            side_seed,
        });
        Ok(self)
    }

    pub fn with_dsd128_ec_dc_corner_hz(mut self, corner_hz: f64) -> Result<Self, String> {
        validate_ec_dc_corner_hz(corner_hz, "DSD128")?;
        self.dsd128_tweaks.ec_dc_bias_corner_hz = Some(corner_hz);
        Ok(self)
    }

    pub fn with_dsd128_future_scorer(mut self, scorer: EcFutureScorer) -> Self {
        self.dsd128_tweaks.ec_future_scorer = Some(scorer);
        self
    }

    pub fn with_dsd128_ec2_long_filter_policy(mut self, policy: Ec2LongFilterPolicy) -> Self {
        self.dsd128_tweaks.ec2_long_filter_policy = Some(policy);
        self
    }

    pub fn with_dsd128_ec2_policy_weights(
        mut self,
        weights: Ec2PolicyWeights,
    ) -> Result<Self, String> {
        validate_ec2_policy_weights(weights)?;
        self.dsd128_tweaks.ec2_policy_weights = Some(weights);
        Ok(self)
    }

    pub fn with_dsd128_ec2_pressure_stage_weights(
        mut self,
        weights: [f64; 7],
    ) -> Result<Self, String> {
        validate_ec2_pressure_stage_weights(&weights, "DSD128")?;
        self.dsd128_tweaks.ec2_pressure_stage_weights = Some(weights);
        Ok(self)
    }

    pub fn with_dsd128_gated_dither(mut self, margin: f64, scale: f64) -> Result<Self, String> {
        validate_gated_dither(margin, scale, "DSD128")?;
        self.dsd128_tweaks.ec_gated_dither_margin = Some(margin);
        self.dsd128_tweaks.ec_gated_dither_scale = Some(scale);
        Ok(self)
    }

    pub fn with_ec2_decision_trace_window_bits(mut self, window_bits: usize) -> Self {
        self.dsd128_tweaks.ec2_decision_trace_window_bits = Some(window_bits.max(1024));
        self
    }

    pub fn with_dsd128_ec_beam_search(mut self, m: usize, n: usize) -> Result<Self, String> {
        validate_ec_beam_search(m, n, "DSD128")?;
        self.dsd128_tweaks.ec_beam_search = Some((m, n));
        Ok(self)
    }

    pub fn with_dsd128_ec_beam_terminal_weight(mut self, weight: f64) -> Result<Self, String> {
        validate_ec_beam_terminal_weight(weight, "DSD128")?;
        self.dsd128_tweaks.ec_beam_terminal_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd128_ec_beam_alternation_weight(mut self, weight: f64) -> Result<Self, String> {
        validate_ec_beam_alternation_weight(weight, "DSD128")?;
        self.dsd128_tweaks.ec_beam_alternation_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd128_ec_beam_alternation_rank_weight(
        mut self,
        weight: f64,
    ) -> Result<Self, String> {
        validate_ec_beam_alternation_weight(weight, "DSD128")?;
        self.dsd128_tweaks.ec_beam_alternation_rank_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd128_ec_beam_alternation_threshold(
        mut self,
        threshold: f64,
    ) -> Result<Self, String> {
        validate_ec_beam_alternation_threshold(threshold, "DSD128")?;
        self.dsd128_tweaks.ec_beam_alternation_threshold = Some(threshold);
        Ok(self)
    }

    pub fn with_dsd128_ec_beam_periodicity_weight(mut self, weight: f64) -> Result<Self, String> {
        validate_ec_beam_periodicity_weight(weight, "DSD128")?;
        self.dsd128_tweaks.ec_beam_periodicity_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd128_ec_beam_periodicity_lags(mut self, lags: &[u8]) -> Result<Self, String> {
        let (lags, count) = validate_ec_beam_periodicity_lags(lags, "DSD128")?;
        self.dsd128_tweaks.ec_beam_periodicity_lags = Some(lags);
        self.dsd128_tweaks.ec_beam_periodicity_lag_count = Some(count);
        Ok(self)
    }

    pub fn with_dsd128_ec_beam_periodicity_window(mut self, window: usize) -> Result<Self, String> {
        validate_ec_beam_periodicity_window(window, "DSD128")?;
        self.dsd128_tweaks.ec_beam_periodicity_window = Some(window);
        Ok(self)
    }

    pub fn with_dsd128_ec_beam_filtered_error_weight(
        mut self,
        weight: f64,
    ) -> Result<Self, String> {
        validate_ec_beam_filtered_error_weight(weight, "DSD128")?;
        self.dsd128_tweaks.ec_beam_filtered_error_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd128_ec_beam_filtered_error_rank_weight(
        mut self,
        weight: f64,
    ) -> Result<Self, String> {
        validate_ec_beam_filtered_error_weight(weight, "DSD128")?;
        self.dsd128_tweaks.ec_beam_filtered_error_rank_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd128_ec_beam_reconstruction_error_weight(
        mut self,
        weight: f64,
    ) -> Result<Self, String> {
        validate_ec_beam_reconstruction_error_weight(weight, "DSD128")?;
        self.dsd128_tweaks.ec_beam_reconstruction_error_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd128_ec_beam_pressure_deadzone(mut self, deadzone: f64) -> Result<Self, String> {
        validate_ec_beam_pressure_deadzone(deadzone, "DSD128")?;
        self.dsd128_tweaks.ec_beam_pressure_deadzone = Some(deadzone);
        Ok(self)
    }

    pub fn with_dsd128_ec_beam_metric_diagnostics(mut self, enabled: bool) -> Self {
        self.dsd128_tweaks.ec_beam_metric_diagnostics = Some(enabled);
        self
    }

    pub fn with_dsd256_ec2_decision_trace_window_bits(mut self, window_bits: usize) -> Self {
        self.dsd256_tweaks.ec2_decision_trace_window_bits = Some(window_bits.max(1024));
        self
    }

    pub fn with_dsd256_ec_beam_search(mut self, m: usize, n: usize) -> Result<Self, String> {
        validate_ec_beam_search(m, n, "DSD256")?;
        self.dsd256_tweaks.ec_beam_search = Some((m, n));
        Ok(self)
    }

    pub fn with_dsd256_ec_beam_terminal_weight(mut self, weight: f64) -> Result<Self, String> {
        validate_ec_beam_terminal_weight(weight, "DSD256")?;
        self.dsd256_tweaks.ec_beam_terminal_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd256_ec_beam_alternation_weight(mut self, weight: f64) -> Result<Self, String> {
        validate_ec_beam_alternation_weight(weight, "DSD256")?;
        self.dsd256_tweaks.ec_beam_alternation_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd256_ec_beam_alternation_rank_weight(
        mut self,
        weight: f64,
    ) -> Result<Self, String> {
        validate_ec_beam_alternation_weight(weight, "DSD256")?;
        self.dsd256_tweaks.ec_beam_alternation_rank_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd256_ec_beam_alternation_threshold(
        mut self,
        threshold: f64,
    ) -> Result<Self, String> {
        validate_ec_beam_alternation_threshold(threshold, "DSD256")?;
        self.dsd256_tweaks.ec_beam_alternation_threshold = Some(threshold);
        Ok(self)
    }

    pub fn with_dsd256_ec_beam_periodicity_weight(mut self, weight: f64) -> Result<Self, String> {
        validate_ec_beam_periodicity_weight(weight, "DSD256")?;
        self.dsd256_tweaks.ec_beam_periodicity_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd256_ec_beam_periodicity_lags(mut self, lags: &[u8]) -> Result<Self, String> {
        let (lags, count) = validate_ec_beam_periodicity_lags(lags, "DSD256")?;
        self.dsd256_tweaks.ec_beam_periodicity_lags = Some(lags);
        self.dsd256_tweaks.ec_beam_periodicity_lag_count = Some(count);
        Ok(self)
    }

    pub fn with_dsd256_ec_beam_periodicity_window(mut self, window: usize) -> Result<Self, String> {
        validate_ec_beam_periodicity_window(window, "DSD256")?;
        self.dsd256_tweaks.ec_beam_periodicity_window = Some(window);
        Ok(self)
    }

    pub fn with_dsd256_ec_beam_filtered_error_weight(
        mut self,
        weight: f64,
    ) -> Result<Self, String> {
        validate_ec_beam_filtered_error_weight(weight, "DSD256")?;
        self.dsd256_tweaks.ec_beam_filtered_error_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd256_ec_beam_filtered_error_rank_weight(
        mut self,
        weight: f64,
    ) -> Result<Self, String> {
        validate_ec_beam_filtered_error_weight(weight, "DSD256")?;
        self.dsd256_tweaks.ec_beam_filtered_error_rank_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd256_ec_beam_reconstruction_error_weight(
        mut self,
        weight: f64,
    ) -> Result<Self, String> {
        validate_ec_beam_reconstruction_error_weight(weight, "DSD256")?;
        self.dsd256_tweaks.ec_beam_reconstruction_error_weight = Some(weight);
        Ok(self)
    }

    pub fn with_dsd256_ec_beam_pressure_deadzone(mut self, deadzone: f64) -> Result<Self, String> {
        validate_ec_beam_pressure_deadzone(deadzone, "DSD256")?;
        self.dsd256_tweaks.ec_beam_pressure_deadzone = Some(deadzone);
        Ok(self)
    }

    pub fn with_dsd256_ec_beam_metric_diagnostics(mut self, enabled: bool) -> Self {
        self.dsd256_tweaks.ec_beam_metric_diagnostics = Some(enabled);
        self
    }

    pub fn with_dsd128_ec4a_predictive(mut self, allow: bool) -> Self {
        self.dsd128_tweaks.ec4a_allow_predictive_triggers = Some(allow);
        self
    }

    pub fn with_dsd128_ec4a_quality_pressure(mut self, allow: bool) -> Self {
        self.dsd128_tweaks.ec4a_dsd128_quality_pressure = allow;
        self
    }

    pub fn with_dsd128_ec4a_quality_pressure_threshold(
        mut self,
        threshold: f64,
    ) -> Result<Self, String> {
        if !threshold.is_finite() || !(0.0..=0.72).contains(&threshold) {
            return Err(
                "DSD128 EC-4A quality pressure threshold must be finite and between 0 and 0.72"
                    .into(),
            );
        }
        self.dsd128_tweaks.ec4a_dsd128_quality_pressure = true;
        self.dsd128_tweaks.ec4a_dsd128_quality_pressure_threshold = Some(threshold);
        Ok(self)
    }

    pub fn with_dsd128_ec4a_quality_pressure_hold(mut self, hold: u32) -> Result<Self, String> {
        if hold == 0 {
            return Err("DSD128 EC-4A quality pressure hold must be positive".into());
        }
        self.dsd128_tweaks.ec4a_dsd128_quality_pressure = true;
        self.dsd128_tweaks.ec4a_dsd128_quality_pressure_hold = Some(hold);
        Ok(self)
    }

    pub fn with_ec4a_decision_trace_window_bits(mut self, window_bits: usize) -> Self {
        self.ec4a_decision_trace_window_bits = Some(window_bits.max(1024));
        self
    }

    pub fn with_dsd_seed_pair(mut self, left: Option<u64>, right: Option<u64>) -> Self {
        self.dsd128_tweaks.seed_left = left;
        self.dsd128_tweaks.seed_right = right;
        self
    }

    pub fn with_dsd128_input_gain_db(mut self, gain_db: f64) -> Result<Self, String> {
        if !gain_db.is_finite() || !(-12.0..=3.0).contains(&gain_db) {
            return Err("DSD128 input gain trim must be finite and between -12 and +3 dB".into());
        }
        self.dsd128_tweaks.input_gain_db = gain_db;
        Ok(self)
    }

    pub fn with_dsd256_dither_scale_multiplier(mut self, multiplier: f64) -> Result<Self, String> {
        if !multiplier.is_finite() || multiplier < 0.0 {
            return Err("DSD256 EC dither scale multiplier must be finite and non-negative".into());
        }
        self.dsd256_tweaks.ec_dither_scale_multiplier = Some(multiplier);
        Ok(self)
    }

    pub fn with_dsd256_dither_shape(mut self, shape: DitherShape) -> Self {
        self.dsd256_tweaks.ec_dither_shape = Some(shape);
        self
    }

    pub fn with_dsd256_dither_prng(mut self, prng: DitherPrng) -> Self {
        self.dsd256_tweaks.ec_dither_prng = Some(prng);
        self
    }

    pub fn with_dsd256_dither_leak_alpha(mut self, alpha: f64) -> Result<Self, String> {
        if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
            return Err("DSD256 EC dither leak alpha must be finite and between 0 and 1".into());
        }
        self.dsd256_tweaks.ec_dither_leak_alpha = Some(alpha);
        Ok(self)
    }

    pub fn with_dsd256_dither_lf_floor_gamma(mut self, gamma: f64) -> Result<Self, String> {
        if !gamma.is_finite() || gamma < 0.0 {
            return Err("DSD256 EC dither LF floor gamma must be finite and non-negative".into());
        }
        self.dsd256_tweaks.ec_dither_lf_floor_gamma = Some(gamma);
        Ok(self)
    }

    pub fn with_dsd256_ec_dc_corner_hz(mut self, corner_hz: f64) -> Result<Self, String> {
        validate_ec_dc_corner_hz(corner_hz, "DSD256")?;
        self.dsd256_tweaks.ec_dc_bias_corner_hz = Some(corner_hz);
        Ok(self)
    }

    pub fn with_dsd256_future_scorer(mut self, scorer: EcFutureScorer) -> Self {
        self.dsd256_tweaks.ec_future_scorer = Some(scorer);
        self
    }

    pub fn with_dsd256_ec2_long_filter_policy(mut self, policy: Ec2LongFilterPolicy) -> Self {
        self.dsd256_tweaks.ec2_long_filter_policy = Some(policy);
        self
    }

    pub fn with_dsd256_ec2_policy_weights(
        mut self,
        weights: Ec2PolicyWeights,
    ) -> Result<Self, String> {
        validate_ec2_policy_weights(weights)?;
        self.dsd256_tweaks.ec2_policy_weights = Some(weights);
        Ok(self)
    }

    pub fn with_dsd256_ec2_pressure_stage_weights(
        mut self,
        weights: [f64; 7],
    ) -> Result<Self, String> {
        validate_ec2_pressure_stage_weights(&weights, "DSD256")?;
        self.dsd256_tweaks.ec2_pressure_stage_weights = Some(weights);
        Ok(self)
    }

    pub fn with_dsd256_gated_dither(mut self, margin: f64, scale: f64) -> Result<Self, String> {
        validate_gated_dither(margin, scale, "DSD256")?;
        self.dsd256_tweaks.ec_gated_dither_margin = Some(margin);
        self.dsd256_tweaks.ec_gated_dither_scale = Some(scale);
        Ok(self)
    }

    pub fn with_dsd256_seed_pair(mut self, left: Option<u64>, right: Option<u64>) -> Self {
        self.dsd256_tweaks.seed_left = left;
        self.dsd256_tweaks.seed_right = right;
        self
    }

    pub fn with_dsd256_input_gain_db(mut self, gain_db: f64) -> Result<Self, String> {
        if !gain_db.is_finite() || !(-12.0..=3.0).contains(&gain_db) {
            return Err("DSD256 input gain trim must be finite and between -12 and +3 dB".into());
        }
        self.dsd256_tweaks.input_gain_db = gain_db;
        Ok(self)
    }

    fn validate(self) -> Result<(), String> {
        self.validate_for_rates(&[DsdRate::Dsd128, DsdRate::Dsd256])
    }

    fn validate_for_rates(self, rates: &[DsdRate]) -> Result<(), String> {
        if self.dsd64_tweaks.ecbeam2_config.is_some()
            && rates.iter().any(|rate| *rate != DsdRate::Dsd64)
        {
            return Err("EcBeam2 experiment controls support DSD64 only".to_string());
        }
        if let Some(obg) = self.ec_obg {
            for &rate in rates {
                if find_ec_obg_variant(rate, obg).is_none() {
                    return Err(format!(
                        "no generated EC coefficient variant for {} OBG{:.2}",
                        dsd_rate_name(rate),
                        obg
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn has_rate_tweaks(self) -> bool {
        self.dsd64_tweaks != DsdExperimentTweaks::default()
            || self.dsd128_tweaks != DsdExperimentTweaks::default()
            || self.dsd256_tweaks != DsdExperimentTweaks::default()
    }

    #[cfg(test)]
    pub fn test_tweaks_for(
        self,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
    ) -> DsdExperimentTweaks {
        self.tweaks_for(dsd_rate, dsd_modulator)
    }

    fn input_gain_only(self) -> Self {
        Self {
            dsd64_tweaks: DsdExperimentTweaks {
                input_gain_db: self.dsd64_tweaks.input_gain_db,
                ..DsdExperimentTweaks::default()
            },
            dsd128_tweaks: DsdExperimentTweaks {
                input_gain_db: self.dsd128_tweaks.input_gain_db,
                ..DsdExperimentTweaks::default()
            },
            dsd256_tweaks: DsdExperimentTweaks {
                input_gain_db: self.dsd256_tweaks.input_gain_db,
                ..DsdExperimentTweaks::default()
            },
            dsd64_input_gain_explicit: self.dsd64_input_gain_explicit,
            ..Self::default()
        }
    }

    fn ec_coeff_for(
        self,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
    ) -> Option<(&'static str, &'static ModulatorCoeffs)> {
        if dsd_modulator == DsdModulator::Standard {
            return None;
        }
        find_ec_obg_variant(dsd_rate, self.ec_obg?)
    }

    fn label(self) -> Option<String> {
        let mut labels = Vec::new();
        if let Some(obg) = self.ec_obg {
            labels.push(format!("ec-obg{obg:.2}"));
        }
        if let Some(config) = self.dsd64_tweaks.ecbeam2_config {
            labels.push(format!(
                "ecbeam2-{run:?}-{profile:?}-dz{deadzone:.4}-dzw{deadzone_weight:.6}-q{quantizer:.9}",
                run = config.run_mode,
                profile = config.profile,
                deadzone = config.state_deadzone,
                deadzone_weight = config.state_deadzone_weight,
                quantizer = config.quantizer_regularizer,
            ));
            if let Some(budget) = config.ultrasonic_budget {
                labels.push(format!("ecbeam2-ultrasonic-budget{budget:.9}"));
            }
            if let Some(budget) = config.signed_error_budget {
                labels.push(format!("ecbeam2-signed-error-budget{budget:.9}"));
            }
        }
        if let Some(multiplier) = self.dsd64_tweaks.ec_dither_scale_multiplier {
            labels.push(format!("dsd64-dither{multiplier:.4}"));
        }
        if let Some(shape) = self.dsd64_tweaks.ec_dither_shape {
            labels.push(format!("dsd64-dither-{shape:?}"));
        }
        if let Some(prng) = self.dsd64_tweaks.ec_dither_prng {
            labels.push(format!("dsd64-prng-{prng:?}"));
        }
        if let Some(alpha) = self.dsd64_tweaks.ec_dither_leak_alpha {
            labels.push(format!("dsd64-leak{alpha:.4}"));
        }
        if let Some(gamma) = self.dsd64_tweaks.ec_dither_lf_floor_gamma {
            labels.push(format!("dsd64-lffloor{gamma:.4}"));
        }
        if let Some(corner_hz) = self.dsd64_tweaks.ec_dc_bias_corner_hz {
            labels.push(format!("dsd64-dc-corner{corner_hz:.2}hz"));
        }
        if let Some(scorer) = self.dsd64_tweaks.ec_future_scorer {
            labels.push(format!("dsd64-future-{scorer:?}"));
        }
        if let Some(policy) = self.dsd64_tweaks.ec2_long_filter_policy {
            labels.push(format!("dsd64-ec2-policy-{}", policy.as_name()));
        }
        if let Some(weights) = self.dsd64_tweaks.ec2_policy_weights {
            labels.push(format!(
                "dsd64-ec2-wq{:.3}-wp{:.3}-wl{:.1}-wt{:.4}-wd{:.4}-disc{:.3}-amb{:.4}",
                weights.quantizer_weight,
                weights.pressure_weight,
                weights.limit_weight,
                weights.transition_weight,
                weights.dc_weight,
                weights.lookahead_discount,
                weights.ambiguity_margin
            ));
        }
        if let Some(weights) = self.dsd64_tweaks.ec2_pressure_stage_weights {
            labels.push(format!("dsd64-ec2-stagew{}", stage_weight_label(&weights)));
        }
        if let (Some(margin), Some(scale)) = (
            self.dsd64_tweaks.ec_gated_dither_margin,
            self.dsd64_tweaks.ec_gated_dither_scale,
        ) {
            labels.push(format!("dsd64-gated-dither-m{margin:.4}-s{scale:.4}"));
        }
        if let Some(window_bits) = self.dsd64_tweaks.ec2_decision_trace_window_bits {
            labels.push(format!("dsd64-ec2-trace-window{window_bits}"));
        }
        if let Some((m, n)) = self.dsd64_tweaks.ec_beam_search {
            labels.push(format!("dsd64-ecbeam-m{m}-n{n}"));
        }
        if let Some(weight) = self.dsd64_tweaks.ec_beam_terminal_weight {
            labels.push(format!("dsd64-beam-terminal{weight:.4}"));
        }
        if let Some(weight) = self.dsd64_tweaks.ec_beam_alternation_weight {
            labels.push(format!("dsd64-beam-alt{weight:.6}"));
        }
        if let Some(weight) = self.dsd64_tweaks.ec_beam_alternation_rank_weight {
            labels.push(format!("dsd64-beam-alt-rank{weight:.6}"));
        }
        if let Some(threshold) = self.dsd64_tweaks.ec_beam_alternation_threshold {
            labels.push(format!("dsd64-beam-alt-thr{threshold:.4}"));
        }
        if let Some(weight) = self.dsd64_tweaks.ec_beam_filtered_error_weight {
            labels.push(format!("dsd64-beam-ferr{weight:.6}"));
        }
        if let Some(weight) = self.dsd64_tweaks.ec_beam_filtered_error_rank_weight {
            labels.push(format!("dsd64-beam-ferr-rank{weight:.6}"));
        }
        if let Some(weight) = self.dsd64_tweaks.ec_beam_reconstruction_error_weight {
            labels.push(format!("dsd64-beam-rerr{weight:.6}"));
        }
        if let Some(seed) = self.dsd64_tweaks.seed_left {
            labels.push(format!("dsd64-seed-l{seed:016x}"));
        }
        if let Some(seed) = self.dsd64_tweaks.seed_right {
            labels.push(format!("dsd64-seed-r{seed:016x}"));
        }
        if self.dsd64_tweaks.input_gain_db != 0.0 {
            labels.push(format!(
                "dsd64-gain{:.2}db",
                self.dsd64_tweaks.input_gain_db
            ));
        }
        if let Some(multiplier) = self.dsd128_tweaks.ec_dither_scale_multiplier {
            labels.push(format!("dsd128-dither{multiplier:.4}"));
        }
        if let Some(shape) = self.dsd128_tweaks.ec_dither_shape {
            labels.push(format!("dsd128-dither-{shape:?}"));
        }
        if let Some(prng) = self.dsd128_tweaks.ec_dither_prng {
            labels.push(format!("dsd128-prng-{prng:?}"));
        }
        if let Some(alpha) = self.dsd128_tweaks.ec_dither_leak_alpha {
            labels.push(format!("dsd128-leak{alpha:.4}"));
        }
        if let Some(gamma) = self.dsd128_tweaks.ec_dither_lf_floor_gamma {
            labels.push(format!("dsd128-lffloor{gamma:.4}"));
        }
        if let Some(corner_hz) = self.dsd128_tweaks.ec_dc_bias_corner_hz {
            labels.push(format!("dsd128-dc-corner{corner_hz:.2}hz"));
        }
        if let Some(common_side) = self.dsd128_tweaks.ec_common_side_dither {
            labels.push(format!("dsd128-common-side-beta{:.5}", common_side.beta));
            labels.push(format!(
                "dsd128-common{seed:016x}",
                seed = common_side.common_seed
            ));
            labels.push(format!(
                "dsd128-side{seed:016x}",
                seed = common_side.side_seed
            ));
        }
        if let Some(scorer) = self.dsd128_tweaks.ec_future_scorer {
            labels.push(format!("dsd128-future-{scorer:?}"));
        }
        if let Some(policy) = self.dsd128_tweaks.ec2_long_filter_policy {
            labels.push(format!("dsd128-ec2-policy-{}", policy.as_name()));
        }
        if let Some(weights) = self.dsd128_tweaks.ec2_policy_weights {
            labels.push(format!(
                "dsd128-ec2-wq{:.3}-wp{:.3}-wl{:.1}-wt{:.4}-wd{:.4}-disc{:.3}-amb{:.4}",
                weights.quantizer_weight,
                weights.pressure_weight,
                weights.limit_weight,
                weights.transition_weight,
                weights.dc_weight,
                weights.lookahead_discount,
                weights.ambiguity_margin
            ));
        }
        if let Some(weights) = self.dsd128_tweaks.ec2_pressure_stage_weights {
            labels.push(format!("dsd128-ec2-stagew{}", stage_weight_label(&weights)));
        }
        if let (Some(margin), Some(scale)) = (
            self.dsd128_tweaks.ec_gated_dither_margin,
            self.dsd128_tweaks.ec_gated_dither_scale,
        ) {
            labels.push(format!("dsd128-gated-dither-m{margin:.4}-s{scale:.4}"));
        }
        if let Some(window_bits) = self.dsd128_tweaks.ec2_decision_trace_window_bits {
            labels.push(format!("ec2-trace-window{window_bits}"));
        }
        if let Some((m, n)) = self.dsd128_tweaks.ec_beam_search {
            labels.push(format!("dsd128-ecbeam-m{m}-n{n}"));
        }
        if let Some(weight) = self.dsd128_tweaks.ec_beam_filtered_error_weight {
            labels.push(format!("dsd128-beam-ferr{weight:.6}"));
        }
        if let Some(weight) = self.dsd128_tweaks.ec_beam_filtered_error_rank_weight {
            labels.push(format!("dsd128-beam-ferr-rank{weight:.6}"));
        }
        if let Some(weight) = self.dsd128_tweaks.ec_beam_reconstruction_error_weight {
            labels.push(format!("dsd128-beam-rerr{weight:.6}"));
        }
        if let Some(allow) = self.dsd128_tweaks.ec4a_allow_predictive_triggers {
            labels.push(format!("dsd128-ec4a-predictive-{allow}"));
        }
        if self.dsd128_tweaks.ec4a_dsd128_quality_pressure {
            labels.push("dsd128-ec4a-quality-pressure".to_string());
        }
        if let Some(threshold) = self.dsd128_tweaks.ec4a_dsd128_quality_pressure_threshold {
            labels.push(format!("dsd128-ec4a-pressure{threshold:.3}"));
        }
        if let Some(hold) = self.dsd128_tweaks.ec4a_dsd128_quality_pressure_hold {
            labels.push(format!("dsd128-ec4a-hold{hold}"));
        }
        if let Some(window_bits) = self.ec4a_decision_trace_window_bits {
            labels.push(format!("ec4a-trace-window{window_bits}"));
        }
        if let Some(seed) = self.dsd128_tweaks.seed_left {
            labels.push(format!("dsd-seed-l{seed:016x}"));
        }
        if let Some(seed) = self.dsd128_tweaks.seed_right {
            labels.push(format!("dsd-seed-r{seed:016x}"));
        }
        if self.dsd128_tweaks.input_gain_db != 0.0 {
            labels.push(format!(
                "dsd128-gain{:.2}db",
                self.dsd128_tweaks.input_gain_db
            ));
        }
        if let Some(multiplier) = self.dsd256_tweaks.ec_dither_scale_multiplier {
            labels.push(format!("dsd256-dither{multiplier:.4}"));
        }
        if let Some(shape) = self.dsd256_tweaks.ec_dither_shape {
            labels.push(format!("dsd256-dither-{shape:?}"));
        }
        if let Some(prng) = self.dsd256_tweaks.ec_dither_prng {
            labels.push(format!("dsd256-prng-{prng:?}"));
        }
        if let Some(alpha) = self.dsd256_tweaks.ec_dither_leak_alpha {
            labels.push(format!("dsd256-leak{alpha:.4}"));
        }
        if let Some(gamma) = self.dsd256_tweaks.ec_dither_lf_floor_gamma {
            labels.push(format!("dsd256-lffloor{gamma:.4}"));
        }
        if let Some(corner_hz) = self.dsd256_tweaks.ec_dc_bias_corner_hz {
            labels.push(format!("dsd256-dc-corner{corner_hz:.2}hz"));
        }
        if let Some(scorer) = self.dsd256_tweaks.ec_future_scorer {
            labels.push(format!("dsd256-future-{scorer:?}"));
        }
        if let Some(weights) = self.dsd256_tweaks.ec2_pressure_stage_weights {
            labels.push(format!("dsd256-ec2-stagew{}", stage_weight_label(&weights)));
        }
        if let (Some(margin), Some(scale)) = (
            self.dsd256_tweaks.ec_gated_dither_margin,
            self.dsd256_tweaks.ec_gated_dither_scale,
        ) {
            labels.push(format!("dsd256-gated-dither-m{margin:.4}-s{scale:.4}"));
        }
        if let Some((m, n)) = self.dsd256_tweaks.ec_beam_search {
            labels.push(format!("dsd256-ecbeam-m{m}-n{n}"));
        }
        if let Some(weight) = self.dsd256_tweaks.ec_beam_filtered_error_weight {
            labels.push(format!("dsd256-beam-ferr{weight:.6}"));
        }
        if let Some(weight) = self.dsd256_tweaks.ec_beam_filtered_error_rank_weight {
            labels.push(format!("dsd256-beam-ferr-rank{weight:.6}"));
        }
        if let Some(weight) = self.dsd256_tweaks.ec_beam_reconstruction_error_weight {
            labels.push(format!("dsd256-beam-rerr{weight:.6}"));
        }
        if let Some(seed) = self.dsd256_tweaks.seed_left {
            labels.push(format!("dsd256-seed-l{seed:016x}"));
        }
        if let Some(seed) = self.dsd256_tweaks.seed_right {
            labels.push(format!("dsd256-seed-r{seed:016x}"));
        }
        if self.dsd256_tweaks.input_gain_db != 0.0 {
            labels.push(format!(
                "dsd256-gain{:.2}db",
                self.dsd256_tweaks.input_gain_db
            ));
        }
        (!labels.is_empty()).then(|| labels.join(" "))
    }

    fn tweaks_for(self, dsd_rate: DsdRate, dsd_modulator: DsdModulator) -> DsdExperimentTweaks {
        let mut tweaks = match (dsd_rate, dsd_modulator) {
            (DsdRate::Dsd64, DsdModulator::Standard) => DsdExperimentTweaks {
                input_gain_db: self.dsd64_tweaks.input_gain_db,
                ..DsdExperimentTweaks::default()
            },
            (DsdRate::Dsd64, _) => self.dsd64_tweaks,
            (DsdRate::Dsd128, DsdModulator::Standard) => DsdExperimentTweaks {
                input_gain_db: self.dsd128_tweaks.input_gain_db,
                ..DsdExperimentTweaks::default()
            },
            (DsdRate::Dsd128, _) => self.dsd128_tweaks,
            (DsdRate::Dsd256, DsdModulator::Standard) => DsdExperimentTweaks {
                input_gain_db: self.dsd256_tweaks.input_gain_db,
                ..DsdExperimentTweaks::default()
            },
            (DsdRate::Dsd256, _) => self.dsd256_tweaks,
        };
        if dsd_modulator.is_adaptive() {
            tweaks.ec4a_decision_trace_window_bits = self.ec4a_decision_trace_window_bits;
        }
        tweaks.with_dsd128_ec4a_policy_defaults(dsd_rate, dsd_modulator)
    }
}

fn stage_weight_label(weights: &[f64; 7]) -> String {
    weights
        .iter()
        .map(|w| format!("{w:.3}"))
        .collect::<Vec<_>>()
        .join("-")
}

fn validate_ec2_pressure_stage_weights(weights: &[f64; 7], rate: &str) -> Result<(), String> {
    if !weights.iter().all(|w| w.is_finite() && *w >= 0.0) {
        return Err(format!(
            "{rate} EC-2 pressure stage weights must all be finite and non-negative"
        ));
    }
    let sum: f64 = weights.iter().sum();
    if !sum.is_finite() || sum <= 0.0 {
        return Err(format!(
            "{rate} EC-2 pressure stage weights must have a positive sum"
        ));
    }
    Ok(())
}

fn validate_gated_dither(margin: f64, scale: f64, rate: &str) -> Result<(), String> {
    if !margin.is_finite() || !(0.0..=1.0).contains(&margin) {
        return Err(format!(
            "{rate} EC gated dither margin must be finite and between 0 and 1"
        ));
    }
    if !scale.is_finite() || !(0.0..=1.0).contains(&scale) {
        return Err(format!(
            "{rate} EC gated dither scale must be finite and between 0 and 1"
        ));
    }
    Ok(())
}

fn validate_ecbeam2_experiment_config(config: EcBeam2ExperimentConfig) -> Result<(), String> {
    config.validated().map_err(str::to_string)?;
    if !config.state_terminal_weight.is_finite()
        || !(0.0..=1.0e6).contains(&config.state_terminal_weight)
    {
        return Err(
            "EcBeam2 state-terminal weight must be finite and between 0 and 1e6".to_string(),
        );
    }
    if !config.state_deadzone.is_finite() || !(0.0..=1.0).contains(&config.state_deadzone) {
        return Err("EcBeam2 state dead-zone must be finite and between 0 and 1".to_string());
    }
    if !config.state_deadzone_weight.is_finite()
        || !(0.0..=4.0).contains(&config.state_deadzone_weight)
    {
        return Err(
            "EcBeam2 state dead-zone weight must be finite and between 0 and 4".to_string(),
        );
    }
    if !config.quantizer_regularizer.is_finite()
        || !(0.0..=0.01).contains(&config.quantizer_regularizer)
    {
        return Err(
            "EcBeam2 quantizer regularizer must be finite and between 0 and 0.01".to_string(),
        );
    }
    for (name, budget, maximum) in [
        ("ultrasonic", config.ultrasonic_budget, 16.0),
        ("signed-error", config.signed_error_budget, 2.0),
    ] {
        if budget.is_some_and(|value| !value.is_finite() || value <= 0.0 || value > maximum) {
            return Err(format!(
                "EcBeam2 {name} budget must be finite, positive, and at most {maximum}"
            ));
        }
    }
    Ok(())
}

fn configure_ecbeam2_diagnostic_window(
    config: &mut DsdExperimentConfig,
    filter: FilterType,
    source_rate: u32,
    wire_rate: u32,
    analysis_start: usize,
    analysis_length: usize,
) -> Result<(), String> {
    let Some(mut ecbeam2_config) = config.dsd64_tweaks.ecbeam2_config else {
        return Ok(());
    };
    let diagnostic_range = dsd_source_window_to_modulator_samples(
        filter,
        source_rate,
        wire_rate,
        analysis_start,
        analysis_length,
    )
    .ok_or_else(|| "failed to map EcBeam2 diagnostic window to wire samples".to_string())?;
    ecbeam2_config.diagnostic_window = Some(EcBeam2DiagnosticWindow {
        start_sequence: u64::try_from(diagnostic_range.start)
            .map_err(|_| "EcBeam2 diagnostic-window start exceeds u64".to_string())?,
        end_sequence: u64::try_from(diagnostic_range.end)
            .map_err(|_| "EcBeam2 diagnostic-window end exceeds u64".to_string())?,
    });
    validate_ecbeam2_experiment_config(ecbeam2_config)?;
    config.dsd64_tweaks.ecbeam2_config = Some(ecbeam2_config);
    Ok(())
}

fn validate_ec_beam_search(m: usize, n: usize, rate: &str) -> Result<(), String> {
    if !(1..=16).contains(&m) {
        return Err(format!("{rate} EcBeam width must be in 1..=16"));
    }
    if !(1..=48).contains(&n) {
        return Err(format!("{rate} EcBeam horizon must be in 1..=48"));
    }
    Ok(())
}

fn validate_ec_beam_terminal_weight(weight: f64, rate: &str) -> Result<(), String> {
    if !weight.is_finite() || weight < 0.0 {
        return Err(format!(
            "{rate} EcBeam terminal weight must be finite and non-negative"
        ));
    }
    Ok(())
}

fn validate_ec_beam_alternation_weight(weight: f64, rate: &str) -> Result<(), String> {
    if !weight.is_finite() || weight < 0.0 {
        return Err(format!(
            "{rate} EcBeam alternation weight must be finite and non-negative"
        ));
    }
    Ok(())
}

fn validate_ec_beam_alternation_threshold(threshold: f64, rate: &str) -> Result<(), String> {
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        return Err(format!(
            "{rate} EcBeam alternation threshold must be finite and between 0 and 1"
        ));
    }
    Ok(())
}

fn validate_ec_beam_periodicity_weight(weight: f64, rate: &str) -> Result<(), String> {
    if !weight.is_finite() || weight < 0.0 {
        return Err(format!(
            "{rate} EcBeam periodicity weight must be finite and non-negative"
        ));
    }
    Ok(())
}

fn validate_ec_beam_periodicity_lags(lags: &[u8], rate: &str) -> Result<([u8; 4], usize), String> {
    if lags.is_empty() || lags.len() > 4 {
        return Err(format!("{rate} EcBeam periodicity requires 1 to 4 lags"));
    }
    let mut selected = [0u8; 4];
    let mut count = 0usize;
    for &lag in lags {
        if !(1..=47).contains(&lag) {
            return Err(format!("{rate} EcBeam periodicity lags must be in 1..=47"));
        }
        if selected[..count].contains(&lag) {
            return Err(format!(
                "{rate} EcBeam periodicity lags must not contain duplicates"
            ));
        }
        selected[count] = lag;
        count += 1;
    }
    selected[..count].sort_unstable();
    Ok((selected, count))
}

fn validate_ec_beam_periodicity_window(window: usize, rate: &str) -> Result<(), String> {
    if !(2..=48).contains(&window) {
        return Err(format!(
            "{rate} EcBeam periodicity window must be in 2..=48"
        ));
    }
    Ok(())
}

fn validate_ec_beam_filtered_error_weight(weight: f64, rate: &str) -> Result<(), String> {
    if !weight.is_finite() || weight < 0.0 {
        return Err(format!(
            "{rate} EcBeam filtered-error weight must be finite and non-negative"
        ));
    }
    Ok(())
}

fn validate_ec_beam_reconstruction_error_weight(weight: f64, rate: &str) -> Result<(), String> {
    if !weight.is_finite() || weight < 0.0 {
        return Err(format!(
            "{rate} EcBeam reconstruction-error weight must be finite and non-negative"
        ));
    }
    Ok(())
}

fn validate_ec_beam_pressure_deadzone(deadzone: f64, rate: &str) -> Result<(), String> {
    if !deadzone.is_finite() || !(0.0..=1.0).contains(&deadzone) {
        return Err(format!(
            "{rate} EcBeam pressure dead-zone must be finite and between 0 and 1"
        ));
    }
    Ok(())
}

fn validate_ec_dc_corner_hz(corner_hz: f64, rate: &str) -> Result<(), String> {
    if !corner_hz.is_finite() || corner_hz <= 0.0 || corner_hz > 2_000.0 {
        return Err(format!(
            "{rate} EC DC corner must be finite, positive, and at most 2000 Hz"
        ));
    }
    Ok(())
}

fn validate_ec2_policy_weights(weights: Ec2PolicyWeights) -> Result<(), String> {
    for (name, value) in [
        ("DSD128 EC quantizer weight", weights.quantizer_weight),
        ("DSD128 EC pressure weight", weights.pressure_weight),
        ("DSD128 EC limit weight", weights.limit_weight),
        ("DSD128 EC transition weight", weights.transition_weight),
        ("DSD128 EC DC weight", weights.dc_weight),
        ("DSD128 EC lookahead discount", weights.lookahead_discount),
        ("DSD128 EC ambiguity margin", weights.ambiguity_margin),
        (
            "DSD128 EC pressure taper start",
            weights.pressure_taper_start,
        ),
        (
            "DSD128 EC pressure taper strength",
            weights.pressure_taper_strength,
        ),
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(format!("{name} must be finite and non-negative"));
        }
    }
    if weights.lookahead_discount > 1.0 {
        return Err("DSD128 EC lookahead discount must be between 0 and 1".into());
    }
    if weights.pressure_taper_start > 1.0 {
        return Err("DSD128 EC pressure taper start must be between 0 and 1".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecbeam2_manifests_use_generated_fixtures_only() {
        for name in ["calibration", "stability_short", "selection", "held_out"] {
            let path = Path::new("audio_tests/ecbeam2/manifests").join(format!("{name}.json"));
            let (manifest, digest, _) =
                load_ecbeam2_corpus_manifest(&path).expect("EcBeam2 manifest should load");
            assert_eq!(digest.len(), 64);
            assert!(
                manifest
                    .fixtures
                    .iter()
                    .all(|fixture| fixture.kind == "generated"),
                "{name} contains a non-generated fixture"
            );
        }
    }

    #[test]
    fn ecbeam2_selection_materializes_both_wire_families() {
        let path = Path::new("audio_tests/ecbeam2/manifests/selection.json");
        let (manifest, _, manifest_dir) =
            load_ecbeam2_corpus_manifest(path).expect("selection manifest should load");
        for source_rate in [44_100, 48_000] {
            let cases = materialize_ecbeam2_corpus_cases(&manifest, &manifest_dir, source_rate)
                .expect("generated cases should materialize");
            assert_eq!(cases.len(), manifest.fixtures.len());
            assert!(cases.iter().all(|case| !case.fixture.left.is_empty()));
        }
    }

    #[test]
    fn ecbeam2_generator_hash_is_manifest_bound() {
        let path = Path::new("audio_tests/ecbeam2/manifests/calibration.json");
        let (mut manifest, _, manifest_dir) =
            load_ecbeam2_corpus_manifest(path).expect("calibration manifest should load");
        manifest.fixtures[0].generator = Some("pink_noise|seed=0xc001|v1".to_string());
        let error = validate_ecbeam2_corpus_manifest_contents(&manifest, &manifest_dir)
            .expect_err("changed generator must be rejected");
        assert!(error.contains("hash mismatch"));
    }
}

#[derive(Debug, Serialize)]
pub struct SuiteReport {
    pub mode: String,
    pub git_commit: Option<String>,
    pub pcm: Vec<PcmMeasurement>,
    pub dsd: Vec<DsdMeasurement>,
}

#[derive(Debug, Serialize)]
struct DsdDecisionSummary {
    mode: String,
    git_commit: Option<String>,
    target_profile: String,
    candidates: Vec<DsdDecisionCandidate>,
}

#[derive(Debug, Serialize)]
struct DsdDecisionCandidate {
    filter: String,
    modulator: String,
    path_variant: String,
    source_rate: u32,
    origin_source_rate: u32,
    renderer_source_rate: u32,
    intermediate_rate: Option<u32>,
    intermediate_bits: Option<u32>,
    intermediate_filter: Option<String>,
    path_prepare_ms: Option<f64>,
    render_ms: Option<f64>,
    dsd_rate: String,
    status: String,
    score: f64,
    score_sections: DsdScoreSections,
    hard_failures: Vec<String>,
    missing_constraints: Vec<String>,
    metrics: Vec<DsdTargetMetric>,
    notes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DsdTargetMetric {
    metric: String,
    value: Option<f64>,
    band: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    caveat: Option<String>,
    good_threshold: Option<f64>,
    excellent_threshold: Option<f64>,
    stretch_threshold: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct Ec4aCrackleMeasurement {
    pub filter: String,
    pub source_rate: u32,
    pub dsd_rate: String,
    pub modulator: String,
    pub seconds: f64,
    pub decoded_frames: usize,
    pub left_click_candidates: usize,
    pub right_click_candidates: usize,
    pub left_max_click_score: f64,
    pub right_max_click_score: f64,
    pub left_max_click_residual: f64,
    pub right_max_click_residual: f64,
    pub decoded_abs_peak: Option<f64>,
    pub stability_resets: u64,
    pub state_clamps: u64,
    pub total_commits: u64,
    pub depth4_commits: u64,
    pub depth4_ratio: f64,
    pub trigger_guard_selected: u64,
    pub trigger_pressure_selected: u64,
    pub trigger_transient_selected: u64,
    pub trigger_ambiguity_selected: u64,
    pub budget_starved: u64,
    pub max_hold_seen: u32,
    pub adaptive_decision_trace: Option<DsdAdaptiveDecisionTrace>,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PcmMeasurement {
    pub filter: String,
    pub source_rate: u32,
    pub target_rate: u32,
    pub dc_gain_db: Option<f64>,
    pub gain_1k_db: Option<f64>,
    pub gain_18k_db: Option<f64>,
    pub passband_ripple_db: Option<f64>,
    pub passband_profile: PassbandProfile,
    pub image_rejection_db: Option<f64>,
    pub impulse_peak_index: Option<usize>,
    pub pre_ringing_energy_db: Option<f64>,
    pub post_ringing_energy_db: Option<f64>,
    pub latency_ms: f64,
    pub memory_bytes: usize,
    pub ns_per_output_frame: Option<f64>,
    pub one_core_percent: Option<f64>,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DsdMeasurement {
    pub modulator: String,
    pub filter: String,
    pub path_variant: String,
    pub source_rate: u32,
    pub origin_source_rate: u32,
    pub renderer_source_rate: u32,
    pub intermediate_rate: Option<u32>,
    pub intermediate_bits: Option<u32>,
    pub intermediate_filter: Option<String>,
    pub path_prepare_ms: Option<f64>,
    pub render_ms: Option<f64>,
    pub dsd_rate: String,
    pub wire_rate: Option<u32>,
    pub passband_profile: PassbandProfile,
    pub residual_db: Option<f64>,
    pub thdn_residual_db: Option<f64>,
    /// Median in-band SINAD across the DSD-domain analysis windows.
    pub inband_snr_db: Option<f64>,
    pub inband_snr_worst_db: Option<f64>,
    pub inband_snr_p05_db: Option<f64>,
    pub inband_snr_p95_db: Option<f64>,
    pub inband_snr_best_db: Option<f64>,
    pub inband_snr_spread_db: Option<f64>,
    pub inband_snr_left_db: Option<f64>,
    pub inband_snr_right_db: Option<f64>,
    pub inband_snr_left_worst_db: Option<f64>,
    pub inband_snr_right_worst_db: Option<f64>,
    pub inband_snr_left_spread_db: Option<f64>,
    pub inband_snr_right_spread_db: Option<f64>,
    /// Low-frequency SINAD diagnostics from a dedicated ~100 Hz coherent-tone
    /// render. Populated only when `FOZMO_DSD_MEASURE_LF_SINAD` is set (the pass
    /// costs a second full render); `None` otherwise. Added for Workstream D to
    /// detect in-band servo damage from DC-bias-tracker corner changes, which
    /// the 1 kHz SINAD tone is too high to see.
    pub inband_lf_sinad_worst_db: Option<f64>,
    pub inband_lf_sinad_db: Option<f64>,
    pub inband_lf_tone_hz: Option<f64>,
    pub stereo_snr_worst_mismatch_db: Option<f64>,
    pub inband_snr_window_count: Option<usize>,
    pub inband_snr_worst_window_start_s: Option<f64>,
    pub inband_noise_rms_dbfs: Option<f64>,
    pub inband_noise_worst_rms_dbfs: Option<f64>,
    pub inband_noise_peak_dbfs: Option<f64>,
    pub inband_noise_peak_spur_hz: Option<f64>,
    pub inband_noise_spur_margin_db: Option<f64>,
    pub inband_noise_left_spur_margin_db: Option<f64>,
    pub inband_noise_right_spur_margin_db: Option<f64>,
    pub inband_noise_20_200_dbfs: Option<f64>,
    pub inband_noise_200_2k_dbfs: Option<f64>,
    pub inband_noise_2k_8k_dbfs: Option<f64>,
    pub inband_noise_8k_16k_dbfs: Option<f64>,
    pub inband_noise_16k_20k_dbfs: Option<f64>,
    pub ultrasonic_24_50k_max_dbfs: Option<f64>,
    pub ultrasonic_24_50k_median_dbfs: Option<f64>,
    pub ultrasonic_24_50k_window_spread_db: Option<f64>,
    pub ultrasonic_50_100k_max_dbfs: Option<f64>,
    pub ultrasonic_50_100k_median_dbfs: Option<f64>,
    pub ultrasonic_50_100k_window_spread_db: Option<f64>,
    pub ultrasonic_100_200k_max_dbfs: Option<f64>,
    pub ultrasonic_100_200k_median_dbfs: Option<f64>,
    pub ultrasonic_100_200k_window_spread_db: Option<f64>,
    pub inband_spurs: Vec<DsdInbandSpurRow>,
    pub inband_windows: Vec<DsdInbandWindowRow>,
    pub ultrasonic_windows: Vec<DsdUltrasonicWindowRow>,
    pub premod_windows: Vec<DsdPremodWindowRow>,
    pub idle_tone_dbfs: Option<f64>,
    pub idle_worst_tone_dbfs: Option<f64>,
    pub idle_worst_density_deviation: Option<f64>,
    pub idle_artifacts: Vec<DsdIdleArtifactRow>,
    pub overload_recovery_diagnostics: Vec<DsdOverloadRecoveryDiagnosticRow>,
    pub low_level_worst_residual_db: Option<f64>,
    pub low_level_worst_spur_dbfs: Option<f64>,
    pub high_freq_tone_worst_residual_db: Option<f64>,
    pub high_freq_tone_worst_spur_dbfs: Option<f64>,
    pub high_freq_imd_residual_db: Option<f64>,
    pub high_freq_imd_spur_dbfs: Option<f64>,
    pub high_freq_worst_residual_db: Option<f64>,
    pub high_freq_worst_spur_dbfs: Option<f64>,
    pub multitone_residual_db: Option<f64>,
    pub multitone_spur_dbfs: Option<f64>,
    pub overload_recovery_dbfs: Option<f64>,
    pub transient_click_candidates: Option<usize>,
    pub transient_click_max_score: Option<f64>,
    pub transient_click_max_residual: Option<f64>,
    pub program_click_candidates: Option<usize>,
    pub program_click_max_score: Option<f64>,
    pub program_click_max_residual: Option<f64>,
    pub decoded_low: Option<f64>,
    pub decoded_peak: Option<f64>,
    pub decoded_abs_peak: Option<f64>,
    pub bit_density: Option<f64>,
    pub bit_density_left: Option<f64>,
    pub bit_density_right: Option<f64>,
    pub bit_density_max_deviation: Option<f64>,
    pub bit_density_left_max_deviation: Option<f64>,
    pub bit_density_right_max_deviation: Option<f64>,
    pub transition_rate: Option<f64>,
    pub limiter_peak_ratio_max: Option<f64>,
    pub limiter_current_block_peak_ratio: Option<f64>,
    pub limiter_current_block_gain: Option<f64>,
    pub limiter_current_block_limited_samples: u64,
    pub limiter_limited_events: u64,
    pub limiter_limited_samples: u64,
    pub stability_resets: u64,
    pub state_clamps: u64,
    pub stress_stability_resets: u64,
    pub stress_state_clamps: u64,
    /// EC-4A depth-4 duty ratio over the main measurement render (None for
    /// non-adaptive modulators).
    pub depth4_ratio: Option<f64>,
    pub adaptive_decision_trace: Option<DsdAdaptiveDecisionTrace>,
    pub ec2_decision_trace: Option<DsdEc2DecisionTrace>,
    pub dsd256_improvement_db: Option<f64>,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DsdRoundtripReport {
    pub mode: String,
    pub git_commit: Option<String>,
    pub measurements: Vec<DsdRoundtripMeasurement>,
    pub baseline_deltas: Vec<DsdRoundtripBaselineDelta>,
    pub pink_noise_seed_summaries: Vec<DsdRoundtripPinkNoiseSeedSummary>,
    #[serde(skip)]
    artifacts: DsdRoundtripArtifacts,
}

#[derive(Debug, Serialize)]
pub struct DsdPrecheckReport {
    pub mode: String,
    pub git_commit: Option<String>,
    pub measurements: Vec<DsdPrecheckMeasurement>,
}

#[derive(Debug, Serialize)]
pub struct DsdPrecheckMeasurement {
    pub candidate_index: usize,
    pub probe: String,
    pub filter: String,
    pub modulator: String,
    pub source_rate: u32,
    pub dsd_rate: String,
    pub wire_rate: u32,
    pub seconds: f64,
    pub frames: usize,
    pub render_ms: Option<f64>,
    pub decoded_abs_peak: Option<f64>,
    pub bit_density: Option<f64>,
    pub bit_density_left: Option<f64>,
    pub bit_density_right: Option<f64>,
    pub bit_density_max_deviation: Option<f64>,
    pub bit_density_left_max_deviation: Option<f64>,
    pub bit_density_right_max_deviation: Option<f64>,
    pub limiter_peak_ratio_max: Option<f64>,
    pub limiter_limited_events: u64,
    pub limiter_limited_samples: u64,
    pub stability_resets: u64,
    pub state_clamps: u64,
    pub status: String,
    pub hard_failures: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug)]
pub struct DsdTriageOptions {
    pub candidate_label: String,
    pub candidate_modulator: DsdModulator,
    pub candidate_config: DsdExperimentConfig,
    pub fixture_manifest_path: PathBuf,
    pub baseline_cache_dir: PathBuf,
    pub out_dir: Option<PathBuf>,
    pub target_wall_seconds: f64,
    pub workers: usize,
    pub build_baseline_cache: bool,
    pub allow_slow: bool,
    pub reproduction_command: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DsdTriageReport {
    pub mode: String,
    pub run_id: String,
    pub candidate_label: String,
    pub out_dir: PathBuf,
    pub git_commit: Option<String>,
    pub target_wall_seconds: f64,
    pub hit_wall_target: bool,
    pub wall_ms_total: f64,
    pub render_ms_total: f64,
    pub measurements: Vec<DsdTriageMetric>,
    pub baselines: Vec<DsdTriageMetric>,
    pub scores: DsdTriageScores,
    pub baseline_cache_manifest: DsdTriageBaselineCacheManifest,
    pub fixture_manifest: DsdTriageResolvedFixtureManifest,
    pub hard_failures: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DsdTriageMetric {
    pub run_id: String,
    pub host_id: String,
    pub git_commit: Option<String>,
    pub measurement_version: String,
    pub scoring_version: String,
    pub fixture_set_version: String,
    pub candidate_label: String,
    pub fixture_id: String,
    pub fixture_class: String,
    pub rate: String,
    #[serde(default)]
    pub native_left_sha256: Option<String>,
    #[serde(default)]
    pub native_right_sha256: Option<String>,
    pub status: String,
    pub render_ms: Option<f64>,
    pub wall_ms: Option<f64>,
    pub residual_relative_db: Option<f64>,
    pub residual_rms_dbfs: Option<f64>,
    pub residual_peak_dbfs: Option<f64>,
    pub worst_window_residual_dbfs: Option<f64>,
    pub residual_peak_spur_dbfs: Option<f64>,
    pub residual_peak_spur_hz: Option<f64>,
    pub residual_peak_to_median_db: Option<f64>,
    pub residual_20_10k_rms_dbfs: Option<f64>,
    pub residual_20_20k_rms_dbfs: Option<f64>,
    pub residual_12_22k_rms_dbfs: Option<f64>,
    pub sine_tone_fit_sinad_db: Option<f64>,
    pub sine_tone_fit_residual_dbfs: Option<f64>,
    #[serde(default)]
    pub sine_tone_fit_20_10k_sinad_db: Option<f64>,
    #[serde(default)]
    pub sine_tone_fit_20_10k_residual_dbfs: Option<f64>,
    #[serde(default)]
    pub sine_tone_fit_4_10k_sinad_db: Option<f64>,
    #[serde(default)]
    pub sine_tone_fit_4_10k_residual_dbfs: Option<f64>,
    pub multitone_fit_sinad_db: Option<f64>,
    pub multitone_fit_residual_dbfs: Option<f64>,
    pub dsd_ultrasonic_24_50k_max_dbfs: Option<f64>,
    pub dsd_ultrasonic_50_100k_max_dbfs: Option<f64>,
    pub dsd_ultrasonic_100_200k_max_dbfs: Option<f64>,
    pub idle_bit_density: Option<f64>,
    pub idle_bit_density_max_deviation: Option<f64>,
    pub fixture_bit_density: Option<f64>,
    pub fixture_bit_density_max_deviation: Option<f64>,
    #[serde(default)]
    pub expected_gain_db: Option<f64>,
    pub fitted_gain_db: Option<f64>,
    #[serde(default)]
    pub delta_gain_vs_expected_db: Option<f64>,
    #[serde(default)]
    pub delta_gain_vs_production_contract_db: Option<f64>,
    #[serde(default)]
    pub delta_decoded_peak_vs_production_contract_db: Option<f64>,
    pub decoded_peak_dbfs: Option<f64>,
    pub decoded_abs_peak: Option<f64>,
    pub state_clamps: u64,
    pub stability_resets: u64,
    pub limiter_limited_events: u64,
    pub limiter_limited_samples: u64,
    pub source_peak_dbfs: Option<f64>,
    pub source_rms_dbfs: Option<f64>,
    pub source_clip_count: Option<u64>,
    pub delta_residual_rms_vs_standard_db: Option<f64>,
    pub delta_residual_rms_vs_prod_ec2_db: Option<f64>,
    pub delta_spur_peak_vs_standard_db: Option<f64>,
    pub delta_spur_peak_vs_prod_ec2_db: Option<f64>,
    pub delta_direction_residual_rms: String,
    pub delta_direction_spur_peak: String,
    pub hard_failures: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DsdTriageScores {
    pub run_id: String,
    pub candidate_label: String,
    pub status: String,
    pub decision: String,
    pub score_total_raw: f64,
    pub score_anchor: f64,
    pub score_delta_from_anchor: f64,
    pub score_real_world: f64,
    pub score_synthetic: f64,
    pub score_edge_case: f64,
    pub render_ms_total: f64,
    pub wall_ms_total: f64,
    pub render_factor_vs_standard: Option<f64>,
    pub render_factor_vs_prod_ec2: Option<f64>,
    pub hit_wall_target: bool,
    pub primary_win: String,
    pub primary_weakness: String,
    pub primary_rejection_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DsdTriageFixtureManifest {
    pub fixture_set_version: String,
    pub real_world: DsdTriageRealWorldFixtureManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DsdTriageRealWorldFixtureManifest {
    pub id: String,
    pub label: String,
    pub path: String,
    pub start_sec: f64,
    pub end_sec: f64,
    pub duration_sec: f64,
    pub source_format: Option<String>,
    pub role: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DsdTriageResolvedFixtureManifest {
    pub fixture_set_version: String,
    pub real_world: DsdTriageResolvedRealWorldFixture,
    pub synthetic_fixture_id: String,
    pub mid_band_fixture_id: String,
    pub edge_fixture_id: String,
    pub stress_fixture_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DsdTriageResolvedRealWorldFixture {
    pub id: String,
    pub label: String,
    pub path: String,
    pub start_sec: f64,
    pub end_sec: f64,
    pub duration_sec: f64,
    pub source_format: Option<String>,
    pub role: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DsdTriageBaselineCacheManifest {
    pub cache_dir: PathBuf,
    pub entries: Vec<DsdTriageBaselineCacheEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DsdTriageBaselineCacheEntry {
    pub baseline_label: String,
    pub fixture_id: String,
    pub path: PathBuf,
    pub config_hash: String,
    pub cache_hit: bool,
    pub built: bool,
}

#[derive(Debug, Clone)]
struct DsdTriageFixture {
    id: String,
    class: &'static str,
    left: Vec<f64>,
    right: Vec<f64>,
    source_peak_dbfs: Option<f64>,
    source_rms_dbfs: Option<f64>,
    source_clip_count: Option<u64>,
    /// Optional source-domain region to score after the renderer has consumed
    /// the full fixture prefix. This preserves the CRFB/profile starting state
    /// frozen by difficult-window manifests without letting the prefix replace
    /// the requested window's reconstruction metrics.
    analysis_start_sample: Option<usize>,
    analysis_length_samples: Option<usize>,
}

const ECBEAM2_CORPUS_SCHEMA_VERSION: &str = "ecbeam2-corpus-v1";
const ECBEAM2_CORPUS_REPORT_SCHEMA_VERSION: &str = "ecbeam2-corpus-report-v1";

#[derive(Debug, Clone, Deserialize)]
struct EcBeam2CorpusManifest {
    schema_version: String,
    corpus_id: String,
    role: String,
    measurement_version: String,
    scoring_version: String,
    fixture_set_version: String,
    source_rates: Vec<u32>,
    wire_rates: Vec<u32>,
    filters: Vec<String>,
    seeds: Vec<u64>,
    fixtures: Vec<EcBeam2CorpusFixtureSpec>,
    difficult_windows: Vec<EcBeam2CorpusWindowSpec>,
}

#[derive(Debug, Clone, Deserialize)]
struct EcBeam2CorpusFixtureSpec {
    id: String,
    kind: String,
    generator: Option<String>,
    generator_spec_sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct EcBeam2CorpusWindowSpec {
    case_id: String,
    fixture_id: String,
    category: String,
    source_rate: u32,
    start_sample: usize,
    length_samples: usize,
}

#[derive(Debug, Clone)]
struct EcBeam2MaterializedCase {
    case_id: String,
    fixture_id: String,
    category: String,
    start_sample: usize,
    length_samples: usize,
    generator_seed: Option<u64>,
    fixture: DsdTriageFixture,
}

#[derive(Debug, Clone, Serialize)]
pub struct EcBeam2CorpusMeasurement {
    pub manifest_sha256: String,
    pub corpus_id: String,
    pub role: String,
    pub case_id: String,
    pub fixture_id: String,
    pub category: String,
    pub source_rate: u32,
    pub wire_rate: u32,
    pub filter: String,
    pub modulator: String,
    pub generator_seed: Option<u64>,
    pub start_sample: usize,
    pub length_samples: usize,
    pub ecbeam2_diagnostics: Option<EcBeam2CorpusDiagnostics>,
    pub metric: DsdTriageMetric,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct EcBeam2CorpusDiagnostics {
    pub committed_samples: u64,
    pub min_survivors: u64,
    pub constraint_escape: u64,
    pub state_repair_fallback: u64,
    pub all_nonfinite_resets: u64,
    pub observer_desynchronizations: u64,
    pub invalid_input_substitutions: u64,
    pub output_length_error: u64,
    pub committed_output_energy: f64,
    pub committed_output_energy_mean: f64,
    pub ultrasonic_ema_max: f64,
    pub signed_error_ema_abs_max: f64,
    pub ultrasonic_ema_p99_9: f64,
    pub ultrasonic_ema_p99_99: f64,
    pub signed_error_ema_abs_p99_9: f64,
    pub signed_error_ema_abs_p99_99: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EcBeam2CorpusCellSummary {
    pub filter: String,
    pub modulator: String,
    pub source_rate: u32,
    pub wire_rate: u32,
    pub rendered_cases: usize,
    pub rendered_fixtures: usize,
    pub seeds: Vec<u64>,
    pub worst_sinad_db: Option<f64>,
    pub spur_margin_db: Option<f64>,
    pub high_frequency_residual_db: Option<f64>,
    pub multitone_residual_db: Option<f64>,
    pub narrowband_peak_dbfs: Option<f64>,
    pub overload_recovery_dbfs: Option<f64>,
    pub worst_window_residual_dbfs: Option<f64>,
    pub decoded_abs_peak: Option<f64>,
    pub bit_density_max_deviation: Option<f64>,
    pub render_ms: f64,
    pub limiter_limited_events: u64,
    pub limiter_limited_samples: u64,
    pub stability_resets: u64,
    pub state_clamps: u64,
    pub hard_failure_count: usize,
    pub ecbeam2_diagnostics_present: bool,
    pub ecbeam2_committed_samples: u64,
    pub ecbeam2_min_survivors: Option<u64>,
    pub ecbeam2_constraint_escape: u64,
    pub ecbeam2_state_repair_fallback: u64,
    pub ecbeam2_all_nonfinite_resets: u64,
    pub ecbeam2_observer_desynchronizations: u64,
    pub ecbeam2_invalid_input_substitutions: u64,
    pub ecbeam2_output_length_error: u64,
    pub ecbeam2_committed_output_energy: f64,
    pub ecbeam2_committed_output_energy_mean: Option<f64>,
    pub ecbeam2_ultrasonic_ema_max: Option<f64>,
    pub ecbeam2_signed_error_ema_abs_max: Option<f64>,
    pub ecbeam2_ultrasonic_ema_p99_9: Option<f64>,
    pub ecbeam2_ultrasonic_ema_p99_99: Option<f64>,
    pub ecbeam2_signed_error_ema_abs_p99_9: Option<f64>,
    pub ecbeam2_signed_error_ema_abs_p99_99: Option<f64>,
    pub ecbeam2_ultrasonic_ema_worst_case: Option<String>,
    pub ecbeam2_ultrasonic_ema_p99_9_worst_case: Option<String>,
    pub ecbeam2_ultrasonic_ema_p99_99_worst_case: Option<String>,
    pub ecbeam2_signed_error_ema_worst_case: Option<String>,
    pub ecbeam2_signed_error_ema_p99_9_worst_case: Option<String>,
    pub ecbeam2_signed_error_ema_p99_99_worst_case: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EcBeam2CorpusReport {
    pub schema_version: String,
    pub corpus_schema_version: String,
    pub manifest_sha256: String,
    pub corpus_id: String,
    pub role: String,
    pub measurement_version: String,
    pub scoring_version: String,
    pub fixture_set_version: String,
    pub declared_source_rates: Vec<u32>,
    pub declared_wire_rates: Vec<u32>,
    pub declared_filters: Vec<String>,
    pub declared_seeds: Vec<u64>,
    pub selected_source_rates: Vec<u32>,
    pub selected_wire_rates: Vec<u32>,
    pub selected_filters: Vec<String>,
    pub selected_modulators: Vec<String>,
    pub expected_fixture_cells: usize,
    pub rendered_fixture_cells: usize,
    pub measurements: Vec<EcBeam2CorpusMeasurement>,
    pub cell_summaries: Vec<EcBeam2CorpusCellSummary>,
    pub hard_failures: Vec<String>,
}

const ECBEAM2_QUALIFICATION_SCHEMA_VERSION: &str = "ecbeam2-qualification-report-v1";

#[derive(Debug, Clone, Serialize)]
pub struct EcBeam2QualificationMeasurement {
    pub case_id: String,
    pub fixture_id: String,
    pub category: String,
    pub source_rate: u32,
    pub wire_rate: u32,
    pub filter: String,
    pub start_sample: usize,
    pub length_samples: usize,
    pub generator_seed: Option<u64>,
    pub input_frames: usize,
    pub native_left_bytes: usize,
    pub native_right_bytes: usize,
    pub native_left_sha256: String,
    pub native_right_sha256: String,
    pub native_stereo_sha256: String,
    pub render_ms: f64,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EcBeam2QualificationReport {
    pub schema_version: String,
    pub mode: String,
    pub corpus_schema_version: String,
    pub manifest_sha256: String,
    pub corpus_id: String,
    pub role: String,
    pub selected_source_rates: Vec<u32>,
    pub selected_wire_rates: Vec<u32>,
    pub selected_filters: Vec<String>,
    pub selected_modulator: String,
    pub git_commit: Option<String>,
    pub working_tree_dirty: Option<bool>,
    pub binary_sha256: Option<String>,
    pub candidate_config_sha256: Option<String>,
    pub measurements: Vec<EcBeam2QualificationMeasurement>,
}

fn ecbeam2_qualification_note_allowed(mode: &str, note: &str) -> bool {
    let key = note.split_once('=').map(|(key, _)| key).unwrap_or(note);
    let common = matches!(
        key,
        "ecbeam2_committed_samples"
            | "ecbeam2_total_committed_samples"
            | "ecbeam2_min_survivors"
            | "ecbeam2_constraint_escape"
            | "ecbeam2_state_repair_fallback"
            | "ecbeam2_all_nonfinite_resets"
            | "ecbeam2_observer_desynchronizations"
            | "ecbeam2_invalid_input_substitutions"
            | "ecbeam2_output_length_error"
            | "ecbeam2_renderer_truncation_events"
            | "ecbeam2_renderer_discarded_left_bits"
            | "ecbeam2_renderer_discarded_right_bits"
            | "ecbeam2_committed_output_energy"
            | "ecbeam2_committed_output_energy_mean"
            | "ecbeam2_maximum_state_overflow"
            | "ecbeam2_state_repair_stage_counts"
    );
    common
        || match mode {
            "scale-probe" => key.starts_with("ecbeam2_scale_"),
            "stability" => matches!(
                key,
                "ecbeam2_first_constraint_escape_sequence"
                    | "ecbeam2_first_state_repair_sequence"
                    | "ecbeam2_last_constraint_escape_sequence"
                    | "ecbeam2_last_state_repair_sequence"
                    | "ecbeam2_maximum_state_overflow"
                    | "ecbeam2_maximum_consecutive_constraint_escapes"
                    | "ecbeam2_maximum_consecutive_state_repairs"
                    | "ecbeam2_state_repair_stage_counts"
                    | "ecbeam2_maximum_normalized_state_by_stage"
            ),
            "budget" => matches!(
                key,
                "ecbeam2_maximum_budget_violation"
                    | "ecbeam2_ultrasonic_budget_escape_count"
                    | "ecbeam2_signed_error_budget_escape_count"
                    | "ecbeam2_both_budget_escape_count"
                    | "ecbeam2_ultrasonic_ema_max"
                    | "ecbeam2_signed_error_ema_abs_max"
            ),
            _ => false,
        }
}

/// Render-only EcBeam2 corpus qualification. This deliberately excludes DSD
/// decoding, FFTs, residual scoring, broad fixtures, and selectable rankings.
#[allow(clippy::too_many_arguments)]
pub fn run_ecbeam2_qualification(
    mode: &str,
    path: &Path,
    filters: &[SelectableDsdFilter],
    source_rates: &[u32],
    modulator: DsdModulator,
    config: DsdExperimentConfig,
    binary_sha256: Option<String>,
    candidate_config_sha256: Option<String>,
) -> Result<EcBeam2QualificationReport, String> {
    if !matches!(mode, "scale-probe" | "stability" | "budget") {
        return Err(format!("unsupported EcBeam2 qualification mode {mode}"));
    }
    if filters.is_empty() || source_rates.is_empty() {
        return Err("EcBeam2 qualification axes must be non-empty".to_string());
    }
    if !matches!(modulator, DsdModulator::EcBeam | DsdModulator::EcBeam2) {
        return Err("EcBeam2 qualification supports only EcBeam and EcBeam2".to_string());
    }
    let (manifest, manifest_sha256, manifest_dir) = load_ecbeam2_corpus_manifest(path)?;
    validate_ecbeam2_corpus_axes(&manifest, filters, source_rates)?;
    config.validate_for_rates(&[DsdRate::Dsd64])?;
    if config.dsd64_tweaks.ecbeam2_config.is_none() {
        return Err("EcBeam2 qualification requires an EcBeam2 candidate config".to_string());
    }

    let selected_wire_rates = source_rates
        .iter()
        .map(|source_rate| {
            DsdRate::Dsd64
                .wire_rate_for_source(*source_rate)
                .ok_or_else(|| format!("unsupported DSD64 source rate {source_rate}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut measurements = Vec::new();
    for &source_rate in source_rates {
        let wire_rate = DsdRate::Dsd64
            .wire_rate_for_source(source_rate)
            .ok_or_else(|| format!("unsupported DSD64 source rate {source_rate}"))?;
        for case in materialize_ecbeam2_corpus_cases(&manifest, &manifest_dir, source_rate)? {
            for filter in filters {
                let frames = case.fixture.left.len().min(case.fixture.right.len());
                let analysis_start = case.fixture.analysis_start_sample.unwrap_or(0);
                let analysis_length = case
                    .fixture
                    .analysis_length_samples
                    .unwrap_or_else(|| frames.saturating_sub(analysis_start));
                let mut effective_config = config;
                configure_ecbeam2_diagnostic_window(
                    &mut effective_config,
                    filter.filter,
                    source_rate,
                    wire_rate,
                    analysis_start,
                    analysis_length,
                )?;
                let mut renderer = new_dsd_renderer(
                    filter.filter,
                    source_rate,
                    DsdRate::Dsd64,
                    modulator,
                    effective_config,
                )
                .map_err(|err| format!("EcBeam2 qualification renderer init failed: {err}"))?;
                renderer.set_native_order(NativeDsdOrder::MsbFirst);
                let started = Instant::now();
                let (native_left, native_right) = render_native_stream(
                    &mut renderer,
                    &case.fixture.left[..frames],
                    &case.fixture.right[..frames],
                );
                let render_ms = started.elapsed().as_secs_f64() * 1000.0;
                let mut notes = Vec::new();
                append_ecbeam2_diagnostics_notes(&mut notes, &renderer);
                notes.retain(|note| ecbeam2_qualification_note_allowed(mode, note));
                notes.push(format!("stability_resets={}", renderer.stability_resets()));
                notes.push(format!("state_clamps={}", renderer.state_clamps()));
                let mut stereo_digest = Sha256::new();
                stereo_digest.update((native_left.len() as u64).to_le_bytes());
                stereo_digest.update(&native_left);
                stereo_digest.update((native_right.len() as u64).to_le_bytes());
                stereo_digest.update(&native_right);
                measurements.push(EcBeam2QualificationMeasurement {
                    case_id: case.case_id.clone(),
                    fixture_id: case.fixture_id.clone(),
                    category: case.category.clone(),
                    source_rate,
                    wire_rate,
                    filter: filter.name.to_string(),
                    start_sample: case.start_sample,
                    length_samples: case.length_samples,
                    generator_seed: case.generator_seed,
                    input_frames: frames,
                    native_left_bytes: native_left.len(),
                    native_right_bytes: native_right.len(),
                    native_left_sha256: sha256_hex(&native_left),
                    native_right_sha256: sha256_hex(&native_right),
                    native_stereo_sha256: format!("{:x}", stereo_digest.finalize()),
                    render_ms,
                    notes,
                });
            }
        }
    }
    Ok(EcBeam2QualificationReport {
        schema_version: ECBEAM2_QUALIFICATION_SCHEMA_VERSION.to_string(),
        mode: mode.to_string(),
        corpus_schema_version: ECBEAM2_CORPUS_SCHEMA_VERSION.to_string(),
        manifest_sha256,
        corpus_id: manifest.corpus_id,
        role: manifest.role,
        selected_source_rates: source_rates.to_vec(),
        selected_wire_rates,
        selected_filters: filters
            .iter()
            .map(|filter| filter.name.to_string())
            .collect(),
        selected_modulator: modulator.as_name().to_string(),
        git_commit: git_commit(),
        working_tree_dirty: working_tree_dirty(),
        binary_sha256,
        candidate_config_sha256,
        measurements,
    })
}

pub fn write_ecbeam2_qualification_artifact(
    report: &EcBeam2QualificationReport,
    out_dir: &Path,
) -> Result<(), String> {
    fs::create_dir_all(out_dir).map_err(|err| err.to_string())?;
    let json = serde_json::to_string_pretty(report).map_err(|err| err.to_string())?;
    fs::write(out_dir.join("ecbeam2_qualification_report.json"), json)
        .map_err(|err| err.to_string())
}

#[derive(Debug)]
struct DsdTriageSpectrumMetrics {
    peak_spur_dbfs: Option<f64>,
    peak_spur_hz: Option<f64>,
    peak_to_median_db: Option<f64>,
    band_20_10k_rms_dbfs: Option<f64>,
    band_20_20k_rms_dbfs: Option<f64>,
    band_12_22k_rms_dbfs: Option<f64>,
}

#[derive(Debug)]
struct TriageFitMetrics {
    sinad_db: Option<f64>,
    residual_dbfs: Option<f64>,
    band_20_10k_sinad_db: Option<f64>,
    band_20_10k_residual_dbfs: Option<f64>,
    band_4_10k_sinad_db: Option<f64>,
    band_4_10k_residual_dbfs: Option<f64>,
}

#[derive(Debug)]
struct TriageScoredDelta {
    fixture_id: String,
    metric_name: &'static str,
    better_delta_db: f64,
    points: f64,
}

pub fn run_ecbeam2_corpus_manifest(
    path: &Path,
    filters: &[SelectableDsdFilter],
    rates: &[DsdRate],
    modulators: &[DsdModulator],
    source_rates: &[u32],
    config: DsdExperimentConfig,
) -> Result<EcBeam2CorpusReport, String> {
    if filters.is_empty() || rates.is_empty() || modulators.is_empty() || source_rates.is_empty() {
        return Err("EcBeam2 corpus execution axes must be non-empty".to_string());
    }
    if rates.iter().any(|rate| *rate != DsdRate::Dsd64) {
        return Err("EcBeam2 corpus execution supports DSD64 only".to_string());
    }
    let (manifest, manifest_sha256, manifest_dir) = load_ecbeam2_corpus_manifest(path)?;
    validate_ecbeam2_corpus_axes(&manifest, filters, source_rates)?;
    config.validate_for_rates(rates)?;

    let selected_wire_rates = source_rates
        .iter()
        .map(|source_rate| {
            DsdRate::Dsd64
                .wire_rate_for_source(*source_rate)
                .ok_or_else(|| format!("unsupported DSD64 source rate {source_rate}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let expected_fixture_cells = manifest.fixtures.len()
        * filters.len()
        * rates.len()
        * modulators.len()
        * source_rates.len();
    let expect_ecbeam2_diagnostics = config.dsd64_tweaks.ecbeam2_config.is_some();
    let mut measurements = Vec::new();

    for &source_rate in source_rates {
        let cases = materialize_ecbeam2_corpus_cases(&manifest, &manifest_dir, source_rate)?;
        for case in &cases {
            for filter in filters {
                for &rate in rates {
                    let wire_rate = rate
                        .wire_rate_for_source(source_rate)
                        .ok_or_else(|| format!("unsupported DSD64 source rate {source_rate}"))?;
                    for &modulator in modulators {
                        let measurement_modulator = dsd_modulator_measurement_name(
                            modulator,
                            production_tweaks_for(config, filter.filter, rate, modulator),
                        );
                        eprintln!(
                            "ecbeam2_quality: EcBeam2 corpus {} {} {} {} {}",
                            manifest.corpus_id,
                            case.case_id,
                            filter.name,
                            measurement_modulator,
                            wire_rate
                        );
                        let mut metric = measure_dsd_triage_case(
                            &manifest.corpus_id,
                            &measurement_modulator,
                            &case.fixture,
                            &manifest.fixture_set_version,
                            filter.filter,
                            rate,
                            source_rate,
                            modulator,
                            config,
                        )?;
                        metric.notes.extend([
                            format!("ecbeam2_corpus_id={}", manifest.corpus_id),
                            format!("ecbeam2_corpus_role={}", manifest.role),
                            format!("ecbeam2_corpus_case_id={}", case.case_id),
                            format!("ecbeam2_corpus_manifest_sha256={manifest_sha256}"),
                        ]);
                        if let Some(seed) = case.generator_seed {
                            metric
                                .notes
                                .push(format!("ecbeam2_corpus_generator_seed={seed}"));
                        }
                        let ecbeam2_diagnostics = ecbeam2_corpus_diagnostics(&metric);
                        let mut failures = metric.hard_failures.clone();
                        failures.extend(ecbeam2_corpus_health_failures(&metric));
                        if expect_ecbeam2_diagnostics && ecbeam2_diagnostics.is_none() {
                            failures.push("missing EcBeam2 corpus diagnostics".to_string());
                        }
                        if let Some(diagnostics) = &ecbeam2_diagnostics {
                            let expected_diagnostic_samples =
                                u64::try_from(case.length_samples).ok().and_then(|length| {
                                    let ratio = wire_rate.checked_div(source_rate)?;
                                    (wire_rate % source_rate == 0)
                                        .then_some(length)?
                                        .checked_mul(u64::from(ratio))?
                                        .checked_mul(2)
                                });
                            failures.extend(ecbeam2_corpus_diagnostic_failures(
                                diagnostics,
                                modulator == DsdModulator::EcBeam2,
                                expected_diagnostic_samples,
                            ));
                        }
                        failures.sort();
                        failures.dedup();
                        metric.hard_failures = failures;
                        metric.status = if metric.hard_failures.is_empty() {
                            "pass".to_string()
                        } else {
                            "fail".to_string()
                        };
                        measurements.push(EcBeam2CorpusMeasurement {
                            manifest_sha256: manifest_sha256.clone(),
                            corpus_id: manifest.corpus_id.clone(),
                            role: manifest.role.clone(),
                            case_id: case.case_id.clone(),
                            fixture_id: case.fixture_id.clone(),
                            category: case.category.clone(),
                            source_rate,
                            wire_rate,
                            filter: filter.name.to_string(),
                            modulator: measurement_modulator,
                            generator_seed: case.generator_seed,
                            start_sample: case.start_sample,
                            length_samples: case.length_samples,
                            ecbeam2_diagnostics,
                            metric,
                        });
                    }
                }
            }
        }
    }

    let cell_summaries = ecbeam2_corpus_cell_summaries(&measurements);
    let rendered_fixture_cells = ecbeam2_rendered_fixture_cell_count(&measurements);
    let mut hard_failures = measurements
        .iter()
        .flat_map(|measurement| {
            measurement.metric.hard_failures.iter().map(|failure| {
                format!(
                    "{}/{}/{}/{}/{}: {failure}",
                    measurement.case_id,
                    measurement.filter,
                    measurement.modulator,
                    measurement.source_rate,
                    measurement.wire_rate
                )
            })
        })
        .collect::<Vec<_>>();
    if rendered_fixture_cells != expected_fixture_cells {
        hard_failures.push(format!(
            "manifest fixture coverage mismatch: rendered {rendered_fixture_cells}, expected {expected_fixture_cells}"
        ));
    }

    Ok(EcBeam2CorpusReport {
        schema_version: ECBEAM2_CORPUS_REPORT_SCHEMA_VERSION.to_string(),
        corpus_schema_version: manifest.schema_version,
        manifest_sha256,
        corpus_id: manifest.corpus_id,
        role: manifest.role,
        measurement_version: manifest.measurement_version,
        scoring_version: manifest.scoring_version,
        fixture_set_version: manifest.fixture_set_version,
        declared_source_rates: manifest.source_rates,
        declared_wire_rates: manifest.wire_rates,
        declared_filters: manifest.filters,
        declared_seeds: manifest.seeds,
        selected_source_rates: source_rates.to_vec(),
        selected_wire_rates,
        selected_filters: filters
            .iter()
            .map(|filter| filter.name.to_string())
            .collect(),
        selected_modulators: modulators
            .iter()
            .map(|modulator| modulator.as_name().to_string())
            .collect(),
        expected_fixture_cells,
        rendered_fixture_cells,
        measurements,
        cell_summaries,
        hard_failures,
    })
}

pub fn apply_ecbeam2_corpus_report(
    suite: &mut SuiteReport,
    corpus: &EcBeam2CorpusReport,
) -> Result<(), String> {
    for dsd in &mut suite.dsd {
        let Some(summary) = corpus.cell_summaries.iter().find(|summary| {
            summary.filter == dsd.filter
                && summary.modulator == dsd.modulator
                && summary.source_rate == dsd.source_rate
                && Some(summary.wire_rate) == dsd.wire_rate
        }) else {
            return Err(format!(
                "EcBeam2 corpus report lacks cell {}/{}/{}/{}",
                dsd.filter, dsd.modulator, dsd.source_rate, dsd.dsd_rate
            ));
        };
        dsd.notes.extend([
            format!("ecbeam2_corpus_id={}", corpus.corpus_id),
            format!("ecbeam2_corpus_role={}", corpus.role),
            format!("ecbeam2_corpus_manifest_sha256={}", corpus.manifest_sha256),
            format!("ecbeam2_corpus_rendered_cases={}", summary.rendered_cases),
            format!(
                "ecbeam2_corpus_rendered_fixtures={}",
                summary.rendered_fixtures
            ),
            format!(
                "ecbeam2_corpus_hard_failure_count={}",
                summary.hard_failure_count
            ),
        ]);
        if summary.ecbeam2_diagnostics_present {
            dsd.notes.extend([
                format!(
                    "ecbeam2_committed_samples={}",
                    summary.ecbeam2_committed_samples
                ),
                format!(
                    "ecbeam2_constraint_escape={}",
                    summary.ecbeam2_constraint_escape
                ),
                format!(
                    "ecbeam2_state_repair_fallback={}",
                    summary.ecbeam2_state_repair_fallback
                ),
                format!(
                    "ecbeam2_all_nonfinite_resets={}",
                    summary.ecbeam2_all_nonfinite_resets
                ),
                format!(
                    "ecbeam2_observer_desynchronizations={}",
                    summary.ecbeam2_observer_desynchronizations
                ),
                format!(
                    "ecbeam2_invalid_input_substitutions={}",
                    summary.ecbeam2_invalid_input_substitutions
                ),
                format!(
                    "ecbeam2_output_length_error={}",
                    summary.ecbeam2_output_length_error
                ),
                format!(
                    "ecbeam2_committed_output_energy={:.12}",
                    summary.ecbeam2_committed_output_energy
                ),
            ]);
            if let Some(min_survivors) = summary.ecbeam2_min_survivors {
                dsd.notes
                    .push(format!("ecbeam2_min_survivors={min_survivors}"));
            }
            for (key, value) in [
                (
                    "ecbeam2_committed_output_energy_mean",
                    summary.ecbeam2_committed_output_energy_mean,
                ),
                (
                    "ecbeam2_ultrasonic_ema_max",
                    summary.ecbeam2_ultrasonic_ema_max,
                ),
                (
                    "ecbeam2_signed_error_ema_abs_max",
                    summary.ecbeam2_signed_error_ema_abs_max,
                ),
                (
                    "ecbeam2_ultrasonic_ema_p99_9",
                    summary.ecbeam2_ultrasonic_ema_p99_9,
                ),
                (
                    "ecbeam2_ultrasonic_ema_p99_99",
                    summary.ecbeam2_ultrasonic_ema_p99_99,
                ),
                (
                    "ecbeam2_signed_error_ema_abs_p99_9",
                    summary.ecbeam2_signed_error_ema_abs_p99_9,
                ),
                (
                    "ecbeam2_signed_error_ema_abs_p99_99",
                    summary.ecbeam2_signed_error_ema_abs_p99_99,
                ),
            ] {
                if let Some(value) = value {
                    dsd.notes.push(format!("{key}={value:.12}"));
                }
            }
            for (key, value) in [
                (
                    "ecbeam2_ultrasonic_ema_worst_case",
                    summary.ecbeam2_ultrasonic_ema_worst_case.as_deref(),
                ),
                (
                    "ecbeam2_ultrasonic_ema_p99_9_worst_case",
                    summary.ecbeam2_ultrasonic_ema_p99_9_worst_case.as_deref(),
                ),
                (
                    "ecbeam2_ultrasonic_ema_p99_99_worst_case",
                    summary.ecbeam2_ultrasonic_ema_p99_99_worst_case.as_deref(),
                ),
                (
                    "ecbeam2_signed_error_ema_worst_case",
                    summary.ecbeam2_signed_error_ema_worst_case.as_deref(),
                ),
                (
                    "ecbeam2_signed_error_ema_p99_9_worst_case",
                    summary.ecbeam2_signed_error_ema_p99_9_worst_case.as_deref(),
                ),
                (
                    "ecbeam2_signed_error_ema_p99_99_worst_case",
                    summary
                        .ecbeam2_signed_error_ema_p99_99_worst_case
                        .as_deref(),
                ),
            ] {
                if let Some(value) = value {
                    dsd.notes.push(format!("{key}={value}"));
                }
            }
        }
        dsd.inband_snr_worst_db = min_opt(dsd.inband_snr_worst_db, summary.worst_sinad_db);
        dsd.inband_noise_spur_margin_db =
            min_opt(dsd.inband_noise_spur_margin_db, summary.spur_margin_db);
        dsd.high_freq_worst_residual_db = max_opt(
            dsd.high_freq_worst_residual_db,
            summary.high_frequency_residual_db,
        );
        dsd.multitone_residual_db =
            max_opt(dsd.multitone_residual_db, summary.multitone_residual_db);
        dsd.overload_recovery_dbfs =
            max_opt(dsd.overload_recovery_dbfs, summary.overload_recovery_dbfs);
        dsd.inband_noise_worst_rms_dbfs = max_opt(
            dsd.inband_noise_worst_rms_dbfs,
            summary.worst_window_residual_dbfs,
        );
        dsd.decoded_abs_peak = max_opt(dsd.decoded_abs_peak, summary.decoded_abs_peak);
        dsd.bit_density_max_deviation = max_opt(
            dsd.bit_density_max_deviation,
            summary.bit_density_max_deviation,
        );
        dsd.render_ms = Some(dsd.render_ms.unwrap_or(0.0) + summary.render_ms);
        dsd.limiter_limited_events = dsd
            .limiter_limited_events
            .saturating_add(summary.limiter_limited_events);
        dsd.limiter_limited_samples = dsd
            .limiter_limited_samples
            .saturating_add(summary.limiter_limited_samples);
        dsd.stability_resets = dsd
            .stability_resets
            .saturating_add(summary.stability_resets);
        dsd.state_clamps = dsd.state_clamps.saturating_add(summary.state_clamps);
    }
    Ok(())
}

pub fn write_ecbeam2_corpus_artifacts(
    report: &EcBeam2CorpusReport,
    out_dir: &Path,
) -> Result<(), String> {
    fs::create_dir_all(out_dir).map_err(|err| err.to_string())?;
    let json = serde_json::to_string_pretty(report).map_err(|err| err.to_string())?;
    fs::write(out_dir.join("ecbeam2_corpus_report.json"), json).map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("ecbeam2_corpus_metrics.csv"),
        ecbeam2_corpus_metrics_csv(report),
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

fn load_ecbeam2_corpus_manifest(
    path: &Path,
) -> Result<(EcBeam2CorpusManifest, String, PathBuf), String> {
    let bytes = fs::read(path).map_err(|err| {
        format!(
            "failed to read EcBeam2 corpus manifest {}: {err}",
            path.display()
        )
    })?;
    let manifest_sha256 = sha256_hex(&bytes);
    let manifest: EcBeam2CorpusManifest = serde_json::from_slice(&bytes).map_err(|err| {
        format!(
            "failed to parse EcBeam2 corpus manifest {}: {err}",
            path.display()
        )
    })?;
    if manifest.schema_version != ECBEAM2_CORPUS_SCHEMA_VERSION {
        return Err(format!(
            "EcBeam2 corpus manifest must use {ECBEAM2_CORPUS_SCHEMA_VERSION}, got {}",
            manifest.schema_version
        ));
    }
    if manifest.corpus_id.trim().is_empty() || manifest.role.trim().is_empty() {
        return Err("EcBeam2 corpus id and role must be non-empty".to_string());
    }
    if manifest.measurement_version != MEASUREMENT_VERSION
        || manifest.scoring_version != SCORING_VERSION
        || manifest.fixture_set_version != FIXTURE_SET_VERSION
    {
        return Err(
            "EcBeam2 corpus measurement/scoring/fixture versions do not match the native harness"
                .to_string(),
        );
    }
    if manifest.fixtures.is_empty() || manifest.difficult_windows.is_empty() {
        return Err("EcBeam2 corpus must contain fixtures and difficult windows".to_string());
    }
    let manifest_dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    validate_ecbeam2_corpus_manifest_contents(&manifest, &manifest_dir)?;
    Ok((manifest, manifest_sha256, manifest_dir))
}

fn validate_ecbeam2_corpus_manifest_contents(
    manifest: &EcBeam2CorpusManifest,
    _manifest_dir: &Path,
) -> Result<(), String> {
    let expected_wires = manifest
        .source_rates
        .iter()
        .map(|source_rate| {
            DsdRate::Dsd64
                .wire_rate_for_source(*source_rate)
                .ok_or_else(|| format!("unsupported EcBeam2 corpus source rate {source_rate}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if expected_wires != manifest.wire_rates {
        return Err(format!(
            "EcBeam2 corpus wire rates {:?} do not match DSD64 source rates {:?}",
            manifest.wire_rates, manifest.source_rates
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut generated_seeds = std::collections::BTreeSet::new();
    for fixture in &manifest.fixtures {
        if fixture.id.trim().is_empty() || !ids.insert(fixture.id.clone()) {
            return Err(format!(
                "invalid or duplicate EcBeam2 fixture id {}",
                fixture.id
            ));
        }
        match fixture.kind.as_str() {
            "generated" => {
                let generator = fixture
                    .generator
                    .as_deref()
                    .ok_or_else(|| format!("generated fixture {} lacks generator", fixture.id))?;
                let expected = fixture.generator_spec_sha256.as_deref().ok_or_else(|| {
                    format!("generated fixture {} lacks generator hash", fixture.id)
                })?;
                if sha256_hex(generator.as_bytes()) != expected {
                    return Err(format!("generated fixture {} hash mismatch", fixture.id));
                }
                validate_ecbeam2_generator_spec(generator)?;
                generated_seeds.insert(ecbeam2_generator_seed(generator)?);
            }
            other => return Err(format!("unsupported EcBeam2 fixture kind {other}")),
        }
    }
    let declared_seeds = manifest
        .seeds
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    if declared_seeds.len() != manifest.seeds.len() || declared_seeds != generated_seeds {
        return Err(format!(
            "EcBeam2 corpus seeds {:?} do not exactly match generated fixture seeds {:?}",
            manifest.seeds, generated_seeds
        ));
    }
    let mut cases = std::collections::BTreeSet::new();
    for window in &manifest.difficult_windows {
        if window.case_id.trim().is_empty() || !cases.insert(window.case_id.clone()) {
            return Err(format!(
                "invalid or duplicate EcBeam2 case id {}",
                window.case_id
            ));
        }
        if !ids.contains(&window.fixture_id) {
            return Err(format!(
                "EcBeam2 case {} references unknown fixture {}",
                window.case_id, window.fixture_id
            ));
        }
        if !manifest.source_rates.contains(&window.source_rate)
            || window.length_samples == 0
            || window
                .start_sample
                .checked_add(window.length_samples)
                .is_none()
        {
            return Err(format!(
                "EcBeam2 case {} has invalid bounds",
                window.case_id
            ));
        }
    }
    Ok(())
}

fn validate_ecbeam2_corpus_axes(
    manifest: &EcBeam2CorpusManifest,
    filters: &[SelectableDsdFilter],
    source_rates: &[u32],
) -> Result<(), String> {
    for source_rate in source_rates {
        if !manifest.source_rates.contains(source_rate) {
            return Err(format!(
                "source rate {source_rate} is not declared by EcBeam2 corpus {}",
                manifest.corpus_id
            ));
        }
    }
    for filter in filters {
        if !manifest.filters.iter().any(|name| name == filter.name) {
            return Err(format!(
                "filter {} is not declared by EcBeam2 corpus {}",
                filter.name, manifest.corpus_id
            ));
        }
    }
    Ok(())
}

fn materialize_ecbeam2_corpus_cases(
    manifest: &EcBeam2CorpusManifest,
    manifest_dir: &Path,
    source_rate: u32,
) -> Result<Vec<EcBeam2MaterializedCase>, String> {
    let mut cases = Vec::new();
    for fixture_spec in &manifest.fixtures {
        let generator_seed = fixture_spec
            .generator
            .as_deref()
            .map(ecbeam2_generator_seed)
            .transpose()?;
        let required_frames =
            ecbeam2_fixture_required_frames(manifest, &fixture_spec.id, source_rate);
        let (left, right) =
            materialize_ecbeam2_fixture(fixture_spec, manifest_dir, source_rate, required_frames)?;
        let source = source_sanity(&left, &right);
        let fixture_windows = manifest
            .difficult_windows
            .iter()
            .filter(|window| {
                window.fixture_id == fixture_spec.id && window.source_rate == source_rate
            })
            .collect::<Vec<_>>();
        if fixture_windows.is_empty() {
            let length_samples = left.len().min(right.len());
            cases.push(EcBeam2MaterializedCase {
                case_id: format!("{}-full-{source_rate}", fixture_spec.id),
                fixture_id: fixture_spec.id.clone(),
                category: ecbeam2_fixture_category(fixture_spec).to_string(),
                start_sample: 0,
                length_samples,
                generator_seed,
                fixture: DsdTriageFixture {
                    id: fixture_spec.id.clone(),
                    class: "ecbeam2_corpus",
                    left,
                    right,
                    source_peak_dbfs: source.0,
                    source_rms_dbfs: source.1,
                    source_clip_count: Some(source.2),
                    analysis_start_sample: None,
                    analysis_length_samples: None,
                },
            });
            continue;
        }
        for window in fixture_windows {
            let end = window
                .start_sample
                .checked_add(window.length_samples)
                .ok_or_else(|| format!("EcBeam2 case {} bounds overflow", window.case_id))?;
            if end > left.len().min(right.len()) {
                return Err(format!(
                    "EcBeam2 case {} requires samples {}..{}, fixture has {}",
                    window.case_id,
                    window.start_sample,
                    end,
                    left.len().min(right.len())
                ));
            }
            let case_source = source_sanity(
                &left[window.start_sample..end],
                &right[window.start_sample..end],
            );
            cases.push(EcBeam2MaterializedCase {
                case_id: window.case_id.clone(),
                fixture_id: fixture_spec.id.clone(),
                category: window.category.clone(),
                start_sample: window.start_sample,
                length_samples: window.length_samples,
                generator_seed,
                fixture: DsdTriageFixture {
                    id: fixture_spec.id.clone(),
                    class: "ecbeam2_corpus",
                    left: left[..end].to_vec(),
                    right: right[..end].to_vec(),
                    source_peak_dbfs: case_source.0,
                    source_rms_dbfs: case_source.1,
                    source_clip_count: Some(case_source.2),
                    analysis_start_sample: Some(window.start_sample),
                    analysis_length_samples: Some(window.length_samples),
                },
            });
        }
    }
    Ok(cases)
}

fn ecbeam2_fixture_required_frames(
    manifest: &EcBeam2CorpusManifest,
    fixture_id: &str,
    source_rate: u32,
) -> usize {
    let minimum = (source_rate as f64 * 0.50).round() as usize;
    manifest
        .difficult_windows
        .iter()
        .filter(|window| window.fixture_id == fixture_id)
        .filter_map(|window| {
            let end = window.start_sample.checked_add(window.length_samples)?;
            let seconds = end as f64 / window.source_rate as f64;
            Some((seconds * source_rate as f64).ceil() as usize)
        })
        .max()
        .unwrap_or(minimum)
        .max(minimum)
}

fn materialize_ecbeam2_fixture(
    spec: &EcBeam2CorpusFixtureSpec,
    _manifest_dir: &Path,
    source_rate: u32,
    required_frames: usize,
) -> Result<(Vec<f64>, Vec<f64>), String> {
    match spec.kind.as_str() {
        "generated" => ecbeam2_generated_fixture(
            spec.generator.as_deref().unwrap_or_default(),
            required_frames,
            source_rate,
        ),
        other => Err(format!("unsupported EcBeam2 fixture kind {other}")),
    }
}

fn validate_ecbeam2_generator_spec(spec: &str) -> Result<(), String> {
    let parts = spec.split('|').collect::<Vec<_>>();
    let seed = |part: &str| {
        part.strip_prefix("seed=")
            .is_some_and(|value| !value.is_empty())
    };
    let valid = match parts.as_slice() {
        [
            "program_multitone" | "pink_noise" | "fades_overload" | "spur_windows",
            seed_part,
            "v1",
        ] => seed(seed_part),
        ["low_level_tones", "-120,-100,-80", seed_part, "v1"] => seed(seed_part),
        ["tiny_dc", "levels=1e-6,1e-5", seed_part, "v1"] => seed(seed_part),
        ["high_frequency", "18000,19000", seed_part, "v1"] => seed(seed_part),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(format!("unsupported EcBeam2 generator spec {spec}"))
    }
}

fn ecbeam2_generator_seed(spec: &str) -> Result<u64, String> {
    let value = spec
        .split('|')
        .find_map(|part| part.strip_prefix("seed="))
        .ok_or_else(|| format!("EcBeam2 generator lacks seed: {spec}"))?;
    value
        .strip_prefix("0x")
        .map(|hex| u64::from_str_radix(hex, 16))
        .unwrap_or_else(|| value.parse::<u64>())
        .map_err(|_| format!("invalid EcBeam2 generator seed {value}"))
}

fn ecbeam2_generated_fixture(
    spec: &str,
    frames: usize,
    sample_rate: u32,
) -> Result<(Vec<f64>, Vec<f64>), String> {
    validate_ecbeam2_generator_spec(spec)?;
    let seed = ecbeam2_generator_seed(spec)?;
    let kind = spec.split('|').next().unwrap_or_default();
    let phase = (seed & 0xffff) as f64 / 65_536.0 * 2.0 * PI;
    let stereo_phase = ((seed >> 16) & 0xffff) as f64 / 65_536.0 * 2.0 * PI;
    let sample = |index: usize, channel_phase: f64| {
        let t = index as f64 / sample_rate as f64;
        match kind {
            "program_multitone" => {
                let tones = [137.0, 499.0, 997.0, 2_711.0, 7_321.0, 13_733.0, 17_101.0];
                tones
                    .iter()
                    .enumerate()
                    .map(|(voice, freq)| {
                        let weight = 1.0 / (voice + 2) as f64;
                        weight
                            * (2.0 * PI * freq * t + phase + channel_phase + voice as f64 * 0.37)
                                .sin()
                    })
                    .sum::<f64>()
                    * 0.42
                    / 1.718
            }
            "pink_noise" => 0.0,
            "low_level_tones" => {
                let levels = [-120.0, -100.0, -80.0];
                let segment = (frames / levels.len()).max(1);
                let band = (index / segment).min(levels.len() - 1);
                let amp = 10.0f64.powf(levels[band] / 20.0);
                let freq = [997.0, 3_997.0, 12_001.0][band];
                amp * (2.0 * PI * freq * t + phase + channel_phase).sin()
            }
            "tiny_dc" => {
                let level = if index < frames / 2 { 1.0e-6 } else { 1.0e-5 };
                let sign = if channel_phase == 0.0 { 1.0 } else { -1.0 };
                sign * level
            }
            "high_frequency" => {
                0.18 * (2.0 * PI * 18_000.0 * t + phase).sin()
                    + 0.18 * (2.0 * PI * 19_000.0 * t + channel_phase).sin()
            }
            "fades_overload" => {
                let p = index as f64 / frames.max(1) as f64;
                let fade = if p < 0.25 {
                    smoothstep(p / 0.25)
                } else if p < 0.50 {
                    1.0 - smoothstep((p - 0.25) / 0.25)
                } else {
                    0.35
                };
                let program = 0.62 * (2.0 * PI * 997.0 * t + phase).sin()
                    + 0.25 * (2.0 * PI * 17_101.0 * t + channel_phase).sin();
                let overload = if (0.70..0.705).contains(&p) {
                    if index.is_multiple_of(2) { 0.98 } else { -0.98 }
                } else {
                    0.0
                };
                (fade * program + overload).clamp(-0.98, 0.98)
            }
            "spur_windows" => {
                let p = index as f64 / frames.max(1) as f64;
                let dc = if p < 0.33 { 1.0e-5 } else { -1.0e-5 };
                dc + 0.002 * (2.0 * PI * 997.0 * t + phase).sin()
                    + 0.004 * (2.0 * PI * 18_997.0 * t + channel_phase).sin()
            }
            _ => 0.0,
        }
    };
    if kind == "pink_noise" {
        return Ok((
            pink_noise(frames, seed, 0.28),
            pink_noise(frames, seed ^ 0x9e37_79b9_7f4a_7c15, 0.28),
        ));
    }
    Ok((
        (0..frames).map(|index| sample(index, 0.0)).collect(),
        (0..frames)
            .map(|index| sample(index, stereo_phase.max(1.0e-12)))
            .collect(),
    ))
}

fn ecbeam2_fixture_category(spec: &EcBeam2CorpusFixtureSpec) -> &'static str {
    match spec
        .generator
        .as_deref()
        .and_then(|generator| generator.split('|').next())
        .unwrap_or_default()
    {
        "program_multitone" => "program",
        "pink_noise" => "broadband",
        "low_level_tones" => "low-level-tone",
        "tiny_dc" => "tiny-dc",
        "high_frequency" => "high-frequency",
        "fades_overload" => "overload-recovery",
        "spur_windows" => "known-spur",
        _ => "fixture",
    }
}

fn ecbeam2_corpus_health_failures(metric: &DsdTriageMetric) -> Vec<String> {
    let mut failures = Vec::new();
    hard_eq_zero(&mut failures, "stability_resets", metric.stability_resets);
    hard_eq_zero(&mut failures, "state_clamps", metric.state_clamps);
    hard_eq_zero(
        &mut failures,
        "limiter_limited_events",
        metric.limiter_limited_events,
    );
    hard_eq_zero(
        &mut failures,
        "limiter_limited_samples",
        metric.limiter_limited_samples,
    );
    hard_max(
        &mut failures,
        "decoded_peak_dbfs",
        metric.decoded_peak_dbfs,
        db(1.05),
    );
    hard_max(
        &mut failures,
        "idle_bit_density_max_deviation",
        metric.idle_bit_density_max_deviation,
        0.005,
    );
    for (name, value) in [
        ("decoded_peak_dbfs", metric.decoded_peak_dbfs),
        (
            "idle_bit_density_max_deviation",
            metric.idle_bit_density_max_deviation,
        ),
    ] {
        if !value.is_some_and(f64::is_finite) {
            failures.push(format!("{name} missing or non-finite"));
        }
    }
    failures
}

fn ecbeam2_corpus_diagnostics(metric: &DsdTriageMetric) -> Option<EcBeam2CorpusDiagnostics> {
    let notes = metric
        .notes
        .iter()
        .filter_map(|note| note.split_once('='))
        .collect::<BTreeMap<_, _>>();
    let parse_u64 = |key: &str| notes.get(key)?.parse::<u64>().ok();
    let parse_f64 = |key: &str| {
        notes
            .get(key)?
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
    };
    Some(EcBeam2CorpusDiagnostics {
        committed_samples: parse_u64("ecbeam2_committed_samples")?,
        min_survivors: parse_u64("ecbeam2_min_survivors")?,
        constraint_escape: parse_u64("ecbeam2_constraint_escape")?,
        state_repair_fallback: parse_u64("ecbeam2_state_repair_fallback")?,
        all_nonfinite_resets: parse_u64("ecbeam2_all_nonfinite_resets")?,
        observer_desynchronizations: parse_u64("ecbeam2_observer_desynchronizations")?,
        invalid_input_substitutions: parse_u64("ecbeam2_invalid_input_substitutions")?,
        output_length_error: parse_u64("ecbeam2_output_length_error")?,
        committed_output_energy: parse_f64("ecbeam2_committed_output_energy")?,
        committed_output_energy_mean: parse_f64("ecbeam2_committed_output_energy_mean")?,
        ultrasonic_ema_max: parse_f64("ecbeam2_ultrasonic_ema_max")?,
        signed_error_ema_abs_max: parse_f64("ecbeam2_signed_error_ema_abs_max")?,
        ultrasonic_ema_p99_9: parse_f64("ecbeam2_ultrasonic_ema_p99_9")?,
        ultrasonic_ema_p99_99: parse_f64("ecbeam2_ultrasonic_ema_p99_99")?,
        signed_error_ema_abs_p99_9: parse_f64("ecbeam2_signed_error_ema_abs_p99_9")?,
        signed_error_ema_abs_p99_99: parse_f64("ecbeam2_signed_error_ema_abs_p99_99")?,
    })
}

fn ecbeam2_corpus_diagnostic_failures(
    diagnostics: &EcBeam2CorpusDiagnostics,
    active_ecbeam2: bool,
    expected_committed_samples: Option<u64>,
) -> Vec<String> {
    let mut failures = Vec::new();
    match expected_committed_samples {
        Some(expected) if diagnostics.committed_samples != expected => failures.push(format!(
            "ecbeam2_committed_samples={} expected={expected}",
            diagnostics.committed_samples
        )),
        None => failures.push("ecbeam2_expected_committed_samples_unavailable".to_string()),
        Some(_) => {}
    }
    if active_ecbeam2 && diagnostics.min_survivors == 0 {
        failures.push("ecbeam2_min_survivors=0".to_string());
    }
    for (name, value) in [
        ("ecbeam2_constraint_escape", diagnostics.constraint_escape),
        (
            "ecbeam2_state_repair_fallback",
            diagnostics.state_repair_fallback,
        ),
        (
            "ecbeam2_all_nonfinite_resets",
            diagnostics.all_nonfinite_resets,
        ),
        (
            "ecbeam2_observer_desynchronizations",
            diagnostics.observer_desynchronizations,
        ),
        (
            "ecbeam2_invalid_input_substitutions",
            diagnostics.invalid_input_substitutions,
        ),
        (
            "ecbeam2_output_length_error",
            diagnostics.output_length_error,
        ),
    ] {
        if value != 0 {
            failures.push(format!("{name}={value}"));
        }
    }
    failures
}

fn ecbeam2_rendered_fixture_cell_count(measurements: &[EcBeam2CorpusMeasurement]) -> usize {
    measurements
        .iter()
        .map(|measurement| {
            (
                measurement.fixture_id.as_str(),
                measurement.filter.as_str(),
                measurement.modulator.as_str(),
                measurement.source_rate,
                measurement.wire_rate,
            )
        })
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

fn ecbeam2_worst_diagnostic_case(
    rows: &[&EcBeam2CorpusMeasurement],
    read: fn(&EcBeam2CorpusDiagnostics) -> f64,
) -> Option<String> {
    rows.iter()
        .filter_map(|row| {
            let diagnostics = row.ecbeam2_diagnostics.as_ref()?;
            Some((*row, read(diagnostics)))
        })
        .filter(|(_, value)| value.is_finite())
        .max_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(row, _)| {
            format!(
                "{}:{}:{}+{}:seed={}",
                row.case_id,
                row.fixture_id,
                row.start_sample,
                row.length_samples,
                row.generator_seed
                    .map(|seed| seed.to_string())
                    .unwrap_or_else(|| "wav".to_string())
            )
        })
}

fn ecbeam2_corpus_cell_summaries(
    measurements: &[EcBeam2CorpusMeasurement],
) -> Vec<EcBeam2CorpusCellSummary> {
    let mut groups: BTreeMap<(String, String, u32, u32), Vec<&EcBeam2CorpusMeasurement>> =
        BTreeMap::new();
    for measurement in measurements {
        groups
            .entry((
                measurement.filter.clone(),
                measurement.modulator.clone(),
                measurement.source_rate,
                measurement.wire_rate,
            ))
            .or_default()
            .push(measurement);
    }
    groups
        .into_iter()
        .map(|((filter, modulator, source_rate, wire_rate), rows)| {
            let min_value = |values: Vec<f64>| values.into_iter().reduce(f64::min);
            let max_value = |values: Vec<f64>| values.into_iter().reduce(f64::max);
            let rendered_fixtures = rows
                .iter()
                .map(|row| row.fixture_id.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            let diagnostics = rows
                .iter()
                .filter_map(|row| row.ecbeam2_diagnostics.as_ref())
                .collect::<Vec<_>>();
            let ecbeam2_committed_samples = diagnostics
                .iter()
                .map(|diagnostics| diagnostics.committed_samples)
                .sum::<u64>();
            let ecbeam2_committed_output_energy = diagnostics
                .iter()
                .map(|diagnostics| diagnostics.committed_output_energy)
                .sum::<f64>();
            let active_ecbeam2 = modulator.starts_with("EcBeam2");
            EcBeam2CorpusCellSummary {
                filter,
                modulator,
                source_rate,
                wire_rate,
                rendered_cases: rows.len(),
                rendered_fixtures,
                seeds: rows
                    .iter()
                    .filter_map(|row| row.generator_seed)
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                worst_sinad_db: min_value(
                    rows.iter()
                        .filter_map(|row| row.metric.residual_relative_db.map(|db| -db))
                        .collect(),
                ),
                spur_margin_db: min_value(
                    rows.iter()
                        .filter_map(|row| row.metric.residual_peak_to_median_db)
                        .collect(),
                ),
                high_frequency_residual_db: max_value(
                    rows.iter()
                        .filter(|row| {
                            matches!(
                                row.category.as_str(),
                                "high-frequency" | "known-spur" | "low-level-tone"
                            )
                        })
                        .filter_map(|row| row.metric.residual_relative_db)
                        .collect(),
                ),
                multitone_residual_db: max_value(
                    rows.iter()
                        .filter(|row| {
                            matches!(
                                row.category.as_str(),
                                "program" | "broadband" | "real-world" | "fade"
                            )
                        })
                        .filter_map(|row| row.metric.residual_relative_db)
                        .collect(),
                ),
                narrowband_peak_dbfs: max_value(
                    rows.iter()
                        .filter_map(|row| row.metric.residual_peak_spur_dbfs)
                        .collect(),
                ),
                overload_recovery_dbfs: max_value(
                    rows.iter()
                        .filter(|row| row.category == "overload-recovery")
                        .filter_map(|row| row.metric.worst_window_residual_dbfs)
                        .collect(),
                ),
                worst_window_residual_dbfs: max_value(
                    rows.iter()
                        .filter_map(|row| row.metric.worst_window_residual_dbfs)
                        .collect(),
                ),
                decoded_abs_peak: max_value(
                    rows.iter()
                        .filter_map(|row| row.metric.decoded_abs_peak)
                        .collect(),
                ),
                bit_density_max_deviation: max_value(
                    rows.iter()
                        .filter_map(|row| row.metric.idle_bit_density_max_deviation)
                        .collect(),
                ),
                render_ms: rows.iter().filter_map(|row| row.metric.render_ms).sum(),
                limiter_limited_events: rows
                    .iter()
                    .map(|row| row.metric.limiter_limited_events)
                    .sum(),
                limiter_limited_samples: rows
                    .iter()
                    .map(|row| row.metric.limiter_limited_samples)
                    .sum(),
                stability_resets: rows.iter().map(|row| row.metric.stability_resets).sum(),
                state_clamps: rows.iter().map(|row| row.metric.state_clamps).sum(),
                hard_failure_count: rows.iter().map(|row| row.metric.hard_failures.len()).sum(),
                ecbeam2_diagnostics_present: !diagnostics.is_empty(),
                ecbeam2_committed_samples,
                ecbeam2_min_survivors: active_ecbeam2
                    .then(|| {
                        diagnostics
                            .iter()
                            .map(|diagnostics| diagnostics.min_survivors)
                            .min()
                    })
                    .flatten(),
                ecbeam2_constraint_escape: diagnostics
                    .iter()
                    .map(|diagnostics| diagnostics.constraint_escape)
                    .sum(),
                ecbeam2_state_repair_fallback: diagnostics
                    .iter()
                    .map(|diagnostics| diagnostics.state_repair_fallback)
                    .sum(),
                ecbeam2_all_nonfinite_resets: diagnostics
                    .iter()
                    .map(|diagnostics| diagnostics.all_nonfinite_resets)
                    .sum(),
                ecbeam2_observer_desynchronizations: diagnostics
                    .iter()
                    .map(|diagnostics| diagnostics.observer_desynchronizations)
                    .sum(),
                ecbeam2_invalid_input_substitutions: diagnostics
                    .iter()
                    .map(|diagnostics| diagnostics.invalid_input_substitutions)
                    .sum(),
                ecbeam2_output_length_error: diagnostics
                    .iter()
                    .map(|diagnostics| diagnostics.output_length_error)
                    .sum(),
                ecbeam2_committed_output_energy,
                ecbeam2_committed_output_energy_mean: (ecbeam2_committed_samples > 0)
                    .then(|| ecbeam2_committed_output_energy / ecbeam2_committed_samples as f64),
                ecbeam2_ultrasonic_ema_max: max_value(
                    diagnostics
                        .iter()
                        .map(|diagnostics| diagnostics.ultrasonic_ema_max)
                        .collect(),
                ),
                ecbeam2_signed_error_ema_abs_max: max_value(
                    diagnostics
                        .iter()
                        .map(|diagnostics| diagnostics.signed_error_ema_abs_max)
                        .collect(),
                ),
                ecbeam2_ultrasonic_ema_p99_9: max_value(
                    diagnostics
                        .iter()
                        .map(|diagnostics| diagnostics.ultrasonic_ema_p99_9)
                        .collect(),
                ),
                ecbeam2_ultrasonic_ema_p99_99: max_value(
                    diagnostics
                        .iter()
                        .map(|diagnostics| diagnostics.ultrasonic_ema_p99_99)
                        .collect(),
                ),
                ecbeam2_signed_error_ema_abs_p99_9: max_value(
                    diagnostics
                        .iter()
                        .map(|diagnostics| diagnostics.signed_error_ema_abs_p99_9)
                        .collect(),
                ),
                ecbeam2_signed_error_ema_abs_p99_99: max_value(
                    diagnostics
                        .iter()
                        .map(|diagnostics| diagnostics.signed_error_ema_abs_p99_99)
                        .collect(),
                ),
                ecbeam2_ultrasonic_ema_worst_case: ecbeam2_worst_diagnostic_case(
                    &rows,
                    |diagnostics| diagnostics.ultrasonic_ema_max,
                ),
                ecbeam2_ultrasonic_ema_p99_9_worst_case: ecbeam2_worst_diagnostic_case(
                    &rows,
                    |diagnostics| diagnostics.ultrasonic_ema_p99_9,
                ),
                ecbeam2_ultrasonic_ema_p99_99_worst_case: ecbeam2_worst_diagnostic_case(
                    &rows,
                    |diagnostics| diagnostics.ultrasonic_ema_p99_99,
                ),
                ecbeam2_signed_error_ema_worst_case: ecbeam2_worst_diagnostic_case(
                    &rows,
                    |diagnostics| diagnostics.signed_error_ema_abs_max,
                ),
                ecbeam2_signed_error_ema_p99_9_worst_case: ecbeam2_worst_diagnostic_case(
                    &rows,
                    |diagnostics| diagnostics.signed_error_ema_abs_p99_9,
                ),
                ecbeam2_signed_error_ema_p99_99_worst_case: ecbeam2_worst_diagnostic_case(
                    &rows,
                    |diagnostics| diagnostics.signed_error_ema_abs_p99_99,
                ),
            }
        })
        .collect()
}

fn ecbeam2_corpus_metrics_csv(report: &EcBeam2CorpusReport) -> String {
    let mut csv = String::from(
        "schema_version,manifest_sha256,corpus_id,role,case_id,fixture_id,generator_seed,category,source_rate,wire_rate,filter,modulator,start_sample,length_samples,status,native_left_sha256,native_right_sha256,render_ms,residual_relative_db,residual_rms_dbfs,worst_window_residual_dbfs,residual_peak_spur_dbfs,residual_peak_spur_hz,residual_peak_to_median_db,residual_20_20k_rms_dbfs,residual_12_22k_rms_dbfs,decoded_abs_peak,idle_bit_density_max_deviation,limiter_limited_events,limiter_limited_samples,stability_resets,state_clamps,hard_failures,notes\n",
    );
    for row in &report.measurements {
        let values = [
            report.schema_version.clone(),
            report.manifest_sha256.clone(),
            report.corpus_id.clone(),
            report.role.clone(),
            row.case_id.clone(),
            row.fixture_id.clone(),
            row.generator_seed
                .map(|seed| seed.to_string())
                .unwrap_or_default(),
            row.category.clone(),
            row.source_rate.to_string(),
            row.wire_rate.to_string(),
            row.filter.clone(),
            row.modulator.clone(),
            row.start_sample.to_string(),
            row.length_samples.to_string(),
            row.metric.status.clone(),
            row.metric.native_left_sha256.clone().unwrap_or_default(),
            row.metric.native_right_sha256.clone().unwrap_or_default(),
            fmt_csv_opt(row.metric.render_ms),
            fmt_csv_opt(row.metric.residual_relative_db),
            fmt_csv_opt(row.metric.residual_rms_dbfs),
            fmt_csv_opt(row.metric.worst_window_residual_dbfs),
            fmt_csv_opt(row.metric.residual_peak_spur_dbfs),
            fmt_csv_opt(row.metric.residual_peak_spur_hz),
            fmt_csv_opt(row.metric.residual_peak_to_median_db),
            fmt_csv_opt(row.metric.residual_20_20k_rms_dbfs),
            fmt_csv_opt(row.metric.residual_12_22k_rms_dbfs),
            fmt_csv_opt(row.metric.decoded_abs_peak),
            fmt_csv_opt(row.metric.idle_bit_density_max_deviation),
            row.metric.limiter_limited_events.to_string(),
            row.metric.limiter_limited_samples.to_string(),
            row.metric.stability_resets.to_string(),
            row.metric.state_clamps.to_string(),
            row.metric.hard_failures.join(";"),
            row.metric.notes.join(";"),
        ];
        csv.push_str(
            &values
                .into_iter()
                .map(csv_cell)
                .collect::<Vec<_>>()
                .join(","),
        );
        csv.push('\n');
    }
    csv
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn dsd_triage_fixtures(
    manifest: &DsdTriageResolvedFixtureManifest,
) -> Result<Vec<DsdTriageFixture>, String> {
    let (real_l, real_r) = load_pcm16_wav_excerpt(
        Path::new(&manifest.real_world.path),
        manifest.real_world.start_sec,
        manifest.real_world.end_sec,
    )?;
    let source = source_sanity(&real_l, &real_r);
    let sample_rate = 44_100;
    let frames = ((manifest.real_world.duration_sec * sample_rate as f64).round() as usize)
        .max(real_l.len().min(real_r.len()));
    let (program_l, program_r) = roundtrip_program_fixture(frames, sample_rate);
    let mid_band_tone = sine(frames, sample_rate, 1_000.0, 10.0f64.powf(-6.0 / 20.0));
    let edge_tone = sine(frames, sample_rate, 19_000.0, 10.0f64.powf(-12.0 / 20.0));
    let (stress_l, stress_r) = hot_dense_program_fixture(&real_l, &real_r);
    let stress_source = source_sanity(&stress_l, &stress_r);
    Ok(vec![
        DsdTriageFixture {
            id: manifest.real_world.id.clone(),
            class: "real_world",
            left: real_l,
            right: real_r,
            source_peak_dbfs: source.0,
            source_rms_dbfs: source.1,
            source_clip_count: Some(source.2),
            analysis_start_sample: None,
            analysis_length_samples: None,
        },
        DsdTriageFixture {
            id: manifest.synthetic_fixture_id.clone(),
            class: "synthetic",
            left: program_l,
            right: program_r,
            source_peak_dbfs: None,
            source_rms_dbfs: None,
            source_clip_count: None,
            analysis_start_sample: None,
            analysis_length_samples: None,
        },
        DsdTriageFixture {
            id: manifest.mid_band_fixture_id.clone(),
            class: "mid_band",
            left: mid_band_tone.clone(),
            right: mid_band_tone,
            source_peak_dbfs: None,
            source_rms_dbfs: None,
            source_clip_count: None,
            analysis_start_sample: None,
            analysis_length_samples: None,
        },
        DsdTriageFixture {
            id: manifest.edge_fixture_id.clone(),
            class: "edge_case",
            left: edge_tone.clone(),
            right: edge_tone,
            source_peak_dbfs: None,
            source_rms_dbfs: None,
            source_clip_count: None,
            analysis_start_sample: None,
            analysis_length_samples: None,
        },
        DsdTriageFixture {
            id: manifest.stress_fixture_id.clone(),
            class: "stress",
            left: stress_l,
            right: stress_r,
            source_peak_dbfs: stress_source.0,
            source_rms_dbfs: stress_source.1,
            source_clip_count: Some(stress_source.2),
            analysis_start_sample: None,
            analysis_length_samples: None,
        },
    ])
}

fn hot_dense_program_fixture(left: &[f64], right: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let peak = left
        .iter()
        .chain(right.iter())
        .map(|sample| sample.abs())
        .fold(0.0, f64::max);
    let scale = if peak > 1.0e-12 { 1.0 / peak } else { 1.0 };
    let scale_sample = |sample: &f64| (sample * scale).clamp(-1.0, 1.0);
    (
        left.iter().map(scale_sample).collect(),
        right.iter().map(scale_sample).collect(),
    )
}

#[allow(clippy::too_many_arguments)]
fn load_or_build_dsd_triage_baseline(
    baseline_label: &str,
    fixture: &DsdTriageFixture,
    fixture_set_version: &str,
    cache_dir: &Path,
    build_missing: bool,
    filter: FilterType,
    rate: DsdRate,
    source_rate: u32,
) -> Result<(DsdTriageMetric, DsdTriageBaselineCacheEntry), String> {
    fs::create_dir_all(cache_dir).map_err(|err| err.to_string())?;
    let baseline_config = dsd_triage_baseline_config(baseline_label)?;
    let config_hash = dsd_triage_config_hash(baseline_label, baseline_config);
    let path = dsd_triage_baseline_cache_path(cache_dir, baseline_label, &fixture.id, &config_hash);
    if path.exists() {
        let text = fs::read_to_string(&path).map_err(|err| err.to_string())?;
        let metric: DsdTriageMetric = serde_json::from_str(&text)
            .map_err(|err| format!("failed to parse baseline cache {}: {err}", path.display()))?;
        validate_dsd_triage_baseline_cache(&metric, baseline_label, fixture, fixture_set_version)?;
        return Ok((
            metric,
            DsdTriageBaselineCacheEntry {
                baseline_label: baseline_label.to_string(),
                fixture_id: fixture.id.clone(),
                path,
                config_hash,
                cache_hit: true,
                built: false,
            },
        ));
    }
    if !build_missing {
        return Err(format!(
            "missing baseline cache for {baseline_label}/{} at {}; rerun with --build-baseline-cache",
            fixture.id,
            path.display()
        ));
    }
    let modulator = match baseline_label {
        "standard" => DsdModulator::Standard,
        "prod_ec2" => DsdModulator::EcDepth2,
        other => return Err(format!("unsupported baseline label {other}")),
    };
    let metric = measure_dsd_triage_case(
        baseline_label,
        baseline_label,
        fixture,
        fixture_set_version,
        filter,
        rate,
        source_rate,
        modulator,
        baseline_config,
    )?;
    let json = serde_json::to_string_pretty(&metric).map_err(|err| err.to_string())?;
    fs::write(&path, json).map_err(|err| err.to_string())?;
    Ok((
        metric,
        DsdTriageBaselineCacheEntry {
            baseline_label: baseline_label.to_string(),
            fixture_id: fixture.id.clone(),
            path,
            config_hash,
            cache_hit: false,
            built: true,
        },
    ))
}

fn dsd_triage_baseline_config(baseline_label: &str) -> Result<DsdExperimentConfig, String> {
    match baseline_label {
        "standard" | "prod_ec2" => {
            let config = DsdExperimentConfig::default()
                .with_dsd64_input_gain_db(-4.0)?
                .with_dsd64_dither_scale_multiplier(0.0)?
                .with_dsd64_dither_shape(DitherShape::HighPassTpdf)
                .with_dsd64_dither_prng(DitherPrng::SplitMix64)
                .with_dsd64_dither_leak_alpha(0.99)?
                .with_dsd64_future_scorer(EcFutureScorer::QuarterPressureNoDcTransition)
                .with_dsd64_ec2_long_filter_policy(Ec2LongFilterPolicy::AmbiguityPressure)
                .with_dsd64_ec2_policy_weights(Ec2PolicyWeights {
                    quantizer_weight: 1.0,
                    pressure_weight: 0.75,
                    limit_weight: 80.0,
                    transition_weight: 0.0,
                    dc_weight: 0.04,
                    lookahead_discount: 0.8,
                    ambiguity_margin: 0.005,
                    pressure_taper_start: 0.45,
                    pressure_taper_strength: 2.0,
                })?;
            Ok(config)
        }
        other => Err(format!("unsupported baseline label {other}")),
    }
}

fn validate_dsd_triage_baseline_cache(
    metric: &DsdTriageMetric,
    baseline_label: &str,
    fixture: &DsdTriageFixture,
    fixture_set_version: &str,
) -> Result<(), String> {
    if metric.candidate_label != baseline_label
        || metric.fixture_id != fixture.id
        || metric.fixture_set_version != fixture_set_version
        || metric.measurement_version != MEASUREMENT_VERSION
        || metric.scoring_version != SCORING_VERSION
        || metric.rate != "DSD64"
    {
        return Err(format!(
            "baseline cache version/key mismatch for {baseline_label}/{}",
            fixture.id
        ));
    }
    Ok(())
}

fn dsd_triage_baseline_cache_path(
    cache_dir: &Path,
    baseline_label: &str,
    fixture_id: &str,
    config_hash: &str,
) -> PathBuf {
    let commit = git_commit().unwrap_or_else(|| "unknown".to_string());
    let host = build_host();
    let dirty_key = git_dirty_cache_key();
    let name = format!(
        "{}__{}__{}__{}__{}__{}__{}__DSD64__{}__{}__{}.json",
        sanitize_cache_component(&host),
        sanitize_cache_component(&commit),
        sanitize_cache_component(&dirty_key),
        sanitize_cache_component(MEASUREMENT_VERSION),
        sanitize_cache_component(SCORING_VERSION),
        sanitize_cache_component("dsd64-triage-v1"),
        sanitize_cache_component(fixture_id),
        sanitize_cache_component(baseline_label),
        sanitize_cache_component(config_hash),
        sanitize_cache_component(codegen_flags().as_str()),
    );
    cache_dir.join(name)
}

fn dsd_triage_config_hash(baseline_label: &str, config: DsdExperimentConfig) -> String {
    short_hash(&format!("{baseline_label}:{config:?}"))
}

fn git_dirty_cache_key() -> String {
    let mut dirty_state = Vec::new();
    let mut any_dirty = false;
    for (label, args) in [
        ("unstaged", &["diff", "--binary"][..]),
        ("staged", &["diff", "--cached", "--binary"][..]),
    ] {
        match Command::new("git").args(args).output() {
            Ok(output) if output.status.success() => {
                if !output.stdout.is_empty() {
                    any_dirty = true;
                }
                dirty_state.extend_from_slice(label.as_bytes());
                dirty_state.push(0);
                dirty_state.extend_from_slice(&output.stdout);
                dirty_state.push(0);
            }
            _ => return "dirty-unknown".to_string(),
        }
    }
    match git_untracked_cache_state() {
        Some(untracked) => {
            if !untracked.is_empty() {
                any_dirty = true;
            }
            dirty_state.extend_from_slice(b"untracked");
            dirty_state.push(0);
            dirty_state.extend_from_slice(&untracked);
            dirty_state.push(0);
        }
        None => return "dirty-unknown".to_string(),
    }
    if any_dirty {
        format!("dirty-{}", short_hash_bytes(&dirty_state))
    } else {
        "clean".to_string()
    }
}

fn git_untracked_cache_state() -> Option<Vec<u8>> {
    let output = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut state = Vec::new();
    for path in output.stdout.split(|byte| *byte == 0) {
        if path.is_empty() {
            continue;
        }
        if dsd_triage_cache_ignored_untracked_path(path) {
            continue;
        }
        let path_text = String::from_utf8_lossy(path);
        let bytes = fs::read(Path::new(path_text.as_ref())).ok()?;
        let digest = Sha256::digest(&bytes);
        state.extend_from_slice(path);
        state.push(0);
        state.extend_from_slice(&digest);
        state.push(0);
    }
    Some(state)
}

fn dsd_triage_cache_ignored_untracked_path(path: &[u8]) -> bool {
    path.starts_with(b"audio_tests/out/") || path.starts_with(b"target/")
}

fn short_hash(value: &str) -> String {
    short_hash_bytes(value.as_bytes())
}

fn short_hash_bytes(value: &[u8]) -> String {
    let digest = Sha256::digest(value);
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn measure_dsd_triage_case(
    run_id: &str,
    candidate_label: &str,
    fixture: &DsdTriageFixture,
    fixture_set_version: &str,
    filter: FilterType,
    dsd_rate: DsdRate,
    source_rate: u32,
    dsd_modulator: DsdModulator,
    mut config: DsdExperimentConfig,
) -> Result<DsdTriageMetric, String> {
    let Some(wire_rate) = dsd_rate.wire_rate_for_source(source_rate) else {
        return Err("unsupported DSD64/source-rate combination".to_string());
    };
    let mut notes = Vec::new();
    if fixture.source_clip_count.unwrap_or(0) > 0 {
        notes.push("source_clip_warning".to_string());
    }
    let frames = fixture.left.len().min(fixture.right.len());
    let left_input = &fixture.left[..frames];
    let right_input = &fixture.right[..frames];
    let analysis_start = fixture.analysis_start_sample.unwrap_or(0);
    let analysis_length = fixture
        .analysis_length_samples
        .unwrap_or_else(|| frames.saturating_sub(analysis_start));
    let analysis_end = analysis_start
        .checked_add(analysis_length)
        .ok_or_else(|| "DSD triage analysis range overflow".to_string())?;
    if analysis_length == 0 || analysis_end > frames {
        return Err(format!(
            "DSD triage analysis range {analysis_start}..{analysis_end} is outside {frames} frames"
        ));
    }
    configure_ecbeam2_diagnostic_window(
        &mut config,
        filter,
        source_rate,
        wire_rate,
        analysis_start,
        analysis_length,
    )?;
    let wall_start = Instant::now();
    let mut renderer = new_dsd_renderer(filter, source_rate, dsd_rate, dsd_modulator, config)
        .map_err(|err| format!("DSD renderer init failed: {err}"))?;
    renderer.set_native_order(NativeDsdOrder::MsbFirst);
    let render_start = Instant::now();
    let (native_l, native_r) = render_native_stream(&mut renderer, left_input, right_input);
    let render_ms = render_start.elapsed().as_secs_f64() * 1000.0;
    let bits_l = unpack_native_msb(&native_l);
    let bits_r = unpack_native_msb(&native_r);
    let decoded_l = dsd_triage_decimate_to_pcm(&bits_l, wire_rate, source_rate)
        .ok_or_else(|| "left DSD triage decimation failed".to_string())?;
    let decoded_r = dsd_triage_decimate_to_pcm(&bits_r, wire_rate, source_rate)
        .ok_or_else(|| "right DSD triage decimation failed".to_string())?;
    notes.push("decoded_path=sinc_extreme32k_triage_decimator".to_string());

    let mut idle_renderer = new_dsd_renderer(filter, source_rate, dsd_rate, dsd_modulator, config)
        .map_err(|err| format!("DSD idle density renderer init failed: {err}"))?;
    idle_renderer.set_native_order(NativeDsdOrder::MsbFirst);
    let silence = vec![0.0; frames];
    let (idle_native_l, idle_native_r) =
        render_native_stream(&mut idle_renderer, &silence, &silence);
    let idle_bits_l = unpack_native_msb(&idle_native_l);
    let idle_bits_r = unpack_native_msb(&idle_native_r);

    let reference_l_full = triage_reference_lowpass_pcm(left_input, source_rate)
        .unwrap_or_else(|| left_input.to_vec());
    let reference_r_full = triage_reference_lowpass_pcm(right_input, source_rate)
        .unwrap_or_else(|| right_input.to_vec());
    let decoded_end = analysis_end.min(decoded_l.len()).min(decoded_r.len());
    if decoded_end <= analysis_start {
        return Err(
            "decoded DSD triage output does not cover the requested analysis window".into(),
        );
    }
    if fixture.analysis_start_sample.is_some() {
        notes.push(format!("analysis_start_sample={analysis_start}"));
        notes.push(format!(
            "analysis_length_samples={}",
            decoded_end - analysis_start
        ));
        notes.push("renderer_prefix_preserved=true".to_string());
    }
    let reference_l = &reference_l_full[analysis_start..decoded_end];
    let reference_r = &reference_r_full[analysis_start..decoded_end];
    let decoded_l_analysis = &decoded_l[analysis_start..decoded_end];
    let decoded_r_analysis = &decoded_r[analysis_start..decoded_end];
    let left = analyze_roundtrip_channel(reference_l, decoded_l_analysis, source_rate);
    let right = analyze_roundtrip_channel(reference_r, decoded_r_analysis, source_rate);
    if left.is_none() {
        notes.push("left channel alignment unavailable".to_string());
    }
    if right.is_none() {
        notes.push("right channel alignment unavailable".to_string());
    }
    let residuals = [left.as_ref(), right.as_ref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let reference_domain_residuals = residuals
        .iter()
        .map(|analysis| triage_reference_domain_residual(analysis))
        .collect::<Vec<_>>();
    let residual_spectrum = reference_domain_residuals
        .iter()
        .filter_map(|residual| triage_residual_spectrum_metrics(residual, source_rate))
        .fold(None, |best, metrics| {
            Some(match best {
                None => metrics,
                Some(best) => triage_worst_spectrum_metrics(best, metrics),
            })
        });
    let worst_window_residual_dbfs = max_opt(
        left.as_ref().and_then(|analysis| {
            worst_window_rms_dbfs(
                &triage_reference_domain_residual(analysis),
                source_rate,
                0.100,
            )
        }),
        right.as_ref().and_then(|analysis| {
            worst_window_rms_dbfs(
                &triage_reference_domain_residual(analysis),
                source_rate,
                0.100,
            )
        }),
    );

    let sine_tones = match fixture.id.as_str() {
        "sine_19k_-12db" => Some([19_000.0]),
        "sine_1k_-6db" => Some([1_000.0]),
        _ => None,
    };
    let sine_fit = sine_tones.map(|tones| {
        combine_fit_metrics(
            left.as_ref().and_then(|analysis| {
                tone_fit_metrics(
                    &triage_reference_domain_samples(&decoded_l, analysis.gain),
                    source_rate,
                    &tones,
                )
            }),
            right.as_ref().and_then(|analysis| {
                tone_fit_metrics(
                    &triage_reference_domain_samples(&decoded_r, analysis.gain),
                    source_rate,
                    &tones,
                )
            }),
        )
    });
    let multitone_fit = (fixture.id == "program_multitone").then(|| {
        let (_, tones) = program_multitone(16, source_rate, 0.42);
        combine_fit_metrics(
            left.as_ref().and_then(|analysis| {
                tone_fit_metrics(
                    &triage_reference_domain_samples(&decoded_l, analysis.gain),
                    source_rate,
                    &tones,
                )
            }),
            right.as_ref().and_then(|analysis| {
                tone_fit_metrics(
                    &triage_reference_domain_samples(&decoded_r, analysis.gain),
                    source_rate,
                    &tones,
                )
            }),
        )
    });
    let ultrasonic =
        dsd_ultrasonic_metrics(&bits_l, &bits_r, wire_rate, DSD_ANALYSIS_FFT_BITS_FULL);
    let limiter = renderer.limiter_telemetry();
    let fixture_density_l = bit_density(&bits_l);
    let fixture_density_r = bit_density(&bits_r);
    let idle_density_l = bit_density(&idle_bits_l);
    let idle_density_r = bit_density(&idle_bits_r);
    let density_window = (wire_rate as usize / 100).max(1024);
    let fixture_density_dev_l = rolling_bit_density_max_deviation(&bits_l, density_window);
    let fixture_density_dev_r = rolling_bit_density_max_deviation(&bits_r, density_window);
    let idle_density_dev_l = rolling_bit_density_max_deviation(&idle_bits_l, density_window);
    let idle_density_dev_r = rolling_bit_density_max_deviation(&idle_bits_r, density_window);
    let decoded_abs_peak = max_opt(sample_abs_peak(&decoded_l), sample_abs_peak(&decoded_r));
    let decoded_peak_dbfs = decoded_abs_peak.map(|peak| db(peak.max(1e-18)));
    let fitted_gain_db = max_abs_signed_opt(
        left.as_ref()
            .map(|analysis| db(analysis.gain.abs().max(1e-18))),
        right
            .as_ref()
            .map(|analysis| db(analysis.gain.abs().max(1e-18))),
    );
    let state_clamps = renderer.state_clamps() + idle_renderer.state_clamps();
    let stability_resets = renderer.stability_resets() + idle_renderer.stability_resets();
    append_beam_diagnostics_notes(&mut notes, &renderer);
    let expected_ecbeam2_window_samples = u64::try_from(analysis_length).ok().and_then(|length| {
        let ratio = wire_rate.checked_div(source_rate)?;
        (wire_rate % source_rate == 0)
            .then_some(length)?
            .checked_mul(u64::from(ratio))?
            .checked_mul(2)
    });
    let idle_ecbeam2_failures = append_ecbeam2_idle_health_notes(
        &mut notes,
        &idle_renderer,
        config.dsd64_tweaks.ecbeam2_config.is_some(),
        dsd_modulator == DsdModulator::EcBeam2,
        expected_ecbeam2_window_samples,
    );
    let mut metric = DsdTriageMetric {
        run_id: run_id.to_string(),
        host_id: build_host(),
        git_commit: git_commit(),
        measurement_version: MEASUREMENT_VERSION.to_string(),
        scoring_version: SCORING_VERSION.to_string(),
        fixture_set_version: fixture_set_version.to_string(),
        candidate_label: candidate_label.to_string(),
        fixture_id: fixture.id.clone(),
        fixture_class: fixture.class.to_string(),
        rate: dsd_rate_name(dsd_rate).to_string(),
        native_left_sha256: Some(sha256_hex(&native_l)),
        native_right_sha256: Some(sha256_hex(&native_r)),
        status: "pass".to_string(),
        render_ms: Some(render_ms),
        wall_ms: Some(wall_start.elapsed().as_secs_f64() * 1000.0),
        residual_relative_db: max_opt(
            left.as_ref().map(|analysis| analysis.residual_relative_db),
            right.as_ref().map(|analysis| analysis.residual_relative_db),
        ),
        residual_rms_dbfs: max_opt(
            left.as_ref().map(triage_reference_domain_residual_rms_dbfs),
            right
                .as_ref()
                .map(triage_reference_domain_residual_rms_dbfs),
        ),
        residual_peak_dbfs: max_opt(
            left.as_ref()
                .map(triage_reference_domain_residual_peak_dbfs),
            right
                .as_ref()
                .map(triage_reference_domain_residual_peak_dbfs),
        ),
        worst_window_residual_dbfs,
        residual_peak_spur_dbfs: residual_spectrum
            .as_ref()
            .and_then(|metrics| metrics.peak_spur_dbfs),
        residual_peak_spur_hz: residual_spectrum
            .as_ref()
            .and_then(|metrics| metrics.peak_spur_hz),
        residual_peak_to_median_db: residual_spectrum
            .as_ref()
            .and_then(|metrics| metrics.peak_to_median_db),
        residual_20_10k_rms_dbfs: residual_spectrum
            .as_ref()
            .and_then(|metrics| metrics.band_20_10k_rms_dbfs),
        residual_20_20k_rms_dbfs: residual_spectrum
            .as_ref()
            .and_then(|metrics| metrics.band_20_20k_rms_dbfs),
        residual_12_22k_rms_dbfs: residual_spectrum
            .as_ref()
            .and_then(|metrics| metrics.band_12_22k_rms_dbfs),
        sine_tone_fit_sinad_db: sine_fit.as_ref().and_then(|metrics| metrics.sinad_db),
        sine_tone_fit_residual_dbfs: sine_fit.as_ref().and_then(|metrics| metrics.residual_dbfs),
        sine_tone_fit_20_10k_sinad_db: sine_fit
            .as_ref()
            .and_then(|metrics| metrics.band_20_10k_sinad_db),
        sine_tone_fit_20_10k_residual_dbfs: sine_fit
            .as_ref()
            .and_then(|metrics| metrics.band_20_10k_residual_dbfs),
        sine_tone_fit_4_10k_sinad_db: sine_fit
            .as_ref()
            .and_then(|metrics| metrics.band_4_10k_sinad_db),
        sine_tone_fit_4_10k_residual_dbfs: sine_fit
            .as_ref()
            .and_then(|metrics| metrics.band_4_10k_residual_dbfs),
        multitone_fit_sinad_db: multitone_fit.as_ref().and_then(|metrics| metrics.sinad_db),
        multitone_fit_residual_dbfs: multitone_fit
            .as_ref()
            .and_then(|metrics| metrics.residual_dbfs),
        dsd_ultrasonic_24_50k_max_dbfs: ultrasonic.ultrasonic_24_50k_max_dbfs,
        dsd_ultrasonic_50_100k_max_dbfs: ultrasonic.ultrasonic_50_100k_max_dbfs,
        dsd_ultrasonic_100_200k_max_dbfs: ultrasonic.ultrasonic_100_200k_max_dbfs,
        idle_bit_density: average_opt(idle_density_l, idle_density_r),
        idle_bit_density_max_deviation: max_opt(idle_density_dev_l, idle_density_dev_r),
        fixture_bit_density: average_opt(fixture_density_l, fixture_density_r),
        fixture_bit_density_max_deviation: max_opt(fixture_density_dev_l, fixture_density_dev_r),
        expected_gain_db: config.expected_gain_db(),
        fitted_gain_db,
        delta_gain_vs_expected_db: opt_delta(fitted_gain_db, config.expected_gain_db()),
        delta_gain_vs_production_contract_db: None,
        delta_decoded_peak_vs_production_contract_db: None,
        decoded_peak_dbfs,
        decoded_abs_peak,
        state_clamps,
        stability_resets,
        limiter_limited_events: limiter.limited_events,
        limiter_limited_samples: limiter.limited_samples,
        source_peak_dbfs: fixture.source_peak_dbfs,
        source_rms_dbfs: fixture.source_rms_dbfs,
        source_clip_count: fixture.source_clip_count,
        delta_residual_rms_vs_standard_db: None,
        delta_residual_rms_vs_prod_ec2_db: None,
        delta_spur_peak_vs_standard_db: None,
        delta_spur_peak_vs_prod_ec2_db: None,
        delta_direction_residual_rms: "lower_is_better".to_string(),
        delta_direction_spur_peak: "lower_is_better".to_string(),
        hard_failures: Vec::new(),
        notes,
    };
    metric.hard_failures = dsd_triage_metric_hard_failures(&metric);
    metric.hard_failures.extend(idle_ecbeam2_failures);
    metric.hard_failures.sort();
    metric.hard_failures.dedup();
    if !metric.hard_failures.is_empty() {
        metric.status = "fail".to_string();
    }
    Ok(metric)
}

fn dsd_triage_metric_hard_failures(metric: &DsdTriageMetric) -> Vec<String> {
    let mut failures = Vec::new();
    for (name, value) in [
        ("residual_rms_dbfs", metric.residual_rms_dbfs),
        ("residual_peak_spur_dbfs", metric.residual_peak_spur_dbfs),
        ("decoded_peak_dbfs", metric.decoded_peak_dbfs),
        (
            "idle_bit_density_max_deviation",
            metric.idle_bit_density_max_deviation,
        ),
        ("fitted_gain_db", metric.fitted_gain_db),
    ] {
        if !value.is_some_and(f64::is_finite) {
            failures.push(format!("{name} missing or non-finite"));
        }
    }
    hard_eq_zero(&mut failures, "stability_resets", metric.stability_resets);
    hard_eq_zero(&mut failures, "state_clamps", metric.state_clamps);
    hard_eq_zero(
        &mut failures,
        "limiter_limited_events",
        metric.limiter_limited_events,
    );
    hard_eq_zero(
        &mut failures,
        "limiter_limited_samples",
        metric.limiter_limited_samples,
    );
    hard_max(
        &mut failures,
        "decoded_peak_dbfs",
        metric.decoded_peak_dbfs,
        db(1.05),
    );
    hard_max(
        &mut failures,
        "idle_bit_density_max_deviation",
        metric.idle_bit_density_max_deviation,
        0.005,
    );
    failures
}

fn triage_residual_spectrum_metrics(
    residual: &[f64],
    sample_rate: u32,
) -> Option<DsdTriageSpectrumMetrics> {
    let spectrum = amplitude_spectrum(residual, sample_rate)?;
    let band = spectrum
        .iter()
        .filter(|(freq, amp)| *freq >= 20.0 && *freq <= 20_000.0 && amp.is_finite())
        .map(|(freq, amp)| (*freq, *amp))
        .collect::<Vec<_>>();
    if band.is_empty() {
        return None;
    }
    let (peak_hz, peak_amp) = band.iter().copied().max_by(|a, b| a.1.total_cmp(&b.1))?;
    let mut amps = band.iter().map(|(_, amp)| *amp).collect::<Vec<_>>();
    amps.sort_by(|a, b| a.total_cmp(b));
    let median = amps[amps.len() / 2].max(1e-18);
    Some(DsdTriageSpectrumMetrics {
        peak_spur_dbfs: Some(db(peak_amp.max(1e-18))),
        peak_spur_hz: Some(peak_hz),
        peak_to_median_db: Some(db(peak_amp.max(1e-18)) - db(median)),
        band_20_10k_rms_dbfs: spectrum_band_rms_dbfs(&spectrum, 20.0, 10_000.0),
        band_20_20k_rms_dbfs: spectrum_band_rms_dbfs(&spectrum, 20.0, 20_000.0),
        band_12_22k_rms_dbfs: spectrum_band_rms_dbfs(&spectrum, 12_000.0, 22_000.0),
    })
}

fn triage_worst_spectrum_metrics(
    left: DsdTriageSpectrumMetrics,
    right: DsdTriageSpectrumMetrics,
) -> DsdTriageSpectrumMetrics {
    let peak_from_right = right.peak_spur_dbfs.unwrap_or(f64::NEG_INFINITY)
        > left.peak_spur_dbfs.unwrap_or(f64::NEG_INFINITY);
    DsdTriageSpectrumMetrics {
        peak_spur_dbfs: max_opt(left.peak_spur_dbfs, right.peak_spur_dbfs),
        peak_spur_hz: if peak_from_right {
            right.peak_spur_hz
        } else {
            left.peak_spur_hz
        },
        peak_to_median_db: max_opt(left.peak_to_median_db, right.peak_to_median_db),
        band_20_10k_rms_dbfs: max_opt(left.band_20_10k_rms_dbfs, right.band_20_10k_rms_dbfs),
        band_20_20k_rms_dbfs: max_opt(left.band_20_20k_rms_dbfs, right.band_20_20k_rms_dbfs),
        band_12_22k_rms_dbfs: max_opt(left.band_12_22k_rms_dbfs, right.band_12_22k_rms_dbfs),
    }
}

fn spectrum_band_rms_dbfs(spectrum: &[(f64, f64)], low_hz: f64, high_hz: f64) -> Option<f64> {
    let power = spectrum
        .iter()
        .filter(|(freq, amp)| *freq >= low_hz && *freq <= high_hz && amp.is_finite())
        .map(|(_, amp)| (amp / 2.0f64.sqrt()).powi(2))
        .sum::<f64>();
    (power > f64::MIN_POSITIVE).then(|| db(power.sqrt()))
}

fn worst_window_rms_dbfs(signal: &[f64], sample_rate: u32, seconds: f64) -> Option<f64> {
    if signal.is_empty() || !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    let window = ((sample_rate as f64 * seconds).round() as usize).clamp(1, signal.len());
    let hop = (window / 4).max(1);
    let mut worst = None;
    let mut start = 0usize;
    while start + window <= signal.len() {
        worst = max_option(worst, db(rms(&signal[start..start + window]).max(1e-18)));
        start += hop;
    }
    worst
}

fn tone_fit_metrics(samples: &[f64], sample_rate: u32, tones: &[f64]) -> Option<TriageFitMetrics> {
    if samples.len() < 1024 || tones.is_empty() {
        return None;
    }
    let skip = ((sample_rate as f64 * 0.050).round() as usize).min(samples.len() / 4);
    let samples = &samples[skip..];
    let n = samples.len() as f64;
    let mean = samples.iter().sum::<f64>() / n;
    let mut fitted = vec![mean; samples.len()];
    for &freq in tones {
        let mut sin_dot = 0.0;
        let mut cos_dot = 0.0;
        let mut sin_pow = 0.0;
        let mut cos_pow = 0.0;
        for (idx, sample) in samples.iter().enumerate() {
            let phase = 2.0 * PI * freq * idx as f64 / sample_rate as f64;
            let s = phase.sin();
            let c = phase.cos();
            sin_dot += (sample - mean) * s;
            cos_dot += (sample - mean) * c;
            sin_pow += s * s;
            cos_pow += c * c;
        }
        let sin_coeff = sin_dot / sin_pow.max(1e-18);
        let cos_coeff = cos_dot / cos_pow.max(1e-18);
        for (idx, fitted_sample) in fitted.iter_mut().enumerate() {
            let phase = 2.0 * PI * freq * idx as f64 / sample_rate as f64;
            *fitted_sample += sin_coeff * phase.sin() + cos_coeff * phase.cos();
        }
    }
    let mut signal_power = 0.0;
    let mut noise_power = 0.0;
    let mut residual = Vec::with_capacity(samples.len());
    for (sample, fitted) in samples.iter().zip(fitted.iter()) {
        signal_power += fitted * fitted;
        let residual_sample = sample - fitted;
        noise_power += residual_sample * residual_sample;
        residual.push(residual_sample);
    }
    if signal_power <= f64::MIN_POSITIVE {
        return None;
    }
    let residual_rms = (noise_power / n).sqrt();
    let signal_rms = (signal_power / n).sqrt();
    let band_20_10k_residual_dbfs = amplitude_spectrum(&residual, sample_rate)
        .as_ref()
        .and_then(|spectrum| spectrum_band_rms_dbfs(spectrum, 20.0, 10_000.0));
    let band_4_10k_residual_dbfs = amplitude_spectrum(&residual, sample_rate)
        .as_ref()
        .and_then(|spectrum| spectrum_band_rms_dbfs(spectrum, 4_000.0, 10_000.0));
    Some(TriageFitMetrics {
        sinad_db: Some(10.0 * (signal_power / noise_power.max(f64::MIN_POSITIVE)).log10()),
        residual_dbfs: Some(db(residual_rms.max(1e-18))),
        band_20_10k_sinad_db: band_20_10k_residual_dbfs
            .map(|residual_dbfs| db(signal_rms.max(1e-18)) - residual_dbfs),
        band_20_10k_residual_dbfs,
        band_4_10k_sinad_db: band_4_10k_residual_dbfs
            .map(|residual_dbfs| db(signal_rms.max(1e-18)) - residual_dbfs),
        band_4_10k_residual_dbfs,
    })
}

fn combine_fit_metrics(
    left: Option<TriageFitMetrics>,
    right: Option<TriageFitMetrics>,
) -> TriageFitMetrics {
    TriageFitMetrics {
        sinad_db: min_opt(
            left.as_ref().and_then(|metrics| metrics.sinad_db),
            right.as_ref().and_then(|metrics| metrics.sinad_db),
        ),
        residual_dbfs: max_opt(
            left.as_ref().and_then(|metrics| metrics.residual_dbfs),
            right.as_ref().and_then(|metrics| metrics.residual_dbfs),
        ),
        band_20_10k_sinad_db: min_opt(
            left.as_ref()
                .and_then(|metrics| metrics.band_20_10k_sinad_db),
            right
                .as_ref()
                .and_then(|metrics| metrics.band_20_10k_sinad_db),
        ),
        band_20_10k_residual_dbfs: max_opt(
            left.as_ref()
                .and_then(|metrics| metrics.band_20_10k_residual_dbfs),
            right
                .as_ref()
                .and_then(|metrics| metrics.band_20_10k_residual_dbfs),
        ),
        band_4_10k_sinad_db: min_opt(
            left.as_ref()
                .and_then(|metrics| metrics.band_4_10k_sinad_db),
            right
                .as_ref()
                .and_then(|metrics| metrics.band_4_10k_sinad_db),
        ),
        band_4_10k_residual_dbfs: max_opt(
            left.as_ref()
                .and_then(|metrics| metrics.band_4_10k_residual_dbfs),
            right
                .as_ref()
                .and_then(|metrics| metrics.band_4_10k_residual_dbfs),
        ),
    }
}

fn apply_dsd_triage_deltas(metric: &mut DsdTriageMetric, baselines: &[DsdTriageMetric]) {
    let standard = baselines.iter().find(|baseline| {
        baseline.candidate_label == "standard" && baseline.fixture_id == metric.fixture_id
    });
    let prod = baselines.iter().find(|baseline| {
        baseline.candidate_label == "prod_ec2" && baseline.fixture_id == metric.fixture_id
    });
    metric.delta_residual_rms_vs_standard_db = opt_delta(
        metric.residual_rms_dbfs,
        standard.and_then(|b| b.residual_rms_dbfs),
    );
    metric.delta_residual_rms_vs_prod_ec2_db = opt_delta(
        metric.residual_rms_dbfs,
        prod.and_then(|b| b.residual_rms_dbfs),
    );
    metric.delta_spur_peak_vs_standard_db = opt_delta(
        metric.residual_peak_spur_dbfs,
        standard.and_then(|b| b.residual_peak_spur_dbfs),
    );
    metric.delta_spur_peak_vs_prod_ec2_db = opt_delta(
        metric.residual_peak_spur_dbfs,
        prod.and_then(|b| b.residual_peak_spur_dbfs),
    );
    let gain_delta_standard = opt_delta(
        metric.fitted_gain_db,
        standard.and_then(|baseline| baseline.fitted_gain_db),
    );
    let gain_delta_prod = opt_delta(
        metric.fitted_gain_db,
        prod.and_then(|baseline| baseline.fitted_gain_db),
    );
    metric.delta_gain_vs_production_contract_db = gain_delta_prod;
    metric.delta_decoded_peak_vs_production_contract_db = opt_delta(
        metric.decoded_peak_dbfs,
        prod.and_then(|baseline| baseline.decoded_peak_dbfs),
    );
    if let Some(delta) = metric.delta_gain_vs_expected_db {
        if delta.is_finite() && delta.abs() > DSD_TRIAGE_GAIN_GATE_DB {
            metric.hard_failures.push(format!(
                "fitted_gain_expected_delta_db {delta:.3} outside ±{DSD_TRIAGE_GAIN_GATE_DB:.3} dB vs declared expected_gain_db"
            ));
            metric.status = "fail".to_string();
        }
        return;
    }
    let min_gain_delta = [gain_delta_standard, gain_delta_prod]
        .into_iter()
        .flatten()
        .filter(|delta| delta.is_finite())
        .map(f64::abs)
        .min_by(|left, right| left.total_cmp(right));
    if let Some(delta) = min_gain_delta
        && delta > DSD_TRIAGE_GAIN_GATE_DB
    {
        metric.hard_failures.push(format!(
                "fitted_gain_delta_db {delta:.3} outside ±{DSD_TRIAGE_GAIN_GATE_DB:.3} dB vs required references"
            ));
        metric.status = "fail".to_string();
    }
    let peak_delta_standard = opt_delta(
        metric.decoded_peak_dbfs,
        standard.and_then(|baseline| baseline.decoded_peak_dbfs),
    );
    let peak_delta_prod = opt_delta(
        metric.decoded_peak_dbfs,
        prod.and_then(|baseline| baseline.decoded_peak_dbfs),
    );
    let min_peak_delta = [peak_delta_standard, peak_delta_prod]
        .into_iter()
        .flatten()
        .filter(|delta| delta.is_finite())
        .map(f64::abs)
        .min_by(|left, right| left.total_cmp(right));
    if let Some(delta) = min_peak_delta
        && delta > DSD_TRIAGE_DECODED_PEAK_GATE_DB
    {
        metric.hard_failures.push(format!(
                "decoded_peak_delta_db {delta:.3} outside ±{DSD_TRIAGE_DECODED_PEAK_GATE_DB:.3} dB vs required references"
            ));
        metric.status = "fail".to_string();
    }
}

#[allow(clippy::too_many_arguments)]
fn score_dsd_triage(
    run_id: &str,
    candidate_label: &str,
    measurements: &[DsdTriageMetric],
    baselines: &[DsdTriageMetric],
    hard_failures: &[String],
    render_ms_total: f64,
    wall_ms_total: f64,
    hit_wall_target: bool,
    allow_slow: bool,
) -> DsdTriageScores {
    let (score_real_world, score_synthetic, score_edge_case) =
        score_dsd_triage_sections(measurements, baselines);
    let score_total_raw = score_real_world + score_synthetic + score_edge_case;
    let score_anchor = triage_baseline_anchor_score(baselines).unwrap_or(75.0);
    let score_delta_from_anchor = score_total_raw - score_anchor;
    let standard_render = baselines
        .iter()
        .filter(|metric| metric.candidate_label == "standard")
        .filter_map(|metric| metric.render_ms)
        .sum::<f64>();
    let prod_render = baselines
        .iter()
        .filter(|metric| metric.candidate_label == "prod_ec2")
        .filter_map(|metric| metric.render_ms)
        .sum::<f64>();
    let render_factor_vs_standard =
        (standard_render > 0.0).then_some(render_ms_total / standard_render);
    let render_factor_vs_prod_ec2 = (prod_render > 0.0).then_some(render_ms_total / prod_render);
    let primary_rejection_reason = hard_failures.first().cloned().unwrap_or_default();
    let decision = if !hard_failures.is_empty() {
        if primary_rejection_reason.contains("baseline") {
            "reject_baseline_cache"
        } else if primary_rejection_reason.contains("density") {
            "reject_density"
        } else if primary_rejection_reason.contains("state_clamps")
            || primary_rejection_reason.contains("stability_resets")
        {
            "reject_stability"
        } else if primary_rejection_reason.contains("limiter_limited") {
            "reject_limiter"
        } else if primary_rejection_reason.contains("wall_ms_total") && !allow_slow {
            "reject_runtime"
        } else if primary_rejection_reason.contains("fitted_gain")
            || primary_rejection_reason.contains("decoded_peak")
        {
            "reject_level"
        } else if primary_rejection_reason.contains("spur") {
            "reject_spur"
        } else {
            "reject_residual"
        }
    } else if !hit_wall_target && !allow_slow {
        "reject_runtime"
    } else if !hit_wall_target {
        "quality_only_too_slow"
    } else if score_delta_from_anchor >= 20.0 {
        "pass_triage"
    } else if score_delta_from_anchor >= 0.0 {
        "keep_for_comparison"
    } else {
        "below_triage_threshold"
    }
    .to_string();
    let status = if decision.starts_with("reject") || decision == "below_triage_threshold" {
        "fail"
    } else {
        "pass"
    }
    .to_string();
    DsdTriageScores {
        run_id: run_id.to_string(),
        candidate_label: candidate_label.to_string(),
        status,
        decision,
        score_total_raw,
        score_anchor,
        score_delta_from_anchor,
        score_real_world,
        score_synthetic,
        score_edge_case,
        render_ms_total,
        wall_ms_total,
        render_factor_vs_standard,
        render_factor_vs_prod_ec2,
        hit_wall_target,
        primary_win: triage_primary_win(measurements, baselines),
        primary_weakness: triage_primary_weakness(measurements, baselines),
        primary_rejection_reason,
    }
}

fn score_dsd_triage_sections(
    measurements: &[DsdTriageMetric],
    baselines: &[DsdTriageMetric],
) -> (f64, f64, f64) {
    let score_real_world = measurements
        .iter()
        .find(|metric| metric.fixture_class == "real_world")
        .map(|metric| score_triage_fixture(metric, baselines))
        .unwrap_or(0.0);
    let score_synthetic = measurements
        .iter()
        .find(|metric| metric.fixture_class == "synthetic")
        .map(|metric| score_triage_fixture(metric, baselines))
        .unwrap_or(0.0);
    let score_edge_case = score_triage_sine_section(measurements, baselines);
    (score_real_world, score_synthetic, score_edge_case)
}

fn triage_baseline_anchor_score(baselines: &[DsdTriageMetric]) -> Option<f64> {
    ["standard", "prod_ec2"]
        .into_iter()
        .filter_map(|label| {
            let measurements = baselines
                .iter()
                .filter(|metric| metric.candidate_label == label)
                .cloned()
                .collect::<Vec<_>>();
            if measurements.is_empty() {
                return None;
            }
            let (real_world, synthetic, edge_case) =
                score_dsd_triage_sections(&measurements, baselines);
            Some(real_world + synthetic + edge_case)
        })
        .min_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal))
}

fn score_triage_fixture(metric: &DsdTriageMetric, baselines: &[DsdTriageMetric]) -> f64 {
    match metric.fixture_class.as_str() {
        "real_world" => {
            score_lower(
                metric.residual_20_10k_rms_dbfs,
                metric,
                baselines,
                10.0,
                |baseline| baseline.residual_20_10k_rms_dbfs,
            ) + score_lower(
                metric.residual_12_22k_rms_dbfs,
                metric,
                baselines,
                8.0,
                |baseline| baseline.residual_12_22k_rms_dbfs,
            ) + score_lower(
                metric.residual_peak_spur_dbfs,
                metric,
                baselines,
                6.0,
                |baseline| baseline.residual_peak_spur_dbfs,
            ) + score_lower(
                metric.residual_peak_dbfs,
                metric,
                baselines,
                2.0,
                |baseline| baseline.residual_peak_dbfs,
            ) + score_lower(
                metric.residual_relative_db,
                metric,
                baselines,
                2.0,
                |baseline| baseline.residual_relative_db,
            ) + score_lower(
                metric.worst_window_residual_dbfs,
                metric,
                baselines,
                3.0,
                |baseline| baseline.worst_window_residual_dbfs,
            ) + score_safety(metric, 2.33)
        }
        "synthetic" => {
            score_higher(
                metric.multitone_fit_sinad_db,
                metric,
                baselines,
                10.0,
                |baseline| baseline.multitone_fit_sinad_db,
            ) + score_lower(
                metric.residual_peak_spur_dbfs,
                metric,
                baselines,
                8.0,
                |baseline| baseline.residual_peak_spur_dbfs,
            ) + score_lower(
                metric.multitone_fit_residual_dbfs,
                metric,
                baselines,
                6.0,
                |baseline| baseline.multitone_fit_residual_dbfs,
            ) + score_lower(
                metric.residual_20_10k_rms_dbfs,
                metric,
                baselines,
                5.0,
                |baseline| baseline.residual_20_10k_rms_dbfs,
            ) + score_safety(metric, 4.33)
        }
        "edge_case" => {
            score_higher(
                metric.sine_tone_fit_sinad_db,
                metric,
                baselines,
                10.0,
                |baseline| baseline.sine_tone_fit_sinad_db,
            ) + score_lower(
                metric.residual_peak_spur_dbfs,
                metric,
                baselines,
                8.0,
                |baseline| baseline.residual_peak_spur_dbfs,
            ) + score_lower(
                metric.sine_tone_fit_residual_dbfs,
                metric,
                baselines,
                6.0,
                |baseline| baseline.sine_tone_fit_residual_dbfs,
            ) + score_lower(
                metric.worst_window_residual_dbfs,
                metric,
                baselines,
                5.0,
                |baseline| baseline.worst_window_residual_dbfs,
            ) + score_safety(metric, 4.33)
        }
        _ => 0.0,
    }
}

fn score_triage_sine_section(
    measurements: &[DsdTriageMetric],
    baselines: &[DsdTriageMetric],
) -> f64 {
    let edge = measurements
        .iter()
        .find(|metric| metric.fixture_class == "edge_case");
    let mid_band = measurements
        .iter()
        .find(|metric| metric.fixture_class == "mid_band");
    match (mid_band, edge) {
        (Some(mid_band), Some(edge)) => {
            score_higher(
                mid_band.sine_tone_fit_4_10k_sinad_db,
                mid_band,
                baselines,
                10.0,
                |baseline| baseline.sine_tone_fit_4_10k_sinad_db,
            ) + score_lower(
                edge.residual_20_10k_rms_dbfs,
                edge,
                baselines,
                13.0,
                |baseline| baseline.residual_20_10k_rms_dbfs,
            ) + score_lower(
                edge.sine_tone_fit_residual_dbfs,
                edge,
                baselines,
                6.0,
                |baseline| baseline.sine_tone_fit_residual_dbfs,
            ) + score_safety(edge, 4.33)
        }
        (None, Some(edge)) => score_triage_fixture(edge, baselines),
        (Some(mid_band), None) => {
            score_higher(
                mid_band.sine_tone_fit_4_10k_sinad_db,
                mid_band,
                baselines,
                10.0,
                |baseline| baseline.sine_tone_fit_4_10k_sinad_db,
            ) + score_safety(mid_band, 4.33)
        }
        (None, None) => 0.0,
    }
}

fn score_lower<F>(
    candidate: Option<f64>,
    metric: &DsdTriageMetric,
    baselines: &[DsdTriageMetric],
    points: f64,
    accessor: F,
) -> f64
where
    F: FnMut(&DsdTriageMetric) -> Option<f64>,
{
    let Some(candidate) = candidate else {
        return 0.0;
    };
    let Some(reference) = best_lower_reference(metric, baselines, accessor) else {
        return points * 0.5;
    };
    let delta = candidate - reference;
    points * linear_score(delta, 0.50, 0.0, -1.50)
}

fn score_higher<F>(
    candidate: Option<f64>,
    metric: &DsdTriageMetric,
    baselines: &[DsdTriageMetric],
    points: f64,
    accessor: F,
) -> f64
where
    F: FnMut(&DsdTriageMetric) -> Option<f64>,
{
    let Some(candidate) = candidate else {
        return 0.0;
    };
    let Some(reference) = best_higher_reference(metric, baselines, accessor) else {
        return points * 0.5;
    };
    let delta = candidate - reference;
    points * linear_score(delta, -1.0, 0.0, 3.0)
}

fn score_safety(metric: &DsdTriageMetric, points: f64) -> f64 {
    if metric.hard_failures.is_empty() {
        points
    } else {
        0.0
    }
}

fn linear_score(delta: f64, zero_at: f64, half_at: f64, full_at: f64) -> f64 {
    if full_at > zero_at {
        if delta <= zero_at {
            0.0
        } else if delta >= full_at {
            1.0
        } else if delta <= half_at {
            0.5 * (delta - zero_at) / (half_at - zero_at).max(1e-18)
        } else {
            0.5 + 0.5 * (delta - half_at) / (full_at - half_at).max(1e-18)
        }
    } else {
        if delta >= zero_at {
            0.0
        } else if delta <= full_at {
            1.0
        } else if delta >= half_at {
            0.5 * (zero_at - delta) / (zero_at - half_at).max(1e-18)
        } else {
            0.5 + 0.5 * (half_at - delta) / (half_at - full_at).max(1e-18)
        }
    }
    .clamp(0.0, 1.0)
}

fn best_lower_reference<F>(
    metric: &DsdTriageMetric,
    baselines: &[DsdTriageMetric],
    mut value: F,
) -> Option<f64>
where
    F: FnMut(&DsdTriageMetric) -> Option<f64>,
{
    baselines
        .iter()
        .filter(|baseline| baseline.fixture_id == metric.fixture_id)
        .filter_map(|baseline| value(baseline).filter(|value| value.is_finite()))
        .min_by(|left, right| left.total_cmp(right))
}

fn best_higher_reference<F>(
    metric: &DsdTriageMetric,
    baselines: &[DsdTriageMetric],
    mut value: F,
) -> Option<f64>
where
    F: FnMut(&DsdTriageMetric) -> Option<f64>,
{
    baselines
        .iter()
        .filter(|baseline| baseline.fixture_id == metric.fixture_id)
        .filter_map(|baseline| value(baseline).filter(|value| value.is_finite()))
        .max_by(|left, right| left.total_cmp(right))
}

fn triage_primary_win(measurements: &[DsdTriageMetric], baselines: &[DsdTriageMetric]) -> String {
    triage_scored_deltas(measurements, baselines)
        .into_iter()
        .filter(|delta| delta.better_delta_db > 0.0)
        .max_by(|left, right| {
            (left.better_delta_db * left.points).total_cmp(&(right.better_delta_db * right.points))
        })
        .map(|delta| {
            format!(
                "{} {} {:.2} dB better",
                delta.fixture_id, delta.metric_name, delta.better_delta_db
            )
        })
        .unwrap_or_default()
}

fn triage_primary_weakness(
    measurements: &[DsdTriageMetric],
    baselines: &[DsdTriageMetric],
) -> String {
    triage_scored_deltas(measurements, baselines)
        .into_iter()
        .filter(|delta| delta.better_delta_db < 0.0)
        .max_by(|left, right| {
            ((-left.better_delta_db) * left.points)
                .total_cmp(&((-right.better_delta_db) * right.points))
        })
        .map(|delta| {
            format!(
                "{} {} {:.2} dB worse",
                delta.fixture_id, delta.metric_name, -delta.better_delta_db
            )
        })
        .unwrap_or_default()
}

fn triage_scored_deltas(
    measurements: &[DsdTriageMetric],
    baselines: &[DsdTriageMetric],
) -> Vec<TriageScoredDelta> {
    let mut deltas = Vec::new();
    let has_mid_band = measurements
        .iter()
        .any(|metric| metric.fixture_class == "mid_band");
    for metric in measurements {
        match metric.fixture_class.as_str() {
            "real_world" => {
                push_lower_scored_delta(
                    &mut deltas,
                    metric,
                    baselines,
                    "residual_20_10k_rms_dbfs",
                    metric.residual_20_10k_rms_dbfs,
                    10.0,
                    |baseline| baseline.residual_20_10k_rms_dbfs,
                );
                push_lower_scored_delta(
                    &mut deltas,
                    metric,
                    baselines,
                    "residual_12_22k_rms_dbfs",
                    metric.residual_12_22k_rms_dbfs,
                    8.0,
                    |baseline| baseline.residual_12_22k_rms_dbfs,
                );
                push_lower_scored_delta(
                    &mut deltas,
                    metric,
                    baselines,
                    "residual_peak_spur_dbfs",
                    metric.residual_peak_spur_dbfs,
                    6.0,
                    |baseline| baseline.residual_peak_spur_dbfs,
                );
                push_lower_scored_delta(
                    &mut deltas,
                    metric,
                    baselines,
                    "residual_peak_dbfs",
                    metric.residual_peak_dbfs,
                    2.0,
                    |baseline| baseline.residual_peak_dbfs,
                );
                push_lower_scored_delta(
                    &mut deltas,
                    metric,
                    baselines,
                    "residual_relative_db",
                    metric.residual_relative_db,
                    2.0,
                    |baseline| baseline.residual_relative_db,
                );
                push_lower_scored_delta(
                    &mut deltas,
                    metric,
                    baselines,
                    "worst_window_residual_dbfs",
                    metric.worst_window_residual_dbfs,
                    3.0,
                    |baseline| baseline.worst_window_residual_dbfs,
                );
            }
            "synthetic" => {
                push_higher_scored_delta(
                    &mut deltas,
                    metric,
                    baselines,
                    "multitone_fit_sinad_db",
                    metric.multitone_fit_sinad_db,
                    10.0,
                    |baseline| baseline.multitone_fit_sinad_db,
                );
                push_lower_scored_delta(
                    &mut deltas,
                    metric,
                    baselines,
                    "residual_peak_spur_dbfs",
                    metric.residual_peak_spur_dbfs,
                    8.0,
                    |baseline| baseline.residual_peak_spur_dbfs,
                );
                push_lower_scored_delta(
                    &mut deltas,
                    metric,
                    baselines,
                    "multitone_fit_residual_dbfs",
                    metric.multitone_fit_residual_dbfs,
                    6.0,
                    |baseline| baseline.multitone_fit_residual_dbfs,
                );
                push_lower_scored_delta(
                    &mut deltas,
                    metric,
                    baselines,
                    "residual_20_10k_rms_dbfs",
                    metric.residual_20_10k_rms_dbfs,
                    5.0,
                    |baseline| baseline.residual_20_10k_rms_dbfs,
                );
            }
            "edge_case" => {
                if !has_mid_band {
                    push_higher_scored_delta(
                        &mut deltas,
                        metric,
                        baselines,
                        "sine_tone_fit_sinad_db",
                        metric.sine_tone_fit_sinad_db,
                        10.0,
                        |baseline| baseline.sine_tone_fit_sinad_db,
                    );
                }
                push_lower_scored_delta(
                    &mut deltas,
                    metric,
                    baselines,
                    "residual_20_10k_rms_dbfs",
                    metric.residual_20_10k_rms_dbfs,
                    13.0,
                    |baseline| baseline.residual_20_10k_rms_dbfs,
                );
                push_lower_scored_delta(
                    &mut deltas,
                    metric,
                    baselines,
                    "sine_tone_fit_residual_dbfs",
                    metric.sine_tone_fit_residual_dbfs,
                    6.0,
                    |baseline| baseline.sine_tone_fit_residual_dbfs,
                );
            }
            "mid_band" => {
                push_higher_scored_delta(
                    &mut deltas,
                    metric,
                    baselines,
                    "sine_tone_fit_4_10k_sinad_db",
                    metric.sine_tone_fit_4_10k_sinad_db,
                    10.0,
                    |baseline| baseline.sine_tone_fit_4_10k_sinad_db,
                );
            }
            _ => {}
        }
    }
    deltas
}

fn push_lower_scored_delta<F>(
    deltas: &mut Vec<TriageScoredDelta>,
    metric: &DsdTriageMetric,
    baselines: &[DsdTriageMetric],
    metric_name: &'static str,
    candidate: Option<f64>,
    points: f64,
    accessor: F,
) where
    F: FnMut(&DsdTriageMetric) -> Option<f64>,
{
    let Some(candidate) = candidate.filter(|value| value.is_finite()) else {
        return;
    };
    let Some(reference) = best_lower_reference(metric, baselines, accessor) else {
        return;
    };
    deltas.push(TriageScoredDelta {
        fixture_id: metric.fixture_id.clone(),
        metric_name,
        better_delta_db: reference - candidate,
        points,
    });
}

fn push_higher_scored_delta<F>(
    deltas: &mut Vec<TriageScoredDelta>,
    metric: &DsdTriageMetric,
    baselines: &[DsdTriageMetric],
    metric_name: &'static str,
    candidate: Option<f64>,
    points: f64,
    accessor: F,
) where
    F: FnMut(&DsdTriageMetric) -> Option<f64>,
{
    let Some(candidate) = candidate.filter(|value| value.is_finite()) else {
        return;
    };
    let Some(reference) = best_higher_reference(metric, baselines, accessor) else {
        return;
    };
    deltas.push(TriageScoredDelta {
        fixture_id: metric.fixture_id.clone(),
        metric_name,
        better_delta_db: candidate - reference,
        points,
    });
}

fn write_dsd_triage_artifacts(
    report: &DsdTriageReport,
    reproduction_command: &str,
) -> Result<(), String> {
    fs::create_dir_all(&report.out_dir).map_err(|err| err.to_string())?;
    fs::write(
        report.out_dir.join("manifest.json"),
        versioned_json(report).map_err(|err| err.to_string())?,
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        report.out_dir.join("fixture_manifest.resolved.json"),
        serde_json::to_string_pretty(&report.fixture_manifest).map_err(|err| err.to_string())?,
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        report.out_dir.join("baseline_cache_manifest.json"),
        serde_json::to_string_pretty(&report.baseline_cache_manifest)
            .map_err(|err| err.to_string())?,
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        report.out_dir.join("triage_metrics.csv"),
        triage_metrics_csv(report),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        report.out_dir.join("triage_scores.csv"),
        triage_scores_csv(report),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        report.out_dir.join("fixture_summary.md"),
        triage_fixture_summary_markdown(report),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        report.out_dir.join("leaderboard.md"),
        triage_leaderboard_markdown(report),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        report.out_dir.join("reproduction_command.sh"),
        format!("#!/usr/bin/env bash\nset -euo pipefail\n{reproduction_command}\n"),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        report.out_dir.join("run_complete.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "run_id": report.run_id,
            "decision": report.scores.decision,
            "score_total_raw": report.scores.score_total_raw,
            "score_anchor": report.scores.score_anchor,
            "score_delta_from_anchor": report.scores.score_delta_from_anchor,
            "completed": true,
        }))
        .map_err(|err| err.to_string())?,
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

fn triage_metrics_csv(report: &DsdTriageReport) -> String {
    let mut csv = "run_id,host_id,git_commit,measurement_version,scoring_version,fixture_set_version,candidate_label,fixture_id,fixture_class,rate,status,render_ms,wall_ms,residual_relative_db,residual_rms_dbfs,residual_peak_dbfs,worst_window_residual_dbfs,residual_peak_spur_dbfs,residual_peak_spur_hz,residual_peak_to_median_db,residual_20_10k_rms_dbfs,residual_20_20k_rms_dbfs,residual_12_22k_rms_dbfs,sine_tone_fit_sinad_db,sine_tone_fit_residual_dbfs,sine_tone_fit_20_10k_sinad_db,sine_tone_fit_20_10k_residual_dbfs,sine_tone_fit_4_10k_sinad_db,sine_tone_fit_4_10k_residual_dbfs,multitone_fit_sinad_db,multitone_fit_residual_dbfs,dsd_ultrasonic_24_50k_max_dbfs,dsd_ultrasonic_50_100k_max_dbfs,dsd_ultrasonic_100_200k_max_dbfs,idle_bit_density,idle_bit_density_max_deviation,fixture_bit_density,fixture_bit_density_max_deviation,expected_gain_db,fitted_gain_db,delta_gain_vs_expected_db,delta_gain_vs_production_contract_db,delta_decoded_peak_vs_production_contract_db,decoded_peak_dbfs,decoded_abs_peak,state_clamps,stability_resets,limiter_limited_events,limiter_limited_samples,source_peak_dbfs,source_rms_dbfs,source_clip_count,delta_residual_rms_vs_standard_db,delta_residual_rms_vs_prod_ec2_db,delta_spur_peak_vs_standard_db,delta_spur_peak_vs_prod_ec2_db,delta_direction_residual_rms,delta_direction_spur_peak,hard_failures,notes\n".to_string();
    for metric in &report.measurements {
        let row = vec![
            metric.run_id.clone(),
            metric.host_id.clone(),
            metric.git_commit.clone().unwrap_or_default(),
            metric.measurement_version.clone(),
            metric.scoring_version.clone(),
            metric.fixture_set_version.clone(),
            metric.candidate_label.clone(),
            metric.fixture_id.clone(),
            metric.fixture_class.clone(),
            metric.rate.clone(),
            metric.status.clone(),
            fmt_csv_opt(metric.render_ms),
            fmt_csv_opt(metric.wall_ms),
            fmt_csv_opt(metric.residual_relative_db),
            fmt_csv_opt(metric.residual_rms_dbfs),
            fmt_csv_opt(metric.residual_peak_dbfs),
            fmt_csv_opt(metric.worst_window_residual_dbfs),
            fmt_csv_opt(metric.residual_peak_spur_dbfs),
            fmt_csv_opt(metric.residual_peak_spur_hz),
            fmt_csv_opt(metric.residual_peak_to_median_db),
            fmt_csv_opt(metric.residual_20_10k_rms_dbfs),
            fmt_csv_opt(metric.residual_20_20k_rms_dbfs),
            fmt_csv_opt(metric.residual_12_22k_rms_dbfs),
            fmt_csv_opt(metric.sine_tone_fit_sinad_db),
            fmt_csv_opt(metric.sine_tone_fit_residual_dbfs),
            fmt_csv_opt(metric.sine_tone_fit_20_10k_sinad_db),
            fmt_csv_opt(metric.sine_tone_fit_20_10k_residual_dbfs),
            fmt_csv_opt(metric.sine_tone_fit_4_10k_sinad_db),
            fmt_csv_opt(metric.sine_tone_fit_4_10k_residual_dbfs),
            fmt_csv_opt(metric.multitone_fit_sinad_db),
            fmt_csv_opt(metric.multitone_fit_residual_dbfs),
            fmt_csv_opt(metric.dsd_ultrasonic_24_50k_max_dbfs),
            fmt_csv_opt(metric.dsd_ultrasonic_50_100k_max_dbfs),
            fmt_csv_opt(metric.dsd_ultrasonic_100_200k_max_dbfs),
            fmt_csv_opt(metric.idle_bit_density),
            fmt_csv_opt(metric.idle_bit_density_max_deviation),
            fmt_csv_opt(metric.fixture_bit_density),
            fmt_csv_opt(metric.fixture_bit_density_max_deviation),
            fmt_csv_opt(metric.expected_gain_db),
            fmt_csv_opt(metric.fitted_gain_db),
            fmt_csv_opt(metric.delta_gain_vs_expected_db),
            fmt_csv_opt(metric.delta_gain_vs_production_contract_db),
            fmt_csv_opt(metric.delta_decoded_peak_vs_production_contract_db),
            fmt_csv_opt(metric.decoded_peak_dbfs),
            fmt_csv_opt(metric.decoded_abs_peak),
            metric.state_clamps.to_string(),
            metric.stability_resets.to_string(),
            metric.limiter_limited_events.to_string(),
            metric.limiter_limited_samples.to_string(),
            fmt_csv_opt(metric.source_peak_dbfs),
            fmt_csv_opt(metric.source_rms_dbfs),
            metric
                .source_clip_count
                .map(|v| v.to_string())
                .unwrap_or_default(),
            fmt_csv_opt(metric.delta_residual_rms_vs_standard_db),
            fmt_csv_opt(metric.delta_residual_rms_vs_prod_ec2_db),
            fmt_csv_opt(metric.delta_spur_peak_vs_standard_db),
            fmt_csv_opt(metric.delta_spur_peak_vs_prod_ec2_db),
            metric.delta_direction_residual_rms.clone(),
            metric.delta_direction_spur_peak.clone(),
            metric.hard_failures.join(";"),
            metric.notes.join(";"),
        ];
        csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
        csv.push('\n');
    }
    csv
}

fn triage_scores_csv(report: &DsdTriageReport) -> String {
    let mut csv = "run_id,candidate_label,status,decision,score_total_raw,score_anchor,score_delta_from_anchor,score_real_world,score_synthetic,score_edge_case,render_ms_total,wall_ms_total,render_factor_vs_standard,render_factor_vs_prod_ec2,hit_wall_target,primary_win,primary_weakness,primary_rejection_reason\n".to_string();
    let s = &report.scores;
    let row = vec![
        s.run_id.clone(),
        s.candidate_label.clone(),
        s.status.clone(),
        s.decision.clone(),
        format!("{:.6}", s.score_total_raw),
        format!("{:.6}", s.score_anchor),
        format!("{:.6}", s.score_delta_from_anchor),
        format!("{:.6}", s.score_real_world),
        format!("{:.6}", s.score_synthetic),
        format!("{:.6}", s.score_edge_case),
        format!("{:.6}", s.render_ms_total),
        format!("{:.6}", s.wall_ms_total),
        fmt_csv_opt(s.render_factor_vs_standard),
        fmt_csv_opt(s.render_factor_vs_prod_ec2),
        s.hit_wall_target.to_string(),
        s.primary_win.clone(),
        s.primary_weakness.clone(),
        s.primary_rejection_reason.clone(),
    ];
    csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
    csv.push('\n');
    csv
}

fn triage_fixture_summary_markdown(report: &DsdTriageReport) -> String {
    let mut md = "# DSD64 triage fixture summary\n\n".to_string();
    md.push_str("| Fixture | Class | Status | RMS dBFS | 20-10k dBFS | Spur dBFS | Spur Hz | Expected gain dB | Gain dB | Gain vs prod dB | Idle density dev | Decoded peak dBFS |\n");
    md.push_str(
        "| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n",
    );
    for metric in &report.measurements {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            metric.fixture_id,
            metric.fixture_class,
            metric.status,
            fmt_md_opt(metric.residual_rms_dbfs),
            fmt_md_opt(metric.residual_20_10k_rms_dbfs),
            fmt_md_opt(metric.residual_peak_spur_dbfs),
            fmt_md_opt(metric.residual_peak_spur_hz),
            fmt_md_opt(metric.expected_gain_db),
            fmt_md_opt(metric.fitted_gain_db),
            fmt_md_opt(metric.delta_gain_vs_production_contract_db),
            fmt_md_opt(metric.idle_bit_density_max_deviation),
            fmt_md_opt(metric.decoded_peak_dbfs),
        ));
    }
    md.push_str("\n## Scoring Baselines\n\n");
    md.push_str("| Baseline | Fixture | Class | Status | RMS dBFS | 20-10k dBFS | Spur dBFS | Spur Hz | Gain dB | Idle density dev | Decoded peak dBFS |\n");
    md.push_str("| --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for metric in &report.baselines {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            metric.candidate_label,
            metric.fixture_id,
            metric.fixture_class,
            metric.status,
            fmt_md_opt(metric.residual_rms_dbfs),
            fmt_md_opt(metric.residual_20_10k_rms_dbfs),
            fmt_md_opt(metric.residual_peak_spur_dbfs),
            fmt_md_opt(metric.residual_peak_spur_hz),
            fmt_md_opt(metric.fitted_gain_db),
            fmt_md_opt(metric.idle_bit_density_max_deviation),
            fmt_md_opt(metric.decoded_peak_dbfs),
        ));
    }
    md
}

fn triage_leaderboard_markdown(report: &DsdTriageReport) -> String {
    format!(
        "# DSD64 triage leaderboard\n\n| Candidate | Decision | Score | Anchor | Delta | Real | Synthetic | Edge | Wall ms |\n| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n| {} | {} | {:.2} | {:.2} | {:+.2} | {:.2} | {:.2} | {:.2} | {:.0} |\n",
        report.candidate_label,
        report.scores.decision,
        report.scores.score_total_raw,
        report.scores.score_anchor,
        report.scores.score_delta_from_anchor,
        report.scores.score_real_world,
        report.scores.score_synthetic,
        report.scores.score_edge_case,
        report.wall_ms_total,
    )
}

fn load_pcm16_wav_excerpt(
    path: &Path,
    start_sec: f64,
    end_sec: f64,
) -> Result<(Vec<f64>, Vec<f64>), String> {
    let data =
        fs::read(path).map_err(|err| format!("failed to read WAV {}: {err}", path.display()))?;
    if data.len() < 44 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err(format!("{} is not a RIFF/WAVE file", path.display()));
    }
    let mut offset = 12usize;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    let mut pcm_data: Option<&[u8]> = None;
    while offset + 8 <= data.len() {
        let id = &data[offset..offset + 4];
        let len = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap()) as usize;
        offset += 8;
        if offset + len > data.len() {
            return Err("truncated WAV chunk".to_string());
        }
        match id {
            b"fmt " => {
                if len < 16 {
                    return Err("WAV fmt chunk too short".to_string());
                }
                let audio_format = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap());
                let channels = u16::from_le_bytes(data[offset + 2..offset + 4].try_into().unwrap());
                let sample_rate =
                    u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap());
                let bits = u16::from_le_bytes(data[offset + 14..offset + 16].try_into().unwrap());
                fmt = Some((audio_format, channels, sample_rate, bits));
            }
            b"data" => {
                pcm_data = Some(&data[offset..offset + len]);
            }
            _ => {}
        }
        offset += len + (len % 2);
    }
    let (audio_format, channels, sample_rate, bits) =
        fmt.ok_or_else(|| "WAV missing fmt chunk".to_string())?;
    if audio_format != 1 || channels != 2 || sample_rate != 44_100 || bits != 16 {
        return Err(format!(
            "WAV must be PCM 16-bit stereo 44.1 kHz, got format={audio_format} channels={channels} sample_rate={sample_rate} bits={bits}"
        ));
    }
    let pcm_data = pcm_data.ok_or_else(|| "WAV missing data chunk".to_string())?;
    let bytes_per_frame = 4usize;
    let start = ((start_sec * sample_rate as f64).round() as usize) * bytes_per_frame;
    let end = ((end_sec * sample_rate as f64).round() as usize) * bytes_per_frame;
    if start >= end || end > pcm_data.len() {
        return Err("WAV excerpt range is outside data chunk".to_string());
    }
    let mut left = Vec::with_capacity((end - start) / bytes_per_frame);
    let mut right = Vec::with_capacity((end - start) / bytes_per_frame);
    for frame in pcm_data[start..end].chunks_exact(bytes_per_frame) {
        let l = i16::from_le_bytes([frame[0], frame[1]]) as f64 / 32768.0;
        let r = i16::from_le_bytes([frame[2], frame[3]]) as f64 / 32768.0;
        left.push(l);
        right.push(r);
    }
    Ok((left, right))
}

fn dsd_triage_decimate_to_pcm(bits: &[f64], wire_rate: u32, target_rate: u32) -> Option<Vec<f64>> {
    if target_rate == 0 || !wire_rate.is_multiple_of(target_rate) {
        return None;
    }
    let mut resampler = SincResampler::new(FilterType::SincExtreme32k, wire_rate, target_rate);
    resampler.input(bits, bits);
    let mut interleaved = Vec::new();
    resampler.process(&mut interleaved);
    resampler.drain_eof(&mut interleaved);
    Some(
        interleaved
            .chunks_exact(2)
            .map(|frame| frame[0])
            .collect::<Vec<_>>(),
    )
}

fn triage_reference_lowpass_pcm(samples: &[f64], sample_rate: u32) -> Option<Vec<f64>> {
    if samples.len() < 1024 {
        return Some(samples.to_vec());
    }
    if DSD_TRIAGE_REFERENCE_LOWPASS_STOP_HZ >= sample_rate as f64 * 0.5 {
        return Some(samples.to_vec());
    }
    let pad_len = ((sample_rate as f64 * 0.050).round() as usize)
        .max(1)
        .min(samples.len().saturating_sub(2));
    let mut padded = Vec::with_capacity(samples.len() + 2 * pad_len);
    for idx in (1..=pad_len).rev() {
        padded.push(samples[idx]);
    }
    padded.extend_from_slice(samples);
    for idx in 0..pad_len {
        padded.push(samples[samples.len() - 2 - idx]);
    }
    let fft_len = padded.len().next_power_of_two();
    let mut planner = RealFftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(fft_len);
    let inverse = planner.plan_fft_inverse(fft_len);
    let mut time = forward.make_input_vec();
    time[..padded.len()].copy_from_slice(&padded);
    let mut spectrum = forward.make_output_vec();
    forward.process(&mut time, &mut spectrum).ok()?;
    let bin_hz = sample_rate as f64 / fft_len as f64;
    for (bin, value) in spectrum.iter_mut().enumerate() {
        let freq = bin as f64 * bin_hz;
        let gain = triage_reference_lowpass_gain(freq);
        value.re *= gain;
        value.im *= gain;
    }
    let mut filtered = inverse.make_output_vec();
    inverse.process(&mut spectrum, &mut filtered).ok()?;
    let scale = fft_len as f64;
    let mut cropped = filtered[pad_len..pad_len + samples.len()].to_vec();
    for sample in &mut cropped {
        *sample /= scale;
    }
    Some(cropped)
}

fn triage_reference_lowpass_gain(freq_hz: f64) -> f64 {
    if freq_hz <= DSD_TRIAGE_REFERENCE_LOWPASS_PASS_HZ {
        1.0
    } else if freq_hz >= DSD_TRIAGE_REFERENCE_LOWPASS_STOP_HZ {
        0.0
    } else {
        let x = (freq_hz - DSD_TRIAGE_REFERENCE_LOWPASS_PASS_HZ)
            / (DSD_TRIAGE_REFERENCE_LOWPASS_STOP_HZ - DSD_TRIAGE_REFERENCE_LOWPASS_PASS_HZ)
                .max(1.0);
        0.5 + 0.5 * (PI * x).cos()
    }
}

fn triage_reference_domain_residual(analysis: &RoundtripChannelAnalysis) -> Vec<f64> {
    triage_reference_domain_samples(&analysis.residual, analysis.gain)
}

fn triage_reference_domain_samples(samples: &[f64], gain: f64) -> Vec<f64> {
    if gain.is_finite() && gain.abs() > 1e-9 {
        samples.iter().map(|sample| sample / gain).collect()
    } else {
        samples.to_vec()
    }
}

fn triage_reference_domain_residual_rms_dbfs(analysis: &RoundtripChannelAnalysis) -> f64 {
    db(rms(&triage_reference_domain_residual(analysis)).max(1e-18))
}

fn triage_reference_domain_residual_peak_dbfs(analysis: &RoundtripChannelAnalysis) -> f64 {
    db(sample_abs_peak(&triage_reference_domain_residual(analysis))
        .unwrap_or(0.0)
        .max(1e-18))
}

fn source_sanity(left: &[f64], right: &[f64]) -> (Option<f64>, Option<f64>, u64) {
    let peak =
        max_opt(sample_abs_peak(left), sample_abs_peak(right)).map(|peak| db(peak.max(1e-18)));
    let samples = left.iter().chain(right.iter()).copied().collect::<Vec<_>>();
    let rms_dbfs = (!samples.is_empty()).then(|| db(rms(&samples).max(1e-18)));
    let clips = samples
        .iter()
        .filter(|sample| sample.abs() >= 32767.0 / 32768.0)
        .count() as u64;
    (peak, rms_dbfs, clips)
}

fn average_opt(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some((left + right) * 0.5),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn triage_run_id(label: &str) -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("{}-{seconds}", sanitize_cache_component(label))
}

fn sanitize_cache_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[derive(Debug, Clone, Serialize)]
pub struct DsdRoundtripMeasurement {
    pub candidate_index: usize,
    pub fixture: String,
    pub filter: String,
    pub modulator: String,
    pub source_rate: u32,
    pub dsd_rate: String,
    pub wire_rate: u32,
    pub seconds: f64,
    pub render_ms: Option<f64>,
    pub alignment_delay_left_samples: Option<isize>,
    pub alignment_delay_right_samples: Option<isize>,
    pub alignment_gain_left: Option<f64>,
    pub alignment_gain_right: Option<f64>,
    pub correlation_left: Option<f64>,
    pub correlation_right: Option<f64>,
    pub correlation_worst: Option<f64>,
    pub residual_rms_db_left: Option<f64>,
    pub residual_rms_db_right: Option<f64>,
    pub residual_rms_db_worst: Option<f64>,
    pub inband_residual_rms_dbfs_left: Option<f64>,
    pub inband_residual_rms_dbfs_right: Option<f64>,
    pub inband_residual_rms_dbfs_worst: Option<f64>,
    pub inband_residual_peak_dbfs_left: Option<f64>,
    pub inband_residual_peak_dbfs_right: Option<f64>,
    pub inband_residual_peak_dbfs_worst: Option<f64>,
    pub inband_residual_spur_margin_db_left: Option<f64>,
    pub inband_residual_spur_margin_db_right: Option<f64>,
    pub inband_residual_spur_margin_db_worst: Option<f64>,
    pub decoded_abs_peak: Option<f64>,
    pub bit_density: Option<f64>,
    pub bit_density_left: Option<f64>,
    pub bit_density_right: Option<f64>,
    pub bit_density_max_deviation: Option<f64>,
    pub bit_density_left_max_deviation: Option<f64>,
    pub bit_density_right_max_deviation: Option<f64>,
    pub passband_profile: PassbandProfile,
    pub limiter_peak_ratio_max: Option<f64>,
    pub limiter_limited_events: u64,
    pub limiter_limited_samples: u64,
    pub stability_resets: u64,
    pub state_clamps: u64,
    pub status: String,
    pub hard_failures: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DsdRoundtripBaselineDelta {
    pub candidate_index: usize,
    pub fixture: String,
    pub filter: String,
    pub source_rate: u32,
    pub dsd_rate: String,
    pub wire_rate: u32,
    pub candidate_modulator: String,
    pub standard_baseline_modulator: String,
    pub current_ec2_baseline_modulator: String,
    pub candidate_status: String,
    pub standard_baseline_status: String,
    pub current_ec2_baseline_status: String,
    pub candidate_hard_failure_count: usize,
    pub standard_baseline_hard_failure_count: usize,
    pub current_ec2_baseline_hard_failure_count: usize,
    pub candidate_correlation_worst: Option<f64>,
    pub standard_baseline_correlation_worst: Option<f64>,
    pub current_ec2_baseline_correlation_worst: Option<f64>,
    pub candidate_minus_standard_correlation_worst: Option<f64>,
    pub candidate_minus_current_ec2_correlation_worst: Option<f64>,
    pub candidate_residual_rms_db_worst: Option<f64>,
    pub standard_baseline_residual_rms_db_worst: Option<f64>,
    pub current_ec2_baseline_residual_rms_db_worst: Option<f64>,
    pub candidate_minus_standard_residual_rms_db_worst_db: Option<f64>,
    pub candidate_minus_current_ec2_residual_rms_db_worst_db: Option<f64>,
    pub candidate_inband_residual_rms_dbfs_worst: Option<f64>,
    pub standard_baseline_inband_residual_rms_dbfs_worst: Option<f64>,
    pub current_ec2_baseline_inband_residual_rms_dbfs_worst: Option<f64>,
    pub candidate_minus_standard_inband_residual_rms_dbfs_worst_db: Option<f64>,
    pub candidate_minus_current_ec2_inband_residual_rms_dbfs_worst_db: Option<f64>,
    pub candidate_inband_residual_peak_dbfs_worst: Option<f64>,
    pub standard_baseline_inband_residual_peak_dbfs_worst: Option<f64>,
    pub current_ec2_baseline_inband_residual_peak_dbfs_worst: Option<f64>,
    pub candidate_minus_standard_inband_residual_peak_dbfs_worst_db: Option<f64>,
    pub candidate_minus_current_ec2_inband_residual_peak_dbfs_worst_db: Option<f64>,
    pub candidate_inband_residual_spur_margin_db_worst: Option<f64>,
    pub standard_baseline_inband_residual_spur_margin_db_worst: Option<f64>,
    pub current_ec2_baseline_inband_residual_spur_margin_db_worst: Option<f64>,
    pub candidate_minus_standard_inband_residual_spur_margin_db_worst_db: Option<f64>,
    pub candidate_minus_current_ec2_inband_residual_spur_margin_db_worst_db: Option<f64>,
    pub candidate_decoded_abs_peak: Option<f64>,
    pub standard_baseline_decoded_abs_peak: Option<f64>,
    pub current_ec2_baseline_decoded_abs_peak: Option<f64>,
    pub candidate_minus_standard_decoded_abs_peak: Option<f64>,
    pub candidate_minus_current_ec2_decoded_abs_peak: Option<f64>,
    pub candidate_bit_density_max_deviation: Option<f64>,
    pub standard_baseline_bit_density_max_deviation: Option<f64>,
    pub current_ec2_baseline_bit_density_max_deviation: Option<f64>,
    pub candidate_minus_standard_bit_density_max_deviation: Option<f64>,
    pub candidate_minus_current_ec2_bit_density_max_deviation: Option<f64>,
    pub candidate_render_ms: Option<f64>,
    pub standard_baseline_render_ms: Option<f64>,
    pub current_ec2_baseline_render_ms: Option<f64>,
    pub candidate_minus_standard_render_ms: Option<f64>,
    pub candidate_minus_current_ec2_render_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DsdRoundtripPinkNoiseSeedSummary {
    pub fixture: String,
    pub filter: String,
    pub modulator: String,
    pub source_rate: u32,
    pub dsd_rate: String,
    pub wire_rate: u32,
    pub seed_count: usize,
    pub candidate_indices: String,
    pub status_worst: String,
    pub hard_failure_count_total: usize,
    pub correlation_worst_across_seeds: Option<f64>,
    pub correlation_median_across_seeds: Option<f64>,
    pub residual_rms_db_worst_across_seeds: Option<f64>,
    pub residual_rms_db_median_across_seeds: Option<f64>,
    pub inband_residual_rms_dbfs_worst_across_seeds: Option<f64>,
    pub inband_residual_rms_dbfs_median_across_seeds: Option<f64>,
    pub inband_residual_peak_dbfs_worst_across_seeds: Option<f64>,
    pub inband_residual_peak_dbfs_median_across_seeds: Option<f64>,
    pub inband_residual_spur_margin_db_worst_across_seeds: Option<f64>,
    pub inband_residual_spur_margin_db_median_across_seeds: Option<f64>,
    pub decoded_abs_peak_worst_across_seeds: Option<f64>,
    pub decoded_abs_peak_median_across_seeds: Option<f64>,
    pub bit_density_max_deviation_worst_across_seeds: Option<f64>,
    pub bit_density_max_deviation_median_across_seeds: Option<f64>,
    pub render_ms_worst_across_seeds: Option<f64>,
    pub render_ms_median_across_seeds: Option<f64>,
}

#[derive(Debug, Default)]
struct DsdRoundtripArtifacts {
    waveform: Vec<RoundtripWaveformPoint>,
    spectrum: Vec<RoundtripSpectrumPoint>,
    residual_spectrum: Vec<RoundtripSpectrumPoint>,
    spectrogram: Vec<RoundtripSpectrogramPoint>,
}

#[derive(Debug)]
struct RoundtripWaveformPoint {
    candidate_index: usize,
    fixture: String,
    filter: String,
    modulator: String,
    dsd_rate: String,
    channel: String,
    sample_index: usize,
    time_s: f64,
    reference: f64,
    measured: f64,
    residual: f64,
}

#[derive(Debug)]
struct RoundtripSpectrumPoint {
    candidate_index: usize,
    fixture: String,
    filter: String,
    modulator: String,
    dsd_rate: String,
    channel: String,
    freq_hz: f64,
    reference_dbfs: Option<f64>,
    measured_dbfs: Option<f64>,
    residual_dbfs: Option<f64>,
}

#[derive(Debug)]
struct RoundtripSpectrogramPoint {
    candidate_index: usize,
    fixture: String,
    filter: String,
    modulator: String,
    dsd_rate: String,
    channel: String,
    start_s: f64,
    freq_hz: f64,
    residual_dbfs: f64,
}

#[derive(Debug)]
struct RoundtripChannelAnalysis {
    delay_samples: isize,
    gain: f64,
    correlation: f64,
    residual_relative_db: f64,
    residual_rms_dbfs: f64,
    residual_peak_dbfs: f64,
    residual_spur_margin_db: Option<f64>,
    aligned_reference: Vec<f64>,
    aligned_measured: Vec<f64>,
    residual: Vec<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DsdIdleArtifactRow {
    pub fixture_label: String,
    pub source: String,
    pub channel: String,
    pub idle_peak_freq_hz: Option<f64>,
    pub idle_peak_dbfs: Option<f64>,
    pub density_max_deviation: Option<f64>,
    pub density_window_bits: usize,
    pub is_idle_worst_tone: bool,
    pub is_idle_worst_density: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DsdOverloadRecoveryDiagnosticRow {
    pub source: String,
    pub channel: String,
    pub tail_start_sample: usize,
    pub tail_len_samples: usize,
    pub raw_tail_peak_value: Option<f64>,
    pub raw_tail_peak_dbfs: Option<f64>,
    pub equals_dsd64_min_nonzero_step: bool,
    pub nonzero_tail_samples: usize,
    pub max_nonzero_run_samples: usize,
    pub tail_rms_dbfs: Option<f64>,
    pub fft_peak_hz: Option<f64>,
    pub fft_peak_dbfs: Option<f64>,
    pub reconstructed_tail_peak_dbfs: Option<f64>,
    pub tail_density_max_deviation: Option<f64>,
    pub density_window_bits: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DsdInbandSpurRow {
    pub channel: String,
    pub peak_spur_hz: Option<f64>,
    pub peak_spur_dbfs: Option<f64>,
    pub median_noise_bin_dbfs: Option<f64>,
    pub p95_noise_bin_dbfs: Option<f64>,
    pub p99_noise_bin_dbfs: Option<f64>,
    pub margin_to_median_db: Option<f64>,
    pub margin_to_p95_db: Option<f64>,
    pub margin_to_p99_db: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DsdInbandWindowRow {
    pub channel: String,
    pub start_s: f64,
    pub sinad_db: f64,
    pub noise_rms_dbfs: f64,
    pub peak_spur_hz: Option<f64>,
    pub peak_spur_dbfs: Option<f64>,
    pub noise_20_200_dbfs: Option<f64>,
    pub noise_200_2k_dbfs: Option<f64>,
    pub noise_2k_8k_dbfs: Option<f64>,
    pub noise_8k_16k_dbfs: Option<f64>,
    pub noise_16k_20k_dbfs: Option<f64>,
    pub is_worst: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DsdUltrasonicWindowRow {
    pub channel: String,
    pub start_s: f64,
    pub end_s: f64,
    pub ultrasonic_24_50k_dbfs: Option<f64>,
    pub ultrasonic_50_100k_dbfs: Option<f64>,
    pub ultrasonic_100_200k_dbfs: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default)]
struct DsdArtifactWindowStats {
    bad_window_count: usize,
    bad_window_ratio: Option<f64>,
    artifact_free_worst_sinad_db: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DsdPremodWindowRow {
    pub start_s: f64,
    pub start_sample: usize,
    pub len_samples: usize,
    pub rms_dbfs: f64,
    pub max_abs_dbfs: f64,
    pub dc_dbfs: f64,
    pub crest_db: f64,
    pub slope_rms_dbfs: f64,
    pub tone_amp_dbfs: f64,
    pub tone_phase_rad: f64,
    pub residual_rms_dbfs: f64,
    pub residual_relative_db: f64,
    pub is_dsd_worst_start: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DsdAdaptiveDecisionTrace {
    pub left: Option<AdaptiveDecisionTraceSnapshot>,
    pub right: Option<AdaptiveDecisionTraceSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DsdEc2DecisionTrace {
    pub left: Option<Ec2DecisionTraceSnapshot>,
    pub right: Option<Ec2DecisionTraceSnapshot>,
}

impl DsdEc2DecisionTrace {
    fn from_renderer(renderer: &DsdRenderer) -> Option<Self> {
        let [left, right] = renderer.ec2_decision_traces();
        (left.is_some() || right.is_some()).then_some(Self { left, right })
    }
}

impl DsdAdaptiveDecisionTrace {
    fn from_renderer(renderer: &DsdRenderer) -> Option<Self> {
        let [left, right] = renderer.adaptive_decision_traces();
        (left.is_some() || right.is_some()).then_some(Self { left, right })
    }
}

#[derive(Debug, Serialize)]
struct DsdIdleArtifactExportRow {
    run_label: String,
    candidate_index: usize,
    rank_group: String,
    rank: usize,
    status: String,
    constrained_quality_score: f64,
    filter: String,
    modulator: String,
    path_variant: String,
    source_rate: u32,
    origin_source_rate: u32,
    renderer_source_rate: u32,
    intermediate_rate: Option<u32>,
    intermediate_bits: Option<u32>,
    intermediate_filter: Option<String>,
    dsd_rate: String,
    wire_rate: Option<u32>,
    headroom_db: f64,
    dither_shape: Option<String>,
    dither_scale: Option<f64>,
    dither_prng: Option<String>,
    leak_alpha: Option<f64>,
    lf_floor_gamma: Option<f64>,
    common_side_beta: Option<f64>,
    common_side_common_seed: Option<String>,
    common_side_side_seed: Option<String>,
    pressure_only: Option<bool>,
    quality_pressure: bool,
    quality_pressure_threshold: Option<f64>,
    quality_pressure_hold: Option<f64>,
    seed_left: Option<String>,
    seed_right: Option<String>,
    fixture_label: String,
    source: String,
    channel: String,
    idle_peak_freq_hz: Option<f64>,
    idle_peak_dbfs: Option<f64>,
    density_max_deviation: Option<f64>,
    density_window_bits: usize,
    is_idle_worst_tone: bool,
    is_idle_worst_density: bool,
}

#[derive(Debug, Clone, Copy)]
struct FilterCase {
    name: &'static str,
    filter: FilterType,
    gate: GateClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateClass {
    ReportOnly,
    Minimum16k,
    Extreme,
}

#[derive(Debug, Clone, Copy)]
struct RateCase {
    source: u32,
    target: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct SelectableDsdFilter {
    pub name: &'static str,
    pub filter: FilterType,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct PassbandProfile {
    pub max_deviation_20hz_20khz_db: Option<f64>,
    pub peak_gain_20hz_20khz_db: Option<f64>,
    pub gain_1k_db: Option<f64>,
    pub gain_3k_db: Option<f64>,
    pub gain_6k_db: Option<f64>,
    pub gain_10k_db: Option<f64>,
    pub gain_18k_db: Option<f64>,
}

pub fn run_suite(mode: SuiteMode) -> Result<SuiteReport, String> {
    run_suite_with_scope(mode, SuiteScope::All)
}

pub fn run_selectable_dsd_matrix() -> Result<SuiteReport, String> {
    run_selectable_dsd_matrix_with_config(
        &default_selectable_dsd_filters(),
        &[DsdRate::Dsd64, DsdRate::Dsd128, DsdRate::Dsd256],
        &[DsdModulator::Standard, DsdModulator::EcDepth2],
        DsdExperimentConfig::default(),
    )
}

pub fn default_selectable_dsd_filters() -> [SelectableDsdFilter; 7] {
    [
        SelectableDsdFilter {
            name: "SplitPhase",
            filter: FilterType::Split128k,
        },
        SelectableDsdFilter {
            name: "LinearPhase",
            filter: FilterType::SincExtreme32k,
        },
        SelectableDsdFilter {
            name: "MinimumPhase",
            filter: FilterType::Minimum16k,
        },
        SelectableDsdFilter {
            name: "IntegratedPhase1",
            filter: FilterType::IntegratedPhase128k,
        },
        SelectableDsdFilter {
            name: "IntegratedPhase2",
            filter: FilterType::IntegratedPhase128kV2,
        },
        SelectableDsdFilter {
            name: "IntegratedPhase3",
            filter: FilterType::IntegratedPhase128kV3,
        },
        SelectableDsdFilter {
            name: "IntegratedPhase4",
            filter: FilterType::IntegratedPhase128kV4,
        },
    ]
}

pub fn run_selectable_dsd_matrix_with_config(
    filters: &[SelectableDsdFilter],
    rates: &[DsdRate],
    modulators: &[DsdModulator],
    config: DsdExperimentConfig,
) -> Result<SuiteReport, String> {
    run_selectable_dsd_matrix_with_config_and_source_rates(
        filters,
        rates,
        modulators,
        &[44_100],
        config,
    )
}

pub fn run_selectable_dsd_matrix_with_config_and_source_rates(
    filters: &[SelectableDsdFilter],
    rates: &[DsdRate],
    modulators: &[DsdModulator],
    source_rates: &[u32],
    config: DsdExperimentConfig,
) -> Result<SuiteReport, String> {
    if filters.is_empty() {
        return Err("selectable DSD matrix needs at least one filter".to_string());
    }
    if rates.is_empty() {
        return Err("selectable DSD matrix needs at least one DSD rate".to_string());
    }
    if modulators.is_empty() {
        return Err("selectable DSD matrix needs at least one modulator".to_string());
    }
    if source_rates.is_empty() {
        return Err("selectable DSD matrix needs at least one source rate".to_string());
    }
    if let Some(source_rate) = source_rates
        .iter()
        .copied()
        .find(|rate| !matches!(rate, 44_100 | 48_000))
    {
        return Err(format!(
            "selectable DSD matrix supports only 44100 and 48000 Hz, got {source_rate}"
        ));
    }
    config.validate_for_rates(rates)?;

    let mut dsd = Vec::new();
    for &source_rate in source_rates {
        for case in filters {
            for &rate in rates {
                for &modulator in modulators {
                    eprintln!(
                        "ecbeam2_quality: selectable DSD {} {} {} {}",
                        case.name,
                        modulator.as_name(),
                        source_rate,
                        dsd_rate_name(rate)
                    );
                    dsd.push(measure_dsd_case(
                        case.name,
                        case.filter,
                        rate,
                        source_rate,
                        modulator,
                        true,
                        true,
                        DSD_SECONDS_FULL,
                        config,
                    ));
                }
            }
        }
    }
    annotate_dsd_improvements(&mut dsd, config);

    Ok(SuiteReport {
        mode: "selectable-dsd-matrix".to_string(),
        git_commit: git_commit(),
        pcm: Vec::new(),
        dsd,
    })
}

pub fn run_dsd_roundtrip_quality(
    quick: bool,
    rates: &[DsdRate],
) -> Result<DsdRoundtripReport, String> {
    run_dsd_roundtrip_quality_with_config(quick, rates, DsdExperimentConfig::default())
}

pub fn run_dsd_roundtrip_quality_with_config(
    quick: bool,
    rates: &[DsdRate],
    config: DsdExperimentConfig,
) -> Result<DsdRoundtripReport, String> {
    run_dsd_roundtrip_quality_with_config_and_fixtures(quick, rates, None, config)
}

pub fn run_dsd_roundtrip_quality_with_config_and_fixtures(
    quick: bool,
    rates: &[DsdRate],
    fixture_filter: Option<&[String]>,
    config: DsdExperimentConfig,
) -> Result<DsdRoundtripReport, String> {
    let seconds = if quick {
        DSD_SECONDS_QUICK
    } else {
        DSD_SECONDS_FULL
    };
    if rates.is_empty() {
        return Err("DSD round-trip quality requires at least one rate".to_string());
    }
    let source_rate = 44_100;
    let filter = FilterType::Split128k;
    let filter_name = "Split128k";
    config.validate_for_rates(rates)?;
    let frame_rate = roundtrip_fixture_rate(rates);
    let frames = dsd_measurement_frames(filter, frame_rate, source_rate, seconds);
    let fixtures = roundtrip_fixtures_filtered(frames, source_rate, fixture_filter)?;

    let cases = dsd_roundtrip_cases(rates);
    let mut measurements = Vec::new();
    let mut artifacts = DsdRoundtripArtifacts::default();
    let mut candidate_index = 0usize;
    for fixture in &fixtures {
        for &(dsd_rate, modulator) in &cases {
            eprintln!(
                "ecbeam2_quality: DSD round-trip {filter_name} {} {} {}",
                fixture.label,
                modulator.as_name(),
                dsd_rate_name(dsd_rate)
            );
            let (measurement, case_artifacts) = measure_dsd_roundtrip_case(
                candidate_index,
                fixture,
                filter_name,
                filter,
                dsd_rate,
                source_rate,
                modulator,
                seconds,
                config,
            )?;
            artifacts.waveform.extend(case_artifacts.waveform);
            artifacts.spectrum.extend(case_artifacts.spectrum);
            artifacts
                .residual_spectrum
                .extend(case_artifacts.residual_spectrum);
            artifacts.spectrogram.extend(case_artifacts.spectrogram);
            measurements.push(measurement);
            candidate_index += 1;
        }
    }

    let current_ec2_baselines = if config.has_rate_tweaks() {
        let baseline_config = config.input_gain_only();
        let mut baselines = Vec::new();
        for fixture in &fixtures {
            for &dsd_rate in rates {
                eprintln!(
                    "ecbeam2_quality: DSD round-trip {filter_name} {} {} {} current-baseline",
                    fixture.label,
                    DsdModulator::EcDepth2.as_name(),
                    dsd_rate_name(dsd_rate)
                );
                let (measurement, _) = measure_dsd_roundtrip_case(
                    candidate_index,
                    fixture,
                    filter_name,
                    filter,
                    dsd_rate,
                    source_rate,
                    DsdModulator::EcDepth2,
                    seconds,
                    baseline_config,
                )?;
                baselines.push(measurement);
                candidate_index += 1;
            }
        }
        baselines
    } else {
        Vec::new()
    };
    let baseline_deltas = roundtrip_baseline_deltas(&measurements, &current_ec2_baselines);
    let pink_noise_seed_summaries = roundtrip_pink_noise_seed_summaries(&measurements);

    Ok(DsdRoundtripReport {
        mode: if quick {
            "dsd-roundtrip-quality-quick"
        } else {
            "dsd-roundtrip-quality-full"
        }
        .to_string(),
        git_commit: git_commit(),
        measurements,
        baseline_deltas,
        pink_noise_seed_summaries,
        artifacts,
    })
}

fn roundtrip_fixtures_filtered(
    frames: usize,
    source_rate: u32,
    fixture_filter: Option<&[String]>,
) -> Result<Vec<DsdRoundtripFixture>, String> {
    let fixtures = roundtrip_fixtures(frames, source_rate);
    let Some(filter) = fixture_filter else {
        return Ok(fixtures);
    };
    let mut selected = Vec::with_capacity(filter.len());
    for label in filter {
        let fixture = fixtures
            .iter()
            .find(|fixture| fixture.label == label)
            .ok_or_else(|| format!("unknown round-trip fixture {label}"))?;
        selected.push(fixture.clone());
    }
    Ok(selected)
}

pub fn run_dsd_stability_precheck(
    rates: &[DsdRate],
    config: DsdExperimentConfig,
) -> Result<DsdPrecheckReport, String> {
    if rates.is_empty() {
        return Err("DSD stability precheck requires at least one rate".to_string());
    }
    config.validate_for_rates(rates)?;
    let source_rate = 44_100;
    let filter = FilterType::Split128k;
    let filter_name = "Split128k";
    let modulator = DsdModulator::EcDepth2;
    let frames = (source_rate as f64 * DSD_STABILITY_PRECHECK_SECONDS).round() as usize;
    let probes = stability_precheck_probes(frames, source_rate);

    let mut measurements = Vec::new();
    let mut candidate_index = 0usize;
    for &(probe, ref left, ref right) in &probes {
        for &rate in rates {
            eprintln!(
                "ecbeam2_quality: DSD stability precheck {filter_name} {probe} {} {}",
                modulator.as_name(),
                dsd_rate_name(rate)
            );
            measurements.push(measure_dsd_precheck_case(
                candidate_index,
                probe,
                left,
                right,
                filter_name,
                filter,
                rate,
                source_rate,
                modulator,
                config,
            )?);
            candidate_index += 1;
        }
    }

    Ok(DsdPrecheckReport {
        mode: "dsd-stability-precheck".to_string(),
        git_commit: git_commit(),
        measurements,
    })
}

fn roundtrip_fixture_rate(rates: &[DsdRate]) -> DsdRate {
    rates
        .iter()
        .copied()
        .max_by_key(|rate| rate.oversample())
        .unwrap_or(DsdRate::Dsd256)
}

fn dsd_roundtrip_cases(rates: &[DsdRate]) -> Vec<(DsdRate, DsdModulator)> {
    let mut cases = Vec::with_capacity(rates.len() * 2);
    for &rate in rates {
        cases.push((rate, DsdModulator::EcDepth2));
        cases.push((rate, DsdModulator::Standard));
    }
    cases
}

fn roundtrip_baseline_deltas(
    measurements: &[DsdRoundtripMeasurement],
    current_ec2_baselines: &[DsdRoundtripMeasurement],
) -> Vec<DsdRoundtripBaselineDelta> {
    measurements
        .iter()
        .filter(|measurement| measurement.modulator == "EcDepth2")
        .filter_map(|candidate| {
            let standard = measurements.iter().find(|baseline| {
                baseline.modulator == "Standard" && roundtrip_same_cell(candidate, baseline)
            })?;
            let current_ec2 = current_ec2_baselines
                .iter()
                .find(|baseline| roundtrip_same_cell(candidate, baseline))
                .unwrap_or(candidate);
            Some(roundtrip_baseline_delta_row(
                candidate,
                standard,
                current_ec2,
            ))
        })
        .collect()
}

fn roundtrip_same_cell(left: &DsdRoundtripMeasurement, right: &DsdRoundtripMeasurement) -> bool {
    left.fixture == right.fixture
        && left.filter == right.filter
        && left.source_rate == right.source_rate
        && left.dsd_rate == right.dsd_rate
        && left.wire_rate == right.wire_rate
}

fn roundtrip_baseline_delta_row(
    candidate: &DsdRoundtripMeasurement,
    standard: &DsdRoundtripMeasurement,
    current_ec2: &DsdRoundtripMeasurement,
) -> DsdRoundtripBaselineDelta {
    DsdRoundtripBaselineDelta {
        candidate_index: candidate.candidate_index,
        fixture: candidate.fixture.clone(),
        filter: candidate.filter.clone(),
        source_rate: candidate.source_rate,
        dsd_rate: candidate.dsd_rate.clone(),
        wire_rate: candidate.wire_rate,
        candidate_modulator: candidate.modulator.clone(),
        standard_baseline_modulator: standard.modulator.clone(),
        current_ec2_baseline_modulator: current_ec2.modulator.clone(),
        candidate_status: candidate.status.clone(),
        standard_baseline_status: standard.status.clone(),
        current_ec2_baseline_status: current_ec2.status.clone(),
        candidate_hard_failure_count: candidate.hard_failures.len(),
        standard_baseline_hard_failure_count: standard.hard_failures.len(),
        current_ec2_baseline_hard_failure_count: current_ec2.hard_failures.len(),
        candidate_correlation_worst: candidate.correlation_worst,
        standard_baseline_correlation_worst: standard.correlation_worst,
        current_ec2_baseline_correlation_worst: current_ec2.correlation_worst,
        candidate_minus_standard_correlation_worst: opt_delta(
            candidate.correlation_worst,
            standard.correlation_worst,
        ),
        candidate_minus_current_ec2_correlation_worst: opt_delta(
            candidate.correlation_worst,
            current_ec2.correlation_worst,
        ),
        candidate_residual_rms_db_worst: candidate.residual_rms_db_worst,
        standard_baseline_residual_rms_db_worst: standard.residual_rms_db_worst,
        current_ec2_baseline_residual_rms_db_worst: current_ec2.residual_rms_db_worst,
        candidate_minus_standard_residual_rms_db_worst_db: opt_delta(
            candidate.residual_rms_db_worst,
            standard.residual_rms_db_worst,
        ),
        candidate_minus_current_ec2_residual_rms_db_worst_db: opt_delta(
            candidate.residual_rms_db_worst,
            current_ec2.residual_rms_db_worst,
        ),
        candidate_inband_residual_rms_dbfs_worst: candidate.inband_residual_rms_dbfs_worst,
        standard_baseline_inband_residual_rms_dbfs_worst: standard.inband_residual_rms_dbfs_worst,
        current_ec2_baseline_inband_residual_rms_dbfs_worst: current_ec2
            .inband_residual_rms_dbfs_worst,
        candidate_minus_standard_inband_residual_rms_dbfs_worst_db: opt_delta(
            candidate.inband_residual_rms_dbfs_worst,
            standard.inband_residual_rms_dbfs_worst,
        ),
        candidate_minus_current_ec2_inband_residual_rms_dbfs_worst_db: opt_delta(
            candidate.inband_residual_rms_dbfs_worst,
            current_ec2.inband_residual_rms_dbfs_worst,
        ),
        candidate_inband_residual_peak_dbfs_worst: candidate.inband_residual_peak_dbfs_worst,
        standard_baseline_inband_residual_peak_dbfs_worst: standard.inband_residual_peak_dbfs_worst,
        current_ec2_baseline_inband_residual_peak_dbfs_worst: current_ec2
            .inband_residual_peak_dbfs_worst,
        candidate_minus_standard_inband_residual_peak_dbfs_worst_db: opt_delta(
            candidate.inband_residual_peak_dbfs_worst,
            standard.inband_residual_peak_dbfs_worst,
        ),
        candidate_minus_current_ec2_inband_residual_peak_dbfs_worst_db: opt_delta(
            candidate.inband_residual_peak_dbfs_worst,
            current_ec2.inband_residual_peak_dbfs_worst,
        ),
        candidate_inband_residual_spur_margin_db_worst: candidate
            .inband_residual_spur_margin_db_worst,
        standard_baseline_inband_residual_spur_margin_db_worst: standard
            .inband_residual_spur_margin_db_worst,
        current_ec2_baseline_inband_residual_spur_margin_db_worst: current_ec2
            .inband_residual_spur_margin_db_worst,
        candidate_minus_standard_inband_residual_spur_margin_db_worst_db: opt_delta(
            candidate.inband_residual_spur_margin_db_worst,
            standard.inband_residual_spur_margin_db_worst,
        ),
        candidate_minus_current_ec2_inband_residual_spur_margin_db_worst_db: opt_delta(
            candidate.inband_residual_spur_margin_db_worst,
            current_ec2.inband_residual_spur_margin_db_worst,
        ),
        candidate_decoded_abs_peak: candidate.decoded_abs_peak,
        standard_baseline_decoded_abs_peak: standard.decoded_abs_peak,
        current_ec2_baseline_decoded_abs_peak: current_ec2.decoded_abs_peak,
        candidate_minus_standard_decoded_abs_peak: opt_delta(
            candidate.decoded_abs_peak,
            standard.decoded_abs_peak,
        ),
        candidate_minus_current_ec2_decoded_abs_peak: opt_delta(
            candidate.decoded_abs_peak,
            current_ec2.decoded_abs_peak,
        ),
        candidate_bit_density_max_deviation: candidate.bit_density_max_deviation,
        standard_baseline_bit_density_max_deviation: standard.bit_density_max_deviation,
        current_ec2_baseline_bit_density_max_deviation: current_ec2.bit_density_max_deviation,
        candidate_minus_standard_bit_density_max_deviation: opt_delta(
            candidate.bit_density_max_deviation,
            standard.bit_density_max_deviation,
        ),
        candidate_minus_current_ec2_bit_density_max_deviation: opt_delta(
            candidate.bit_density_max_deviation,
            current_ec2.bit_density_max_deviation,
        ),
        candidate_render_ms: candidate.render_ms,
        standard_baseline_render_ms: standard.render_ms,
        current_ec2_baseline_render_ms: current_ec2.render_ms,
        candidate_minus_standard_render_ms: opt_delta(candidate.render_ms, standard.render_ms),
        candidate_minus_current_ec2_render_ms: opt_delta(
            candidate.render_ms,
            current_ec2.render_ms,
        ),
    }
}

fn roundtrip_pink_noise_seed_summaries(
    measurements: &[DsdRoundtripMeasurement],
) -> Vec<DsdRoundtripPinkNoiseSeedSummary> {
    let mut groups: BTreeMap<(String, String, u32, String, u32), Vec<&DsdRoundtripMeasurement>> =
        BTreeMap::new();
    for measurement in measurements {
        if roundtrip_is_pink_noise_seed(&measurement.fixture) {
            groups
                .entry((
                    measurement.filter.clone(),
                    measurement.modulator.clone(),
                    measurement.source_rate,
                    measurement.dsd_rate.clone(),
                    measurement.wire_rate,
                ))
                .or_default()
                .push(measurement);
        }
    }

    groups
        .into_iter()
        .map(
            |((filter, modulator, source_rate, dsd_rate, wire_rate), mut group)| {
                group.sort_by(|left, right| {
                    left.fixture
                        .cmp(&right.fixture)
                        .then_with(|| left.candidate_index.cmp(&right.candidate_index))
                });
                let candidate_indices = group
                    .iter()
                    .map(|measurement| measurement.candidate_index.to_string())
                    .collect::<Vec<_>>()
                    .join(";");
                let hard_failure_count_total = group
                    .iter()
                    .map(|measurement| measurement.hard_failures.len())
                    .sum();
                let status_worst =
                    if group.iter().any(|measurement| measurement.status == "fail") {
                        "fail"
                    } else {
                        "pass"
                    }
                    .to_string();

                DsdRoundtripPinkNoiseSeedSummary {
                    fixture: "pink_noise".to_string(),
                    filter,
                    modulator,
                    source_rate,
                    dsd_rate,
                    wire_rate,
                    seed_count: group.len(),
                    candidate_indices,
                    status_worst,
                    hard_failure_count_total,
                    correlation_worst_across_seeds: opt_min_by(&group, |measurement| {
                        measurement.correlation_worst
                    }),
                    correlation_median_across_seeds: opt_median_by(&group, |measurement| {
                        measurement.correlation_worst
                    }),
                    residual_rms_db_worst_across_seeds: opt_max_by(&group, |measurement| {
                        measurement.residual_rms_db_worst
                    }),
                    residual_rms_db_median_across_seeds: opt_median_by(&group, |measurement| {
                        measurement.residual_rms_db_worst
                    }),
                    inband_residual_rms_dbfs_worst_across_seeds: opt_max_by(
                        &group,
                        |measurement| measurement.inband_residual_rms_dbfs_worst,
                    ),
                    inband_residual_rms_dbfs_median_across_seeds: opt_median_by(
                        &group,
                        |measurement| measurement.inband_residual_rms_dbfs_worst,
                    ),
                    inband_residual_peak_dbfs_worst_across_seeds: opt_max_by(
                        &group,
                        |measurement| measurement.inband_residual_peak_dbfs_worst,
                    ),
                    inband_residual_peak_dbfs_median_across_seeds: opt_median_by(
                        &group,
                        |measurement| measurement.inband_residual_peak_dbfs_worst,
                    ),
                    inband_residual_spur_margin_db_worst_across_seeds: opt_min_by(
                        &group,
                        |measurement| measurement.inband_residual_spur_margin_db_worst,
                    ),
                    inband_residual_spur_margin_db_median_across_seeds: opt_median_by(
                        &group,
                        |measurement| measurement.inband_residual_spur_margin_db_worst,
                    ),
                    decoded_abs_peak_worst_across_seeds: opt_max_by(&group, |measurement| {
                        measurement.decoded_abs_peak
                    }),
                    decoded_abs_peak_median_across_seeds: opt_median_by(&group, |measurement| {
                        measurement.decoded_abs_peak
                    }),
                    bit_density_max_deviation_worst_across_seeds: opt_max_by(
                        &group,
                        |measurement| measurement.bit_density_max_deviation,
                    ),
                    bit_density_max_deviation_median_across_seeds: opt_median_by(
                        &group,
                        |measurement| measurement.bit_density_max_deviation,
                    ),
                    render_ms_worst_across_seeds: opt_max_by(&group, |measurement| {
                        measurement.render_ms
                    }),
                    render_ms_median_across_seeds: opt_median_by(&group, |measurement| {
                        measurement.render_ms
                    }),
                }
            },
        )
        .collect()
}

fn roundtrip_is_pink_noise_seed(fixture: &str) -> bool {
    fixture.starts_with("pink_noise_seed")
}

fn opt_min_by<T, F>(items: &[T], mut value: F) -> Option<f64>
where
    F: FnMut(&T) -> Option<f64>,
{
    items
        .iter()
        .filter_map(|item| value(item).filter(|value| value.is_finite()))
        .min_by(|left, right| left.total_cmp(right))
}

fn opt_max_by<T, F>(items: &[T], mut value: F) -> Option<f64>
where
    F: FnMut(&T) -> Option<f64>,
{
    items
        .iter()
        .filter_map(|item| value(item).filter(|value| value.is_finite()))
        .max_by(|left, right| left.total_cmp(right))
}

fn opt_median_by<T, F>(items: &[T], mut value: F) -> Option<f64>
where
    F: FnMut(&T) -> Option<f64>,
{
    let mut values: Vec<f64> = items
        .iter()
        .filter_map(|item| value(item).filter(|value| value.is_finite()))
        .collect();
    if values.is_empty() {
        return None;
    }
    values.sort_by(|left, right| left.total_cmp(right));
    Some(values[values.len() / 2])
}

fn opt_delta(candidate: Option<f64>, baseline: Option<f64>) -> Option<f64> {
    match (candidate, baseline) {
        (Some(candidate), Some(baseline)) if candidate.is_finite() && baseline.is_finite() => {
            Some(candidate - baseline)
        }
        _ => None,
    }
}

pub fn print_roundtrip_report(report: &DsdRoundtripReport) {
    println!("DSD round-trip quality report ({})", report.mode);
    if let Some(commit) = &report.git_commit {
        println!("commit: {commit}");
    }
    println!();
    println!(
        "{:<18} {:<9} {:<9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>8}",
        "fixture",
        "rate",
        "mod",
        "corr",
        "resid",
        "rmsFS",
        "peakFS",
        "spur",
        "abs pk",
        "density",
        "status"
    );
    for m in &report.measurements {
        println!(
            "{:<18} {:<9} {:<9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>8}",
            m.fixture,
            m.dsd_rate,
            m.modulator,
            fmt_opt(m.correlation_worst),
            fmt_opt(m.residual_rms_db_worst),
            fmt_opt(m.inband_residual_rms_dbfs_worst),
            fmt_opt(m.inband_residual_peak_dbfs_worst),
            fmt_opt(m.inband_residual_spur_margin_db_worst),
            fmt_opt(m.decoded_abs_peak),
            fmt_opt(m.bit_density_max_deviation),
            m.status,
        );
        for note in &m.notes {
            println!("  note: {note}");
        }
        for failure in &m.hard_failures {
            println!("  failure: {failure}");
        }
    }
}

pub fn print_precheck_report(report: &DsdPrecheckReport) {
    println!("DSD stability precheck report ({})", report.mode);
    if let Some(commit) = &report.git_commit {
        println!("commit: {commit}");
    }
    println!();
    println!(
        "{:<20} {:<9} {:<9} {:>9} {:>9} {:>9} {:>9} {:>8}",
        "probe", "rate", "mod", "abs pk", "density", "limit", "health", "status"
    );
    for m in &report.measurements {
        println!(
            "{:<20} {:<9} {:<9} {:>9} {:>9} {:>9} {:>9} {:>8}",
            m.probe,
            m.dsd_rate,
            m.modulator,
            fmt_opt(m.decoded_abs_peak),
            fmt_opt(m.bit_density_max_deviation),
            m.limiter_limited_events + m.limiter_limited_samples,
            m.stability_resets + m.state_clamps,
            m.status,
        );
        for note in &m.notes {
            println!("  note: {note}");
        }
        for failure in &m.hard_failures {
            println!("  failure: {failure}");
        }
    }
}

pub fn roundtrip_gate_failures(report: &DsdRoundtripReport) -> Vec<String> {
    report
        .measurements
        .iter()
        .flat_map(|measurement| {
            measurement.hard_failures.iter().map(|failure| {
                format!(
                    "{} {} {}: {failure}",
                    measurement.filter, measurement.modulator, measurement.dsd_rate
                )
            })
        })
        .collect()
}

pub fn precheck_gate_failures(report: &DsdPrecheckReport) -> Vec<String> {
    report
        .measurements
        .iter()
        .flat_map(|measurement| {
            measurement.hard_failures.iter().map(|failure| {
                format!(
                    "{} {} {} {}: {failure}",
                    measurement.filter,
                    measurement.probe,
                    measurement.modulator,
                    measurement.dsd_rate
                )
            })
        })
        .collect()
}

pub fn dsd_measurement_trust_battery_failures() -> Vec<String> {
    let mut failures = Vec::new();
    let reference = synthetic_trust_signal(-80.0, None, false);
    let with_ultrasonic = synthetic_trust_signal(-80.0, None, true);
    let spur = synthetic_trust_signal(-96.0, Some((7_000.0, -72.0)), false);

    let Some(reference_metrics) = trust_stereo_metrics(&reference) else {
        return vec!["reference synthetic metric did not produce measurements".to_string()];
    };
    let Some(ultrasonic_metrics) = trust_stereo_metrics(&with_ultrasonic) else {
        return vec!["ultrasonic synthetic metric did not produce measurements".to_string()];
    };
    let Some(spur_metrics) = trust_stereo_metrics(&spur) else {
        return vec!["spur synthetic metric did not produce measurements".to_string()];
    };

    check_within(
        &mut failures,
        "known in-band noise tone SINAD",
        reference_metrics.aggregate.sinad_db,
        80.0,
        0.5,
        "dB",
    );
    check_within(
        &mut failures,
        "known in-band noise tone RMS",
        reference_metrics.aggregate.noise_rms_dbfs,
        db(trust_spur_amp(-80.0) / 2.0f64.sqrt()),
        0.5,
        "dBFS",
    );

    for (name, clean, ultrasonic) in [
        (
            "ultrasonic-only noise SINAD shift",
            reference_metrics.aggregate.sinad_db,
            ultrasonic_metrics.aggregate.sinad_db,
        ),
        (
            "ultrasonic-only noise RMS shift",
            reference_metrics.aggregate.noise_rms_dbfs,
            ultrasonic_metrics.aggregate.noise_rms_dbfs,
        ),
        (
            "ultrasonic-only noise worst-window shift",
            reference_metrics.aggregate.sinad_worst_db,
            ultrasonic_metrics.aggregate.sinad_worst_db,
        ),
    ] {
        let shift = (ultrasonic - clean).abs();
        if shift > 0.1 {
            failures.push(format!("{name} moved {shift:.3} dB > 0.100 dB"));
        }
    }

    check_opt_within(
        &mut failures,
        "known narrow spur frequency",
        spur_metrics.aggregate.peak_spur_hz,
        7_000.0,
        trust_bin_hz(),
        "Hz",
    );
    check_opt_within(
        &mut failures,
        "known narrow spur level",
        spur_metrics.aggregate.peak_spur_dbfs,
        db(trust_spur_amp(-72.0) / 2.0f64.sqrt()),
        0.5,
        "dBFS",
    );

    let Some(repeated_metrics) = trust_stereo_metrics(&reference) else {
        failures.push("repeat synthetic metric did not produce measurements".to_string());
        return failures;
    };
    if trust_metric_fingerprint(&reference_metrics) != trust_metric_fingerprint(&repeated_metrics) {
        failures.push("repeat synthetic metric was not bit-identical".to_string());
    }

    failures
}

pub fn run_suite_with_scope(mode: SuiteMode, scope: SuiteScope) -> Result<SuiteReport, String> {
    run_suite_with_scope_and_config(mode, scope, DsdExperimentConfig::default())
}

pub fn run_suite_with_scope_and_config(
    mode: SuiteMode,
    scope: SuiteScope,
    config: DsdExperimentConfig,
) -> Result<SuiteReport, String> {
    config.validate()?;
    let pcm = if matches!(
        scope,
        SuiteScope::DsdOnly | SuiteScope::EcDepthOnly | SuiteScope::Ec3Tuning
    ) {
        Vec::new()
    } else {
        run_pcm_measurements(mode)
    };
    let mut dsd = run_dsd_measurements(mode, scope, config);
    annotate_dsd_improvements(&mut dsd, config);

    let mut report_mode = match mode {
        SuiteMode::Quick => "quick",
        SuiteMode::Full => "full",
    }
    .to_string();
    if let Some(label) = config.label() {
        report_mode.push(' ');
        report_mode.push_str(&label);
    }

    Ok(SuiteReport {
        mode: report_mode,
        git_commit: git_commit(),
        pcm,
        dsd,
    })
}

pub fn run_src_sweep(mode: SuiteMode) -> Result<SuiteReport, String> {
    Ok(SuiteReport {
        mode: match mode {
            SuiteMode::Quick => "src-sweep-quick",
            SuiteMode::Full => "src-sweep-full",
        }
        .to_string(),
        git_commit: git_commit(),
        pcm: run_src_sweep_measurements(mode),
        dsd: Vec::new(),
    })
}

pub fn print_console_report(report: &SuiteReport) {
    println!("Audio DSP quality report ({})", report.mode);
    if let Some(commit) = &report.git_commit {
        println!("commit: {commit}");
    }
    println!();
    println!(
        "{:<18} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>10} {:>10}",
        "PCM filter",
        "src",
        "dst",
        "pb dev",
        "pb peak",
        "1k dB",
        "3k dB",
        "6k dB",
        "10k dB",
        "18k dB",
        "image dB",
        "core %"
    );
    for m in &report.pcm {
        println!(
            "{:<18} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>10} {:>10}",
            m.filter,
            m.source_rate,
            m.target_rate,
            fmt_opt(m.passband_profile.max_deviation_20hz_20khz_db),
            fmt_opt(m.passband_profile.peak_gain_20hz_20khz_db),
            fmt_opt(m.passband_profile.gain_1k_db),
            fmt_opt(m.passband_profile.gain_3k_db),
            fmt_opt(m.passband_profile.gain_6k_db),
            fmt_opt(m.passband_profile.gain_10k_db),
            fmt_opt(m.passband_profile.gain_18k_db),
            fmt_opt(m.image_rejection_db),
            fmt_opt(m.one_core_percent),
        );
        for note in &m.notes {
            println!("  note: {note}");
        }
    }

    println!();
    println!(
        "{:<18} {:<9} {:>9} {:>8} {:>9} {:>8} {:>8} {:>9} {:>9} {:>9} {:>7} {:>5} {:>10} {:>9} {:>9} {:>9} {:>8} {:>8} {:>8}",
        "Filter",
        "mod",
        "src",
        "rate",
        "pb dev",
        "pb pk",
        "18k",
        "med dB",
        "worst",
        "spread",
        "stereo",
        "win",
        "idle dB",
        "noise pk",
        "spur",
        "HF res",
        "dens",
        "clamps",
        "resets"
    );
    for m in &report.dsd {
        println!(
            "{:<18} {:<9} {:>9} {:>8} {:>9} {:>8} {:>8} {:>9} {:>9} {:>9} {:>7} {:>5} {:>10} {:>9} {:>9} {:>9} {:>8} {:>8} {:>8}",
            m.filter,
            m.modulator,
            m.source_rate,
            m.dsd_rate,
            fmt_opt(m.passband_profile.max_deviation_20hz_20khz_db),
            fmt_opt(m.passband_profile.peak_gain_20hz_20khz_db),
            fmt_opt(m.passband_profile.gain_18k_db),
            fmt_opt(m.inband_snr_db),
            fmt_opt(m.inband_snr_worst_db),
            fmt_opt(m.inband_snr_spread_db),
            fmt_opt(m.stereo_snr_worst_mismatch_db),
            m.inband_snr_window_count
                .map(|count| count.to_string())
                .unwrap_or_else(|| "n/a".to_string()),
            fmt_opt(m.idle_worst_tone_dbfs.or(m.idle_tone_dbfs)),
            fmt_opt(m.inband_noise_peak_dbfs),
            fmt_opt(m.inband_noise_spur_margin_db),
            fmt_opt(m.high_freq_worst_residual_db),
            fmt_opt(m.bit_density_max_deviation),
            m.state_clamps,
            m.stability_resets
        );
        for note in &m.notes {
            println!("  note: {note}");
        }
    }
}

pub fn gate_failures(report: &SuiteReport) -> Vec<String> {
    let mut failures = Vec::new();
    for pcm in &report.pcm {
        let Some(class) = gate_class_for_filter(&pcm.filter) else {
            continue;
        };
        if class == GateClass::ReportOnly {
            continue;
        }
        // Some experimental/high-latency filters intentionally skip bounded-window
        // steady-state tone metrics. Keep those rows visible in reports without
        // turning unavailable data into a full-suite failure.
        if class == GateClass::Extreme && (pcm.gain_1k_db.is_none() || pcm.dc_gain_db.is_none()) {
            continue;
        }
        check_abs(
            &mut failures,
            pcm.gain_1k_db,
            0.02,
            format!(
                "{} {}->{} 1 kHz gain",
                pcm.filter, pcm.source_rate, pcm.target_rate
            ),
        );
        check_abs(
            &mut failures,
            pcm.dc_gain_db,
            0.02,
            format!(
                "{} {}->{} DC gain",
                pcm.filter, pcm.source_rate, pcm.target_rate
            ),
        );
        check_max(
            &mut failures,
            pcm.passband_profile.peak_gain_20hz_20khz_db,
            0.02,
            format!(
                "{} {}->{} 20 Hz-20 kHz peak passband gain",
                pcm.filter, pcm.source_rate, pcm.target_rate
            ),
        );
        match class {
            GateClass::Minimum16k => {
                check_min(
                    &mut failures,
                    pcm.gain_18k_db,
                    -1.0,
                    format!(
                        "{} {}->{} 18 kHz gain",
                        pcm.filter, pcm.source_rate, pcm.target_rate
                    ),
                );
                check_min(
                    &mut failures,
                    pcm.image_rejection_db,
                    95.0,
                    format!(
                        "{} {}->{} image rejection",
                        pcm.filter, pcm.source_rate, pcm.target_rate
                    ),
                );
            }
            GateClass::Extreme => {
                if let Some(gain) = pcm.gain_18k_db {
                    check_min(
                        &mut failures,
                        Some(gain),
                        -0.2,
                        format!(
                            "{} {}->{} 18 kHz gain",
                            pcm.filter, pcm.source_rate, pcm.target_rate
                        ),
                    );
                }
                if let Some(rejection) = pcm.image_rejection_db {
                    check_min(
                        &mut failures,
                        Some(rejection),
                        110.0,
                        format!(
                            "{} {}->{} image rejection",
                            pcm.filter, pcm.source_rate, pcm.target_rate
                        ),
                    );
                }
            }
            GateClass::ReportOnly => {}
        }
    }

    if CALIBRATED {
        for dsd in &report.dsd {
            if dsd.stability_resets != 0 {
                failures.push(format!(
                    "{} {} {} {} had {} stability resets",
                    dsd.filter, dsd.modulator, dsd.source_rate, dsd.dsd_rate, dsd.stability_resets
                ));
            }
            if dsd.stress_stability_resets != 0 {
                failures.push(format!(
                    "{} {} {} {} stress pass had {} stability resets",
                    dsd.filter,
                    dsd.modulator,
                    dsd.source_rate,
                    dsd.dsd_rate,
                    dsd.stress_stability_resets
                ));
            }
            check_max(
                &mut failures,
                dsd.passband_profile.peak_gain_20hz_20khz_db,
                0.02,
                format!(
                    "{} {} {} {} 20 Hz-20 kHz peak passband gain",
                    dsd.filter, dsd.modulator, dsd.source_rate, dsd.dsd_rate
                ),
            );
            let idle_tone_required = matches!(dsd.filter.as_str(), "Minimum16k" | "Split128k");
            if dsd.modulator.starts_with("EcDepth")
                && (idle_tone_required || dsd.idle_tone_dbfs.is_some())
            {
                check_max(
                    &mut failures,
                    dsd.idle_tone_dbfs,
                    -85.0,
                    format!(
                        "{} {} {} {} idle tone",
                        dsd.filter, dsd.modulator, dsd.source_rate, dsd.dsd_rate
                    ),
                );
            }
            if !report.mode.starts_with("quick") {
                check_dsd_noise_floor(&mut failures, dsd);
                check_ec1_artifact_regression(&mut failures, dsd);
                check_dsd128_ec2_artifact_regression(&mut failures, dsd);
                check_dsd128_ec4a_target_bands(&mut failures, dsd);
            }
        }
        if !report.mode.starts_with("dsd128-ec4a-target-only") {
            check_dsd128_ec4a_pressure_only_reference(report, &mut failures);
        }
    }

    failures
}

pub fn run_dsd_noise_floor_gate_cases() -> Vec<DsdMeasurement> {
    if !CALIBRATED {
        return Vec::new();
    }
    [
        (
            "Split128k",
            FilterType::Split128k,
            DsdRate::Dsd128,
            DsdModulator::Standard,
        ),
        (
            "Split128k",
            FilterType::Split128k,
            DsdRate::Dsd128,
            DsdModulator::EcDepth1,
        ),
        (
            "Split128k",
            FilterType::Split128k,
            DsdRate::Dsd128,
            DsdModulator::EcDepth2,
        ),
        (
            "Split128k",
            FilterType::Split128k,
            DsdRate::Dsd256,
            DsdModulator::Standard,
        ),
        (
            "Split128k",
            FilterType::Split128k,
            DsdRate::Dsd256,
            DsdModulator::EcDepth1,
        ),
        (
            "Split128k",
            FilterType::Split128k,
            DsdRate::Dsd256,
            DsdModulator::EcDepth2,
        ),
        (
            "SincExtreme32k",
            FilterType::SincExtreme32k,
            DsdRate::Dsd256,
            DsdModulator::EcDepth1,
        ),
        (
            "SincExtreme32k",
            FilterType::SincExtreme32k,
            DsdRate::Dsd256,
            DsdModulator::EcDepth2,
        ),
    ]
    .into_iter()
    .map(|(name, filter, dsd_rate, modulator)| {
        measure_dsd_case(
            name,
            filter,
            dsd_rate,
            44_100,
            modulator,
            false,
            false,
            DSD_SECONDS_FULL,
            DsdExperimentConfig::default(),
        )
    })
    .collect()
}

pub fn dsd_noise_floor_gate_failures(measurements: &[DsdMeasurement]) -> Vec<String> {
    let mut failures = Vec::new();
    for measurement in measurements {
        check_dsd_noise_floor(&mut failures, measurement);
    }
    failures
}

pub fn run_ec4a_dsd256_crackle_torture(
    seconds: f64,
    trace_window_bits: Option<usize>,
) -> Result<Ec4aCrackleMeasurement, String> {
    if !CALIBRATED {
        return Err("DSD coefficient table is not calibrated".to_string());
    }

    let source_rate = 44_100;
    let dsd_rate = DsdRate::Dsd256;
    let modulator = DsdModulator::EcDepth4Adaptive;
    let Some(wire_rate) = dsd_rate.wire_rate_for_source(source_rate) else {
        return Err("unsupported DSD256 source/wire-rate combination".to_string());
    };
    let bytes_per_decoded_frame = (wire_rate / source_rate / 8) as usize;
    let frames = (source_rate as f64 * seconds).round() as usize;
    let (left, right) = ec4a_crackle_torture_fixture(frames, source_rate);
    let experiment_tweaks = DsdExperimentTweaks {
        ec4a_decision_trace_window_bits: trace_window_bits,
        ..DsdExperimentTweaks::default()
    };
    let mut renderer = DsdRenderer::new_with_dsd_modulator_and_experiment_tweaks(
        FilterType::Split128k,
        source_rate,
        dsd_rate,
        modulator,
        None,
        experiment_tweaks,
    )
    .map_err(|err| err.to_string())?;
    renderer.set_native_order(NativeDsdOrder::MsbFirst);

    let mut out_l = Vec::new();
    let mut out_r = Vec::new();
    let mut decoded_l = Vec::with_capacity(frames + source_rate as usize);
    let mut decoded_r = Vec::with_capacity(frames + source_rate as usize);
    let mut decimator_l = NativeByteDecimator::new(bytes_per_decoded_frame);
    let mut decimator_r = NativeByteDecimator::new(bytes_per_decoded_frame);

    for start in (0..left.len()).step_by(CHUNK_FRAMES) {
        let end = (start + CHUNK_FRAMES).min(left.len());
        renderer.upsample(&left[start..end], &right[start..end]);
        out_l.clear();
        out_r.clear();
        renderer.modulate_and_pack_native(1.0, &mut out_l, &mut out_r);
        decimator_l.push(&out_l, &mut decoded_l);
        decimator_r.push(&out_r, &mut decoded_r);
    }
    out_l.clear();
    out_r.clear();
    renderer.flush_modulators_and_pack_native(&mut out_l, &mut out_r);
    decimator_l.push(&out_l, &mut decoded_l);
    decimator_r.push(&out_r, &mut decoded_r);
    out_l.clear();
    out_r.clear();
    renderer.flush_native_with_idle(&mut out_l, &mut out_r);
    decimator_l.push(&out_l, &mut decoded_l);
    decimator_r.push(&out_r, &mut decoded_r);

    let left_clicks = crackle_stats(&decoded_l, source_rate);
    let right_clicks = crackle_stats(&decoded_r, source_rate);
    let telemetry = renderer.adaptive_telemetry();
    let mut notes = Vec::new();
    if decimator_l.pending_len() != 0 || decimator_r.pending_len() != 0 {
        notes.push(format!(
            "native DSD byte decimator ended with pending bytes L={} R={}",
            decimator_l.pending_len(),
            decimator_r.pending_len()
        ));
    }
    let decoded_abs_peak = sample_abs_peak(&decoded_l)
        .into_iter()
        .chain(sample_abs_peak(&decoded_r))
        .reduce(f64::max);

    Ok(Ec4aCrackleMeasurement {
        filter: "Split128k".to_string(),
        source_rate,
        dsd_rate: dsd_rate_name(dsd_rate).to_string(),
        modulator: modulator.as_name().to_string(),
        seconds,
        decoded_frames: decoded_l.len().min(decoded_r.len()),
        left_click_candidates: left_clicks.candidates,
        right_click_candidates: right_clicks.candidates,
        left_max_click_score: left_clicks.max_score,
        right_max_click_score: right_clicks.max_score,
        left_max_click_residual: left_clicks.max_residual,
        right_max_click_residual: right_clicks.max_residual,
        decoded_abs_peak,
        stability_resets: renderer.stability_resets(),
        state_clamps: renderer.state_clamps(),
        total_commits: telemetry.total_commits,
        depth4_commits: telemetry.depth4_commits,
        depth4_ratio: telemetry.depth4_ratio(),
        trigger_guard_selected: telemetry.trigger_guard_selected,
        trigger_pressure_selected: telemetry.trigger_pressure_selected,
        trigger_transient_selected: telemetry.trigger_transient_selected,
        trigger_ambiguity_selected: telemetry.trigger_ambiguity_selected,
        budget_starved: telemetry.budget_starved,
        max_hold_seen: telemetry.max_hold_seen,
        adaptive_decision_trace: DsdAdaptiveDecisionTrace::from_renderer(&renderer),
        notes,
    })
}

pub fn print_crackle_report(m: &Ec4aCrackleMeasurement) {
    println!(
        "EC-4A DSD256 crackle torture: {} {} {} {} {:.1}s",
        m.filter, m.modulator, m.source_rate, m.dsd_rate, m.seconds
    );
    println!(
        "decoded={} abs_peak={} clicks L/R={}/{} max_score L/R={:.2}/{:.2} max_residual L/R={:.4}/{:.4}",
        m.decoded_frames,
        fmt_opt(m.decoded_abs_peak),
        m.left_click_candidates,
        m.right_click_candidates,
        m.left_max_click_score,
        m.right_max_click_score,
        m.left_max_click_residual,
        m.right_max_click_residual
    );
    println!(
        "health resets={} clamps={} depth4={}/{} ({:.4}%) triggers g/p/t/a={}/{}/{}/{} starved={} hold={}",
        m.stability_resets,
        m.state_clamps,
        m.depth4_commits,
        m.total_commits,
        m.depth4_ratio * 100.0,
        m.trigger_guard_selected,
        m.trigger_pressure_selected,
        m.trigger_transient_selected,
        m.trigger_ambiguity_selected,
        m.budget_starved,
        m.max_hold_seen
    );
    for note in &m.notes {
        println!("  note: {note}");
    }
}

pub fn ec4a_crackle_gate_failures(m: &Ec4aCrackleMeasurement) -> Vec<String> {
    let mut failures = Vec::new();
    let expected_min_frames = (m.source_rate as f64
        * (m.seconds - CRACKLE_ANALYSIS_SETTLE_SECONDS).max(0.0))
    .round() as usize;
    if m.decoded_frames < expected_min_frames {
        failures.push(format!(
            "decoded frame count {} is below expected minimum {}",
            m.decoded_frames, expected_min_frames
        ));
    }
    if m.left_click_candidates != 0 || m.right_click_candidates != 0 {
        failures.push(format!(
            "isolated click candidates detected L/R={}/{} (max scores {:.2}/{:.2})",
            m.left_click_candidates,
            m.right_click_candidates,
            m.left_max_click_score,
            m.right_max_click_score
        ));
    }
    check_max(
        &mut failures,
        m.decoded_abs_peak,
        1.05,
        "EC-4A crackle torture decoded abs peak".to_string(),
    );
    if m.stability_resets != 0 {
        failures.push(format!(
            "EC-4A crackle torture had {} stability resets",
            m.stability_resets
        ));
    }
    if m.state_clamps != 0 {
        failures.push(format!(
            "EC-4A crackle torture had {} state clamps",
            m.state_clamps
        ));
    }
    if m.depth4_commits == 0 {
        failures.push("EC-4A crackle torture did not exercise depth-4 commits".to_string());
    }
    failures
}

pub fn write_crackle_artifacts(
    measurement: &Ec4aCrackleMeasurement,
    out_dir: &Path,
) -> Result<(), String> {
    fs::create_dir_all(out_dir).map_err(|err| err.to_string())?;
    let json = serde_json::to_string_pretty(measurement).map_err(|err| err.to_string())?;
    fs::write(out_dir.join("ec4a_crackle_torture.json"), json).map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("ec4a_crackle_torture.csv"),
        crackle_metrics_csv(measurement),
    )
    .map_err(|err| err.to_string())?;
    if measurement.adaptive_decision_trace.is_some() {
        fs::write(
            out_dir.join("ec4a_decision_trace.csv"),
            crackle_decision_trace_csv(measurement),
        )
        .map_err(|err| err.to_string())?;
    }
    Ok(())
}

pub fn write_report_artifacts(report: &SuiteReport, out_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(out_dir).map_err(|err| err.to_string())?;
    let run_label = out_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(report.mode.as_str());
    let json = versioned_json(report).map_err(|err| err.to_string())?;
    fs::write(out_dir.join("summary.json"), json).map_err(|err| err.to_string())?;
    let decision_summary = dsd_decision_summary(report);
    let decision_json = versioned_json(&decision_summary).map_err(|err| err.to_string())?;
    fs::write(out_dir.join("decision_summary.json"), decision_json)
        .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("decision_summary.md"),
        dsd_decision_summary_markdown(&decision_summary),
    )
    .map_err(|err| err.to_string())?;
    fs::write(out_dir.join("metrics.csv"), metrics_csv(report)).map_err(|err| err.to_string())?;
    fs::write(out_dir.join("dsd_rankings.csv"), dsd_rankings_csv(report))
        .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("dsd_modulator_comparison.csv"),
        dsd_modulator_comparison_csv(report),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("dsd_modulator_comparison.md"),
        dsd_modulator_comparison_markdown(report),
    )
    .map_err(|err| err.to_string())?;
    if report_has_dsd128_ec4a_candidates(report) {
        fs::write(
            out_dir.join("dsd128_ec4a_candidates.csv"),
            dsd128_ec4a_candidates_csv(report),
        )
        .map_err(|err| err.to_string())?;
        fs::write(
            out_dir.join("dsd128_ec4a_candidates.md"),
            dsd128_ec4a_candidates_markdown(report),
        )
        .map_err(|err| err.to_string())?;
    }
    let idle_artifacts = dsd_idle_artifact_export_rows(report, run_label);
    let idle_json = serde_json::to_string_pretty(&idle_artifacts).map_err(|err| err.to_string())?;
    fs::write(out_dir.join("dsd_idle_artifacts.json"), idle_json).map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("dsd_idle_artifacts.csv"),
        dsd_idle_artifacts_csv(&idle_artifacts),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("dsd_overload_recovery_diagnostics.csv"),
        dsd_overload_recovery_diagnostics_csv(report, run_label),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("dsd_inband_spurs.csv"),
        dsd_inband_spurs_csv(report, run_label),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("dsd_inband_windows.csv"),
        dsd_inband_windows_csv(report, run_label),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("dsd_ultrasonic_windows.csv"),
        dsd_ultrasonic_windows_csv(report, run_label),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("dsd_premod_windows.csv"),
        dsd_premod_windows_csv(report, run_label),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("path_comparison.csv"),
        path_comparison_csv(report),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("path_comparison.md"),
        path_comparison_markdown(report),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("ec4a_decision_trace.csv"),
        ec4a_decision_trace_csv(report),
    )
    .map_err(|err| err.to_string())?;
    let ec2_trace_csv = ec2_decision_trace_csv(report);
    fs::write(out_dir.join("ec2_decision_trace.csv"), &ec2_trace_csv)
        .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("ec2_window_summary.csv"),
        ec2_window_summary_csv(report),
    )
    .map_err(|err| err.to_string())?;
    fs::write(out_dir.join("pcm_stopband.svg"), pcm_svg(report)).map_err(|err| err.to_string())?;
    fs::write(out_dir.join("dsd_residual.svg"), dsd_svg(report)).map_err(|err| err.to_string())?;
    Ok(())
}

pub fn write_roundtrip_artifacts(
    report: &DsdRoundtripReport,
    out_dir: &Path,
) -> Result<(), String> {
    fs::create_dir_all(out_dir).map_err(|err| err.to_string())?;
    let json = versioned_json(report).map_err(|err| err.to_string())?;
    fs::write(out_dir.join("roundtrip_summary.json"), json).map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("roundtrip_metrics.csv"),
        roundtrip_metrics_csv(report),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("roundtrip_baseline_deltas.csv"),
        roundtrip_baseline_deltas_csv(report),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("roundtrip_baseline_deltas.md"),
        roundtrip_baseline_deltas_markdown(report),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("roundtrip_pink_noise_seed_summary.csv"),
        roundtrip_pink_noise_seed_summary_csv(report),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("roundtrip_pink_noise_seed_summary.md"),
        roundtrip_pink_noise_seed_summary_markdown(report),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("roundtrip_waveform_overlay.csv"),
        roundtrip_waveform_csv(report),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("roundtrip_spectrum.csv"),
        roundtrip_spectrum_csv(&report.artifacts.spectrum),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("roundtrip_residual_spectrum.csv"),
        roundtrip_spectrum_csv(&report.artifacts.residual_spectrum),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("roundtrip_spectrogram.csv"),
        roundtrip_spectrogram_csv(report),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("roundtrip_waveform_overlay.svg"),
        roundtrip_waveform_svg(report),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("roundtrip_spectrum.svg"),
        roundtrip_spectrum_svg(report),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("roundtrip_residual_spectrum.svg"),
        roundtrip_residual_spectrum_svg(report),
    )
    .map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("roundtrip_residual_spectrogram.svg"),
        roundtrip_residual_spectrogram_svg(report),
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

pub fn write_precheck_artifacts(report: &DsdPrecheckReport, out_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(out_dir).map_err(|err| err.to_string())?;
    let json = versioned_json(report).map_err(|err| err.to_string())?;
    fs::write(out_dir.join("precheck_summary.json"), json).map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("precheck_metrics.csv"),
        precheck_metrics_csv(report),
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

fn report_has_dsd128_ec4a_candidates(report: &SuiteReport) -> bool {
    report
        .dsd
        .iter()
        .any(|dsd| dsd.dsd_rate == "DSD128" && dsd.modulator == "EcDepth4Adaptive")
}

fn run_pcm_measurements(mode: SuiteMode) -> Vec<PcmMeasurement> {
    let filters = match mode {
        SuiteMode::Quick => vec![
            FilterCase {
                name: "SincExtreme32k",
                filter: FilterType::SincExtreme32k,
                gate: GateClass::ReportOnly,
            },
            FilterCase {
                name: "Minimum16k",
                filter: FilterType::Minimum16k,
                gate: GateClass::Minimum16k,
            },
            FilterCase {
                name: "Minimum16k",
                filter: FilterType::Minimum16k,
                gate: GateClass::Minimum16k,
            },
            FilterCase {
                name: "Split128k",
                filter: FilterType::Split128k,
                gate: GateClass::Minimum16k,
            },
        ],
        SuiteMode::Full => vec![
            FilterCase {
                name: "SincExtreme32k",
                filter: FilterType::SincExtreme32k,
                gate: GateClass::ReportOnly,
            },
            FilterCase {
                name: "Minimum16k",
                filter: FilterType::Minimum16k,
                gate: GateClass::Minimum16k,
            },
            FilterCase {
                name: "Minimum16k",
                filter: FilterType::Minimum16k,
                gate: GateClass::Minimum16k,
            },
            FilterCase {
                name: "Split128k",
                filter: FilterType::Split128k,
                gate: GateClass::Minimum16k,
            },
            FilterCase {
                name: "SincExtreme32k",
                filter: FilterType::SincExtreme32k,
                gate: GateClass::Extreme,
            },
            FilterCase {
                name: "Split128k",
                filter: FilterType::Split128k,
                gate: GateClass::Extreme,
            },
        ],
    };
    let rates = match mode {
        SuiteMode::Quick => vec![RateCase {
            source: 44_100,
            target: 352_800,
        }],
        SuiteMode::Full => vec![
            RateCase {
                source: 44_100,
                target: 352_800,
            },
            RateCase {
                source: 48_000,
                target: 384_000,
            },
            RateCase {
                source: 96_000,
                target: 384_000,
            },
        ],
    };
    let seconds = match mode {
        SuiteMode::Quick => PCM_SECONDS_QUICK,
        SuiteMode::Full => PCM_SECONDS_FULL,
    };

    let mut results = Vec::new();
    for filter in filters {
        for rate in &rates {
            eprintln!(
                "ecbeam2_quality: PCM {} {}->{}",
                filter.name, rate.source, rate.target
            );
            results.push(measure_pcm_case(filter, *rate, seconds));
        }
    }
    results
}

fn run_src_sweep_measurements(mode: SuiteMode) -> Vec<PcmMeasurement> {
    let filters = [
        FilterCase {
            name: "Minimum16k",
            filter: FilterType::Minimum16k,
            gate: GateClass::Minimum16k,
        },
        FilterCase {
            name: "Minimum16k",
            filter: FilterType::Minimum16k,
            gate: GateClass::Minimum16k,
        },
        FilterCase {
            name: "Split128k",
            filter: FilterType::Split128k,
            gate: GateClass::Minimum16k,
        },
    ];
    let rates = [
        RateCase {
            source: 44_100,
            target: 48_000,
        },
        RateCase {
            source: 48_000,
            target: 44_100,
        },
        RateCase {
            source: 44_100,
            target: 96_000,
        },
        RateCase {
            source: 44_100,
            target: 384_000,
        },
        RateCase {
            source: 48_000,
            target: 88_200,
        },
        RateCase {
            source: 48_000,
            target: 352_800,
        },
        RateCase {
            source: 96_000,
            target: 88_200,
        },
        RateCase {
            source: 192_000,
            target: 176_400,
        },
    ];
    let seconds = match mode {
        SuiteMode::Quick => PCM_SECONDS_QUICK,
        SuiteMode::Full => PCM_SECONDS_FULL,
    };

    let mut results = Vec::new();
    for filter in filters {
        for rate in rates {
            eprintln!(
                "ecbeam2_quality: SRC sweep {} {}->{}",
                filter.name, rate.source, rate.target
            );
            results.push(measure_pcm_case(filter, rate, seconds));
        }
    }
    results
}

fn measure_pcm_case(filter: FilterCase, rate: RateCase, seconds: f64) -> PcmMeasurement {
    let mut notes = Vec::new();
    let resampler = SincResampler::new(filter.filter, rate.source, rate.target);
    let latency_ms = resampler.latency_ms();
    let memory_bytes = resampler.estimated_memory_bytes();

    let (dc_gain_db, gain_1k_db, gain_18k_db, image_rejection_db) = (
        measure_dc_gain(filter.filter, rate, seconds),
        measure_tone_gain(filter.filter, rate, 997.0, seconds),
        measure_tone_gain(filter.filter, rate, 18_000.0, seconds),
        measure_image_rejection(filter.filter, rate, seconds),
    );
    let passband_profile = measure_passband_profile(filter.filter, rate, seconds);
    let passband_ripple_db = passband_profile.max_deviation_20hz_20khz_db;
    let (impulse_peak_index, pre_ringing_energy_db, post_ringing_energy_db) =
        measure_impulse(filter.filter, rate, filter.gate, &mut notes);
    let (ns_per_output_frame, one_core_percent) =
        measure_throughput(filter.filter, rate, seconds.min(0.25));

    PcmMeasurement {
        filter: filter.name.to_string(),
        source_rate: rate.source,
        target_rate: rate.target,
        dc_gain_db,
        gain_1k_db,
        gain_18k_db,
        passband_ripple_db,
        passband_profile,
        image_rejection_db,
        impulse_peak_index,
        pre_ringing_energy_db,
        post_ringing_energy_db,
        latency_ms,
        memory_bytes,
        ns_per_output_frame,
        one_core_percent,
        notes,
    }
}

fn run_dsd_measurements(
    mode: SuiteMode,
    scope: SuiteScope,
    config: DsdExperimentConfig,
) -> Vec<DsdMeasurement> {
    if !CALIBRATED {
        return vec![DsdMeasurement {
            modulator: "n/a".to_string(),
            filter: "DSD".to_string(),
            path_variant: "direct".to_string(),
            source_rate: 0,
            origin_source_rate: 0,
            renderer_source_rate: 0,
            intermediate_rate: None,
            intermediate_bits: None,
            intermediate_filter: None,
            path_prepare_ms: None,
            render_ms: None,
            dsd_rate: "skipped".to_string(),
            wire_rate: None,
            passband_profile: PassbandProfile::default(),
            residual_db: None,
            thdn_residual_db: None,
            inband_snr_db: None,
            inband_snr_worst_db: None,
            inband_snr_p05_db: None,
            inband_snr_p95_db: None,
            inband_snr_best_db: None,
            inband_snr_spread_db: None,
            inband_snr_left_db: None,
            inband_snr_right_db: None,
            inband_snr_left_worst_db: None,
            inband_snr_right_worst_db: None,
            inband_snr_left_spread_db: None,
            inband_snr_right_spread_db: None,
            inband_lf_sinad_worst_db: None,
            inband_lf_sinad_db: None,
            inband_lf_tone_hz: None,
            stereo_snr_worst_mismatch_db: None,
            inband_snr_window_count: None,
            inband_snr_worst_window_start_s: None,
            inband_noise_rms_dbfs: None,
            inband_noise_worst_rms_dbfs: None,
            inband_noise_peak_dbfs: None,
            inband_noise_peak_spur_hz: None,
            inband_noise_spur_margin_db: None,
            inband_noise_left_spur_margin_db: None,
            inband_noise_right_spur_margin_db: None,
            inband_noise_20_200_dbfs: None,
            inband_noise_200_2k_dbfs: None,
            inband_noise_2k_8k_dbfs: None,
            inband_noise_8k_16k_dbfs: None,
            inband_noise_16k_20k_dbfs: None,
            ultrasonic_24_50k_max_dbfs: None,
            ultrasonic_24_50k_median_dbfs: None,
            ultrasonic_24_50k_window_spread_db: None,
            ultrasonic_50_100k_max_dbfs: None,
            ultrasonic_50_100k_median_dbfs: None,
            ultrasonic_50_100k_window_spread_db: None,
            ultrasonic_100_200k_max_dbfs: None,
            ultrasonic_100_200k_median_dbfs: None,
            ultrasonic_100_200k_window_spread_db: None,
            inband_spurs: Vec::new(),
            inband_windows: Vec::new(),
            ultrasonic_windows: Vec::new(),
            premod_windows: Vec::new(),
            idle_tone_dbfs: None,
            idle_worst_tone_dbfs: None,
            idle_worst_density_deviation: None,
            idle_artifacts: Vec::new(),
            overload_recovery_diagnostics: Vec::new(),
            low_level_worst_residual_db: None,
            low_level_worst_spur_dbfs: None,
            high_freq_tone_worst_residual_db: None,
            high_freq_tone_worst_spur_dbfs: None,
            high_freq_imd_residual_db: None,
            high_freq_imd_spur_dbfs: None,
            high_freq_worst_residual_db: None,
            high_freq_worst_spur_dbfs: None,
            multitone_residual_db: None,
            multitone_spur_dbfs: None,
            overload_recovery_dbfs: None,
            transient_click_candidates: None,
            transient_click_max_score: None,
            transient_click_max_residual: None,
            program_click_candidates: None,
            program_click_max_score: None,
            program_click_max_residual: None,
            decoded_low: None,
            decoded_peak: None,
            decoded_abs_peak: None,
            bit_density: None,
            bit_density_left: None,
            bit_density_right: None,
            bit_density_max_deviation: None,
            bit_density_left_max_deviation: None,
            bit_density_right_max_deviation: None,
            transition_rate: None,
            limiter_peak_ratio_max: None,
            limiter_current_block_peak_ratio: None,
            limiter_current_block_gain: None,
            limiter_current_block_limited_samples: 0,
            limiter_limited_events: 0,
            limiter_limited_samples: 0,
            stability_resets: 0,
            state_clamps: 0,
            stress_stability_resets: 0,
            stress_state_clamps: 0,
            depth4_ratio: None,
            adaptive_decision_trace: None,
            ec2_decision_trace: None,
            dsd256_improvement_db: None,
            notes: vec!["DSD coefficient table is not calibrated".to_string()],
        }];
    }

    let seconds = match (mode, scope) {
        (SuiteMode::Quick, _) => DSD_SECONDS_QUICK,
        (SuiteMode::Full, SuiteScope::EcDepthOnly | SuiteScope::Ec3Tuning) => {
            DSD_EC_DEPTH_SECONDS_FULL
        }
        (SuiteMode::Full, _) => DSD_SECONDS_FULL,
    };
    let paths = match (mode, scope) {
        (_, SuiteScope::EcDepthOnly) => vec![
            ("Split128k", FilterType::Split128k, DsdRate::Dsd128),
            ("Split128k", FilterType::Split128k, DsdRate::Dsd256),
        ],
        (_, SuiteScope::Ec3Tuning) => {
            vec![("Split128k", FilterType::Split128k, DsdRate::Dsd128)]
        }
        (SuiteMode::Quick, _) => vec![
            ("Minimum16k", FilterType::Minimum16k, DsdRate::Dsd128),
            ("Minimum16k", FilterType::Minimum16k, DsdRate::Dsd256),
        ],
        (SuiteMode::Full, _) => vec![
            ("Minimum16k", FilterType::Minimum16k, DsdRate::Dsd128),
            ("Minimum16k", FilterType::Minimum16k, DsdRate::Dsd256),
            ("Minimum16k", FilterType::Minimum16k, DsdRate::Dsd128),
            ("Minimum16k", FilterType::Minimum16k, DsdRate::Dsd256),
            ("Split128k", FilterType::Split128k, DsdRate::Dsd128),
            ("Split128k", FilterType::Split128k, DsdRate::Dsd256),
            ("Split128k", FilterType::Split128k, DsdRate::Dsd128),
            ("Split128k", FilterType::Split128k, DsdRate::Dsd256),
            (
                "SincExtreme32k",
                FilterType::SincExtreme32k,
                DsdRate::Dsd256,
            ),
        ],
    };

    let mut results = Vec::new();
    let source_rates: &[u32] = match (mode, scope) {
        (_, SuiteScope::EcDepthOnly | SuiteScope::Ec3Tuning) | (SuiteMode::Quick, _) => &[44_100],
        (SuiteMode::Full, _) => &[44_100, 48_000],
    };
    let modulators: &[DsdModulator] = match scope {
        SuiteScope::EcDepthOnly => &[
            DsdModulator::Standard,
            DsdModulator::EcDepth1,
            DsdModulator::EcDepth2,
            DsdModulator::EcDepth3,
            DsdModulator::EcDepth4,
            DsdModulator::EcDepth8,
            DsdModulator::EcDepth4Adaptive,
        ],
        SuiteScope::Ec3Tuning => &[DsdModulator::EcDepth2, DsdModulator::EcDepth3],
        SuiteScope::All | SuiteScope::DsdOnly => &[
            DsdModulator::Standard,
            DsdModulator::EcDepth1,
            DsdModulator::EcDepth2,
            DsdModulator::EcDepth4,
            DsdModulator::EcDepth4Adaptive,
        ],
    };
    for &source_rate in source_rates {
        for (name, filter, rate) in &paths {
            for &modulator in modulators {
                if !should_measure_dsd_modulator(mode, scope, *filter, source_rate, modulator) {
                    continue;
                }
                let run_dsd128_ec4a_target_artifacts = mode == SuiteMode::Full
                    && scope == SuiteScope::All
                    && *filter == FilterType::Split128k
                    && *rate == DsdRate::Dsd128
                    && source_rate == 44_100
                    && matches!(
                        modulator,
                        DsdModulator::EcDepth2 | DsdModulator::EcDepth4Adaptive
                    );
                let run_depth_artifacts = mode == SuiteMode::Full
                    && (run_dsd128_ec4a_target_artifacts
                        || (dsd_artifact_filter_enabled(scope, *filter)
                            && source_rate == 44_100
                            && (*rate == DsdRate::Dsd256
                                || (*filter == FilterType::Split128k
                                    && *rate == DsdRate::Dsd128)
                                || (matches!(
                                    scope,
                                    SuiteScope::EcDepthOnly | SuiteScope::Ec3Tuning
                                ) && *rate == DsdRate::Dsd128))));
                let run_stress = run_depth_artifacts;
                let run_artifact_probes = run_depth_artifacts;
                let case_seconds = dsd_case_seconds(seconds, modulator);
                eprintln!(
                    "ecbeam2_quality: DSD {name} {} {} {}",
                    modulator.as_name(),
                    source_rate,
                    dsd_rate_name(*rate)
                );
                results.push(measure_dsd_case(
                    name,
                    *filter,
                    *rate,
                    source_rate,
                    modulator,
                    run_stress,
                    run_artifact_probes,
                    case_seconds,
                    config,
                ));
            }
        }
    }
    results
}

fn should_measure_dsd_modulator(
    mode: SuiteMode,
    scope: SuiteScope,
    filter: FilterType,
    source_rate: u32,
    modulator: DsdModulator,
) -> bool {
    if scope == SuiteScope::EcDepthOnly {
        return mode == SuiteMode::Full && filter == FilterType::Split128k && source_rate == 44_100;
    }
    if scope == SuiteScope::Ec3Tuning {
        return mode == SuiteMode::Full
            && filter == FilterType::Split128k
            && source_rate == 44_100
            && matches!(modulator, DsdModulator::EcDepth2 | DsdModulator::EcDepth3);
    }
    if filter == FilterType::Split128k {
        return mode == SuiteMode::Full
            && source_rate == 44_100
            && matches!(modulator, DsdModulator::EcDepth2);
    }
    match (mode, modulator) {
        (SuiteMode::Quick, DsdModulator::EcDepth2) => true,
        (SuiteMode::Quick, _) => false,
        (SuiteMode::Full, DsdModulator::EcDepth4) => {
            filter == FilterType::Minimum16k && source_rate == 44_100
        }
        (SuiteMode::Full, DsdModulator::EcDepth4Adaptive) => {
            source_rate == 44_100
                && matches!(
                    filter,
                    FilterType::Minimum16k | FilterType::Split128k | FilterType::SincExtreme32k
                )
        }
        (SuiteMode::Full, _) => true,
    }
}

fn dsd_artifact_filter_enabled(scope: SuiteScope, filter: FilterType) -> bool {
    match scope {
        SuiteScope::EcDepthOnly | SuiteScope::Ec3Tuning => filter == FilterType::Split128k,
        SuiteScope::All | SuiteScope::DsdOnly => {
            matches!(filter, FilterType::Minimum16k | FilterType::Split128k)
        }
    }
}

fn dsd_case_seconds(default_seconds: f64, modulator: DsdModulator) -> f64 {
    if matches!(modulator, DsdModulator::EcDepth4 | DsdModulator::EcDepth8) {
        DSD_EC4_SECONDS_FULL
    } else {
        default_seconds
    }
}

fn find_ec_obg_variant(
    dsd_rate: DsdRate,
    obg: f64,
) -> Option<(&'static str, &'static ModulatorCoeffs)> {
    let osr = dsd_rate.oversample();
    ALL_VARIANTS
        .iter()
        .copied()
        .find(|(_, coeffs)| coeffs.osr == osr && (coeffs.obg - obg).abs() <= 0.0005)
}

fn new_dsd_renderer(
    filter: FilterType,
    source_rate: u32,
    dsd_rate: DsdRate,
    dsd_modulator: DsdModulator,
    config: DsdExperimentConfig,
) -> Result<DsdRenderer, &'static str> {
    let coeffs_override = config
        .ec_coeff_for(dsd_rate, dsd_modulator)
        .map(|(_, coeffs)| coeffs);
    DsdRenderer::new_with_dsd_modulator_and_experiment_tweaks(
        filter,
        source_rate,
        dsd_rate,
        dsd_modulator,
        coeffs_override,
        production_tweaks_for(config, filter, dsd_rate, dsd_modulator),
    )
}

fn production_tweaks_for(
    config: DsdExperimentConfig,
    filter: FilterType,
    dsd_rate: DsdRate,
    dsd_modulator: DsdModulator,
) -> DsdExperimentTweaks {
    config
        .tweaks_for(dsd_rate, dsd_modulator)
        .with_production_policy_defaults(filter, dsd_rate, dsd_modulator)
}

#[allow(clippy::too_many_arguments)]
fn measure_dsd_roundtrip_case(
    candidate_index: usize,
    fixture: &DsdRoundtripFixture,
    filter_name: &str,
    filter: FilterType,
    dsd_rate: DsdRate,
    source_rate: u32,
    dsd_modulator: DsdModulator,
    seconds: f64,
    config: DsdExperimentConfig,
) -> Result<(DsdRoundtripMeasurement, DsdRoundtripArtifacts), String> {
    let Some(wire_rate) = dsd_rate.wire_rate_for_source(source_rate) else {
        return Err(format!(
            "unsupported {} source/wire-rate combination for {} Hz",
            dsd_rate_name(dsd_rate),
            source_rate
        ));
    };
    let reference_l = &fixture.left;
    let reference_r = &fixture.right;
    let frames = reference_l.len().min(reference_r.len());
    let mut renderer = new_dsd_renderer(filter, source_rate, dsd_rate, dsd_modulator, config)
        .map_err(|err| format!("DSD renderer init failed: {err}"))?;
    renderer.set_native_order(NativeDsdOrder::MsbFirst);
    let render_start = Instant::now();
    let (native_l, native_r) = render_native_stream(&mut renderer, reference_l, reference_r);
    let render_ms = render_start.elapsed().as_secs_f64() * 1000.0;
    let bits_l = unpack_native_msb(&native_l);
    let bits_r = unpack_native_msb(&native_r);
    let decoded_l = dsd_prefilter_and_decimate_to_pcm(&bits_l, wire_rate, source_rate)
        .ok_or_else(|| "left DSD prefilter/decimation failed".to_string())?;
    let decoded_r = dsd_prefilter_and_decimate_to_pcm(&bits_r, wire_rate, source_rate)
        .ok_or_else(|| "right DSD prefilter/decimation failed".to_string())?;
    let silence = vec![0.0; frames];
    let mut density_renderer =
        new_dsd_renderer(filter, source_rate, dsd_rate, dsd_modulator, config)
            .map_err(|err| format!("DSD density renderer init failed: {err}"))?;
    density_renderer.set_native_order(NativeDsdOrder::MsbFirst);
    let (density_native_l, density_native_r) =
        render_native_stream(&mut density_renderer, &silence, &silence);
    let density_bits_l = unpack_native_msb(&density_native_l);
    let density_bits_r = unpack_native_msb(&density_native_r);
    let left = analyze_roundtrip_channel(reference_l, &decoded_l, source_rate);
    let right = analyze_roundtrip_channel(reference_r, &decoded_r, source_rate);
    let mut notes = vec!["bit_density_source=silence".to_string()];
    if left.is_none() {
        notes.push("left channel alignment unavailable".to_string());
    }
    if right.is_none() {
        notes.push("right channel alignment unavailable".to_string());
    }

    let limiter = renderer.limiter_telemetry();
    let bit_density_left = bit_density(&density_bits_l);
    let bit_density_right = bit_density(&density_bits_r);
    let bit_density = match (bit_density_left, bit_density_right) {
        (Some(left), Some(right)) => Some((left + right) * 0.5),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    };
    let density_window = (wire_rate as usize / 100).max(1024);
    let bit_density_left_max_deviation =
        rolling_bit_density_max_deviation(&density_bits_l, density_window);
    let bit_density_right_max_deviation =
        rolling_bit_density_max_deviation(&density_bits_r, density_window);
    let bit_density_max_deviation = max_opt(
        bit_density_left_max_deviation,
        bit_density_right_max_deviation,
    );
    let decoded_abs_peak = max_opt(sample_abs_peak(&decoded_l), sample_abs_peak(&decoded_r));
    let passband_profile = measure_passband_profile(
        filter,
        RateCase {
            source: source_rate,
            target: wire_rate,
        },
        seconds,
    );
    let stability_resets = renderer.stability_resets();
    let state_clamps = renderer.state_clamps();

    let mut measurement = DsdRoundtripMeasurement {
        candidate_index,
        fixture: fixture.label.to_string(),
        filter: filter_name.to_string(),
        modulator: dsd_modulator.as_name().to_string(),
        source_rate,
        dsd_rate: dsd_rate_name(dsd_rate).to_string(),
        wire_rate,
        seconds,
        render_ms: Some(render_ms),
        alignment_delay_left_samples: left.as_ref().map(|analysis| analysis.delay_samples),
        alignment_delay_right_samples: right.as_ref().map(|analysis| analysis.delay_samples),
        alignment_gain_left: left.as_ref().map(|analysis| analysis.gain),
        alignment_gain_right: right.as_ref().map(|analysis| analysis.gain),
        correlation_left: left.as_ref().map(|analysis| analysis.correlation),
        correlation_right: right.as_ref().map(|analysis| analysis.correlation),
        correlation_worst: min_opt(
            left.as_ref().map(|analysis| analysis.correlation),
            right.as_ref().map(|analysis| analysis.correlation),
        ),
        residual_rms_db_left: left.as_ref().map(|analysis| analysis.residual_relative_db),
        residual_rms_db_right: right.as_ref().map(|analysis| analysis.residual_relative_db),
        residual_rms_db_worst: max_opt(
            left.as_ref().map(|analysis| analysis.residual_relative_db),
            right.as_ref().map(|analysis| analysis.residual_relative_db),
        ),
        inband_residual_rms_dbfs_left: left.as_ref().map(|analysis| analysis.residual_rms_dbfs),
        inband_residual_rms_dbfs_right: right.as_ref().map(|analysis| analysis.residual_rms_dbfs),
        inband_residual_rms_dbfs_worst: max_opt(
            left.as_ref().map(|analysis| analysis.residual_rms_dbfs),
            right.as_ref().map(|analysis| analysis.residual_rms_dbfs),
        ),
        inband_residual_peak_dbfs_left: left.as_ref().map(|analysis| analysis.residual_peak_dbfs),
        inband_residual_peak_dbfs_right: right.as_ref().map(|analysis| analysis.residual_peak_dbfs),
        inband_residual_peak_dbfs_worst: max_opt(
            left.as_ref().map(|analysis| analysis.residual_peak_dbfs),
            right.as_ref().map(|analysis| analysis.residual_peak_dbfs),
        ),
        inband_residual_spur_margin_db_left: left
            .as_ref()
            .and_then(|analysis| analysis.residual_spur_margin_db),
        inband_residual_spur_margin_db_right: right
            .as_ref()
            .and_then(|analysis| analysis.residual_spur_margin_db),
        inband_residual_spur_margin_db_worst: min_opt(
            left.as_ref()
                .and_then(|analysis| analysis.residual_spur_margin_db),
            right
                .as_ref()
                .and_then(|analysis| analysis.residual_spur_margin_db),
        ),
        decoded_abs_peak,
        bit_density,
        bit_density_left,
        bit_density_right,
        bit_density_max_deviation,
        bit_density_left_max_deviation,
        bit_density_right_max_deviation,
        passband_profile,
        limiter_peak_ratio_max: Some(limiter.peak_ratio_max as f64),
        limiter_limited_events: limiter.limited_events,
        limiter_limited_samples: limiter.limited_samples,
        stability_resets,
        state_clamps,
        status: "pass".to_string(),
        hard_failures: Vec::new(),
        notes,
    };
    measurement.hard_failures = dsd_roundtrip_hard_failures(&measurement);
    if !measurement.hard_failures.is_empty() {
        measurement.status = "fail".to_string();
    }

    let mut artifacts = DsdRoundtripArtifacts::default();
    if let Some(left) = &left {
        append_roundtrip_artifacts(&mut artifacts, &measurement, "left", source_rate, left);
    }
    if let Some(right) = &right {
        append_roundtrip_artifacts(&mut artifacts, &measurement, "right", source_rate, right);
    }
    Ok((measurement, artifacts))
}

#[derive(Clone)]
struct DsdRoundtripFixture {
    label: &'static str,
    left: Vec<f64>,
    right: Vec<f64>,
}

fn stability_precheck_probes(
    frames: usize,
    sample_rate: u32,
) -> Vec<(&'static str, Vec<f64>, Vec<f64>)> {
    let loud_hf = sine(frames, sample_rate, 19_000.0, 0.80);
    let lf_dc = lf_with_dc_offset(frames, sample_rate);
    let silence = vec![0.0; frames];
    let pink = pink_noise(frames, 0x5354_4142_494c_4954, 0.42);
    vec![
        ("sine_19k_hot", loud_hf.clone(), loud_hf),
        ("lf_dc_offset", lf_dc.clone(), lf_dc),
        ("silence", silence.clone(), silence),
        ("pink_noise_seeded", pink.clone(), pink),
    ]
}

fn lf_with_dc_offset(frames: usize, sample_rate: u32) -> Vec<f64> {
    (0..frames)
        .map(|idx| {
            let t = idx as f64 / sample_rate as f64;
            (0.72 * (2.0 * PI * 41.0 * t).sin() + 0.18).clamp(-0.98, 0.98)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn measure_dsd_precheck_case(
    candidate_index: usize,
    probe: &str,
    reference_l: &[f64],
    reference_r: &[f64],
    filter_name: &str,
    filter: FilterType,
    dsd_rate: DsdRate,
    source_rate: u32,
    dsd_modulator: DsdModulator,
    config: DsdExperimentConfig,
) -> Result<DsdPrecheckMeasurement, String> {
    let Some(wire_rate) = dsd_rate.wire_rate_for_source(source_rate) else {
        return Err(format!(
            "unsupported {} source/wire-rate combination for {} Hz",
            dsd_rate_name(dsd_rate),
            source_rate
        ));
    };
    let frames = reference_l.len().min(reference_r.len());
    let mut renderer = new_dsd_renderer(filter, source_rate, dsd_rate, dsd_modulator, config)
        .map_err(|err| format!("DSD renderer init failed: {err}"))?;
    renderer.set_native_order(NativeDsdOrder::MsbFirst);
    let render_start = Instant::now();
    let (native_l, native_r) = render_native_stream(&mut renderer, reference_l, reference_r);
    let render_ms = render_start.elapsed().as_secs_f64() * 1000.0;
    let bits_l = unpack_native_msb(&native_l);
    let bits_r = unpack_native_msb(&native_r);
    let decoded_l = dsd_prefilter_and_decimate_to_pcm(&bits_l, wire_rate, source_rate)
        .ok_or_else(|| "left DSD prefilter/decimation failed".to_string())?;
    let decoded_r = dsd_prefilter_and_decimate_to_pcm(&bits_r, wire_rate, source_rate)
        .ok_or_else(|| "right DSD prefilter/decimation failed".to_string())?;
    let bit_density_left = bit_density(&bits_l);
    let bit_density_right = bit_density(&bits_r);
    let bit_density = match (bit_density_left, bit_density_right) {
        (Some(left), Some(right)) => Some((left + right) * 0.5),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    };
    let density_window = (wire_rate as usize / 100).max(1024);
    let bit_density_left_max_deviation = rolling_bit_density_max_deviation(&bits_l, density_window);
    let bit_density_right_max_deviation =
        rolling_bit_density_max_deviation(&bits_r, density_window);
    let limiter = renderer.limiter_telemetry();
    let mut measurement = DsdPrecheckMeasurement {
        candidate_index,
        probe: probe.to_string(),
        filter: filter_name.to_string(),
        modulator: dsd_modulator.as_name().to_string(),
        source_rate,
        dsd_rate: dsd_rate_name(dsd_rate).to_string(),
        wire_rate,
        seconds: frames as f64 / source_rate as f64,
        frames,
        render_ms: Some(render_ms),
        decoded_abs_peak: max_opt(sample_abs_peak(&decoded_l), sample_abs_peak(&decoded_r)),
        bit_density,
        bit_density_left,
        bit_density_right,
        bit_density_max_deviation: max_opt(
            bit_density_left_max_deviation,
            bit_density_right_max_deviation,
        ),
        bit_density_left_max_deviation,
        bit_density_right_max_deviation,
        limiter_peak_ratio_max: Some(limiter.peak_ratio_max as f64),
        limiter_limited_events: limiter.limited_events,
        limiter_limited_samples: limiter.limited_samples,
        stability_resets: renderer.stability_resets(),
        state_clamps: renderer.state_clamps(),
        status: "pass".to_string(),
        hard_failures: Vec::new(),
        notes: vec!["precheck_hard_health_only".to_string()],
    };
    measurement.hard_failures = dsd_precheck_hard_failures(&measurement);
    if !measurement.hard_failures.is_empty() {
        measurement.status = "fail".to_string();
    }
    Ok(measurement)
}

fn dsd_precheck_hard_failures(measurement: &DsdPrecheckMeasurement) -> Vec<String> {
    let mut failures = Vec::new();
    hard_eq_zero(
        &mut failures,
        "stability_resets",
        measurement.stability_resets,
    );
    hard_eq_zero(&mut failures, "state_clamps", measurement.state_clamps);
    hard_eq_zero(
        &mut failures,
        "limiter_limited_events",
        measurement.limiter_limited_events,
    );
    hard_eq_zero(
        &mut failures,
        "limiter_limited_samples",
        measurement.limiter_limited_samples,
    );
    hard_max(
        &mut failures,
        "decoded_abs_peak",
        measurement.decoded_abs_peak,
        1.05,
    );
    if measurement.probe == "silence" {
        hard_max(
            &mut failures,
            "bit_density_max_deviation",
            measurement.bit_density_max_deviation,
            0.005,
        );
    }
    failures
}

fn roundtrip_fixtures(frames: usize, sample_rate: u32) -> Vec<DsdRoundtripFixture> {
    let (program_l, program_r) = roundtrip_program_fixture(frames, sample_rate);
    let mut fixtures = vec![DsdRoundtripFixture {
        label: "program_multitone",
        left: program_l,
        right: program_r,
    }];
    for (idx, (left_seed, right_seed)) in DSD_ROUNDTRIP_PINK_NOISE_SEEDS.iter().enumerate() {
        let label = match idx {
            0 => "pink_noise_seed1",
            1 => "pink_noise_seed2",
            2 => "pink_noise_seed3",
            _ => "pink_noise_seed_extra",
        };
        fixtures.push(DsdRoundtripFixture {
            label,
            left: pink_noise(frames, *left_seed, 0.28),
            right: pink_noise(frames, *right_seed, 0.28),
        });
    }
    fixtures.extend([
        DsdRoundtripFixture {
            label: "sine_997_-6db",
            left: sine(frames, sample_rate, 997.0, 10.0f64.powf(-6.0 / 20.0)),
            right: sine(frames, sample_rate, 997.0, 10.0f64.powf(-6.0 / 20.0)),
        },
        DsdRoundtripFixture {
            label: "sine_997_-60db",
            left: sine(frames, sample_rate, 997.0, 10.0f64.powf(-60.0 / 20.0)),
            right: sine(frames, sample_rate, 997.0, 10.0f64.powf(-60.0 / 20.0)),
        },
        DsdRoundtripFixture {
            label: "sine_19k_-12db",
            left: sine(frames, sample_rate, 19_000.0, 10.0f64.powf(-12.0 / 20.0)),
            right: sine(frames, sample_rate, 19_000.0, 10.0f64.powf(-12.0 / 20.0)),
        },
        DsdRoundtripFixture {
            label: "two_tone_18k_19k",
            left: two_tone(frames, sample_rate, 18_000.0, 19_000.0, 0.18),
            right: two_tone(frames, sample_rate, 18_000.0, 19_000.0, 0.18),
        },
    ]);
    debug_assert_eq!(fixtures.len(), DSD_ROUNDTRIP_FIXTURE_COUNT);
    fixtures
}

fn roundtrip_program_fixture(frames: usize, sample_rate: u32) -> (Vec<f64>, Vec<f64>) {
    let (base, _) = program_multitone(frames, sample_rate, 0.42);
    let right = base
        .iter()
        .enumerate()
        .map(|(idx, sample)| {
            let t = idx as f64 / sample_rate as f64;
            0.94 * sample + 0.018 * (2.0 * PI * 659.25 * t + 0.4).sin()
        })
        .collect();
    (base, right)
}

fn pink_noise(frames: usize, seed: u64, amp: f64) -> Vec<f64> {
    let mut b0 = 0.0;
    let mut b1 = 0.0;
    let mut b2 = 0.0;
    let mut max_abs = 0.0f64;
    let mut out = Vec::with_capacity(frames);
    for idx in 0..frames {
        let white = deterministic_noise_with_seed(idx, seed);
        b0 = 0.99765 * b0 + white * 0.0990460;
        b1 = 0.96300 * b1 + white * 0.2965164;
        b2 = 0.57000 * b2 + white * 1.0526913;
        let sample = b0 + b1 + b2 + white * 0.1848;
        max_abs = max_abs.max(sample.abs());
        out.push(sample);
    }
    let scale = amp / max_abs.max(1e-18);
    for sample in &mut out {
        *sample *= scale;
    }
    out
}

fn deterministic_noise_with_seed(idx: usize, seed: u64) -> f64 {
    let mut x = idx as u64 ^ seed;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    let value = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
    let mantissa = (value >> 11) as f64 / ((1u64 << 53) as f64);
    2.0 * mantissa - 1.0
}

fn dsd_roundtrip_hard_failures(measurement: &DsdRoundtripMeasurement) -> Vec<String> {
    let mut failures = Vec::new();
    hard_eq_zero(
        &mut failures,
        "stability_resets",
        measurement.stability_resets,
    );
    hard_eq_zero(&mut failures, "state_clamps", measurement.state_clamps);
    hard_eq_zero(
        &mut failures,
        "limiter_limited_events",
        measurement.limiter_limited_events,
    );
    hard_eq_zero(
        &mut failures,
        "limiter_limited_samples",
        measurement.limiter_limited_samples,
    );
    hard_max(
        &mut failures,
        "decoded_abs_peak",
        measurement.decoded_abs_peak,
        1.05,
    );
    hard_max(
        &mut failures,
        "bit_density_max_deviation",
        measurement.bit_density_max_deviation,
        0.005,
    );
    hard_max(
        &mut failures,
        "passband_peak_gain_20hz_20khz_db",
        measurement.passband_profile.peak_gain_20hz_20khz_db,
        0.02,
    );
    if measurement.correlation_worst.is_none() {
        failures.push("correlation_worst unavailable".to_string());
    }
    failures
}

/// Dedicated low-frequency SINAD probe for Workstream D. Renders a ~100 Hz
/// coherent tone through the same path as the main measurement and reports its
/// worst/median in-band SINAD plus the coherent tone frequency actually used.
/// Returns `None` if the render is too short for a spectral window. This costs a
/// full second render, so `measure_dsd_case` only calls it behind an env gate.
#[allow(clippy::too_many_arguments)]
fn measure_dsd_lf_sinad(
    filter: FilterType,
    path: DsdPcmPath,
    dsd_rate: DsdRate,
    dsd_modulator: DsdModulator,
    config: DsdExperimentConfig,
    source_rate: u32,
    renderer_source_rate: u32,
    wire_rate: u32,
    frames: usize,
    tone_fft_bits: usize,
) -> Option<(f64, f64, f64)> {
    let lf_tone_hz = coherent_dsd_tone_hz(wire_rate, tone_fft_bits, DSD_LF_SINAD_TONE_HZ);
    let input = sine(frames, source_rate, lf_tone_hz, 0.35);
    let renderer_input = path.prepare_renderer_input(filter, &input).ok()?;
    let mut renderer = new_dsd_renderer(
        filter,
        renderer_source_rate,
        dsd_rate,
        dsd_modulator,
        config,
    )
    .ok()?;
    renderer.set_native_order(NativeDsdOrder::MsbFirst);
    let (native_l, native_r) =
        render_native_stream(&mut renderer, &renderer_input, &renderer_input);
    let bits_l = unpack_native_msb(&native_l);
    let bits_r = unpack_native_msb(&native_r);
    let metrics = dsd_stereo_inband_tone_metrics(
        &bits_l,
        &bits_r,
        wire_rate,
        lf_tone_hz,
        0.35,
        tone_fft_bits,
    )?;
    Some((
        metrics.aggregate.sinad_worst_db,
        metrics.aggregate.sinad_db,
        lf_tone_hz,
    ))
}

fn dsd_modulator_measurement_name(
    dsd_modulator: DsdModulator,
    tweaks: DsdExperimentTweaks,
) -> String {
    if let Some((m, n)) = tweaks.ec_beam_search {
        format!("EcBeamM{m}N{n}")
    } else {
        dsd_modulator.as_name().to_string()
    }
}

fn append_beam_diagnostics_notes(notes: &mut Vec<String>, renderer: &DsdRenderer) {
    append_ecbeam2_diagnostics_notes(notes, renderer);
    let mut emit_count = 0u64;
    let mut path_switches = 0u64;
    let mut delayed_flips = 0u64;
    let mut pruned_total = 0u64;
    let mut beam_clamp_total = 0u64;
    let mut min_survivors: Option<u64> = None;
    for diag in renderer.beam_diagnostics().into_iter().flatten() {
        emit_count += diag.emit_count;
        path_switches += diag.path_switches;
        delayed_flips += diag.delayed_flips;
        pruned_total += diag.pruned_total;
        beam_clamp_total += diag.beam_clamp_total;
        min_survivors = Some(match min_survivors {
            Some(current) => current.min(diag.min_survivors),
            None => diag.min_survivors,
        });
    }
    if min_survivors.is_some() {
        notes.push(format!("beam_emit_count={emit_count}"));
        notes.push(format!("beam_path_switches={path_switches}"));
        notes.push(format!("beam_delayed_flips={delayed_flips}"));
        notes.push(format!("beam_pruned_total={pruned_total}"));
        notes.push(format!("beam_clamp_total={beam_clamp_total}"));
        notes.push(format!(
            "beam_min_survivors={}",
            min_survivors.unwrap_or_default()
        ));
    }

    let mut reconstruction_samples = 0u64;
    let mut reconstruction_energy_sum = 0.0;
    let mut reconstruction_energy_max: f64 = 0.0;
    let mut reconstruction_weighted_sum = 0.0;
    let mut reconstruction_weighted_max: f64 = 0.0;
    let mut reconstruction_ratio_samples = 0u64;
    let mut reconstruction_ratio_sum = 0.0;
    let mut reconstruction_ratio_max: f64 = 0.0;
    for diag in renderer
        .beam_reconstruction_diagnostics()
        .into_iter()
        .flatten()
    {
        reconstruction_samples += diag.samples;
        reconstruction_energy_sum += diag.filtered_energy_sum;
        reconstruction_energy_max = reconstruction_energy_max.max(diag.filtered_energy_max);
        reconstruction_weighted_sum += diag.weighted_contribution_sum;
        reconstruction_weighted_max =
            reconstruction_weighted_max.max(diag.weighted_contribution_max);
        reconstruction_ratio_samples += diag.contribution_to_legacy_ratio_samples;
        reconstruction_ratio_sum += diag.contribution_to_legacy_ratio_sum;
        reconstruction_ratio_max =
            reconstruction_ratio_max.max(diag.contribution_to_legacy_ratio_max);
    }
    if reconstruction_samples > 0 {
        notes.push(format!(
            "beam_reconstruction_error_samples={reconstruction_samples}"
        ));
        notes.push(format!(
            "beam_reconstruction_error_energy_mean={:.9}",
            reconstruction_energy_sum / reconstruction_samples as f64
        ));
        notes.push(format!(
            "beam_reconstruction_error_energy_max={reconstruction_energy_max:.9}"
        ));
        notes.push(format!(
            "beam_reconstruction_error_weighted_mean={:.9}",
            reconstruction_weighted_sum / reconstruction_samples as f64
        ));
        notes.push(format!(
            "beam_reconstruction_error_weighted_max={reconstruction_weighted_max:.9}"
        ));
        if reconstruction_ratio_samples > 0 {
            notes.push(format!(
                "beam_reconstruction_error_legacy_ratio_mean={:.9}",
                reconstruction_ratio_sum / reconstruction_ratio_samples as f64
            ));
            notes.push(format!(
                "beam_reconstruction_error_legacy_ratio_max={reconstruction_ratio_max:.9}"
            ));
        }
    }

    let mut periodicity_samples = 0u64;
    let mut periodicity_penalty_sum = 0.0;
    let mut periodicity_penalty_max: f64 = 0.0;
    let mut periodicity_weighted_sum = 0.0;
    let mut periodicity_weighted_max: f64 = 0.0;
    let mut periodicity_ratio_samples = 0u64;
    let mut periodicity_ratio_sum = 0.0;
    let mut periodicity_ratio_max: f64 = 0.0;
    for diag in renderer
        .beam_periodicity_diagnostics()
        .into_iter()
        .flatten()
    {
        periodicity_samples += diag.samples;
        periodicity_penalty_sum += diag.penalty_sum;
        periodicity_penalty_max = periodicity_penalty_max.max(diag.penalty_max);
        periodicity_weighted_sum += diag.weighted_contribution_sum;
        periodicity_weighted_max = periodicity_weighted_max.max(diag.weighted_contribution_max);
        periodicity_ratio_samples += diag.contribution_to_legacy_ratio_samples;
        periodicity_ratio_sum += diag.contribution_to_legacy_ratio_sum;
        periodicity_ratio_max = periodicity_ratio_max.max(diag.contribution_to_legacy_ratio_max);
    }
    if periodicity_samples > 0 {
        notes.push(format!("beam_periodicity_samples={periodicity_samples}"));
        notes.push(format!(
            "beam_periodicity_penalty_mean={:.9}",
            periodicity_penalty_sum / periodicity_samples as f64
        ));
        notes.push(format!(
            "beam_periodicity_penalty_max={periodicity_penalty_max:.9}"
        ));
        notes.push(format!(
            "beam_periodicity_weighted_mean={:.9}",
            periodicity_weighted_sum / periodicity_samples as f64
        ));
        notes.push(format!(
            "beam_periodicity_weighted_max={periodicity_weighted_max:.9}"
        ));
        if periodicity_ratio_samples > 0 {
            notes.push(format!(
                "beam_periodicity_legacy_ratio_mean={:.9}",
                periodicity_ratio_sum / periodicity_ratio_samples as f64
            ));
            notes.push(format!(
                "beam_periodicity_legacy_ratio_max={periodicity_ratio_max:.9}"
            ));
        }
    }
}

fn append_ecbeam2_idle_health_notes(
    notes: &mut Vec<String>,
    renderer: &DsdRenderer,
    diagnostics_required: bool,
    active_ecbeam2: bool,
    expected_window_samples: Option<u64>,
) -> Vec<String> {
    let diagnostics = renderer
        .ecbeam2_diagnostics()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if diagnostics.is_empty() {
        return diagnostics_required
            .then(|| "missing idle EcBeam2 diagnostics".to_string())
            .into_iter()
            .collect();
    }
    let sum = |read: fn(&fozmo::audio::dsd::delta_sigma::EcBeam2Diagnostics) -> u64| {
        diagnostics.iter().map(read).sum::<u64>()
    };
    let committed_samples = sum(|diag| diag.diagnostic_window_samples);
    let constraint_escape = sum(|diag| diag.constraint_escape);
    let state_repair_fallback = sum(|diag| diag.state_repair_fallback);
    let all_nonfinite_resets = sum(|diag| diag.all_nonfinite_resets);
    let observer_desynchronizations = sum(|diag| diag.observer_desynchronizations);
    let invalid_input_substitutions = sum(|diag| diag.invalid_input_substitutions);
    let truncation = renderer.truncation_telemetry();
    let output_length_error = sum(|diag| diag.output_length_events).wrapping_add(truncation.events);
    let min_survivors = diagnostics
        .iter()
        .map(|diag| diag.min_survivors)
        .min()
        .unwrap_or_default();
    for (name, value) in [
        ("ecbeam2_idle_committed_samples", committed_samples),
        ("ecbeam2_idle_constraint_escape", constraint_escape),
        ("ecbeam2_idle_state_repair_fallback", state_repair_fallback),
        ("ecbeam2_idle_all_nonfinite_resets", all_nonfinite_resets),
        (
            "ecbeam2_idle_observer_desynchronizations",
            observer_desynchronizations,
        ),
        (
            "ecbeam2_idle_invalid_input_substitutions",
            invalid_input_substitutions,
        ),
        ("ecbeam2_idle_output_length_error", output_length_error),
        (
            "ecbeam2_idle_renderer_discarded_left_bits",
            truncation.discarded_left_bits,
        ),
        (
            "ecbeam2_idle_renderer_discarded_right_bits",
            truncation.discarded_right_bits,
        ),
    ] {
        notes.push(format!("{name}={value}"));
    }
    notes.push(format!("ecbeam2_idle_min_survivors={min_survivors}"));

    let mut failures = Vec::new();
    match expected_window_samples {
        Some(expected) if committed_samples != expected => failures.push(format!(
            "ecbeam2_idle_committed_samples={committed_samples} expected={expected}"
        )),
        None => failures.push("ecbeam2_idle_expected_committed_samples_unavailable".to_string()),
        Some(_) => {}
    }
    if active_ecbeam2 && min_survivors == 0 {
        failures.push("ecbeam2_idle_min_survivors=0".to_string());
    }
    for (name, value) in [
        ("ecbeam2_idle_constraint_escape", constraint_escape),
        ("ecbeam2_idle_state_repair_fallback", state_repair_fallback),
        ("ecbeam2_idle_all_nonfinite_resets", all_nonfinite_resets),
        (
            "ecbeam2_idle_observer_desynchronizations",
            observer_desynchronizations,
        ),
        (
            "ecbeam2_idle_invalid_input_substitutions",
            invalid_input_substitutions,
        ),
        ("ecbeam2_idle_output_length_error", output_length_error),
    ] {
        if value != 0 {
            failures.push(format!("{name}={value}"));
        }
    }
    failures
}

fn append_ecbeam2_diagnostics_notes(notes: &mut Vec<String>, renderer: &DsdRenderer) {
    let diagnostics = renderer
        .ecbeam2_diagnostics()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if diagnostics.is_empty() {
        return;
    }
    let sum_u64 = |read: fn(&fozmo::audio::dsd::delta_sigma::EcBeam2Diagnostics) -> u64| {
        diagnostics.iter().map(read).sum::<u64>()
    };
    let sum_f64 = |read: fn(&fozmo::audio::dsd::delta_sigma::EcBeam2Diagnostics) -> f64| {
        diagnostics.iter().map(read).sum::<f64>()
    };
    let max_f64 = |read: fn(&fozmo::audio::dsd::delta_sigma::EcBeam2Diagnostics) -> f64| {
        diagnostics.iter().map(read).fold(0.0_f64, f64::max)
    };
    let total_committed_samples = sum_u64(|diag| diag.committed_samples);
    let committed_samples = sum_u64(|diag| diag.diagnostic_window_samples);
    let diagnostic_window_enabled = diagnostics
        .iter()
        .any(|diag| diag.diagnostic_window_enabled);
    let min_survivors = diagnostics
        .iter()
        .map(|diag| diag.min_survivors)
        .min()
        .unwrap_or_default();
    notes.push(format!("ecbeam2_committed_samples={committed_samples}"));
    notes.push(format!(
        "ecbeam2_total_committed_samples={total_committed_samples}"
    ));
    notes.push(format!(
        "ecbeam2_diagnostic_window_enabled={diagnostic_window_enabled}"
    ));
    notes.push(format!(
        "ecbeam2_diagnostic_window_start_sequence={}",
        diagnostics
            .iter()
            .map(|diag| diag.diagnostic_window_start_sequence)
            .min()
            .unwrap_or_default()
    ));
    notes.push(format!(
        "ecbeam2_diagnostic_window_end_sequence={}",
        diagnostics
            .iter()
            .map(|diag| diag.diagnostic_window_end_sequence)
            .max()
            .unwrap_or_default()
    ));
    notes.push(format!(
        "ecbeam2_diagnostic_window_starting_tail_energy={:.12}",
        sum_f64(|diag| diag.diagnostic_window_starting_tail_energy)
    ));
    notes.push(format!(
        "ecbeam2_diagnostic_window_remaining_tail_energy={:.12}",
        sum_f64(|diag| diag.diagnostic_window_remaining_tail_energy)
    ));
    notes.push(format!(
        "ecbeam2_a1_frontier_events={}",
        sum_u64(|diag| diag.a1_frontier_events)
    ));
    notes.push(format!(
        "ecbeam2_a1_best_child_disagreements={}",
        sum_u64(|diag| diag.a1_best_child_disagreements)
    ));
    notes.push(format!(
        "ecbeam2_a1_top_m_disagreements={}",
        sum_u64(|diag| diag.a1_top_m_disagreements)
    ));
    notes.push(format!(
        "ecbeam2_positive_bits={}",
        sum_u64(|diag| diag.positive_bits)
    ));
    notes.push(format!(
        "ecbeam2_diagnostic_window_positive_bits={}",
        sum_u64(|diag| diag.diagnostic_window_positive_bits)
    ));
    notes.push(format!(
        "ecbeam2_path_switches={}",
        sum_u64(|diag| diag.path_switches)
    ));
    notes.push(format!(
        "ecbeam2_pruned_total={}",
        sum_u64(|diag| diag.pruned_total)
    ));
    notes.push(format!("ecbeam2_min_survivors={min_survivors}"));
    notes.push(format!(
        "ecbeam2_constraint_escape={}",
        sum_u64(|diag| diag.constraint_escape)
    ));
    notes.push(format!(
        "ecbeam2_state_repair_fallback={}",
        sum_u64(|diag| diag.state_repair_fallback)
    ));
    for (name, value) in [
        (
            "ecbeam2_first_constraint_escape_sequence",
            diagnostics
                .iter()
                .filter_map(|diag| diag.first_constraint_escape_sequence)
                .min(),
        ),
        (
            "ecbeam2_first_state_repair_sequence",
            diagnostics
                .iter()
                .filter_map(|diag| diag.first_state_repair_sequence)
                .min(),
        ),
        (
            "ecbeam2_last_constraint_escape_sequence",
            diagnostics
                .iter()
                .filter_map(|diag| diag.last_constraint_escape_sequence)
                .max(),
        ),
        (
            "ecbeam2_last_state_repair_sequence",
            diagnostics
                .iter()
                .filter_map(|diag| diag.last_state_repair_sequence)
                .max(),
        ),
    ] {
        if let Some(value) = value {
            notes.push(format!("{name}={value}"));
        }
    }
    for (name, value) in [
        (
            "ecbeam2_maximum_state_overflow",
            max_f64(|diag| diag.maximum_state_overflow),
        ),
        (
            "ecbeam2_maximum_budget_violation",
            max_f64(|diag| diag.maximum_budget_violation),
        ),
    ] {
        notes.push(format!("{name}={value:.12}"));
    }
    for (name, value) in [
        (
            "ecbeam2_maximum_consecutive_constraint_escapes",
            diagnostics
                .iter()
                .map(|diag| diag.maximum_consecutive_constraint_escapes)
                .max()
                .unwrap_or_default(),
        ),
        (
            "ecbeam2_maximum_consecutive_state_repairs",
            diagnostics
                .iter()
                .map(|diag| diag.maximum_consecutive_state_repairs)
                .max()
                .unwrap_or_default(),
        ),
        (
            "ecbeam2_ultrasonic_budget_escape_count",
            sum_u64(|diag| diag.ultrasonic_budget_escape_count),
        ),
        (
            "ecbeam2_signed_error_budget_escape_count",
            sum_u64(|diag| diag.signed_error_budget_escape_count),
        ),
        (
            "ecbeam2_both_budget_escape_count",
            sum_u64(|diag| diag.both_budget_escape_count),
        ),
    ] {
        notes.push(format!("{name}={value}"));
    }
    let stage_counts = (0..7)
        .map(|stage| {
            diagnostics
                .iter()
                .map(|diag| diag.state_repair_stage_counts[stage])
                .sum::<u64>()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("-");
    notes.push(format!("ecbeam2_state_repair_stage_counts={stage_counts}"));
    let stage_maxima = (0..7)
        .map(|stage| {
            format!(
                "{:.12}",
                diagnostics
                    .iter()
                    .map(|diag| diag.maximum_normalized_state_by_stage[stage])
                    .fold(0.0f64, f64::max)
            )
        })
        .collect::<Vec<_>>()
        .join("-");
    notes.push(format!(
        "ecbeam2_maximum_normalized_state_by_stage={stage_maxima}"
    ));
    for (prefix, distribution) in [
        (
            "ecbeam2_scale_reconstruction_increment_abs",
            diagnostics
                .iter()
                .map(|diag| diag.reconstruction_increment_scale)
                .max_by(|left, right| left.p95.total_cmp(&right.p95))
                .unwrap_or_default(),
        ),
        (
            "ecbeam2_scale_state_terminal_delta_abs",
            diagnostics
                .iter()
                .map(|diag| diag.state_terminal_delta_scale)
                .max_by(|left, right| left.p95.total_cmp(&right.p95))
                .unwrap_or_default(),
        ),
        (
            "ecbeam2_scale_state_barrier_raw",
            diagnostics
                .iter()
                .map(|diag| diag.state_barrier_raw_scale)
                .max_by(|left, right| left.p95.total_cmp(&right.p95))
                .unwrap_or_default(),
        ),
        (
            "ecbeam2_scale_quantizer_error_squared",
            diagnostics
                .iter()
                .map(|diag| diag.quantizer_error_squared_scale)
                .max_by(|left, right| left.p95.total_cmp(&right.p95))
                .unwrap_or_default(),
        ),
    ] {
        notes.push(format!("{prefix}_median={:.12}", distribution.median));
        notes.push(format!("{prefix}_p95={:.12}", distribution.p95));
        notes.push(format!("{prefix}_p99={:.12}", distribution.p99));
        notes.push(format!("{prefix}_max={:.12}", distribution.maximum));
    }
    notes.push(format!(
        "ecbeam2_all_nonfinite_resets={}",
        sum_u64(|diag| diag.all_nonfinite_resets)
    ));
    notes.push(format!(
        "ecbeam2_observer_desynchronizations={}",
        sum_u64(|diag| diag.observer_desynchronizations)
    ));
    notes.push(format!(
        "ecbeam2_invalid_input_substitutions={}",
        sum_u64(|diag| diag.invalid_input_substitutions)
    ));
    notes.push(format!(
        "ecbeam2_output_length_error={}",
        sum_u64(|diag| diag.output_length_events)
            .wrapping_add(renderer.truncation_telemetry().events)
    ));
    let truncation = renderer.truncation_telemetry();
    notes.push(format!(
        "ecbeam2_renderer_truncation_events={}",
        truncation.events
    ));
    notes.push(format!(
        "ecbeam2_renderer_discarded_left_bits={}",
        truncation.discarded_left_bits
    ));
    notes.push(format!(
        "ecbeam2_renderer_discarded_right_bits={}",
        truncation.discarded_right_bits
    ));
    notes.push(format!(
        "ecbeam2_committed_output_energy={:.12}",
        sum_f64(|diag| diag.committed_output_energy)
    ));
    if committed_samples > 0 {
        notes.push(format!(
            "ecbeam2_committed_output_energy_mean={:.12}",
            sum_f64(|diag| diag.committed_output_energy) / committed_samples as f64
        ));
    }
    notes.push(format!(
        "ecbeam2_committed_tail_adjusted_energy={:.12}",
        sum_f64(|diag| diag.committed_tail_adjusted_energy)
    ));
    notes.push(format!(
        "ecbeam2_remaining_tail_energy={:.12}",
        if diagnostic_window_enabled {
            sum_f64(|diag| diag.diagnostic_window_remaining_tail_energy)
        } else {
            sum_f64(|diag| diag.remaining_tail_energy)
        }
    ));
    notes.push(format!(
        "ecbeam2_maximum_tail_energy={:.12}",
        max_f64(|diag| diag.maximum_tail_energy)
    ));
    notes.push(format!(
        "ecbeam2_committed_ultrasonic_energy={:.12}",
        sum_f64(|diag| diag.committed_ultrasonic_energy)
    ));
    notes.push(format!(
        "ecbeam2_maximum_ultrasonic_power={:.12}",
        max_f64(|diag| diag.maximum_ultrasonic_power)
    ));
    notes.push(format!(
        "ecbeam2_ultrasonic_ema_max={:.12}",
        max_f64(|diag| diag.maximum_ultrasonic_ema)
    ));
    notes.push(format!(
        "ecbeam2_signed_error_ema_abs_max={:.12}",
        max_f64(|diag| diag.maximum_signed_error_ema)
    ));
    notes.push(format!(
        "ecbeam2_ultrasonic_ema_p99_9={:.12}",
        max_f64(|diag| diag.ultrasonic_ema_p999)
    ));
    notes.push(format!(
        "ecbeam2_ultrasonic_ema_p99_99={:.12}",
        max_f64(|diag| diag.ultrasonic_ema_p9999)
    ));
    notes.push(format!(
        "ecbeam2_signed_error_ema_abs_p99_9={:.12}",
        max_f64(|diag| diag.signed_error_ema_abs_p999)
    ));
    notes.push(format!(
        "ecbeam2_signed_error_ema_abs_p99_99={:.12}",
        max_f64(|diag| diag.signed_error_ema_abs_p9999)
    ));
    notes.push(format!(
        "ecbeam2_a1_frontier_ultrasonic_ema_max={:.12}",
        max_f64(|diag| diag.a1_frontier_maximum_ultrasonic_ema)
    ));
    notes.push(format!(
        "ecbeam2_a1_frontier_signed_error_ema_abs_max={:.12}",
        max_f64(|diag| diag.a1_frontier_maximum_signed_error_ema)
    ));
    notes.push(format!(
        "ecbeam2_a1_frontier_ultrasonic_ema_p99_9={:.12}",
        max_f64(|diag| diag.a1_frontier_ultrasonic_ema_p999)
    ));
    notes.push(format!(
        "ecbeam2_a1_frontier_ultrasonic_ema_p99_99={:.12}",
        max_f64(|diag| diag.a1_frontier_ultrasonic_ema_p9999)
    ));
    notes.push(format!(
        "ecbeam2_a1_frontier_signed_error_ema_abs_p99_9={:.12}",
        max_f64(|diag| diag.a1_frontier_signed_error_ema_abs_p999)
    ));
    notes.push(format!(
        "ecbeam2_a1_frontier_signed_error_ema_abs_p99_99={:.12}",
        max_f64(|diag| diag.a1_frontier_signed_error_ema_abs_p9999)
    ));
    notes.push(format!(
        "ecbeam2_reconstruction_1ms_ema_max={:.12}",
        max_f64(|diag| diag.maximum_reconstruction_1ms_ema)
    ));
    notes.push(format!(
        "ecbeam2_reconstruction_10ms_ema_max={:.12}",
        max_f64(|diag| diag.maximum_reconstruction_10ms_ema)
    ));
    notes.push(format!(
        "ecbeam2_reconstruction_1ms_energy_max={:.12}",
        max_f64(|diag| diag.maximum_reconstruction_1ms_energy)
    ));
    notes.push(format!(
        "ecbeam2_reconstruction_10ms_energy_max={:.12}",
        max_f64(|diag| diag.maximum_reconstruction_10ms_energy)
    ));
    notes.push(format!(
        "ecbeam2_peak_causal_filtered_error={:.12}",
        max_f64(|diag| diag.maximum_abs_reconstruction_output)
    ));
    notes.push(format!(
        "ecbeam2_best_fourth_margin_min={:.12}",
        diagnostics
            .iter()
            .filter(|diag| diag.best_fourth_margin_samples > 0)
            .map(|diag| diag.minimum_best_fourth_margin)
            .reduce(f64::min)
            .unwrap_or_default()
    ));
    notes.push(format!(
        "ecbeam2_best_fourth_margin_max={:.12}",
        max_f64(|diag| diag.maximum_best_fourth_margin)
    ));
    notes.push(format!(
        "ecbeam2_a1_best_fourth_margin_min={:.12}",
        diagnostics
            .iter()
            .filter(|diag| diag.a1_best_fourth_margin_samples > 0)
            .map(|diag| diag.a1_minimum_best_fourth_margin)
            .reduce(f64::min)
            .unwrap_or_default()
    ));
    notes.push(format!(
        "ecbeam2_a1_best_fourth_margin_max={:.12}",
        max_f64(|diag| diag.a1_maximum_best_fourth_margin)
    ));
    notes.push(format!(
        "ecbeam2_predicted_segments_recorded={}",
        sum_u64(|diag| diag.predicted_segments_recorded)
    ));
    notes.push(format!(
        "ecbeam2_matched_complete_segments={}",
        sum_u64(|diag| diag.matched_complete_segments)
    ));
    notes.push(format!(
        "ecbeam2_changed_before_commit_segments={}",
        sum_u64(|diag| diag.changed_before_commit_segments)
    ));
    notes.push(format!(
        "ecbeam2_segment_identity_error_max={:.12}",
        max_f64(|diag| diag.maximum_segment_identity_error)
    ));
    notes.push(format!(
        "ecbeam2_committed_sequence={}",
        diagnostics
            .iter()
            .map(|diag| diag.committed_sequence)
            .max()
            .unwrap_or_default()
    ));
    notes.push(format!(
        "ecbeam2_committed_state_epoch={}",
        diagnostics
            .iter()
            .map(|diag| diag.committed_state_epoch)
            .max()
            .unwrap_or_default()
    ));
}

// Audio quality cases spell out filter, rate, modulator, and probe switches in test matrices.
#[allow(clippy::too_many_arguments)]
fn measure_dsd_case(
    name: &str,
    filter: FilterType,
    dsd_rate: DsdRate,
    source_rate: u32,
    dsd_modulator: DsdModulator,
    run_stress: bool,
    run_artifact_probes: bool,
    seconds: f64,
    config: DsdExperimentConfig,
) -> DsdMeasurement {
    measure_dsd_case_with_path(
        name,
        filter,
        dsd_rate,
        DsdPcmPath::direct(source_rate),
        dsd_modulator,
        run_stress,
        run_artifact_probes,
        seconds,
        config,
    )
}

// Audio quality cases spell out filter, path, modulator, and probe switches in test matrices.
#[allow(clippy::too_many_arguments)]
fn measure_dsd_case_with_path(
    name: &str,
    filter: FilterType,
    dsd_rate: DsdRate,
    path: DsdPcmPath,
    dsd_modulator: DsdModulator,
    run_stress: bool,
    run_artifact_probes: bool,
    seconds: f64,
    config: DsdExperimentConfig,
) -> DsdMeasurement {
    let mut notes = Vec::new();
    let tweaks = production_tweaks_for(config, filter, dsd_rate, dsd_modulator);
    let modulator = dsd_modulator_measurement_name(dsd_modulator, tweaks);
    let source_rate = path.origin_source_rate;
    let renderer_source_rate = path.renderer_source_rate;
    let wire_rate = dsd_rate.wire_rate_for_source(renderer_source_rate);
    let Some(wire_rate) = wire_rate else {
        return DsdMeasurement {
            modulator,
            filter: name.to_string(),
            path_variant: path.path_variant.to_string(),
            source_rate,
            origin_source_rate: path.origin_source_rate,
            renderer_source_rate,
            intermediate_rate: path.intermediate_rate,
            intermediate_bits: path.intermediate_bits,
            intermediate_filter: path.intermediate_filter_name().map(str::to_string),
            path_prepare_ms: None,
            render_ms: None,
            dsd_rate: dsd_rate_name(dsd_rate).to_string(),
            wire_rate: None,
            passband_profile: PassbandProfile::default(),
            residual_db: None,
            thdn_residual_db: None,
            inband_snr_db: None,
            inband_snr_worst_db: None,
            inband_snr_p05_db: None,
            inband_snr_p95_db: None,
            inband_snr_best_db: None,
            inband_snr_spread_db: None,
            inband_snr_left_db: None,
            inband_snr_right_db: None,
            inband_snr_left_worst_db: None,
            inband_snr_right_worst_db: None,
            inband_snr_left_spread_db: None,
            inband_snr_right_spread_db: None,
            inband_lf_sinad_worst_db: None,
            inband_lf_sinad_db: None,
            inband_lf_tone_hz: None,
            stereo_snr_worst_mismatch_db: None,
            inband_snr_window_count: None,
            inband_snr_worst_window_start_s: None,
            inband_noise_rms_dbfs: None,
            inband_noise_worst_rms_dbfs: None,
            inband_noise_peak_dbfs: None,
            inband_noise_peak_spur_hz: None,
            inband_noise_spur_margin_db: None,
            inband_noise_left_spur_margin_db: None,
            inband_noise_right_spur_margin_db: None,
            inband_noise_20_200_dbfs: None,
            inband_noise_200_2k_dbfs: None,
            inband_noise_2k_8k_dbfs: None,
            inband_noise_8k_16k_dbfs: None,
            inband_noise_16k_20k_dbfs: None,
            ultrasonic_24_50k_max_dbfs: None,
            ultrasonic_24_50k_median_dbfs: None,
            ultrasonic_24_50k_window_spread_db: None,
            ultrasonic_50_100k_max_dbfs: None,
            ultrasonic_50_100k_median_dbfs: None,
            ultrasonic_50_100k_window_spread_db: None,
            ultrasonic_100_200k_max_dbfs: None,
            ultrasonic_100_200k_median_dbfs: None,
            ultrasonic_100_200k_window_spread_db: None,
            inband_spurs: Vec::new(),
            inband_windows: Vec::new(),
            ultrasonic_windows: Vec::new(),
            premod_windows: Vec::new(),
            idle_tone_dbfs: None,
            idle_worst_tone_dbfs: None,
            idle_worst_density_deviation: None,
            idle_artifacts: Vec::new(),
            overload_recovery_diagnostics: Vec::new(),
            low_level_worst_residual_db: None,
            low_level_worst_spur_dbfs: None,
            high_freq_tone_worst_residual_db: None,
            high_freq_tone_worst_spur_dbfs: None,
            high_freq_imd_residual_db: None,
            high_freq_imd_spur_dbfs: None,
            high_freq_worst_residual_db: None,
            high_freq_worst_spur_dbfs: None,
            multitone_residual_db: None,
            multitone_spur_dbfs: None,
            overload_recovery_dbfs: None,
            transient_click_candidates: None,
            transient_click_max_score: None,
            transient_click_max_residual: None,
            program_click_candidates: None,
            program_click_max_score: None,
            program_click_max_residual: None,
            decoded_low: None,
            decoded_peak: None,
            decoded_abs_peak: None,
            bit_density: None,
            bit_density_left: None,
            bit_density_right: None,
            bit_density_max_deviation: None,
            bit_density_left_max_deviation: None,
            bit_density_right_max_deviation: None,
            transition_rate: None,
            limiter_peak_ratio_max: None,
            limiter_current_block_peak_ratio: None,
            limiter_current_block_gain: None,
            limiter_current_block_limited_samples: 0,
            limiter_limited_events: 0,
            limiter_limited_samples: 0,
            stability_resets: 0,
            state_clamps: 0,
            stress_stability_resets: 0,
            stress_state_clamps: 0,
            depth4_ratio: None,
            adaptive_decision_trace: None,
            ec2_decision_trace: None,
            dsd256_improvement_db: None,
            notes: vec!["unsupported source/wire-rate combination".to_string()],
        };
    };

    let frames = dsd_path_measurement_frames(filter, dsd_rate, path, seconds);
    let tone_fft_bits = dsd_tone_window_fft_bits(dsd_rate, seconds);
    let analysis_fft_bits = dsd_analysis_fft_bits(seconds);
    let tone_freq = coherent_dsd_tone_hz(wire_rate, tone_fft_bits, 1_000.0);
    let input = sine(frames, source_rate, tone_freq, 0.35);
    notes.push(format!("path_variant={}", path.path_variant));
    notes.push(format!("origin_source_rate={}", path.origin_source_rate));
    notes.push(format!("renderer_source_rate={renderer_source_rate}"));
    notes.push("inband_measurement=prefilter-24k".to_string());
    if let Some(rate) = path.intermediate_rate {
        notes.push(format!("intermediate_rate={rate}"));
    }
    if let Some(bits) = path.intermediate_bits {
        notes.push(format!("intermediate_bits={bits}"));
    }
    if let Some(filter_name) = path.intermediate_filter_name() {
        notes.push(format!("intermediate_filter={filter_name}"));
    }
    if let Some((label, coeffs)) = config.ec_coeff_for(dsd_rate, dsd_modulator) {
        notes.push(format!(
            "ec_coeff_variant={label} obg={:.2} input_peak={:.3}",
            coeffs.obg, coeffs.input_peak
        ));
    }
    let rate_note = dsd_rate_name(dsd_rate).to_ascii_lowercase();
    if let Some(multiplier) = tweaks.ec_dither_scale_multiplier {
        notes.push(format!(
            "{rate_note}_ec_dither_scale_multiplier={multiplier:.6}"
        ));
    }
    if let Some(shape) = tweaks.ec_dither_shape {
        notes.push(format!("{rate_note}_ec_dither_shape={shape:?}"));
    }
    if let Some(prng) = tweaks.ec_dither_prng {
        notes.push(format!("{rate_note}_ec_dither_prng={prng:?}"));
    }
    if let Some(alpha) = tweaks.ec_dither_leak_alpha {
        notes.push(format!("{rate_note}_ec_dither_leak_alpha={alpha:.6}"));
    }
    if let Some(gamma) = tweaks.ec_dither_lf_floor_gamma {
        notes.push(format!("{rate_note}_ec_dither_lf_floor_gamma={gamma:.6}"));
    }
    if let Some(common_side) = tweaks.ec_common_side_dither {
        notes.push(format!(
            "{rate_note}_ec_common_side_dither_beta={:.6}",
            common_side.beta
        ));
        notes.push(format!(
            "{rate_note}_ec_common_side_common_seed=0x{:016x}",
            common_side.common_seed
        ));
        notes.push(format!(
            "{rate_note}_ec_common_side_side_seed=0x{:016x}",
            common_side.side_seed
        ));
    }
    if let Some(scorer) = tweaks.ec_future_scorer {
        notes.push(format!("{rate_note}_ec_future_scorer={scorer:?}"));
        let effective = scorer.effective_for_osr(dsd_rate.oversample());
        if effective != scorer {
            notes.push(format!(
                "{rate_note}_ec_future_scorer_effective={effective:?}"
            ));
        }
    }
    let beam_active = tweaks.ec_beam_search.is_some();
    if let Some(policy) = tweaks.ec2_long_filter_policy.filter(|_| !beam_active) {
        notes.push(format!(
            "{rate_note}_ec2_long_filter_policy={}",
            policy.as_name()
        ));
    }
    if let Some(weights) = tweaks.ec2_policy_weights {
        let prefix = if beam_active {
            format!("{rate_note}_beam")
        } else {
            format!("{rate_note}_ec2")
        };
        notes.push(format!(
            "{prefix}_quantizer_weight={:.6}",
            weights.quantizer_weight
        ));
        notes.push(format!(
            "{prefix}_pressure_weight={:.6}",
            weights.pressure_weight
        ));
        notes.push(format!("{prefix}_limit_weight={:.6}", weights.limit_weight));
        notes.push(format!(
            "{prefix}_transition_weight={:.6}",
            weights.transition_weight
        ));
        notes.push(format!("{prefix}_dc_weight={:.6}", weights.dc_weight));
        if !beam_active {
            notes.push(format!(
                "{prefix}_lookahead_discount={:.6}",
                weights.lookahead_discount
            ));
            notes.push(format!(
                "{prefix}_ambiguity_margin={:.6}",
                weights.ambiguity_margin
            ));
            notes.push(format!(
                "{prefix}_pressure_taper_start={:.6}",
                weights.pressure_taper_start
            ));
            notes.push(format!(
                "{prefix}_pressure_taper_strength={:.6}",
                weights.pressure_taper_strength
            ));
        }
    }
    if let Some(weights) = tweaks.ec2_pressure_stage_weights {
        let prefix = if beam_active {
            format!("{rate_note}_beam_pressure_stage_weights")
        } else {
            format!("{rate_note}_ec2_pressure_stage_weights")
        };
        notes.push(format!("{prefix}={}", stage_weight_label(&weights)));
    }
    if let Some(window_bits) = tweaks.ec2_decision_trace_window_bits {
        notes.push(format!("ec2_decision_trace_window_bits={window_bits}"));
    }
    if let Some((m, n)) = tweaks.ec_beam_search {
        notes.push(format!("{rate_note}_ec_beam_m={m}"));
        notes.push(format!("{rate_note}_ec_beam_n={n}"));
    }
    if let Some(weight) = tweaks.ec_beam_terminal_weight {
        notes.push(format!("{rate_note}_beam_terminal_weight={weight:.6}"));
    }
    if let Some(weight) = tweaks.ec_beam_alternation_weight {
        notes.push(format!("{rate_note}_beam_alternation_weight={weight:.6}"));
    }
    if let Some(weight) = tweaks.ec_beam_alternation_rank_weight {
        notes.push(format!(
            "{rate_note}_beam_alternation_rank_weight={weight:.6}"
        ));
    }
    if let Some(threshold) = tweaks.ec_beam_alternation_threshold {
        notes.push(format!(
            "{rate_note}_beam_alternation_threshold={threshold:.6}"
        ));
    }
    if let Some(weight) = tweaks.ec_beam_periodicity_weight {
        notes.push(format!("{rate_note}_beam_periodicity_weight={weight:.6}"));
    }
    if let (Some(lags), Some(count)) = (
        tweaks.ec_beam_periodicity_lags,
        tweaks.ec_beam_periodicity_lag_count,
    ) {
        let label = lags[..count.min(lags.len())]
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join("-");
        notes.push(format!("{rate_note}_beam_periodicity_lags={label}"));
    }
    if let Some(window) = tweaks.ec_beam_periodicity_window {
        notes.push(format!("{rate_note}_beam_periodicity_window={window}"));
    }
    if let Some(weight) = tweaks.ec_beam_filtered_error_weight {
        notes.push(format!(
            "{rate_note}_beam_filtered_error_weight={weight:.6}"
        ));
    }
    if let Some(weight) = tweaks.ec_beam_filtered_error_rank_weight {
        notes.push(format!(
            "{rate_note}_beam_filtered_error_rank_weight={weight:.6}"
        ));
    }
    if let Some(weight) = tweaks.ec_beam_reconstruction_error_weight {
        notes.push(format!(
            "{rate_note}_beam_reconstruction_error_weight={weight:.6}"
        ));
    }
    if let Some(deadzone) = tweaks.ec_beam_pressure_deadzone {
        notes.push(format!("{rate_note}_beam_pressure_deadzone={deadzone:.6}"));
    }
    if let Some(value) = tweaks.ec_beam_pressure_accum_scale {
        notes.push(format!("{rate_note}_beam_pressure_accum_scale={value:.6}"));
    }
    if let Some(value) = tweaks.ec_beam_pressure_rank_scale {
        notes.push(format!("{rate_note}_beam_pressure_rank_scale={value:.6}"));
    }
    if let Some(value) = tweaks.ec_beam_dc_accum_scale {
        notes.push(format!("{rate_note}_beam_dc_accum_scale={value:.6}"));
    }
    if let Some(value) = tweaks.ec_beam_dc_rank_scale {
        notes.push(format!("{rate_note}_beam_dc_rank_scale={value:.6}"));
    }
    if let Some(enabled) = tweaks.ec_beam_metric_diagnostics {
        notes.push(format!("{rate_note}_beam_metric_diagnostics={enabled}"));
    }
    if let Some(allow) = tweaks
        .ec4a_allow_predictive_triggers
        .filter(|_| dsd_modulator.is_adaptive())
    {
        notes.push(format!(
            "{rate_note}_ec4a_allow_predictive_triggers={allow}"
        ));
    }
    if tweaks.ec4a_dsd128_quality_pressure && dsd_modulator.is_adaptive() {
        notes.push(format!("{rate_note}_ec4a_quality_pressure=true"));
    }
    if let Some(threshold) = tweaks
        .ec4a_dsd128_quality_pressure_threshold
        .filter(|_| dsd_modulator.is_adaptive())
    {
        notes.push(format!(
            "{rate_note}_ec4a_quality_pressure_threshold={threshold:.6}"
        ));
    }
    if let Some(hold) = tweaks
        .ec4a_dsd128_quality_pressure_hold
        .filter(|_| dsd_modulator.is_adaptive())
    {
        notes.push(format!("{rate_note}_ec4a_quality_pressure_hold={hold}"));
    }
    if let Some(window_bits) = tweaks
        .ec4a_decision_trace_window_bits
        .filter(|_| dsd_modulator.is_adaptive())
    {
        notes.push(format!("ec4a_decision_trace_window_bits={window_bits}"));
    }
    if let Some(seed) = tweaks.seed_left {
        notes.push(format!("dsd_seed_left=0x{seed:016x}"));
    }
    if let Some(seed) = tweaks.seed_right {
        notes.push(format!("dsd_seed_right=0x{seed:016x}"));
    }
    if let Some(config) = tweaks.ecbeam2_config {
        notes.push(format!(
            "ecbeam2_state_terminal_weight={:.12}",
            config.state_terminal_weight
        ));
        notes.push(format!(
            "ecbeam2_state_barrier_knee={:.12}",
            config.state_deadzone
        ));
        notes.push(format!(
            "ecbeam2_state_barrier_weight={:.12}",
            config.state_deadzone_weight
        ));
        notes.push(format!(
            "ecbeam2_quantizer_regularizer={:.12}",
            config.quantizer_regularizer
        ));
    }
    if tweaks.input_gain_db != 0.0 {
        notes.push(format!(
            "{rate_note}_input_gain_db={:.3}",
            tweaks.input_gain_db
        ));
    }
    let prepare_start = Instant::now();
    let renderer_input = match path.prepare_renderer_input(filter, &input) {
        Ok(input) => input,
        Err(err) => {
            let path_prepare_ms = Some(prepare_start.elapsed().as_secs_f64() * 1000.0);
            return DsdMeasurement {
                modulator,
                filter: name.to_string(),
                path_variant: path.path_variant.to_string(),
                source_rate,
                origin_source_rate: path.origin_source_rate,
                renderer_source_rate,
                intermediate_rate: path.intermediate_rate,
                intermediate_bits: path.intermediate_bits,
                intermediate_filter: path.intermediate_filter_name().map(str::to_string),
                path_prepare_ms,
                render_ms: None,
                dsd_rate: dsd_rate_name(dsd_rate).to_string(),
                wire_rate: Some(wire_rate),
                passband_profile: PassbandProfile::default(),
                residual_db: None,
                thdn_residual_db: None,
                inband_snr_db: None,
                inband_snr_worst_db: None,
                inband_snr_p05_db: None,
                inband_snr_p95_db: None,
                inband_snr_best_db: None,
                inband_snr_spread_db: None,
                inband_snr_left_db: None,
                inband_snr_right_db: None,
                inband_snr_left_worst_db: None,
                inband_snr_right_worst_db: None,
                inband_snr_left_spread_db: None,
                inband_snr_right_spread_db: None,
                inband_lf_sinad_worst_db: None,
                inband_lf_sinad_db: None,
                inband_lf_tone_hz: None,
                stereo_snr_worst_mismatch_db: None,
                inband_snr_window_count: None,
                inband_snr_worst_window_start_s: None,
                inband_noise_rms_dbfs: None,
                inband_noise_worst_rms_dbfs: None,
                inband_noise_peak_dbfs: None,
                inband_noise_peak_spur_hz: None,
                inband_noise_spur_margin_db: None,
                inband_noise_left_spur_margin_db: None,
                inband_noise_right_spur_margin_db: None,
                inband_noise_20_200_dbfs: None,
                inband_noise_200_2k_dbfs: None,
                inband_noise_2k_8k_dbfs: None,
                inband_noise_8k_16k_dbfs: None,
                inband_noise_16k_20k_dbfs: None,
                ultrasonic_24_50k_max_dbfs: None,
                ultrasonic_24_50k_median_dbfs: None,
                ultrasonic_24_50k_window_spread_db: None,
                ultrasonic_50_100k_max_dbfs: None,
                ultrasonic_50_100k_median_dbfs: None,
                ultrasonic_50_100k_window_spread_db: None,
                ultrasonic_100_200k_max_dbfs: None,
                ultrasonic_100_200k_median_dbfs: None,
                ultrasonic_100_200k_window_spread_db: None,
                inband_spurs: Vec::new(),
                inband_windows: Vec::new(),
                ultrasonic_windows: Vec::new(),
                premod_windows: Vec::new(),
                idle_tone_dbfs: None,
                idle_worst_tone_dbfs: None,
                idle_worst_density_deviation: None,
                idle_artifacts: Vec::new(),
                overload_recovery_diagnostics: Vec::new(),
                low_level_worst_residual_db: None,
                low_level_worst_spur_dbfs: None,
                high_freq_tone_worst_residual_db: None,
                high_freq_tone_worst_spur_dbfs: None,
                high_freq_imd_residual_db: None,
                high_freq_imd_spur_dbfs: None,
                high_freq_worst_residual_db: None,
                high_freq_worst_spur_dbfs: None,
                multitone_residual_db: None,
                multitone_spur_dbfs: None,
                overload_recovery_dbfs: None,
                transient_click_candidates: None,
                transient_click_max_score: None,
                transient_click_max_residual: None,
                program_click_candidates: None,
                program_click_max_score: None,
                program_click_max_residual: None,
                decoded_low: None,
                decoded_peak: None,
                decoded_abs_peak: None,
                bit_density: None,
                bit_density_left: None,
                bit_density_right: None,
                bit_density_max_deviation: None,
                bit_density_left_max_deviation: None,
                bit_density_right_max_deviation: None,
                transition_rate: None,
                limiter_peak_ratio_max: None,
                limiter_current_block_peak_ratio: None,
                limiter_current_block_gain: None,
                limiter_current_block_limited_samples: 0,
                limiter_limited_events: 0,
                limiter_limited_samples: 0,
                stability_resets: 0,
                state_clamps: 0,
                stress_stability_resets: 0,
                stress_state_clamps: 0,
                depth4_ratio: None,
                adaptive_decision_trace: None,
                ec2_decision_trace: None,
                dsd256_improvement_db: None,
                notes: vec![err],
            };
        }
    };
    let path_prepare_ms = Some(prepare_start.elapsed().as_secs_f64() * 1000.0);
    let mut renderer = match new_dsd_renderer(
        filter,
        renderer_source_rate,
        dsd_rate,
        dsd_modulator,
        config,
    ) {
        Ok(renderer) => renderer,
        Err(err) => {
            return DsdMeasurement {
                modulator,
                filter: name.to_string(),
                path_variant: path.path_variant.to_string(),
                source_rate,
                origin_source_rate: path.origin_source_rate,
                renderer_source_rate,
                intermediate_rate: path.intermediate_rate,
                intermediate_bits: path.intermediate_bits,
                intermediate_filter: path.intermediate_filter_name().map(str::to_string),
                path_prepare_ms,
                render_ms: None,
                dsd_rate: dsd_rate_name(dsd_rate).to_string(),
                wire_rate: Some(wire_rate),
                passband_profile: PassbandProfile::default(),
                residual_db: None,
                thdn_residual_db: None,
                inband_snr_db: None,
                inband_snr_worst_db: None,
                inband_snr_p05_db: None,
                inband_snr_p95_db: None,
                inband_snr_best_db: None,
                inband_snr_spread_db: None,
                inband_snr_left_db: None,
                inband_snr_right_db: None,
                inband_snr_left_worst_db: None,
                inband_snr_right_worst_db: None,
                inband_snr_left_spread_db: None,
                inband_snr_right_spread_db: None,
                inband_lf_sinad_worst_db: None,
                inband_lf_sinad_db: None,
                inband_lf_tone_hz: None,
                stereo_snr_worst_mismatch_db: None,
                inband_snr_window_count: None,
                inband_snr_worst_window_start_s: None,
                inband_noise_rms_dbfs: None,
                inband_noise_worst_rms_dbfs: None,
                inband_noise_peak_dbfs: None,
                inband_noise_peak_spur_hz: None,
                inband_noise_spur_margin_db: None,
                inband_noise_left_spur_margin_db: None,
                inband_noise_right_spur_margin_db: None,
                inband_noise_20_200_dbfs: None,
                inband_noise_200_2k_dbfs: None,
                inband_noise_2k_8k_dbfs: None,
                inband_noise_8k_16k_dbfs: None,
                inband_noise_16k_20k_dbfs: None,
                ultrasonic_24_50k_max_dbfs: None,
                ultrasonic_24_50k_median_dbfs: None,
                ultrasonic_24_50k_window_spread_db: None,
                ultrasonic_50_100k_max_dbfs: None,
                ultrasonic_50_100k_median_dbfs: None,
                ultrasonic_50_100k_window_spread_db: None,
                ultrasonic_100_200k_max_dbfs: None,
                ultrasonic_100_200k_median_dbfs: None,
                ultrasonic_100_200k_window_spread_db: None,
                inband_spurs: Vec::new(),
                inband_windows: Vec::new(),
                ultrasonic_windows: Vec::new(),
                premod_windows: Vec::new(),
                idle_tone_dbfs: None,
                idle_worst_tone_dbfs: None,
                idle_worst_density_deviation: None,
                idle_artifacts: Vec::new(),
                overload_recovery_diagnostics: Vec::new(),
                low_level_worst_residual_db: None,
                low_level_worst_spur_dbfs: None,
                high_freq_tone_worst_residual_db: None,
                high_freq_tone_worst_spur_dbfs: None,
                high_freq_imd_residual_db: None,
                high_freq_imd_spur_dbfs: None,
                high_freq_worst_residual_db: None,
                high_freq_worst_spur_dbfs: None,
                multitone_residual_db: None,
                multitone_spur_dbfs: None,
                overload_recovery_dbfs: None,
                transient_click_candidates: None,
                transient_click_max_score: None,
                transient_click_max_residual: None,
                program_click_candidates: None,
                program_click_max_score: None,
                program_click_max_residual: None,
                decoded_low: None,
                decoded_peak: None,
                decoded_abs_peak: None,
                bit_density: None,
                bit_density_left: None,
                bit_density_right: None,
                bit_density_max_deviation: None,
                bit_density_left_max_deviation: None,
                bit_density_right_max_deviation: None,
                transition_rate: None,
                limiter_peak_ratio_max: None,
                limiter_current_block_peak_ratio: None,
                limiter_current_block_gain: None,
                limiter_current_block_limited_samples: 0,
                limiter_limited_events: 0,
                limiter_limited_samples: 0,
                stability_resets: 0,
                state_clamps: 0,
                stress_stability_resets: 0,
                stress_state_clamps: 0,
                depth4_ratio: None,
                adaptive_decision_trace: None,
                ec2_decision_trace: None,
                dsd256_improvement_db: None,
                notes: vec![err.to_string()],
            };
        }
    };
    renderer.set_native_order(NativeDsdOrder::MsbFirst);
    let render_start = Instant::now();
    let (native_l, native_r) =
        render_native_stream(&mut renderer, &renderer_input, &renderer_input);
    let render_ms = render_start.elapsed().as_secs_f64() * 1000.0;
    if let Err(err) = maybe_dump_dsd_bitstream(DsdBitDumpRequest {
        filter: name,
        modulator: &modulator,
        path_variant: path.path_variant,
        source_rate,
        origin_source_rate: path.origin_source_rate,
        renderer_source_rate,
        dsd_rate,
        wire_rate,
        tone_hz: tone_freq,
        tone_fft_bits,
        left: &native_l,
        right: &native_r,
    }) {
        notes.push(format!("dsd_bit_dump_error={err}"));
    }
    let bits_l = unpack_native_msb(&native_l);
    let bits_r = unpack_native_msb(&native_r);
    let stability_resets = renderer.stability_resets();
    let state_clamps = renderer.state_clamps();
    let limiter_telemetry = renderer.limiter_telemetry();
    append_beam_diagnostics_notes(&mut notes, &renderer);
    let depth4_ratio = dsd_modulator.is_adaptive().then(|| renderer.depth4_ratio());
    let adaptive_decision_trace = dsd_modulator
        .is_adaptive()
        .then(|| DsdAdaptiveDecisionTrace::from_renderer(&renderer))
        .flatten();
    let bit_density_left = bit_density(&bits_l);
    let bit_density_right = bit_density(&bits_r);
    let bit_density = match (bit_density_left, bit_density_right) {
        (Some(left), Some(right)) => Some((left + right) * 0.5),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    };
    let transition_rate = max_opt(transition_rate(&bits_l), transition_rate(&bits_r));
    let ratio = (wire_rate / source_rate) as usize;
    let decoded_l = decimate_dsd_bits(&bits_l, ratio);
    let decoded_r = decimate_dsd_bits(&bits_r, ratio);
    let decoded_low = min_opt(sample_low(&decoded_l), sample_low(&decoded_r));
    let decoded_peak = max_opt(sample_peak(&decoded_l), sample_peak(&decoded_r));
    let decoded_abs_peak = max_opt(sample_abs_peak(&decoded_l), sample_abs_peak(&decoded_r));
    let inband_metrics =
        dsd_stereo_inband_tone_metrics(&bits_l, &bits_r, wire_rate, tone_freq, 0.35, tone_fft_bits);
    if inband_metrics.is_none() {
        notes.push("not enough DSD samples for in-band spectral measurement".to_string());
    }
    // Workstream D: optional low-frequency SINAD probe. The 1 kHz SINAD tone
    // sits far above the DC-bias tracker's servo corner, so it is blind to
    // in-band servo damage from corner changes; this dedicated ~100 Hz render
    // exposes it. Gated because it costs a second full render.
    let (inband_lf_sinad_worst_db, inband_lf_sinad_db, inband_lf_tone_hz) = if env_flag_enabled(
        "FOZMO_DSD_MEASURE_LF_SINAD",
    ) {
        match measure_dsd_lf_sinad(
            filter,
            path,
            dsd_rate,
            dsd_modulator,
            config,
            source_rate,
            renderer_source_rate,
            wire_rate,
            frames,
            tone_fft_bits,
        ) {
            Some((worst, median, tone_hz)) => {
                notes.push(format!(
                        "lf_sinad_tone_hz={tone_hz:.3} lf_sinad_worst_db={worst:.2} lf_sinad_db={median:.2}"
                    ));
                (Some(worst), Some(median), Some(tone_hz))
            }
            None => {
                notes.push("lf_sinad: not enough DSD samples".to_string());
                (None, None, None)
            }
        }
    } else {
        (None, None, None)
    };
    let aggregate_metrics = inband_metrics.as_ref().map(|metrics| &metrics.aggregate);
    let left_metrics = inband_metrics.as_ref().map(|metrics| &metrics.left);
    let right_metrics = inband_metrics.as_ref().map(|metrics| &metrics.right);
    let residual_db = aggregate_metrics.map(|metrics| metrics.residual_db);
    let thdn_residual_db = residual_db;
    let inband_snr_db = aggregate_metrics.map(|metrics| metrics.sinad_db);
    let inband_snr_worst_db = aggregate_metrics.map(|metrics| metrics.sinad_worst_db);
    let inband_snr_p05_db = aggregate_metrics.map(|metrics| metrics.sinad_p05_db);
    let inband_snr_p95_db = aggregate_metrics.map(|metrics| metrics.sinad_p95_db);
    let inband_snr_best_db = aggregate_metrics.map(|metrics| metrics.sinad_best_db);
    let inband_snr_spread_db = aggregate_metrics.map(|metrics| metrics.sinad_spread_db);
    let inband_snr_left_db = left_metrics.map(|metrics| metrics.sinad_db);
    let inband_snr_right_db = right_metrics.map(|metrics| metrics.sinad_db);
    let inband_snr_left_worst_db = left_metrics.map(|metrics| metrics.sinad_worst_db);
    let inband_snr_right_worst_db = right_metrics.map(|metrics| metrics.sinad_worst_db);
    let inband_snr_left_spread_db = left_metrics.map(|metrics| metrics.sinad_spread_db);
    let inband_snr_right_spread_db = right_metrics.map(|metrics| metrics.sinad_spread_db);
    let stereo_snr_worst_mismatch_db = inband_metrics
        .as_ref()
        .and_then(|metrics| metrics.worst_sinad_mismatch_db);
    let inband_snr_window_count = aggregate_metrics.map(|metrics| metrics.window_count);
    let inband_snr_worst_window_start_s =
        aggregate_metrics.and_then(|metrics| metrics.worst_window_start_s);
    let inband_noise_rms_dbfs = aggregate_metrics.map(|metrics| metrics.noise_rms_dbfs);
    let inband_noise_worst_rms_dbfs = aggregate_metrics.map(|metrics| metrics.noise_worst_rms_dbfs);
    let inband_noise_peak_dbfs = aggregate_metrics.and_then(|metrics| metrics.peak_spur_dbfs);
    let inband_noise_peak_spur_hz = aggregate_metrics.and_then(|metrics| metrics.peak_spur_hz);
    let inband_noise_spur_margin_db = aggregate_metrics.and_then(|metrics| metrics.spur_margin_db);
    let inband_noise_left_spur_margin_db = left_metrics.and_then(|metrics| metrics.spur_margin_db);
    let inband_noise_right_spur_margin_db =
        right_metrics.and_then(|metrics| metrics.spur_margin_db);
    let inband_noise_20_200_dbfs = aggregate_metrics.and_then(|metrics| metrics.noise_20_200_dbfs);
    let inband_noise_200_2k_dbfs = aggregate_metrics.and_then(|metrics| metrics.noise_200_2k_dbfs);
    let inband_noise_2k_8k_dbfs = aggregate_metrics.and_then(|metrics| metrics.noise_2k_8k_dbfs);
    let inband_noise_8k_16k_dbfs = aggregate_metrics.and_then(|metrics| metrics.noise_8k_16k_dbfs);
    let inband_noise_16k_20k_dbfs =
        aggregate_metrics.and_then(|metrics| metrics.noise_16k_20k_dbfs);
    let inband_spurs = inband_metrics
        .as_ref()
        .map(dsd_inband_spur_rows)
        .unwrap_or_default();
    let inband_windows = inband_metrics
        .as_ref()
        .map(|metrics| metrics.windows.clone())
        .unwrap_or_default();
    let ultrasonic_metrics = dsd_ultrasonic_metrics(&bits_l, &bits_r, wire_rate, tone_fft_bits);
    let ultrasonic_windows = ultrasonic_metrics.windows;
    let premod_windows = if dsd_premod_windows_enabled() {
        dsd_premod_window_metrics(
            filter,
            dsd_rate,
            renderer_source_rate,
            dsd_modulator,
            &renderer_input,
            wire_rate,
            tone_freq,
            tone_fft_bits,
            inband_snr_worst_window_start_s,
            config,
        )
    } else {
        Vec::new()
    };
    let bit_density_left_max_deviation =
        rolling_bit_density_max_deviation(&bits_l, (wire_rate as usize / 100).max(1024));
    let bit_density_right_max_deviation =
        rolling_bit_density_max_deviation(&bits_r, (wire_rate as usize / 100).max(1024));
    let bit_density_max_deviation = max_opt(
        bit_density_left_max_deviation,
        bit_density_right_max_deviation,
    );

    let silence = vec![0.0; frames];
    let silence_input = path.prepare_renderer_input(filter, &silence).ok();
    let mut silence_renderer = new_dsd_renderer(
        filter,
        renderer_source_rate,
        dsd_rate,
        dsd_modulator,
        config,
    )
    .ok();
    let idle_tone_dbfs = silence_renderer.as_mut().and_then(|renderer| {
        renderer.set_native_order(NativeDsdOrder::MsbFirst);
        let bits = renderer_native_left_bits(renderer, silence_input.as_deref()?);
        dsd_idle_peak_dbfs(&bits, wire_rate, analysis_fft_bits)
    });
    let artifact_metrics = if run_artifact_probes {
        measure_dsd_artifacts(
            filter,
            dsd_rate,
            path,
            dsd_modulator,
            frames,
            wire_rate,
            config,
        )
    } else {
        DsdArtifactMetrics::default()
    };
    let (stress_stability_resets, stress_state_clamps, stress_notes) = if run_stress {
        measure_dsd_stress(filter, dsd_rate, path, dsd_modulator, config)
    } else {
        (0, 0, Vec::new())
    };
    notes.extend(stress_notes);
    notes.extend(artifact_metrics.notes);
    let passband_profile = measure_passband_profile(
        filter,
        RateCase {
            source: renderer_source_rate,
            target: wire_rate,
        },
        seconds,
    );

    DsdMeasurement {
        modulator,
        filter: name.to_string(),
        path_variant: path.path_variant.to_string(),
        source_rate,
        origin_source_rate: path.origin_source_rate,
        renderer_source_rate,
        intermediate_rate: path.intermediate_rate,
        intermediate_bits: path.intermediate_bits,
        intermediate_filter: path.intermediate_filter_name().map(str::to_string),
        path_prepare_ms,
        render_ms: Some(render_ms),
        dsd_rate: dsd_rate_name(dsd_rate).to_string(),
        wire_rate: Some(wire_rate),
        passband_profile,
        residual_db,
        thdn_residual_db,
        inband_snr_db,
        inband_snr_worst_db,
        inband_snr_p05_db,
        inband_snr_p95_db,
        inband_snr_best_db,
        inband_snr_spread_db,
        inband_snr_left_db,
        inband_snr_right_db,
        inband_snr_left_worst_db,
        inband_snr_right_worst_db,
        inband_snr_left_spread_db,
        inband_snr_right_spread_db,
        inband_lf_sinad_worst_db,
        inband_lf_sinad_db,
        inband_lf_tone_hz,
        stereo_snr_worst_mismatch_db,
        inband_snr_window_count,
        inband_snr_worst_window_start_s,
        inband_noise_rms_dbfs,
        inband_noise_worst_rms_dbfs,
        inband_noise_peak_dbfs,
        inband_noise_peak_spur_hz,
        inband_noise_spur_margin_db,
        inband_noise_left_spur_margin_db,
        inband_noise_right_spur_margin_db,
        inband_noise_20_200_dbfs,
        inband_noise_200_2k_dbfs,
        inband_noise_2k_8k_dbfs,
        inband_noise_8k_16k_dbfs,
        inband_noise_16k_20k_dbfs,
        ultrasonic_24_50k_max_dbfs: ultrasonic_metrics.ultrasonic_24_50k_max_dbfs,
        ultrasonic_24_50k_median_dbfs: ultrasonic_metrics.ultrasonic_24_50k_median_dbfs,
        ultrasonic_24_50k_window_spread_db: ultrasonic_metrics.ultrasonic_24_50k_window_spread_db,
        ultrasonic_50_100k_max_dbfs: ultrasonic_metrics.ultrasonic_50_100k_max_dbfs,
        ultrasonic_50_100k_median_dbfs: ultrasonic_metrics.ultrasonic_50_100k_median_dbfs,
        ultrasonic_50_100k_window_spread_db: ultrasonic_metrics.ultrasonic_50_100k_window_spread_db,
        ultrasonic_100_200k_max_dbfs: ultrasonic_metrics.ultrasonic_100_200k_max_dbfs,
        ultrasonic_100_200k_median_dbfs: ultrasonic_metrics.ultrasonic_100_200k_median_dbfs,
        ultrasonic_100_200k_window_spread_db: ultrasonic_metrics
            .ultrasonic_100_200k_window_spread_db,
        inband_spurs,
        inband_windows,
        ultrasonic_windows,
        premod_windows,
        idle_tone_dbfs,
        idle_worst_tone_dbfs: artifact_metrics.idle_worst_tone_dbfs,
        idle_worst_density_deviation: artifact_metrics.idle_worst_density_deviation,
        idle_artifacts: artifact_metrics.idle_artifacts,
        overload_recovery_diagnostics: artifact_metrics.overload_recovery_diagnostics,
        low_level_worst_residual_db: artifact_metrics.low_level_worst_residual_db,
        low_level_worst_spur_dbfs: artifact_metrics.low_level_worst_spur_dbfs,
        high_freq_tone_worst_residual_db: artifact_metrics.high_freq_tone_worst_residual_db,
        high_freq_tone_worst_spur_dbfs: artifact_metrics.high_freq_tone_worst_spur_dbfs,
        high_freq_imd_residual_db: artifact_metrics.high_freq_imd_residual_db,
        high_freq_imd_spur_dbfs: artifact_metrics.high_freq_imd_spur_dbfs,
        high_freq_worst_residual_db: artifact_metrics.high_freq_worst_residual_db,
        high_freq_worst_spur_dbfs: artifact_metrics.high_freq_worst_spur_dbfs,
        multitone_residual_db: artifact_metrics.multitone_residual_db,
        multitone_spur_dbfs: artifact_metrics.multitone_spur_dbfs,
        overload_recovery_dbfs: artifact_metrics.overload_recovery_dbfs,
        transient_click_candidates: artifact_metrics.transient_click_candidates,
        transient_click_max_score: artifact_metrics.transient_click_max_score,
        transient_click_max_residual: artifact_metrics.transient_click_max_residual,
        program_click_candidates: artifact_metrics.program_click_candidates,
        program_click_max_score: artifact_metrics.program_click_max_score,
        program_click_max_residual: artifact_metrics.program_click_max_residual,
        decoded_low,
        decoded_peak,
        decoded_abs_peak,
        bit_density,
        bit_density_left,
        bit_density_right,
        bit_density_max_deviation,
        bit_density_left_max_deviation,
        bit_density_right_max_deviation,
        transition_rate,
        limiter_peak_ratio_max: Some(limiter_telemetry.peak_ratio_max as f64),
        limiter_current_block_peak_ratio: Some(limiter_telemetry.current_block_peak_ratio as f64),
        limiter_current_block_gain: Some(limiter_telemetry.current_block_gain as f64),
        limiter_current_block_limited_samples: limiter_telemetry.current_block_limited_samples,
        limiter_limited_events: limiter_telemetry.limited_events,
        limiter_limited_samples: limiter_telemetry.limited_samples,
        stability_resets,
        state_clamps,
        stress_stability_resets,
        stress_state_clamps,
        depth4_ratio,
        adaptive_decision_trace,
        ec2_decision_trace: DsdEc2DecisionTrace::from_renderer(&renderer),
        dsd256_improvement_db: None,
        notes,
    }
}

fn dsd_measurement_frames(
    filter: FilterType,
    dsd_rate: DsdRate,
    source_rate: u32,
    seconds: f64,
) -> usize {
    dsd_path_measurement_frames(filter, dsd_rate, DsdPcmPath::direct(source_rate), seconds)
}

fn dsd_path_measurement_frames(
    filter: FilterType,
    dsd_rate: DsdRate,
    path: DsdPcmPath,
    seconds: f64,
) -> usize {
    let source_rate = path.origin_source_rate;
    let frames = (source_rate as f64 * seconds).round() as usize;
    let Some(wire_rate) = dsd_rate.wire_rate_for_source(path.renderer_source_rate) else {
        return frames;
    };
    let ratio = (wire_rate / source_rate) as usize;
    let analysis_frames = (dsd_analysis_settle_bits(wire_rate, seconds)
        + dsd_tone_window_fft_bits(dsd_rate, seconds))
    .div_ceil(ratio);
    let latency_frames = dsd_path_latency_frames(filter, dsd_rate, path);
    let minimum_frames = latency_frames + analysis_frames + CHUNK_FRAMES * 4;
    if filter == FilterType::Minimum16k {
        frames.max(CHUNK_FRAMES * 8).max(minimum_frames)
    } else {
        frames.max(minimum_frames)
    }
}

fn dsd_path_latency_frames(filter: FilterType, dsd_rate: DsdRate, path: DsdPcmPath) -> usize {
    let Some(wire_rate) = dsd_rate.wire_rate_for_source(path.renderer_source_rate) else {
        return 0;
    };
    let intermediate_latency_frames = path
        .intermediate_rate
        .map(|rate| {
            dsd_filter_latency_frames(
                path.intermediate_filter.unwrap_or(filter),
                path.origin_source_rate,
                rate,
            )
        })
        .unwrap_or(0);
    let renderer_latency_at_renderer_rate =
        dsd_filter_latency_frames(filter, path.renderer_source_rate, wire_rate);
    let renderer_latency_frames = ceil_mul_div_usize(
        renderer_latency_at_renderer_rate,
        path.origin_source_rate as usize,
        path.renderer_source_rate as usize,
    );
    intermediate_latency_frames + renderer_latency_frames
}

fn dsd_analysis_fft_bits(seconds: f64) -> usize {
    if seconds > DSD_SECONDS_QUICK {
        DSD_ANALYSIS_FFT_BITS_FULL
    } else {
        DSD_ANALYSIS_FFT_BITS_QUICK
    }
}

fn dsd_tone_window_fft_bits(dsd_rate: DsdRate, seconds: f64) -> usize {
    dsd_tone_window_fft_bits_for_mode(
        dsd_rate,
        seconds,
        env_flag_enabled("FOZMO_DSD_RATE_SCALED_TONE_WINDOWS"),
    )
}

fn dsd_tone_window_fft_bits_for_mode(dsd_rate: DsdRate, seconds: f64, rate_scaled: bool) -> usize {
    if seconds > DSD_SECONDS_QUICK {
        if rate_scaled {
            DSD_TONE_WINDOW_FFT_BITS_FULL
                * (dsd_rate.oversample() / DsdRate::Dsd64.oversample()) as usize
        } else {
            DSD_TONE_WINDOW_FFT_BITS_FULL
        }
    } else {
        DSD_ANALYSIS_FFT_BITS_QUICK
    }
}

fn dsd_analysis_settle_bits(wire_rate: u32, seconds: f64) -> usize {
    if seconds > DSD_SECONDS_QUICK {
        (wire_rate as f64 * DSD_ANALYSIS_SETTLE_SECONDS_FULL).round() as usize
    } else {
        0
    }
}

fn dsd_filter_latency_frames(filter: FilterType, source_rate: u32, wire_rate: u32) -> usize {
    let resampler = SincResampler::new(filter, source_rate, wire_rate);
    (resampler.latency_ms() * source_rate as f64 / 1000.0).ceil() as usize
}

fn ceil_mul_div_usize(value: usize, multiplier: usize, divisor: usize) -> usize {
    if divisor == 0 {
        return 0;
    }
    value.saturating_mul(multiplier).saturating_add(divisor - 1) / divisor
}

fn coherent_dsd_tone_hz(wire_rate: u32, fft_bits: usize, target_hz: f64) -> f64 {
    let bin_hz = wire_rate as f64 / fft_bits as f64;
    let bin = (target_hz / bin_hz).round().max(1.0);
    bin * bin_hz
}

fn render_native_stream(
    renderer: &mut DsdRenderer,
    left: &[f64],
    right: &[f64],
) -> (Vec<u8>, Vec<u8>) {
    let mut out_l = Vec::new();
    let mut out_r = Vec::new();
    for start in (0..left.len()).step_by(CHUNK_FRAMES) {
        let end = (start + CHUNK_FRAMES).min(left.len());
        renderer.upsample(&left[start..end], &right[start..end]);
        renderer.modulate_and_pack_native(1.0, &mut out_l, &mut out_r);
    }
    // Materialize the resampler's held lookahead before flushing the
    // modulators. `drain_resampler_eof` trims to the exact nominal
    // input-to-wire ratio, so this completes the real source window without
    // attributing extra zero-padding samples to it.
    renderer.drain_resampler_eof();
    renderer.modulate_and_pack_native(1.0, &mut out_l, &mut out_r);
    renderer.flush_modulators_and_pack_native(&mut out_l, &mut out_r);
    renderer.flush_native_with_idle(&mut out_l, &mut out_r);
    (out_l, out_r)
}

fn renderer_native_left_bits(renderer: &mut DsdRenderer, input: &[f64]) -> Vec<f64> {
    let (left, _) = render_native_stream(renderer, input, input);
    unpack_native_msb(&left)
}

fn renderer_native_stereo_bits(
    renderer: &mut DsdRenderer,
    left: &[f64],
    right: &[f64],
) -> (Vec<f64>, Vec<f64>) {
    let (left, right) = render_native_stream(renderer, left, right);
    (unpack_native_msb(&left), unpack_native_msb(&right))
}

struct DsdBitDumpRequest<'a> {
    filter: &'a str,
    modulator: &'a str,
    path_variant: &'a str,
    source_rate: u32,
    origin_source_rate: u32,
    renderer_source_rate: u32,
    dsd_rate: DsdRate,
    wire_rate: u32,
    tone_hz: f64,
    tone_fft_bits: usize,
    left: &'a [u8],
    right: &'a [u8],
}

fn maybe_dump_dsd_bitstream(request: DsdBitDumpRequest<'_>) -> Result<(), String> {
    let Some(dir) = dsd_bit_dump_dir() else {
        return Ok(());
    };
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    dump_dsd_bitstream_channel(&dir, &request, "left", request.left)?;
    dump_dsd_bitstream_channel(&dir, &request, "right", request.right)?;
    Ok(())
}

fn dump_dsd_bitstream_channel(
    dir: &Path,
    request: &DsdBitDumpRequest<'_>,
    channel: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let dsd_rate = dsd_rate_name(request.dsd_rate);
    let stem = format!(
        "{}__{}__{}__{}hz__{}__{}",
        sanitize_file_component(request.filter),
        sanitize_file_component(request.modulator),
        sanitize_file_component(request.path_variant),
        request.renderer_source_rate,
        dsd_rate.to_ascii_lowercase(),
        channel
    );
    let filename = format!("{stem}.dsd");
    fs::write(dir.join(&filename), bytes).map_err(|err| err.to_string())?;
    append_dsd_bit_dump_index(
        dir,
        request,
        channel,
        bytes.len(),
        bytes.len().saturating_mul(8),
        &filename,
    )
}

fn append_dsd_bit_dump_index(
    dir: &Path,
    request: &DsdBitDumpRequest<'_>,
    channel: &str,
    byte_count: usize,
    bit_count: usize,
    filename: &str,
) -> Result<(), String> {
    let path = dir.join("dsd_bitstreams.csv");
    let write_header = !path.exists();
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| err.to_string())?;
    if write_header {
        writeln!(
            file,
            "filter,modulator,path_variant,source_rate,origin_source_rate,renderer_source_rate,dsd_rate,wire_rate,tone_hz,tone_fft_bits,chunk_frames,native_order,channel,byte_count,bit_count,file"
        )
        .map_err(|err| err.to_string())?;
    }
    let row = [
        request.filter.to_string(),
        request.modulator.to_string(),
        request.path_variant.to_string(),
        request.source_rate.to_string(),
        request.origin_source_rate.to_string(),
        request.renderer_source_rate.to_string(),
        dsd_rate_name(request.dsd_rate).to_string(),
        request.wire_rate.to_string(),
        format!("{:.9}", request.tone_hz),
        request.tone_fft_bits.to_string(),
        CHUNK_FRAMES.to_string(),
        "msb-first".to_string(),
        channel.to_string(),
        byte_count.to_string(),
        bit_count.to_string(),
        filename.to_string(),
    ];
    writeln!(
        file,
        "{}",
        row.into_iter().map(csv_cell).collect::<Vec<_>>().join(",")
    )
    .map_err(|err| err.to_string())
}

fn sanitize_file_component(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() { "_".to_string() } else { out }
}

fn dsd_bit_dump_dir() -> Option<PathBuf> {
    env_flag_value("FOZMO_DSD_DUMP_BITS").map(|value| match value.as_str() {
        "1" | "true" | "yes" | "on" => PathBuf::from("dsd_bit_dumps"),
        _ => PathBuf::from(value),
    })
}

fn env_flag_value(name: &str) -> Option<String> {
    let value = env::var(name).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if matches!(lower.as_str(), "0" | "false" | "no" | "off") {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn env_flag_enabled(name: &str) -> bool {
    env_flag_value(name).is_some()
}

fn dsd_premod_windows_enabled() -> bool {
    env_flag_enabled("FOZMO_DSD_PREMOD_WINDOWS")
}

fn collect_premod_left_stream(renderer: &mut DsdRenderer, input: &[f64]) -> Vec<f64> {
    let mut left = Vec::new();
    for start in (0..input.len()).step_by(CHUNK_FRAMES) {
        let end = (start + CHUNK_FRAMES).min(input.len());
        let pcm = renderer.upsample(&input[start..end], &input[start..end]);
        left.reserve(pcm.len() / 2);
        for frame in pcm.chunks_exact(2) {
            left.push(frame[0]);
        }
    }
    let pcm = renderer.drain_resampler_eof();
    left.reserve(pcm.len() / 2);
    for frame in pcm.chunks_exact(2) {
        left.push(frame[0]);
    }
    left
}

// Premodulation metrics are called from explicit experiment matrices; grouping would hide columns.
#[allow(clippy::too_many_arguments)]
fn dsd_premod_window_metrics(
    filter: FilterType,
    dsd_rate: DsdRate,
    renderer_source_rate: u32,
    dsd_modulator: DsdModulator,
    renderer_input: &[f64],
    wire_rate: u32,
    tone_hz: f64,
    target_fft_len: usize,
    dsd_worst_start_s: Option<f64>,
    config: DsdExperimentConfig,
) -> Vec<DsdPremodWindowRow> {
    let Ok(mut renderer) = new_dsd_renderer(
        filter,
        renderer_source_rate,
        dsd_rate,
        dsd_modulator,
        config,
    ) else {
        return Vec::new();
    };
    let premod = collect_premod_left_stream(&mut renderer, renderer_input);
    let Some(fft_len) = dsd_fft_len(premod.len(), target_fft_len) else {
        return Vec::new();
    };
    dsd_distribution_window_starts(premod.len(), fft_len, wire_rate, target_fft_len)
        .into_iter()
        .filter_map(|start| {
            premod_window_row(
                &premod,
                wire_rate,
                tone_hz,
                start,
                fft_len,
                dsd_worst_start_s,
            )
        })
        .collect()
}

fn premod_window_row(
    samples: &[f64],
    sample_rate: u32,
    tone_hz: f64,
    start: usize,
    len: usize,
    dsd_worst_start_s: Option<f64>,
) -> Option<DsdPremodWindowRow> {
    let window = samples.get(start..start + len)?;
    if window.len() < 2 {
        return None;
    }
    let rms_value = rms(window).max(1e-18);
    let max_abs = window
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0f64, f64::max)
        .max(1e-18);
    let dc = (window.iter().sum::<f64>() / window.len() as f64)
        .abs()
        .max(1e-18);
    let slope_rms = (window
        .windows(2)
        .map(|pair| {
            let delta = pair[1] - pair[0];
            delta * delta
        })
        .sum::<f64>()
        / (window.len() - 1) as f64)
        .sqrt()
        .max(1e-18);
    let (tone_amp, tone_phase, residual_rms) =
        fitted_tone_metrics(window, sample_rate, tone_hz, start);
    let start_s = start as f64 / sample_rate as f64;
    Some(DsdPremodWindowRow {
        start_s,
        start_sample: start,
        len_samples: len,
        rms_dbfs: db(rms_value),
        max_abs_dbfs: db(max_abs),
        dc_dbfs: db(dc),
        crest_db: db(max_abs / rms_value),
        slope_rms_dbfs: db(slope_rms),
        tone_amp_dbfs: db(tone_amp.max(1e-18)),
        tone_phase_rad: tone_phase,
        residual_rms_dbfs: db(residual_rms.max(1e-18)),
        residual_relative_db: db((residual_rms / tone_amp.max(1e-18)).max(1e-18)),
        is_dsd_worst_start: dsd_worst_start_s
            .map(|worst| (start_s - worst).abs() <= 0.5 / sample_rate as f64)
            .unwrap_or(false),
    })
}

fn fitted_tone_metrics(
    samples: &[f64],
    sample_rate: u32,
    tone_hz: f64,
    absolute_start: usize,
) -> (f64, f64, f64) {
    let omega = 2.0 * PI * tone_hz / sample_rate as f64;
    let mut sin_dot = 0.0;
    let mut cos_dot = 0.0;
    for (idx, sample) in samples.iter().enumerate() {
        let phase = omega * (absolute_start + idx) as f64;
        sin_dot += sample * phase.sin();
        cos_dot += sample * phase.cos();
    }
    let scale = 2.0 / samples.len().max(1) as f64;
    let sin_coeff = sin_dot * scale;
    let cos_coeff = cos_dot * scale;
    let amp = (sin_coeff * sin_coeff + cos_coeff * cos_coeff).sqrt();
    let phase = cos_coeff.atan2(sin_coeff);
    let residual_sum = samples
        .iter()
        .enumerate()
        .map(|(idx, sample)| {
            let angle = omega * (absolute_start + idx) as f64;
            let fitted = sin_coeff * angle.sin() + cos_coeff * angle.cos();
            let residual = sample - fitted;
            residual * residual
        })
        .sum::<f64>();
    let residual_rms = (residual_sum / samples.len().max(1) as f64).sqrt();
    (amp, phase, residual_rms)
}

#[derive(Debug, Default)]
struct DsdArtifactMetrics {
    idle_worst_tone_dbfs: Option<f64>,
    idle_worst_tone_source: Option<String>,
    idle_worst_density_deviation: Option<f64>,
    idle_worst_density_source: Option<String>,
    idle_artifacts: Vec<DsdIdleArtifactRow>,
    low_level_worst_residual_db: Option<f64>,
    low_level_worst_residual_source: Option<String>,
    low_level_worst_spur_dbfs: Option<f64>,
    low_level_worst_spur_source: Option<String>,
    high_freq_tone_worst_residual_db: Option<f64>,
    high_freq_tone_worst_spur_dbfs: Option<f64>,
    high_freq_imd_residual_db: Option<f64>,
    high_freq_imd_spur_dbfs: Option<f64>,
    high_freq_worst_residual_db: Option<f64>,
    high_freq_worst_residual_source: Option<String>,
    high_freq_worst_spur_dbfs: Option<f64>,
    high_freq_worst_spur_source: Option<String>,
    multitone_residual_db: Option<f64>,
    multitone_spur_dbfs: Option<f64>,
    overload_recovery_dbfs: Option<f64>,
    overload_recovery_diagnostics: Vec<DsdOverloadRecoveryDiagnosticRow>,
    transient_click_candidates: Option<usize>,
    transient_click_max_score: Option<f64>,
    transient_click_max_residual: Option<f64>,
    transient_click_source: Option<String>,
    program_click_candidates: Option<usize>,
    program_click_max_score: Option<f64>,
    program_click_max_residual: Option<f64>,
    program_click_source: Option<String>,
    notes: Vec<String>,
}

fn measure_dsd_artifacts(
    filter: FilterType,
    dsd_rate: DsdRate,
    path: DsdPcmPath,
    dsd_modulator: DsdModulator,
    frames: usize,
    wire_rate: u32,
    config: DsdExperimentConfig,
) -> DsdArtifactMetrics {
    let source_rate = path.origin_source_rate;
    let ratio = (wire_rate / source_rate) as usize;
    let latency_frames = dsd_path_latency_frames(filter, dsd_rate, path);
    let probe_frames = frames.max(latency_frames + CHUNK_FRAMES * 8);
    let density_window_bits = (wire_rate as usize / 100).max(1024);
    let mut metrics = DsdArtifactMetrics::default();

    let mut idle_fixtures = vec![
        ("silence".to_string(), vec![0.0; probe_frames]),
        (
            "-120dBFS_997Hz".to_string(),
            sine(
                probe_frames,
                source_rate,
                997.0,
                10.0f64.powf(-120.0 / 20.0),
            ),
        ),
    ];
    for dc in [
        0.0, 1.0e-7, -1.0e-7, 3.0e-7, -3.0e-7, 1.0e-6, -1.0e-6, 3.0e-6, -3.0e-6, 1.0e-5, -1.0e-5,
    ] {
        idle_fixtures.push((dc_fixture_label(dc), vec![dc; probe_frames]));
    }

    for (label, input) in idle_fixtures {
        let Some(channels) = render_decoded_probe_channels(
            filter,
            dsd_rate,
            path,
            dsd_modulator,
            &input,
            ratio,
            config,
        ) else {
            continue;
        };
        for (channel, decoded, bits) in &channels {
            let source = format!("{label}:{channel}");
            let idle_peak = max_band_peak_dbfs(decoded, source_rate, 20.0, 20_000.0);
            if let Some((_, tone)) = idle_peak {
                record_max_with_source(
                    &mut metrics.idle_worst_tone_dbfs,
                    &mut metrics.idle_worst_tone_source,
                    tone,
                    &source,
                );
            }
            let density = rolling_bit_density_max_deviation(bits, density_window_bits);
            if let Some(density) = density {
                record_max_with_source(
                    &mut metrics.idle_worst_density_deviation,
                    &mut metrics.idle_worst_density_source,
                    density,
                    &source,
                );
            }
            metrics.idle_artifacts.push(DsdIdleArtifactRow {
                fixture_label: label.clone(),
                source,
                channel: (*channel).to_string(),
                idle_peak_freq_hz: idle_peak.map(|(freq, _)| freq),
                idle_peak_dbfs: idle_peak.map(|(_, dbfs)| dbfs),
                density_max_deviation: density,
                density_window_bits,
                is_idle_worst_tone: false,
                is_idle_worst_density: false,
            });
        }
    }
    mark_idle_artifact_winners(&mut metrics);

    for dbfs in [-120.0, -100.0, -80.0] {
        let label = format!("{dbfs:.0}dBFS_997Hz");
        let input = sine(probe_frames, source_rate, 997.0, 10.0f64.powf(dbfs / 20.0));
        let Some(channels) = render_decoded_probe_channels(
            filter,
            dsd_rate,
            path,
            dsd_modulator,
            &input,
            ratio,
            config,
        ) else {
            continue;
        };
        for (channel, decoded, _) in &channels {
            let source = format!("{label}:{channel}");
            if let Some((relative_db, peak_dbfs)) =
                fitted_probe_residual_metrics(decoded, source_rate, &[997.0])
            {
                if let Some(relative_db) = relative_db {
                    record_max_with_source(
                        &mut metrics.low_level_worst_residual_db,
                        &mut metrics.low_level_worst_residual_source,
                        relative_db,
                        &source,
                    );
                }
                if let Some(peak_dbfs) = peak_dbfs {
                    record_max_with_source(
                        &mut metrics.low_level_worst_spur_dbfs,
                        &mut metrics.low_level_worst_spur_source,
                        peak_dbfs,
                        &source,
                    );
                }
            }
        }
    }

    for (label, input, tones) in [
        (
            "19k_-12dBFS",
            sine(
                probe_frames,
                source_rate,
                19_000.0,
                10.0f64.powf(-12.0 / 20.0),
            ),
            vec![19_000.0],
        ),
        (
            "20k_-18dBFS",
            sine(
                probe_frames,
                source_rate,
                20_000.0,
                10.0f64.powf(-18.0 / 20.0),
            ),
            vec![20_000.0],
        ),
    ] {
        let Some(channels) = render_decoded_probe_channels(
            filter,
            dsd_rate,
            path,
            dsd_modulator,
            &input,
            ratio,
            config,
        ) else {
            continue;
        };
        for (channel, decoded, _) in &channels {
            let source = format!("{label}:{channel}");
            if let Some((relative_db, peak_dbfs)) =
                fitted_probe_residual_metrics(decoded, source_rate, &tones)
            {
                if let Some(relative_db) = relative_db {
                    metrics.high_freq_tone_worst_residual_db =
                        max_option(metrics.high_freq_tone_worst_residual_db, relative_db);
                    record_max_with_source(
                        &mut metrics.high_freq_worst_residual_db,
                        &mut metrics.high_freq_worst_residual_source,
                        relative_db,
                        &source,
                    );
                }
                if let Some(peak_dbfs) = peak_dbfs {
                    metrics.high_freq_tone_worst_spur_dbfs =
                        max_option(metrics.high_freq_tone_worst_spur_dbfs, peak_dbfs);
                    record_max_with_source(
                        &mut metrics.high_freq_worst_spur_dbfs,
                        &mut metrics.high_freq_worst_spur_source,
                        peak_dbfs,
                        &source,
                    );
                }
            }
        }
    }

    for (label, input, tones) in [(
        "18k_19k_imd",
        two_tone(probe_frames, source_rate, 18_000.0, 19_000.0, 0.18),
        vec![18_000.0, 19_000.0],
    )] {
        let Some(channels) = render_decoded_probe_channels(
            filter,
            dsd_rate,
            path,
            dsd_modulator,
            &input,
            ratio,
            config,
        ) else {
            continue;
        };
        for (channel, decoded, _) in &channels {
            let source = format!("{label}:{channel}");
            if let Some((relative_db, peak_dbfs)) =
                fitted_probe_residual_metrics(decoded, source_rate, &tones)
            {
                if let Some(relative_db) = relative_db {
                    metrics.high_freq_imd_residual_db =
                        max_option(metrics.high_freq_imd_residual_db, relative_db);
                    record_max_with_source(
                        &mut metrics.high_freq_worst_residual_db,
                        &mut metrics.high_freq_worst_residual_source,
                        relative_db,
                        &source,
                    );
                }
                if let Some(peak_dbfs) = peak_dbfs {
                    metrics.high_freq_imd_spur_dbfs =
                        max_option(metrics.high_freq_imd_spur_dbfs, peak_dbfs);
                    record_max_with_source(
                        &mut metrics.high_freq_worst_spur_dbfs,
                        &mut metrics.high_freq_worst_spur_source,
                        peak_dbfs,
                        &source,
                    );
                }
            }
        }
    }

    let (program, program_tones) = program_multitone(probe_frames, source_rate, 0.42);
    if let Some(channels) = render_decoded_probe_channels(
        filter,
        dsd_rate,
        path,
        dsd_modulator,
        &program,
        ratio,
        config,
    ) {
        for (channel, decoded, _) in &channels {
            if let Some((relative_db, peak_dbfs)) =
                fitted_probe_residual_metrics(decoded, source_rate, &program_tones)
            {
                if let Some(relative_db) = relative_db {
                    metrics.multitone_residual_db =
                        max_option(metrics.multitone_residual_db, relative_db);
                }
                if let Some(peak_dbfs) = peak_dbfs {
                    metrics.multitone_spur_dbfs =
                        max_option(metrics.multitone_spur_dbfs, peak_dbfs);
                }
            }
            let clicks = crackle_stats_with_settle(decoded, 16);
            metrics.record_program_clicks(&format!("dense_multitone:{channel}"), clicks);
        }
    }

    if dsd_rate == DsdRate::Dsd128 {
        let mut labels = Vec::new();
        for probe in dsd128_program_probes(probe_frames, source_rate) {
            labels.push(probe.label);
            let Some(channels) = render_decoded_probe_channels(
                filter,
                dsd_rate,
                path,
                dsd_modulator,
                &probe.input,
                ratio,
                config,
            ) else {
                continue;
            };
            for (channel, decoded, _) in &channels {
                let clicks = crackle_stats_with_settle(decoded, probe.click_settle);
                metrics.record_program_clicks(&format!("{}:{channel}", probe.label), clicks);
            }
        }
        if !labels.is_empty() {
            metrics
                .notes
                .push(format!("dsd128_program_probes={}", labels.join("|")));
        }
    }

    for probe in dsd_transient_probes(probe_frames, source_rate) {
        let Some(channels) = render_decoded_probe_channels(
            filter,
            dsd_rate,
            path,
            dsd_modulator,
            &probe.input,
            ratio,
            config,
        ) else {
            continue;
        };
        for (channel, decoded, bits) in &channels {
            let source = format!("{}:{channel}", probe.label);
            if probe.label == "overload_recovery" {
                let start = decoded.len() * 3 / 4;
                if let Some(peak) =
                    sample_abs_peak(&decoded[start..]).map(|peak| db(peak.max(1e-18)))
                {
                    metrics.overload_recovery_dbfs =
                        max_option(metrics.overload_recovery_dbfs, peak);
                }
                if let Some(diagnostic) = overload_recovery_diagnostic(
                    &source,
                    channel,
                    decoded,
                    bits,
                    start,
                    ratio,
                    wire_rate,
                    source_rate,
                    density_window_bits,
                    dsd_rate,
                ) {
                    metrics.overload_recovery_diagnostics.push(diagnostic);
                }
                let clicks = crackle_stats_with_settle(&decoded[start..], 16);
                metrics.record_transient_clicks(&source, clicks);
            } else {
                let clicks = crackle_stats_with_settle(decoded, probe.click_settle);
                metrics.record_transient_clicks(&source, clicks);
            }
        }
    }

    metrics.push_source_notes();
    metrics
}

#[allow(clippy::too_many_arguments)]
fn overload_recovery_diagnostic(
    source: &str,
    channel: &str,
    decoded: &[f64],
    bits: &[f64],
    tail_start_sample: usize,
    ratio: usize,
    wire_rate: u32,
    source_rate: u32,
    density_window_bits: usize,
    dsd_rate: DsdRate,
) -> Option<DsdOverloadRecoveryDiagnosticRow> {
    let tail = decoded.get(tail_start_sample..)?;
    if tail.is_empty() {
        return None;
    }
    let raw_tail_peak_value = tail
        .iter()
        .copied()
        .max_by(|left, right| left.abs().partial_cmp(&right.abs()).unwrap())?;
    let raw_tail_peak_abs = raw_tail_peak_value.abs();
    let tail_bit_start = tail_start_sample.saturating_mul(ratio).min(bits.len());
    let tail_bits = &bits[tail_bit_start..];
    let reconstructed_tail_peak_dbfs =
        dsd_prefilter_and_decimate_to_pcm(bits, wire_rate, source_rate).and_then(|reconstructed| {
            sample_abs_peak(reconstructed.get(tail_start_sample..)?).map(|peak| db(peak.max(1e-18)))
        });
    let fft_peak = max_band_peak_dbfs(tail, source_rate, 0.0, source_rate as f64 * 0.5);
    Some(DsdOverloadRecoveryDiagnosticRow {
        source: source.to_string(),
        channel: channel.to_string(),
        tail_start_sample,
        tail_len_samples: tail.len(),
        raw_tail_peak_value: Some(raw_tail_peak_value),
        raw_tail_peak_dbfs: Some(db(raw_tail_peak_abs.max(1e-18))),
        equals_dsd64_min_nonzero_step: dsd_rate == DsdRate::Dsd64
            && (raw_tail_peak_abs - 1.0 / 32.0).abs() <= 1.0e-12,
        nonzero_tail_samples: tail.iter().filter(|sample| sample.abs() > 0.0).count(),
        max_nonzero_run_samples: max_nonzero_run(tail),
        tail_rms_dbfs: Some(db(rms(tail).max(1e-18))),
        fft_peak_hz: fft_peak.map(|(freq, _)| freq),
        fft_peak_dbfs: fft_peak.map(|(_, dbfs)| dbfs),
        reconstructed_tail_peak_dbfs,
        tail_density_max_deviation: rolling_bit_density_max_deviation(
            tail_bits,
            density_window_bits,
        ),
        density_window_bits,
    })
}

fn max_nonzero_run(samples: &[f64]) -> usize {
    let mut current = 0usize;
    let mut max_run = 0usize;
    for sample in samples {
        if sample.abs() > 0.0 {
            current += 1;
            max_run = max_run.max(current);
        } else {
            current = 0;
        }
    }
    max_run
}

impl DsdArtifactMetrics {
    fn record_program_clicks(&mut self, label: &str, clicks: CrackleStats) {
        self.program_click_candidates =
            Some(self.program_click_candidates.unwrap_or(0) + clicks.candidates);
        if self
            .program_click_max_score
            .is_none_or(|score| clicks.max_score > score)
        {
            self.program_click_max_score = Some(clicks.max_score);
            self.program_click_source = Some(label.to_string());
        }
        self.program_click_max_residual =
            max_option(self.program_click_max_residual, clicks.max_residual);
    }

    fn record_transient_clicks(&mut self, label: &str, clicks: CrackleStats) {
        self.transient_click_candidates =
            Some(self.transient_click_candidates.unwrap_or(0) + clicks.candidates);
        if self
            .transient_click_max_score
            .is_none_or(|score| clicks.max_score > score)
        {
            self.transient_click_max_score = Some(clicks.max_score);
            self.transient_click_source = Some(label.to_string());
        }
        self.transient_click_max_residual =
            max_option(self.transient_click_max_residual, clicks.max_residual);
    }

    fn push_source_notes(&mut self) {
        push_source_note(
            &mut self.notes,
            "idle_worst_tone_source",
            self.idle_worst_tone_source.as_deref(),
        );
        push_source_note(
            &mut self.notes,
            "idle_worst_density_source",
            self.idle_worst_density_source.as_deref(),
        );
        push_source_note(
            &mut self.notes,
            "low_level_worst_residual_source",
            self.low_level_worst_residual_source.as_deref(),
        );
        push_source_note(
            &mut self.notes,
            "low_level_worst_spur_source",
            self.low_level_worst_spur_source.as_deref(),
        );
        push_source_note(
            &mut self.notes,
            "high_freq_worst_residual_source",
            self.high_freq_worst_residual_source.as_deref(),
        );
        push_source_note(
            &mut self.notes,
            "high_freq_worst_spur_source",
            self.high_freq_worst_spur_source.as_deref(),
        );
        push_source_note(
            &mut self.notes,
            "program_click_source",
            self.program_click_source.as_deref(),
        );
        push_source_note(
            &mut self.notes,
            "transient_click_source",
            self.transient_click_source.as_deref(),
        );
    }
}

fn mark_idle_artifact_winners(metrics: &mut DsdArtifactMetrics) {
    for row in &mut metrics.idle_artifacts {
        row.is_idle_worst_tone = match (row.idle_peak_dbfs, metrics.idle_worst_tone_dbfs) {
            (Some(row_value), Some(worst)) => (row_value - worst).abs() <= 1.0e-9,
            _ => false,
        };
        row.is_idle_worst_density = match (
            row.density_max_deviation,
            metrics.idle_worst_density_deviation,
        ) {
            (Some(row_value), Some(worst)) => (row_value - worst).abs() <= 1.0e-12,
            _ => false,
        };
    }
}

fn dc_fixture_label(dc: f64) -> String {
    if dc == 0.0 {
        return "dc_0".to_string();
    }
    let sign = if dc.is_sign_negative() { "-" } else { "" };
    let magnitude = dc.abs();
    let suffix = if (magnitude - 1.0e-7).abs() < f64::EPSILON {
        "1e-7"
    } else if (magnitude - 3.0e-7).abs() < f64::EPSILON {
        "3e-7"
    } else if (magnitude - 1.0e-6).abs() < f64::EPSILON {
        "1e-6"
    } else if (magnitude - 3.0e-6).abs() < f64::EPSILON {
        "3e-6"
    } else if (magnitude - 1.0e-5).abs() < f64::EPSILON {
        "1e-5"
    } else {
        return format!("dc_{dc:.3e}");
    };
    format!("dc_{sign}{suffix}")
}

fn fitted_probe_residual_metrics(
    decoded: &[f64],
    source_rate: u32,
    tones: &[f64],
) -> Option<(Option<f64>, Option<f64>)> {
    let residual = fitted_tone_residual(decoded, source_rate, tones)?;
    let peak_dbfs = residual_spectrum_metrics(&residual.error, source_rate)
        .and_then(|spectrum| spectrum.peak_dbfs);
    Some((residual.relative_db, peak_dbfs))
}

fn render_decoded_probe(
    filter: FilterType,
    dsd_rate: DsdRate,
    path: DsdPcmPath,
    dsd_modulator: DsdModulator,
    input: &[f64],
    ratio: usize,
    config: DsdExperimentConfig,
) -> Option<(Vec<f64>, Vec<f64>)> {
    let ((decoded_l, _decoded_r), (bits_l, _bits_r)) =
        render_decoded_stereo_probe(filter, dsd_rate, path, dsd_modulator, input, ratio, config)?;
    Some((decoded_l, bits_l))
}

fn render_decoded_probe_channels(
    filter: FilterType,
    dsd_rate: DsdRate,
    path: DsdPcmPath,
    dsd_modulator: DsdModulator,
    input: &[f64],
    ratio: usize,
    config: DsdExperimentConfig,
) -> Option<Vec<DecodedProbeChannel>> {
    let ((decoded_l, decoded_r), (bits_l, bits_r)) =
        render_decoded_stereo_probe(filter, dsd_rate, path, dsd_modulator, input, ratio, config)?;
    Some(vec![
        ("left", decoded_l, bits_l),
        ("right", decoded_r, bits_r),
    ])
}

fn render_decoded_stereo_probe(
    filter: FilterType,
    dsd_rate: DsdRate,
    path: DsdPcmPath,
    dsd_modulator: DsdModulator,
    input: &[f64],
    ratio: usize,
    config: DsdExperimentConfig,
) -> Option<StereoProbeOutput> {
    let renderer_input = path.prepare_renderer_input(filter, input).ok()?;
    let mut renderer = new_dsd_renderer(
        filter,
        path.renderer_source_rate,
        dsd_rate,
        dsd_modulator,
        config,
    )
    .ok()?;
    renderer.set_native_order(NativeDsdOrder::MsbFirst);
    let (bits_l, bits_r) =
        renderer_native_stereo_bits(&mut renderer, &renderer_input, &renderer_input);
    let decoded_l = decimate_dsd_bits(&bits_l, ratio);
    let decoded_r = decimate_dsd_bits(&bits_r, ratio);
    Some(((decoded_l, decoded_r), (bits_l, bits_r)))
}

fn ec4a_crackle_torture_fixture(frames: usize, sample_rate: u32) -> (Vec<f64>, Vec<f64>) {
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    for idx in 0..frames {
        let t = idx as f64 / sample_rate as f64;
        let (l, r) = ec4a_crackle_torture_sample(t);
        left.push(l);
        right.push(r);
    }
    (left, right)
}

fn ec4a_crackle_torture_sample(t: f64) -> (f64, f64) {
    let (left, right) = if t < 5.0 {
        let low = 10.0f64.powf(-120.0 / 20.0) * (2.0 * PI * 997.0 * t).sin();
        (low + 1.0e-6, 0.8 * low - 1.0e-6)
    } else if t < 20.0 {
        let local = t - 5.0;
        let sweep = 0.5 - 0.5 * (2.0 * PI * local / 15.0).cos();
        let dbfs = -12.0 + 11.0 * sweep;
        let amp = 10.0f64.powf(dbfs / 20.0);
        (
            amp * (2.0 * PI * 997.0 * t).sin(),
            0.93 * amp * (2.0 * PI * 1_379.0 * t + 0.4).sin(),
        )
    } else if t < 35.0 {
        let local = t - 20.0;
        let env = 0.5 + 0.5 * (2.0 * PI * local / 7.5).sin();
        let a = 10.0f64.powf((-20.0 + 8.0 * env) / 20.0);
        (
            a * ((2.0 * PI * 18_000.0 * t).sin() + 0.72 * (2.0 * PI * 19_000.0 * t + 0.2).sin()),
            a * (0.85 * (2.0 * PI * 19_000.0 * t).sin()
                + 0.55 * (2.0 * PI * 20_000.0 * t + 0.5).sin()),
        )
    } else if t < 45.0 {
        let local = t - 35.0;
        let period = 0.25;
        let phase = local % period;
        if phase < 0.040 {
            let env = raised_cosine_pulse(phase / 0.040);
            (
                0.86 * env * (2.0 * PI * 5_000.0 * t).sin(),
                -0.80 * env * (2.0 * PI * 4_300.0 * t + 0.3).sin(),
            )
        } else {
            (0.0, 0.0)
        }
    } else {
        let local = t - 45.0;
        let env = 0.65 + 0.25 * (2.0 * PI * local / 5.0).sin();
        let l = 0.22 * (2.0 * PI * 233.0 * t).sin()
            + 0.18 * (2.0 * PI * 997.0 * t + 0.2).sin()
            + 0.15 * (2.0 * PI * 3_911.0 * t + 0.7).sin()
            + 0.12 * (2.0 * PI * 7_321.0 * t + 1.1).sin()
            + 0.10 * (2.0 * PI * 13_733.0 * t + 0.5).sin();
        let r = 0.20 * (2.0 * PI * 311.0 * t + 0.4).sin()
            + 0.18 * (2.0 * PI * 1_237.0 * t).sin()
            + 0.14 * (2.0 * PI * 4_777.0 * t + 0.9).sin()
            + 0.11 * (2.0 * PI * 9_101.0 * t + 0.1).sin()
            + 0.09 * (2.0 * PI * 15_217.0 * t + 1.3).sin();
        (env * l, env * r)
    };
    (left.clamp(-0.95, 0.95), right.clamp(-0.95, 0.95))
}

fn raised_cosine_pulse(phase: f64) -> f64 {
    let phase = phase.clamp(0.0, 1.0);
    (PI * phase).sin().powi(2)
}

struct NativeByteDecimator {
    bytes_per_frame: usize,
    pending: Vec<u8>,
}

impl NativeByteDecimator {
    fn new(bytes_per_frame: usize) -> Self {
        Self {
            bytes_per_frame: bytes_per_frame.max(1),
            pending: Vec::new(),
        }
    }

    fn push(&mut self, bytes: &[u8], decoded: &mut Vec<f64>) {
        if !self.pending.is_empty() {
            let needed = self.bytes_per_frame - self.pending.len();
            let take = needed.min(bytes.len());
            self.pending.extend_from_slice(&bytes[..take]);
            if self.pending.len() == self.bytes_per_frame {
                decoded.push(native_frame_density(&self.pending));
                self.pending.clear();
            }
            if take == bytes.len() {
                return;
            }
            self.push_aligned(&bytes[take..], decoded);
        } else {
            self.push_aligned(bytes, decoded);
        }
    }

    fn push_aligned(&mut self, bytes: &[u8], decoded: &mut Vec<f64>) {
        let full_len = bytes.len() / self.bytes_per_frame * self.bytes_per_frame;
        for chunk in bytes[..full_len].chunks_exact(self.bytes_per_frame) {
            decoded.push(native_frame_density(chunk));
        }
        if full_len < bytes.len() {
            self.pending.extend_from_slice(&bytes[full_len..]);
        }
    }

    fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

fn native_frame_density(bytes: &[u8]) -> f64 {
    let ones: u32 = bytes.iter().map(|byte| byte.count_ones()).sum();
    let bits = (bytes.len() * 8) as f64;
    (2.0 * ones as f64 - bits) / bits
}

#[derive(Debug, Clone, Copy, Default)]
struct CrackleStats {
    candidates: usize,
    max_score: f64,
    max_residual: f64,
}

fn crackle_stats(samples: &[f64], sample_rate: u32) -> CrackleStats {
    let settle = (sample_rate as f64 * CRACKLE_ANALYSIS_SETTLE_SECONDS).round() as usize;
    crackle_stats_with_settle(samples, settle)
}

fn crackle_stats_with_settle(samples: &[f64], settle: usize) -> CrackleStats {
    if samples.len() <= settle * 2 + 40 {
        return CrackleStats::default();
    }
    let start = settle + 16;
    let end = samples.len() - settle - 16;
    let mut stats = CrackleStats::default();
    for idx in start..end {
        let residual = click_residual(samples, idx).abs();
        stats.max_residual = stats.max_residual.max(residual);
        if residual < CRACKLE_RESIDUAL_FLOOR {
            continue;
        }
        let mut local = 0.0;
        let mut count = 0usize;
        for neighbor in idx - 16..=idx + 16 {
            if neighbor.abs_diff(idx) <= 1 {
                continue;
            }
            local += click_residual(samples, neighbor).abs();
            count += 1;
        }
        let score = residual / (local / count.max(1) as f64 + 1.0e-6);
        stats.max_score = stats.max_score.max(score);
        if score > CRACKLE_SCORE_LIMIT {
            stats.candidates += 1;
        }
    }
    stats
}

fn click_residual(samples: &[f64], idx: usize) -> f64 {
    samples[idx] - 0.5 * (samples[idx - 1] + samples[idx + 1])
}

fn measure_dsd_stress(
    filter: FilterType,
    dsd_rate: DsdRate,
    path: DsdPcmPath,
    dsd_modulator: DsdModulator,
    config: DsdExperimentConfig,
) -> (u64, u64, Vec<String>) {
    let source_rate = path.origin_source_rate;
    let frames = ((source_rate as f64 * 0.004).round() as usize).max(512);
    let scenarios = [
        StressScenario::Dc(1.0e-5),
        StressScenario::Dc(1.0e-4),
        StressScenario::SineDb(-60.0),
        StressScenario::SineDb(-20.0),
        StressScenario::SineDb(-6.0),
        StressScenario::SineDb(-0.45),
    ];

    let mut notes = Vec::new();
    let mut input = Vec::with_capacity(frames * scenarios.len());
    for scenario in scenarios {
        match scenario {
            StressScenario::Dc(value) => vec![value; frames],
            StressScenario::SineDb(dbfs) => {
                sine(frames, source_rate, 997.0, 10.0f64.powf(dbfs / 20.0))
            }
        }
        .into_iter()
        .for_each(|sample| input.push(sample));
    }
    let renderer_input = match path.prepare_renderer_input(filter, &input) {
        Ok(input) => input,
        Err(err) => {
            notes.push(format!("stress: {err}"));
            return (0, 0, notes);
        }
    };
    let mut renderer = match new_dsd_renderer(
        filter,
        path.renderer_source_rate,
        dsd_rate,
        dsd_modulator,
        config,
    ) {
        Ok(renderer) => renderer,
        Err(err) => {
            notes.push(format!("stress: {err}"));
            return (0, 0, notes);
        }
    };
    renderer.set_native_order(NativeDsdOrder::MsbFirst);
    let _ = render_native_stream(&mut renderer, &renderer_input, &renderer_input);
    let resets = renderer.stability_resets();
    let clamps = renderer.state_clamps();
    if resets != 0 {
        notes.push(format!("stress battery: {resets} stability resets"));
    }
    (resets, clamps, notes)
}

#[derive(Debug, Clone, Copy)]
enum StressScenario {
    Dc(f64),
    SineDb(f64),
}

fn annotate_dsd_improvements(results: &mut [DsdMeasurement], config: DsdExperimentConfig) {
    if config.has_rate_tweaks() {
        return;
    }

    for source_rate in [44_100, 48_000] {
        for modulator in [
            "Standard",
            "EcDepth1",
            "EcDepth2",
            "EcDepth3",
            "EcDepth4",
            "EcDepth8",
            "EcDepth4Adaptive",
        ] {
            for filter in [
                "Minimum16k",
                "Minimum16k",
                "Split128k",
                "SincExtreme32k",
                "Split128k",
            ] {
                let baseline = results
                    .iter()
                    .find(|m| {
                        m.modulator == modulator
                            && m.filter == filter
                            && m.source_rate == source_rate
                            && m.dsd_rate == "DSD128"
                    })
                    .and_then(|m| m.residual_db);
                if let Some(baseline) = baseline {
                    for result in results.iter_mut().filter(|m| {
                        m.modulator == modulator
                            && m.filter == filter
                            && m.source_rate == source_rate
                            && m.dsd_rate == "DSD256"
                    }) {
                        if let Some(residual) = result.residual_db {
                            result.dsd256_improvement_db = Some(baseline - residual);
                        }
                    }
                }
            }
        }
    }
}

fn run_resampler_mono(filter: FilterType, rate: RateCase, input: &[f64]) -> Vec<f64> {
    run_resampler_mono_with_flush(filter, rate, input, 0)
}

fn run_resampler_mono_with_flush(
    filter: FilterType,
    rate: RateCase,
    input: &[f64],
    flush_blocks: usize,
) -> Vec<f64> {
    let mut resampler = SincResampler::new(filter, rate.source, rate.target);
    let zeros = vec![0.0; CHUNK_FRAMES];
    let mut output = Vec::new();
    let mut block = Vec::new();

    for start in (0..input.len()).step_by(CHUNK_FRAMES) {
        let end = (start + CHUNK_FRAMES).min(input.len());
        resampler.input(&input[start..end], &input[start..end]);
        block.clear();
        resampler.process(&mut block);
        output.extend(block.iter().step_by(2).copied());
    }

    for _ in 0..flush_blocks {
        resampler.input(&zeros, &zeros);
        block.clear();
        resampler.process(&mut block);
        output.extend(block.iter().step_by(2).copied());
    }

    output
}

fn measure_dc_gain(filter: FilterType, rate: RateCase, seconds: f64) -> Option<f64> {
    let frames = (rate.source as f64 * seconds).round() as usize;
    let input = vec![1.0; frames.max(minimum_measurement_frames(filter))];
    let output = run_resampler_mono(filter, rate, &input);
    let slice = settled_slice(&output)?;
    let mean = slice.iter().sum::<f64>() / slice.len() as f64;
    Some(db(mean.abs().max(1e-18)))
}

fn measure_tone_gain(filter: FilterType, rate: RateCase, freq: f64, seconds: f64) -> Option<f64> {
    let frames = (rate.source as f64 * seconds).round() as usize;
    let input = sine(
        frames.max(minimum_measurement_frames(filter)),
        rate.source,
        freq,
        0.5,
    );
    let output = run_resampler_mono(filter, rate, &input);
    let slice = settled_slice(&output)?;
    let amp = tone_amplitude(slice, rate.target, freq);
    Some(db((amp / 0.5).max(1e-18)))
}

fn measure_passband_profile(filter: FilterType, rate: RateCase, seconds: f64) -> PassbandProfile {
    let seconds = seconds.max(1.0);
    let gain_20hz_db = measure_tone_gain(filter, rate, 20.0, seconds);
    let gain_1k_db = measure_tone_gain(filter, rate, 1_000.0, seconds);
    let gain_3k_db = measure_tone_gain(filter, rate, 3_000.0, seconds);
    let gain_6k_db = measure_tone_gain(filter, rate, 6_000.0, seconds);
    let gain_10k_db = measure_tone_gain(filter, rate, 10_000.0, seconds);
    let gain_18k_db = measure_tone_gain(filter, rate, 18_000.0, seconds);
    let gain_20k_db = measure_tone_gain(filter, rate, 20_000.0, seconds);
    let sweep_gains = [
        gain_20hz_db,
        gain_1k_db,
        gain_3k_db,
        gain_6k_db,
        gain_10k_db,
        gain_18k_db,
        gain_20k_db,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let max_deviation_20hz_20khz_db = if sweep_gains.len() >= 3 {
        Some(
            sweep_gains
                .iter()
                .copied()
                .map(f64::abs)
                .fold(0.0, f64::max),
        )
    } else {
        None
    };
    let peak_gain_20hz_20khz_db = if sweep_gains.len() >= 3 {
        Some(
            sweep_gains
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max),
        )
    } else {
        None
    };
    PassbandProfile {
        max_deviation_20hz_20khz_db,
        peak_gain_20hz_20khz_db,
        gain_1k_db,
        gain_3k_db,
        gain_6k_db,
        gain_10k_db,
        gain_18k_db,
    }
}

fn passband_ripple(filter: FilterType, rate: RateCase, seconds: f64) -> Option<f64> {
    measure_passband_profile(filter, rate, seconds).max_deviation_20hz_20khz_db
}

fn measure_image_rejection(filter: FilterType, rate: RateCase, seconds: f64) -> Option<f64> {
    let frames = (rate.source as f64 * seconds).round() as usize;
    let input = sine(
        frames.max(minimum_measurement_frames(filter)),
        rate.source,
        997.0,
        0.5,
    );
    let output = run_resampler_mono(filter, rate, &input);
    let slice = settled_slice(&output)?;
    let tone = tone_amplitude(slice, rate.target, 997.0).max(1e-18);
    let image = max_band_amplitude(
        slice,
        rate.target,
        rate.source as f64 * 0.54,
        rate.target as f64 * 0.49,
    )?;
    Some(db((tone / image.max(1e-18)).max(1e-18)))
}

fn minimum_measurement_frames(filter: FilterType) -> usize {
    match filter {
        FilterType::Minimum16k => 32_768,
        FilterType::SincExtreme32k => 65_536,
        FilterType::LinearPhase128k
        | FilterType::Split128k
        | FilterType::Split128kV2
        | FilterType::SplitPhase128kV3
        | FilterType::SplitPhase128kV4 => 131_072,
        FilterType::IntegratedPhase128k
        | FilterType::IntegratedPhase128kV2
        | FilterType::IntegratedPhase128kV3
        | FilterType::IntegratedPhase128kV4
        | FilterType::MinimumPhase128k
        | FilterType::MinimumPhase128kV2
        | FilterType::MinimumPhase128kV3
        | FilterType::MinimumPhase128kV4 => 131_072,
        FilterType::MinimumPhaseCompact128k => 131_072,
        FilterType::MinimumPhaseCompact128kV2 | FilterType::SmoothPhase128k => 131_072,
    }
}

fn measure_impulse(
    filter: FilterType,
    rate: RateCase,
    gate: GateClass,
    notes: &mut Vec<String>,
) -> (Option<usize>, Option<f64>, Option<f64>) {
    let frames = match gate {
        GateClass::ReportOnly => 16_384,
        GateClass::Minimum16k | GateClass::Extreme => 65_536,
    }
    .max(minimum_measurement_frames(filter));
    let mut input = vec![0.0; frames];
    input[0] = 1.0;
    let output = run_resampler_mono(filter, rate, &input);
    if output.is_empty() {
        notes.push("impulse produced no output in bounded measurement window".to_string());
        return (None, None, None);
    }
    let (peak_idx, peak) = output
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .map(|(idx, sample)| (idx, sample.abs()))
        .unwrap();
    let total: f64 = output.iter().map(|x| x * x).sum::<f64>().max(1e-30);
    let pre: f64 = output[..peak_idx].iter().map(|x| x * x).sum();
    let post: f64 = output[peak_idx.saturating_add(1)..]
        .iter()
        .map(|x| x * x)
        .sum();
    let peak_energy = (peak * peak).max(1e-30);
    (
        Some(peak_idx),
        Some(db((pre / total).sqrt().max(1e-18)).min(db((pre / peak_energy).sqrt().max(1e-18)))),
        Some(db((post / total).sqrt().max(1e-18)).min(db((post / peak_energy).sqrt().max(1e-18)))),
    )
}

fn measure_throughput(
    filter: FilterType,
    rate: RateCase,
    seconds: f64,
) -> (Option<f64>, Option<f64>) {
    let frames = (rate.source as f64 * seconds).round() as usize;
    let frames = frames.max(minimum_measurement_frames(filter)).max(4096);
    let left = multitone(frames, rate.source, 0.5);
    let right = multitone(frames, rate.source, 0.45);
    let mut resampler = SincResampler::new(filter, rate.source, rate.target);
    let mut output = Vec::new();
    let mut frames_out = 0usize;
    let start_time = Instant::now();
    for start in (0..left.len()).step_by(CHUNK_FRAMES) {
        let end = (start + CHUNK_FRAMES).min(left.len());
        resampler.input(&left[start..end], &right[start..end]);
        output.clear();
        frames_out += resampler.process(&mut output);
        black_box(&output);
    }
    let elapsed = start_time.elapsed();
    if frames_out == 0 {
        return (None, None);
    }
    let ns = elapsed.as_nanos() as f64 / frames_out as f64;
    let audio_seconds = frames_out as f64 / rate.target as f64;
    let core = elapsed.as_secs_f64() / audio_seconds * 100.0;
    (Some(ns), Some(core))
}

fn settled_slice(output: &[f64]) -> Option<&[f64]> {
    if output.len() < 2048 {
        return None;
    }
    let start = output.len() / 4;
    let end = output.len() * 3 / 4;
    (end > start + 1024).then_some(&output[start..end])
}

fn sine(frames: usize, sample_rate: u32, freq: f64, amp: f64) -> Vec<f64> {
    (0..frames)
        .map(|i| amp * (2.0 * PI * freq * i as f64 / sample_rate as f64).sin())
        .collect()
}

fn multitone(frames: usize, sample_rate: u32, amp: f64) -> Vec<f64> {
    (0..frames)
        .map(|i| {
            let t = i as f64 / sample_rate as f64;
            amp * (0.52 * (2.0 * PI * 997.0 * t).sin()
                + 0.31 * (2.0 * PI * 7_321.0 * t).sin()
                + 0.17 * (2.0 * PI * 17_101.0 * t).sin())
        })
        .collect()
}

fn program_multitone(frames: usize, sample_rate: u32, amp: f64) -> (Vec<f64>, Vec<f64>) {
    let tones = vec![
        137.0, 233.0, 311.0, 499.0, 997.0, 1_697.0, 2_711.0, 3_911.0, 5_839.0, 7_321.0, 10_039.0,
        13_733.0, 17_101.0,
    ];
    let weights = [
        0.22, 0.20, 0.18, 0.16, 0.15, 0.13, 0.11, 0.10, 0.085, 0.075, 0.060, 0.050, 0.040,
    ];
    let norm = weights.iter().sum::<f64>().max(1e-18);
    let signal = (0..frames)
        .map(|i| {
            let t = i as f64 / sample_rate as f64;
            let sample = tones
                .iter()
                .zip(weights.iter())
                .enumerate()
                .map(|(idx, (freq, weight))| {
                    let phase = 0.37 * idx as f64;
                    weight * (2.0 * PI * freq * t + phase).sin()
                })
                .sum::<f64>();
            amp * sample / norm
        })
        .collect();
    (signal, tones)
}

#[derive(Debug)]
struct NamedProbe {
    label: &'static str,
    input: Vec<f64>,
    click_settle: usize,
}

const DSD128_PROGRAM_PROBE_LABELS: [&str; 7] = [
    "sparse_piano",
    "female_vocal_reverb_tail",
    "brushed_cymbals",
    "low_level_acoustic_guitar",
    "dense_rock_chorus",
    "sub_bass_plus_treble_transient",
    "fade_in_fade_out",
];

fn dsd128_program_probes(frames: usize, sample_rate: u32) -> Vec<NamedProbe> {
    let settle = (sample_rate as f64 * 0.030).round() as usize;
    vec![
        NamedProbe {
            label: "sparse_piano",
            input: sparse_piano_probe(frames, sample_rate),
            click_settle: settle,
        },
        NamedProbe {
            label: "female_vocal_reverb_tail",
            input: female_vocal_reverb_probe(frames, sample_rate),
            click_settle: settle,
        },
        NamedProbe {
            label: "brushed_cymbals",
            input: brushed_cymbals_probe(frames, sample_rate),
            click_settle: settle,
        },
        NamedProbe {
            label: "low_level_acoustic_guitar",
            input: low_level_acoustic_guitar_probe(frames, sample_rate),
            click_settle: settle,
        },
        NamedProbe {
            label: "dense_rock_chorus",
            input: dense_rock_chorus_probe(frames, sample_rate),
            click_settle: settle,
        },
        NamedProbe {
            label: "sub_bass_plus_treble_transient",
            input: sub_bass_treble_transient_probe(frames, sample_rate),
            click_settle: settle,
        },
        NamedProbe {
            label: "fade_in_fade_out",
            input: fade_in_fade_out_probe(frames, sample_rate),
            click_settle: settle,
        },
    ]
}

fn dsd_transient_probes(frames: usize, sample_rate: u32) -> Vec<NamedProbe> {
    let settle = (sample_rate as f64 * 0.030).round() as usize;
    vec![
        NamedProbe {
            label: "overload_recovery",
            input: recovery_probe(frames),
            click_settle: 16,
        },
        NamedProbe {
            label: "track_seek_pause_resume_new_track",
            input: transport_transition_probe(frames, sample_rate),
            click_settle: settle,
        },
    ]
}

fn sparse_piano_probe(frames: usize, sample_rate: u32) -> Vec<f64> {
    let notes = [
        (0.04, 261.63, 0.32),
        (0.29, 329.63, 0.24),
        (0.53, 392.00, 0.20),
        (0.71, 523.25, 0.16),
    ];
    (0..frames)
        .map(|idx| {
            let t = idx as f64 / sample_rate as f64;
            notes
                .iter()
                .map(|(start, freq, amp)| {
                    if t < *start {
                        return 0.0;
                    }
                    let local = t - start;
                    let strike = (-local / 0.24).exp();
                    let body = (2.0 * PI * freq * t).sin()
                        + 0.45 * (2.0 * PI * freq * 2.01 * t + 0.2).sin()
                        + 0.22 * (2.0 * PI * freq * 3.02 * t + 0.6).sin();
                    amp * strike * body / 1.67
                })
                .sum::<f64>()
                .clamp(-0.90, 0.90)
        })
        .collect()
}

fn female_vocal_reverb_probe(frames: usize, sample_rate: u32) -> Vec<f64> {
    let dry: Vec<f64> = (0..frames)
        .map(|idx| {
            let t = idx as f64 / sample_rate as f64;
            let phrase_env = if t < 0.12 {
                smoothstep(t / 0.12)
            } else if t < 0.58 {
                1.0
            } else {
                (1.0 - smoothstep((t - 0.58) / 0.24)).max(0.0)
            };
            let vibrato = 1.0 + 0.012 * (2.0 * PI * 5.4 * t).sin();
            let f0 = 232.0 * vibrato;
            let carrier = 0.52 * (2.0 * PI * f0 * t).sin()
                + 0.30 * (2.0 * PI * 2.0 * f0 * t + 0.4).sin()
                + 0.16 * (2.0 * PI * 3.0 * f0 * t + 1.0).sin();
            let formants = 0.20 * (2.0 * PI * 720.0 * t + 0.3).sin()
                + 0.12 * (2.0 * PI * 1_180.0 * t + 1.1).sin()
                + 0.07 * (2.0 * PI * 2_700.0 * t + 0.6).sin();
            0.30 * phrase_env * (carrier + formants)
        })
        .collect();
    let delay_a = (sample_rate as f64 * 0.071).round() as usize;
    let delay_b = (sample_rate as f64 * 0.113).round() as usize;
    (0..frames)
        .map(|idx| {
            let tail_a = idx
                .checked_sub(delay_a)
                .and_then(|src| dry.get(src).copied())
                .unwrap_or(0.0)
                * 0.28;
            let tail_b = idx
                .checked_sub(delay_b)
                .and_then(|src| dry.get(src).copied())
                .unwrap_or(0.0)
                * 0.18;
            (dry[idx] + tail_a + tail_b).clamp(-0.85, 0.85)
        })
        .collect()
}

fn brushed_cymbals_probe(frames: usize, sample_rate: u32) -> Vec<f64> {
    let mut last_noise = 0.0;
    (0..frames)
        .map(|idx| {
            let t = idx as f64 / sample_rate as f64;
            let noise = deterministic_noise(idx);
            let high = noise - 0.82 * last_noise;
            last_noise = noise;
            let sweep = 0.5 + 0.5 * (2.0 * PI * 1.7 * t).sin();
            let metallic = 0.22 * (2.0 * PI * 7_900.0 * t + 0.4).sin()
                + 0.18 * (2.0 * PI * 11_300.0 * t + 1.1).sin()
                + 0.12 * (2.0 * PI * 15_700.0 * t + 0.2).sin();
            (0.16 * (0.35 + 0.65 * sweep) * high + 0.05 * metallic).clamp(-0.72, 0.72)
        })
        .collect()
}

fn low_level_acoustic_guitar_probe(frames: usize, sample_rate: u32) -> Vec<f64> {
    let notes = [
        (0.02, 110.0),
        (0.14, 146.83),
        (0.26, 196.0),
        (0.38, 246.94),
        (0.50, 329.63),
        (0.62, 392.0),
    ];
    (0..frames)
        .map(|idx| {
            let t = idx as f64 / sample_rate as f64;
            let plucks = notes
                .iter()
                .map(|(start, freq)| {
                    if t < *start {
                        return 0.0;
                    }
                    let local = t - start;
                    let pick = (-local / 0.006).exp() * (2.0 * PI * 4_800.0 * t).sin();
                    let string = (-local / 0.18).exp()
                        * ((2.0 * PI * freq * t).sin()
                            + 0.38 * (2.0 * PI * freq * 2.0 * t + 0.3).sin()
                            + 0.18 * (2.0 * PI * freq * 3.0 * t + 0.8).sin());
                    pick * 0.10 + string * 0.90
                })
                .sum::<f64>();
            0.010 * plucks
        })
        .collect()
}

fn dense_rock_chorus_probe(frames: usize, sample_rate: u32) -> Vec<f64> {
    (0..frames)
        .map(|idx| {
            let t = idx as f64 / sample_rate as f64;
            let bass =
                0.28 * (2.0 * PI * 82.41 * t).sin() + 0.14 * (2.0 * PI * 164.82 * t + 0.2).sin();
            let guitars = [196.0, 246.94, 293.66, 392.0]
                .iter()
                .enumerate()
                .map(|(voice, freq)| {
                    let phase = voice as f64 * 0.41;
                    let raw = (2.0 * PI * freq * t + phase).sin()
                        + 0.55 * (2.0 * PI * freq * 2.0 * t + phase).sin()
                        + 0.33 * (2.0 * PI * freq * 3.0 * t + phase).sin();
                    raw.tanh()
                })
                .sum::<f64>()
                * 0.13;
            let beat = (t * 2.0).fract();
            let snare = if (beat - 0.5).abs() < 0.018 {
                0.22 * deterministic_noise(idx) * (1.0 - (beat - 0.5).abs() / 0.018)
            } else {
                0.0
            };
            (bass + guitars + snare).clamp(-0.86, 0.86)
        })
        .collect()
}

fn sub_bass_treble_transient_probe(frames: usize, sample_rate: u32) -> Vec<f64> {
    (0..frames)
        .map(|idx| {
            let t = idx as f64 / sample_rate as f64;
            let sub = 0.42 * (2.0 * PI * 34.7 * t).sin();
            let burst = [0.18, 0.47, 0.73]
                .iter()
                .map(|start| {
                    if t < *start {
                        return 0.0;
                    }
                    let local = t - start;
                    (-local / 0.010).exp() * (2.0 * PI * 13_800.0 * t + 0.7).sin() * 0.28
                })
                .sum::<f64>();
            (sub + burst).clamp(-0.88, 0.88)
        })
        .collect()
}

fn fade_in_fade_out_probe(frames: usize, sample_rate: u32) -> Vec<f64> {
    let (tone, _) = program_multitone(frames, sample_rate, 0.55);
    tone.into_iter()
        .enumerate()
        .map(|(idx, sample)| {
            let phase = idx as f64 / frames.max(1) as f64;
            let fade = if phase < 0.35 {
                smoothstep(phase / 0.35)
            } else if phase > 0.65 {
                1.0 - smoothstep((phase - 0.65) / 0.35)
            } else {
                1.0
            };
            fade * sample
        })
        .collect()
}

fn transport_transition_probe(frames: usize, sample_rate: u32) -> Vec<f64> {
    (0..frames)
        .map(|idx| {
            let t = idx as f64 / sample_rate as f64;
            let phase = idx as f64 / frames.max(1) as f64;
            let sample = if phase < 0.22 {
                0.30 * (2.0 * PI * 440.0 * t).sin() + 0.12 * (2.0 * PI * 1_320.0 * t).sin()
            } else if phase < 0.30 {
                0.0
            } else if phase < 0.50 {
                0.26 * (2.0 * PI * 554.37 * (t + 3.7)).sin()
                    + 0.10 * (2.0 * PI * 1_662.0 * (t + 3.7)).sin()
            } else if phase < 0.62 {
                0.0
            } else if phase < 0.80 {
                0.24 * (2.0 * PI * 659.25 * t + 0.8).sin()
                    + 0.09 * (2.0 * PI * 1_977.0 * t + 0.2).sin()
            } else {
                0.34 * (2.0 * PI * 220.0 * t + 1.2).sin()
                    + 0.17 * (2.0 * PI * 880.0 * t + 0.4).sin()
            };
            transition_boundary_fade(phase) * sample
        })
        .collect()
}

fn two_tone(frames: usize, sample_rate: u32, freq_a: f64, freq_b: f64, amp: f64) -> Vec<f64> {
    (0..frames)
        .map(|i| {
            let t = i as f64 / sample_rate as f64;
            amp * ((2.0 * PI * freq_a * t).sin() + (2.0 * PI * freq_b * t).sin())
        })
        .collect()
}

fn recovery_probe(frames: usize) -> Vec<f64> {
    let mut input = vec![0.0; frames];
    let start = frames / 4;
    for (idx, value) in [0.98, -0.98, 0.92, -0.92, 0.0, 0.0, 0.75, -0.75]
        .into_iter()
        .enumerate()
    {
        if start + idx < input.len() {
            input[start + idx] = value;
        }
    }
    input
}

fn transition_boundary_fade(phase: f64) -> f64 {
    const BOUNDARIES: [f64; 5] = [0.22, 0.30, 0.50, 0.62, 0.80];
    let fade_width = 0.012;
    BOUNDARIES
        .iter()
        .map(|boundary| {
            let distance = (phase - boundary).abs();
            if distance >= fade_width {
                1.0
            } else {
                smoothstep(distance / fade_width)
            }
        })
        .fold(1.0, f64::min)
}

fn smoothstep(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn deterministic_noise(idx: usize) -> f64 {
    let mut x = idx as u64;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    let value = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
    let mantissa = (value >> 11) as f64 / ((1u64 << 53) as f64);
    2.0 * mantissa - 1.0
}

fn tone_amplitude(signal: &[f64], sample_rate: u32, freq: f64) -> f64 {
    if signal.is_empty() {
        return 0.0;
    }
    let mut re = 0.0;
    let mut im = 0.0;
    for (idx, sample) in signal.iter().enumerate() {
        let window = hann(idx, signal.len());
        let phase = -2.0 * PI * freq * idx as f64 / sample_rate as f64;
        re += sample * window * phase.cos();
        im += sample * window * phase.sin();
    }
    let coherent_gain = signal
        .iter()
        .enumerate()
        .map(|(idx, _)| hann(idx, signal.len()))
        .sum::<f64>()
        / signal.len() as f64;
    2.0 * (re * re + im * im).sqrt() / (signal.len() as f64 * coherent_gain.max(1e-18))
}

#[derive(Debug)]
struct ToneResidual {
    error: Vec<f64>,
    relative_db: Option<f64>,
}

fn fitted_tone_residual(signal: &[f64], sample_rate: u32, freqs: &[f64]) -> Option<ToneResidual> {
    if signal.len() < 1024 || freqs.is_empty() {
        return None;
    }
    let mean = signal.iter().sum::<f64>() / signal.len() as f64;
    let mut fitted = vec![mean; signal.len()];
    for &freq in freqs {
        let mut sin_dot = 0.0;
        let mut cos_dot = 0.0;
        let mut sin_energy = 0.0;
        let mut cos_energy = 0.0;
        for (idx, sample) in signal.iter().enumerate() {
            let phase = 2.0 * PI * freq * idx as f64 / sample_rate as f64;
            let sin = phase.sin();
            let cos = phase.cos();
            let centered = sample - mean;
            sin_dot += centered * sin;
            cos_dot += centered * cos;
            sin_energy += sin * sin;
            cos_energy += cos * cos;
        }
        let sin_gain = sin_dot / sin_energy.max(1e-18);
        let cos_gain = cos_dot / cos_energy.max(1e-18);
        for (idx, sample) in fitted.iter_mut().enumerate() {
            let phase = 2.0 * PI * freq * idx as f64 / sample_rate as f64;
            *sample += sin_gain * phase.sin() + cos_gain * phase.cos();
        }
    }
    let error: Vec<f64> = signal
        .iter()
        .zip(fitted.iter())
        .map(|(sample, fit)| sample - fit)
        .collect();
    let fit_rms = rms(&fitted);
    Some(ToneResidual {
        relative_db: (fit_rms > 1e-18).then(|| db((rms(&error) / fit_rms).max(1e-18))),
        error,
    })
}

fn max_band_amplitude(signal: &[f64], sample_rate: u32, low_hz: f64, high_hz: f64) -> Option<f64> {
    max_band_peak(signal, sample_rate, low_hz, high_hz).map(|(_, amp)| amp)
}

fn max_band_peak(
    signal: &[f64],
    sample_rate: u32,
    low_hz: f64,
    high_hz: f64,
) -> Option<(f64, f64)> {
    let spectrum = amplitude_spectrum(signal, sample_rate)?;
    spectrum
        .into_iter()
        .filter(|(freq, amp)| *freq >= low_hz && *freq <= high_hz && amp.is_finite())
        .max_by(|(_, left), (_, right)| left.partial_cmp(right).unwrap())
}

fn max_band_peak_dbfs(
    signal: &[f64],
    sample_rate: u32,
    low_hz: f64,
    high_hz: f64,
) -> Option<(f64, f64)> {
    max_band_peak(signal, sample_rate, low_hz, high_hz)
        .map(|(freq, amp)| (freq, db(amp.max(1e-18))))
}

fn max_band_dbfs(signal: &[f64], sample_rate: u32, low_hz: f64, high_hz: f64) -> Option<f64> {
    max_band_amplitude(signal, sample_rate, low_hz, high_hz).map(|amp| db(amp.max(1e-18)))
}

#[derive(Debug)]
struct SpectrumMetrics {
    peak_dbfs: Option<f64>,
    spur_margin_db: Option<f64>,
}

#[derive(Debug)]
struct DsdPowerSpectrum {
    powers: Vec<f64>,
    fft_len: usize,
    sample_rate: u32,
    window_sum: f64,
}

#[derive(Debug, Clone)]
struct DsdInbandMetrics {
    residual_db: f64,
    sinad_db: f64,
    sinad_worst_db: f64,
    sinad_p05_db: f64,
    sinad_p95_db: f64,
    sinad_best_db: f64,
    sinad_spread_db: f64,
    window_count: usize,
    worst_window_start_s: Option<f64>,
    noise_rms_dbfs: f64,
    noise_worst_rms_dbfs: f64,
    peak_spur_dbfs: Option<f64>,
    peak_spur_hz: Option<f64>,
    median_noise_bin_dbfs: Option<f64>,
    p95_noise_bin_dbfs: Option<f64>,
    p99_noise_bin_dbfs: Option<f64>,
    spur_margin_db: Option<f64>,
    spur_margin_to_p95_db: Option<f64>,
    spur_margin_to_p99_db: Option<f64>,
    noise_20_200_dbfs: Option<f64>,
    noise_200_2k_dbfs: Option<f64>,
    noise_2k_8k_dbfs: Option<f64>,
    noise_8k_16k_dbfs: Option<f64>,
    noise_16k_20k_dbfs: Option<f64>,
}

#[derive(Debug, Clone)]
struct DsdStereoInbandMetrics {
    aggregate: DsdInbandMetrics,
    left: DsdInbandMetrics,
    right: DsdInbandMetrics,
    windows: Vec<DsdInbandWindowRow>,
    worst_sinad_mismatch_db: Option<f64>,
}

fn dsd_stereo_inband_tone_metrics(
    bits_l: &[f64],
    bits_r: &[f64],
    wire_rate: u32,
    tone_hz: f64,
    tone_amp: f64,
    target_fft_len: usize,
) -> Option<DsdStereoInbandMetrics> {
    let filtered_l = dsd_inband_prefilter(bits_l, wire_rate)?;
    let filtered_r = dsd_inband_prefilter(bits_r, wire_rate)?;
    let (left, left_windows) = dsd_inband_tone_metrics_and_windows(
        &filtered_l,
        wire_rate,
        tone_hz,
        tone_amp,
        target_fft_len,
    )?;
    let (right, right_windows) = dsd_inband_tone_metrics_and_windows(
        &filtered_r,
        wire_rate,
        tone_hz,
        tone_amp,
        target_fft_len,
    )?;
    let worst_from_left = left.sinad_worst_db <= right.sinad_worst_db;
    let peak_from_left = left.peak_spur_dbfs.unwrap_or(f64::NEG_INFINITY)
        >= right.peak_spur_dbfs.unwrap_or(f64::NEG_INFINITY);
    let aggregate = DsdInbandMetrics {
        residual_db: left.residual_db.max(right.residual_db),
        sinad_db: left.sinad_db.min(right.sinad_db),
        sinad_worst_db: left.sinad_worst_db.min(right.sinad_worst_db),
        sinad_p05_db: left.sinad_p05_db.min(right.sinad_p05_db),
        sinad_p95_db: left.sinad_p95_db.min(right.sinad_p95_db),
        sinad_best_db: left.sinad_best_db.min(right.sinad_best_db),
        sinad_spread_db: left.sinad_spread_db.max(right.sinad_spread_db),
        window_count: left.window_count.min(right.window_count),
        worst_window_start_s: if worst_from_left {
            left.worst_window_start_s
        } else {
            right.worst_window_start_s
        },
        noise_rms_dbfs: left.noise_rms_dbfs.max(right.noise_rms_dbfs),
        noise_worst_rms_dbfs: left.noise_worst_rms_dbfs.max(right.noise_worst_rms_dbfs),
        peak_spur_dbfs: max_opt(left.peak_spur_dbfs, right.peak_spur_dbfs),
        peak_spur_hz: if peak_from_left {
            left.peak_spur_hz
        } else {
            right.peak_spur_hz
        },
        median_noise_bin_dbfs: max_opt(left.median_noise_bin_dbfs, right.median_noise_bin_dbfs),
        p95_noise_bin_dbfs: max_opt(left.p95_noise_bin_dbfs, right.p95_noise_bin_dbfs),
        p99_noise_bin_dbfs: max_opt(left.p99_noise_bin_dbfs, right.p99_noise_bin_dbfs),
        spur_margin_db: min_opt(left.spur_margin_db, right.spur_margin_db),
        spur_margin_to_p95_db: min_opt(left.spur_margin_to_p95_db, right.spur_margin_to_p95_db),
        spur_margin_to_p99_db: min_opt(left.spur_margin_to_p99_db, right.spur_margin_to_p99_db),
        noise_20_200_dbfs: max_opt(left.noise_20_200_dbfs, right.noise_20_200_dbfs),
        noise_200_2k_dbfs: max_opt(left.noise_200_2k_dbfs, right.noise_200_2k_dbfs),
        noise_2k_8k_dbfs: max_opt(left.noise_2k_8k_dbfs, right.noise_2k_8k_dbfs),
        noise_8k_16k_dbfs: max_opt(left.noise_8k_16k_dbfs, right.noise_8k_16k_dbfs),
        noise_16k_20k_dbfs: max_opt(left.noise_16k_20k_dbfs, right.noise_16k_20k_dbfs),
    };
    let mut windows = Vec::with_capacity(left_windows.len() + right_windows.len());
    windows.extend(dsd_inband_window_rows("left", &left_windows));
    windows.extend(dsd_inband_window_rows("right", &right_windows));
    Some(DsdStereoInbandMetrics {
        worst_sinad_mismatch_db: Some((left.sinad_worst_db - right.sinad_worst_db).abs()),
        aggregate,
        left,
        right,
        windows,
    })
}

const TRUST_SAMPLE_RATE: u32 = 262_144;
const TRUST_LEN: usize = 393_216;
const TRUST_TONE_HZ: f64 = 1_000.0;
const TRUST_TONE_AMP: f64 = 0.5;

fn synthetic_trust_signal(
    broadband_tone_dbc: f64,
    narrow_spur: Option<(f64, f64)>,
    ultrasonic: bool,
) -> Vec<f64> {
    let broadband_amp = trust_spur_amp(broadband_tone_dbc);
    let narrow_spur = narrow_spur.map(|(freq, dbc)| (freq, trust_spur_amp(dbc)));
    (0..TRUST_LEN)
        .map(|idx| {
            let t = idx as f64 / TRUST_SAMPLE_RATE as f64;
            let mut sample = TRUST_TONE_AMP * (2.0 * PI * TRUST_TONE_HZ * t).sin();
            sample += broadband_amp * (2.0 * PI * 3_000.0 * t + 0.31).sin();
            if let Some((freq, amp)) = narrow_spur {
                sample += amp * (2.0 * PI * freq * t + 0.73).sin();
            }
            if ultrasonic {
                sample += 0.42 * (2.0 * PI * 96_000.0 * t + 0.11).sin();
                sample += 0.31 * (2.0 * PI * 104_000.0 * t + 0.47).sin();
                sample += 0.23 * (2.0 * PI * 117_000.0 * t + 0.83).sin();
            }
            sample
        })
        .collect()
}

fn trust_spur_amp(dbc: f64) -> f64 {
    TRUST_TONE_AMP * 10.0f64.powf(dbc / 20.0)
}

fn trust_stereo_metrics(bits: &[f64]) -> Option<DsdStereoInbandMetrics> {
    dsd_stereo_inband_tone_metrics(
        bits,
        bits,
        TRUST_SAMPLE_RATE,
        TRUST_TONE_HZ,
        TRUST_TONE_AMP,
        DSD_TONE_WINDOW_FFT_BITS_FULL,
    )
}

fn trust_bin_hz() -> f64 {
    let fft_len = dsd_fft_len(TRUST_LEN, DSD_TONE_WINDOW_FFT_BITS_FULL).unwrap_or(TRUST_LEN);
    TRUST_SAMPLE_RATE as f64 / fft_len as f64
}

fn check_within(
    failures: &mut Vec<String>,
    name: &str,
    actual: f64,
    expected: f64,
    tolerance: f64,
    unit: &str,
) {
    let delta = (actual - expected).abs();
    if delta > tolerance {
        failures.push(format!(
            "{name} {actual:.3} {unit}, expected {expected:.3} +/- {tolerance:.3} {unit}"
        ));
    }
}

fn check_opt_within(
    failures: &mut Vec<String>,
    name: &str,
    actual: Option<f64>,
    expected: f64,
    tolerance: f64,
    unit: &str,
) {
    match actual {
        Some(actual) => check_within(failures, name, actual, expected, tolerance, unit),
        None => failures.push(format!("{name} missing")),
    }
}

fn trust_metric_fingerprint(metrics: &DsdStereoInbandMetrics) -> Vec<u64> {
    let mut bits = Vec::new();
    for metric in [&metrics.aggregate, &metrics.left, &metrics.right] {
        bits.extend([
            metric.residual_db.to_bits(),
            metric.sinad_db.to_bits(),
            metric.sinad_worst_db.to_bits(),
            metric.sinad_p05_db.to_bits(),
            metric.sinad_p95_db.to_bits(),
            metric.sinad_best_db.to_bits(),
            metric.sinad_spread_db.to_bits(),
            metric.noise_rms_dbfs.to_bits(),
            metric.noise_worst_rms_dbfs.to_bits(),
            metric.peak_spur_dbfs.unwrap_or(f64::NAN).to_bits(),
            metric.peak_spur_hz.unwrap_or(f64::NAN).to_bits(),
            metric.spur_margin_db.unwrap_or(f64::NAN).to_bits(),
        ]);
    }
    for window in &metrics.windows {
        bits.extend([
            window.start_s.to_bits(),
            window.sinad_db.to_bits(),
            window.noise_rms_dbfs.to_bits(),
            window.peak_spur_hz.unwrap_or(f64::NAN).to_bits(),
            window.peak_spur_dbfs.unwrap_or(f64::NAN).to_bits(),
        ]);
    }
    bits
}

fn dsd_inband_prefilter(bits: &[f64], sample_rate: u32) -> Option<Vec<f64>> {
    if bits.len() < 1024 {
        return None;
    }
    if sample_rate as f64 <= DSD_INBAND_PREFILTER_PASS_HZ * 2.0 {
        return Some(bits.to_vec());
    }
    let pad_len = dsd_inband_prefilter_pad_len(bits.len(), sample_rate);
    let mut padded_bits = Vec::with_capacity(bits.len() + 2 * pad_len);
    for idx in (1..=pad_len).rev() {
        padded_bits.push(bits[idx]);
    }
    padded_bits.extend_from_slice(bits);
    for idx in 0..pad_len {
        padded_bits.push(bits[bits.len() - 2 - idx]);
    }
    let fft_len = padded_bits.len().next_power_of_two();
    let mut planner = RealFftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(fft_len);
    let inverse = planner.plan_fft_inverse(fft_len);
    let mut time = forward.make_input_vec();
    time[..padded_bits.len()].copy_from_slice(&padded_bits);
    let mut spectrum = forward.make_output_vec();
    forward.process(&mut time, &mut spectrum).ok()?;
    let bin_hz = sample_rate as f64 / fft_len as f64;
    for (bin, value) in spectrum.iter_mut().enumerate() {
        let freq = bin as f64 * bin_hz;
        let gain = dsd_inband_prefilter_gain(freq);
        value.re *= gain;
        value.im *= gain;
    }
    let mut filtered = inverse.make_output_vec();
    inverse.process(&mut spectrum, &mut filtered).ok()?;
    let scale = fft_len as f64;
    let mut cropped = filtered[pad_len..pad_len + bits.len()].to_vec();
    for sample in &mut cropped {
        *sample /= scale;
    }
    Some(cropped)
}

fn dsd_inband_prefilter_pad_len(bit_len: usize, sample_rate: u32) -> usize {
    if bit_len < 3 {
        return 0;
    }
    ((sample_rate as f64 * 0.050).round() as usize)
        .max(1024)
        .min((bit_len - 1) / 2)
}

fn dsd_inband_prefilter_gain(freq: f64) -> f64 {
    if freq <= DSD_INBAND_PREFILTER_PASS_HZ {
        1.0
    } else if freq >= DSD_INBAND_PREFILTER_STOP_HZ {
        0.0
    } else {
        let x = (freq - DSD_INBAND_PREFILTER_PASS_HZ)
            / (DSD_INBAND_PREFILTER_STOP_HZ - DSD_INBAND_PREFILTER_PASS_HZ);
        0.5 + 0.5 * (PI * x).cos()
    }
}

fn dsd_inband_spur_rows(metrics: &DsdStereoInbandMetrics) -> Vec<DsdInbandSpurRow> {
    [
        ("aggregate", &metrics.aggregate),
        ("left", &metrics.left),
        ("right", &metrics.right),
    ]
    .into_iter()
    .map(|(channel, metrics)| DsdInbandSpurRow {
        channel: channel.to_string(),
        peak_spur_hz: metrics.peak_spur_hz,
        peak_spur_dbfs: metrics.peak_spur_dbfs,
        median_noise_bin_dbfs: metrics.median_noise_bin_dbfs,
        p95_noise_bin_dbfs: metrics.p95_noise_bin_dbfs,
        p99_noise_bin_dbfs: metrics.p99_noise_bin_dbfs,
        margin_to_median_db: metrics.spur_margin_db,
        margin_to_p95_db: metrics.spur_margin_to_p95_db,
        margin_to_p99_db: metrics.spur_margin_to_p99_db,
    })
    .collect()
}

fn dsd_inband_tone_metrics(
    bits: &[f64],
    wire_rate: u32,
    tone_hz: f64,
    tone_amp: f64,
    target_fft_len: usize,
) -> Option<DsdInbandMetrics> {
    dsd_inband_tone_metrics_and_windows(bits, wire_rate, tone_hz, tone_amp, target_fft_len)
        .map(|(metrics, _)| metrics)
}

fn dsd_inband_tone_metrics_and_windows(
    bits: &[f64],
    wire_rate: u32,
    tone_hz: f64,
    tone_amp: f64,
    target_fft_len: usize,
) -> Option<(DsdInbandMetrics, Vec<DsdInbandMetrics>)> {
    let fft_len = dsd_fft_len(bits.len(), target_fft_len)?;
    let starts = dsd_distribution_window_starts(bits.len(), fft_len, wire_rate, target_fft_len);
    let mut windows = Vec::new();
    for start in starts {
        let spectrum = dsd_power_spectrum_window(bits, wire_rate, fft_len, start)?;
        let mut metrics = dsd_inband_tone_metrics_from_spectrum(&spectrum, tone_hz, tone_amp)?;
        metrics.worst_window_start_s = Some(start as f64 / wire_rate as f64);
        windows.push(metrics);
    }
    if windows.is_empty() {
        return None;
    }
    windows.sort_by(|a, b| a.sinad_db.partial_cmp(&b.sinad_db).unwrap());
    let worst = windows.first()?.sinad_db;
    let best = windows.last()?.sinad_db;
    let p05 = percentile_by_rank(&windows, 0.05, |window| window.sinad_db)?;
    let p95 = percentile_by_rank(&windows, 0.95, |window| window.sinad_db)?;
    let noise_worst_rms_dbfs = windows
        .iter()
        .map(|window| window.noise_rms_dbfs)
        .fold(f64::NEG_INFINITY, f64::max);
    let mut median = windows[windows.len() / 2].clone();
    median.sinad_worst_db = worst;
    median.worst_window_start_s = windows
        .first()
        .and_then(|window| window.worst_window_start_s);
    median.sinad_p05_db = p05;
    median.sinad_p95_db = p95;
    median.sinad_best_db = best;
    median.sinad_spread_db = best - worst;
    median.window_count = windows.len();
    median.noise_worst_rms_dbfs = noise_worst_rms_dbfs;
    Some((median, windows))
}

fn dsd_inband_window_rows(channel: &str, windows: &[DsdInbandMetrics]) -> Vec<DsdInbandWindowRow> {
    let worst_sinad = windows
        .iter()
        .map(|window| window.sinad_db)
        .fold(f64::INFINITY, f64::min);
    windows
        .iter()
        .map(|window| DsdInbandWindowRow {
            channel: channel.to_string(),
            start_s: window.worst_window_start_s.unwrap_or(0.0),
            sinad_db: window.sinad_db,
            noise_rms_dbfs: window.noise_rms_dbfs,
            peak_spur_hz: window.peak_spur_hz,
            peak_spur_dbfs: window.peak_spur_dbfs,
            noise_20_200_dbfs: window.noise_20_200_dbfs,
            noise_200_2k_dbfs: window.noise_200_2k_dbfs,
            noise_2k_8k_dbfs: window.noise_2k_8k_dbfs,
            noise_8k_16k_dbfs: window.noise_8k_16k_dbfs,
            noise_16k_20k_dbfs: window.noise_16k_20k_dbfs,
            is_worst: (window.sinad_db - worst_sinad).abs() <= f64::EPSILON,
        })
        .collect()
}

#[derive(Debug, Default)]
struct DsdUltrasonicMetrics {
    ultrasonic_24_50k_max_dbfs: Option<f64>,
    ultrasonic_24_50k_median_dbfs: Option<f64>,
    ultrasonic_24_50k_window_spread_db: Option<f64>,
    ultrasonic_50_100k_max_dbfs: Option<f64>,
    ultrasonic_50_100k_median_dbfs: Option<f64>,
    ultrasonic_50_100k_window_spread_db: Option<f64>,
    ultrasonic_100_200k_max_dbfs: Option<f64>,
    ultrasonic_100_200k_median_dbfs: Option<f64>,
    ultrasonic_100_200k_window_spread_db: Option<f64>,
    windows: Vec<DsdUltrasonicWindowRow>,
}

fn dsd_ultrasonic_metrics(
    bits_l: &[f64],
    bits_r: &[f64],
    wire_rate: u32,
    target_fft_len: usize,
) -> DsdUltrasonicMetrics {
    let bit_len = bits_l.len().min(bits_r.len());
    let Some(fft_len) = dsd_fft_len(bit_len, target_fft_len) else {
        return DsdUltrasonicMetrics::default();
    };
    let starts = dsd_distribution_window_starts(bit_len, fft_len, wire_rate, target_fft_len);
    let mut windows = Vec::with_capacity(starts.len() * 2);
    for (channel, bits) in [("left", bits_l), ("right", bits_r)] {
        for &start in &starts {
            let Some(spectrum) = dsd_power_spectrum_window(bits, wire_rate, fft_len, start) else {
                continue;
            };
            windows.push(DsdUltrasonicWindowRow {
                channel: channel.to_string(),
                start_s: start as f64 / wire_rate as f64,
                end_s: (start + fft_len) as f64 / wire_rate as f64,
                ultrasonic_24_50k_dbfs: dsd_spectrum_band_rms_dbfs(&spectrum, 24_000.0, 50_000.0),
                ultrasonic_50_100k_dbfs: dsd_spectrum_band_rms_dbfs(&spectrum, 50_000.0, 100_000.0),
                ultrasonic_100_200k_dbfs: dsd_spectrum_band_rms_dbfs(
                    &spectrum, 100_000.0, 200_000.0,
                ),
            });
        }
    }

    let (
        ultrasonic_24_50k_max_dbfs,
        ultrasonic_24_50k_median_dbfs,
        ultrasonic_24_50k_window_spread_db,
    ) = dsd_ultrasonic_band_stats(&windows, |window| window.ultrasonic_24_50k_dbfs);
    let (
        ultrasonic_50_100k_max_dbfs,
        ultrasonic_50_100k_median_dbfs,
        ultrasonic_50_100k_window_spread_db,
    ) = dsd_ultrasonic_band_stats(&windows, |window| window.ultrasonic_50_100k_dbfs);
    let (
        ultrasonic_100_200k_max_dbfs,
        ultrasonic_100_200k_median_dbfs,
        ultrasonic_100_200k_window_spread_db,
    ) = dsd_ultrasonic_band_stats(&windows, |window| window.ultrasonic_100_200k_dbfs);

    DsdUltrasonicMetrics {
        ultrasonic_24_50k_max_dbfs,
        ultrasonic_24_50k_median_dbfs,
        ultrasonic_24_50k_window_spread_db,
        ultrasonic_50_100k_max_dbfs,
        ultrasonic_50_100k_median_dbfs,
        ultrasonic_50_100k_window_spread_db,
        ultrasonic_100_200k_max_dbfs,
        ultrasonic_100_200k_median_dbfs,
        ultrasonic_100_200k_window_spread_db,
        windows,
    }
}

fn dsd_spectrum_band_rms_dbfs(
    spectrum: &DsdPowerSpectrum,
    low_hz: f64,
    high_hz: f64,
) -> Option<f64> {
    let nyquist = spectrum.sample_rate as f64 * 0.5;
    if low_hz >= nyquist {
        return None;
    }
    let high_hz = high_hz.min(nyquist);
    let bin_hz = spectrum.sample_rate as f64 / spectrum.fft_len as f64;
    let low_bin = (low_hz / bin_hz).ceil().max(1.0) as usize;
    let high_bin = ((high_hz / bin_hz).floor() as usize).min(spectrum.powers.len() - 1);
    if high_bin < low_bin {
        return None;
    }
    let power: f64 = spectrum.powers[low_bin..=high_bin]
        .iter()
        .copied()
        .filter(|power| power.is_finite())
        .map(|power| power.max(f64::MIN_POSITIVE))
        .sum();
    if power <= f64::MIN_POSITIVE {
        return None;
    }
    Some(db(
        (2.0 * power).sqrt() / spectrum.window_sum.max(f64::MIN_POSITIVE)
    ))
}

fn dsd_ultrasonic_band_stats<F>(
    windows: &[DsdUltrasonicWindowRow],
    mut value: F,
) -> (Option<f64>, Option<f64>, Option<f64>)
where
    F: FnMut(&DsdUltrasonicWindowRow) -> Option<f64>,
{
    let mut values: Vec<f64> = windows
        .iter()
        .filter_map(|window| value(window).filter(|value| value.is_finite()))
        .collect();
    if values.is_empty() {
        return (None, None, None);
    }
    values.sort_by(|left, right| left.total_cmp(right));
    let min = values[0];
    let median = values[values.len() / 2];
    let max = values[values.len() - 1];
    (Some(max), Some(median), Some(max - min))
}

fn dsd_artifact_window_stats(windows: &[DsdInbandWindowRow]) -> DsdArtifactWindowStats {
    let mut sinads: Vec<f64> = windows
        .iter()
        .map(|window| window.sinad_db)
        .filter(|value| value.is_finite())
        .collect();
    if sinads.is_empty() {
        return DsdArtifactWindowStats::default();
    }
    sinads.sort_by(|a, b| a.total_cmp(b));
    let median = sinads[sinads.len() / 2];
    let bad_threshold = median - 20.0;
    let bad_window_count = sinads
        .iter()
        .filter(|sinad| **sinad < bad_threshold)
        .count();
    let artifact_free_worst_sinad_db = sinads
        .iter()
        .copied()
        .filter(|sinad| *sinad >= bad_threshold)
        .min_by(|a, b| a.total_cmp(b));
    DsdArtifactWindowStats {
        bad_window_count,
        bad_window_ratio: Some(bad_window_count as f64 / sinads.len() as f64),
        artifact_free_worst_sinad_db,
    }
}

fn dsd_inband_tone_metrics_from_spectrum(
    spectrum: &DsdPowerSpectrum,
    tone_hz: f64,
    tone_amp: f64,
) -> Option<DsdInbandMetrics> {
    let bin_hz = spectrum.sample_rate as f64 / spectrum.fft_len as f64;
    let low_bin = (20.0 / bin_hz).ceil().max(1.0) as usize;
    let high_bin = ((20_000.0 / bin_hz).floor() as usize).min(spectrum.powers.len() - 1);
    if high_bin <= low_bin {
        return None;
    }
    let signal_bin = (tone_hz / bin_hz).round() as usize;
    if signal_bin <= low_bin || signal_bin >= high_bin {
        return None;
    }
    let guard = ((30.0 / bin_hz).ceil() as usize).max(16);
    let signal_lo = signal_bin.saturating_sub(guard).max(low_bin);
    let signal_hi = (signal_bin + guard).min(high_bin);
    let signal_power: f64 = spectrum.powers[signal_lo..=signal_hi].iter().sum();
    if signal_power <= f64::MIN_POSITIVE {
        return None;
    }

    let peak_high_bin = ((DSD_SPUR_PEAK_HIGH_HZ / bin_hz).floor() as usize).min(high_bin);
    let mut noise_powers = Vec::new();
    let mut tonal_noise_powers = Vec::new();
    let mut peak_noise_bin = None;
    let mut peak_noise_power = f64::NEG_INFINITY;
    for bin in low_bin..=high_bin {
        if bin < signal_lo || bin > signal_hi {
            let power = spectrum.powers[bin];
            if power.is_finite() {
                let power = power.max(f64::MIN_POSITIVE);
                noise_powers.push(power);
                if bin <= peak_high_bin {
                    tonal_noise_powers.push(power);
                    if power > peak_noise_power {
                        peak_noise_power = power;
                        peak_noise_bin = Some(bin);
                    }
                }
            }
        }
    }
    if noise_powers.is_empty() {
        return None;
    }
    let noise_power: f64 = noise_powers.iter().sum();
    let sinad_db = 10.0 * (signal_power / noise_power.max(f64::MIN_POSITIVE)).log10();
    let residual_db = -sinad_db;
    let signal_rms = tone_amp.abs() / 2.0f64.sqrt();
    let noise_rms_dbfs = db(signal_rms * (noise_power / signal_power).sqrt());
    let noise_20_200_dbfs = dsd_noise_bucket_dbfs(
        spectrum,
        20.0,
        200.0,
        (signal_lo, signal_hi),
        signal_power,
        signal_rms,
    );
    let noise_200_2k_dbfs = dsd_noise_bucket_dbfs(
        spectrum,
        200.0,
        2_000.0,
        (signal_lo, signal_hi),
        signal_power,
        signal_rms,
    );
    let noise_2k_8k_dbfs = dsd_noise_bucket_dbfs(
        spectrum,
        2_000.0,
        8_000.0,
        (signal_lo, signal_hi),
        signal_power,
        signal_rms,
    );
    let noise_8k_16k_dbfs = dsd_noise_bucket_dbfs(
        spectrum,
        8_000.0,
        16_000.0,
        (signal_lo, signal_hi),
        signal_power,
        signal_rms,
    );
    let noise_16k_20k_dbfs = dsd_noise_bucket_dbfs(
        spectrum,
        16_000.0,
        20_000.0,
        (signal_lo, signal_hi),
        signal_power,
        signal_rms,
    );

    if tonal_noise_powers.is_empty() {
        tonal_noise_powers = noise_powers.clone();
    }
    tonal_noise_powers.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let peak_power = *tonal_noise_powers.last().unwrap();
    let median_power = tonal_noise_powers[tonal_noise_powers.len() / 2].max(f64::MIN_POSITIVE);
    let p95_power = percentile_by_rank(&tonal_noise_powers, 0.95, |power| *power)
        .unwrap_or(median_power)
        .max(f64::MIN_POSITIVE);
    let p99_power = percentile_by_rank(&tonal_noise_powers, 0.99, |power| *power)
        .unwrap_or(p95_power)
        .max(f64::MIN_POSITIVE);
    let noise_bin_dbfs =
        |power: f64| db(tone_amp.abs() * (power / signal_power).sqrt()).max(-360.0);
    let peak_spur_dbfs = Some(db(tone_amp.abs() * (peak_power / signal_power).sqrt()));
    let peak_spur_hz = peak_noise_bin.map(|bin| bin as f64 * bin_hz);
    let median_noise_bin_dbfs = Some(noise_bin_dbfs(median_power));
    let p95_noise_bin_dbfs = Some(noise_bin_dbfs(p95_power));
    let p99_noise_bin_dbfs = Some(noise_bin_dbfs(p99_power));
    let spur_margin_db = Some(10.0 * (peak_power / median_power).log10());
    let spur_margin_to_p95_db = Some(10.0 * (peak_power / p95_power).log10());
    let spur_margin_to_p99_db = Some(10.0 * (peak_power / p99_power).log10());

    Some(DsdInbandMetrics {
        residual_db,
        sinad_db,
        sinad_worst_db: sinad_db,
        sinad_p05_db: sinad_db,
        sinad_p95_db: sinad_db,
        sinad_best_db: sinad_db,
        sinad_spread_db: 0.0,
        window_count: 1,
        worst_window_start_s: Some(0.0),
        noise_rms_dbfs,
        noise_worst_rms_dbfs: noise_rms_dbfs,
        peak_spur_dbfs,
        peak_spur_hz,
        median_noise_bin_dbfs,
        p95_noise_bin_dbfs,
        p99_noise_bin_dbfs,
        spur_margin_db,
        spur_margin_to_p95_db,
        spur_margin_to_p99_db,
        noise_20_200_dbfs,
        noise_200_2k_dbfs,
        noise_2k_8k_dbfs,
        noise_8k_16k_dbfs,
        noise_16k_20k_dbfs,
    })
}

fn percentile_by_rank<T, F>(items: &[T], percentile: f64, value: F) -> Option<f64>
where
    F: Fn(&T) -> f64,
{
    if items.is_empty() {
        return None;
    }
    let index = ((items.len() - 1) as f64 * percentile.clamp(0.0, 1.0)).round() as usize;
    Some(value(&items[index]))
}

fn dsd_noise_bucket_dbfs(
    spectrum: &DsdPowerSpectrum,
    low_hz: f64,
    high_hz: f64,
    signal_bins: (usize, usize),
    signal_power: f64,
    signal_rms: f64,
) -> Option<f64> {
    let bin_hz = spectrum.sample_rate as f64 / spectrum.fft_len as f64;
    let low_bin = (low_hz / bin_hz).ceil().max(1.0) as usize;
    let high_bin = ((high_hz / bin_hz).floor() as usize).min(spectrum.powers.len() - 1);
    if high_bin <= low_bin {
        return None;
    }
    let (signal_lo, signal_hi) = signal_bins;
    let mut power = 0.0;
    for bin in low_bin..=high_bin {
        if bin >= signal_lo && bin <= signal_hi {
            continue;
        }
        let bin_power = spectrum.powers[bin];
        if bin_power.is_finite() {
            power += bin_power.max(f64::MIN_POSITIVE);
        }
    }
    if power <= f64::MIN_POSITIVE {
        return None;
    }
    Some(db(signal_rms * (power / signal_power).sqrt()))
}

fn dsd_idle_peak_dbfs(bits: &[f64], wire_rate: u32, target_fft_len: usize) -> Option<f64> {
    let spectrum = dsd_power_spectrum(bits, wire_rate, target_fft_len)?;
    let bin_hz = spectrum.sample_rate as f64 / spectrum.fft_len as f64;
    let low_bin = (20.0 / bin_hz).ceil().max(1.0) as usize;
    let high_bin = ((20_000.0 / bin_hz).floor() as usize).min(spectrum.powers.len() - 1);
    if high_bin <= low_bin {
        return None;
    }
    spectrum.powers[low_bin..=high_bin]
        .iter()
        .copied()
        .reduce(f64::max)
        .map(|power| {
            let amp = 2.0 * power.max(f64::MIN_POSITIVE).sqrt()
                / spectrum.window_sum.max(f64::MIN_POSITIVE);
            db(amp)
        })
}

fn dsd_power_spectrum(
    bits: &[f64],
    sample_rate: u32,
    target_fft_len: usize,
) -> Option<DsdPowerSpectrum> {
    let fft_len = dsd_fft_len(bits.len(), target_fft_len)?;
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(fft_len);
    let mut window_sum = 0.0;
    let window: Vec<f64> = (0..fft_len)
        .map(|idx| {
            let window = blackman_harris(idx, fft_len);
            window_sum += window;
            window
        })
        .collect();
    let starts = dsd_spectral_window_starts(bits.len(), fft_len, sample_rate, target_fft_len);
    let mut powers = vec![0.0; fft_len / 2 + 1];
    let mut time = vec![0.0; fft_len];
    let mut spectrum = fft.make_output_vec();
    for start in starts.iter().copied() {
        let input = &bits[start..start + fft_len];
        for (idx, sample) in input.iter().enumerate() {
            time[idx] = sample * window[idx];
        }
        fft.process(&mut time, &mut spectrum).ok()?;
        for (acc, bin) in powers.iter_mut().zip(spectrum.iter()) {
            *acc += bin.norm_sqr();
        }
    }
    let scale = starts.len().max(1) as f64;
    for power in &mut powers {
        *power /= scale;
    }
    Some(DsdPowerSpectrum {
        powers,
        fft_len,
        sample_rate,
        window_sum,
    })
}

fn dsd_power_spectrum_window(
    bits: &[f64],
    sample_rate: u32,
    fft_len: usize,
    start: usize,
) -> Option<DsdPowerSpectrum> {
    if fft_len < 1024 || start + fft_len > bits.len() {
        return None;
    }
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(fft_len);
    let mut time = vec![0.0; fft_len];
    let mut window_sum = 0.0;
    let input = &bits[start..start + fft_len];
    for (idx, sample) in input.iter().enumerate() {
        let window = blackman_harris(idx, fft_len);
        window_sum += window;
        time[idx] = sample * window;
    }
    let mut spectrum = fft.make_output_vec();
    fft.process(&mut time, &mut spectrum).ok()?;
    let powers = spectrum.iter().map(|bin| bin.norm_sqr()).collect();
    Some(DsdPowerSpectrum {
        powers,
        fft_len,
        sample_rate,
        window_sum,
    })
}

fn dsd_fft_len(bit_len: usize, target_fft_len: usize) -> Option<usize> {
    if bit_len < 1024 {
        return None;
    }
    let fft_len = bit_len
        .next_power_of_two()
        .min(target_fft_len)
        .min(bit_len.next_power_of_two() / 2);
    let fft_len = if bit_len.is_power_of_two() {
        bit_len.min(target_fft_len)
    } else {
        fft_len
    };
    (fft_len >= 1024 && bit_len >= fft_len).then_some(fft_len)
}

fn dsd_distribution_window_starts(
    bit_len: usize,
    fft_len: usize,
    sample_rate: u32,
    target_fft_len: usize,
) -> Vec<usize> {
    if bit_len <= fft_len {
        return vec![0];
    }
    let max_start = bit_len - fft_len;
    if target_fft_len < DSD_TONE_WINDOW_FFT_BITS_FULL {
        return vec![max_start];
    }
    let edge_margin =
        ((sample_rate as f64) * DSD_INBAND_PREFILTER_EDGE_DISCARD_SECONDS).round() as usize;
    let min_start = ((sample_rate as f64) * DSD_ANALYSIS_SETTLE_SECONDS_FULL).round() as usize;
    let min_start = min_start.max(edge_margin.min(max_start));
    let max_analysis_start = max_start.saturating_sub(edge_margin.min(max_start));
    if max_analysis_start <= min_start {
        return vec![max_analysis_start];
    }
    let hop = (fft_len / DSD_DISTRIBUTION_HOP_DIVISOR_FULL.max(1)).max(1);
    let mut starts = Vec::new();
    let mut start = min_start;
    while start <= max_analysis_start {
        starts.push(start);
        start = start.saturating_add(hop);
    }
    if starts.last().copied() != Some(max_analysis_start) {
        starts.push(max_analysis_start);
    }
    starts
}

fn dsd_spectral_window_starts(
    bit_len: usize,
    fft_len: usize,
    sample_rate: u32,
    target_fft_len: usize,
) -> Vec<usize> {
    if bit_len <= fft_len {
        return vec![0];
    }
    let max_start = bit_len - fft_len;
    if target_fft_len < DSD_ANALYSIS_FFT_BITS_FULL {
        return vec![max_start];
    }
    let min_start = ((sample_rate as f64) * DSD_ANALYSIS_SETTLE_SECONDS_FULL).round() as usize;
    if max_start <= min_start {
        return vec![max_start];
    }
    vec![min_start, (min_start + max_start) / 2, max_start]
}

fn blackman_harris(idx: usize, len: usize) -> f64 {
    if len <= 1 {
        return 1.0;
    }
    let t = 2.0 * PI * idx as f64 / (len - 1) as f64;
    0.35875 - 0.48829 * t.cos() + 0.14128 * (2.0 * t).cos() - 0.01168 * (3.0 * t).cos()
}

fn residual_spectrum_metrics(signal: &[f64], sample_rate: u32) -> Option<SpectrumMetrics> {
    let mut amplitudes: Vec<f64> = amplitude_spectrum(signal, sample_rate)?
        .into_iter()
        .filter(|(freq, _)| *freq >= 20.0 && *freq <= 20_000.0)
        .map(|(_, amp)| amp)
        .filter(|amp| amp.is_finite())
        .collect();
    if amplitudes.is_empty() {
        return None;
    }
    amplitudes.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = amplitudes[amplitudes.len() / 2].max(1e-18);
    let peak = amplitudes.iter().copied().fold(0.0f64, f64::max).max(1e-18);
    let peak_dbfs = db(peak);
    Some(SpectrumMetrics {
        peak_dbfs: Some(peak_dbfs),
        spur_margin_db: Some(peak_dbfs - db(median)),
    })
}

fn amplitude_spectrum(signal: &[f64], sample_rate: u32) -> Option<Vec<(f64, f64)>> {
    if signal.len() < 128 {
        return None;
    }
    let n = signal.len().next_power_of_two().min(65_536);
    let start = signal.len().saturating_sub(n) / 2;
    let input = &signal[start..start + n.min(signal.len() - start)];
    let n = input.len().next_power_of_two();
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(n);
    let mut time = vec![0.0; n];
    let window_sum: f64 = (0..input.len()).map(|idx| hann(idx, input.len())).sum();
    for (idx, sample) in input.iter().enumerate() {
        time[idx] = sample * hann(idx, input.len());
    }
    let mut spectrum = fft.make_output_vec();
    fft.process(&mut time, &mut spectrum).ok()?;
    Some(
        spectrum
            .iter()
            .enumerate()
            .map(|(idx, bin)| {
                let freq = idx as f64 * sample_rate as f64 / n as f64;
                let amp = 2.0 * bin.norm() / window_sum.max(1e-18);
                (freq, amp)
            })
            .collect(),
    )
}

fn unpack_native_msb(bytes: &[u8]) -> Vec<f64> {
    let mut bits = Vec::with_capacity(bytes.len() * 8);
    for byte in bytes {
        for shift in (0..8).rev() {
            bits.push(if (byte >> shift) & 1 == 1 { 1.0 } else { -1.0 });
        }
    }
    bits
}

fn decimate_dsd_bits(bits: &[f64], ratio: usize) -> Vec<f64> {
    if ratio == 0 {
        return Vec::new();
    }
    bits.chunks_exact(ratio)
        .map(|chunk| chunk.iter().sum::<f64>() / ratio as f64)
        .collect()
}

fn aligned_residual_db(reference: &[f64], measured: &[f64]) -> Option<f64> {
    let (delay, gain) = best_delay_and_gain(reference, measured, 512)?;
    let mut residual = Vec::new();
    let mut aligned_ref = Vec::new();
    for (ref_idx, meas_idx) in aligned_indices(reference.len(), measured.len(), delay) {
        aligned_ref.push(reference[ref_idx] * gain);
        residual.push(measured[meas_idx] - reference[ref_idx] * gain);
    }
    if residual.len() < 1024 {
        return None;
    }
    Some(db(
        (rms(&residual) / rms(&aligned_ref).max(1e-18)).max(1e-18)
    ))
}

fn dsd_prefilter_and_decimate_to_pcm(
    bits: &[f64],
    wire_rate: u32,
    target_rate: u32,
) -> Option<Vec<f64>> {
    if target_rate == 0 || !wire_rate.is_multiple_of(target_rate) {
        return None;
    }
    let filtered = dsd_inband_prefilter(bits, wire_rate)?;
    Some(decimate_dsd_bits(
        &filtered,
        (wire_rate / target_rate) as usize,
    ))
}

fn analyze_roundtrip_channel(
    reference: &[f64],
    measured: &[f64],
    sample_rate: u32,
) -> Option<RoundtripChannelAnalysis> {
    let (delay_samples, gain) = best_delay_and_gain(reference, measured, 8192)?;
    let mut aligned_reference = Vec::new();
    let mut aligned_measured = Vec::new();
    let mut residual = Vec::new();
    for (ref_idx, meas_idx) in aligned_indices(reference.len(), measured.len(), delay_samples) {
        let reference = reference[ref_idx] * gain;
        let measured = measured[meas_idx];
        aligned_reference.push(reference);
        aligned_measured.push(measured);
        residual.push(measured - reference);
    }
    if residual.len() < 1024 {
        return None;
    }
    let dot = aligned_reference
        .iter()
        .zip(aligned_measured.iter())
        .map(|(reference, measured)| reference * measured)
        .sum::<f64>();
    let ref_energy = aligned_reference
        .iter()
        .map(|sample| sample * sample)
        .sum::<f64>();
    let measured_energy = aligned_measured
        .iter()
        .map(|sample| sample * sample)
        .sum::<f64>();
    let correlation = if ref_energy <= 1e-18 || measured_energy <= 1e-18 {
        0.0
    } else {
        dot / (ref_energy * measured_energy).sqrt()
    };
    let residual_rms = rms(&residual);
    let residual_peak = sample_abs_peak(&residual).unwrap_or(0.0);
    let residual_spur_margin_db = residual_spectrum_metrics(&residual, sample_rate)
        .and_then(|metrics| metrics.spur_margin_db);
    Some(RoundtripChannelAnalysis {
        delay_samples,
        gain,
        correlation,
        residual_relative_db: db((residual_rms / rms(&aligned_reference).max(1e-18)).max(1e-18)),
        residual_rms_dbfs: db(residual_rms.max(1e-18)),
        residual_peak_dbfs: db(residual_peak.max(1e-18)),
        residual_spur_margin_db,
        aligned_reference,
        aligned_measured,
        residual,
    })
}

fn append_roundtrip_artifacts(
    artifacts: &mut DsdRoundtripArtifacts,
    measurement: &DsdRoundtripMeasurement,
    channel: &str,
    sample_rate: u32,
    analysis: &RoundtripChannelAnalysis,
) {
    append_roundtrip_waveform_points(artifacts, measurement, channel, sample_rate, analysis);
    append_roundtrip_spectrum_points(artifacts, measurement, channel, sample_rate, analysis);
    append_roundtrip_spectrogram_points(artifacts, measurement, channel, sample_rate, analysis);
}

fn append_roundtrip_waveform_points(
    artifacts: &mut DsdRoundtripArtifacts,
    measurement: &DsdRoundtripMeasurement,
    channel: &str,
    sample_rate: u32,
    analysis: &RoundtripChannelAnalysis,
) {
    let target_points = 2048usize;
    let step = analysis
        .aligned_reference
        .len()
        .div_ceil(target_points)
        .max(1);
    for idx in (0..analysis.aligned_reference.len()).step_by(step) {
        artifacts.waveform.push(RoundtripWaveformPoint {
            candidate_index: measurement.candidate_index,
            fixture: measurement.fixture.clone(),
            filter: measurement.filter.clone(),
            modulator: measurement.modulator.clone(),
            dsd_rate: measurement.dsd_rate.clone(),
            channel: channel.to_string(),
            sample_index: idx,
            time_s: idx as f64 / sample_rate as f64,
            reference: analysis.aligned_reference[idx],
            measured: analysis.aligned_measured[idx],
            residual: analysis.residual[idx],
        });
    }
}

fn append_roundtrip_spectrum_points(
    artifacts: &mut DsdRoundtripArtifacts,
    measurement: &DsdRoundtripMeasurement,
    channel: &str,
    sample_rate: u32,
    analysis: &RoundtripChannelAnalysis,
) {
    if let (Some(reference), Some(measured)) = (
        amplitude_spectrum(&analysis.aligned_reference, sample_rate),
        amplitude_spectrum(&analysis.aligned_measured, sample_rate),
    ) {
        for ((freq, reference_amp), (_, measured_amp)) in reference.into_iter().zip(measured) {
            if freq > 22_050.0 {
                continue;
            }
            artifacts.spectrum.push(RoundtripSpectrumPoint {
                candidate_index: measurement.candidate_index,
                fixture: measurement.fixture.clone(),
                filter: measurement.filter.clone(),
                modulator: measurement.modulator.clone(),
                dsd_rate: measurement.dsd_rate.clone(),
                channel: channel.to_string(),
                freq_hz: freq,
                reference_dbfs: Some(db(reference_amp.max(1e-18))),
                measured_dbfs: Some(db(measured_amp.max(1e-18))),
                residual_dbfs: None,
            });
        }
    }
    if let Some(residual) = amplitude_spectrum(&analysis.residual, sample_rate) {
        for (freq, residual_amp) in residual {
            if freq > 22_050.0 {
                continue;
            }
            artifacts.residual_spectrum.push(RoundtripSpectrumPoint {
                candidate_index: measurement.candidate_index,
                fixture: measurement.fixture.clone(),
                filter: measurement.filter.clone(),
                modulator: measurement.modulator.clone(),
                dsd_rate: measurement.dsd_rate.clone(),
                channel: channel.to_string(),
                freq_hz: freq,
                reference_dbfs: None,
                measured_dbfs: None,
                residual_dbfs: Some(db(residual_amp.max(1e-18))),
            });
        }
    }
}

fn append_roundtrip_spectrogram_points(
    artifacts: &mut DsdRoundtripArtifacts,
    measurement: &DsdRoundtripMeasurement,
    channel: &str,
    sample_rate: u32,
    analysis: &RoundtripChannelAnalysis,
) {
    let window = 4096usize;
    if analysis.residual.len() < window {
        return;
    }
    let hop = window / 2;
    for start in (0..=analysis.residual.len() - window).step_by(hop) {
        let Some(spectrum) =
            amplitude_spectrum(&analysis.residual[start..start + window], sample_rate)
        else {
            continue;
        };
        for (bin, (freq, amp)) in spectrum.into_iter().enumerate() {
            if freq > 20_000.0 {
                break;
            }
            if bin % 4 != 0 {
                continue;
            }
            artifacts.spectrogram.push(RoundtripSpectrogramPoint {
                candidate_index: measurement.candidate_index,
                fixture: measurement.fixture.clone(),
                filter: measurement.filter.clone(),
                modulator: measurement.modulator.clone(),
                dsd_rate: measurement.dsd_rate.clone(),
                channel: channel.to_string(),
                start_s: start as f64 / sample_rate as f64,
                freq_hz: freq,
                residual_dbfs: db(amp.max(1e-18)),
            });
        }
    }
}

fn decoded_sine_residual_db(measured: &[f64], sample_rate: u32, freq: f64) -> Option<f64> {
    if measured.len() < 1024 {
        return None;
    }
    let amp = tone_amplitude(measured, sample_rate, freq);
    let mut fitted = Vec::with_capacity(measured.len());
    let mut residual = Vec::with_capacity(measured.len());
    for (idx, sample) in measured.iter().enumerate() {
        let s = amp * (2.0 * PI * freq * idx as f64 / sample_rate as f64).sin();
        fitted.push(s);
        residual.push(sample - s);
    }
    Some(db((rms(&residual) / rms(&fitted).max(1e-18)).max(1e-18)))
}

fn best_delay_and_gain(
    reference: &[f64],
    measured: &[f64],
    max_delay: isize,
) -> Option<(isize, f64)> {
    let mut best = None;
    for delay in -max_delay..=max_delay {
        let mut dot = 0.0;
        let mut ref_energy = 0.0;
        let mut meas_energy = 0.0;
        let mut count = 0usize;
        for (ref_idx, meas_idx) in aligned_indices(reference.len(), measured.len(), delay) {
            let r = reference[ref_idx];
            let m = measured[meas_idx];
            dot += r * m;
            ref_energy += r * r;
            meas_energy += m * m;
            count += 1;
        }
        if count < 1024 || ref_energy <= 1e-18 || meas_energy <= 1e-18 {
            continue;
        }
        let corr = dot.abs() / (ref_energy * meas_energy).sqrt();
        let gain = dot / ref_energy;
        if best.is_none_or(|(best_corr, _, _)| corr > best_corr) {
            best = Some((corr, delay, gain));
        }
    }
    best.map(|(_, delay, gain)| (delay, gain))
}

fn aligned_indices(
    ref_len: usize,
    measured_len: usize,
    delay: isize,
) -> impl Iterator<Item = (usize, usize)> {
    let ref_start = if delay < 0 { (-delay) as usize } else { 0 };
    let meas_start = if delay > 0 { delay as usize } else { 0 };
    let len = ref_len
        .saturating_sub(ref_start)
        .min(measured_len.saturating_sub(meas_start));
    (0..len).map(move |idx| (ref_start + idx, meas_start + idx))
}

fn rms(signal: &[f64]) -> f64 {
    (signal.iter().map(|x| x * x).sum::<f64>() / signal.len().max(1) as f64).sqrt()
}

fn hann(idx: usize, len: usize) -> f64 {
    if len <= 1 {
        1.0
    } else {
        0.5 - 0.5 * (2.0 * PI * idx as f64 / (len - 1) as f64).cos()
    }
}

fn db(value: f64) -> f64 {
    20.0 * value.max(1e-18).log10()
}

fn dsd_rate_name(rate: DsdRate) -> &'static str {
    match rate {
        DsdRate::Dsd64 => "DSD64",
        DsdRate::Dsd128 => "DSD128",
        DsdRate::Dsd256 => "DSD256",
    }
}

fn dsd_wire_rate_from_name(dsd_rate: &str, source_rate: u32) -> Option<u32> {
    match dsd_rate {
        "DSD64" => DsdRate::Dsd64.wire_rate_for_source(source_rate),
        "DSD128" => DsdRate::Dsd128.wire_rate_for_source(source_rate),
        "DSD256" => DsdRate::Dsd256.wire_rate_for_source(source_rate),
        _ => None,
    }
}

fn bit_density(bits: &[f64]) -> Option<f64> {
    (!bits.is_empty())
        .then(|| bits.iter().filter(|&&bit| bit > 0.0).count() as f64 / bits.len() as f64)
}

fn sample_low(samples: &[f64]) -> Option<f64> {
    samples.iter().copied().reduce(f64::min)
}

fn sample_peak(samples: &[f64]) -> Option<f64> {
    samples.iter().copied().reduce(f64::max)
}

fn sample_abs_peak(samples: &[f64]) -> Option<f64> {
    samples.iter().copied().map(f64::abs).reduce(f64::max)
}

fn transition_rate(bits: &[f64]) -> Option<f64> {
    (bits.len() > 1).then(|| {
        bits.windows(2)
            .filter(|pair| (pair[0] > 0.0) != (pair[1] > 0.0))
            .count() as f64
            / (bits.len() - 1) as f64
    })
}

fn rolling_bit_density_max_deviation(bits: &[f64], window: usize) -> Option<f64> {
    if bits.is_empty() {
        return None;
    }
    let window = window.clamp(1, bits.len());
    let mut ones = bits[..window].iter().filter(|&&bit| bit > 0.0).count();
    let mut max_deviation = (ones as f64 / window as f64 - 0.5).abs();
    for idx in window..bits.len() {
        if bits[idx - window] > 0.0 {
            ones -= 1;
        }
        if bits[idx] > 0.0 {
            ones += 1;
        }
        max_deviation = max_deviation.max((ones as f64 / window as f64 - 0.5).abs());
    }
    Some(max_deviation)
}

fn max_option(current: Option<f64>, candidate: f64) -> Option<f64> {
    Some(current.map_or(candidate, |value| value.max(candidate)))
}

fn max_opt(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn max_abs_signed_opt(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(if left.abs() >= right.abs() {
            left
        } else {
            right
        }),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn min_opt(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn record_max_with_source(
    current: &mut Option<f64>,
    source: &mut Option<String>,
    candidate: f64,
    candidate_source: &str,
) {
    if current.is_none_or(|value| candidate > value) {
        *current = Some(candidate);
        *source = Some(candidate_source.to_string());
    }
}

fn push_source_note(notes: &mut Vec<String>, key: &str, source: Option<&str>) {
    if let Some(source) = source {
        notes.push(format!("{key}={source}"));
    }
}

fn gate_class_for_filter(name: &str) -> Option<GateClass> {
    match name {
        "Minimum16k" | "Split128k" => Some(GateClass::Minimum16k),
        "SincExtreme32k" => Some(GateClass::Extreme),
        _ => None,
    }
}

fn check_abs(failures: &mut Vec<String>, value: Option<f64>, limit: f64, label: String) {
    match value {
        Some(value) if value.abs() <= limit => {}
        Some(value) => failures.push(format!("{label} {value:.2} dB exceeds ±{limit:.2} dB")),
        None => failures.push(format!("{label} was unavailable")),
    }
}

fn check_min(failures: &mut Vec<String>, value: Option<f64>, limit: f64, label: String) {
    match value {
        Some(value) if value >= limit => {}
        Some(value) => failures.push(format!("{label} {value:.2} is below {limit:.2}")),
        None => failures.push(format!("{label} was unavailable")),
    }
}

fn check_max(failures: &mut Vec<String>, value: Option<f64>, limit: f64, label: String) {
    match value {
        Some(value) if value <= limit => {}
        Some(value) => failures.push(format!("{label} {value:.2} is above {limit:.2}")),
        None => failures.push(format!("{label} was unavailable")),
    }
}

fn check_pair_delta(
    failures: &mut Vec<String>,
    value: Option<f64>,
    reference: Option<f64>,
    limit: f64,
    label: String,
) {
    match (value, reference) {
        (Some(value), Some(reference)) if (value - reference).abs() <= limit => {}
        (Some(value), Some(reference)) => failures.push(format!(
            "{label} delta {:+.2} exceeds ±{limit:.2} (value {value:.2}, EC-2 {reference:.2})",
            value - reference
        )),
        _ => failures.push(format!("{label} comparison was unavailable")),
    }
}

fn check_max_usize(failures: &mut Vec<String>, value: Option<usize>, limit: usize, label: String) {
    match value {
        Some(value) if value <= limit => {}
        Some(value) => failures.push(format!("{label} {value} is above {limit}")),
        None => failures.push(format!("{label} was unavailable")),
    }
}

fn check_dsd_spur_margin_or_absolute_peak(
    failures: &mut Vec<String>,
    dsd: &DsdMeasurement,
    margin_limit_db: f64,
    absolute_peak_limit_dbfs: f64,
    label: &str,
) {
    let margin_ok = dsd
        .inband_noise_spur_margin_db
        .is_some_and(|value| value.is_finite() && value >= margin_limit_db);
    let absolute_ok = dsd
        .inband_noise_peak_dbfs
        .is_some_and(|value| value.is_finite() && value <= absolute_peak_limit_dbfs);
    if margin_ok || absolute_ok {
        return;
    }
    failures.push(format!(
        "{label} margin {} is below {:.2} and absolute peak {} is above {:.2} dBFS",
        fmt_opt(dsd.inband_noise_spur_margin_db),
        margin_limit_db,
        fmt_opt(dsd.inband_noise_peak_dbfs),
        absolute_peak_limit_dbfs
    ));
}

fn dsd_idle_worst_tone_is_dc_fixture_only(dsd: &DsdMeasurement) -> bool {
    if dsd.idle_artifacts.is_empty() {
        return false;
    }
    let has_dc_worst = dsd.idle_artifacts.iter().any(|artifact| {
        artifact.is_idle_worst_tone
            && artifact.fixture_label.starts_with("dc_")
            && artifact.fixture_label != "dc_0"
    });
    if !has_dc_worst {
        return false;
    }
    dsd.idle_artifacts
        .iter()
        .filter(|artifact| {
            !artifact.fixture_label.starts_with("dc_") || artifact.fixture_label == "dc_0"
        })
        .all(|artifact| artifact.idle_peak_dbfs.is_none_or(|value| value <= -120.0))
}

fn check_dsd_noise_floor(failures: &mut Vec<String>, dsd: &DsdMeasurement) {
    let gate = DsdRateGate::for_measurement(dsd);
    check_max(
        failures,
        dsd.inband_noise_worst_rms_dbfs,
        gate.worst_noise_max_dbfs,
        format!(
            "{} {} {} {} worst-window in-band noise RMS",
            dsd.filter, dsd.modulator, dsd.source_rate, dsd.dsd_rate
        ),
    );
}

fn check_ec1_artifact_regression(failures: &mut Vec<String>, dsd: &DsdMeasurement) {
    if dsd.modulator != "EcDepth1"
        || dsd.filter != "Split128k"
        || dsd.source_rate != 44_100
        || dsd.dsd_rate != "DSD256"
        || dsd.high_freq_worst_residual_db.is_none()
    {
        return;
    }

    check_max(
        failures,
        dsd.inband_snr_spread_db,
        20.0,
        "Split128k EcDepth1 44100 DSD256 in-band SINAD spread".to_string(),
    );
    check_min(
        failures,
        dsd.inband_noise_spur_margin_db,
        15.0,
        "Split128k EcDepth1 44100 DSD256 in-band spur margin".to_string(),
    );
    check_max(
        failures,
        dsd.idle_worst_tone_dbfs,
        -70.0,
        "Split128k EcDepth1 44100 DSD256 idle worst tone".to_string(),
    );
    check_max(
        failures,
        dsd.high_freq_worst_residual_db,
        -10.0,
        "Split128k EcDepth1 44100 DSD256 high-frequency worst residual".to_string(),
    );
    check_max(
        failures,
        dsd.high_freq_worst_spur_dbfs,
        -50.0,
        "Split128k EcDepth1 44100 DSD256 high-frequency worst spur".to_string(),
    );
    check_max(
        failures,
        dsd.decoded_abs_peak,
        0.25,
        "Split128k EcDepth1 44100 DSD256 decoded absolute peak".to_string(),
    );
    check_max(
        failures,
        dsd.bit_density_max_deviation,
        0.00018,
        "Split128k EcDepth1 44100 DSD256 bit-density max deviation".to_string(),
    );
    check_max_usize(
        failures,
        dsd.transient_click_candidates,
        0,
        "Split128k EcDepth1 44100 DSD256 transient click candidates".to_string(),
    );
    check_max_usize(
        failures,
        dsd.program_click_candidates,
        0,
        "Split128k EcDepth1 44100 DSD256 program click candidates".to_string(),
    );
}

fn check_dsd128_ec2_artifact_regression(failures: &mut Vec<String>, dsd: &DsdMeasurement) {
    if dsd.modulator != "EcDepth2"
        || dsd.filter != "Split128k"
        || dsd.source_rate != 44_100
        || dsd.dsd_rate != "DSD128"
        || dsd.high_freq_worst_residual_db.is_none()
    {
        return;
    }

    check_max(
        failures,
        dsd.inband_snr_spread_db,
        28.0,
        "Split128k EcDepth2 44100 DSD128 in-band SINAD spread".to_string(),
    );
    check_dsd_spur_margin_or_absolute_peak(
        failures,
        dsd,
        dsd128_ec2_spur_margin_limit_db(dsd),
        -180.0,
        "Split128k EcDepth2 44100 DSD128 in-band spur",
    );
    check_max(
        failures,
        dsd.inband_noise_worst_rms_dbfs,
        DsdRateGate::for_measurement(dsd).worst_noise_max_dbfs,
        "Split128k EcDepth2 44100 DSD128 worst-window in-band noise RMS".to_string(),
    );
    check_max_usize(
        failures,
        dsd.transient_click_candidates,
        0,
        "Split128k EcDepth2 44100 DSD128 transient click candidates".to_string(),
    );
    check_max_usize(
        failures,
        dsd.program_click_candidates,
        0,
        "Split128k EcDepth2 44100 DSD128 program click candidates".to_string(),
    );
    check_max(
        failures,
        dsd.decoded_abs_peak,
        0.25,
        "Split128k EcDepth2 44100 DSD128 decoded absolute peak".to_string(),
    );
}

fn check_dsd128_ec4a_pressure_only_reference(report: &SuiteReport, failures: &mut Vec<String>) {
    for ec4a in report.dsd.iter().filter(|m| {
        m.modulator == "EcDepth4Adaptive"
            && m.filter == "Split128k"
            && m.source_rate == 44_100
            && m.dsd_rate == "DSD128"
            && dsd_note_bool(m, "dsd128_ec4a_allow_predictive_triggers") == Some(false)
    }) {
        let headroom_db = dsd128_input_gain_db(ec4a);
        let Some(ec2) = report.dsd.iter().find(|m| {
            m.modulator == "EcDepth2"
                && m.filter == ec4a.filter
                && m.source_rate == ec4a.source_rate
                && m.dsd_rate == ec4a.dsd_rate
                && (dsd128_input_gain_db(m) - headroom_db).abs() <= 0.001
                && dsd_note_value(m, "dsd128_ec_dither_scale_multiplier")
                    == dsd_note_value(ec4a, "dsd128_ec_dither_scale_multiplier")
                && dsd_note_string(m, "dsd128_ec_dither_shape")
                    == dsd_note_string(ec4a, "dsd128_ec_dither_shape")
                && dsd_note_string(m, "dsd128_ec_dither_prng")
                    == dsd_note_string(ec4a, "dsd128_ec_dither_prng")
                && dsd_note_value(m, "dsd128_ec_dither_leak_alpha")
                    == dsd_note_value(ec4a, "dsd128_ec_dither_leak_alpha")
                && dsd_note_value(m, "dsd128_ec_dither_lf_floor_gamma")
                    == dsd_note_value(ec4a, "dsd128_ec_dither_lf_floor_gamma")
                && dsd_note_value(m, "dsd128_ec_common_side_dither_beta")
                    == dsd_note_value(ec4a, "dsd128_ec_common_side_dither_beta")
                && dsd_note_string(m, "dsd128_ec_common_side_common_seed")
                    == dsd_note_string(ec4a, "dsd128_ec_common_side_common_seed")
                && dsd_note_string(m, "dsd128_ec_common_side_side_seed")
                    == dsd_note_string(ec4a, "dsd128_ec_common_side_side_seed")
                && dsd_note_string(m, "dsd128_ec_future_scorer")
                    == dsd_note_string(ec4a, "dsd128_ec_future_scorer")
                && dsd_note_string(m, "dsd_seed_left") == dsd_note_string(ec4a, "dsd_seed_left")
                && dsd_note_string(m, "dsd_seed_right") == dsd_note_string(ec4a, "dsd_seed_right")
        }) else {
            failures.push(format!(
                "Split128k EcDepth4Adaptive pressure-only 44100 DSD128 {headroom_db:.1} dB has no EC-2 reference row"
            ));
            continue;
        };

        check_pair_delta(
            failures,
            ec4a.inband_noise_spur_margin_db,
            ec2.inband_noise_spur_margin_db,
            0.10,
            format!(
                "Split128k EcDepth4Adaptive pressure-only 44100 DSD128 {headroom_db:.1} dB spur margin vs EC-2"
            ),
        );
        check_pair_delta(
            failures,
            ec4a.inband_noise_worst_rms_dbfs,
            ec2.inband_noise_worst_rms_dbfs,
            0.10,
            format!(
                "Split128k EcDepth4Adaptive pressure-only 44100 DSD128 {headroom_db:.1} dB worst-window noise vs EC-2"
            ),
        );
        check_pair_delta(
            failures,
            ec4a.high_freq_worst_residual_db,
            ec2.high_freq_worst_residual_db,
            0.10,
            format!(
                "Split128k EcDepth4Adaptive pressure-only 44100 DSD128 {headroom_db:.1} dB HF residual vs EC-2"
            ),
        );
    }
}

fn check_dsd128_ec4a_target_bands(failures: &mut Vec<String>, dsd: &DsdMeasurement) {
    if dsd.modulator != "EcDepth4Adaptive"
        || dsd.filter != "Split128k"
        || dsd.source_rate != 44_100
        || dsd.dsd_rate != "DSD128"
        || dsd_note_bool(dsd, "dsd128_ec4a_allow_predictive_triggers") != Some(false)
    {
        return;
    }

    check_min(
        failures,
        dsd.inband_snr_left_worst_db,
        150.0,
        "Split128k EcDepth4Adaptive pressure-only 44100 DSD128 left worst SINAD".to_string(),
    );
    check_min(
        failures,
        dsd.inband_snr_right_worst_db,
        150.0,
        "Split128k EcDepth4Adaptive pressure-only 44100 DSD128 right worst SINAD".to_string(),
    );
    check_max(
        failures,
        dsd.inband_snr_spread_db,
        5.0,
        "Split128k EcDepth4Adaptive pressure-only 44100 DSD128 worst-channel SINAD spread"
            .to_string(),
    );
    check_max(
        failures,
        dsd.stereo_snr_worst_mismatch_db,
        3.0,
        "Split128k EcDepth4Adaptive pressure-only 44100 DSD128 stereo SINAD mismatch".to_string(),
    );
    check_dsd_spur_margin_or_absolute_peak(
        failures,
        dsd,
        15.0,
        -180.0,
        "Split128k EcDepth4Adaptive pressure-only 44100 DSD128 worst-channel spur",
    );
    check_max(
        failures,
        dsd.bit_density_max_deviation,
        0.0002,
        "Split128k EcDepth4Adaptive pressure-only 44100 DSD128 worst-channel density deviation"
            .to_string(),
    );
    check_max_usize(
        failures,
        dsd.transient_click_candidates,
        0,
        "Split128k EcDepth4Adaptive pressure-only 44100 DSD128 transient click candidates"
            .to_string(),
    );
    check_max_usize(
        failures,
        dsd.program_click_candidates,
        0,
        "Split128k EcDepth4Adaptive pressure-only 44100 DSD128 program click candidates"
            .to_string(),
    );
}

fn dsd128_ec2_spur_margin_limit_db(dsd: &DsdMeasurement) -> f64 {
    let is_production_dither = dsd_note_string(dsd, "dsd128_ec_dither_shape")
        == Some("HighPassTpdf")
        && dsd_note_value(dsd, "dsd128_ec_dither_scale_multiplier")
            .is_some_and(|scale| (scale - DSD128_EC4A_DITHER_SCALE_MULTIPLIER).abs() <= 0.0005);
    if is_production_dither {
        return 16.0;
    }
    if dsd128_input_gain_db(dsd) <= -3.5 {
        18.0
    } else {
        19.0
    }
}

fn dsd128_headroom_attenuation_db(dsd: &DsdMeasurement) -> f64 {
    (-dsd128_input_gain_db(dsd)).clamp(0.0, 12.0)
}

fn dsd128_input_gain_db(dsd: &DsdMeasurement) -> f64 {
    dsd_note_value(dsd, "dsd128_input_gain_db").unwrap_or(0.0)
}

fn dsd_rate_note_prefix(dsd: &DsdMeasurement) -> String {
    dsd.dsd_rate.to_ascii_lowercase()
}

fn dsd_input_gain_db(dsd: &DsdMeasurement) -> f64 {
    dsd_note_value(dsd, &format!("{}_input_gain_db", dsd_rate_note_prefix(dsd))).unwrap_or(0.0)
}

fn dsd_rate_note_value(dsd: &DsdMeasurement, suffix: &str) -> Option<f64> {
    dsd_note_value(dsd, &format!("{}_{}", dsd_rate_note_prefix(dsd), suffix))
}

fn dsd_rate_note_string<'a>(dsd: &'a DsdMeasurement, suffix: &str) -> Option<&'a str> {
    dsd.notes.iter().find_map(|note| {
        let prefix = dsd_rate_note_prefix(dsd);
        note.strip_prefix(&prefix)
            .and_then(|rest| rest.strip_prefix('_'))
            .and_then(|rest| rest.strip_prefix(suffix))
            .and_then(|rest| rest.strip_prefix('='))
    })
}

fn dsd_note_bool(dsd: &DsdMeasurement, key: &str) -> Option<bool> {
    dsd.notes.iter().find_map(|note| {
        note.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
            .and_then(|value| value.parse::<bool>().ok())
    })
}

fn dsd_note_value(dsd: &DsdMeasurement, key: &str) -> Option<f64> {
    dsd.notes.iter().find_map(|note| {
        note.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
            .and_then(|value| value.parse::<f64>().ok())
    })
}

fn dsd_note_string<'a>(dsd: &'a DsdMeasurement, key: &str) -> Option<&'a str> {
    dsd.notes.iter().find_map(|note| {
        note.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
    })
}

fn git_commit() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn working_tree_dirty() -> Option<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()?;
    output.status.success().then_some(!output.stdout.is_empty())
}

#[derive(Debug, Clone, Serialize)]
struct ArtifactProvenance {
    measurement_version: &'static str,
    scoring_version: &'static str,
    gate_table_version: &'static str,
    fixture_set_version: &'static str,
    candidate_schema_version: &'static str,
    modulator_commit: Option<String>,
    build_host: String,
    codegen_flags: String,
}

#[derive(Serialize)]
struct VersionedJsonArtifact<'a, T>
where
    T: Serialize,
{
    #[serde(flatten)]
    provenance: ArtifactProvenance,
    #[serde(flatten)]
    artifact: &'a T,
}

fn artifact_provenance() -> ArtifactProvenance {
    ArtifactProvenance {
        measurement_version: MEASUREMENT_VERSION,
        scoring_version: SCORING_VERSION,
        gate_table_version: GATE_TABLE_VERSION,
        fixture_set_version: FIXTURE_SET_VERSION,
        candidate_schema_version: CANDIDATE_SCHEMA_VERSION,
        modulator_commit: git_commit(),
        build_host: build_host(),
        codegen_flags: codegen_flags(),
    }
}

fn versioned_json<T: Serialize>(artifact: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&VersionedJsonArtifact {
        provenance: artifact_provenance(),
        artifact,
    })
}

fn build_host() -> String {
    static HOST: OnceLock<String> = OnceLock::new();
    HOST.get_or_init(|| {
        Command::new("hostname")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|host| !host.is_empty())
            .unwrap_or_else(|| format!("{}-{}", env::consts::OS, env::consts::ARCH))
    })
    .clone()
}

fn codegen_flags() -> String {
    env::var("RUSTFLAGS").unwrap_or_else(|_| "unknown".to_string())
}

fn provenance_csv_header() -> &'static str {
    "measurement_version,scoring_version,gate_table_version,fixture_set_version,candidate_schema_version,modulator_commit,build_host,codegen_flags"
}

fn provenance_csv_values() -> Vec<String> {
    let provenance = artifact_provenance();
    vec![
        provenance.measurement_version.to_string(),
        provenance.scoring_version.to_string(),
        provenance.gate_table_version.to_string(),
        provenance.fixture_set_version.to_string(),
        provenance.candidate_schema_version.to_string(),
        provenance.modulator_commit.unwrap_or_default(),
        provenance.build_host,
        provenance.codegen_flags,
    ]
}

fn fmt_opt(value: Option<f64>) -> String {
    value
        .map(|v| format!("{v:.2}"))
        .unwrap_or_else(|| "n/a".to_string())
}

#[derive(Debug, Clone)]
struct DsdQualityRanking {
    dsd_index: usize,
    rank_group: String,
    rank: usize,
    headline_snr_rank: Option<usize>,
    status: DsdRankingStatus,
    score: f64,
    sections: DsdScoreSections,
    hard_failures: Vec<String>,
    missing_constraints: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct DsdScoreSections {
    hard_health: f64,
    tonal_risk: f64,
    broad_residual: f64,
    baseband_agreement: f64,
    ultrasonic_profile: f64,
    runtime: f64,
    robustness: f64,
}

impl DsdScoreSections {
    fn metric_rows(self) -> [(&'static str, f64); 7] {
        [
            ("score_section_hard_health", self.hard_health),
            ("score_section_tonal_risk", self.tonal_risk),
            ("score_section_broad_residual", self.broad_residual),
            ("score_section_baseband_agreement", self.baseband_agreement),
            ("score_section_ultrasonic_profile", self.ultrasonic_profile),
            ("score_section_runtime", self.runtime),
            ("score_section_robustness", self.robustness),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DsdRankingStatus {
    Pass,
    Partial,
    Reject,
}

impl DsdRankingStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Partial => "partial",
            Self::Reject => "reject",
        }
    }

    fn code(self) -> f64 {
        match self {
            Self::Pass => 0.0,
            Self::Partial => 1.0,
            Self::Reject => 2.0,
        }
    }
}

fn dsd_quality_rankings(report: &SuiteReport) -> Vec<DsdQualityRanking> {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (idx, dsd) in report.dsd.iter().enumerate() {
        groups.entry(dsd_rank_group(dsd)).or_default().push(idx);
    }

    let mut rankings = Vec::new();
    for (rank_group, indices) in groups {
        let mut headline = indices.clone();
        headline.sort_by(|left, right| {
            compare_option_desc(
                report.dsd[*left].inband_snr_db,
                report.dsd[*right].inband_snr_db,
            )
            .then_with(|| left.cmp(right))
        });
        let headline_ranks: BTreeMap<usize, usize> = headline
            .into_iter()
            .enumerate()
            .map(|(rank, idx)| (idx, rank + 1))
            .collect();

        let mut scored: Vec<_> = indices
            .into_iter()
            .map(|idx| {
                let score = score_dsd_candidate(&report.dsd[idx]);
                (idx, score)
            })
            .collect();
        scored.sort_by(|(left_idx, left), (right_idx, right)| {
            left.status
                .cmp(&right.status)
                .then_with(|| right.score.total_cmp(&left.score))
                .then_with(|| {
                    compare_option_desc(
                        report.dsd[*left_idx].inband_snr_worst_db,
                        report.dsd[*right_idx].inband_snr_worst_db,
                    )
                })
                .then_with(|| {
                    compare_option_desc(
                        report.dsd[*left_idx].inband_noise_spur_margin_db,
                        report.dsd[*right_idx].inband_noise_spur_margin_db,
                    )
                })
                .then_with(|| {
                    report.dsd[*left_idx]
                        .modulator
                        .cmp(&report.dsd[*right_idx].modulator)
                })
                .then_with(|| left_idx.cmp(right_idx))
        });

        for (rank, (idx, score)) in scored.into_iter().enumerate() {
            rankings.push(DsdQualityRanking {
                dsd_index: idx,
                rank_group: rank_group.clone(),
                rank: rank + 1,
                headline_snr_rank: headline_ranks.get(&idx).copied(),
                status: score.status,
                score: score.score,
                sections: score.sections,
                hard_failures: score.hard_failures,
                missing_constraints: score.missing_constraints,
            });
        }
    }

    rankings.sort_by(|left, right| {
        left.rank_group
            .cmp(&right.rank_group)
            .then_with(|| left.rank.cmp(&right.rank))
    });
    rankings
}

fn dsd_decision_summary(report: &SuiteReport) -> DsdDecisionSummary {
    let candidates = report
        .dsd
        .iter()
        .map(|dsd| {
            let score = score_dsd_candidate(dsd);
            DsdDecisionCandidate {
                filter: dsd.filter.clone(),
                modulator: dsd.modulator.clone(),
                path_variant: dsd.path_variant.clone(),
                source_rate: dsd.source_rate,
                origin_source_rate: dsd.origin_source_rate,
                renderer_source_rate: dsd.renderer_source_rate,
                intermediate_rate: dsd.intermediate_rate,
                intermediate_bits: dsd.intermediate_bits,
                intermediate_filter: dsd.intermediate_filter.clone(),
                path_prepare_ms: dsd.path_prepare_ms,
                render_ms: dsd.render_ms,
                dsd_rate: dsd.dsd_rate.clone(),
                status: score.status.as_str().to_string(),
                score: score.score,
                score_sections: score.sections,
                hard_failures: score.hard_failures,
                missing_constraints: score.missing_constraints,
                metrics: dsd_target_metrics(dsd),
                notes: dsd.notes.clone(),
            }
        })
        .collect();
    DsdDecisionSummary {
        mode: report.mode.clone(),
        git_commit: report.git_commit.clone(),
        target_profile: "rate-aware DSD gate bands: good/excellent/stretch".to_string(),
        candidates,
    }
}

fn dsd_target_metrics(dsd: &DsdMeasurement) -> Vec<DsdTargetMetric> {
    let gate = DsdRateGate::for_measurement(dsd);
    let excellent_sinad =
        gate.worst_sinad_min_db + (gate.worst_sinad_score_max_db - gate.worst_sinad_min_db) * 0.5;
    let mut metrics = vec![
        target_metric_higher(
            "median_sinad_db",
            dsd.inband_snr_db,
            gate.worst_sinad_min_db,
            excellent_sinad,
            gate.worst_sinad_score_max_db,
        ),
        target_metric_higher(
            "worst_sinad_db",
            dsd.inband_snr_worst_db,
            gate.worst_sinad_min_db,
            excellent_sinad,
            gate.worst_sinad_score_max_db,
        ),
        target_metric_lower(
            "worst_inband_noise_rms_dbfs",
            dsd.inband_noise_worst_rms_dbfs,
            gate.worst_noise_max_dbfs,
            gate.worst_noise_max_dbfs - 6.0,
            gate.worst_noise_score_floor_dbfs,
        ),
        target_metric_lower(
            "sinad_spread_db",
            dsd.inband_snr_spread_db,
            gate.window_spread_max_db,
            (gate.window_spread_max_db - 1.0).max(0.0),
            (gate.window_spread_max_db - 2.0).max(0.0),
        ),
        target_metric_lower(
            "stereo_channel_mismatch_db",
            dsd.stereo_snr_worst_mismatch_db,
            3.0,
            2.0,
            1.0,
        ),
        target_metric_lower(
            "idle_tone_dbfs",
            dsd.idle_worst_tone_dbfs.or(dsd.idle_tone_dbfs),
            -70.0,
            -80.0,
            -90.0,
        ),
        target_metric_higher(
            "spur_margin_db",
            dsd.inband_noise_spur_margin_db,
            15.0,
            18.0,
            25.0,
        ),
        target_metric_zero("clicks", candidate_clicks(dsd).map(|count| count as f64)),
        target_metric_zero(
            "clamps_resets",
            Some(
                (dsd.stability_resets
                    + dsd.state_clamps
                    + dsd.stress_stability_resets
                    + dsd.stress_state_clamps) as f64,
            ),
        ),
        target_metric_lower(
            "density_deviation",
            dsd.bit_density_max_deviation,
            0.0002,
            0.00015,
            0.0001,
        ),
        DsdTargetMetric {
            metric: "hf_residual_db".to_string(),
            value: dsd.high_freq_worst_residual_db,
            band: "informational".to_string(),
            caveat: None,
            good_threshold: None,
            excellent_threshold: None,
            stretch_threshold: None,
        },
    ];
    annotate_dsd_target_metric_caveats(dsd, &mut metrics);
    metrics
}

fn annotate_dsd_target_metric_caveats(dsd: &DsdMeasurement, metrics: &mut [DsdTargetMetric]) {
    if dsd_idle_worst_tone_is_dc_fixture_only(dsd) {
        annotate_metric_caveat(
            metrics,
            "idle_tone_dbfs",
            "signed tiny-DC fixture only; silence, dc_0, and -120 dBFS probes are clean",
        );
    }

    if dsd
        .inband_noise_peak_dbfs
        .is_some_and(|value| value.is_finite() && value <= -180.0)
    {
        annotate_metric_caveat(
            metrics,
            "spur_margin_db",
            "absolute peak is below -180 dBFS",
        );
    }
}

fn annotate_metric_caveat(metrics: &mut [DsdTargetMetric], metric_name: &str, caveat: &str) {
    if let Some(metric) = metrics
        .iter_mut()
        .find(|metric| metric.metric == metric_name && metric.band == "miss")
    {
        metric.band = "accepted caveat".to_string();
        metric.caveat = Some(caveat.to_string());
    }
}

fn candidate_clicks(dsd: &DsdMeasurement) -> Option<usize> {
    match (dsd.transient_click_candidates, dsd.program_click_candidates) {
        (Some(transient), Some(program)) => Some(transient + program),
        (Some(transient), None) => Some(transient),
        (None, Some(program)) => Some(program),
        (None, None) => None,
    }
}

fn target_metric_higher(
    metric: &str,
    value: Option<f64>,
    good: f64,
    excellent: f64,
    stretch: f64,
) -> DsdTargetMetric {
    DsdTargetMetric {
        metric: metric.to_string(),
        value,
        band: value
            .map(|value| {
                if value >= stretch {
                    "stretch"
                } else if value >= excellent {
                    "excellent"
                } else if value >= good {
                    "good"
                } else {
                    "miss"
                }
                .to_string()
            })
            .unwrap_or_else(|| "unavailable".to_string()),
        caveat: None,
        good_threshold: Some(good),
        excellent_threshold: Some(excellent),
        stretch_threshold: Some(stretch),
    }
}

fn target_metric_lower(
    metric: &str,
    value: Option<f64>,
    good: f64,
    excellent: f64,
    stretch: f64,
) -> DsdTargetMetric {
    DsdTargetMetric {
        metric: metric.to_string(),
        value,
        band: value
            .map(|value| {
                if value <= stretch {
                    "stretch"
                } else if value <= excellent {
                    "excellent"
                } else if value <= good {
                    "good"
                } else {
                    "miss"
                }
                .to_string()
            })
            .unwrap_or_else(|| "unavailable".to_string()),
        caveat: None,
        good_threshold: Some(good),
        excellent_threshold: Some(excellent),
        stretch_threshold: Some(stretch),
    }
}

fn target_metric_zero(metric: &str, value: Option<f64>) -> DsdTargetMetric {
    DsdTargetMetric {
        metric: metric.to_string(),
        value,
        band: value
            .map(|value| if value == 0.0 { "stretch" } else { "miss" }.to_string())
            .unwrap_or_else(|| "unavailable".to_string()),
        caveat: None,
        good_threshold: Some(0.0),
        excellent_threshold: Some(0.0),
        stretch_threshold: Some(0.0),
    }
}

fn dsd_decision_summary_markdown(summary: &DsdDecisionSummary) -> String {
    let mut md = String::new();
    md.push_str("# DSD decision summary\n\n");
    md.push_str(&format!("mode: {}\n\n", summary.mode));
    md.push_str("| Candidate | Path | Status | Score | Hard | Tonal | Broad | Baseband | Ultra | Runtime | Robust | Median | Worst | Spread | Stereo | Idle | Spur | Density | Clicks | Prep ms | Render ms | Failures |\n");
    md.push_str("| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for candidate in &summary.candidates {
        let metric = |name: &str| {
            candidate
                .metrics
                .iter()
                .find(|metric| metric.metric == name)
                .map(|metric| {
                    format!(
                        "{} ({})",
                        metric
                            .value
                            .map(|value| format!("{value:.3}"))
                            .unwrap_or_else(|| "n/a".to_string()),
                        metric.band
                    )
                })
                .unwrap_or_else(|| "n/a".to_string())
        };
        md.push_str(&format!(
            "| {} {} {} {} | {} | {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            candidate.filter,
            candidate.modulator,
            candidate.source_rate,
            candidate.dsd_rate,
            candidate.path_variant,
            candidate.status,
            candidate.score,
            candidate.score_sections.hard_health,
            candidate.score_sections.tonal_risk,
            candidate.score_sections.broad_residual,
            candidate.score_sections.baseband_agreement,
            candidate.score_sections.ultrasonic_profile,
            candidate.score_sections.runtime,
            candidate.score_sections.robustness,
            metric("median_sinad_db"),
            metric("worst_sinad_db"),
            metric("sinad_spread_db"),
            metric("stereo_channel_mismatch_db"),
            metric("idle_tone_dbfs"),
            metric("spur_margin_db"),
            metric("density_deviation"),
            metric("clicks"),
            fmt_md_opt(candidate.path_prepare_ms),
            fmt_md_opt(candidate.render_ms),
            candidate.hard_failures.len(),
        ));
    }
    let caveats: Vec<String> = summary
        .candidates
        .iter()
        .flat_map(|candidate| {
            candidate.metrics.iter().filter_map(|metric| {
                metric.caveat.as_ref().map(|caveat| {
                    format!(
                        "- {} {} {} {} `{}`: {}\n",
                        candidate.filter,
                        candidate.modulator,
                        candidate.source_rate,
                        candidate.dsd_rate,
                        metric.metric,
                        caveat
                    )
                })
            })
        })
        .collect();
    if !caveats.is_empty() {
        md.push_str("\n## Accepted Caveats\n\n");
        for caveat in caveats {
            md.push_str(&caveat);
        }
    }
    md
}

#[derive(Debug)]
struct DsdCandidateScore {
    status: DsdRankingStatus,
    score: f64,
    sections: DsdScoreSections,
    hard_failures: Vec<String>,
    missing_constraints: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct DsdRateGate {
    worst_sinad_min_db: f64,
    worst_sinad_score_max_db: f64,
    worst_noise_max_dbfs: f64,
    worst_noise_score_floor_dbfs: f64,
    window_spread_max_db: f64,
    high_freq_worst_residual_max_db: f64,
    spur_margin_min_db: f64,
}

impl DsdRateGate {
    fn for_measurement(dsd: &DsdMeasurement) -> Self {
        match dsd.dsd_rate.as_str() {
            "DSD64" => Self {
                worst_sinad_min_db: 112.0,
                worst_sinad_score_max_db: 130.0,
                worst_noise_max_dbfs: -124.0,
                worst_noise_score_floor_dbfs: -135.0,
                window_spread_max_db: 3.5,
                high_freq_worst_residual_max_db: 2.5,
                spur_margin_min_db: 5.0,
            },
            "DSD128" => Self {
                worst_sinad_min_db: 155.0,
                worst_sinad_score_max_db: 175.0,
                worst_noise_max_dbfs: -168.0,
                worst_noise_score_floor_dbfs: -185.0,
                window_spread_max_db: 3.5,
                high_freq_worst_residual_max_db: -7.5,
                spur_margin_min_db: 5.0,
            },
            "DSD256" => Self {
                worst_sinad_min_db: 180.0,
                worst_sinad_score_max_db: 200.0,
                worst_noise_max_dbfs: -190.0,
                worst_noise_score_floor_dbfs: -205.0,
                window_spread_max_db: 3.5,
                high_freq_worst_residual_max_db: -7.5,
                spur_margin_min_db: 5.0,
            },
            _ => Self {
                worst_sinad_min_db: 155.0,
                worst_sinad_score_max_db: 175.0,
                worst_noise_max_dbfs: -168.0,
                worst_noise_score_floor_dbfs: -185.0,
                window_spread_max_db: 3.5,
                high_freq_worst_residual_max_db: -7.5,
                spur_margin_min_db: 5.0,
            },
        }
    }

    fn score_floor_db(self) -> f64 {
        self.worst_sinad_min_db
    }
}

fn score_dsd_candidate(dsd: &DsdMeasurement) -> DsdCandidateScore {
    let mut score = 0.0;
    let mut hard_failures = Vec::new();
    let mut missing_constraints = Vec::new();
    let artifact_windows = dsd_artifact_window_stats(&dsd.inband_windows);
    let gate = DsdRateGate::for_measurement(dsd);

    require_metric(&mut missing_constraints, "inband_snr_db", dsd.inband_snr_db);
    require_metric(
        &mut missing_constraints,
        "inband_snr_worst_db",
        dsd.inband_snr_worst_db,
    );
    require_metric(
        &mut missing_constraints,
        "inband_snr_spread_db",
        dsd.inband_snr_spread_db,
    );
    require_metric(
        &mut missing_constraints,
        "stereo_snr_worst_mismatch_db",
        dsd.stereo_snr_worst_mismatch_db,
    );
    require_metric(
        &mut missing_constraints,
        "inband_noise_worst_rms_dbfs",
        dsd.inband_noise_worst_rms_dbfs,
    );
    require_metric(
        &mut missing_constraints,
        "inband_noise_spur_margin_db",
        dsd.inband_noise_spur_margin_db,
    );
    require_metric(
        &mut missing_constraints,
        "decoded_abs_peak",
        dsd.decoded_abs_peak,
    );
    require_metric(
        &mut missing_constraints,
        "bit_density_max_deviation",
        dsd.bit_density_max_deviation,
    );

    hard_eq_zero(&mut hard_failures, "stability_resets", dsd.stability_resets);
    hard_eq_zero(&mut hard_failures, "state_clamps", dsd.state_clamps);
    hard_eq_zero(
        &mut hard_failures,
        "stress_stability_resets",
        dsd.stress_stability_resets,
    );
    hard_eq_zero(
        &mut hard_failures,
        "stress_state_clamps",
        dsd.stress_state_clamps,
    );
    hard_max(
        &mut hard_failures,
        "decoded_abs_peak",
        dsd.decoded_abs_peak,
        1.05,
    );
    hard_max(
        &mut hard_failures,
        "bit_density_max_deviation",
        dsd.bit_density_max_deviation,
        0.005,
    );
    hard_max(
        &mut hard_failures,
        "passband_peak_gain_20hz_20khz_db",
        dsd.passband_profile.peak_gain_20hz_20khz_db,
        0.02,
    );
    hard_min(
        &mut hard_failures,
        "inband_snr_worst_db",
        dsd.inband_snr_worst_db,
        gate.worst_sinad_min_db,
    );
    hard_max(
        &mut hard_failures,
        "inband_snr_spread_db",
        dsd.inband_snr_spread_db,
        gate.window_spread_max_db,
    );
    hard_max(
        &mut hard_failures,
        "stereo_snr_worst_mismatch_db",
        dsd.stereo_snr_worst_mismatch_db,
        3.0,
    );
    if dsd.inband_snr_window_count.unwrap_or(0) > 1 {
        hard_max(
            &mut hard_failures,
            "inband_noise_worst_rms_dbfs",
            dsd.inband_noise_worst_rms_dbfs,
            gate.worst_noise_max_dbfs,
        );
    }
    hard_min(
        &mut hard_failures,
        "inband_noise_spur_margin_db",
        dsd.inband_noise_spur_margin_db,
        gate.spur_margin_min_db,
    );
    if !dsd_idle_worst_tone_is_dc_fixture_only(dsd) {
        hard_max(
            &mut hard_failures,
            "idle_worst_tone_dbfs",
            dsd.idle_worst_tone_dbfs.or(dsd.idle_tone_dbfs),
            -70.0,
        );
    }
    hard_max(
        &mut hard_failures,
        "high_freq_worst_residual_db",
        dsd.high_freq_worst_residual_db,
        gate.high_freq_worst_residual_max_db,
    );
    hard_max_usize(
        &mut hard_failures,
        "transient_click_candidates",
        dsd.transient_click_candidates,
        0,
    );
    hard_max_usize(
        &mut hard_failures,
        "program_click_candidates",
        dsd.program_click_candidates,
        0,
    );

    add_higher_score(
        &mut score,
        dsd.inband_snr_worst_db,
        gate.score_floor_db(),
        gate.worst_sinad_score_max_db,
        28.0,
    );
    add_higher_score(
        &mut score,
        dsd.inband_noise_spur_margin_db,
        gate.spur_margin_min_db,
        30.0,
        24.0,
    );
    add_lower_score(
        &mut score,
        dsd.inband_noise_worst_rms_dbfs,
        gate.worst_noise_score_floor_dbfs,
        gate.worst_noise_max_dbfs,
        22.0,
    );
    add_lower_score(
        &mut score,
        dsd.inband_snr_spread_db,
        0.0,
        gate.window_spread_max_db,
        20.0,
    );
    add_lower_score(&mut score, dsd.stereo_snr_worst_mismatch_db, 0.0, 3.0, 12.0);
    add_lower_score(
        &mut score,
        dsd.high_freq_worst_residual_db,
        -20.0,
        -7.0,
        8.0,
    );
    add_lower_score(&mut score, dsd.high_freq_worst_spur_dbfs, -90.0, -45.0, 6.0);
    add_lower_score(
        &mut score,
        dsd.low_level_worst_residual_db,
        -150.0,
        -80.0,
        6.0,
    );
    add_lower_score(&mut score, dsd.multitone_residual_db, -80.0, -20.0, 6.0);
    add_lower_score(
        &mut score,
        dsd.idle_worst_tone_dbfs.or(dsd.idle_tone_dbfs),
        -120.0,
        -70.0,
        6.0,
    );
    add_lower_score(&mut score, dsd.inband_noise_peak_dbfs, -170.0, -110.0, 4.0);
    add_lower_score(&mut score, dsd.decoded_abs_peak, 0.0, 1.05, 4.0);
    add_lower_score(&mut score, dsd.bit_density_max_deviation, 0.0, 0.005, 4.0);
    add_lower_score(
        &mut score,
        artifact_windows.bad_window_ratio,
        0.0,
        0.5,
        16.0,
    );
    add_higher_score(
        &mut score,
        artifact_windows.artifact_free_worst_sinad_db,
        gate.score_floor_db(),
        gate.worst_sinad_score_max_db,
        10.0,
    );
    add_higher_score(
        &mut score,
        dsd.inband_snr_db,
        gate.score_floor_db(),
        gate.worst_sinad_score_max_db,
        4.0,
    );
    score -= artifact_windows.bad_window_count as f64 * 3.0;
    score -= dsd.transient_click_candidates.unwrap_or(0) as f64 * 20.0;
    score -= dsd.program_click_candidates.unwrap_or(0) as f64 * 20.0;
    score -= (dsd.stability_resets
        + dsd.state_clamps
        + dsd.stress_stability_resets
        + dsd.stress_state_clamps) as f64
        * 100.0;
    let sections = dsd_score_sections(dsd, artifact_windows, gate);

    let status = if !hard_failures.is_empty() {
        DsdRankingStatus::Reject
    } else if !missing_constraints.is_empty() {
        DsdRankingStatus::Partial
    } else {
        DsdRankingStatus::Pass
    };

    DsdCandidateScore {
        status,
        score,
        sections,
        hard_failures,
        missing_constraints,
    }
}

fn dsd_score_sections(
    dsd: &DsdMeasurement,
    artifact_windows: DsdArtifactWindowStats,
    gate: DsdRateGate,
) -> DsdScoreSections {
    let hard_health = if dsd_hard_health_ok(dsd) { 100.0 } else { 0.0 };
    let tonal_risk = higher_score(
        dsd.inband_noise_spur_margin_db,
        gate.spur_margin_min_db,
        30.0,
        24.0,
    ) + lower_score(
        dsd.idle_worst_tone_dbfs.or(dsd.idle_tone_dbfs),
        -120.0,
        -70.0,
        6.0,
    ) + lower_score(dsd.inband_noise_peak_dbfs, -170.0, -110.0, 4.0)
        + lower_score(dsd.high_freq_worst_spur_dbfs, -90.0, -45.0, 6.0)
        + lower_score(dsd.low_level_worst_spur_dbfs, -110.0, -45.0, 4.0)
        - candidate_clicks(dsd).unwrap_or(0) as f64 * 20.0;
    let broad_residual = lower_score(
        dsd.inband_noise_worst_rms_dbfs,
        gate.worst_noise_score_floor_dbfs,
        gate.worst_noise_max_dbfs,
        22.0,
    ) + lower_score(dsd.high_freq_worst_residual_db, -20.0, -7.0, 8.0)
        + lower_score(dsd.low_level_worst_residual_db, -150.0, -80.0, 6.0)
        + lower_score(dsd.multitone_residual_db, -80.0, -20.0, 6.0);
    let baseband_agreement = higher_score(
        dsd.inband_snr_worst_db,
        gate.score_floor_db(),
        gate.worst_sinad_score_max_db,
        28.0,
    ) + lower_score(
        dsd.inband_snr_spread_db,
        0.0,
        gate.window_spread_max_db,
        20.0,
    ) + lower_score(dsd.stereo_snr_worst_mismatch_db, 0.0, 3.0, 12.0)
        + lower_score(artifact_windows.bad_window_ratio, 0.0, 0.5, 16.0)
        + higher_score(
            artifact_windows.artifact_free_worst_sinad_db,
            gate.score_floor_db(),
            gate.worst_sinad_score_max_db,
            10.0,
        )
        + higher_score(
            dsd.inband_snr_db,
            gate.score_floor_db(),
            gate.worst_sinad_score_max_db,
            4.0,
        )
        - artifact_windows.bad_window_count as f64 * 3.0;
    let ultrasonic_profile = lower_score(dsd.ultrasonic_24_50k_max_dbfs, -100.0, -40.0, 8.0)
        + lower_score(dsd.ultrasonic_24_50k_window_spread_db, 0.0, 12.0, 4.0)
        + lower_score(dsd.ultrasonic_50_100k_max_dbfs, -80.0, -10.0, 8.0)
        + lower_score(dsd.ultrasonic_50_100k_window_spread_db, 0.0, 12.0, 4.0)
        + lower_score(dsd.ultrasonic_100_200k_max_dbfs, -70.0, -5.0, 6.0)
        + lower_score(dsd.ultrasonic_100_200k_window_spread_db, 0.0, 12.0, 3.0);
    let runtime = lower_score(dsd.path_prepare_ms, 0.0, 1_000.0, 5.0)
        + lower_score(dsd.render_ms, 0.0, 5_000.0, 10.0);
    let robustness = lower_score(
        dsd.inband_snr_spread_db,
        0.0,
        gate.window_spread_max_db,
        8.0,
    ) + lower_score(dsd.stereo_snr_worst_mismatch_db, 0.0, 3.0, 8.0)
        + higher_score(
            artifact_windows.artifact_free_worst_sinad_db,
            gate.score_floor_db(),
            gate.worst_sinad_score_max_db,
            6.0,
        )
        - artifact_windows.bad_window_count as f64 * 2.0;

    DsdScoreSections {
        hard_health,
        tonal_risk,
        broad_residual,
        baseband_agreement,
        ultrasonic_profile,
        runtime,
        robustness,
    }
}

fn dsd_hard_health_ok(dsd: &DsdMeasurement) -> bool {
    dsd.stability_resets == 0
        && dsd.state_clamps == 0
        && dsd.stress_stability_resets == 0
        && dsd.stress_state_clamps == 0
        && dsd.limiter_limited_events == 0
        && dsd.limiter_limited_samples == 0
        && dsd
            .decoded_abs_peak
            .is_some_and(|value| value.is_finite() && value <= 1.05)
        && dsd
            .bit_density_max_deviation
            .is_some_and(|value| value.is_finite() && value <= 0.005)
        && dsd
            .passband_profile
            .peak_gain_20hz_20khz_db
            .is_none_or(|value| value.is_finite() && value <= 0.02)
}

fn dsd_rank_group(dsd: &DsdMeasurement) -> String {
    format!("{}-{}-{}", dsd.filter, dsd.source_rate, dsd.dsd_rate)
}

fn compare_option_desc(left: Option<f64>, right: Option<f64>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => right.total_cmp(&left),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn require_metric(missing: &mut Vec<String>, name: &str, value: Option<f64>) {
    if value.is_none_or(|value| !value.is_finite()) {
        missing.push(name.to_string());
    }
}

fn hard_eq_zero(failures: &mut Vec<String>, name: &str, value: u64) {
    if value != 0 {
        failures.push(format!("{name}={value}"));
    }
}

fn hard_min(failures: &mut Vec<String>, name: &str, value: Option<f64>, limit: f64) {
    if let Some(value) = value.filter(|value| value.is_finite())
        && value < limit
    {
        failures.push(format!("{name} {value:.3} < {limit:.3}"));
    }
}

fn hard_max(failures: &mut Vec<String>, name: &str, value: Option<f64>, limit: f64) {
    if let Some(value) = value.filter(|value| value.is_finite())
        && value > limit
    {
        failures.push(format!("{name} {value:.3} > {limit:.3}"));
    }
}

fn hard_max_usize(failures: &mut Vec<String>, name: &str, value: Option<usize>, limit: usize) {
    if let Some(value) = value
        && value > limit
    {
        failures.push(format!("{name} {value} > {limit}"));
    }
}

fn add_higher_score(score: &mut f64, value: Option<f64>, min: f64, max: f64, weight: f64) {
    *score += higher_score(value, min, max, weight);
}

fn add_lower_score(score: &mut f64, value: Option<f64>, min: f64, max: f64, weight: f64) {
    *score += lower_score(value, min, max, weight);
}

fn higher_score(value: Option<f64>, min: f64, max: f64, weight: f64) -> f64 {
    if max <= min {
        return 0.0;
    }
    value
        .filter(|value| value.is_finite())
        .map(|value| ((value.clamp(min, max) - min) / (max - min)) * weight)
        .unwrap_or(0.0)
}

fn lower_score(value: Option<f64>, min: f64, max: f64, weight: f64) -> f64 {
    if max <= min {
        return 0.0;
    }
    if let Some(value) = value.filter(|value| value.is_finite()) {
        ((max - value.clamp(min, max)) / (max - min)) * weight
    } else {
        0.0
    }
}

fn dsd_rankings_csv(report: &SuiteReport) -> String {
    let rankings = dsd_quality_rankings(report);
    let mut csv = format!(
        "{},candidate_index,rank_group,rank,headline_snr_rank,status,constrained_quality_score,score_section_hard_health,score_section_tonal_risk,score_section_broad_residual,score_section_baseband_agreement,score_section_ultrasonic_profile,score_section_runtime,score_section_robustness,hard_failure_count,missing_constraint_count,filter,modulator,path_variant,source_rate,origin_source_rate,renderer_source_rate,intermediate_rate,intermediate_bits,intermediate_filter,path_prepare_ms,render_ms,dsd_rate,passband_max_deviation_20hz_20khz_db,passband_peak_gain_20hz_20khz_db,passband_gain_1k_db,passband_gain_3k_db,passband_gain_6k_db,passband_gain_10k_db,passband_gain_18k_db,candidate_notes,inband_snr_db,inband_snr_worst_db,inband_snr_spread_db,inband_bad_window_count,inband_bad_window_ratio,artifact_free_worst_sinad_db,stereo_snr_worst_mismatch_db,inband_snr_left_worst_db,inband_snr_right_worst_db,inband_noise_worst_rms_dbfs,inband_noise_spur_margin_db,ultrasonic_24_50k_max_dbfs,ultrasonic_24_50k_median_dbfs,ultrasonic_24_50k_window_spread_db,ultrasonic_50_100k_max_dbfs,ultrasonic_50_100k_median_dbfs,ultrasonic_50_100k_window_spread_db,ultrasonic_100_200k_max_dbfs,ultrasonic_100_200k_median_dbfs,ultrasonic_100_200k_window_spread_db,idle_worst_tone_dbfs,low_level_worst_residual_db,low_level_worst_spur_dbfs,high_freq_worst_residual_db,high_freq_worst_spur_dbfs,multitone_residual_db,multitone_spur_dbfs,overload_recovery_dbfs,decoded_abs_peak,limiter_peak_ratio_max,limiter_limited_events,limiter_limited_samples,bit_density_max_deviation,transient_click_candidates,transient_click_max_score,transient_click_max_residual,program_click_candidates,program_click_max_score,program_click_max_residual,stability_resets,state_clamps,stress_stability_resets,stress_state_clamps,hard_failures,missing_constraints\n",
        provenance_csv_header()
    );

    for ranking in rankings {
        let dsd = &report.dsd[ranking.dsd_index];
        let artifact_windows = dsd_artifact_window_stats(&dsd.inband_windows);
        let mut row = provenance_csv_values();
        row.extend([
            (ranking.dsd_index + 1).to_string(),
            ranking.rank_group,
            ranking.rank.to_string(),
            ranking
                .headline_snr_rank
                .map(|rank| rank.to_string())
                .unwrap_or_default(),
            ranking.status.as_str().to_string(),
            format!("{:.6}", ranking.score),
            format!("{:.6}", ranking.sections.hard_health),
            format!("{:.6}", ranking.sections.tonal_risk),
            format!("{:.6}", ranking.sections.broad_residual),
            format!("{:.6}", ranking.sections.baseband_agreement),
            format!("{:.6}", ranking.sections.ultrasonic_profile),
            format!("{:.6}", ranking.sections.runtime),
            format!("{:.6}", ranking.sections.robustness),
            ranking.hard_failures.len().to_string(),
            ranking.missing_constraints.len().to_string(),
            dsd.filter.clone(),
            dsd.modulator.clone(),
            dsd.path_variant.clone(),
            dsd.source_rate.to_string(),
            dsd.origin_source_rate.to_string(),
            dsd.renderer_source_rate.to_string(),
            dsd.intermediate_rate
                .map(|rate| rate.to_string())
                .unwrap_or_default(),
            dsd.intermediate_bits
                .map(|bits| bits.to_string())
                .unwrap_or_default(),
            dsd.intermediate_filter.clone().unwrap_or_default(),
            fmt_csv_opt(dsd.path_prepare_ms),
            fmt_csv_opt(dsd.render_ms),
            dsd.dsd_rate.clone(),
            fmt_csv_opt(dsd.passband_profile.max_deviation_20hz_20khz_db),
            fmt_csv_opt(dsd.passband_profile.peak_gain_20hz_20khz_db),
            fmt_csv_opt(dsd.passband_profile.gain_1k_db),
            fmt_csv_opt(dsd.passband_profile.gain_3k_db),
            fmt_csv_opt(dsd.passband_profile.gain_6k_db),
            fmt_csv_opt(dsd.passband_profile.gain_10k_db),
            fmt_csv_opt(dsd.passband_profile.gain_18k_db),
            dsd.notes.join("; "),
            fmt_csv_opt(dsd.inband_snr_db),
            fmt_csv_opt(dsd.inband_snr_worst_db),
            fmt_csv_opt(dsd.inband_snr_spread_db),
            artifact_windows.bad_window_count.to_string(),
            fmt_csv_opt(artifact_windows.bad_window_ratio),
            fmt_csv_opt(artifact_windows.artifact_free_worst_sinad_db),
            fmt_csv_opt(dsd.stereo_snr_worst_mismatch_db),
            fmt_csv_opt(dsd.inband_snr_left_worst_db),
            fmt_csv_opt(dsd.inband_snr_right_worst_db),
            fmt_csv_opt(dsd.inband_noise_worst_rms_dbfs),
            fmt_csv_opt(dsd.inband_noise_spur_margin_db),
            fmt_csv_opt(dsd.ultrasonic_24_50k_max_dbfs),
            fmt_csv_opt(dsd.ultrasonic_24_50k_median_dbfs),
            fmt_csv_opt(dsd.ultrasonic_24_50k_window_spread_db),
            fmt_csv_opt(dsd.ultrasonic_50_100k_max_dbfs),
            fmt_csv_opt(dsd.ultrasonic_50_100k_median_dbfs),
            fmt_csv_opt(dsd.ultrasonic_50_100k_window_spread_db),
            fmt_csv_opt(dsd.ultrasonic_100_200k_max_dbfs),
            fmt_csv_opt(dsd.ultrasonic_100_200k_median_dbfs),
            fmt_csv_opt(dsd.ultrasonic_100_200k_window_spread_db),
            fmt_csv_opt(dsd.idle_worst_tone_dbfs.or(dsd.idle_tone_dbfs)),
            fmt_csv_opt(dsd.low_level_worst_residual_db),
            fmt_csv_opt(dsd.low_level_worst_spur_dbfs),
            fmt_csv_opt(dsd.high_freq_worst_residual_db),
            fmt_csv_opt(dsd.high_freq_worst_spur_dbfs),
            fmt_csv_opt(dsd.multitone_residual_db),
            fmt_csv_opt(dsd.multitone_spur_dbfs),
            fmt_csv_opt(dsd.overload_recovery_dbfs),
            fmt_csv_opt(dsd.decoded_abs_peak),
            fmt_csv_opt(dsd.limiter_peak_ratio_max),
            dsd.limiter_limited_events.to_string(),
            dsd.limiter_limited_samples.to_string(),
            fmt_csv_opt(dsd.bit_density_max_deviation),
            dsd.transient_click_candidates
                .map(|count| count.to_string())
                .unwrap_or_default(),
            fmt_csv_opt(dsd.transient_click_max_score),
            fmt_csv_opt(dsd.transient_click_max_residual),
            dsd.program_click_candidates
                .map(|count| count.to_string())
                .unwrap_or_default(),
            fmt_csv_opt(dsd.program_click_max_score),
            fmt_csv_opt(dsd.program_click_max_residual),
            dsd.stability_resets.to_string(),
            dsd.state_clamps.to_string(),
            dsd.stress_stability_resets.to_string(),
            dsd.stress_state_clamps.to_string(),
            ranking.hard_failures.join("; "),
            ranking.missing_constraints.join("; "),
        ]);
        csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
        csv.push('\n');
    }

    csv
}

fn dsd_modulator_pairs(report: &SuiteReport) -> Vec<(&DsdMeasurement, &DsdMeasurement)> {
    let mut pairs = Vec::new();
    for standard in report.dsd.iter().filter(|dsd| dsd.modulator == "Standard") {
        if let Some(ec2) = report.dsd.iter().find(|dsd| {
            dsd.modulator == "EcDepth2"
                && dsd.filter == standard.filter
                && dsd.path_variant == standard.path_variant
                && dsd.source_rate == standard.source_rate
                && dsd.origin_source_rate == standard.origin_source_rate
                && dsd.renderer_source_rate == standard.renderer_source_rate
                && dsd.dsd_rate == standard.dsd_rate
        }) {
            pairs.push((standard, ec2));
        }
    }
    pairs
}

fn dsd_modulator_comparison_metrics(
    standard: &DsdMeasurement,
    ec2: &DsdMeasurement,
) -> [(&'static str, Option<f64>, Option<f64>, bool); 11] {
    [
        (
            "inband_snr_worst_db",
            standard.inband_snr_worst_db,
            ec2.inband_snr_worst_db,
            true,
        ),
        (
            "inband_snr_db",
            standard.inband_snr_db,
            ec2.inband_snr_db,
            true,
        ),
        (
            "inband_noise_spur_margin_db",
            standard.inband_noise_spur_margin_db,
            ec2.inband_noise_spur_margin_db,
            true,
        ),
        (
            "inband_snr_spread_db",
            standard.inband_snr_spread_db,
            ec2.inband_snr_spread_db,
            false,
        ),
        (
            "overload_recovery_dbfs",
            standard.overload_recovery_dbfs,
            ec2.overload_recovery_dbfs,
            false,
        ),
        (
            "high_freq_worst_residual_db",
            standard.high_freq_worst_residual_db,
            ec2.high_freq_worst_residual_db,
            false,
        ),
        (
            "high_freq_worst_spur_dbfs",
            standard.high_freq_worst_spur_dbfs,
            ec2.high_freq_worst_spur_dbfs,
            false,
        ),
        (
            "multitone_residual_db",
            standard.multitone_residual_db,
            ec2.multitone_residual_db,
            false,
        ),
        (
            "multitone_spur_dbfs",
            standard.multitone_spur_dbfs,
            ec2.multitone_spur_dbfs,
            false,
        ),
        (
            "bit_density_max_deviation",
            standard.bit_density_max_deviation,
            ec2.bit_density_max_deviation,
            false,
        ),
        (
            "decoded_abs_peak",
            standard.decoded_abs_peak,
            ec2.decoded_abs_peak,
            false,
        ),
    ]
}

fn dsd_modulator_comparison_csv(report: &SuiteReport) -> String {
    let mut csv = String::from(
        "filter,dsd_rate,metric,standard,ec_depth2,delta_ec2_minus_standard,better_modulator,lower_is_better\n",
    );
    for (standard, ec2) in dsd_modulator_pairs(report) {
        for (metric, standard_value, ec2_value, higher_is_better) in
            dsd_modulator_comparison_metrics(standard, ec2)
        {
            let delta = match (standard_value, ec2_value) {
                (Some(standard), Some(ec2)) => Some(ec2 - standard),
                _ => None,
            };
            let better = match (standard_value, ec2_value) {
                (Some(standard), Some(ec2)) if higher_is_better && ec2 > standard => "EcDepth2",
                (Some(standard), Some(ec2)) if higher_is_better && standard > ec2 => "Standard",
                (Some(standard), Some(ec2)) if !higher_is_better && ec2 < standard => "EcDepth2",
                (Some(standard), Some(ec2)) if !higher_is_better && standard < ec2 => "Standard",
                (Some(_), Some(_)) => "tie",
                _ => "",
            };
            let row = [
                standard.filter.clone(),
                standard.dsd_rate.clone(),
                metric.to_string(),
                fmt_csv_opt(standard_value),
                fmt_csv_opt(ec2_value),
                fmt_csv_opt(delta),
                better.to_string(),
                (!higher_is_better).to_string(),
            ];
            csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
            csv.push('\n');
        }
    }
    csv
}

fn dsd_modulator_comparison_markdown(report: &SuiteReport) -> String {
    let mut md = String::new();
    md.push_str("# DSD Standard vs EC Depth 2 comparison\n\n");
    md.push_str(&format!("mode: {}\n\n", report.mode));
    md.push_str(
        "Delta is EC Depth 2 minus Standard. For dBFS residual/artifact metrics and density, lower is better.\n\n",
    );
    let pairs = dsd_modulator_pairs(report);
    if pairs.is_empty() {
        md.push_str("No Standard/EC Depth 2 pairs found.\n");
        return md;
    }
    for (standard, ec2) in pairs {
        md.push_str(&format!("## {} {}\n\n", standard.filter, standard.dsd_rate));
        md.push_str("| Metric | Standard | EC Depth 2 | Delta | Better |\n");
        md.push_str("| --- | ---: | ---: | ---: | --- |\n");
        for (metric, standard_value, ec2_value, higher_is_better) in
            dsd_modulator_comparison_metrics(standard, ec2)
        {
            let delta = match (standard_value, ec2_value) {
                (Some(standard), Some(ec2)) => Some(ec2 - standard),
                _ => None,
            };
            let better = match (standard_value, ec2_value) {
                (Some(standard), Some(ec2)) if higher_is_better && ec2 > standard => "EC Depth 2",
                (Some(standard), Some(ec2)) if higher_is_better && standard > ec2 => "Standard",
                (Some(standard), Some(ec2)) if !higher_is_better && ec2 < standard => "EC Depth 2",
                (Some(standard), Some(ec2)) if !higher_is_better && standard < ec2 => "Standard",
                (Some(_), Some(_)) => "tie",
                _ => "n/a",
            };
            md.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                metric,
                fmt_md_opt(standard_value),
                fmt_md_opt(ec2_value),
                fmt_md_opt(delta),
                better,
            ));
        }
        md.push('\n');
    }
    md
}

fn dsd128_ec4a_candidates_csv(report: &SuiteReport) -> String {
    let mut csv = String::from(
        "candidate_index,status,score,path_variant,origin_source_rate,renderer_source_rate,intermediate_rate,intermediate_bits,intermediate_filter,path_prepare_ms,render_ms,modulator,headroom_db,dither_shape,dither_scale,dither_prng,leak_alpha,lf_floor_gamma,common_side_beta,common_side_common_seed,common_side_side_seed,pressure_only,quality_pressure,quality_pressure_threshold,quality_pressure_hold,seed_left,seed_right,median_sinad,worst_sinad,spread,left_worst,right_worst,stereo_mismatch,spur_margin,left_spur_margin,right_spur_margin,idle_worst,density,clicks,resets_clamps,hf_residual,depth4_ratio,hard_failures,notes\n",
    );
    for (idx, dsd) in report.dsd.iter().enumerate() {
        if !is_dsd128_split16k_candidate(dsd) {
            continue;
        }
        let score = score_dsd_candidate(dsd);
        let row = [
            (idx + 1).to_string(),
            score.status.as_str().to_string(),
            format!("{:.6}", score.score),
            dsd.path_variant.clone(),
            dsd.origin_source_rate.to_string(),
            dsd.renderer_source_rate.to_string(),
            dsd.intermediate_rate
                .map(|rate| rate.to_string())
                .unwrap_or_default(),
            dsd.intermediate_bits
                .map(|bits| bits.to_string())
                .unwrap_or_default(),
            dsd.intermediate_filter.clone().unwrap_or_default(),
            fmt_csv_opt(dsd.path_prepare_ms),
            fmt_csv_opt(dsd.render_ms),
            dsd.modulator.clone(),
            fmt_csv_opt(Some(dsd128_input_gain_db(dsd))),
            dsd_note_string(dsd, "dsd128_ec_dither_shape")
                .unwrap_or_default()
                .to_string(),
            dsd_note_value(dsd, "dsd128_ec_dither_scale_multiplier")
                .map(|value| format!("{value:.6}"))
                .unwrap_or_default(),
            dsd_note_string(dsd, "dsd128_ec_dither_prng")
                .unwrap_or("XorShift64")
                .to_string(),
            dsd_note_value(dsd, "dsd128_ec_dither_leak_alpha")
                .map(|value| format!("{value:.6}"))
                .unwrap_or_else(|| "1.000000".to_string()),
            dsd_note_value(dsd, "dsd128_ec_dither_lf_floor_gamma")
                .map(|value| format!("{value:.6}"))
                .unwrap_or_else(|| "0.000000".to_string()),
            dsd_note_value(dsd, "dsd128_ec_common_side_dither_beta")
                .map(|value| format!("{value:.6}"))
                .unwrap_or_default(),
            dsd_note_string(dsd, "dsd128_ec_common_side_common_seed")
                .unwrap_or_default()
                .to_string(),
            dsd_note_string(dsd, "dsd128_ec_common_side_side_seed")
                .unwrap_or_default()
                .to_string(),
            dsd_note_bool(dsd, "dsd128_ec4a_allow_predictive_triggers")
                .map(|allow| (!allow).to_string())
                .unwrap_or_default(),
            dsd_note_bool(dsd, "dsd128_ec4a_quality_pressure")
                .map(|allow| allow.to_string())
                .unwrap_or_else(|| {
                    dsd.notes
                        .iter()
                        .any(|note| note == "dsd128_ec4a_quality_pressure=true")
                        .to_string()
                }),
            dsd_note_value(dsd, "dsd128_ec4a_quality_pressure_threshold")
                .map(|value| format!("{value:.6}"))
                .unwrap_or_default(),
            dsd_note_value(dsd, "dsd128_ec4a_quality_pressure_hold")
                .map(|value| format!("{value:.0}"))
                .unwrap_or_default(),
            dsd_note_string(dsd, "dsd_seed_left")
                .unwrap_or_default()
                .to_string(),
            dsd_note_string(dsd, "dsd_seed_right")
                .unwrap_or_default()
                .to_string(),
            fmt_csv_opt(dsd.inband_snr_db),
            fmt_csv_opt(dsd.inband_snr_worst_db),
            fmt_csv_opt(dsd.inband_snr_spread_db),
            fmt_csv_opt(dsd.inband_snr_left_worst_db),
            fmt_csv_opt(dsd.inband_snr_right_worst_db),
            fmt_csv_opt(dsd.stereo_snr_worst_mismatch_db),
            fmt_csv_opt(dsd.inband_noise_spur_margin_db),
            fmt_csv_opt(dsd.inband_noise_left_spur_margin_db),
            fmt_csv_opt(dsd.inband_noise_right_spur_margin_db),
            fmt_csv_opt(dsd.idle_worst_tone_dbfs.or(dsd.idle_tone_dbfs)),
            fmt_csv_opt(dsd.bit_density_max_deviation),
            candidate_clicks(dsd)
                .map(|count| count.to_string())
                .unwrap_or_default(),
            (dsd.stability_resets
                + dsd.state_clamps
                + dsd.stress_stability_resets
                + dsd.stress_state_clamps)
                .to_string(),
            fmt_csv_opt(dsd.high_freq_worst_residual_db),
            fmt_csv_opt(dsd.depth4_ratio),
            score.hard_failures.join("; "),
            dsd.notes.join("; "),
        ];
        csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
        csv.push('\n');
    }
    csv
}

fn dsd_idle_artifact_export_rows(
    report: &SuiteReport,
    run_label: &str,
) -> Vec<DsdIdleArtifactExportRow> {
    let rankings = dsd_quality_rankings(report);
    let mut rows = Vec::new();
    for (idx, dsd) in report.dsd.iter().enumerate() {
        let ranking = rankings.iter().find(|ranking| ranking.dsd_index == idx);
        let (rank_group, rank, status, score) = ranking
            .map(|ranking| {
                (
                    ranking.rank_group.clone(),
                    ranking.rank,
                    ranking.status.as_str().to_string(),
                    ranking.score,
                )
            })
            .unwrap_or_else(|| ("unranked".to_string(), 0, "unknown".to_string(), 0.0));
        let quality_pressure =
            dsd_note_bool(dsd, "dsd128_ec4a_quality_pressure").unwrap_or_else(|| {
                dsd.notes
                    .iter()
                    .any(|note| note == "dsd128_ec4a_quality_pressure=true")
            });
        for artifact in &dsd.idle_artifacts {
            rows.push(DsdIdleArtifactExportRow {
                run_label: run_label.to_string(),
                candidate_index: idx + 1,
                rank_group: rank_group.clone(),
                rank,
                status: status.clone(),
                constrained_quality_score: score,
                filter: dsd.filter.clone(),
                modulator: dsd.modulator.clone(),
                path_variant: dsd.path_variant.clone(),
                source_rate: dsd.source_rate,
                origin_source_rate: dsd.origin_source_rate,
                renderer_source_rate: dsd.renderer_source_rate,
                intermediate_rate: dsd.intermediate_rate,
                intermediate_bits: dsd.intermediate_bits,
                intermediate_filter: dsd.intermediate_filter.clone(),
                dsd_rate: dsd.dsd_rate.clone(),
                wire_rate: dsd.wire_rate,
                headroom_db: dsd128_input_gain_db(dsd),
                dither_shape: dsd_note_string(dsd, "dsd128_ec_dither_shape").map(str::to_string),
                dither_scale: dsd_note_value(dsd, "dsd128_ec_dither_scale_multiplier"),
                dither_prng: dsd_note_string(dsd, "dsd128_ec_dither_prng").map(str::to_string),
                leak_alpha: dsd_note_value(dsd, "dsd128_ec_dither_leak_alpha"),
                lf_floor_gamma: dsd_note_value(dsd, "dsd128_ec_dither_lf_floor_gamma"),
                common_side_beta: dsd_note_value(dsd, "dsd128_ec_common_side_dither_beta"),
                common_side_common_seed: dsd_note_string(dsd, "dsd128_ec_common_side_common_seed")
                    .map(str::to_string),
                common_side_side_seed: dsd_note_string(dsd, "dsd128_ec_common_side_side_seed")
                    .map(str::to_string),
                pressure_only: dsd_note_bool(dsd, "dsd128_ec4a_allow_predictive_triggers")
                    .map(|allow| !allow),
                quality_pressure,
                quality_pressure_threshold: dsd_note_value(
                    dsd,
                    "dsd128_ec4a_quality_pressure_threshold",
                ),
                quality_pressure_hold: dsd_note_value(dsd, "dsd128_ec4a_quality_pressure_hold"),
                seed_left: dsd_note_string(dsd, "dsd_seed_left").map(str::to_string),
                seed_right: dsd_note_string(dsd, "dsd_seed_right").map(str::to_string),
                fixture_label: artifact.fixture_label.clone(),
                source: artifact.source.clone(),
                channel: artifact.channel.clone(),
                idle_peak_freq_hz: artifact.idle_peak_freq_hz,
                idle_peak_dbfs: artifact.idle_peak_dbfs,
                density_max_deviation: artifact.density_max_deviation,
                density_window_bits: artifact.density_window_bits,
                is_idle_worst_tone: artifact.is_idle_worst_tone,
                is_idle_worst_density: artifact.is_idle_worst_density,
            });
        }
    }
    rows
}

fn dsd_inband_spurs_csv(report: &SuiteReport, run_label: &str) -> String {
    let rankings = dsd_quality_rankings(report);
    let mut csv = String::from(
        "run_label,candidate_index,rank_group,rank,status,filter,modulator,path_variant,source_rate,origin_source_rate,renderer_source_rate,dsd_rate,wire_rate,headroom_db,dither_shape,dither_scale,seed_left,seed_right,channel,peak_spur_hz,peak_spur_dbfs,median_noise_bin_dbfs,p95_noise_bin_dbfs,p99_noise_bin_dbfs,margin_to_median_db,margin_to_p95_db,margin_to_p99_db\n",
    );
    for (idx, dsd) in report.dsd.iter().enumerate() {
        let ranking = rankings.iter().find(|ranking| ranking.dsd_index == idx);
        let (rank_group, rank, status) = ranking
            .map(|ranking| {
                (
                    ranking.rank_group.clone(),
                    ranking.rank,
                    ranking.status.as_str().to_string(),
                )
            })
            .unwrap_or_else(|| ("unranked".to_string(), 0, "unknown".to_string()));
        for spur in &dsd.inband_spurs {
            let row = [
                run_label.to_string(),
                (idx + 1).to_string(),
                rank_group.clone(),
                rank.to_string(),
                status.clone(),
                dsd.filter.clone(),
                dsd.modulator.clone(),
                dsd.path_variant.clone(),
                dsd.source_rate.to_string(),
                dsd.origin_source_rate.to_string(),
                dsd.renderer_source_rate.to_string(),
                dsd.dsd_rate.clone(),
                dsd.wire_rate
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                fmt_csv_opt(Some(dsd_input_gain_db(dsd))),
                dsd_rate_note_string(dsd, "ec_dither_shape")
                    .unwrap_or_default()
                    .to_string(),
                fmt_csv_opt(dsd_rate_note_value(dsd, "ec_dither_scale_multiplier")),
                dsd_note_string(dsd, "dsd_seed_left")
                    .unwrap_or_default()
                    .to_string(),
                dsd_note_string(dsd, "dsd_seed_right")
                    .unwrap_or_default()
                    .to_string(),
                spur.channel.clone(),
                fmt_csv_opt(spur.peak_spur_hz),
                fmt_csv_opt(spur.peak_spur_dbfs),
                fmt_csv_opt(spur.median_noise_bin_dbfs),
                fmt_csv_opt(spur.p95_noise_bin_dbfs),
                fmt_csv_opt(spur.p99_noise_bin_dbfs),
                fmt_csv_opt(spur.margin_to_median_db),
                fmt_csv_opt(spur.margin_to_p95_db),
                fmt_csv_opt(spur.margin_to_p99_db),
            ];
            csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
            csv.push('\n');
        }
    }
    csv
}

fn dsd_overload_recovery_diagnostics_csv(report: &SuiteReport, run_label: &str) -> String {
    let rankings = dsd_quality_rankings(report);
    let mut csv = String::from(
        "run_label,candidate_index,rank_group,rank,status,filter,modulator,path_variant,source_rate,origin_source_rate,renderer_source_rate,dsd_rate,wire_rate,headroom_db,dither_shape,dither_scale,seed_left,seed_right,source,channel,tail_start_sample,tail_len_samples,raw_tail_peak_value,raw_tail_peak_dbfs,equals_dsd64_min_nonzero_step,nonzero_tail_samples,max_nonzero_run_samples,tail_rms_dbfs,fft_peak_hz,fft_peak_dbfs,reconstructed_tail_peak_dbfs,tail_density_max_deviation,density_window_bits\n",
    );
    for (idx, dsd) in report.dsd.iter().enumerate() {
        let ranking = rankings.iter().find(|ranking| ranking.dsd_index == idx);
        let (rank_group, rank, status) = ranking
            .map(|ranking| {
                (
                    ranking.rank_group.clone(),
                    ranking.rank,
                    ranking.status.as_str().to_string(),
                )
            })
            .unwrap_or_else(|| ("unranked".to_string(), 0, "unknown".to_string()));
        for diagnostic in &dsd.overload_recovery_diagnostics {
            let row = [
                run_label.to_string(),
                (idx + 1).to_string(),
                rank_group.clone(),
                rank.to_string(),
                status.clone(),
                dsd.filter.clone(),
                dsd.modulator.clone(),
                dsd.path_variant.clone(),
                dsd.source_rate.to_string(),
                dsd.origin_source_rate.to_string(),
                dsd.renderer_source_rate.to_string(),
                dsd.dsd_rate.clone(),
                dsd.wire_rate
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                fmt_csv_opt(Some(dsd_input_gain_db(dsd))),
                dsd_rate_note_string(dsd, "ec_dither_shape")
                    .unwrap_or_default()
                    .to_string(),
                fmt_csv_opt(dsd_rate_note_value(dsd, "ec_dither_scale_multiplier")),
                dsd_note_string(dsd, "dsd_seed_left")
                    .unwrap_or_default()
                    .to_string(),
                dsd_note_string(dsd, "dsd_seed_right")
                    .unwrap_or_default()
                    .to_string(),
                diagnostic.source.clone(),
                diagnostic.channel.clone(),
                diagnostic.tail_start_sample.to_string(),
                diagnostic.tail_len_samples.to_string(),
                fmt_csv_opt(diagnostic.raw_tail_peak_value),
                fmt_csv_opt(diagnostic.raw_tail_peak_dbfs),
                diagnostic.equals_dsd64_min_nonzero_step.to_string(),
                diagnostic.nonzero_tail_samples.to_string(),
                diagnostic.max_nonzero_run_samples.to_string(),
                fmt_csv_opt(diagnostic.tail_rms_dbfs),
                fmt_csv_opt(diagnostic.fft_peak_hz),
                fmt_csv_opt(diagnostic.fft_peak_dbfs),
                fmt_csv_opt(diagnostic.reconstructed_tail_peak_dbfs),
                fmt_csv_opt(diagnostic.tail_density_max_deviation),
                diagnostic.density_window_bits.to_string(),
            ];
            csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
            csv.push('\n');
        }
    }
    csv
}

fn dsd_inband_windows_csv(report: &SuiteReport, run_label: &str) -> String {
    let rankings = dsd_quality_rankings(report);
    let mut csv = String::from(
        "run_label,candidate_index,rank_group,rank,status,filter,modulator,path_variant,source_rate,origin_source_rate,renderer_source_rate,dsd_rate,wire_rate,headroom_db,dither_shape,dither_scale,seed_left,seed_right,channel,start_s,sinad_db,noise_rms_dbfs,peak_spur_hz,peak_spur_dbfs,noise_20_200_dbfs,noise_200_2k_dbfs,noise_2k_8k_dbfs,noise_8k_16k_dbfs,noise_16k_20k_dbfs,is_worst\n",
    );
    for (idx, dsd) in report.dsd.iter().enumerate() {
        let ranking = rankings.iter().find(|ranking| ranking.dsd_index == idx);
        let (rank_group, rank, status) = ranking
            .map(|ranking| {
                (
                    ranking.rank_group.clone(),
                    ranking.rank,
                    ranking.status.as_str().to_string(),
                )
            })
            .unwrap_or_else(|| ("unranked".to_string(), 0, "unknown".to_string()));
        for window in &dsd.inband_windows {
            let row = [
                run_label.to_string(),
                (idx + 1).to_string(),
                rank_group.clone(),
                rank.to_string(),
                status.clone(),
                dsd.filter.clone(),
                dsd.modulator.clone(),
                dsd.path_variant.clone(),
                dsd.source_rate.to_string(),
                dsd.origin_source_rate.to_string(),
                dsd.renderer_source_rate.to_string(),
                dsd.dsd_rate.clone(),
                dsd.wire_rate
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                fmt_csv_opt(Some(dsd_input_gain_db(dsd))),
                dsd_rate_note_string(dsd, "ec_dither_shape")
                    .unwrap_or_default()
                    .to_string(),
                fmt_csv_opt(dsd_rate_note_value(dsd, "ec_dither_scale_multiplier")),
                dsd_note_string(dsd, "dsd_seed_left")
                    .unwrap_or_default()
                    .to_string(),
                dsd_note_string(dsd, "dsd_seed_right")
                    .unwrap_or_default()
                    .to_string(),
                window.channel.clone(),
                fmt_csv_opt(Some(window.start_s)),
                fmt_csv_opt(Some(window.sinad_db)),
                fmt_csv_opt(Some(window.noise_rms_dbfs)),
                fmt_csv_opt(window.peak_spur_hz),
                fmt_csv_opt(window.peak_spur_dbfs),
                fmt_csv_opt(window.noise_20_200_dbfs),
                fmt_csv_opt(window.noise_200_2k_dbfs),
                fmt_csv_opt(window.noise_2k_8k_dbfs),
                fmt_csv_opt(window.noise_8k_16k_dbfs),
                fmt_csv_opt(window.noise_16k_20k_dbfs),
                window.is_worst.to_string(),
            ];
            csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
            csv.push('\n');
        }
    }
    csv
}

fn dsd_ultrasonic_windows_csv(report: &SuiteReport, run_label: &str) -> String {
    let rankings = dsd_quality_rankings(report);
    let mut csv = String::from(
        "run_label,candidate_index,rank_group,rank,status,filter,modulator,path_variant,source_rate,origin_source_rate,renderer_source_rate,dsd_rate,wire_rate,headroom_db,dither_shape,dither_scale,seed_left,seed_right,channel,start_s,end_s,ultrasonic_24_50k_dbfs,ultrasonic_50_100k_dbfs,ultrasonic_100_200k_dbfs\n",
    );
    for (idx, dsd) in report.dsd.iter().enumerate() {
        let ranking = rankings.iter().find(|ranking| ranking.dsd_index == idx);
        let (rank_group, rank, status) = ranking
            .map(|ranking| {
                (
                    ranking.rank_group.clone(),
                    ranking.rank,
                    ranking.status.as_str().to_string(),
                )
            })
            .unwrap_or_else(|| ("unranked".to_string(), 0, "unknown".to_string()));
        for window in &dsd.ultrasonic_windows {
            let row = [
                run_label.to_string(),
                (idx + 1).to_string(),
                rank_group.clone(),
                rank.to_string(),
                status.clone(),
                dsd.filter.clone(),
                dsd.modulator.clone(),
                dsd.path_variant.clone(),
                dsd.source_rate.to_string(),
                dsd.origin_source_rate.to_string(),
                dsd.renderer_source_rate.to_string(),
                dsd.dsd_rate.clone(),
                dsd.wire_rate
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                fmt_csv_opt(Some(dsd_input_gain_db(dsd))),
                dsd_rate_note_string(dsd, "ec_dither_shape")
                    .unwrap_or_default()
                    .to_string(),
                fmt_csv_opt(dsd_rate_note_value(dsd, "ec_dither_scale_multiplier")),
                dsd_note_string(dsd, "dsd_seed_left")
                    .unwrap_or_default()
                    .to_string(),
                dsd_note_string(dsd, "dsd_seed_right")
                    .unwrap_or_default()
                    .to_string(),
                window.channel.clone(),
                fmt_csv_opt(Some(window.start_s)),
                fmt_csv_opt(Some(window.end_s)),
                fmt_csv_opt(window.ultrasonic_24_50k_dbfs),
                fmt_csv_opt(window.ultrasonic_50_100k_dbfs),
                fmt_csv_opt(window.ultrasonic_100_200k_dbfs),
            ];
            csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
            csv.push('\n');
        }
    }
    csv
}

fn dsd_premod_windows_csv(report: &SuiteReport, run_label: &str) -> String {
    let rankings = dsd_quality_rankings(report);
    let mut csv = String::from(
        "run_label,candidate_index,rank_group,rank,status,filter,modulator,path_variant,source_rate,origin_source_rate,renderer_source_rate,dsd_rate,wire_rate,headroom_db,dither_shape,dither_scale,seed_left,seed_right,start_s,start_sample,len_samples,rms_dbfs,max_abs_dbfs,dc_dbfs,crest_db,slope_rms_dbfs,tone_amp_dbfs,tone_phase_rad,residual_rms_dbfs,residual_relative_db,is_dsd_worst_start\n",
    );
    for (idx, dsd) in report.dsd.iter().enumerate() {
        let ranking = rankings.iter().find(|ranking| ranking.dsd_index == idx);
        let (rank_group, rank, status) = ranking
            .map(|ranking| {
                (
                    ranking.rank_group.clone(),
                    ranking.rank,
                    ranking.status.as_str().to_string(),
                )
            })
            .unwrap_or_else(|| ("unranked".to_string(), 0, "unknown".to_string()));
        for window in &dsd.premod_windows {
            let row = [
                run_label.to_string(),
                (idx + 1).to_string(),
                rank_group.clone(),
                rank.to_string(),
                status.clone(),
                dsd.filter.clone(),
                dsd.modulator.clone(),
                dsd.path_variant.clone(),
                dsd.source_rate.to_string(),
                dsd.origin_source_rate.to_string(),
                dsd.renderer_source_rate.to_string(),
                dsd.dsd_rate.clone(),
                dsd.wire_rate
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                fmt_csv_opt(Some(dsd_input_gain_db(dsd))),
                dsd_rate_note_string(dsd, "ec_dither_shape")
                    .unwrap_or_default()
                    .to_string(),
                fmt_csv_opt(dsd_rate_note_value(dsd, "ec_dither_scale_multiplier")),
                dsd_note_string(dsd, "dsd_seed_left")
                    .unwrap_or_default()
                    .to_string(),
                dsd_note_string(dsd, "dsd_seed_right")
                    .unwrap_or_default()
                    .to_string(),
                format!("{:.12}", window.start_s),
                window.start_sample.to_string(),
                window.len_samples.to_string(),
                format!("{:.6}", window.rms_dbfs),
                format!("{:.6}", window.max_abs_dbfs),
                format!("{:.6}", window.dc_dbfs),
                format!("{:.6}", window.crest_db),
                format!("{:.6}", window.slope_rms_dbfs),
                format!("{:.6}", window.tone_amp_dbfs),
                format!("{:.12}", window.tone_phase_rad),
                format!("{:.6}", window.residual_rms_dbfs),
                format!("{:.6}", window.residual_relative_db),
                window.is_dsd_worst_start.to_string(),
            ];
            csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
            csv.push('\n');
        }
    }
    csv
}

fn dsd_idle_artifacts_csv(rows: &[DsdIdleArtifactExportRow]) -> String {
    let mut csv = String::from(
        "run_label,candidate_index,rank_group,rank,status,constrained_quality_score,filter,modulator,path_variant,source_rate,origin_source_rate,renderer_source_rate,intermediate_rate,intermediate_bits,intermediate_filter,dsd_rate,wire_rate,headroom_db,dither_shape,dither_scale,dither_prng,leak_alpha,lf_floor_gamma,common_side_beta,common_side_common_seed,common_side_side_seed,pressure_only,quality_pressure,quality_pressure_threshold,quality_pressure_hold,seed_left,seed_right,fixture_label,source,channel,idle_peak_freq_hz,idle_peak_dbfs,density_max_deviation,density_window_bits,is_idle_worst_tone,is_idle_worst_density\n",
    );
    for row in rows {
        let values = [
            row.run_label.clone(),
            row.candidate_index.to_string(),
            row.rank_group.clone(),
            row.rank.to_string(),
            row.status.clone(),
            format!("{:.6}", row.constrained_quality_score),
            row.filter.clone(),
            row.modulator.clone(),
            row.path_variant.clone(),
            row.source_rate.to_string(),
            row.origin_source_rate.to_string(),
            row.renderer_source_rate.to_string(),
            row.intermediate_rate
                .map(|rate| rate.to_string())
                .unwrap_or_default(),
            row.intermediate_bits
                .map(|bits| bits.to_string())
                .unwrap_or_default(),
            row.intermediate_filter.clone().unwrap_or_default(),
            row.dsd_rate.clone(),
            row.wire_rate
                .map(|value| value.to_string())
                .unwrap_or_default(),
            fmt_csv_opt(Some(row.headroom_db)),
            row.dither_shape.clone().unwrap_or_default(),
            fmt_csv_opt(row.dither_scale),
            row.dither_prng.clone().unwrap_or_default(),
            fmt_csv_opt(row.leak_alpha),
            fmt_csv_opt(row.lf_floor_gamma),
            fmt_csv_opt(row.common_side_beta),
            row.common_side_common_seed.clone().unwrap_or_default(),
            row.common_side_side_seed.clone().unwrap_or_default(),
            row.pressure_only
                .map(|value| value.to_string())
                .unwrap_or_default(),
            row.quality_pressure.to_string(),
            fmt_csv_opt(row.quality_pressure_threshold),
            fmt_csv_opt(row.quality_pressure_hold),
            row.seed_left.clone().unwrap_or_default(),
            row.seed_right.clone().unwrap_or_default(),
            row.fixture_label.clone(),
            row.source.clone(),
            row.channel.clone(),
            fmt_csv_opt(row.idle_peak_freq_hz),
            fmt_csv_opt(row.idle_peak_dbfs),
            fmt_csv_opt(row.density_max_deviation),
            row.density_window_bits.to_string(),
            row.is_idle_worst_tone.to_string(),
            row.is_idle_worst_density.to_string(),
        ];
        csv.push_str(
            &values
                .into_iter()
                .map(csv_cell)
                .collect::<Vec<_>>()
                .join(","),
        );
        csv.push('\n');
    }
    csv
}

fn dsd128_ec4a_candidates_markdown(report: &SuiteReport) -> String {
    let mut rows: Vec<_> = report
        .dsd
        .iter()
        .enumerate()
        .filter(|(_, dsd)| is_dsd128_split16k_candidate(dsd))
        .map(|(idx, dsd)| (idx, dsd, score_dsd_candidate(dsd)))
        .collect();
    rows.sort_by(|(_, _, left), (_, _, right)| {
        left.status
            .cmp(&right.status)
            .then_with(|| right.score.total_cmp(&left.score))
    });

    let mut md = String::new();
    md.push_str("# DSD128 Split128k EC-4A candidates\n\n");
    md.push_str(&format!("mode: {}\n\n", report.mode));
    md.push_str("| # | Path | Modulator | Status | Worst | Spread | L Spur | R Spur | Idle | Density | Clicks | Prep ms | Render ms | Depth4 | Notes |\n");
    md.push_str(
        "| ---: | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |\n",
    );
    for (idx, dsd, score) in rows {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            idx + 1,
            dsd.path_variant,
            dsd.modulator,
            score.status.as_str(),
            fmt_md_opt(dsd.inband_snr_worst_db),
            fmt_md_opt(dsd.inband_snr_spread_db),
            fmt_md_opt(dsd.inband_noise_left_spur_margin_db),
            fmt_md_opt(dsd.inband_noise_right_spur_margin_db),
            fmt_md_opt(dsd.idle_worst_tone_dbfs.or(dsd.idle_tone_dbfs)),
            fmt_md_opt(dsd.bit_density_max_deviation),
            candidate_clicks(dsd)
                .map(|count| count.to_string())
                .unwrap_or_else(|| "n/a".to_string()),
            fmt_md_opt(dsd.path_prepare_ms),
            fmt_md_opt(dsd.render_ms),
            fmt_md_opt(dsd.depth4_ratio),
            markdown_cell(&dsd.notes.join("; ")),
        ));
    }
    md
}

fn path_comparison_pairs(report: &SuiteReport) -> Vec<(&DsdMeasurement, &DsdMeasurement)> {
    let mut pairs = Vec::new();
    for direct in report
        .dsd
        .iter()
        .filter(|dsd| dsd.path_variant == "direct48_dsd128")
    {
        if let Some(staged) = report.dsd.iter().find(|dsd| {
            dsd.path_variant == "pcm1536k32_dsd128"
                && dsd.filter == direct.filter
                && dsd.dsd_rate == direct.dsd_rate
                && dsd.origin_source_rate == direct.origin_source_rate
        }) {
            pairs.push((direct, staged));
        }
    }
    pairs
}

fn path_comparison_csv(report: &SuiteReport) -> String {
    let mut csv =
        String::from("filter,metric,direct48_dsd128,pcm1536k32_dsd128,delta_staged_minus_direct\n");
    let pairs = path_comparison_pairs(report);
    if pairs.is_empty() {
        return csv;
    }
    for (direct, staged) in pairs {
        for (metric, direct_value, staged_value) in path_comparison_metrics(direct, staged) {
            let delta = match (direct_value, staged_value) {
                (Some(direct), Some(staged)) => Some(staged - direct),
                _ => None,
            };
            let row = [
                direct.filter.clone(),
                metric.to_string(),
                fmt_csv_opt(direct_value),
                fmt_csv_opt(staged_value),
                fmt_csv_opt(delta),
            ];
            csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
            csv.push('\n');
        }
        let (accepted, reasons) = path_comparison_acceptance(direct, staged);
        csv.push_str(&format!(
            "{},{},{},,\n",
            csv_cell(direct.filter.clone()),
            csv_cell("accepted".to_string()),
            csv_cell(accepted.to_string())
        ));
        for reason in reasons {
            csv.push_str(&format!(
                "{},{},{},,\n",
                csv_cell(direct.filter.clone()),
                csv_cell("acceptance_note".to_string()),
                csv_cell(reason)
            ));
        }
    }
    csv
}

fn path_comparison_markdown(report: &SuiteReport) -> String {
    let mut md = String::new();
    md.push_str("# DSD128 48 kHz PCM midpoint comparison\n\n");
    md.push_str(&format!("mode: {}\n\n", report.mode));
    let pairs = path_comparison_pairs(report);
    if pairs.is_empty() {
        md.push_str("No direct/staged path pair found.\n");
        return md;
    }
    for (direct, staged) in pairs {
        let (accepted, reasons) = path_comparison_acceptance(direct, staged);
        md.push_str(&format!("## {}\n\n", direct.filter));
        md.push_str(&format!(
            "accepted for follow-up: **{}**\n\n",
            if accepted { "yes" } else { "no" }
        ));
        md.push_str(
            "Delta is staged minus direct; for `idle_worst_tone_dbfs`, lower is better.\n\n",
        );
        if !reasons.is_empty() {
            md.push_str("### Acceptance Notes\n\n");
            for reason in reasons {
                md.push_str(&format!("- {reason}\n"));
            }
            md.push('\n');
        }
        md.push_str("| Metric | Direct48 | PCM1536k32 | Delta |\n");
        md.push_str("| --- | ---: | ---: | ---: |\n");
        for (metric, direct_value, staged_value) in path_comparison_metrics(direct, staged) {
            let delta = match (direct_value, staged_value) {
                (Some(direct), Some(staged)) => Some(staged - direct),
                _ => None,
            };
            md.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                metric,
                fmt_md_opt(direct_value),
                fmt_md_opt(staged_value),
                fmt_md_opt(delta),
            ));
        }
        md.push('\n');
    }
    md
}

fn path_comparison_metrics(
    direct: &DsdMeasurement,
    staged: &DsdMeasurement,
) -> Vec<(&'static str, Option<f64>, Option<f64>)> {
    vec![
        (
            "median_sinad_db",
            direct.inband_snr_db,
            staged.inband_snr_db,
        ),
        (
            "worst_sinad_db",
            direct.inband_snr_worst_db,
            staged.inband_snr_worst_db,
        ),
        (
            "sinad_spread_db",
            direct.inband_snr_spread_db,
            staged.inband_snr_spread_db,
        ),
        (
            "stereo_snr_worst_mismatch_db",
            direct.stereo_snr_worst_mismatch_db,
            staged.stereo_snr_worst_mismatch_db,
        ),
        (
            "spur_margin_db",
            direct.inband_noise_spur_margin_db,
            staged.inband_noise_spur_margin_db,
        ),
        (
            "idle_worst_tone_dbfs",
            direct.idle_worst_tone_dbfs.or(direct.idle_tone_dbfs),
            staged.idle_worst_tone_dbfs.or(staged.idle_tone_dbfs),
        ),
        (
            "density_deviation",
            direct.bit_density_max_deviation,
            staged.bit_density_max_deviation,
        ),
        (
            "click_candidates",
            candidate_clicks(direct).map(|count| count as f64),
            candidate_clicks(staged).map(|count| count as f64),
        ),
        (
            "resets_clamps",
            Some(
                (direct.stability_resets
                    + direct.state_clamps
                    + direct.stress_stability_resets
                    + direct.stress_state_clamps) as f64,
            ),
            Some(
                (staged.stability_resets
                    + staged.state_clamps
                    + staged.stress_stability_resets
                    + staged.stress_state_clamps) as f64,
            ),
        ),
        (
            "path_prepare_ms",
            direct.path_prepare_ms,
            staged.path_prepare_ms,
        ),
        ("render_ms", direct.render_ms, staged.render_ms),
    ]
}

fn path_comparison_acceptance(
    direct: &DsdMeasurement,
    staged: &DsdMeasurement,
) -> (bool, Vec<String>) {
    let mut failures = Vec::new();
    let staged_score = score_dsd_candidate(staged);
    if !staged_score.hard_failures.is_empty() {
        failures.push(format!(
            "staged path has hard failures: {}",
            staged_score.hard_failures.join("; ")
        ));
    }

    match (direct.inband_snr_worst_db, staged.inband_snr_worst_db) {
        (Some(direct_worst), Some(staged_worst)) if staged_worst + 1.0 < direct_worst => {
            failures.push(format!(
                "worst SINAD regressed by {:.3} dB",
                direct_worst - staged_worst
            ));
        }
        (Some(_), Some(_)) => {}
        _ => failures.push("worst SINAD comparison unavailable".to_string()),
    }

    if target_band_lower_is_better(staged.inband_snr_spread_db, 3.0, 5.0)
        < target_band_lower_is_better(direct.inband_snr_spread_db, 3.0, 5.0)
    {
        failures.push("staged SINAD spread fell to a worse target band".to_string());
    }
    if target_band_lower_is_better(staged.stereo_snr_worst_mismatch_db, 2.0, 3.0)
        < target_band_lower_is_better(direct.stereo_snr_worst_mismatch_db, 2.0, 3.0)
    {
        failures.push("staged stereo mismatch fell to a worse target band".to_string());
    }

    let spur_improvement = match (
        direct.inband_noise_spur_margin_db,
        staged.inband_noise_spur_margin_db,
    ) {
        (Some(direct_spur), Some(staged_spur)) => staged_spur - direct_spur,
        _ => f64::NEG_INFINITY,
    };
    let idle_improvement = match (
        direct.idle_worst_tone_dbfs.or(direct.idle_tone_dbfs),
        staged.idle_worst_tone_dbfs.or(staged.idle_tone_dbfs),
    ) {
        (Some(direct_idle), Some(staged_idle)) => direct_idle - staged_idle,
        _ => f64::NEG_INFINITY,
    };
    if spur_improvement < 1.0 && idle_improvement < 1.0 {
        failures.push(format!(
            "neither spur margin nor idle tone improved by 1 dB (spur_margin_improvement_db {spur_improvement:.3}, idle_tone_improvement_db {idle_improvement:.3}; positive is better)"
        ));
    }

    let staged_clicks = candidate_clicks(staged).unwrap_or(usize::MAX);
    let staged_resets_clamps = staged.stability_resets
        + staged.state_clamps
        + staged.stress_stability_resets
        + staged.stress_state_clamps;
    if staged_clicks != 0 {
        failures.push(format!("staged path has {staged_clicks} click candidates"));
    }
    if staged_resets_clamps != 0 {
        failures.push(format!(
            "staged path has {staged_resets_clamps} resets/clamps"
        ));
    }

    (failures.is_empty(), failures)
}

fn target_band_lower_is_better(value: Option<f64>, excellent: f64, good: f64) -> u8 {
    match value.filter(|value| value.is_finite()) {
        Some(value) if value <= excellent => 2,
        Some(value) if value <= good => 1,
        Some(_) => 0,
        None => 0,
    }
}

fn is_dsd128_split16k_candidate(dsd: &DsdMeasurement) -> bool {
    dsd.filter == "Split128k"
        && matches!(dsd.source_rate, 44_100 | 48_000)
        && dsd.dsd_rate == "DSD128"
}

fn fmt_md_opt(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite())
        .map(|value| format!("{value:.3}"))
        .unwrap_or_else(|| "n/a".to_string())
}

fn markdown_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

fn fmt_csv_opt(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite())
        .map(|value| format!("{value:.6}"))
        .unwrap_or_default()
}

fn csv_cell(value: String) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value
    }
}

// CSV output mirrors the measurement schema columns, so explicit parameters document row shape.
#[allow(clippy::too_many_arguments)]
fn push_metric_csv_row(
    csv: &mut String,
    kind: &str,
    filter: &str,
    modulator: &str,
    path_variant: &str,
    source_rate: u32,
    origin_source_rate: Option<u32>,
    renderer_source_rate: Option<u32>,
    intermediate_rate: Option<u32>,
    intermediate_bits: Option<u32>,
    intermediate_filter: Option<&str>,
    target: &str,
    metric: &str,
    value: Option<f64>,
) {
    let Some(value) = value else {
        return;
    };
    let mut row = provenance_csv_values();
    row.extend([
        kind.to_string(),
        filter.to_string(),
        modulator.to_string(),
        path_variant.to_string(),
        source_rate.to_string(),
        origin_source_rate
            .map(|rate| rate.to_string())
            .unwrap_or_default(),
        renderer_source_rate
            .map(|rate| rate.to_string())
            .unwrap_or_default(),
        intermediate_rate
            .map(|rate| rate.to_string())
            .unwrap_or_default(),
        intermediate_bits
            .map(|bits| bits.to_string())
            .unwrap_or_default(),
        intermediate_filter.unwrap_or_default().to_string(),
        target.to_string(),
        metric.to_string(),
        format!("{value:.6}"),
    ]);
    csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
    csv.push('\n');
}

fn ec2_trace_metric_rows(trace: &DsdEc2DecisionTrace) -> Vec<(&'static str, f64)> {
    let mut total_commits = 0_u64;
    let mut near_tie_count = 0_u64;
    let mut ambiguity_override_count = 0_u64;
    let mut pressure_taper_count = 0_u64;
    let mut root_flip_count = 0_u64;
    let mut nonfinite_best_score_count = 0_u64;
    let mut f_abs_gt_1_count = 0_u64;
    let mut pressure_max = 0.0_f64;
    for snapshot in [trace.left.as_ref(), trace.right.as_ref()]
        .into_iter()
        .flatten()
    {
        total_commits += snapshot.summary.total_commits;
        near_tie_count += snapshot.summary.near_tie_count;
        ambiguity_override_count += snapshot.summary.ambiguity_override_count;
        pressure_taper_count += snapshot.summary.pressure_taper_count;
        root_flip_count += snapshot.summary.root_flip_count;
        nonfinite_best_score_count += snapshot.summary.nonfinite_best_score_count;
        f_abs_gt_1_count += snapshot.summary.f_abs_gt_1_count;
        for window in &snapshot.windows {
            pressure_max = pressure_max.max(window.pressure_max);
        }
    }
    vec![
        ("ec2_trace_total_commits", total_commits as f64),
        ("ec2_trace_near_tie_count", near_tie_count as f64),
        (
            "ec2_trace_ambiguity_override_count",
            ambiguity_override_count as f64,
        ),
        (
            "ec2_trace_pressure_taper_count",
            pressure_taper_count as f64,
        ),
        ("ec2_trace_root_flip_count", root_flip_count as f64),
        (
            "ec2_trace_nonfinite_best_score_count",
            nonfinite_best_score_count as f64,
        ),
        ("ec2_trace_f_abs_gt_1_count", f_abs_gt_1_count as f64),
        ("ec2_trace_pressure_max", pressure_max),
    ]
}

fn roundtrip_metrics_csv(report: &DsdRoundtripReport) -> String {
    let mut csv = format!(
        "{},candidate_index,fixture,status,filter,modulator,source_rate,dsd_rate,wire_rate,seconds,render_ms,delay_left,delay_right,gain_left,gain_right,correlation_left,correlation_right,correlation_worst,residual_rms_db_left,residual_rms_db_right,residual_rms_db_worst,inband_residual_rms_dbfs_left,inband_residual_rms_dbfs_right,inband_residual_rms_dbfs_worst,inband_residual_peak_dbfs_left,inband_residual_peak_dbfs_right,inband_residual_peak_dbfs_worst,inband_residual_spur_margin_db_left,inband_residual_spur_margin_db_right,inband_residual_spur_margin_db_worst,decoded_abs_peak,bit_density,bit_density_left,bit_density_right,bit_density_max_deviation,passband_peak_gain_20hz_20khz_db,limiter_peak_ratio_max,limiter_limited_events,limiter_limited_samples,stability_resets,state_clamps,hard_failures,notes\n",
        provenance_csv_header()
    );
    for m in &report.measurements {
        let mut row = provenance_csv_values();
        row.extend([m.candidate_index].map(|value| value.to_string()));
        row.extend([
            m.fixture.clone(),
            m.status.clone(),
            m.filter.clone(),
            m.modulator.clone(),
            m.source_rate.to_string(),
            m.dsd_rate.clone(),
            m.wire_rate.to_string(),
            format!("{:.6}", m.seconds),
            fmt_csv_opt(m.render_ms),
            m.alignment_delay_left_samples
                .map(|v| v.to_string())
                .unwrap_or_default(),
            m.alignment_delay_right_samples
                .map(|v| v.to_string())
                .unwrap_or_default(),
            fmt_csv_opt(m.alignment_gain_left),
            fmt_csv_opt(m.alignment_gain_right),
            fmt_csv_opt(m.correlation_left),
            fmt_csv_opt(m.correlation_right),
            fmt_csv_opt(m.correlation_worst),
            fmt_csv_opt(m.residual_rms_db_left),
            fmt_csv_opt(m.residual_rms_db_right),
            fmt_csv_opt(m.residual_rms_db_worst),
            fmt_csv_opt(m.inband_residual_rms_dbfs_left),
            fmt_csv_opt(m.inband_residual_rms_dbfs_right),
            fmt_csv_opt(m.inband_residual_rms_dbfs_worst),
            fmt_csv_opt(m.inband_residual_peak_dbfs_left),
            fmt_csv_opt(m.inband_residual_peak_dbfs_right),
            fmt_csv_opt(m.inband_residual_peak_dbfs_worst),
            fmt_csv_opt(m.inband_residual_spur_margin_db_left),
            fmt_csv_opt(m.inband_residual_spur_margin_db_right),
            fmt_csv_opt(m.inband_residual_spur_margin_db_worst),
            fmt_csv_opt(m.decoded_abs_peak),
            fmt_csv_opt(m.bit_density),
            fmt_csv_opt(m.bit_density_left),
            fmt_csv_opt(m.bit_density_right),
            fmt_csv_opt(m.bit_density_max_deviation),
            fmt_csv_opt(m.passband_profile.peak_gain_20hz_20khz_db),
            fmt_csv_opt(m.limiter_peak_ratio_max),
            m.limiter_limited_events.to_string(),
            m.limiter_limited_samples.to_string(),
            m.stability_resets.to_string(),
            m.state_clamps.to_string(),
        ]);
        row.extend([m.hard_failures.join(";"), m.notes.join(";")]);
        csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
        csv.push('\n');
    }
    csv
}

fn roundtrip_baseline_deltas_csv(report: &DsdRoundtripReport) -> String {
    let mut csv = format!(
        "{},candidate_index,fixture,filter,source_rate,dsd_rate,wire_rate,candidate_modulator,standard_baseline_modulator,current_ec2_baseline_modulator,candidate_status,standard_baseline_status,current_ec2_baseline_status,candidate_hard_failure_count,standard_baseline_hard_failure_count,current_ec2_baseline_hard_failure_count,candidate_correlation_worst,standard_baseline_correlation_worst,current_ec2_baseline_correlation_worst,candidate_minus_standard_correlation_worst,candidate_minus_current_ec2_correlation_worst,candidate_residual_rms_db_worst,standard_baseline_residual_rms_db_worst,current_ec2_baseline_residual_rms_db_worst,candidate_minus_standard_residual_rms_db_worst_db,candidate_minus_current_ec2_residual_rms_db_worst_db,candidate_inband_residual_rms_dbfs_worst,standard_baseline_inband_residual_rms_dbfs_worst,current_ec2_baseline_inband_residual_rms_dbfs_worst,candidate_minus_standard_inband_residual_rms_dbfs_worst_db,candidate_minus_current_ec2_inband_residual_rms_dbfs_worst_db,candidate_inband_residual_peak_dbfs_worst,standard_baseline_inband_residual_peak_dbfs_worst,current_ec2_baseline_inband_residual_peak_dbfs_worst,candidate_minus_standard_inband_residual_peak_dbfs_worst_db,candidate_minus_current_ec2_inband_residual_peak_dbfs_worst_db,candidate_inband_residual_spur_margin_db_worst,standard_baseline_inband_residual_spur_margin_db_worst,current_ec2_baseline_inband_residual_spur_margin_db_worst,candidate_minus_standard_inband_residual_spur_margin_db_worst_db,candidate_minus_current_ec2_inband_residual_spur_margin_db_worst_db,candidate_decoded_abs_peak,standard_baseline_decoded_abs_peak,current_ec2_baseline_decoded_abs_peak,candidate_minus_standard_decoded_abs_peak,candidate_minus_current_ec2_decoded_abs_peak,candidate_bit_density_max_deviation,standard_baseline_bit_density_max_deviation,current_ec2_baseline_bit_density_max_deviation,candidate_minus_standard_bit_density_max_deviation,candidate_minus_current_ec2_bit_density_max_deviation,candidate_render_ms,standard_baseline_render_ms,current_ec2_baseline_render_ms,candidate_minus_standard_render_ms,candidate_minus_current_ec2_render_ms\n",
        provenance_csv_header()
    );
    for d in &report.baseline_deltas {
        let mut row = provenance_csv_values();
        row.extend([
            d.candidate_index.to_string(),
            d.fixture.clone(),
            d.filter.clone(),
            d.source_rate.to_string(),
            d.dsd_rate.clone(),
            d.wire_rate.to_string(),
            d.candidate_modulator.clone(),
            d.standard_baseline_modulator.clone(),
            d.current_ec2_baseline_modulator.clone(),
            d.candidate_status.clone(),
            d.standard_baseline_status.clone(),
            d.current_ec2_baseline_status.clone(),
            d.candidate_hard_failure_count.to_string(),
            d.standard_baseline_hard_failure_count.to_string(),
            d.current_ec2_baseline_hard_failure_count.to_string(),
            fmt_csv_opt(d.candidate_correlation_worst),
            fmt_csv_opt(d.standard_baseline_correlation_worst),
            fmt_csv_opt(d.current_ec2_baseline_correlation_worst),
            fmt_csv_opt(d.candidate_minus_standard_correlation_worst),
            fmt_csv_opt(d.candidate_minus_current_ec2_correlation_worst),
            fmt_csv_opt(d.candidate_residual_rms_db_worst),
            fmt_csv_opt(d.standard_baseline_residual_rms_db_worst),
            fmt_csv_opt(d.current_ec2_baseline_residual_rms_db_worst),
            fmt_csv_opt(d.candidate_minus_standard_residual_rms_db_worst_db),
            fmt_csv_opt(d.candidate_minus_current_ec2_residual_rms_db_worst_db),
            fmt_csv_opt(d.candidate_inband_residual_rms_dbfs_worst),
            fmt_csv_opt(d.standard_baseline_inband_residual_rms_dbfs_worst),
            fmt_csv_opt(d.current_ec2_baseline_inband_residual_rms_dbfs_worst),
            fmt_csv_opt(d.candidate_minus_standard_inband_residual_rms_dbfs_worst_db),
            fmt_csv_opt(d.candidate_minus_current_ec2_inband_residual_rms_dbfs_worst_db),
            fmt_csv_opt(d.candidate_inband_residual_peak_dbfs_worst),
            fmt_csv_opt(d.standard_baseline_inband_residual_peak_dbfs_worst),
            fmt_csv_opt(d.current_ec2_baseline_inband_residual_peak_dbfs_worst),
            fmt_csv_opt(d.candidate_minus_standard_inband_residual_peak_dbfs_worst_db),
            fmt_csv_opt(d.candidate_minus_current_ec2_inband_residual_peak_dbfs_worst_db),
            fmt_csv_opt(d.candidate_inband_residual_spur_margin_db_worst),
            fmt_csv_opt(d.standard_baseline_inband_residual_spur_margin_db_worst),
            fmt_csv_opt(d.current_ec2_baseline_inband_residual_spur_margin_db_worst),
            fmt_csv_opt(d.candidate_minus_standard_inband_residual_spur_margin_db_worst_db),
            fmt_csv_opt(d.candidate_minus_current_ec2_inband_residual_spur_margin_db_worst_db),
            fmt_csv_opt(d.candidate_decoded_abs_peak),
            fmt_csv_opt(d.standard_baseline_decoded_abs_peak),
            fmt_csv_opt(d.current_ec2_baseline_decoded_abs_peak),
            fmt_csv_opt(d.candidate_minus_standard_decoded_abs_peak),
            fmt_csv_opt(d.candidate_minus_current_ec2_decoded_abs_peak),
            fmt_csv_opt(d.candidate_bit_density_max_deviation),
            fmt_csv_opt(d.standard_baseline_bit_density_max_deviation),
            fmt_csv_opt(d.current_ec2_baseline_bit_density_max_deviation),
            fmt_csv_opt(d.candidate_minus_standard_bit_density_max_deviation),
            fmt_csv_opt(d.candidate_minus_current_ec2_bit_density_max_deviation),
            fmt_csv_opt(d.candidate_render_ms),
            fmt_csv_opt(d.standard_baseline_render_ms),
            fmt_csv_opt(d.current_ec2_baseline_render_ms),
            fmt_csv_opt(d.candidate_minus_standard_render_ms),
            fmt_csv_opt(d.candidate_minus_current_ec2_render_ms),
        ]);
        csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
        csv.push('\n');
    }
    csv
}

fn roundtrip_baseline_deltas_markdown(report: &DsdRoundtripReport) -> String {
    let mut md = String::new();
    md.push_str("# DSD round-trip baseline deltas\n\n");
    md.push_str("Delta columns are `candidate - baseline`; for residual and peak dBFS metrics, negative deltas mean the candidate is lower than the baseline.\n\n");
    md.push_str("| Fixture | Rate | Filter | Candidate | Δ residual RMS vs Standard | Δ residual RMS vs current EC2 | Δ in-band RMS vs Standard | Δ in-band RMS vs current EC2 | Δ spur margin vs Standard | Δ spur margin vs current EC2 | Δ render ms vs Standard | Δ render ms vs current EC2 | Status |\n");
    md.push_str(
        "| --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |\n",
    );
    for d in &report.baseline_deltas {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {}/{}/{} |\n",
            d.fixture,
            d.dsd_rate,
            d.filter,
            d.candidate_modulator,
            fmt_md_opt(d.candidate_minus_standard_residual_rms_db_worst_db),
            fmt_md_opt(d.candidate_minus_current_ec2_residual_rms_db_worst_db),
            fmt_md_opt(d.candidate_minus_standard_inband_residual_rms_dbfs_worst_db),
            fmt_md_opt(d.candidate_minus_current_ec2_inband_residual_rms_dbfs_worst_db),
            fmt_md_opt(d.candidate_minus_standard_inband_residual_spur_margin_db_worst_db),
            fmt_md_opt(d.candidate_minus_current_ec2_inband_residual_spur_margin_db_worst_db),
            fmt_md_opt(d.candidate_minus_standard_render_ms),
            fmt_md_opt(d.candidate_minus_current_ec2_render_ms),
            d.candidate_status,
            d.standard_baseline_status,
            d.current_ec2_baseline_status,
        ));
    }
    md
}

fn roundtrip_pink_noise_seed_summary_csv(report: &DsdRoundtripReport) -> String {
    let mut csv = format!(
        "{},fixture,filter,modulator,source_rate,dsd_rate,wire_rate,seed_count,candidate_indices,status_worst,hard_failure_count_total,correlation_worst_across_seeds,correlation_median_across_seeds,residual_rms_db_worst_across_seeds,residual_rms_db_median_across_seeds,inband_residual_rms_dbfs_worst_across_seeds,inband_residual_rms_dbfs_median_across_seeds,inband_residual_peak_dbfs_worst_across_seeds,inband_residual_peak_dbfs_median_across_seeds,inband_residual_spur_margin_db_worst_across_seeds,inband_residual_spur_margin_db_median_across_seeds,decoded_abs_peak_worst_across_seeds,decoded_abs_peak_median_across_seeds,bit_density_max_deviation_worst_across_seeds,bit_density_max_deviation_median_across_seeds,render_ms_worst_across_seeds,render_ms_median_across_seeds\n",
        provenance_csv_header()
    );
    for summary in &report.pink_noise_seed_summaries {
        let mut row = provenance_csv_values();
        row.extend([
            summary.fixture.clone(),
            summary.filter.clone(),
            summary.modulator.clone(),
            summary.source_rate.to_string(),
            summary.dsd_rate.clone(),
            summary.wire_rate.to_string(),
            summary.seed_count.to_string(),
            summary.candidate_indices.clone(),
            summary.status_worst.clone(),
            summary.hard_failure_count_total.to_string(),
            fmt_csv_opt(summary.correlation_worst_across_seeds),
            fmt_csv_opt(summary.correlation_median_across_seeds),
            fmt_csv_opt(summary.residual_rms_db_worst_across_seeds),
            fmt_csv_opt(summary.residual_rms_db_median_across_seeds),
            fmt_csv_opt(summary.inband_residual_rms_dbfs_worst_across_seeds),
            fmt_csv_opt(summary.inband_residual_rms_dbfs_median_across_seeds),
            fmt_csv_opt(summary.inband_residual_peak_dbfs_worst_across_seeds),
            fmt_csv_opt(summary.inband_residual_peak_dbfs_median_across_seeds),
            fmt_csv_opt(summary.inband_residual_spur_margin_db_worst_across_seeds),
            fmt_csv_opt(summary.inband_residual_spur_margin_db_median_across_seeds),
            fmt_csv_opt(summary.decoded_abs_peak_worst_across_seeds),
            fmt_csv_opt(summary.decoded_abs_peak_median_across_seeds),
            fmt_csv_opt(summary.bit_density_max_deviation_worst_across_seeds),
            fmt_csv_opt(summary.bit_density_max_deviation_median_across_seeds),
            fmt_csv_opt(summary.render_ms_worst_across_seeds),
            fmt_csv_opt(summary.render_ms_median_across_seeds),
        ]);
        csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
        csv.push('\n');
    }
    csv
}

fn roundtrip_pink_noise_seed_summary_markdown(report: &DsdRoundtripReport) -> String {
    let mut md = String::new();
    md.push_str("# DSD round-trip pink-noise seed summary\n\n");
    md.push_str("Worst columns are conservative across seed rows: residual, peak, density, and runtime use the maximum; correlation and spur margin use the minimum.\n\n");
    md.push_str("| Fixture | Rate | Filter | Modulator | Seeds | Worst in-band RMS | Median in-band RMS | Worst residual RMS | Median residual RMS | Worst spur margin | Median spur margin | Worst density | Median density | Status |\n");
    md.push_str(
        "| --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |\n",
    );
    for summary in &report.pink_noise_seed_summaries {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            summary.fixture,
            summary.dsd_rate,
            summary.filter,
            summary.modulator,
            summary.seed_count,
            fmt_md_opt(summary.inband_residual_rms_dbfs_worst_across_seeds),
            fmt_md_opt(summary.inband_residual_rms_dbfs_median_across_seeds),
            fmt_md_opt(summary.residual_rms_db_worst_across_seeds),
            fmt_md_opt(summary.residual_rms_db_median_across_seeds),
            fmt_md_opt(summary.inband_residual_spur_margin_db_worst_across_seeds),
            fmt_md_opt(summary.inband_residual_spur_margin_db_median_across_seeds),
            fmt_md_opt(summary.bit_density_max_deviation_worst_across_seeds),
            fmt_md_opt(summary.bit_density_max_deviation_median_across_seeds),
            summary.status_worst,
        ));
    }
    md
}

fn precheck_metrics_csv(report: &DsdPrecheckReport) -> String {
    let mut csv = format!(
        "{},candidate_index,probe,status,filter,modulator,source_rate,dsd_rate,wire_rate,seconds,frames,render_ms,decoded_abs_peak,bit_density,bit_density_left,bit_density_right,bit_density_max_deviation,bit_density_left_max_deviation,bit_density_right_max_deviation,limiter_peak_ratio_max,limiter_limited_events,limiter_limited_samples,stability_resets,state_clamps,hard_failures,notes\n",
        provenance_csv_header()
    );
    for m in &report.measurements {
        let mut row = provenance_csv_values();
        row.extend([m.candidate_index].map(|value| value.to_string()));
        row.extend([
            m.probe.clone(),
            m.status.clone(),
            m.filter.clone(),
            m.modulator.clone(),
            m.source_rate.to_string(),
            m.dsd_rate.clone(),
            m.wire_rate.to_string(),
            format!("{:.6}", m.seconds),
            m.frames.to_string(),
            fmt_csv_opt(m.render_ms),
            fmt_csv_opt(m.decoded_abs_peak),
            fmt_csv_opt(m.bit_density),
            fmt_csv_opt(m.bit_density_left),
            fmt_csv_opt(m.bit_density_right),
            fmt_csv_opt(m.bit_density_max_deviation),
            fmt_csv_opt(m.bit_density_left_max_deviation),
            fmt_csv_opt(m.bit_density_right_max_deviation),
            fmt_csv_opt(m.limiter_peak_ratio_max),
            m.limiter_limited_events.to_string(),
            m.limiter_limited_samples.to_string(),
            m.stability_resets.to_string(),
            m.state_clamps.to_string(),
            m.hard_failures.join(";"),
            m.notes.join(";"),
        ]);
        csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
        csv.push('\n');
    }
    csv
}

fn roundtrip_waveform_csv(report: &DsdRoundtripReport) -> String {
    let mut csv = String::from(
        "candidate_index,fixture,filter,modulator,dsd_rate,channel,sample_index,time_s,reference,measured,residual\n",
    );
    for point in &report.artifacts.waveform {
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{:.9},{:.12},{:.12},{:.12}\n",
            point.candidate_index,
            csv_cell(point.fixture.clone()),
            csv_cell(point.filter.clone()),
            csv_cell(point.modulator.clone()),
            csv_cell(point.dsd_rate.clone()),
            csv_cell(point.channel.clone()),
            point.sample_index,
            point.time_s,
            point.reference,
            point.measured,
            point.residual,
        ));
    }
    csv
}

fn roundtrip_spectrum_csv(points: &[RoundtripSpectrumPoint]) -> String {
    let mut csv = String::from(
        "candidate_index,fixture,filter,modulator,dsd_rate,channel,freq_hz,reference_dbfs,measured_dbfs,residual_dbfs\n",
    );
    for point in points {
        csv.push_str(&format!(
            "{},{},{},{},{},{},{:.6},{},{},{}\n",
            point.candidate_index,
            csv_cell(point.fixture.clone()),
            csv_cell(point.filter.clone()),
            csv_cell(point.modulator.clone()),
            csv_cell(point.dsd_rate.clone()),
            csv_cell(point.channel.clone()),
            point.freq_hz,
            fmt_csv_opt(point.reference_dbfs),
            fmt_csv_opt(point.measured_dbfs),
            fmt_csv_opt(point.residual_dbfs),
        ));
    }
    csv
}

fn roundtrip_spectrogram_csv(report: &DsdRoundtripReport) -> String {
    let mut csv = String::from(
        "candidate_index,fixture,filter,modulator,dsd_rate,channel,start_s,freq_hz,residual_dbfs\n",
    );
    for point in &report.artifacts.spectrogram {
        csv.push_str(&format!(
            "{},{},{},{},{},{},{:.9},{:.6},{:.6}\n",
            point.candidate_index,
            csv_cell(point.fixture.clone()),
            csv_cell(point.filter.clone()),
            csv_cell(point.modulator.clone()),
            csv_cell(point.dsd_rate.clone()),
            csv_cell(point.channel.clone()),
            point.start_s,
            point.freq_hz,
            point.residual_dbfs,
        ));
    }
    csv
}

fn metrics_csv(report: &SuiteReport) -> String {
    let mut csv = format!(
        "{},kind,filter,modulator,path_variant,source_rate,origin_source_rate,renderer_source_rate,intermediate_rate,intermediate_bits,intermediate_filter,target_or_dsd,metric,value\n",
        provenance_csv_header()
    );
    for m in &report.pcm {
        let target = m.target_rate.to_string();
        for (metric, value) in [
            ("dc_gain_db", m.dc_gain_db),
            ("gain_1k_db", m.gain_1k_db),
            ("gain_18k_db", m.gain_18k_db),
            (
                "passband_max_deviation_20hz_20khz_db",
                m.passband_profile.max_deviation_20hz_20khz_db,
            ),
            (
                "passband_peak_gain_20hz_20khz_db",
                m.passband_profile.peak_gain_20hz_20khz_db,
            ),
            ("passband_gain_1k_db", m.passband_profile.gain_1k_db),
            ("passband_gain_3k_db", m.passband_profile.gain_3k_db),
            ("passband_gain_6k_db", m.passband_profile.gain_6k_db),
            ("passband_gain_10k_db", m.passband_profile.gain_10k_db),
            ("passband_gain_18k_db", m.passband_profile.gain_18k_db),
            ("image_rejection_db", m.image_rejection_db),
            ("one_core_percent", m.one_core_percent),
        ] {
            push_metric_csv_row(
                &mut csv,
                "pcm",
                &m.filter,
                "",
                "",
                m.source_rate,
                None,
                None,
                None,
                None,
                None,
                &target,
                metric,
                value,
            );
        }
    }
    for m in &report.dsd {
        let artifact_windows = dsd_artifact_window_stats(&m.inband_windows);
        for (metric, value) in [
            (
                "passband_max_deviation_20hz_20khz_db",
                m.passband_profile.max_deviation_20hz_20khz_db,
            ),
            (
                "passband_peak_gain_20hz_20khz_db",
                m.passband_profile.peak_gain_20hz_20khz_db,
            ),
            ("passband_gain_1k_db", m.passband_profile.gain_1k_db),
            ("passband_gain_3k_db", m.passband_profile.gain_3k_db),
            ("passband_gain_6k_db", m.passband_profile.gain_6k_db),
            ("passband_gain_10k_db", m.passband_profile.gain_10k_db),
            ("passband_gain_18k_db", m.passband_profile.gain_18k_db),
            ("residual_db", m.residual_db),
            ("inband_snr_db", m.inband_snr_db),
            ("inband_snr_worst_db", m.inband_snr_worst_db),
            ("inband_snr_p05_db", m.inband_snr_p05_db),
            ("inband_snr_p95_db", m.inband_snr_p95_db),
            ("inband_snr_best_db", m.inband_snr_best_db),
            ("inband_snr_spread_db", m.inband_snr_spread_db),
            ("inband_snr_left_db", m.inband_snr_left_db),
            ("inband_snr_right_db", m.inband_snr_right_db),
            ("inband_snr_left_worst_db", m.inband_snr_left_worst_db),
            ("inband_snr_right_worst_db", m.inband_snr_right_worst_db),
            ("inband_snr_left_spread_db", m.inband_snr_left_spread_db),
            ("inband_snr_right_spread_db", m.inband_snr_right_spread_db),
            (
                "stereo_snr_worst_mismatch_db",
                m.stereo_snr_worst_mismatch_db,
            ),
            (
                "inband_snr_window_count",
                m.inband_snr_window_count.map(|count| count as f64),
            ),
            (
                "inband_snr_worst_window_start_s",
                m.inband_snr_worst_window_start_s,
            ),
            (
                "inband_bad_window_count",
                Some(artifact_windows.bad_window_count as f64),
            ),
            ("inband_bad_window_ratio", artifact_windows.bad_window_ratio),
            (
                "artifact_free_worst_sinad_db",
                artifact_windows.artifact_free_worst_sinad_db,
            ),
            ("inband_noise_rms_dbfs", m.inband_noise_rms_dbfs),
            ("inband_noise_worst_rms_dbfs", m.inband_noise_worst_rms_dbfs),
            ("inband_noise_peak_dbfs", m.inband_noise_peak_dbfs),
            ("inband_noise_peak_spur_hz", m.inband_noise_peak_spur_hz),
            ("inband_noise_spur_margin_db", m.inband_noise_spur_margin_db),
            (
                "inband_noise_left_spur_margin_db",
                m.inband_noise_left_spur_margin_db,
            ),
            (
                "inband_noise_right_spur_margin_db",
                m.inband_noise_right_spur_margin_db,
            ),
            ("inband_noise_20_200_dbfs", m.inband_noise_20_200_dbfs),
            ("inband_noise_200_2k_dbfs", m.inband_noise_200_2k_dbfs),
            ("inband_noise_2k_8k_dbfs", m.inband_noise_2k_8k_dbfs),
            ("inband_noise_8k_16k_dbfs", m.inband_noise_8k_16k_dbfs),
            ("inband_noise_16k_20k_dbfs", m.inband_noise_16k_20k_dbfs),
            ("ultrasonic_24_50k_max_dbfs", m.ultrasonic_24_50k_max_dbfs),
            (
                "ultrasonic_24_50k_median_dbfs",
                m.ultrasonic_24_50k_median_dbfs,
            ),
            (
                "ultrasonic_24_50k_window_spread_db",
                m.ultrasonic_24_50k_window_spread_db,
            ),
            ("ultrasonic_50_100k_max_dbfs", m.ultrasonic_50_100k_max_dbfs),
            (
                "ultrasonic_50_100k_median_dbfs",
                m.ultrasonic_50_100k_median_dbfs,
            ),
            (
                "ultrasonic_50_100k_window_spread_db",
                m.ultrasonic_50_100k_window_spread_db,
            ),
            (
                "ultrasonic_100_200k_max_dbfs",
                m.ultrasonic_100_200k_max_dbfs,
            ),
            (
                "ultrasonic_100_200k_median_dbfs",
                m.ultrasonic_100_200k_median_dbfs,
            ),
            (
                "ultrasonic_100_200k_window_spread_db",
                m.ultrasonic_100_200k_window_spread_db,
            ),
            ("idle_tone_dbfs", m.idle_tone_dbfs),
            ("idle_worst_tone_dbfs", m.idle_worst_tone_dbfs),
            (
                "idle_worst_density_deviation",
                m.idle_worst_density_deviation,
            ),
            ("low_level_worst_residual_db", m.low_level_worst_residual_db),
            ("low_level_worst_spur_dbfs", m.low_level_worst_spur_dbfs),
            (
                "high_freq_tone_worst_residual_db",
                m.high_freq_tone_worst_residual_db,
            ),
            (
                "high_freq_tone_worst_spur_dbfs",
                m.high_freq_tone_worst_spur_dbfs,
            ),
            ("high_freq_imd_residual_db", m.high_freq_imd_residual_db),
            ("high_freq_imd_spur_dbfs", m.high_freq_imd_spur_dbfs),
            ("high_freq_worst_residual_db", m.high_freq_worst_residual_db),
            ("high_freq_worst_spur_dbfs", m.high_freq_worst_spur_dbfs),
            ("multitone_residual_db", m.multitone_residual_db),
            ("multitone_spur_dbfs", m.multitone_spur_dbfs),
            ("overload_recovery_dbfs", m.overload_recovery_dbfs),
            (
                "transient_click_candidates",
                m.transient_click_candidates.map(|count| count as f64),
            ),
            ("transient_click_max_score", m.transient_click_max_score),
            (
                "transient_click_max_residual",
                m.transient_click_max_residual,
            ),
            (
                "program_click_candidates",
                m.program_click_candidates.map(|count| count as f64),
            ),
            ("program_click_max_score", m.program_click_max_score),
            ("program_click_max_residual", m.program_click_max_residual),
            ("decoded_low", m.decoded_low),
            ("decoded_peak", m.decoded_peak),
            ("decoded_abs_peak", m.decoded_abs_peak),
            ("bit_density", m.bit_density),
            ("bit_density_left", m.bit_density_left),
            ("bit_density_right", m.bit_density_right),
            ("bit_density_max_deviation", m.bit_density_max_deviation),
            (
                "bit_density_left_max_deviation",
                m.bit_density_left_max_deviation,
            ),
            (
                "bit_density_right_max_deviation",
                m.bit_density_right_max_deviation,
            ),
            ("transition_rate", m.transition_rate),
            ("limiter_peak_ratio_max", m.limiter_peak_ratio_max),
            (
                "limiter_current_block_peak_ratio",
                m.limiter_current_block_peak_ratio,
            ),
            ("limiter_current_block_gain", m.limiter_current_block_gain),
            (
                "limiter_current_block_limited_samples",
                Some(m.limiter_current_block_limited_samples as f64),
            ),
            (
                "limiter_limited_events",
                Some(m.limiter_limited_events as f64),
            ),
            (
                "limiter_limited_samples",
                Some(m.limiter_limited_samples as f64),
            ),
            ("dsd256_improvement_db", m.dsd256_improvement_db),
            ("depth4_ratio", m.depth4_ratio),
            ("stability_resets", Some(m.stability_resets as f64)),
            ("state_clamps", Some(m.state_clamps as f64)),
            (
                "stress_stability_resets",
                Some(m.stress_stability_resets as f64),
            ),
            ("stress_state_clamps", Some(m.stress_state_clamps as f64)),
        ] {
            push_metric_csv_row(
                &mut csv,
                "dsd",
                &m.filter,
                &m.modulator,
                &m.path_variant,
                m.source_rate,
                Some(m.origin_source_rate),
                Some(m.renderer_source_rate),
                m.intermediate_rate,
                m.intermediate_bits,
                m.intermediate_filter.as_deref(),
                &m.dsd_rate,
                metric,
                value,
            );
        }
        if let Some(trace) = &m.ec2_decision_trace {
            for (metric, value) in ec2_trace_metric_rows(trace) {
                push_metric_csv_row(
                    &mut csv,
                    "dsd",
                    &m.filter,
                    &m.modulator,
                    &m.path_variant,
                    m.source_rate,
                    Some(m.origin_source_rate),
                    Some(m.renderer_source_rate),
                    m.intermediate_rate,
                    m.intermediate_bits,
                    m.intermediate_filter.as_deref(),
                    &m.dsd_rate,
                    metric,
                    Some(value),
                );
            }
        }
    }
    for ranking in dsd_quality_rankings(report) {
        let m = &report.dsd[ranking.dsd_index];
        for (metric, value) in [
            ("candidate_index", Some((ranking.dsd_index + 1) as f64)),
            ("quality_rank", Some(ranking.rank as f64)),
            (
                "headline_snr_rank",
                ranking.headline_snr_rank.map(|rank| rank as f64),
            ),
            ("constrained_quality_score", Some(ranking.score)),
            (
                "score_section_hard_health",
                Some(ranking.sections.hard_health),
            ),
            (
                "score_section_tonal_risk",
                Some(ranking.sections.tonal_risk),
            ),
            (
                "score_section_broad_residual",
                Some(ranking.sections.broad_residual),
            ),
            (
                "score_section_baseband_agreement",
                Some(ranking.sections.baseband_agreement),
            ),
            (
                "score_section_ultrasonic_profile",
                Some(ranking.sections.ultrasonic_profile),
            ),
            ("score_section_runtime", Some(ranking.sections.runtime)),
            (
                "score_section_robustness",
                Some(ranking.sections.robustness),
            ),
            ("rank_status_code", Some(ranking.status.code())),
            (
                "hard_failure_count",
                Some(ranking.hard_failures.len() as f64),
            ),
            (
                "missing_constraint_count",
                Some(ranking.missing_constraints.len() as f64),
            ),
        ] {
            push_metric_csv_row(
                &mut csv,
                "dsd_rank",
                &m.filter,
                &m.modulator,
                &m.path_variant,
                m.source_rate,
                Some(m.origin_source_rate),
                Some(m.renderer_source_rate),
                m.intermediate_rate,
                m.intermediate_bits,
                m.intermediate_filter.as_deref(),
                &m.dsd_rate,
                metric,
                value,
            );
        }
    }
    csv
}

fn ec4a_decision_trace_csv(report: &SuiteReport) -> String {
    let mut csv = String::from(EC4A_DECISION_TRACE_CSV_HEADER);
    for (candidate_index, measurement) in report.dsd.iter().enumerate() {
        let Some(trace) = &measurement.adaptive_decision_trace else {
            continue;
        };
        for (channel, snapshot) in [
            ("left", trace.left.as_ref()),
            ("right", trace.right.as_ref()),
        ] {
            let Some(snapshot) = snapshot else {
                continue;
            };
            push_ec4a_trace_rows(
                &mut csv,
                candidate_index + 1,
                &measurement.filter,
                &measurement.modulator,
                &measurement.path_variant,
                measurement.source_rate,
                &measurement.dsd_rate,
                channel,
                snapshot,
            );
        }
    }
    csv
}

const EC4A_DECISION_TRACE_CSV_HEADER: &str = "candidate_index,filter,modulator,path_variant,source_rate,dsd_rate,channel,window_index,start_bit,len_bits,total_commits,depth4_commits,depth4_ratio,trigger_none,trigger_guard,trigger_pressure,trigger_transient,trigger_ambiguity,pressure_mean,pressure_max,pressure_ge_045,pressure_ge_060,pressure_ge_072,pressure_ge_082,root_margin_mean,root_margin_min,root_hot_raw_count,guard_hot_count,chosen_plus,chosen_minus,transitions,root_flip_count,depth4_root_flip_count,depth4_shadow_depth2_same_root,depth4_shadow_depth2_diff_root,depth4_shadow_score_delta_sum,depth4_shadow_score_delta_min,depth4_shadow_score_delta_max,nonfinite_best_score_count,clean_policy_mismatch_count,f_abs_gt_1_count\n";

fn ec2_decision_trace_csv(report: &SuiteReport) -> String {
    let mut csv = String::from(EC2_DECISION_TRACE_CSV_HEADER);
    for (candidate_index, measurement) in report.dsd.iter().enumerate() {
        let Some(trace) = &measurement.ec2_decision_trace else {
            continue;
        };
        for (channel, snapshot) in [
            ("left", trace.left.as_ref()),
            ("right", trace.right.as_ref()),
        ] {
            let Some(snapshot) = snapshot else {
                continue;
            };
            push_ec2_trace_rows(
                &mut csv,
                candidate_index + 1,
                &measurement.filter,
                &measurement.modulator,
                &measurement.path_variant,
                measurement.source_rate,
                &measurement.dsd_rate,
                channel,
                snapshot,
                measurement.inband_snr_worst_window_start_s,
            );
        }
    }
    csv
}

fn ec2_window_summary_csv(report: &SuiteReport) -> String {
    let mut csv = String::from(
        "candidate_index,filter,modulator,path_variant,source_rate,renderer_source_rate,dsd_rate,channel,ec2_window_index,start_bit,len_bits,start_s,end_s,overlapping_inband_windows,overlaps_worst_inband_window,worst_inband_start_s,min_inband_sinad_db,max_inband_noise_rms_dbfs,total_commits,near_tie_count,ambiguity_override_count,pressure_taper_count,pressure_mean,pressure_max,root_margin_mean,root_margin_min,transitions,root_flip_count,quantizer_error_mean,quantizer_error_max,committed_state_pressure_mean,committed_state_pressure_max,committed_state_energy_mean,committed_state_energy_max,committed_state_stage0_max,committed_state_stage1_max,committed_state_stage2_max,committed_state_stage3_max,committed_state_stage4_max,committed_state_stage5_max,committed_state_stage6_max,nonfinite_best_score_count,f_abs_gt_1_count\n",
    );
    for (candidate_index, measurement) in report.dsd.iter().enumerate() {
        let Some(trace) = &measurement.ec2_decision_trace else {
            continue;
        };
        let wire_rate =
            dsd_wire_rate_from_name(&measurement.dsd_rate, measurement.renderer_source_rate)
                .unwrap_or(0) as f64;
        for (channel, snapshot) in [
            ("left", trace.left.as_ref()),
            ("right", trace.right.as_ref()),
        ] {
            let Some(snapshot) = snapshot else {
                continue;
            };
            for (window_index, window) in snapshot.windows.iter().enumerate() {
                let start_s = (wire_rate > 0.0).then_some(window.start_bit as f64 / wire_rate);
                let end_s = (wire_rate > 0.0)
                    .then_some((window.start_bit + window.len_bits) as f64 / wire_rate);
                let overlapping: Vec<&DsdInbandWindowRow> = measurement
                    .inband_windows
                    .iter()
                    .filter(|inband| {
                        inband.channel == channel
                            && match (start_s, end_s) {
                                (Some(start), Some(end)) => {
                                    inband.start_s >= start && inband.start_s < end
                                }
                                _ => false,
                            }
                    })
                    .collect();
                let overlaps_worst = overlapping.iter().any(|window| window.is_worst);
                let worst_start = measurement
                    .inband_windows
                    .iter()
                    .find(|window| window.channel == channel && window.is_worst)
                    .map(|window| window.start_s);
                let min_sinad = overlapping
                    .iter()
                    .map(|window| window.sinad_db)
                    .reduce(f64::min);
                let max_noise = overlapping
                    .iter()
                    .map(|window| window.noise_rms_dbfs)
                    .reduce(f64::max);
                let row = [
                    (candidate_index + 1).to_string(),
                    measurement.filter.clone(),
                    measurement.modulator.clone(),
                    measurement.path_variant.clone(),
                    measurement.source_rate.to_string(),
                    measurement.renderer_source_rate.to_string(),
                    measurement.dsd_rate.clone(),
                    channel.to_string(),
                    (window_index + 1).to_string(),
                    window.start_bit.to_string(),
                    window.len_bits.to_string(),
                    fmt_csv_opt(start_s),
                    fmt_csv_opt(end_s),
                    overlapping.len().to_string(),
                    overlaps_worst.to_string(),
                    fmt_csv_opt(worst_start),
                    fmt_csv_opt(min_sinad),
                    fmt_csv_opt(max_noise),
                    window.total_commits.to_string(),
                    window.near_tie_count.to_string(),
                    window.ambiguity_override_count.to_string(),
                    window.pressure_taper_count.to_string(),
                    format!("{:.9}", window.pressure_mean),
                    format!("{:.9}", window.pressure_max),
                    format!("{:.9}", window.root_margin_mean),
                    format!("{:.9}", window.root_margin_min),
                    window.transitions.to_string(),
                    window.root_flip_count.to_string(),
                    format!("{:.9}", window.quantizer_error_mean),
                    format!("{:.9}", window.quantizer_error_max),
                    format!("{:.9}", window.committed_state_pressure_mean),
                    format!("{:.9}", window.committed_state_pressure_max),
                    format!("{:.9}", window.committed_state_energy_mean),
                    format!("{:.9}", window.committed_state_energy_max),
                    format!("{:.9}", window.committed_state_stage0_max),
                    format!("{:.9}", window.committed_state_stage1_max),
                    format!("{:.9}", window.committed_state_stage2_max),
                    format!("{:.9}", window.committed_state_stage3_max),
                    format!("{:.9}", window.committed_state_stage4_max),
                    format!("{:.9}", window.committed_state_stage5_max),
                    format!("{:.9}", window.committed_state_stage6_max),
                    window.nonfinite_best_score_count.to_string(),
                    window.f_abs_gt_1_count.to_string(),
                ];
                csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
                csv.push('\n');
            }
        }
    }
    csv
}

const EC2_DECISION_TRACE_CSV_HEADER: &str = "candidate_index,filter,modulator,path_variant,source_rate,dsd_rate,channel,window_index,start_bit,len_bits,start_s,end_s,worst_window_start_s,overlaps_worst_window,total_commits,near_tie_count,ambiguity_override_count,pressure_taper_count,pressure_mean,pressure_max,pressure_ge_045,pressure_ge_060,pressure_ge_072,pressure_ge_082,root_margin_mean,root_margin_min,root_hot_raw_count,chosen_plus,chosen_minus,transitions,root_flip_count,quantizer_error_mean,quantizer_error_max,dc_bias_abs_mean,dc_bias_abs_max,dither_abs_mean,dither_abs_max,committed_state_pressure_mean,committed_state_pressure_max,committed_state_energy_mean,committed_state_energy_max,committed_state_stage0_max,committed_state_stage1_max,committed_state_stage2_max,committed_state_stage3_max,committed_state_stage4_max,committed_state_stage5_max,committed_state_stage6_max,nonfinite_best_score_count,f_abs_gt_1_count\n";

// EC2 trace rows mirror the long CSV schema so measurement columns stay explicit.
#[allow(clippy::too_many_arguments)]
fn push_ec2_trace_rows(
    csv: &mut String,
    candidate_index: usize,
    filter: &str,
    modulator: &str,
    path_variant: &str,
    source_rate: u32,
    dsd_rate: &str,
    channel: &str,
    snapshot: &Ec2DecisionTraceSnapshot,
    worst_window_start_s: Option<f64>,
) {
    let wire_rate = dsd_wire_rate_from_name(dsd_rate, source_rate).unwrap_or(0) as f64;
    for (window_index, window) in snapshot.windows.iter().enumerate() {
        let start_s = if wire_rate > 0.0 {
            Some(window.start_bit as f64 / wire_rate)
        } else {
            None
        };
        let end_s = if wire_rate > 0.0 {
            Some((window.start_bit + window.len_bits) as f64 / wire_rate)
        } else {
            None
        };
        let overlaps_worst = match (start_s, end_s, worst_window_start_s) {
            (Some(start), Some(end), Some(worst)) => worst >= start && worst < end,
            _ => false,
        };
        let row = [
            candidate_index.to_string(),
            filter.to_string(),
            modulator.to_string(),
            path_variant.to_string(),
            source_rate.to_string(),
            dsd_rate.to_string(),
            channel.to_string(),
            (window_index + 1).to_string(),
            window.start_bit.to_string(),
            window.len_bits.to_string(),
            fmt_csv_opt(start_s),
            fmt_csv_opt(end_s),
            fmt_csv_opt(worst_window_start_s),
            overlaps_worst.to_string(),
            window.total_commits.to_string(),
            window.near_tie_count.to_string(),
            window.ambiguity_override_count.to_string(),
            window.pressure_taper_count.to_string(),
            format!("{:.9}", window.pressure_mean),
            format!("{:.9}", window.pressure_max),
            window.pressure_ge_045.to_string(),
            window.pressure_ge_060.to_string(),
            window.pressure_ge_072.to_string(),
            window.pressure_ge_082.to_string(),
            format!("{:.9}", window.root_margin_mean),
            format!("{:.9}", window.root_margin_min),
            window.root_hot_raw_count.to_string(),
            window.chosen_plus.to_string(),
            window.chosen_minus.to_string(),
            window.transitions.to_string(),
            window.root_flip_count.to_string(),
            format!("{:.9}", window.quantizer_error_mean),
            format!("{:.9}", window.quantizer_error_max),
            format!("{:.9}", window.dc_bias_abs_mean),
            format!("{:.9}", window.dc_bias_abs_max),
            format!("{:.9}", window.dither_abs_mean),
            format!("{:.9}", window.dither_abs_max),
            format!("{:.9}", window.committed_state_pressure_mean),
            format!("{:.9}", window.committed_state_pressure_max),
            format!("{:.9}", window.committed_state_energy_mean),
            format!("{:.9}", window.committed_state_energy_max),
            format!("{:.9}", window.committed_state_stage0_max),
            format!("{:.9}", window.committed_state_stage1_max),
            format!("{:.9}", window.committed_state_stage2_max),
            format!("{:.9}", window.committed_state_stage3_max),
            format!("{:.9}", window.committed_state_stage4_max),
            format!("{:.9}", window.committed_state_stage5_max),
            format!("{:.9}", window.committed_state_stage6_max),
            window.nonfinite_best_score_count.to_string(),
            window.f_abs_gt_1_count.to_string(),
        ];
        csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
        csv.push('\n');
    }
}

fn crackle_decision_trace_csv(measurement: &Ec4aCrackleMeasurement) -> String {
    let mut csv = String::from(EC4A_DECISION_TRACE_CSV_HEADER);
    let Some(trace) = &measurement.adaptive_decision_trace else {
        return csv;
    };
    for (channel, snapshot) in [
        ("left", trace.left.as_ref()),
        ("right", trace.right.as_ref()),
    ] {
        let Some(snapshot) = snapshot else {
            continue;
        };
        push_ec4a_trace_rows(
            &mut csv,
            1,
            &measurement.filter,
            &measurement.modulator,
            "crackle_torture",
            measurement.source_rate,
            &measurement.dsd_rate,
            channel,
            snapshot,
        );
    }
    csv
}

// EC4A trace rows include shared CSV dimensions plus a trace snapshot.
#[allow(clippy::too_many_arguments)]
fn push_ec4a_trace_rows(
    csv: &mut String,
    candidate_index: usize,
    filter: &str,
    modulator: &str,
    path_variant: &str,
    source_rate: u32,
    dsd_rate: &str,
    channel: &str,
    snapshot: &AdaptiveDecisionTraceSnapshot,
) {
    for (window_index, window) in snapshot.windows.iter().enumerate() {
        let depth4_ratio = if window.total_commits == 0 {
            0.0
        } else {
            window.depth4_commits as f64 / window.total_commits as f64
        };
        let row = [
            candidate_index.to_string(),
            filter.to_string(),
            modulator.to_string(),
            path_variant.to_string(),
            source_rate.to_string(),
            dsd_rate.to_string(),
            channel.to_string(),
            (window_index + 1).to_string(),
            window.start_bit.to_string(),
            window.len_bits.to_string(),
            window.total_commits.to_string(),
            window.depth4_commits.to_string(),
            format!("{depth4_ratio:.9}"),
            window.trigger_none.to_string(),
            window.trigger_guard.to_string(),
            window.trigger_pressure.to_string(),
            window.trigger_transient.to_string(),
            window.trigger_ambiguity.to_string(),
            format!("{:.9}", window.pressure_mean),
            format!("{:.9}", window.pressure_max),
            window.pressure_ge_045.to_string(),
            window.pressure_ge_060.to_string(),
            window.pressure_ge_072.to_string(),
            window.pressure_ge_082.to_string(),
            format!("{:.9}", window.root_margin_mean),
            format!("{:.9}", window.root_margin_min),
            window.root_hot_raw_count.to_string(),
            window.guard_hot_count.to_string(),
            window.chosen_plus.to_string(),
            window.chosen_minus.to_string(),
            window.transitions.to_string(),
            window.root_flip_count.to_string(),
            window.depth4_root_flip_count.to_string(),
            window.depth4_shadow_depth2_same_root.to_string(),
            window.depth4_shadow_depth2_diff_root.to_string(),
            format!("{:.9}", window.depth4_shadow_score_delta_sum),
            format!("{:.9}", window.depth4_shadow_score_delta_min),
            format!("{:.9}", window.depth4_shadow_score_delta_max),
            window.nonfinite_best_score_count.to_string(),
            window.clean_policy_mismatch_count.to_string(),
            window.f_abs_gt_1_count.to_string(),
        ];
        csv.push_str(&row.into_iter().map(csv_cell).collect::<Vec<_>>().join(","));
        csv.push('\n');
    }
}

fn crackle_metrics_csv(m: &Ec4aCrackleMeasurement) -> String {
    let mut csv = String::from("kind,filter,modulator,source_rate,target_or_dsd,metric,value\n");
    let target = &m.dsd_rate;
    for (metric, value) in [
        ("seconds", m.seconds),
        ("decoded_frames", m.decoded_frames as f64),
        ("left_click_candidates", m.left_click_candidates as f64),
        ("right_click_candidates", m.right_click_candidates as f64),
        ("left_max_click_score", m.left_max_click_score),
        ("right_max_click_score", m.right_max_click_score),
        ("left_max_click_residual", m.left_max_click_residual),
        ("right_max_click_residual", m.right_max_click_residual),
        ("stability_resets", m.stability_resets as f64),
        ("state_clamps", m.state_clamps as f64),
        ("total_commits", m.total_commits as f64),
        ("depth4_commits", m.depth4_commits as f64),
        ("depth4_ratio", m.depth4_ratio),
        ("trigger_guard_selected", m.trigger_guard_selected as f64),
        (
            "trigger_pressure_selected",
            m.trigger_pressure_selected as f64,
        ),
        (
            "trigger_transient_selected",
            m.trigger_transient_selected as f64,
        ),
        (
            "trigger_ambiguity_selected",
            m.trigger_ambiguity_selected as f64,
        ),
        ("budget_starved", m.budget_starved as f64),
        ("max_hold_seen", m.max_hold_seen as f64),
    ] {
        push_metric(
            &mut csv,
            "ec4a_crackle",
            &m.filter,
            &m.modulator,
            m.source_rate,
            target,
            metric,
            Some(value),
        );
    }
    push_metric(
        &mut csv,
        "ec4a_crackle",
        &m.filter,
        &m.modulator,
        m.source_rate,
        target,
        "decoded_abs_peak",
        m.decoded_abs_peak,
    );
    csv
}

// Compact metric rows mirror the CSV dimensions used across the harness.
#[allow(clippy::too_many_arguments)]
fn push_metric(
    csv: &mut String,
    kind: &str,
    filter: &str,
    modulator: &str,
    source_rate: u32,
    target: &str,
    metric: &str,
    value: Option<f64>,
) {
    if let Some(value) = value {
        csv.push_str(&format!(
            "{kind},{filter},{modulator},{source_rate},{target},{metric},{value:.6}\n"
        ));
    }
}

fn roundtrip_waveform_svg(report: &DsdRoundtripReport) -> String {
    let rate = roundtrip_primary_rate(report);
    let points: Vec<_> = report
        .artifacts
        .waveform
        .iter()
        .filter(|point| {
            point.fixture == "program_multitone"
                && point.modulator == "EcDepth2"
                && point.dsd_rate == rate
                && point.channel == "left"
        })
        .collect();
    let reference = points
        .iter()
        .map(|point| (point.time_s, point.reference))
        .collect::<Vec<_>>();
    let measured = points
        .iter()
        .map(|point| (point.time_s, point.measured))
        .collect::<Vec<_>>();
    let residual = points
        .iter()
        .map(|point| (point.time_s, point.residual))
        .collect::<Vec<_>>();
    let title = format!("{rate} EC round-trip waveform");
    line_svg(
        &title,
        &[
            ("reference", "#0f172a", reference),
            ("measured", "#2563eb", measured),
            ("residual", "#dc2626", residual),
        ],
        None,
        Some((-0.55, 0.55)),
    )
}

fn roundtrip_spectrum_svg(report: &DsdRoundtripReport) -> String {
    let primary_rate = roundtrip_primary_rate(report);
    let comparison_rate = roundtrip_comparison_rate(report, &primary_rate);
    let reference = roundtrip_spectrum_series(
        &report.artifacts.spectrum,
        &primary_rate,
        "EcDepth2",
        "program_multitone",
        "left",
        |point| point.reference_dbfs,
    );
    let primary_measured = roundtrip_spectrum_series(
        &report.artifacts.spectrum,
        &primary_rate,
        "EcDepth2",
        "program_multitone",
        "left",
        |point| point.measured_dbfs,
    );
    let comparison_measured = comparison_rate
        .as_deref()
        .map(|rate| {
            roundtrip_spectrum_series(
                &report.artifacts.spectrum,
                rate,
                "EcDepth2",
                "program_multitone",
                "left",
                |point| point.measured_dbfs,
            )
        })
        .unwrap_or_default();
    line_svg(
        "Original vs DSD round-trip spectrum",
        &[
            ("reference", "#0f172a", reference),
            ("primary measured", "#2563eb", primary_measured),
            ("comparison measured", "#16a34a", comparison_measured),
        ],
        Some((0.0, 22_050.0)),
        Some((-180.0, 5.0)),
    )
}

fn roundtrip_residual_spectrum_svg(report: &DsdRoundtripReport) -> String {
    let primary_rate = roundtrip_primary_rate(report);
    let comparison_rate = roundtrip_comparison_rate(report, &primary_rate);
    let primary = roundtrip_spectrum_series(
        &report.artifacts.residual_spectrum,
        &primary_rate,
        "EcDepth2",
        "program_multitone",
        "left",
        |point| point.residual_dbfs,
    );
    let comparison = comparison_rate
        .as_deref()
        .map(|rate| {
            roundtrip_spectrum_series(
                &report.artifacts.residual_spectrum,
                rate,
                "EcDepth2",
                "program_multitone",
                "left",
                |point| point.residual_dbfs,
            )
        })
        .unwrap_or_default();
    line_svg(
        "DSD round-trip residual spectrum",
        &[
            ("primary residual", "#2563eb", primary),
            ("comparison residual", "#16a34a", comparison),
        ],
        Some((0.0, 22_050.0)),
        Some((-180.0, 0.0)),
    )
}

fn roundtrip_residual_spectrogram_svg(report: &DsdRoundtripReport) -> String {
    let rate = roundtrip_comparison_rate(report, &roundtrip_primary_rate(report))
        .unwrap_or_else(|| roundtrip_primary_rate(report));
    let mut points: Vec<_> = report
        .artifacts
        .spectrogram
        .iter()
        .filter(|point| {
            point.fixture == "program_multitone"
                && point.modulator == "EcDepth2"
                && point.dsd_rate == rate
                && point.channel == "left"
        })
        .collect();
    if points.is_empty() {
        points = report.artifacts.spectrogram.iter().collect();
    }
    let title = format!("{rate} EC residual spectrogram");
    heatmap_svg(&title, &points)
}

fn roundtrip_primary_rate(report: &DsdRoundtripReport) -> String {
    for preferred in ["DSD128", "DSD64", "DSD256"] {
        if report
            .measurements
            .iter()
            .any(|m| m.dsd_rate == preferred && m.modulator == "EcDepth2")
        {
            return preferred.to_string();
        }
    }
    report
        .measurements
        .iter()
        .find(|m| m.modulator == "EcDepth2")
        .map(|m| m.dsd_rate.clone())
        .unwrap_or_else(|| "DSD128".to_string())
}

fn roundtrip_comparison_rate(report: &DsdRoundtripReport, primary_rate: &str) -> Option<String> {
    for preferred in ["DSD256", "DSD128", "DSD64"] {
        if preferred != primary_rate
            && report
                .measurements
                .iter()
                .any(|m| m.dsd_rate == preferred && m.modulator == "EcDepth2")
        {
            return Some(preferred.to_string());
        }
    }
    None
}

fn roundtrip_spectrum_series(
    points: &[RoundtripSpectrumPoint],
    dsd_rate: &str,
    modulator: &str,
    fixture: &str,
    channel: &str,
    value: impl Fn(&RoundtripSpectrumPoint) -> Option<f64>,
) -> Vec<(f64, f64)> {
    points
        .iter()
        .filter(|point| {
            point.dsd_rate == dsd_rate
                && point.modulator == modulator
                && point.fixture == fixture
                && point.channel == channel
        })
        .filter_map(|point| value(point).map(|value| (point.freq_hz, value)))
        .collect()
}

fn pcm_svg(report: &SuiteReport) -> String {
    let values: Vec<_> = report
        .pcm
        .iter()
        .filter_map(|m| {
            m.image_rejection_db
                .map(|v| (format!("{} {}", m.filter, m.source_rate), v))
        })
        .collect();
    bar_svg("PCM image rejection (dB)", &values, 0.0, 160.0)
}

fn dsd_svg(report: &SuiteReport) -> String {
    let values: Vec<_> = report
        .dsd
        .iter()
        .filter_map(|m| {
            m.residual_db.map(|v| {
                (
                    format!("{} {} {}", m.modulator, m.dsd_rate, m.source_rate),
                    -v,
                )
            })
        })
        .collect();
    bar_svg("DSD residual depth (-dB)", &values, 0.0, 140.0)
}

#[allow(clippy::type_complexity)]
fn line_svg(
    title: &str,
    series: &[(&str, &str, Vec<(f64, f64)>)],
    x_range: Option<(f64, f64)>,
    y_range: Option<(f64, f64)>,
) -> String {
    let width = 960.0;
    let height = 420.0;
    let left = 64.0;
    let right = 24.0;
    let top = 58.0;
    let bottom = 46.0;
    let plot_w = width - left - right;
    let plot_h = height - top - bottom;
    let all_points = series
        .iter()
        .flat_map(|(_, _, points)| points.iter().copied())
        .filter(|(_, y)| y.is_finite())
        .collect::<Vec<_>>();
    let (x_min, x_max) = x_range.unwrap_or_else(|| {
        let min = all_points
            .iter()
            .map(|(x, _)| *x)
            .fold(f64::INFINITY, f64::min);
        let max = all_points
            .iter()
            .map(|(x, _)| *x)
            .fold(f64::NEG_INFINITY, f64::max);
        if min.is_finite() && max.is_finite() && max > min {
            (min, max)
        } else {
            (0.0, 1.0)
        }
    });
    let (y_min, y_max) = y_range.unwrap_or_else(|| {
        let min = all_points
            .iter()
            .map(|(_, y)| *y)
            .fold(f64::INFINITY, f64::min);
        let max = all_points
            .iter()
            .map(|(_, y)| *y)
            .fold(f64::NEG_INFINITY, f64::max);
        if min.is_finite() && max.is_finite() && max > min {
            (min, max)
        } else {
            (-1.0, 1.0)
        }
    });
    let sx = |x: f64| left + ((x - x_min) / (x_max - x_min)).clamp(0.0, 1.0) * plot_w;
    let sy = |y: f64| top + (1.0 - ((y - y_min) / (y_max - y_min)).clamp(0.0, 1.0)) * plot_h;
    let mut svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">
<rect width="100%" height="100%" fill="#f8fafc"/>
<text x="24" y="34" font-family="system-ui, sans-serif" font-size="20" fill="#0f172a">{}</text>
<rect x="{left}" y="{top}" width="{plot_w}" height="{plot_h}" fill="#ffffff" stroke="#cbd5e1"/>
"##,
        escape_xml(title)
    );
    for (idx, (label, color, _)) in series.iter().enumerate() {
        let x = left + idx as f64 * 170.0;
        svg.push_str(&format!(
            r##"<line x1="{x:.1}" y1="48" x2="{:.1}" y2="48" stroke="{color}" stroke-width="2"/>
<text x="{:.1}" y="52" font-family="system-ui, sans-serif" font-size="12" fill="#334155">{}</text>
"##,
            x + 22.0,
            x + 28.0,
            escape_xml(label)
        ));
    }
    for (_, color, points) in series {
        if points.is_empty() {
            continue;
        }
        let step = points.len().div_ceil(1600).max(1);
        let polyline = points
            .iter()
            .step_by(step)
            .filter(|(_, y)| y.is_finite())
            .map(|(x, y)| format!("{:.2},{:.2}", sx(*x), sy(*y)))
            .collect::<Vec<_>>()
            .join(" ");
        svg.push_str(&format!(
            r##"<polyline points="{polyline}" fill="none" stroke="{color}" stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round"/>
"##
        ));
    }
    svg.push_str(&format!(
        r##"<text x="{left}" y="{:.1}" font-family="system-ui, sans-serif" font-size="11" fill="#64748b">{:.2}</text>
<text x="{:.1}" y="{:.1}" font-family="system-ui, sans-serif" font-size="11" fill="#64748b">{:.2}</text>
<text x="24" y="{top}" font-family="system-ui, sans-serif" font-size="11" fill="#64748b">{:.1}</text>
<text x="24" y="{:.1}" font-family="system-ui, sans-serif" font-size="11" fill="#64748b">{:.1}</text>
</svg>
"##,
        height - 18.0,
        x_min,
        width - right - 54.0,
        height - 18.0,
        x_max,
        y_max,
        top + plot_h,
        y_min
    ));
    svg
}

fn heatmap_svg(title: &str, points: &[&RoundtripSpectrogramPoint]) -> String {
    let width = 960.0;
    let height = 460.0;
    let left = 64.0;
    let right = 24.0;
    let top = 58.0;
    let bottom = 48.0;
    let plot_w = width - left - right;
    let plot_h = height - top - bottom;
    let max_t = points
        .iter()
        .map(|point| point.start_s)
        .fold(0.0f64, f64::max)
        .max(1.0);
    let max_f = points
        .iter()
        .map(|point| point.freq_hz)
        .fold(0.0f64, f64::max)
        .max(20_000.0);
    let mut svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">
<rect width="100%" height="100%" fill="#f8fafc"/>
<text x="24" y="34" font-family="system-ui, sans-serif" font-size="20" fill="#0f172a">{}</text>
<rect x="{left}" y="{top}" width="{plot_w}" height="{plot_h}" fill="#0f172a"/>
"##,
        escape_xml(title)
    );
    for point in points.iter().step_by(points.len().div_ceil(12_000).max(1)) {
        let x = left + (point.start_s / max_t).clamp(0.0, 1.0) * plot_w;
        let y = top + (1.0 - (point.freq_hz / max_f).clamp(0.0, 1.0)) * plot_h;
        let intensity = ((point.residual_dbfs + 180.0) / 120.0).clamp(0.0, 1.0);
        let red = (40.0 + 215.0 * intensity) as u8;
        let green = (70.0 + 130.0 * (1.0 - (intensity - 0.35).abs()).max(0.0)) as u8;
        let blue = (130.0 + 80.0 * (1.0 - intensity)) as u8;
        svg.push_str(&format!(
            r##"<rect x="{x:.2}" y="{y:.2}" width="2.5" height="2.5" fill="#{red:02x}{green:02x}{blue:02x}"/>
"##
        ));
    }
    svg.push_str(&format!(
        r##"<text x="{left}" y="{:.1}" font-family="system-ui, sans-serif" font-size="11" fill="#64748b">0.00s</text>
<text x="{:.1}" y="{:.1}" font-family="system-ui, sans-serif" font-size="11" fill="#64748b">{:.2}s</text>
<text x="24" y="{top}" font-family="system-ui, sans-serif" font-size="11" fill="#64748b">{:.0}Hz</text>
<text x="24" y="{:.1}" font-family="system-ui, sans-serif" font-size="11" fill="#64748b">0Hz</text>
</svg>
"##,
        height - 18.0,
        width - right - 54.0,
        height - 18.0,
        max_t,
        max_f,
        top + plot_h
    ));
    svg
}

fn bar_svg(title: &str, values: &[(String, f64)], min: f64, max: f64) -> String {
    let width = 960.0;
    let row_h = 28.0;
    let height = 80.0 + row_h * values.len().max(1) as f64;
    let mut svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">
<rect width="100%" height="100%" fill="#f8fafc"/>
<text x="24" y="34" font-family="system-ui, sans-serif" font-size="20" fill="#0f172a">{title}</text>
"##
    );
    for (idx, (label, value)) in values.iter().enumerate() {
        let y = 64.0 + idx as f64 * row_h;
        let normalized = ((*value - min) / (max - min)).clamp(0.0, 1.0);
        let bar_w = normalized * 680.0;
        svg.push_str(&format!(
            r##"<text x="24" y="{:.1}" font-family="system-ui, sans-serif" font-size="12" fill="#334155">{}</text>
<rect x="210" y="{:.1}" width="{:.1}" height="16" rx="2" fill="#2563eb"/>
<text x="{:.1}" y="{:.1}" font-family="system-ui, sans-serif" font-size="12" fill="#0f172a">{:.2}</text>
"##,
            y + 13.0,
            escape_xml(label),
            y,
            bar_w,
            218.0 + bar_w,
            y + 13.0,
            value
        ));
    }
    svg.push_str("</svg>\n");
    svg
}

fn escape_xml(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn self_test_fft_bin_scaling() {
    let sample_rate = 48_000;
    let signal = sine(48_000, sample_rate, 1_000.0, 0.25);
    let amp = tone_amplitude(&signal, sample_rate, 1_000.0);
    assert!((amp - 0.25).abs() < 0.002, "amp={amp}");
}

pub fn self_test_alignment() {
    let reference = sine(4096, 48_000, 997.0, 0.5);
    let mut measured = vec![0.0; 17];
    measured.extend(reference.iter().map(|v| v * 0.75));
    let residual = aligned_residual_db(&reference, &measured).expect("alignment");
    assert!(residual < -250.0, "residual={residual}");
}

pub fn self_test_dsd_unpacking() {
    let bits = unpack_native_msb(&[0b1010_0001]);
    assert_eq!(bits, vec![1.0, -1.0, 1.0, -1.0, -1.0, -1.0, -1.0, 1.0]);
}

pub fn self_test_decimator() {
    let bits = vec![1.0, 1.0, -1.0, -1.0, 1.0, -1.0, 1.0, -1.0];
    let decoded = decimate_dsd_bits(&bits, 4);
    assert_eq!(decoded, vec![0.0, 0.0]);
}

pub fn self_test_crackle_detector() {
    let clean = sine(96_000, 48_000, 997.0, 0.5);
    let clean_stats = crackle_stats(&clean, 48_000);
    assert_eq!(clean_stats.candidates, 0, "clean sine flagged as click");

    let high = sine(96_000, 48_000, 19_000.0, 0.2);
    let high_stats = crackle_stats(&high, 48_000);
    assert_eq!(high_stats.candidates, 0, "clean HF sine flagged as click");

    let mut clicked = clean;
    clicked[48_000] += 0.8;
    let clicked_stats = crackle_stats(&clicked, 48_000);
    assert!(
        clicked_stats.candidates > 0,
        "injected click was not detected"
    );
}

pub fn self_test_dsd128_program_probe_catalog() {
    assert_eq!(
        DSD128_PROGRAM_PROBE_LABELS,
        [
            "sparse_piano",
            "female_vocal_reverb_tail",
            "brushed_cymbals",
            "low_level_acoustic_guitar",
            "dense_rock_chorus",
            "sub_bass_plus_treble_transient",
            "fade_in_fade_out",
        ]
    );

    let probes = dsd128_program_probes(8192, 44_100);
    let labels: Vec<_> = probes.iter().map(|probe| probe.label).collect();
    assert_eq!(labels, DSD128_PROGRAM_PROBE_LABELS);
    assert!(
        probes
            .iter()
            .all(|probe| probe.input.len() == 8192 && probe.input.iter().all(|v| v.is_finite())),
        "DSD128 program probes must be finite and frame-aligned"
    );

    let transition = dsd_transient_probes(8192, 44_100);
    assert!(
        transition
            .iter()
            .any(|probe| probe.label == "track_seek_pause_resume_new_track"),
        "transport transition probe must stay registered"
    );
}

pub fn self_test_quality_ranking_prefers_constrained_health() {
    let report = SuiteReport {
        mode: "ranking-self-test".to_string(),
        git_commit: None,
        pcm: Vec::new(),
        dsd: vec![
            synthetic_dsd_measurement("MedianHero", 170.0, 121.0, 40.0, 1.0),
            synthetic_dsd_measurement("Balanced", 160.0, 155.0, 2.5, 22.0),
        ],
    };
    let rankings = dsd_quality_rankings(&report);
    let best = rankings
        .iter()
        .find(|ranking| ranking.rank == 1)
        .expect("ranking should produce a winner");
    assert_eq!(report.dsd[best.dsd_index].modulator, "Balanced");
    assert_eq!(best.status, DsdRankingStatus::Pass);

    let median_hero = rankings
        .iter()
        .find(|ranking| report.dsd[ranking.dsd_index].modulator == "MedianHero")
        .expect("median-only candidate should be ranked");
    assert_eq!(median_hero.headline_snr_rank, Some(1));
    assert_eq!(median_hero.status, DsdRankingStatus::Reject);
}

fn synthetic_dsd_measurement(
    modulator: &str,
    median_snr: f64,
    worst_snr: f64,
    snr_spread: f64,
    spur_margin: f64,
) -> DsdMeasurement {
    synthetic_dsd_measurement_for_rate(
        "DSD128",
        5_644_800,
        modulator,
        median_snr,
        worst_snr,
        snr_spread,
        spur_margin,
        -172.0,
    )
}

#[allow(clippy::too_many_arguments)]
fn synthetic_dsd_measurement_for_rate(
    dsd_rate: &str,
    wire_rate: u32,
    modulator: &str,
    median_snr: f64,
    worst_snr: f64,
    snr_spread: f64,
    spur_margin: f64,
    worst_noise_dbfs: f64,
) -> DsdMeasurement {
    DsdMeasurement {
        modulator: modulator.to_string(),
        filter: "Split128k".to_string(),
        path_variant: "direct".to_string(),
        source_rate: 44_100,
        origin_source_rate: 44_100,
        renderer_source_rate: 44_100,
        intermediate_rate: None,
        intermediate_bits: None,
        intermediate_filter: None,
        path_prepare_ms: None,
        render_ms: None,
        dsd_rate: dsd_rate.to_string(),
        wire_rate: Some(wire_rate),
        passband_profile: PassbandProfile::default(),
        residual_db: Some(-150.0),
        thdn_residual_db: Some(-150.0),
        inband_snr_db: Some(median_snr),
        inband_snr_worst_db: Some(worst_snr),
        inband_snr_p05_db: Some(worst_snr),
        inband_snr_p95_db: Some(median_snr),
        inband_snr_best_db: Some(median_snr + 2.0),
        inband_snr_spread_db: Some(snr_spread),
        inband_snr_left_db: Some(median_snr),
        inband_snr_right_db: Some(median_snr - 0.5),
        inband_snr_left_worst_db: Some(worst_snr),
        inband_snr_right_worst_db: Some(worst_snr - 0.5),
        inband_snr_left_spread_db: Some(snr_spread),
        inband_snr_right_spread_db: Some(snr_spread + 0.5),
        inband_lf_sinad_worst_db: None,
        inband_lf_sinad_db: None,
        inband_lf_tone_hz: None,
        stereo_snr_worst_mismatch_db: Some(0.5),
        inband_snr_window_count: Some(8),
        inband_snr_worst_window_start_s: Some(0.0),
        inband_noise_rms_dbfs: Some(worst_noise_dbfs - 6.0),
        inband_noise_worst_rms_dbfs: Some(worst_noise_dbfs),
        inband_noise_peak_dbfs: Some(-135.0),
        inband_noise_peak_spur_hz: Some(1_000.0),
        inband_noise_spur_margin_db: Some(spur_margin),
        inband_noise_left_spur_margin_db: Some(spur_margin),
        inband_noise_right_spur_margin_db: Some(spur_margin - 0.5),
        inband_noise_20_200_dbfs: Some(worst_noise_dbfs - 8.0),
        inband_noise_200_2k_dbfs: Some(worst_noise_dbfs - 6.0),
        inband_noise_2k_8k_dbfs: Some(worst_noise_dbfs - 4.0),
        inband_noise_8k_16k_dbfs: Some(worst_noise_dbfs - 2.0),
        inband_noise_16k_20k_dbfs: Some(worst_noise_dbfs),
        ultrasonic_24_50k_max_dbfs: Some(-85.0),
        ultrasonic_24_50k_median_dbfs: Some(-88.0),
        ultrasonic_24_50k_window_spread_db: Some(3.0),
        ultrasonic_50_100k_max_dbfs: Some(-75.0),
        ultrasonic_50_100k_median_dbfs: Some(-78.0),
        ultrasonic_50_100k_window_spread_db: Some(3.0),
        ultrasonic_100_200k_max_dbfs: Some(-70.0),
        ultrasonic_100_200k_median_dbfs: Some(-73.0),
        ultrasonic_100_200k_window_spread_db: Some(3.0),
        inband_spurs: Vec::new(),
        inband_windows: Vec::new(),
        ultrasonic_windows: Vec::new(),
        premod_windows: Vec::new(),
        idle_tone_dbfs: Some(-92.0),
        idle_worst_tone_dbfs: Some(-90.0),
        idle_worst_density_deviation: Some(0.00001),
        idle_artifacts: Vec::new(),
        overload_recovery_diagnostics: Vec::new(),
        low_level_worst_residual_db: Some(-120.0),
        low_level_worst_spur_dbfs: Some(-80.0),
        high_freq_tone_worst_residual_db: Some(-10.0),
        high_freq_tone_worst_spur_dbfs: Some(-70.0),
        high_freq_imd_residual_db: Some(-45.0),
        high_freq_imd_spur_dbfs: Some(-70.0),
        high_freq_worst_residual_db: Some(-10.0),
        high_freq_worst_spur_dbfs: Some(-70.0),
        multitone_residual_db: Some(-55.0),
        multitone_spur_dbfs: Some(-70.0),
        overload_recovery_dbfs: Some(-80.0),
        transient_click_candidates: Some(0),
        transient_click_max_score: Some(0.0),
        transient_click_max_residual: Some(0.0),
        program_click_candidates: Some(0),
        program_click_max_score: Some(0.0),
        program_click_max_residual: Some(0.0),
        decoded_low: Some(-0.2),
        decoded_peak: Some(0.2),
        decoded_abs_peak: Some(0.2),
        bit_density: Some(0.5),
        bit_density_left: Some(0.5),
        bit_density_right: Some(0.5),
        bit_density_max_deviation: Some(0.0001),
        bit_density_left_max_deviation: Some(0.0001),
        bit_density_right_max_deviation: Some(0.0001),
        transition_rate: Some(0.5),
        limiter_peak_ratio_max: None,
        limiter_current_block_peak_ratio: None,
        limiter_current_block_gain: None,
        limiter_current_block_limited_samples: 0,
        limiter_limited_events: 0,
        limiter_limited_samples: 0,
        stability_resets: 0,
        state_clamps: 0,
        stress_stability_resets: 0,
        stress_state_clamps: 0,
        depth4_ratio: None,
        adaptive_decision_trace: None,
        ec2_decision_trace: None,
        dsd256_improvement_db: None,
        notes: Vec::new(),
    }
}
