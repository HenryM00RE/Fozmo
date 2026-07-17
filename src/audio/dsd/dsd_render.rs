//! End-to-end PCM → DoP renderer.
//!
//! Chains together:
//!   1. [`SincResampler`] — integer cascade pushing 44.1/48 kHz f64 PCM up to the DSD rate
//!      (2.8224 MHz for DSD64, 5.6448 MHz for DSD128, 11.2896 MHz for DSD256).
//!   2. [`CrfbModulator`] — one per channel, runs the upsampled f64 through a 7th-order
//!      delta-sigma loop and emits a 1-bit stream.
//!   3. [`DopPacker`] — repacks the bit streams into DoP frames (24-bit values with
//!      0x05/0xFA marker in the top 8 bits).
//!
//! Output is interleaved stereo `i32` at DSD_rate/16 — the same wire format that a
//! standard 24-bit/176.4 kHz (DSD64), 24-bit/352.8 kHz (DSD128) or 24-bit/705.6 kHz
//! (DSD256) PCM endpoint expects.
//!
//! The modulate stage is pipelined one block deep: each `modulate_*` call hands the
//! freshly upsampled block to two persistent per-channel worker threads and packs the
//! *previous* block's bits. Decode + upsample of block N+1 therefore overlaps
//! modulation of block N, so real-time throughput is bounded by the slowest stage
//! instead of the sum of stages. The held block is emitted by the end-of-stream
//! flush, which the engine already calls at EOF in both modulator modes.

// Staged DSD renderer paths compile before every transport enables them by default.
#![allow(dead_code)]

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(feature = "ecbeam2_observer")]
use crate::audio::dsd::delta_sigma::EcBeam2ObserverConfig;
use crate::audio::dsd::delta_sigma::{
    AdaptiveDecisionTraceSnapshot, BeamDiagnostics, BeamPeriodicityDiagnostics,
    BeamReconstructionDiagnostics, CrfbModulator, DitherPrng, DitherShape, DsdModulator,
    Ec2DecisionTraceSnapshot, Ec2LongFilterPolicy, Ec2PolicyWeights, EcBeam2Diagnostics,
    EcBeam2ExperimentConfig, EcBeam2Modulator, EcBeam2RunMode, EcFutureScorer, ModulatorMode,
    dc_bias_decay_for_corner_hz, ecbeam2_production_config,
};
use crate::audio::dsd::dop::DopPacker;
use crate::audio::dsd::dsd_coeffs::{
    CRFB_OSR64_OBG165, CRFB7_EC_OSR64, CRFB7_EC_OSR64_OBG144, CRFB7_EC_OSR128, CRFB7_EC_OSR256,
    CRFB7_STANDARD_OSR64, CRFB7_STANDARD_OSR128, CRFB7_STANDARD_OSR256, ModulatorCoeffs,
};
use crate::audio::dsd::native_dsd::{NativeDsdOrder, NativeDsdPacker};
use crate::audio::dsp::resampler::{FilterType, SincResampler};

const DSD_LIMITER_KNEE_RATIO: f64 = 0.95;
const DEFAULT_DSD_ISI_PENALTY: f64 = 0.0;
const DEFAULT_EC_BEAM_DC_BIAS_CORNER_HZ: f64 = 20.0;
/// Version identifier for the effective production-policy defaults applied by
/// [`DsdExperimentTweaks::with_production_policy_defaults`]. Measurement tools
/// record this separately from their own schema so policy-only changes cannot
/// masquerade as an equivalent renderer configuration.
pub const DSD_PRODUCTION_POLICY_VERSION: &str = "dsd-production-policy-v4";
pub const DSD64_EC_BEAM_A1_DEFAULT_EXPECTED_GAIN_DB: f64 = -14.8;
pub const DSD64_EC_BEAM_A1_DEFAULT_INPUT_GAIN_DB: f64 = -2.0;
pub const DSD64_ECBEAM2_REQUIRED_HEADROOM_DB: f64 = -2.0;
pub const DSD64_EC_BEAM_A1_DEFAULT_OBG: f64 = 1.65;
pub const DSD64_EC_BEAM_A1_PRESSURE_STAGE_WEIGHTS: [f64; 7] =
    [0.4375, 0.4375, 0.65625, 0.875, 1.09375, 1.53125, 1.96875];
/// Carries the sqrt(2) variance-parity compensation (dither shapes are
/// power-matched at equal scale); effective dither is unchanged from the
/// pre-normalization 0.30.
pub const DSD128_EC4A_DITHER_SCALE_MULTIPLIER: f64 = 0.30 * core::f64::consts::SQRT_2;

pub(crate) fn ecbeam2_filter_supported(filter_type: FilterType) -> bool {
    matches!(
        filter_type,
        FilterType::LinearPhase128k
            | FilterType::Minimum16k
            | FilterType::MinimumPhaseCompact128kV2
            | FilterType::Split128k
            | FilterType::SmoothPhase128k
    )
}

/// Map a source-PCM window onto the wire-rate sample domain emitted by the DSD
/// resampler. The FIR engines pre-pad their kernels to compensate group delay,
/// and EOF draining emits exactly `input_frames * ratio`, so source sample zero
/// is wire sequence zero. Quality tools use this single boundary for both
/// frozen-corpus diagnostics and EcBeam2 exact-oracle state prefixes.
pub fn dsd_source_window_to_modulator_samples(
    filter_type: FilterType,
    source_rate: u32,
    wire_rate: u32,
    source_start: usize,
    source_length: usize,
) -> Option<std::ops::Range<usize>> {
    if source_rate == 0
        || wire_rate == 0
        || !wire_rate.is_multiple_of(source_rate)
        || source_length == 0
    {
        return None;
    }
    let ratio = usize::try_from(wire_rate / source_rate).ok()?;
    // Retain the filter argument in this shared boundary so future filter
    // families with a different alignment contract cannot be added silently.
    if !matches!(
        filter_type,
        FilterType::LinearPhase128k
            | FilterType::Minimum16k
            | FilterType::Split128k
            | FilterType::IntegratedPhase128k
            | FilterType::IntegratedPhase128kV2
            | FilterType::IntegratedPhase128kV3
            | FilterType::IntegratedPhase128kV4
            | FilterType::MinimumPhase128k
            | FilterType::MinimumPhase128kV2
            | FilterType::MinimumPhase128kV3
            | FilterType::MinimumPhase128kV4
            | FilterType::MinimumPhaseCompact128k
            | FilterType::MinimumPhaseCompact128kV2
            | FilterType::SmoothPhase128k
            | FilterType::SincExtreme32k
    ) {
        return None;
    }
    let start = source_start.checked_mul(ratio)?;
    let length = source_length.checked_mul(ratio)?;
    Some(start..start.checked_add(length)?)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DsdAdaptiveTelemetry {
    pub total_commits: u64,
    pub depth4_commits: u64,
    pub trigger_guard_selected: u64,
    pub trigger_pressure_selected: u64,
    pub trigger_transient_selected: u64,
    pub trigger_ambiguity_selected: u64,
    pub budget_starved: u64,
    pub max_hold_seen: u32,
}

impl DsdAdaptiveTelemetry {
    pub fn depth4_ratio(self) -> f64 {
        if self.total_commits == 0 {
            0.0
        } else {
            self.depth4_commits as f64 / self.total_commits as f64
        }
    }
}
const DSD_MOD_SEED_LEFT: u64 = 0xA5A5_F00F_DEAD_BEEF;
const DSD_MOD_SEED_RIGHT: u64 = 0xE99B_C2D7_05F8_D3E1;

fn sanitize_isi_penalty(penalty: f64) -> f64 {
    if penalty.is_finite() {
        penalty.clamp(0.0, 0.05)
    } else {
        DEFAULT_DSD_ISI_PENALTY
    }
}

fn effective_modulator_input_gain(
    dsd_modulator: DsdModulator,
    input_gain: f64,
    experiment_gain_db: f64,
) -> f64 {
    let requested = input_gain * 10.0f64.powf(experiment_gain_db / 20.0);
    if dsd_modulator == DsdModulator::EcBeam2 {
        // Playback settings already supply -2 dB, so cap instead of multiplying
        // by a second headroom factor.  This also protects direct renderer
        // callers while preserving deliberately quieter input gain.
        requested.min(10.0f64.powf(DSD64_ECBEAM2_REQUIRED_HEADROOM_DB / 20.0))
    } else {
        requested
    }
}

/// Choice of DSD output rate. Selects both the cascade target rate and the modulator
/// coefficient table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdRate {
    Dsd64,
    Dsd128,
    Dsd256,
}

#[derive(Clone, Copy)]
struct NamedModulatorCoeffs {
    coeffs: &'static ModulatorCoeffs,
    name: &'static str,
}

#[derive(Clone, Copy)]
struct EcBeam2ProductionPolicy {
    config: EcBeam2ExperimentConfig,
    coefficients: NamedModulatorCoeffs,
}

fn ecbeam2_production_policy(dsd_rate: DsdRate) -> Option<EcBeam2ProductionPolicy> {
    let coefficients = match dsd_rate {
        DsdRate::Dsd64 => NamedModulatorCoeffs {
            coeffs: crate::audio::dsd::delta_sigma::ecbeam2_dsd64_production_coefficients(),
            name: "ECBEAM2_OSR64_OBG164_INPUT468_V1",
        },
        DsdRate::Dsd128 => NamedModulatorCoeffs {
            coeffs: crate::audio::dsd::delta_sigma::ecbeam2_dsd128_production_coefficients(),
            name: "ECBEAM2_OSR128_OBG164_INPUT468_V1",
        },
        DsdRate::Dsd256 => NamedModulatorCoeffs {
            coeffs: crate::audio::dsd::delta_sigma::ecbeam2_dsd256_production_coefficients(),
            name: "ECBEAM2_OSR256_OBG164_INPUT468_V1",
        },
    };
    Some(EcBeam2ProductionPolicy {
        config: ecbeam2_production_config(),
        coefficients,
    })
}

fn default_coeffs_for_mode(rate: DsdRate, mode: ModulatorMode) -> NamedModulatorCoeffs {
    match (rate, mode) {
        (DsdRate::Dsd64, ModulatorMode::Standard) => NamedModulatorCoeffs {
            coeffs: &CRFB7_STANDARD_OSR64,
            name: "CRFB7_STANDARD_OSR64",
        },
        (DsdRate::Dsd128, ModulatorMode::Standard) => NamedModulatorCoeffs {
            coeffs: &CRFB7_STANDARD_OSR128,
            name: "CRFB7_STANDARD_OSR128",
        },
        (DsdRate::Dsd256, ModulatorMode::Standard) => NamedModulatorCoeffs {
            coeffs: &CRFB7_STANDARD_OSR256,
            name: "CRFB7_STANDARD_OSR256",
        },
        (DsdRate::Dsd64, ModulatorMode::Ec) => NamedModulatorCoeffs {
            coeffs: &CRFB7_EC_OSR64,
            name: "CRFB7_EC_OSR64",
        },
        (DsdRate::Dsd128, ModulatorMode::Ec) => NamedModulatorCoeffs {
            coeffs: &CRFB7_EC_OSR128,
            name: "CRFB7_EC_OSR128",
        },
        (DsdRate::Dsd256, ModulatorMode::Ec) => NamedModulatorCoeffs {
            coeffs: &CRFB7_EC_OSR256,
            name: "CRFB7_EC_OSR256",
        },
    }
}

impl DsdRate {
    pub fn wire_rate_44k_family(self) -> u32 {
        match self {
            DsdRate::Dsd64 => 2_822_400,
            DsdRate::Dsd128 => 5_644_800,
            DsdRate::Dsd256 => 11_289_600,
        }
    }

    pub fn oversample(self) -> u32 {
        match self {
            DsdRate::Dsd64 => 64,
            DsdRate::Dsd128 => 128,
            DsdRate::Dsd256 => 256,
        }
    }

    pub fn coeffs_for_mode(self, mode: ModulatorMode) -> &'static ModulatorCoeffs {
        default_coeffs_for_mode(self, mode).coeffs
    }

    /// DSD wire rate for a given PCM source rate. DSD rates are *fixed* per family:
    ///
    /// * 44.1 kHz family (44.1 / 88.2 / 176.4 / 352.8 kHz) → 2.8224 MHz (DSD64),
    ///   5.6448 MHz (DSD128) or 11.2896 MHz (DSD256).
    /// * 48 kHz family (48 / 96 / 192 / 384 kHz) → 3.072 MHz (DSD64), 6.144 MHz
    ///   (DSD128) or 12.288 MHz (DSD256).
    ///
    /// Returns `None` if the source is in neither family or the implied upsample
    /// ratio isn't a power of two ≥ 2 (the cascade can't reach it). High-rate
    /// sources like 176.4 kHz that already exceed the DSD modulator's notional
    /// in-band coverage (the OSR=128/256 tables are tuned for ~22.05 kHz audio
    /// bandwidth) still work — the noise-shaping isn't optimal above ~22 kHz
    /// but doesn't actively break.
    pub fn wire_rate_for_source(self, source_rate: u32) -> Option<u32> {
        if source_rate == 0 {
            return None;
        }
        let base = if source_rate.is_multiple_of(44_100) {
            2_822_400
        } else if source_rate.is_multiple_of(48_000) {
            3_072_000
        } else {
            return None;
        };
        let target = match self {
            DsdRate::Dsd64 => base,
            DsdRate::Dsd128 => base * 2,
            DsdRate::Dsd256 => base * 4,
        };
        if target <= source_rate {
            return None;
        }
        if !target.is_multiple_of(source_rate) {
            return None;
        }
        let ratio = target / source_rate;
        if !ratio.is_power_of_two() {
            return None;
        }
        Some(target)
    }

    /// DoP frame rate (i.e. WASAPI exclusive PCM rate) for the given wire rate.
    pub fn dop_frame_rate(wire_rate: u32) -> u32 {
        wire_rate / 16
    }
}

pub struct DsdRenderer {
    upsampler: DsdUpsampler,
    worker_l: ModulatorWorker,
    worker_r: ModulatorWorker,
    /// A block has been handed to the workers and not yet collected.
    in_flight: bool,
    dop_packer: DopPacker,
    native_packer: NativeDsdPacker,
    /// Interleaved f64 PCM at the DSD rate, produced by the resampler each call.
    pcm_scratch: Vec<f64>,
    /// Source-rate scratch used only for NaN/Inf scrubbing before upsampling.
    source_scratch_l: Vec<f64>,
    source_scratch_r: Vec<f64>,
    /// Per-channel deinterleaved buffers (recycled through the worker channels).
    pcm_l: Vec<f64>,
    pcm_r: Vec<f64>,
    /// Per-channel 1-bit DSD output of the *previous* block, returned by the workers.
    bits_l: Vec<u8>,
    bits_r: Vec<u8>,
    /// Spare bit buffers cycled into the next worker job.
    spare_bits_l: Vec<u8>,
    spare_bits_r: Vec<u8>,
    /// Per-channel modulator health counters, refreshed each time a worker
    /// result is collected.
    stability_resets_lr: [u64; 2],
    state_clamps_lr: [u64; 2],
    ec2_decision_trace_lr: [Option<Ec2DecisionTraceSnapshot>; 2],
    beam_diagnostics_lr: [Option<BeamDiagnostics>; 2],
    beam_reconstruction_diagnostics_lr: [Option<BeamReconstructionDiagnostics>; 2],
    beam_periodicity_diagnostics_lr: [Option<BeamPeriodicityDiagnostics>; 2],
    ecbeam2_diagnostics_lr: [Option<EcBeam2Diagnostics>; 2],
    limiter_telemetry: DsdLimiterTelemetry,
    truncation_telemetry: DsdTruncationTelemetry,
    source_rate: u32,
    dsd_rate: DsdRate,
    coeffs: &'static ModulatorCoeffs,
    coefficient_table_name: &'static str,
    modulator_seeds: [u64; 2],
    modulator_mode: ModulatorMode,
    dsd_modulator: DsdModulator,
    isi_penalty: f64,
    experiment_tweaks: DsdExperimentTweaks,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DsdLimiterTelemetry {
    pub current_block_peak_ratio: f32,
    pub peak_ratio_max: f32,
    pub current_block_gain: f32,
    pub current_block_limited_samples: u64,
    pub limited_events: u64,
    pub limited_samples: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DsdTruncationTelemetry {
    pub events: u64,
    pub discarded_left_bits: u64,
    pub discarded_right_bits: u64,
    pub last_left_len: usize,
    pub last_right_len: usize,
    pub last_kept_len: usize,
}

impl Default for DsdLimiterTelemetry {
    fn default() -> Self {
        Self {
            current_block_peak_ratio: 0.0,
            peak_ratio_max: 0.0,
            current_block_gain: 1.0,
            current_block_limited_samples: 0,
            limited_events: 0,
            limited_samples: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DsdRenderTiming {
    pub upsample: Duration,
    pub modulate_submit_collect: Duration,
    pub pack: Duration,
    pub flush_modulators: Duration,
    pub flush_pack: Duration,
}

impl DsdRenderTiming {
    pub fn block_total(self) -> Duration {
        self.upsample + self.modulate_submit_collect + self.pack
    }

    pub fn flush_total(self) -> Duration {
        self.flush_modulators + self.flush_pack
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DsdCommonSideDither {
    pub beta: f64,
    pub common_seed: u64,
    pub side_seed: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DsdExperimentTweaks {
    /// Isolated EcBeam2 controls. `None` selects its versioned active defaults
    /// and never affects production EcBeam.
    pub ecbeam2_config: Option<EcBeam2ExperimentConfig>,
    /// `Some(true)` retains qualification telemetry, while `Some(false)` uses
    /// the bit-identical lean playback path. `None` resolves to full telemetry
    /// for explicit research configurations and lean telemetry for ordinary
    /// playback.
    pub ecbeam2_full_diagnostics: Option<bool>,
    pub ec_dither_scale_multiplier: Option<f64>,
    pub ec_dither_shape: Option<DitherShape>,
    pub ec_dither_prng: Option<DitherPrng>,
    pub ec_dither_leak_alpha: Option<f64>,
    pub ec_dither_lf_floor_gamma: Option<f64>,
    /// −3 dB corner (Hz) for the EC DC-bias tracker. `None` keeps the legacy
    /// rate-dependent decay (`EC_DC_BIAS_DECAY`) except in EcBeam, where the
    /// default is 20 Hz; see
    /// [`dc_bias_decay_for_corner_hz`].
    pub ec_dc_bias_corner_hz: Option<f64>,
    pub ec_common_side_dither: Option<DsdCommonSideDither>,
    /// Ambiguity-gated comparator dither: relative near-tie margin on the EC
    /// root scores and peak comparator perturbation. Both must be positive to
    /// take effect; `None` keeps the bit-identical default (no gated dither).
    pub ec_gated_dither_margin: Option<f64>,
    pub ec_gated_dither_scale: Option<f64>,
    pub ec_future_scorer: Option<EcFutureScorer>,
    pub ec2_long_filter_policy: Option<Ec2LongFilterPolicy>,
    pub ec2_policy_weights: Option<Ec2PolicyWeights>,
    /// Per-integrator (per-stage) EC-2 pressure weights. `None` keeps the
    /// uniform `1/7` scalar path; see
    /// [`CrfbModulator::set_pressure_stage_weights`] for normalization rules
    /// (weights are normalized to sum 1.0, so this knob only redistributes
    /// pressure and stays orthogonal to `pressure_weight`).
    pub ec2_pressure_stage_weights: Option<[f64; 7]>,
    pub ec2_decision_trace_window_bits: Option<usize>,
    pub ec4a_allow_predictive_triggers: Option<bool>,
    pub ec4a_dsd128_quality_pressure: bool,
    pub ec4a_dsd128_quality_pressure_threshold: Option<f64>,
    pub ec4a_dsd128_quality_pressure_hold: Option<u32>,
    pub ec4a_decision_trace_window_bits: Option<usize>,
    /// EcBeam M/N search geometry. Production ECB currently uses M4/N8; the
    /// explicit field remains available to the measurement harness.
    pub ec_beam_search: Option<(usize, usize)>,
    pub ec_beam_terminal_weight: Option<f64>,
    pub ec_beam_alternation_weight: Option<f64>,
    pub ec_beam_alternation_rank_weight: Option<f64>,
    pub ec_beam_alternation_threshold: Option<f64>,
    pub ec_beam_filtered_error_weight: Option<f64>,
    pub ec_beam_filtered_error_rank_weight: Option<f64>,
    pub ec_beam_reconstruction_error_weight: Option<f64>,
    pub ec_beam_pressure_deadzone: Option<f64>,
    pub ec_beam_periodicity_weight: Option<f64>,
    pub ec_beam_periodicity_lags: Option<[u8; 4]>,
    pub ec_beam_periodicity_lag_count: Option<usize>,
    pub ec_beam_periodicity_window: Option<usize>,
    pub ec_beam_pressure_accum_scale: Option<f64>,
    pub ec_beam_pressure_rank_scale: Option<f64>,
    pub ec_beam_dc_accum_scale: Option<f64>,
    pub ec_beam_dc_rank_scale: Option<f64>,
    pub ec_beam_metric_diagnostics: Option<bool>,
    pub seed_left: Option<u64>,
    pub seed_right: Option<u64>,
    pub input_gain_db: f64,
}

impl DsdExperimentTweaks {
    pub fn with_ec_beam_a1_defaults(mut self) -> Self {
        if self.ec_dither_scale_multiplier.is_none() {
            self.ec_dither_scale_multiplier = Some(0.0);
        }
        if self.ec_dither_shape.is_none() {
            self.ec_dither_shape = Some(DitherShape::HighPassTpdf);
        }
        if self.ec_dither_prng.is_none() {
            self.ec_dither_prng = Some(DitherPrng::SplitMix64);
        }
        if self.ec_dither_leak_alpha.is_none() {
            self.ec_dither_leak_alpha = Some(0.99);
        }
        if self.ec_future_scorer.is_none() {
            self.ec_future_scorer = Some(EcFutureScorer::QuantizerOnly);
        }
        if self.ec2_long_filter_policy.is_none() {
            self.ec2_long_filter_policy = Some(Ec2LongFilterPolicy::AmbiguityPressure);
        }
        if self.ec2_policy_weights.is_none() {
            self.ec2_policy_weights = Some(Ec2PolicyWeights {
                quantizer_weight: 0.8,
                pressure_weight: 2.75,
                limit_weight: 80.0,
                transition_weight: 0.002,
                dc_weight: 0.04,
                lookahead_discount: 0.8,
                ambiguity_margin: 0.0,
                pressure_taper_start: 0.60,
                pressure_taper_strength: 0.0,
            });
        }
        if self.ec2_pressure_stage_weights.is_none() {
            self.ec2_pressure_stage_weights = Some(DSD64_EC_BEAM_A1_PRESSURE_STAGE_WEIGHTS);
        }
        if self.ec_beam_terminal_weight.is_none() {
            self.ec_beam_terminal_weight = Some(0.3);
        }
        if self.ec_beam_alternation_weight.is_none() {
            self.ec_beam_alternation_weight = Some(0.0005);
        }
        self
    }

    pub fn with_dsd128_ec4a_policy_defaults(
        mut self,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
    ) -> Self {
        if dsd_rate == DsdRate::Dsd128 && dsd_modulator == DsdModulator::EcDepth4Adaptive {
            if self.ec4a_allow_predictive_triggers.is_none() {
                self.ec4a_allow_predictive_triggers = Some(false);
            }
            if self.ec_dither_scale_multiplier.is_none() {
                self.ec_dither_scale_multiplier = Some(DSD128_EC4A_DITHER_SCALE_MULTIPLIER);
            }
            if self.ec_dither_shape.is_none() {
                self.ec_dither_shape = Some(DitherShape::HighPassTpdf);
            }
        }
        self
    }

    pub fn with_production_policy_defaults(
        mut self,
        filter_type: FilterType,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
    ) -> Self {
        if dsd_modulator == DsdModulator::EcBeam {
            if self.ec_beam_search.is_none() {
                self.ec_beam_search = Some((4, 8));
            }
            return self.with_ec_beam_a1_defaults();
        }

        if dsd_modulator == DsdModulator::EcBeam2 {
            let explicit_research_config = self.ecbeam2_config.is_some();
            if self.ecbeam2_full_diagnostics.is_none() {
                self.ecbeam2_full_diagnostics = Some(explicit_research_config);
            }
            if self.ecbeam2_config.is_none() {
                self.ecbeam2_config =
                    ecbeam2_production_policy(dsd_rate).map(|policy| policy.config);
            }
            return self;
        }

        if filter_type.uses_long_filter_dsd_defaults()
            && dsd_rate == DsdRate::Dsd64
            && dsd_modulator == DsdModulator::EcDepth2
        {
            if self.ec_beam_search.is_some() {
                self = self.with_ec_beam_a1_defaults();
            }
            if self.ec_dither_scale_multiplier.is_none() {
                self.ec_dither_scale_multiplier = Some(0.0);
            }
            if self.ec_dither_shape.is_none() {
                self.ec_dither_shape = Some(DitherShape::HighPassTpdf);
            }
            if self.ec_dither_prng.is_none() {
                self.ec_dither_prng = Some(DitherPrng::SplitMix64);
            }
            if self.ec_dither_leak_alpha.is_none() {
                self.ec_dither_leak_alpha = Some(0.99);
            }
            if self.ec_future_scorer.is_none() {
                self.ec_future_scorer = Some(EcFutureScorer::QuarterPressureNoDcTransition);
            }
            if self.ec2_long_filter_policy.is_none() {
                self.ec2_long_filter_policy = Some(Ec2LongFilterPolicy::AmbiguityPressure);
            }
            if self.ec2_policy_weights.is_none() {
                self.ec2_policy_weights = Some(Ec2PolicyWeights {
                    quantizer_weight: 1.0,
                    // Promoted 1.5 -> 0.75 (2026-07-06): with the OBG 1.44
                    // table this measured +4.2/+4.35 dB worst SINAD vs the old
                    // production pair, runtime-neutral, zero clamps/resets.
                    pressure_weight: 0.75,
                    limit_weight: 80.0,
                    transition_weight: 0.0,
                    dc_weight: 0.04,
                    lookahead_discount: 0.8,
                    ambiguity_margin: 0.005,
                    pressure_taper_start: 0.45,
                    pressure_taper_strength: 2.0,
                });
            }
        }

        if filter_type.uses_long_filter_dsd_defaults()
            && dsd_rate == DsdRate::Dsd128
            && dsd_modulator == DsdModulator::EcDepth2
        {
            if self.ec_dither_scale_multiplier.is_none() {
                self.ec_dither_scale_multiplier = Some(0.0);
            }
            if self.ec_dither_shape.is_none() {
                self.ec_dither_shape = Some(DitherShape::HighPassTpdf);
            }
            if self.ec_dither_prng.is_none() {
                self.ec_dither_prng = Some(DitherPrng::SplitMix64);
            }
            if self.ec_future_scorer.is_none() {
                self.ec_future_scorer = Some(EcFutureScorer::QuantizerOnly);
            }
        }

        if filter_type.uses_split_family_dsd_defaults()
            && dsd_rate == DsdRate::Dsd128
            && dsd_modulator == DsdModulator::EcDepth2
        {
            if self.ec2_long_filter_policy.is_none() {
                self.ec2_long_filter_policy = Some(Ec2LongFilterPolicy::AmbiguityPressure);
            }
            if self.ec2_policy_weights.is_none() {
                self.ec2_policy_weights = Some(Ec2PolicyWeights {
                    quantizer_weight: 0.8,
                    pressure_weight: 1.5,
                    limit_weight: 80.0,
                    transition_weight: 0.002,
                    dc_weight: 0.04,
                    lookahead_discount: 0.6,
                    ambiguity_margin: 0.0,
                    pressure_taper_start: 0.60,
                    pressure_taper_strength: 0.0,
                });
            }
        }

        if filter_type.uses_long_filter_dsd_defaults()
            && dsd_rate == DsdRate::Dsd256
            && dsd_modulator == DsdModulator::EcDepth2
        {
            if self.ec_dither_scale_multiplier.is_none() {
                self.ec_dither_scale_multiplier = Some(0.0);
            }
            if self.ec_dither_shape.is_none() {
                self.ec_dither_shape = Some(DitherShape::HighPassTpdf);
            }
            if self.ec_dither_prng.is_none() {
                self.ec_dither_prng = Some(DitherPrng::SplitMix64);
            }
            if self.ec_future_scorer.is_none() {
                self.ec_future_scorer = Some(EcFutureScorer::QuantizerOnly);
            }
            if self.ec2_long_filter_policy.is_none() {
                self.ec2_long_filter_policy = Some(Ec2LongFilterPolicy::AmbiguityPressure);
            }
            if self.ec2_policy_weights.is_none() {
                self.ec2_policy_weights = Some(Ec2PolicyWeights {
                    quantizer_weight: 0.8,
                    pressure_weight: 1.5,
                    limit_weight: 80.0,
                    transition_weight: 0.002,
                    dc_weight: 0.04,
                    lookahead_discount: 0.6,
                    ambiguity_margin: 0.0,
                    pressure_taper_start: 0.60,
                    pressure_taper_strength: 0.0,
                });
            }
        }

        self
    }
}

fn ec_dc_bias_corner_hz_for_tweaks(tweaks: DsdExperimentTweaks) -> Option<f64> {
    tweaks.ec_dc_bias_corner_hz.or_else(|| {
        tweaks
            .ec_beam_search
            .map(|_| DEFAULT_EC_BEAM_DC_BIAS_CORNER_HZ)
    })
}

/// Production EC coefficient-table override, gated exactly like the tuned
/// policy block in [`DsdExperimentTweaks::with_production_policy_defaults`].
/// Plain DSD64 EcDepth2 keeps the 2026-07-06 OBG 1.44 promotion, while the
/// DSD64 EcBeam A1 uses the validated OBG 1.65 table. SincExtreme32k and the
/// higher-rate selectable ECB paths keep their rate's conservative EC table.
fn production_ec_coeffs_for(
    filter_type: FilterType,
    dsd_rate: DsdRate,
    dsd_modulator: DsdModulator,
    tweaks: DsdExperimentTweaks,
) -> Option<NamedModulatorCoeffs> {
    if dsd_modulator == DsdModulator::EcBeam {
        return (filter_type.uses_long_filter_dsd_defaults() && dsd_rate == DsdRate::Dsd64)
            .then_some(NamedModulatorCoeffs {
                coeffs: &CRFB_OSR64_OBG165,
                name: "CRFB_OSR64_OBG165",
            });
    }
    if !(filter_type.uses_long_filter_dsd_defaults()
        && dsd_rate == DsdRate::Dsd64
        && dsd_modulator == DsdModulator::EcDepth2)
    {
        return None;
    }
    if tweaks.ec_beam_search.is_some() {
        Some(NamedModulatorCoeffs {
            coeffs: &CRFB_OSR64_OBG165,
            name: "CRFB_OSR64_OBG165",
        })
    } else {
        Some(NamedModulatorCoeffs {
            coeffs: &CRFB7_EC_OSR64_OBG144,
            name: "CRFB7_EC_OSR64_OBG144",
        })
    }
}

fn select_modulator_coeffs(
    filter_type: FilterType,
    dsd_rate: DsdRate,
    dsd_modulator: DsdModulator,
    coeffs_override: Option<&'static ModulatorCoeffs>,
    experiment_tweaks: DsdExperimentTweaks,
) -> Result<NamedModulatorCoeffs, &'static str> {
    let modulator_mode = dsd_modulator.mode();
    if let Some(coeffs) = coeffs_override {
        if dsd_modulator == DsdModulator::EcBeam2 {
            return Err("EcBeam2 coefficient overrides are not supported");
        }
        if modulator_mode != ModulatorMode::Ec {
            return Err("DSD coefficient override is only supported for EC modulators");
        }
        if coeffs.osr != dsd_rate.oversample() {
            return Err("DSD coefficient override OSR does not match the selected DSD rate");
        }
        return Ok(NamedModulatorCoeffs {
            coeffs,
            name: "custom_override",
        });
    }
    if dsd_modulator == DsdModulator::EcBeam2 {
        if !ecbeam2_filter_supported(filter_type) {
            return Err("7th Order Search supports only the four selectable 128k filters");
        }
        return ecbeam2_production_policy(dsd_rate)
            .map(|policy| policy.coefficients)
            .ok_or("EcBeam2 has no production policy for the selected DSD rate");
    }
    Ok(
        production_ec_coeffs_for(filter_type, dsd_rate, dsd_modulator, experiment_tweaks)
            .unwrap_or_else(|| default_coeffs_for_mode(dsd_rate, modulator_mode)),
    )
}

enum ModJob {
    /// Modulate one block of gained, limited per-channel PCM into bits.
    Process { input: Vec<f64>, bits: Vec<u8> },
    /// Emit the EC lookahead tail (no-op bits in Standard mode).
    Flush { bits: Vec<u8> },
    /// Reset integrator state (keeps the dither RNG running). No response.
    Reset,
}

enum WorkerModulator {
    Crfb(Box<CrfbModulator>),
    EcBeam2(Box<EcBeam2Modulator>),
}

impl WorkerModulator {
    fn process_into_bits(&mut self, input: &[f64], bits: &mut Vec<u8>) {
        match self {
            Self::Crfb(modulator) => modulator.process_into_bits(input, bits),
            Self::EcBeam2(modulator) => modulator.process_into_bits(input, bits),
        }
    }

    fn flush_into_bits(&mut self, bits: &mut Vec<u8>) {
        match self {
            Self::Crfb(modulator) => modulator.flush_into_bits(bits),
            Self::EcBeam2(modulator) => modulator.flush_into_bits(bits),
        }
    }

    fn reset(&mut self) {
        match self {
            Self::Crfb(modulator) => modulator.reset(),
            Self::EcBeam2(modulator) => modulator.reset(),
        }
    }

    fn stability_resets(&self) -> u64 {
        match self {
            Self::Crfb(modulator) => modulator.stability_resets(),
            Self::EcBeam2(modulator) => modulator.stability_resets(),
        }
    }

    fn state_clamps(&self) -> u64 {
        match self {
            Self::Crfb(modulator) => modulator.state_clamps(),
            Self::EcBeam2(modulator) => modulator.state_clamps(),
        }
    }

    fn ec2_decision_trace(&self) -> Option<Ec2DecisionTraceSnapshot> {
        match self {
            Self::Crfb(modulator) => modulator.ec2_decision_trace(),
            Self::EcBeam2(_) => None,
        }
    }

    fn beam_diagnostics(&self) -> Option<BeamDiagnostics> {
        match self {
            Self::Crfb(modulator) => modulator.beam_diagnostics(),
            Self::EcBeam2(_) => None,
        }
    }

    fn beam_reconstruction_diagnostics(&self) -> Option<BeamReconstructionDiagnostics> {
        match self {
            Self::Crfb(modulator) => modulator.beam_reconstruction_diagnostics(),
            Self::EcBeam2(_) => None,
        }
    }

    fn beam_periodicity_diagnostics(&self) -> Option<BeamPeriodicityDiagnostics> {
        match self {
            Self::Crfb(modulator) => modulator.beam_periodicity_diagnostics(),
            Self::EcBeam2(_) => None,
        }
    }

    fn ecbeam2_diagnostics(&self) -> Option<EcBeam2Diagnostics> {
        match self {
            Self::Crfb(modulator) => {
                #[cfg(feature = "ecbeam2_observer")]
                {
                    modulator
                        .ecbeam2_observer_snapshot()
                        .map(|snapshot| EcBeam2Diagnostics {
                            committed_samples: snapshot
                                .delayed_commits
                                .wrapping_add(snapshot.flush_commits)
                                .wrapping_add(snapshot.recovery_commits),
                            positive_bits: snapshot.committed_positive_bits,
                            diagnostic_window_enabled: snapshot.diagnostic_window_enabled,
                            diagnostic_window_start_sequence: snapshot
                                .diagnostic_window_start_sequence,
                            diagnostic_window_end_sequence: snapshot.diagnostic_window_end_sequence,
                            diagnostic_window_samples: snapshot.diagnostic_window_samples,
                            diagnostic_window_positive_bits: snapshot
                                .diagnostic_window_positive_bits,
                            diagnostic_window_starting_tail_energy: snapshot
                                .diagnostic_window_starting_tail_energy,
                            diagnostic_window_remaining_tail_energy: snapshot
                                .diagnostic_window_remaining_tail_energy,
                            a1_frontier_events: snapshot.diagnostic_window_frontier_samples,
                            a1_best_child_disagreements: snapshot.best_child_disagreements,
                            a1_top_m_disagreements: snapshot.top_m_disagreements,
                            observer_desynchronizations: snapshot.desynchronizations,
                            all_nonfinite_resets: snapshot.recovery_commits,
                            invalid_input_substitutions: snapshot.invalid_input_substitutions,
                            a1_frontier_maximum_ultrasonic_ema: snapshot
                                .maximum_selected_ultrasonic_ema,
                            a1_frontier_maximum_signed_error_ema: snapshot
                                .maximum_selected_signed_error_ema,
                            a1_frontier_ultrasonic_ema_p999: snapshot.selected_ultrasonic_ema_p999,
                            a1_frontier_ultrasonic_ema_p9999: snapshot
                                .selected_ultrasonic_ema_p9999,
                            a1_frontier_signed_error_ema_abs_p999: snapshot
                                .selected_signed_error_ema_abs_p999,
                            a1_frontier_signed_error_ema_abs_p9999: snapshot
                                .selected_signed_error_ema_abs_p9999,
                            maximum_ultrasonic_ema: snapshot.maximum_committed_ultrasonic_ema,
                            maximum_signed_error_ema: snapshot.maximum_committed_signed_error_ema,
                            ultrasonic_ema_p999: snapshot.committed_ultrasonic_ema_p999,
                            ultrasonic_ema_p9999: snapshot.committed_ultrasonic_ema_p9999,
                            signed_error_ema_abs_p999: snapshot.committed_signed_error_ema_abs_p999,
                            signed_error_ema_abs_p9999: snapshot
                                .committed_signed_error_ema_abs_p9999,
                            committed_output_energy: snapshot
                                .committed_reconstruction_output_energy,
                            committed_tail_adjusted_energy: snapshot
                                .committed_reconstruction_tail_adjusted_energy,
                            remaining_tail_energy: snapshot.remaining_reconstruction_tail,
                            maximum_tail_energy: snapshot.maximum_reconstruction_tail,
                            committed_ultrasonic_energy: snapshot.committed_ultrasonic_energy,
                            maximum_ultrasonic_power: snapshot.maximum_ultrasonic_power,
                            maximum_reconstruction_1ms_energy: snapshot
                                .maximum_reconstruction_1ms_energy,
                            maximum_reconstruction_10ms_energy: snapshot
                                .maximum_reconstruction_10ms_energy,
                            maximum_abs_reconstruction_output: snapshot
                                .maximum_abs_reconstruction_output,
                            committed_sequence: snapshot
                                .delayed_commits
                                .wrapping_add(snapshot.flush_commits)
                                .wrapping_add(snapshot.recovery_commits),
                            committed_state_epoch: snapshot.epoch,
                            best_fourth_margin_last: snapshot.ecbeam2_best_fourth_margin_last,
                            minimum_best_fourth_margin: snapshot.ecbeam2_minimum_best_fourth_margin,
                            maximum_best_fourth_margin: snapshot.ecbeam2_maximum_best_fourth_margin,
                            best_fourth_margin_samples: snapshot.diagnostic_window_frontier_samples,
                            a1_best_fourth_margin_last: snapshot.production_best_fourth_margin_last,
                            a1_minimum_best_fourth_margin: snapshot
                                .production_minimum_best_fourth_margin,
                            a1_maximum_best_fourth_margin: snapshot
                                .production_maximum_best_fourth_margin,
                            a1_best_fourth_margin_samples: snapshot
                                .diagnostic_window_frontier_samples,
                            ..EcBeam2Diagnostics::default()
                        })
                }
                #[cfg(not(feature = "ecbeam2_observer"))]
                {
                    let _ = modulator;
                    None
                }
            }
            Self::EcBeam2(modulator) => Some(modulator.diagnostics()),
        }
    }
}

struct ModOutput {
    bits: Vec<u8>,
    /// The input buffer of a `Process` job, returned for recycling.
    input: Option<Vec<f64>>,
    stability_resets: u64,
    state_clamps: u64,
    ec2_decision_trace: Option<Ec2DecisionTraceSnapshot>,
    beam_diagnostics: Option<BeamDiagnostics>,
    beam_reconstruction_diagnostics: Option<BeamReconstructionDiagnostics>,
    beam_periodicity_diagnostics: Option<BeamPeriodicityDiagnostics>,
    ecbeam2_diagnostics: Option<EcBeam2Diagnostics>,
}

/// Persistent single-channel modulator thread. Owning the `CrfbModulator` on a
/// long-lived thread (rather than spawning per block) lets modulation of block N
/// overlap upsampling of block N+1 and keeps the integrator state warm in one
/// core's cache.
struct ModulatorWorker {
    jobs: mpsc::Sender<ModJob>,
    results: mpsc::Receiver<ModOutput>,
}

impl ModulatorWorker {
    // Worker startup passes the complete modulator configuration once; a builder would add noise here.
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        coeffs: &'static ModulatorCoeffs,
        seed: u64,
        dsd_modulator: DsdModulator,
        mode: ModulatorMode,
        lookahead_depth: usize,
        isi_penalty: f64,
        tweaks: DsdExperimentTweaks,
        common_side_sign: f64,
        wire_rate: u32,
        name: &str,
    ) -> Result<Self, &'static str> {
        let mut modulator = CrfbModulator::new_with_mode(coeffs, seed, mode)?;
        modulator.set_lookahead_depth(lookahead_depth);
        modulator.set_isi_penalty(isi_penalty);
        if mode == ModulatorMode::Ec {
            if let Some(multiplier) = tweaks.ec_dither_scale_multiplier {
                modulator.set_ec_dither_scale_multiplier(multiplier);
            }
            if let Some(shape) = tweaks.ec_dither_shape {
                modulator.set_dither_shape(shape);
            }
            if let Some(prng) = tweaks.ec_dither_prng {
                modulator.set_dither_prng(prng, seed);
            }
            if let Some(alpha) = tweaks.ec_dither_leak_alpha {
                modulator.set_high_pass_dither_leak_alpha(alpha);
            }
            if let Some(gamma) = tweaks.ec_dither_lf_floor_gamma {
                modulator.set_high_pass_dither_lf_floor_gamma(gamma);
            }
            if let Some(corner_hz) = ec_dc_bias_corner_hz_for_tweaks(tweaks) {
                modulator.set_dc_bias_decay(dc_bias_decay_for_corner_hz(corner_hz, wire_rate));
            }
            if let Some(common_side) = tweaks.ec_common_side_dither {
                modulator.set_common_side_dither(
                    common_side.common_seed,
                    common_side.side_seed,
                    common_side.beta,
                    common_side_sign,
                );
            }
            if let Some(scorer) = tweaks.ec_future_scorer {
                modulator.set_future_scorer(scorer);
            }
            if let Some(policy) = tweaks.ec2_long_filter_policy {
                modulator.set_ec2_long_filter_policy(policy);
            }
            if let Some(weights) = tweaks.ec2_policy_weights {
                modulator.set_ec2_policy_weights(weights);
            }
            if let Some(weights) = tweaks.ec2_pressure_stage_weights {
                modulator.set_pressure_stage_weights(&weights);
            }
            if let Some((m, n)) = tweaks.ec_beam_search {
                modulator.set_beam_search(m, n);
            }
            if let Some(weight) = tweaks.ec_beam_terminal_weight {
                modulator.set_beam_terminal_weight(weight);
            }
            if let Some(weight) = tweaks.ec_beam_alternation_weight {
                modulator.set_beam_alternation_weight(weight);
            }
            if let Some(weight) = tweaks.ec_beam_alternation_rank_weight {
                modulator.set_beam_alternation_rank_weight(weight);
            }
            if let Some(threshold) = tweaks.ec_beam_alternation_threshold {
                modulator.set_beam_alternation_threshold(threshold);
            }
            if let Some(weight) = tweaks.ec_beam_filtered_error_weight {
                modulator.set_beam_filtered_error_weight(weight);
            }
            if let Some(weight) = tweaks.ec_beam_filtered_error_rank_weight {
                modulator.set_beam_filtered_error_rank_weight(weight);
            }
            if let Some(weight) = tweaks.ec_beam_reconstruction_error_weight {
                modulator.set_beam_reconstruction_error_weight(weight);
            }
            if let Some(deadzone) = tweaks.ec_beam_pressure_deadzone {
                modulator.set_beam_pressure_deadzone(deadzone);
            }
            if let Some(weight) = tweaks.ec_beam_periodicity_weight {
                modulator.set_beam_periodicity_weight(weight);
            }
            if let (Some(lags), Some(count)) = (
                tweaks.ec_beam_periodicity_lags,
                tweaks.ec_beam_periodicity_lag_count,
            ) {
                modulator.set_beam_periodicity_lags(&lags[..count.min(lags.len())]);
            }
            if let Some(window) = tweaks.ec_beam_periodicity_window {
                modulator.set_beam_periodicity_window(window);
            }
            if tweaks.ec_beam_pressure_accum_scale.is_some()
                || tweaks.ec_beam_pressure_rank_scale.is_some()
                || tweaks.ec_beam_dc_accum_scale.is_some()
                || tweaks.ec_beam_dc_rank_scale.is_some()
            {
                modulator.set_beam_auxiliary_metric_scales(
                    tweaks.ec_beam_pressure_accum_scale.unwrap_or(0.0),
                    tweaks.ec_beam_pressure_rank_scale.unwrap_or(1.0),
                    tweaks.ec_beam_dc_accum_scale.unwrap_or(0.0),
                    tweaks.ec_beam_dc_rank_scale.unwrap_or(1.0),
                );
            }
            if let Some(enabled) = tweaks.ec_beam_metric_diagnostics {
                modulator.set_beam_metric_diagnostics_enabled(enabled);
            }
            if tweaks.ec_gated_dither_margin.is_some() || tweaks.ec_gated_dither_scale.is_some() {
                modulator.set_gated_dither(
                    tweaks.ec_gated_dither_margin.unwrap_or(0.0),
                    tweaks.ec_gated_dither_scale.unwrap_or(0.0),
                );
            }
            modulator.set_ec2_decision_trace_window_bits(tweaks.ec2_decision_trace_window_bits);
        }
        if let Some(config) = tweaks.ecbeam2_config {
            let config = config.validated()?;
            match (dsd_modulator, config.run_mode) {
                (DsdModulator::EcBeam, EcBeam2RunMode::ShadowA1) => {
                    #[cfg(feature = "ecbeam2_observer")]
                    modulator
                        .enable_ecbeam2_observer(EcBeam2ObserverConfig {
                            wire_rate,
                            capture_events: false,
                            event_capacity: 0,
                            diagnostic_window: config.diagnostic_window,
                        })
                        .map_err(|_| "EcBeam2 observer could not attach to production EcBeam")?;
                    #[cfg(not(feature = "ecbeam2_observer"))]
                    return Err(
                        "EcBeam2 ShadowA1 requires building with the ecbeam2_observer feature",
                    );
                }
                (DsdModulator::EcBeam2, EcBeam2RunMode::Active) => {}
                (DsdModulator::EcBeam2, EcBeam2RunMode::ShadowA1) => {
                    return Err("EcBeam2 ShadowA1 must observe production EcBeam");
                }
                (_, EcBeam2RunMode::Active) => {
                    return Err("active EcBeam2 controls require the EcBeam2 modulator");
                }
                (_, EcBeam2RunMode::ShadowA1) => {
                    return Err("EcBeam2 ShadowA1 requires the production EcBeam modulator");
                }
            }
        }
        let mut modulator = if dsd_modulator == DsdModulator::EcBeam2 {
            if isi_penalty != 0.0 {
                return Err("EcBeam2 requires zero ISI compensation");
            }
            WorkerModulator::EcBeam2(Box::new(EcBeam2Modulator::new_with_diagnostics(
                coeffs,
                seed,
                wire_rate,
                tweaks.ecbeam2_config.unwrap_or_default(),
                tweaks.ecbeam2_full_diagnostics.unwrap_or(false),
            )?))
        } else {
            WorkerModulator::Crfb(Box::new(modulator))
        };
        let (job_tx, job_rx) = mpsc::channel::<ModJob>();
        let (result_tx, result_rx) = mpsc::channel::<ModOutput>();
        let thread_name = name.to_string();
        let log_name = thread_name.clone();
        if crate::audio::debug::audio_debug_enabled() {
            eprintln!(
                "AudioWorker DEBUG: spawning DSD modulator worker name={} mode={:?} lookahead={} isi_penalty={:.5} coeff_osr={} coeff_obg={:.2} input_peak={:.6}",
                log_name,
                mode,
                lookahead_depth,
                isi_penalty,
                coeffs.osr,
                coeffs.obg,
                coeffs.input_peak,
            );
        }
        thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                promote_thread_to_audio_qos();
                if crate::audio::debug::audio_debug_enabled() {
                    eprintln!(
                        "AudioWorker DEBUG: DSD modulator worker online name={} mode={:?} lookahead={}",
                        log_name, mode, lookahead_depth
                    );
                }
                while let Ok(job) = job_rx.recv() {
                    let output = match job {
                        ModJob::Process { input, mut bits } => {
                            bits.clear();
                            modulator.process_into_bits(&input, &mut bits);
                            ModOutput {
                                bits,
                                input: Some(input),
                                stability_resets: modulator.stability_resets(),
                                state_clamps: modulator.state_clamps(),
                                ec2_decision_trace: modulator.ec2_decision_trace(),
                                beam_diagnostics: modulator.beam_diagnostics(),
                                beam_reconstruction_diagnostics: modulator
                                    .beam_reconstruction_diagnostics(),
                                beam_periodicity_diagnostics: modulator
                                    .beam_periodicity_diagnostics(),
                                ecbeam2_diagnostics: modulator.ecbeam2_diagnostics(),
                            }
                        }
                        ModJob::Flush { mut bits } => {
                            bits.clear();
                            modulator.flush_into_bits(&mut bits);
                            ModOutput {
                                bits,
                                input: None,
                                stability_resets: modulator.stability_resets(),
                                state_clamps: modulator.state_clamps(),
                                ec2_decision_trace: modulator.ec2_decision_trace(),
                                beam_diagnostics: modulator.beam_diagnostics(),
                                beam_reconstruction_diagnostics: modulator
                                    .beam_reconstruction_diagnostics(),
                                beam_periodicity_diagnostics: modulator
                                    .beam_periodicity_diagnostics(),
                                ecbeam2_diagnostics: modulator.ecbeam2_diagnostics(),
                            }
                        }
                        ModJob::Reset => {
                            modulator.reset();
                            continue;
                        }
                    };
                    if result_tx.send(output).is_err() {
                        break;
                    }
                }
            })
            .map_err(|_| "failed to spawn DSD modulator worker thread")?;
        Ok(Self {
            jobs: job_tx,
            results: result_rx,
        })
    }

    fn submit(&self, job: ModJob) {
        self.jobs
            .send(job)
            .expect("DSD modulator worker thread exited unexpectedly");
    }

    fn collect(&self) -> ModOutput {
        self.results
            .recv()
            .expect("DSD modulator worker thread exited unexpectedly")
    }
}

/// The modulator workers sit on the real-time audio path: ask the scheduler to
/// treat them accordingly so they aren't parked on efficiency cores or preempted
/// by background work. (macOS has no hard core pinning; QoS is the supported way
/// to keep a thread on performance cores.)
#[cfg(target_os = "macos")]
fn promote_thread_to_audio_qos() {
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
    }
}

#[cfg(not(target_os = "macos"))]
fn promote_thread_to_audio_qos() {}

// Both variants are stateful DSP pipelines; boxing would add indirection in the render loop.
#[allow(clippy::large_enum_variant)]
enum DsdUpsampler {
    Direct(SincResampler),
    CrossFamily(CrossFamilyDsdChain),
}

impl DsdUpsampler {
    fn new(filter_type: FilterType, source_rate: u32, target_rate: u32) -> Self {
        // Any 48k-family source forced onto a 44.1-family DSD wire rate
        // (DSD64, DSD128 or DSD256) needs the cross-family hop; a Direct
        // resampler would see a non-integer ratio and silently degrade to the
        // capped fractional polyphase path.
        let is_44k_dsd_target = target_rate == DsdRate::Dsd64.wire_rate_44k_family()
            || target_rate == DsdRate::Dsd128.wire_rate_44k_family()
            || target_rate == DsdRate::Dsd256.wire_rate_44k_family();
        if is_44k_dsd_target && matches!(source_rate, 48_000 | 96_000 | 192_000) {
            Self::CrossFamily(CrossFamilyDsdChain::new(
                filter_type,
                source_rate,
                target_rate,
            ))
        } else {
            Self::Direct(SincResampler::new(filter_type, source_rate, target_rate))
        }
    }

    fn target_rate(&self) -> u32 {
        match self {
            Self::Direct(resampler) => resampler.target_rate(),
            Self::CrossFamily(chain) => chain.target_rate(),
        }
    }

    fn debug_name(&self) -> &'static str {
        match self {
            Self::Direct(_) => "direct-integer-family",
            Self::CrossFamily(_) => "cross-family-48k-to-44k",
        }
    }

    fn input(&mut self, samples_l: &[f64], samples_r: &[f64]) {
        match self {
            Self::Direct(resampler) => resampler.input(samples_l, samples_r),
            Self::CrossFamily(chain) => chain.input(samples_l, samples_r),
        }
    }

    fn process(&mut self, output: &mut Vec<f64>) -> usize {
        match self {
            Self::Direct(resampler) => resampler.process(output),
            Self::CrossFamily(chain) => chain.process(output),
        }
    }

    fn drain_eof(&mut self, output: &mut Vec<f64>) -> usize {
        match self {
            Self::Direct(resampler) => resampler.drain_eof(output),
            Self::CrossFamily(chain) => chain.drain_eof(output),
        }
    }

    fn reset(&mut self) {
        match self {
            Self::Direct(resampler) => resampler.reset(),
            Self::CrossFamily(chain) => chain.reset(),
        }
    }
}

struct CrossFamilyDsdChain {
    stage1: Option<SincResampler>,
    stage2: SincResampler,
    stage3: SincResampler,
    stage1_out: Vec<f64>,
    stage2_out: Vec<f64>,
    plane_l: Vec<f64>,
    plane_r: Vec<f64>,
}

impl CrossFamilyDsdChain {
    const HOP_RATE_48K: u32 = 192_000;
    const HOP_RATE_44K: u32 = 176_400;

    fn new(filter_type: FilterType, source_rate: u32, target_rate: u32) -> Self {
        let stage1 = (source_rate != Self::HOP_RATE_48K)
            .then(|| SincResampler::new(filter_type, source_rate, Self::HOP_RATE_48K));
        Self {
            stage1,
            stage2: SincResampler::new_exact_160_147_without_capped_polyphase_warning(
                filter_type,
                Self::HOP_RATE_48K,
                Self::HOP_RATE_44K,
            ),
            stage3: SincResampler::new(filter_type, Self::HOP_RATE_44K, target_rate),
            stage1_out: Vec::new(),
            stage2_out: Vec::new(),
            plane_l: Vec::new(),
            plane_r: Vec::new(),
        }
    }

    fn target_rate(&self) -> u32 {
        self.stage3.target_rate()
    }

    fn input(&mut self, samples_l: &[f64], samples_r: &[f64]) {
        self.stage1_out.clear();
        if let Some(stage1) = &mut self.stage1 {
            stage1.input(samples_l, samples_r);
            stage1.process(&mut self.stage1_out);
        } else {
            interleave_stereo(samples_l, samples_r, &mut self.stage1_out);
        }

        if self.stage1_out.is_empty() {
            return;
        }
        feed_interleaved(
            &mut self.stage2,
            &self.stage1_out,
            &mut self.plane_l,
            &mut self.plane_r,
        );
    }

    fn process(&mut self, output: &mut Vec<f64>) -> usize {
        self.stage2_out.clear();
        self.stage2.process(&mut self.stage2_out);
        if !self.stage2_out.is_empty() {
            feed_interleaved(
                &mut self.stage3,
                &self.stage2_out,
                &mut self.plane_l,
                &mut self.plane_r,
            );
        }
        self.stage3.process(output)
    }

    fn drain_eof(&mut self, output: &mut Vec<f64>) -> usize {
        self.stage1_out.clear();
        if let Some(stage1) = &mut self.stage1 {
            stage1.drain_eof(&mut self.stage1_out);
            if !self.stage1_out.is_empty() {
                feed_interleaved(
                    &mut self.stage2,
                    &self.stage1_out,
                    &mut self.plane_l,
                    &mut self.plane_r,
                );
            }
        }

        self.stage2_out.clear();
        self.stage2.drain_eof(&mut self.stage2_out);
        if !self.stage2_out.is_empty() {
            feed_interleaved(
                &mut self.stage3,
                &self.stage2_out,
                &mut self.plane_l,
                &mut self.plane_r,
            );
        }

        self.stage3.drain_eof(output)
    }

    fn reset(&mut self) {
        if let Some(stage1) = &mut self.stage1 {
            stage1.reset();
        }
        self.stage2.reset();
        self.stage3.reset();
        self.stage1_out.clear();
        self.stage2_out.clear();
        self.plane_l.clear();
        self.plane_r.clear();
    }
}

impl DsdRenderer {
    /// `source_rate` is the decoded media rate (44100, 48000, etc.). Returns an
    /// error if the modulator coefficient tables haven't been calibrated yet
    /// (i.e. `tools/gen_crfb.py` has not been run).
    pub fn new(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
    ) -> Result<Self, &'static str> {
        Self::new_with_dsd_modulator(filter_type, source_rate, dsd_rate, DsdModulator::default())
    }

    pub fn new_with_dsd_modulator(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
    ) -> Result<Self, &'static str> {
        Self::new_with_dsd_modulator_and_isi_penalty(
            filter_type,
            source_rate,
            dsd_rate,
            dsd_modulator,
            DEFAULT_DSD_ISI_PENALTY,
        )
    }

    pub fn new_with_dsd_modulator_and_coeffs(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
        coeffs_override: Option<&'static ModulatorCoeffs>,
    ) -> Result<Self, &'static str> {
        Self::new_with_dsd_modulator_and_isi_penalty_and_coeffs(
            filter_type,
            source_rate,
            dsd_rate,
            dsd_modulator,
            DEFAULT_DSD_ISI_PENALTY,
            coeffs_override,
            DsdExperimentTweaks::default(),
        )
    }

    pub fn new_with_dsd_modulator_and_experiment_tweaks(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
        coeffs_override: Option<&'static ModulatorCoeffs>,
        experiment_tweaks: DsdExperimentTweaks,
    ) -> Result<Self, &'static str> {
        Self::new_with_dsd_modulator_and_isi_penalty_and_coeffs(
            filter_type,
            source_rate,
            dsd_rate,
            dsd_modulator,
            DEFAULT_DSD_ISI_PENALTY,
            coeffs_override,
            experiment_tweaks,
        )
    }

    pub fn new_with_dsd_modulator_and_isi_penalty(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
        isi_penalty: f64,
    ) -> Result<Self, &'static str> {
        Self::new_with_dsd_modulator_and_isi_penalty_and_coeffs(
            filter_type,
            source_rate,
            dsd_rate,
            dsd_modulator,
            isi_penalty,
            None,
            DsdExperimentTweaks::default(),
        )
    }

    pub fn new_with_dsd_modulator_and_isi_penalty_and_coeffs(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
        isi_penalty: f64,
        coeffs_override: Option<&'static ModulatorCoeffs>,
        experiment_tweaks: DsdExperimentTweaks,
    ) -> Result<Self, &'static str> {
        let target_rate = dsd_rate.wire_rate_for_source(source_rate).ok_or(
            "DSD output requires a 44.1 kHz- or 48 kHz-family source rate \
             (44.1/88.2/176.4 or 48/96/192 kHz)",
        )?;
        Self::new_with_wire_rate(
            filter_type,
            source_rate,
            dsd_rate,
            target_rate,
            dsd_modulator,
            isi_penalty,
            coeffs_override,
            experiment_tweaks,
        )
    }

    pub fn new_with_modulator_mode(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
        modulator_mode: ModulatorMode,
    ) -> Result<Self, &'static str> {
        Self::new_with_dsd_modulator(
            filter_type,
            source_rate,
            dsd_rate,
            DsdModulator::from_mode(modulator_mode),
        )
    }

    pub fn new_44k_family(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
    ) -> Result<Self, &'static str> {
        Self::new_44k_family_with_modulator_mode(
            filter_type,
            source_rate,
            dsd_rate,
            ModulatorMode::Ec,
        )
    }

    pub fn new_44k_family_with_modulator_mode(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
        modulator_mode: ModulatorMode,
    ) -> Result<Self, &'static str> {
        Self::new_44k_family_with_dsd_modulator(
            filter_type,
            source_rate,
            dsd_rate,
            DsdModulator::from_mode(modulator_mode),
        )
    }

    pub fn new_44k_family_with_dsd_modulator(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
    ) -> Result<Self, &'static str> {
        Self::new_44k_family_with_dsd_modulator_and_isi_penalty(
            filter_type,
            source_rate,
            dsd_rate,
            dsd_modulator,
            DEFAULT_DSD_ISI_PENALTY,
        )
    }

    pub fn new_44k_family_with_dsd_modulator_and_isi_penalty(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
        dsd_modulator: DsdModulator,
        isi_penalty: f64,
    ) -> Result<Self, &'static str> {
        if !is_44k_family(source_rate) && !is_48k_family(source_rate) {
            return Err(
                "DSD output requires a 44.1 kHz- or 48 kHz-family source rate \
                 (44.1/88.2/176.4 or 48/96/192 kHz)",
            );
        }
        if is_48k_family(source_rate) && !matches!(source_rate, 48_000 | 96_000 | 192_000) {
            return Err("44.1-family DSD forcing supports 48/96/192 kHz sources");
        }
        let target_rate = dsd_rate.wire_rate_44k_family();
        if target_rate <= source_rate {
            return Err("DSD output target rate must be above the source rate");
        }
        Self::new_with_wire_rate(
            filter_type,
            source_rate,
            dsd_rate,
            target_rate,
            dsd_modulator,
            isi_penalty,
            None,
            DsdExperimentTweaks::default(),
        )
    }

    // DSD construction keeps rate, modulator, and experiment inputs explicit at the mode boundary.
    #[allow(clippy::too_many_arguments)]
    fn new_with_wire_rate(
        filter_type: FilterType,
        source_rate: u32,
        dsd_rate: DsdRate,
        target_rate: u32,
        dsd_modulator: DsdModulator,
        isi_penalty: f64,
        coeffs_override: Option<&'static ModulatorCoeffs>,
        experiment_tweaks: DsdExperimentTweaks,
    ) -> Result<Self, &'static str> {
        if dsd_modulator == DsdModulator::EcBeam2 {
            if !ecbeam2_filter_supported(filter_type) {
                return Err("7th Order Search supports only the four selectable 128k filters");
            }
            // EcBeam2's production contract is stricter than the legacy
            // renderer sanitizer: reject negative and non-finite values too,
            // rather than silently normalizing either one to zero.
            if isi_penalty != 0.0 {
                return Err("EcBeam2 requires zero ISI compensation");
            }
        }
        let upsampler = DsdUpsampler::new(filter_type, source_rate, target_rate);
        let modulator_mode = dsd_modulator.mode();
        let lookahead_depth = dsd_modulator.lookahead_depth();
        let isi_penalty = sanitize_isi_penalty(isi_penalty);
        let experiment_tweaks =
            experiment_tweaks.with_production_policy_defaults(filter_type, dsd_rate, dsd_modulator);
        let coefficient_table = select_modulator_coeffs(
            filter_type,
            dsd_rate,
            dsd_modulator,
            coeffs_override,
            experiment_tweaks,
        )?;
        let coeffs = coefficient_table.coeffs;
        if crate::audio::debug::audio_debug_enabled() {
            eprintln!(
                "AudioWorker DEBUG: DSD renderer init: source={}Hz wire={}Hz dop_frame={}Hz rate={:?} filter={} upsampler={} modulator={} mode={:?} lookahead={} isi_penalty={:.5} coeff_osr={} coeff_obg={:.2} input_peak={:.6}",
                source_rate,
                upsampler.target_rate(),
                upsampler.target_rate() / 16,
                dsd_rate,
                filter_type.as_name(),
                upsampler.debug_name(),
                dsd_modulator.as_name(),
                modulator_mode,
                lookahead_depth,
                isi_penalty,
                coeffs.osr,
                coeffs.obg,
                coeffs.input_peak,
            );
        }
        // Use two distinct seeds so L and R dither streams are independent.
        let modulator_seeds = [
            experiment_tweaks.seed_left.unwrap_or(DSD_MOD_SEED_LEFT),
            experiment_tweaks.seed_right.unwrap_or(DSD_MOD_SEED_RIGHT),
        ];
        let worker_l = ModulatorWorker::spawn(
            coeffs,
            modulator_seeds[0],
            dsd_modulator,
            modulator_mode,
            lookahead_depth,
            isi_penalty,
            experiment_tweaks,
            1.0,
            upsampler.target_rate(),
            "dsd-mod-l",
        )?;
        let worker_r = ModulatorWorker::spawn(
            coeffs,
            modulator_seeds[1],
            dsd_modulator,
            modulator_mode,
            lookahead_depth,
            isi_penalty,
            experiment_tweaks,
            -1.0,
            upsampler.target_rate(),
            "dsd-mod-r",
        )?;
        Ok(Self {
            upsampler,
            worker_l,
            worker_r,
            in_flight: false,
            dop_packer: DopPacker::new(),
            native_packer: NativeDsdPacker::new(NativeDsdOrder::MsbFirst),
            pcm_scratch: Vec::new(),
            source_scratch_l: Vec::new(),
            source_scratch_r: Vec::new(),
            pcm_l: Vec::new(),
            pcm_r: Vec::new(),
            bits_l: Vec::new(),
            bits_r: Vec::new(),
            spare_bits_l: Vec::new(),
            spare_bits_r: Vec::new(),
            stability_resets_lr: [0; 2],
            state_clamps_lr: [0; 2],
            ec2_decision_trace_lr: [None, None],
            beam_diagnostics_lr: [None, None],
            beam_reconstruction_diagnostics_lr: [None, None],
            beam_periodicity_diagnostics_lr: [None, None],
            ecbeam2_diagnostics_lr: [None, None],
            limiter_telemetry: DsdLimiterTelemetry::default(),
            truncation_telemetry: DsdTruncationTelemetry::default(),
            source_rate,
            dsd_rate,
            coeffs,
            coefficient_table_name: coefficient_table.name,
            modulator_seeds,
            modulator_mode,
            dsd_modulator,
            isi_penalty,
            experiment_tweaks,
        })
    }

    pub fn source_rate(&self) -> u32 {
        self.source_rate
    }

    /// Full-scale PCM is mapped to this coefficient-table input before the
    /// one-bit loop. Measurement decoders divide their reconstructed output by
    /// the same declared gain to return to the post-headroom PCM domain.
    pub fn modulator_input_peak(&self) -> f64 {
        self.coeffs.input_peak
    }

    /// Stable identity of the effective coefficient table. Explicit coefficient
    /// overrides report `"custom_override"` instead of impersonating a built-in
    /// table with the same numeric contents.
    pub fn coefficient_table_name(&self) -> &'static str {
        self.coefficient_table_name
    }

    /// Nominal oversampling ratio for which the effective coefficient table was
    /// designed. This is not necessarily the wire/source-rate ratio for hi-res PCM.
    pub fn coefficient_osr(&self) -> u32 {
        self.coeffs.osr
    }

    /// Out-of-band gain of the effective coefficient table.
    pub fn coefficient_obg(&self) -> f64 {
        self.coeffs.obg
    }

    /// Effective policy values after production defaults have been applied.
    /// This is an inspection boundary for deterministic measurement reports.
    pub fn effective_experiment_tweaks(&self) -> DsdExperimentTweaks {
        self.experiment_tweaks
    }

    /// Initial per-channel seeds actually passed to the modulator workers after
    /// filling any omitted seed with the production default.
    pub fn effective_modulator_seeds(&self) -> [u64; 2] {
        self.modulator_seeds
    }

    /// Output sample rate as seen on the wire by a DoP-capable DAC.
    /// DSD64 → 176.4 kHz, DSD128 → 352.8 kHz, DSD256 → 705.6 kHz.
    pub fn dop_frame_rate(&self) -> u32 {
        self.upsampler.target_rate() / 16
    }

    pub fn reset(&mut self) {
        // Discard any block still in flight before resetting the modulators.
        self.collect_in_flight_into_bits();
        self.worker_l.submit(ModJob::Reset);
        self.worker_r.submit(ModJob::Reset);
        self.upsampler.reset();
        self.dop_packer.reset();
        self.native_packer.reset();
        self.pcm_scratch.clear();
        self.source_scratch_l.clear();
        self.source_scratch_r.clear();
        self.pcm_l.clear();
        self.pcm_r.clear();
        self.bits_l.clear();
        self.bits_r.clear();
        self.limiter_telemetry.current_block_peak_ratio = 0.0;
        self.limiter_telemetry.peak_ratio_max = 0.0;
        self.limiter_telemetry.current_block_gain = 1.0;
        self.limiter_telemetry.current_block_limited_samples = 0;
        self.ec2_decision_trace_lr = [None, None];
        self.beam_diagnostics_lr = [None, None];
        self.beam_reconstruction_diagnostics_lr = [None, None];
        self.beam_periodicity_diagnostics_lr = [None, None];
        self.ecbeam2_diagnostics_lr = [None, None];
        self.truncation_telemetry = DsdTruncationTelemetry::default();
    }

    /// Counters lag by one block: they're refreshed each time a worker result is
    /// collected, which is exactly when its bits become observable downstream.
    pub fn stability_resets(&self) -> u64 {
        self.stability_resets_lr[0] + self.stability_resets_lr[1]
    }

    pub fn state_clamps(&self) -> u64 {
        self.state_clamps_lr[0] + self.state_clamps_lr[1]
    }

    pub fn ec2_decision_traces(&self) -> [Option<Ec2DecisionTraceSnapshot>; 2] {
        self.ec2_decision_trace_lr.clone()
    }

    pub fn beam_diagnostics(&self) -> [Option<BeamDiagnostics>; 2] {
        self.beam_diagnostics_lr
    }

    pub fn beam_reconstruction_diagnostics(&self) -> [Option<BeamReconstructionDiagnostics>; 2] {
        self.beam_reconstruction_diagnostics_lr
    }

    pub fn beam_periodicity_diagnostics(&self) -> [Option<BeamPeriodicityDiagnostics>; 2] {
        self.beam_periodicity_diagnostics_lr
    }

    pub fn ecbeam2_diagnostics(&self) -> [Option<EcBeam2Diagnostics>; 2] {
        self.ecbeam2_diagnostics_lr
    }

    pub fn adaptive_decision_traces(&self) -> [Option<AdaptiveDecisionTraceSnapshot>; 2] {
        [None, None]
    }

    pub fn adaptive_telemetry(&self) -> DsdAdaptiveTelemetry {
        DsdAdaptiveTelemetry::default()
    }

    pub fn depth4_ratio(&self) -> f64 {
        0.0
    }

    pub fn limiter_telemetry(&self) -> DsdLimiterTelemetry {
        self.limiter_telemetry
    }

    pub fn truncation_telemetry(&self) -> DsdTruncationTelemetry {
        self.truncation_telemetry
    }

    /// Fold a collected worker result back into the renderer's buffer pools and
    /// health counters without touching `bits_l`/`bits_r`.
    fn recycle_collected(&mut self, output: ModOutput, left: bool) {
        let channel = if left { 0 } else { 1 };
        self.stability_resets_lr[channel] = output.stability_resets;
        self.state_clamps_lr[channel] = output.state_clamps;
        self.ec2_decision_trace_lr[channel] = output.ec2_decision_trace;
        self.beam_diagnostics_lr[channel] = output.beam_diagnostics;
        self.beam_reconstruction_diagnostics_lr[channel] = output.beam_reconstruction_diagnostics;
        self.beam_periodicity_diagnostics_lr[channel] = output.beam_periodicity_diagnostics;
        self.ecbeam2_diagnostics_lr[channel] = output.ecbeam2_diagnostics;
        if left {
            self.spare_bits_l = output.bits;
        } else {
            self.spare_bits_r = output.bits;
        }
        if let Some(input) = output.input {
            if left {
                self.pcm_l = input;
            } else {
                self.pcm_r = input;
            }
        }
    }

    pub fn modulator_mode(&self) -> ModulatorMode {
        self.modulator_mode
    }

    pub fn dsd_modulator(&self) -> DsdModulator {
        self.dsd_modulator
    }

    pub fn isi_penalty(&self) -> f64 {
        self.isi_penalty
    }

    /// Stage 1: upsample decoded PCM up to the DSD rate.
    ///
    /// Returns a mutable view of an interleaved-stereo f64 buffer the caller can
    /// modify in place — e.g. to apply EQ or pre-modulator volume — before the
    /// modulate step. The buffer is owned by the renderer and will be reused on
    /// the next call.
    pub fn upsample(&mut self, samples_l: &[f64], samples_r: &[f64]) -> &mut Vec<f64> {
        if source_needs_sanitize(samples_l) || source_needs_sanitize(samples_r) {
            fill_sanitized_source_scratch(&mut self.source_scratch_l, samples_l);
            fill_sanitized_source_scratch(&mut self.source_scratch_r, samples_r);
            self.upsampler
                .input(&self.source_scratch_l, &self.source_scratch_r);
        } else {
            self.upsampler.input(samples_l, samples_r);
        }
        self.pcm_scratch.clear();
        self.upsampler.process(&mut self.pcm_scratch);
        &mut self.pcm_scratch
    }

    pub fn drain_resampler_eof(&mut self) -> &mut Vec<f64> {
        self.pcm_scratch.clear();
        self.upsampler.drain_eof(&mut self.pcm_scratch);
        &mut self.pcm_scratch
    }

    /// Materialize the current upsampler block in the exact scalar domain seen
    /// by EcBeam2 (`u`): post coefficient-table gain, mandatory headroom, block
    /// rider, and soft limiter. This is a quality-tool inspection boundary; it
    /// neither submits work to the modulators nor changes renderer telemetry.
    ///
    /// Keeping this helper on the renderer makes exact-oracle tooling share the
    /// production normalization implementation instead of approximating it in
    /// a second command-line program.
    #[doc(hidden)]
    pub fn ecbeam2_oracle_modulator_input_block(
        &self,
        input_gain: f64,
    ) -> Result<(Vec<f64>, Vec<f64>), &'static str> {
        if self.dsd_modulator != DsdModulator::EcBeam2 || self.dsd_rate != DsdRate::Dsd64 {
            return Err("EcBeam2 oracle input inspection requires the DSD64 EcBeam2 renderer");
        }
        let mut left = Vec::new();
        let mut right = Vec::new();
        prepare_modulator_input_planes(
            &self.pcm_scratch,
            self.coeffs,
            self.dsd_modulator,
            self.experiment_tweaks,
            input_gain,
            &mut left,
            &mut right,
        );
        Ok((left, right))
    }

    /// Stage 2 (pipelined): hand the upsampled buffer produced by [`upsample`] to
    /// the modulator workers and surface the *previous* block's bits for packing.
    /// Output therefore lags input by one block; the end-of-stream flush emits the
    /// held block.
    ///
    /// `input_gain` is multiplied after mapping PCM full scale to the selected
    /// coefficient table's measured modulator input peak.
    /// Use this to apply user volume (and any EQ-related makeup gain) — DoP bytes
    /// cannot be scaled downstream without scrambling the 0x05/0xFA markers.
    fn modulate(&mut self, input_gain: f64) -> bool {
        let frames = self.pcm_scratch.len() / 2;
        if frames == 0 {
            // Preserve any already-collected block until the next non-empty block
            // or EOF flush. Collecting here would make an empty upsample call
            // unexpectedly surface delayed DSD bits.
            return false;
        }
        // Collect the previous block first so its input planes are free for reuse.
        self.collect_in_flight_into_bits();
        let has_packable_bits = self.truncate_current_bits_to_equal_len();

        let prepared = prepare_modulator_input_planes(
            &self.pcm_scratch,
            self.coeffs,
            self.dsd_modulator,
            self.experiment_tweaks,
            input_gain,
            &mut self.pcm_l,
            &mut self.pcm_r,
        );
        self.record_limiter_block(
            prepared.block_peak,
            prepared.headroom_gain,
            prepared.block_limited_samples,
        );

        self.worker_l.submit(ModJob::Process {
            input: std::mem::take(&mut self.pcm_l),
            bits: std::mem::take(&mut self.spare_bits_l),
        });
        self.worker_r.submit(ModJob::Process {
            input: std::mem::take(&mut self.pcm_r),
            bits: std::mem::take(&mut self.spare_bits_r),
        });
        self.in_flight = true;

        has_packable_bits
    }

    fn record_limiter_block(
        &mut self,
        block_peak: f64,
        block_gain: f64,
        block_limited_samples: u64,
    ) {
        record_limiter_telemetry(
            &mut self.limiter_telemetry,
            self.coeffs.input_peak,
            block_peak,
            block_gain,
            block_limited_samples,
        );
    }

    pub fn modulate_and_pack(&mut self, input_gain: f64, out: &mut Vec<i32>) {
        if self.modulate(input_gain) {
            self.dop_packer.push_stream(&self.bits_l, &self.bits_r, out);
        }
    }

    pub fn render_profiled(
        &mut self,
        samples_l: &[f64],
        samples_r: &[f64],
        input_gain: f64,
        out: &mut Vec<i32>,
    ) -> DsdRenderTiming {
        let mut timing = DsdRenderTiming::default();

        let start = Instant::now();
        self.upsample(samples_l, samples_r);
        timing.upsample = start.elapsed();

        let start = Instant::now();
        let has_packable_bits = self.modulate(input_gain);
        timing.modulate_submit_collect = start.elapsed();

        if has_packable_bits {
            let start = Instant::now();
            self.dop_packer.push_stream(&self.bits_l, &self.bits_r, out);
            timing.pack = start.elapsed();
        }

        timing
    }

    pub fn set_native_order(&mut self, order: NativeDsdOrder) {
        self.native_packer.set_order(order);
    }

    pub fn modulate_and_pack_native(
        &mut self,
        input_gain: f64,
        out_l: &mut Vec<u8>,
        out_r: &mut Vec<u8>,
    ) {
        if self.modulate(input_gain) {
            self.native_packer
                .push_stream(&self.bits_l, &self.bits_r, out_l, out_r);
        }
    }

    /// End-of-stream flush: emit the EC modulators' held lookahead tail through the
    /// DoP packer. No-op in Standard mode (the modulators hold no latency). Call once
    /// at track end, after the final [`modulate_and_pack`](Self::modulate_and_pack).
    pub fn flush_modulators_and_pack(&mut self, out: &mut Vec<i32>) {
        if self.flush_modulators() {
            self.dop_packer.push_stream(&self.bits_l, &self.bits_r, out);
        }
    }

    pub fn flush_modulators_and_pack_profiled(&mut self, out: &mut Vec<i32>) -> DsdRenderTiming {
        let mut timing = DsdRenderTiming::default();

        let start = Instant::now();
        let has_packable_bits = self.flush_modulators();
        timing.flush_modulators = start.elapsed();

        if has_packable_bits {
            let start = Instant::now();
            self.dop_packer.push_stream(&self.bits_l, &self.bits_r, out);
            timing.flush_pack = start.elapsed();
        }

        timing
    }

    /// Native-DSD counterpart of [`flush_modulators_and_pack`](Self::flush_modulators_and_pack).
    /// Call before [`flush_native_with_idle`](Self::flush_native_with_idle) so the tail
    /// bits land ahead of the idle padding.
    pub fn flush_modulators_and_pack_native(&mut self, out_l: &mut Vec<u8>, out_r: &mut Vec<u8>) {
        if self.flush_modulators() {
            self.native_packer
                .push_stream(&self.bits_l, &self.bits_r, out_l, out_r);
        }
    }

    /// Pull the in-flight block's bits into `bits_l`/`bits_r` (clearing them if
    /// nothing is in flight) and recycle the freed buffers. Leaves `in_flight` false.
    fn collect_in_flight_into_bits(&mut self) {
        if !self.in_flight {
            self.bits_l.clear();
            self.bits_r.clear();
            return;
        }
        let out_l = self.worker_l.collect();
        let out_r = self.worker_r.collect();
        let prev_bits_l = std::mem::replace(&mut self.bits_l, out_l.bits);
        let prev_bits_r = std::mem::replace(&mut self.bits_r, out_r.bits);
        self.recycle_collected(
            ModOutput {
                bits: prev_bits_l,
                input: out_l.input,
                stability_resets: out_l.stability_resets,
                state_clamps: out_l.state_clamps,
                ec2_decision_trace: out_l.ec2_decision_trace,
                beam_diagnostics: out_l.beam_diagnostics,
                beam_reconstruction_diagnostics: out_l.beam_reconstruction_diagnostics,
                beam_periodicity_diagnostics: out_l.beam_periodicity_diagnostics,
                ecbeam2_diagnostics: out_l.ecbeam2_diagnostics,
            },
            true,
        );
        self.recycle_collected(
            ModOutput {
                bits: prev_bits_r,
                input: out_r.input,
                stability_resets: out_r.stability_resets,
                state_clamps: out_r.state_clamps,
                ec2_decision_trace: out_r.ec2_decision_trace,
                beam_diagnostics: out_r.beam_diagnostics,
                beam_reconstruction_diagnostics: out_r.beam_reconstruction_diagnostics,
                beam_periodicity_diagnostics: out_r.beam_periodicity_diagnostics,
                ecbeam2_diagnostics: out_r.ecbeam2_diagnostics,
            },
            false,
        );
        self.in_flight = false;
    }

    fn flush_modulators(&mut self) -> bool {
        // First reel in the pipelined block still held by the workers…
        self.collect_in_flight_into_bits();

        // …then append the EC lookahead tail (empty in Standard mode).
        self.worker_l.submit(ModJob::Flush {
            bits: std::mem::take(&mut self.spare_bits_l),
        });
        self.worker_r.submit(ModJob::Flush {
            bits: std::mem::take(&mut self.spare_bits_r),
        });
        let tail_l = self.worker_l.collect();
        let tail_r = self.worker_r.collect();
        self.bits_l.extend_from_slice(&tail_l.bits);
        self.bits_r.extend_from_slice(&tail_r.bits);
        self.recycle_collected(tail_l, true);
        self.recycle_collected(tail_r, false);

        self.truncate_current_bits_to_equal_len()
    }

    fn truncate_current_bits_to_equal_len(&mut self) -> bool {
        // Both channels normally see the same frame count. If a worker ever returns
        // divergent lengths, keep the DoP/native packers from seeing an L/R desync.
        let left_len = self.bits_l.len();
        let right_len = self.bits_r.len();
        let len = self.bits_l.len().min(self.bits_r.len());
        if left_len != right_len {
            self.truncation_telemetry.events = self.truncation_telemetry.events.wrapping_add(1);
            self.truncation_telemetry.discarded_left_bits = self
                .truncation_telemetry
                .discarded_left_bits
                .wrapping_add(left_len.saturating_sub(len) as u64);
            self.truncation_telemetry.discarded_right_bits = self
                .truncation_telemetry
                .discarded_right_bits
                .wrapping_add(right_len.saturating_sub(len) as u64);
            self.truncation_telemetry.last_left_len = left_len;
            self.truncation_telemetry.last_right_len = right_len;
            self.truncation_telemetry.last_kept_len = len;
        }
        self.bits_l.truncate(len);
        self.bits_r.truncate(len);
        len > 0
    }

    pub fn flush_native_with_idle(&mut self, out_l: &mut Vec<u8>, out_r: &mut Vec<u8>) {
        self.native_packer.flush_with_idle(out_l, out_r);
    }

    /// Convenience wrapper: upsample + modulate + pack with no in-band processing
    /// and unity volume. Kept for tests and callers that don't need EQ/volume.
    pub fn render(&mut self, samples_l: &[f64], samples_r: &[f64], out: &mut Vec<i32>) {
        self.upsample(samples_l, samples_r);
        self.modulate_and_pack(1.0, out);
    }
}

fn is_44k_family(source_rate: u32) -> bool {
    source_rate != 0 && source_rate.is_multiple_of(44_100)
}

fn is_48k_family(source_rate: u32) -> bool {
    source_rate != 0 && source_rate.is_multiple_of(48_000)
}

fn limit_modulator_input(sample: f64, input_peak: f64) -> f64 {
    ModulatorInputLimiter::new(input_peak).limit(sample)
}

fn block_headroom_gain(block_peak: f64, target_peak: f64) -> f64 {
    if block_peak.is_finite()
        && target_peak.is_finite()
        && block_peak > target_peak
        && target_peak > 0.0
    {
        target_peak / block_peak
    } else {
        1.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct PreparedModulatorInput {
    block_peak: f64,
    headroom_gain: f64,
    block_limited_samples: u64,
}

#[allow(clippy::too_many_arguments)]
fn prepare_modulator_input_planes(
    pcm_scratch: &[f64],
    coeffs: &ModulatorCoeffs,
    dsd_modulator: DsdModulator,
    experiment_tweaks: DsdExperimentTweaks,
    input_gain: f64,
    pcm_l: &mut Vec<f64>,
    pcm_r: &mut Vec<f64>,
) -> PreparedModulatorInput {
    prepare_modulator_input_planes_for_peak(
        pcm_scratch,
        coeffs.input_peak,
        dsd_modulator,
        experiment_tweaks,
        input_gain,
        pcm_l,
        pcm_r,
    )
}

#[allow(clippy::too_many_arguments)]
fn prepare_modulator_input_planes_for_peak(
    pcm_scratch: &[f64],
    input_peak: f64,
    dsd_modulator: DsdModulator,
    experiment_tweaks: DsdExperimentTweaks,
    input_gain: f64,
    pcm_l: &mut Vec<f64>,
    pcm_r: &mut Vec<f64>,
) -> PreparedModulatorInput {
    let frames = pcm_scratch.len() / 2;
    let effective_input_gain =
        effective_modulator_input_gain(dsd_modulator, input_gain, experiment_tweaks.input_gain_db);
    let gain = input_peak * effective_input_gain;
    let limiter = ModulatorInputLimiter::new(input_peak);
    let mut block_peak = 0.0f64;
    let mut block_limited_samples = 0_u64;
    pcm_l.clear();
    pcm_r.clear();
    pcm_l.reserve(frames);
    pcm_r.reserve(frames);
    for chunk in pcm_scratch.chunks_exact(2) {
        let raw_l = chunk[0] * gain;
        let raw_r = chunk[1] * gain;
        if raw_l.is_finite() {
            block_peak = block_peak.max(raw_l.abs());
        }
        if raw_r.is_finite() {
            block_peak = block_peak.max(raw_r.abs());
        }
        if limiter.knee_touched(raw_l) {
            block_limited_samples += 1;
        }
        if limiter.knee_touched(raw_r) {
            block_limited_samples += 1;
        }
        pcm_l.push(limiter.limit(raw_l));
        pcm_r.push(limiter.limit(raw_r));
    }
    let headroom_gain = block_headroom_gain(block_peak, limiter.knee_start());
    if headroom_gain < 1.0 {
        pcm_l.clear();
        pcm_r.clear();
        let ridden_gain = gain * headroom_gain;
        for chunk in pcm_scratch.chunks_exact(2) {
            let raw_l = chunk[0] * ridden_gain;
            let raw_r = chunk[1] * ridden_gain;
            pcm_l.push(limiter.limit(raw_l));
            pcm_r.push(limiter.limit(raw_r));
        }
    }
    PreparedModulatorInput {
        block_peak,
        headroom_gain,
        block_limited_samples,
    }
}

fn record_limiter_telemetry(
    telemetry: &mut DsdLimiterTelemetry,
    input_peak: f64,
    block_peak: f64,
    block_gain: f64,
    block_limited_samples: u64,
) {
    let peak_ratio = if input_peak > 0.0 {
        (block_peak / input_peak).min(f32::MAX as f64) as f32
    } else {
        0.0
    };
    telemetry.current_block_peak_ratio = peak_ratio;
    telemetry.peak_ratio_max = telemetry.peak_ratio_max.max(peak_ratio);
    telemetry.current_block_gain = block_gain.min(f32::MAX as f64) as f32;
    telemetry.current_block_limited_samples = block_limited_samples;
    if block_limited_samples > 0 {
        telemetry.limited_events += 1;
        telemetry.limited_samples += block_limited_samples;
    }
}

#[derive(Clone, Copy)]
struct ModulatorInputLimiter {
    input_limit: f64,
    knee_start: f64,
    knee_width: f64,
}

impl ModulatorInputLimiter {
    fn new(input_peak: f64) -> Self {
        if !input_peak.is_finite() || input_peak <= 0.0 {
            return Self {
                input_limit: 0.0,
                knee_start: 0.0,
                knee_width: 0.0,
            };
        }
        let input_limit = input_peak.abs();
        let knee_start = input_limit * DSD_LIMITER_KNEE_RATIO;
        Self {
            input_limit,
            knee_start,
            knee_width: input_limit - knee_start,
        }
    }

    fn limit(self, sample: f64) -> f64 {
        if !sample.is_finite() || self.input_limit <= 0.0 {
            return 0.0;
        }

        let magnitude = sample.abs();
        if magnitude <= self.knee_start {
            return sample;
        }

        if self.knee_width <= f64::EPSILON {
            return sample.signum() * self.input_limit;
        }

        let excess = (magnitude - self.knee_start) / self.knee_width;
        let limited = self.knee_start + self.knee_width * excess.tanh().min(1.0);
        sample.signum() * limited.min(self.input_limit)
    }

    fn knee_touched(self, sample: f64) -> bool {
        sample.is_finite() && self.input_limit > 0.0 && sample.abs() > self.knee_start
    }

    fn knee_start(self) -> f64 {
        self.knee_start
    }
}

fn source_abs_peak(samples: &[f64]) -> f64 {
    samples
        .iter()
        .copied()
        .map(|sample| sample.abs())
        .fold(0.0, f64::max)
}

fn source_needs_sanitize(samples: &[f64]) -> bool {
    samples.iter().any(|sample| !sample.is_finite())
}

fn fill_sanitized_source_scratch(dst: &mut Vec<f64>, src: &[f64]) {
    dst.clear();
    dst.reserve(src.len());
    dst.extend(src.iter().map(|&sample| sanitize_source_sample(sample)));
}

fn sanitize_source_sample(sample: f64) -> f64 {
    if sample.is_finite() { sample } else { 0.0 }
}

fn interleave_stereo(samples_l: &[f64], samples_r: &[f64], out: &mut Vec<f64>) {
    out.clear();
    let frames = samples_l.len().min(samples_r.len());
    out.reserve(frames * 2);
    for idx in 0..frames {
        out.push(samples_l[idx]);
        out.push(samples_r[idx]);
    }
}

fn feed_interleaved(
    resampler: &mut SincResampler,
    interleaved: &[f64],
    plane_l: &mut Vec<f64>,
    plane_r: &mut Vec<f64>,
) {
    plane_l.clear();
    plane_r.clear();
    plane_l.reserve(interleaved.len() / 2);
    plane_r.reserve(interleaved.len() / 2);
    for frame in interleaved.chunks_exact(2) {
        plane_l.push(frame[0]);
        plane_r.push(frame[1]);
    }
    resampler.input(plane_l, plane_r);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dsd::dsd_coeffs::CALIBRATED;

    #[test]
    fn renderer_construction_matches_calibration_flag() {
        let result = DsdRenderer::new(FilterType::SincExtreme32k, 44_100, DsdRate::Dsd128);
        if CALIBRATED {
            let renderer = result.expect("calibrated coefficients should construct");
            assert_eq!(renderer.coefficient_table_name(), "CRFB7_STANDARD_OSR128");
            assert_eq!(renderer.coefficient_osr(), CRFB7_STANDARD_OSR128.osr);
            assert_eq!(renderer.coefficient_obg(), CRFB7_STANDARD_OSR128.obg);
            assert_eq!(
                renderer.modulator_input_peak(),
                CRFB7_STANDARD_OSR128.input_peak
            );
            assert_eq!(
                renderer.effective_modulator_seeds(),
                [DSD_MOD_SEED_LEFT, DSD_MOD_SEED_RIGHT]
            );
            assert_eq!(
                renderer.effective_experiment_tweaks(),
                DsdExperimentTweaks::default()
            );
        } else {
            assert!(result.is_err(), "placeholder coefficients must refuse");
        }
    }

    #[test]
    fn renderer_stores_sanitized_isi_penalty() {
        if !CALIBRATED {
            return;
        }
        let renderer = DsdRenderer::new_with_dsd_modulator_and_isi_penalty(
            FilterType::SincExtreme32k,
            44_100,
            DsdRate::Dsd128,
            DsdModulator::EcDepth2,
            0.01,
        )
        .expect("construction succeeds when calibrated");
        assert!((renderer.isi_penalty() - 0.01).abs() < f64::EPSILON);

        let clamped = DsdRenderer::new_with_dsd_modulator_and_isi_penalty(
            FilterType::SincExtreme32k,
            44_100,
            DsdRate::Dsd128,
            DsdModulator::EcDepth2,
            1.0,
        )
        .expect("construction succeeds when calibrated");
        assert!((clamped.isi_penalty() - 0.05).abs() < f64::EPSILON);
    }

    #[test]
    fn dsd128_long_filter_ec2_defaults_to_tuned_production_policy() {
        for filter in [
            FilterType::Minimum16k,
            FilterType::Split128k,
            FilterType::IntegratedPhase128k,
            FilterType::IntegratedPhase128kV2,
            FilterType::IntegratedPhase128kV3,
            FilterType::IntegratedPhase128kV4,
        ] {
            let default = DsdExperimentTweaks::default().with_production_policy_defaults(
                filter,
                DsdRate::Dsd128,
                DsdModulator::EcDepth2,
            );

            assert_eq!(default.ec_dither_scale_multiplier, Some(0.0));
            assert_eq!(default.ec_dither_shape, Some(DitherShape::HighPassTpdf));
            assert_eq!(default.ec_dither_prng, Some(DitherPrng::SplitMix64));
            assert_eq!(
                default.ec_future_scorer,
                Some(EcFutureScorer::QuantizerOnly)
            );
        }

        let split128k = DsdExperimentTweaks::default().with_production_policy_defaults(
            FilterType::Split128k,
            DsdRate::Dsd128,
            DsdModulator::EcDepth2,
        );
        assert_eq!(
            split128k.ec2_long_filter_policy,
            Some(Ec2LongFilterPolicy::AmbiguityPressure)
        );
        assert_eq!(
            split128k.ec2_policy_weights,
            Some(Ec2PolicyWeights {
                quantizer_weight: 0.8,
                pressure_weight: 1.5,
                limit_weight: 80.0,
                transition_weight: 0.002,
                dc_weight: 0.04,
                lookahead_discount: 0.6,
                ambiguity_margin: 0.0,
                pressure_taper_start: 0.60,
                pressure_taper_strength: 0.0,
            })
        );
        for filter in [
            FilterType::IntegratedPhase128k,
            FilterType::IntegratedPhase128kV2,
            FilterType::IntegratedPhase128kV3,
            FilterType::IntegratedPhase128kV4,
        ] {
            let integrated = DsdExperimentTweaks::default().with_production_policy_defaults(
                filter,
                DsdRate::Dsd128,
                DsdModulator::EcDepth2,
            );
            assert_eq!(
                integrated.ec2_long_filter_policy,
                split128k.ec2_long_filter_policy
            );
            assert_eq!(integrated.ec2_policy_weights, split128k.ec2_policy_weights);
        }

        let minimum16k = DsdExperimentTweaks::default().with_production_policy_defaults(
            FilterType::Minimum16k,
            DsdRate::Dsd128,
            DsdModulator::EcDepth2,
        );
        assert_eq!(minimum16k.ec2_long_filter_policy, None);
        assert_eq!(minimum16k.ec2_policy_weights, None);

        let explicit = DsdExperimentTweaks {
            ec_dither_scale_multiplier: Some(0.25),
            ec_dither_prng: Some(DitherPrng::Xoshiro256StarStar),
            ec_future_scorer: Some(EcFutureScorer::FullDiscount25),
            ec2_long_filter_policy: Some(Ec2LongFilterPolicy::Off),
            ec2_policy_weights: Some(Ec2PolicyWeights::default()),
            ..DsdExperimentTweaks::default()
        }
        .with_production_policy_defaults(
            FilterType::Split128k,
            DsdRate::Dsd128,
            DsdModulator::EcDepth2,
        );

        assert_eq!(explicit.ec_dither_scale_multiplier, Some(0.25));
        assert_eq!(
            explicit.ec_dither_prng,
            Some(DitherPrng::Xoshiro256StarStar)
        );
        assert_eq!(
            explicit.ec_future_scorer,
            Some(EcFutureScorer::FullDiscount25)
        );
        assert_eq!(
            explicit.ec2_long_filter_policy,
            Some(Ec2LongFilterPolicy::Off)
        );
        assert_eq!(
            explicit.ec2_policy_weights,
            Some(Ec2PolicyWeights::default())
        );

        let linear = DsdExperimentTweaks::default().with_production_policy_defaults(
            FilterType::SincExtreme32k,
            DsdRate::Dsd128,
            DsdModulator::EcDepth2,
        );
        assert_eq!(linear.ec_dither_scale_multiplier, None);
        assert_eq!(linear.ec_future_scorer, None);
        assert_eq!(linear.ec2_long_filter_policy, None);

        let linear_dsd256 = DsdExperimentTweaks::default().with_production_policy_defaults(
            FilterType::SincExtreme32k,
            DsdRate::Dsd256,
            DsdModulator::EcDepth2,
        );
        assert_eq!(linear_dsd256.ec_dither_scale_multiplier, None);
        assert_eq!(linear_dsd256.ec_future_scorer, None);
        assert_eq!(linear_dsd256.ec2_long_filter_policy, None);
    }

    #[test]
    fn ecbeam_defaults_dc_bias_tracker_to_rate_consistent_corner() {
        let non_beam = DsdExperimentTweaks::default();
        assert_eq!(ec_dc_bias_corner_hz_for_tweaks(non_beam), None);

        let beam = DsdExperimentTweaks {
            ec_beam_search: Some((4, 8)),
            ..DsdExperimentTweaks::default()
        };
        assert_eq!(
            ec_dc_bias_corner_hz_for_tweaks(beam),
            Some(DEFAULT_EC_BEAM_DC_BIAS_CORNER_HZ)
        );

        let explicit = DsdExperimentTweaks {
            ec_dc_bias_corner_hz: Some(10.0),
            ec_beam_search: Some((4, 8)),
            ..DsdExperimentTweaks::default()
        };
        assert_eq!(ec_dc_bias_corner_hz_for_tweaks(explicit), Some(10.0));

        let d64_decay = dc_bias_decay_for_corner_hz(DEFAULT_EC_BEAM_DC_BIAS_CORNER_HZ, 2_822_400);
        let d256_decay = dc_bias_decay_for_corner_hz(DEFAULT_EC_BEAM_DC_BIAS_CORNER_HZ, 11_289_600);
        assert!((d64_decay - (1.0 - core::f64::consts::TAU * 20.0 / 2_822_400.0)).abs() < 1e-15);
        assert!(
            d256_decay > d64_decay,
            "higher wire rates should get a slower per-sample decay"
        );
    }

    #[test]
    fn dsd64_ec2_defaults_to_tuned_production_policy() {
        for filter in [
            FilterType::Minimum16k,
            FilterType::Split128k,
            FilterType::IntegratedPhase128k,
        ] {
            let default = DsdExperimentTweaks::default().with_production_policy_defaults(
                filter,
                DsdRate::Dsd64,
                DsdModulator::EcDepth2,
            );

            assert_eq!(default.ec_dither_scale_multiplier, Some(0.0));
            assert_eq!(default.ec_dither_shape, Some(DitherShape::HighPassTpdf));
            assert_eq!(default.ec_dither_prng, Some(DitherPrng::SplitMix64));
            assert_eq!(default.ec_dither_leak_alpha, Some(0.99));
            assert_eq!(
                default.ec_future_scorer,
                Some(EcFutureScorer::QuarterPressureNoDcTransition)
            );
            assert_eq!(
                default.ec2_long_filter_policy,
                Some(Ec2LongFilterPolicy::AmbiguityPressure)
            );
            assert_eq!(
                default.ec2_policy_weights,
                Some(Ec2PolicyWeights {
                    quantizer_weight: 1.0,
                    pressure_weight: 0.75,
                    limit_weight: 80.0,
                    transition_weight: 0.0,
                    dc_weight: 0.04,
                    lookahead_discount: 0.8,
                    ambiguity_margin: 0.005,
                    pressure_taper_start: 0.45,
                    pressure_taper_strength: 2.0,
                })
            );
        }

        let explicit = DsdExperimentTweaks {
            ec_dither_scale_multiplier: Some(0.25),
            ec_dither_prng: Some(DitherPrng::Xoshiro256StarStar),
            ec_dither_leak_alpha: Some(0.98),
            ec_future_scorer: Some(EcFutureScorer::FullDiscount25),
            ec2_long_filter_policy: Some(Ec2LongFilterPolicy::Off),
            ec2_policy_weights: Some(Ec2PolicyWeights::default()),
            ..DsdExperimentTweaks::default()
        }
        .with_production_policy_defaults(
            FilterType::Split128k,
            DsdRate::Dsd64,
            DsdModulator::EcDepth2,
        );

        assert_eq!(explicit.ec_dither_scale_multiplier, Some(0.25));
        assert_eq!(
            explicit.ec_dither_prng,
            Some(DitherPrng::Xoshiro256StarStar)
        );
        assert_eq!(explicit.ec_dither_leak_alpha, Some(0.98));
        assert_eq!(
            explicit.ec_future_scorer,
            Some(EcFutureScorer::FullDiscount25)
        );
        assert_eq!(
            explicit.ec2_long_filter_policy,
            Some(Ec2LongFilterPolicy::Off)
        );
        assert_eq!(
            explicit.ec2_policy_weights,
            Some(Ec2PolicyWeights::default())
        );
    }

    #[test]
    fn dsd64_ecbeam_defaults_to_a1_obg165_policy() {
        for filter in [
            FilterType::Minimum16k,
            FilterType::Split128k,
            FilterType::IntegratedPhase128k,
        ] {
            let default = DsdExperimentTweaks {
                ec_beam_search: Some((4, 8)),
                ..DsdExperimentTweaks::default()
            }
            .with_production_policy_defaults(
                filter,
                DsdRate::Dsd64,
                DsdModulator::EcDepth2,
            );

            assert_eq!(default.ec_dither_scale_multiplier, Some(0.0));
            assert_eq!(default.ec_dither_shape, Some(DitherShape::HighPassTpdf));
            assert_eq!(default.ec_dither_prng, Some(DitherPrng::SplitMix64));
            assert_eq!(default.ec_dither_leak_alpha, Some(0.99));
            assert_eq!(
                default.ec_future_scorer,
                Some(EcFutureScorer::QuantizerOnly)
            );
            assert_eq!(
                default.ec2_long_filter_policy,
                Some(Ec2LongFilterPolicy::AmbiguityPressure)
            );
            assert_eq!(
                default.ec2_policy_weights,
                Some(Ec2PolicyWeights {
                    quantizer_weight: 0.8,
                    pressure_weight: 2.75,
                    limit_weight: 80.0,
                    transition_weight: 0.002,
                    dc_weight: 0.04,
                    lookahead_discount: 0.8,
                    ambiguity_margin: 0.0,
                    pressure_taper_start: 0.60,
                    pressure_taper_strength: 0.0,
                })
            );
            assert_eq!(
                default.ec2_pressure_stage_weights,
                Some(DSD64_EC_BEAM_A1_PRESSURE_STAGE_WEIGHTS)
            );
            assert_eq!(default.ec_beam_terminal_weight, Some(0.3));
            assert_eq!(default.ec_beam_alternation_weight, Some(0.0005));

            let coeffs =
                production_ec_coeffs_for(filter, DsdRate::Dsd64, DsdModulator::EcDepth2, default)
                    .expect("DSD64 EcBeam production coeffs");
            assert!((coeffs.coeffs.obg - DSD64_EC_BEAM_A1_DEFAULT_OBG).abs() < 1e-12);
        }

        let plain = DsdExperimentTweaks::default().with_production_policy_defaults(
            FilterType::Split128k,
            DsdRate::Dsd64,
            DsdModulator::EcDepth2,
        );
        let coeffs = production_ec_coeffs_for(
            FilterType::Split128k,
            DsdRate::Dsd64,
            DsdModulator::EcDepth2,
            plain,
        )
        .expect("plain DSD64 EcDepth2 production coeffs");
        assert!((coeffs.coeffs.obg - 1.44).abs() < 1e-12);
    }

    #[test]
    fn selectable_ecbeam_enables_a1_search_at_every_dsd_rate() {
        for rate in [DsdRate::Dsd64, DsdRate::Dsd128, DsdRate::Dsd256] {
            let default = DsdExperimentTweaks::default().with_production_policy_defaults(
                FilterType::Split128k,
                rate,
                DsdModulator::EcBeam,
            );

            assert_eq!(default.ec_beam_search, Some((4, 8)));
            assert_eq!(default.ec_dither_scale_multiplier, Some(0.0));
            assert_eq!(
                default.ec_future_scorer,
                Some(EcFutureScorer::QuantizerOnly)
            );
            assert_eq!(
                default.ec2_pressure_stage_weights,
                Some(DSD64_EC_BEAM_A1_PRESSURE_STAGE_WEIGHTS)
            );
            assert_eq!(default.ec_beam_terminal_weight, Some(0.3));
            assert_eq!(default.ec_beam_alternation_weight, Some(0.0005));
        }
    }

    #[test]
    fn selectable_ecbeam_matches_explicit_dsd64_a1_profile() {
        let selectable = DsdExperimentTweaks::default().with_production_policy_defaults(
            FilterType::Split128k,
            DsdRate::Dsd64,
            DsdModulator::EcBeam,
        );
        let explicit_a1 = DsdExperimentTweaks {
            ec_beam_search: Some((4, 8)),
            ..DsdExperimentTweaks::default()
        }
        .with_production_policy_defaults(
            FilterType::Split128k,
            DsdRate::Dsd64,
            DsdModulator::EcDepth2,
        );

        assert_eq!(selectable, explicit_a1);
        let coeffs = production_ec_coeffs_for(
            FilterType::Split128k,
            DsdRate::Dsd64,
            DsdModulator::EcBeam,
            selectable,
        )
        .expect("selectable ECB coefficient table");
        assert!((coeffs.coeffs.obg - DSD64_EC_BEAM_A1_DEFAULT_OBG).abs() < 1e-12);
    }

    #[test]
    fn ecbeam2_policy_defaults_separate_playback_and_research_telemetry() {
        let dsd64_playback = DsdExperimentTweaks::default().with_production_policy_defaults(
            FilterType::Minimum16k,
            DsdRate::Dsd64,
            DsdModulator::EcBeam2,
        );
        let expected_dsd64 = ecbeam2_production_config();
        assert_eq!(dsd64_playback.ecbeam2_config, Some(expected_dsd64));
        assert_eq!(expected_dsd64.quantizer_regularizer, 0.03);
        assert_eq!(dsd64_playback.ecbeam2_full_diagnostics, Some(false));

        let dsd128_playback = DsdExperimentTweaks::default().with_production_policy_defaults(
            FilterType::Split128k,
            DsdRate::Dsd128,
            DsdModulator::EcBeam2,
        );
        let expected = ecbeam2_production_config();
        assert_eq!(dsd128_playback.ecbeam2_config, Some(expected));
        assert_eq!(dsd128_playback.ecbeam2_full_diagnostics, Some(false));

        let research = DsdExperimentTweaks {
            ecbeam2_config: Some(EcBeam2ExperimentConfig::default()),
            ..DsdExperimentTweaks::default()
        }
        .with_production_policy_defaults(
            FilterType::Minimum16k,
            DsdRate::Dsd64,
            DsdModulator::EcBeam2,
        );
        assert_eq!(research.ecbeam2_full_diagnostics, Some(true));
    }

    #[test]
    fn selectable_ecbeam_renderer_uses_beam_path_at_every_dsd_rate() {
        for filter in [
            FilterType::Split128k,
            FilterType::IntegratedPhase128k,
            FilterType::IntegratedPhase128kV2,
            FilterType::IntegratedPhase128kV3,
            FilterType::IntegratedPhase128kV4,
        ] {
            for rate in [DsdRate::Dsd64, DsdRate::Dsd128, DsdRate::Dsd256] {
                let renderer =
                    DsdRenderer::new_with_dsd_modulator(filter, 44_100, rate, DsdModulator::EcBeam)
                        .expect("selectable Search renderer");

                assert_eq!(renderer.dsd_modulator(), DsdModulator::EcBeam);
                assert_eq!(renderer.experiment_tweaks.ec_beam_search, Some((4, 8)));
                if rate == DsdRate::Dsd64 {
                    assert!((renderer.coeffs.obg - DSD64_EC_BEAM_A1_DEFAULT_OBG).abs() < 1e-12);
                }
            }
        }
    }

    #[test]
    fn minimum_phase128k_profiles_construct_dsd128_search() {
        for filter in [
            FilterType::MinimumPhase128k,
            FilterType::MinimumPhase128kV2,
            FilterType::MinimumPhase128kV3,
            FilterType::MinimumPhase128kV4,
            FilterType::MinimumPhaseCompact128k,
            FilterType::MinimumPhaseCompact128kV2,
            FilterType::SmoothPhase128k,
        ] {
            let renderer = DsdRenderer::new_with_dsd_modulator(
                filter,
                44_100,
                DsdRate::Dsd128,
                DsdModulator::EcBeam,
            )
            .expect("Minimum Phase 128k DSD128 Search renderer");
            assert_eq!(renderer.dsd_modulator(), DsdModulator::EcBeam);
            assert_eq!(renderer.experiment_tweaks.ec_beam_search, Some((4, 8)));
        }
    }

    #[test]
    fn dsd256_ec2_defaults_to_dsd64_style_production_policy() {
        for filter in [
            FilterType::Minimum16k,
            FilterType::Split128k,
            FilterType::IntegratedPhase128k,
        ] {
            let default = DsdExperimentTweaks::default().with_production_policy_defaults(
                filter,
                DsdRate::Dsd256,
                DsdModulator::EcDepth2,
            );

            assert_eq!(default.ec_dither_scale_multiplier, Some(0.0));
            assert_eq!(default.ec_dither_shape, Some(DitherShape::HighPassTpdf));
            assert_eq!(default.ec_dither_prng, Some(DitherPrng::SplitMix64));
            assert_eq!(
                default.ec_future_scorer,
                Some(EcFutureScorer::QuantizerOnly)
            );
            assert_eq!(
                default.ec2_long_filter_policy,
                Some(Ec2LongFilterPolicy::AmbiguityPressure)
            );
            assert_eq!(
                default.ec2_policy_weights,
                Some(Ec2PolicyWeights {
                    quantizer_weight: 0.8,
                    pressure_weight: 1.5,
                    limit_weight: 80.0,
                    transition_weight: 0.002,
                    dc_weight: 0.04,
                    lookahead_discount: 0.6,
                    ambiguity_margin: 0.0,
                    pressure_taper_start: 0.60,
                    pressure_taper_strength: 0.0,
                })
            );
        }

        let explicit = DsdExperimentTweaks {
            ec_dither_scale_multiplier: Some(0.25),
            ec_dither_prng: Some(DitherPrng::Xoshiro256StarStar),
            ec_future_scorer: Some(EcFutureScorer::FullDiscount25),
            ec2_long_filter_policy: Some(Ec2LongFilterPolicy::Off),
            ec2_policy_weights: Some(Ec2PolicyWeights::default()),
            ..DsdExperimentTweaks::default()
        }
        .with_production_policy_defaults(
            FilterType::Minimum16k,
            DsdRate::Dsd256,
            DsdModulator::EcDepth2,
        );

        assert_eq!(explicit.ec_dither_scale_multiplier, Some(0.25));
        assert_eq!(
            explicit.ec_dither_prng,
            Some(DitherPrng::Xoshiro256StarStar)
        );
        assert_eq!(
            explicit.ec_future_scorer,
            Some(EcFutureScorer::FullDiscount25)
        );
        assert_eq!(
            explicit.ec2_long_filter_policy,
            Some(Ec2LongFilterPolicy::Off)
        );
        assert_eq!(
            explicit.ec2_policy_weights,
            Some(Ec2PolicyWeights::default())
        );
    }

    #[test]
    fn renderer_produces_dop_frames_at_expected_rate() {
        if !CALIBRATED {
            return;
        }
        let mut r = DsdRenderer::new(FilterType::Minimum16k, 44_100, DsdRate::Dsd128)
            .expect("construction succeeds when calibrated");
        // Push 1024 source frames @ 44.1kHz: expect roughly 128× upsample → 131072
        // DSD bits, packed 16-to-1 = 8192 DoP samples per channel = 16384 interleaved i32s.
        // Cascade group delay swallows the first batch's worth, so settle for "well above 0".
        let l: Vec<f64> = (0..8192)
            .map(|i| 0.4 * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 44_100.0).sin())
            .collect();
        let r_in = l.clone();
        let mut out = Vec::new();
        r.render(&l, &r_in, &mut out);
        // The modulate stage is pipelined one block deep; the flush emits the
        // held block.
        r.flush_modulators_and_pack(&mut out);
        assert!(
            out.len() >= 8192,
            "expected substantial DoP output, got {} samples",
            out.len()
        );
        // Interleaved stereo → length must be even.
        assert_eq!(out.len() % 2, 0);
        assert_eq!(r.stability_resets(), 0);
    }

    #[test]
    fn dsd64_wire_rates_cover_both_source_families() {
        for source in [44_100, 88_200, 176_400] {
            assert_eq!(DsdRate::Dsd64.wire_rate_for_source(source), Some(2_822_400));
        }
        for source in [48_000, 96_000, 192_000] {
            assert_eq!(DsdRate::Dsd64.wire_rate_for_source(source), Some(3_072_000));
        }
        assert_eq!(DsdRate::Dsd64.wire_rate_for_source(22_050), None);
        // DoP carrier rates: DSD64 fits standard 176.4/192 kHz PCM endpoints.
        assert_eq!(DsdRate::dop_frame_rate(2_822_400), 176_400);
        assert_eq!(DsdRate::dop_frame_rate(3_072_000), 192_000);
    }

    #[test]
    fn dsd64_renderer_produces_dop_frames_at_176k_carrier() {
        if !CALIBRATED {
            return;
        }
        let mut r = DsdRenderer::new(FilterType::Minimum16k, 44_100, DsdRate::Dsd64)
            .expect("calibrated DSD64 renderer");
        assert_eq!(r.dop_frame_rate(), 176_400);
        let l: Vec<f64> = (0..8192)
            .map(|i| 0.4 * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 44_100.0).sin())
            .collect();
        let r_in = l.clone();
        let mut out = Vec::new();
        r.render(&l, &r_in, &mut out);
        r.flush_modulators_and_pack(&mut out);
        assert!(
            out.len() >= 4096,
            "expected substantial DSD64 DoP output, got {} samples",
            out.len()
        );
        assert_eq!(out.len() % 2, 0);
        assert_eq!(r.stability_resets(), 0);
    }

    #[test]
    fn dsd64_renderer_uses_osr64_coefficients() {
        for mode in [ModulatorMode::Standard, ModulatorMode::Ec] {
            assert_eq!(DsdRate::Dsd64.coeffs_for_mode(mode).osr, 64);
        }
    }

    #[test]
    fn default_coefficient_table_names_match_their_tables() {
        for (rate, mode, expected_name, expected_coeffs) in [
            (
                DsdRate::Dsd64,
                ModulatorMode::Standard,
                "CRFB7_STANDARD_OSR64",
                &CRFB7_STANDARD_OSR64,
            ),
            (
                DsdRate::Dsd128,
                ModulatorMode::Standard,
                "CRFB7_STANDARD_OSR128",
                &CRFB7_STANDARD_OSR128,
            ),
            (
                DsdRate::Dsd256,
                ModulatorMode::Standard,
                "CRFB7_STANDARD_OSR256",
                &CRFB7_STANDARD_OSR256,
            ),
            (
                DsdRate::Dsd64,
                ModulatorMode::Ec,
                "CRFB7_EC_OSR64",
                &CRFB7_EC_OSR64,
            ),
            (
                DsdRate::Dsd128,
                ModulatorMode::Ec,
                "CRFB7_EC_OSR128",
                &CRFB7_EC_OSR128,
            ),
            (
                DsdRate::Dsd256,
                ModulatorMode::Ec,
                "CRFB7_EC_OSR256",
                &CRFB7_EC_OSR256,
            ),
        ] {
            let selected = default_coeffs_for_mode(rate, mode);
            assert_eq!(selected.name, expected_name);
            assert_eq!(selected.coeffs.osr, expected_coeffs.osr);
            assert_eq!(selected.coeffs.obg, expected_coeffs.obg);
            assert_eq!(selected.coeffs.input_peak, expected_coeffs.input_peak);
        }
    }

    #[test]
    fn coefficient_selection_reports_production_policy_and_custom_overrides() {
        let ec2_tweaks = DsdExperimentTweaks::default().with_production_policy_defaults(
            FilterType::Split128k,
            DsdRate::Dsd64,
            DsdModulator::EcDepth2,
        );
        let ec2 = select_modulator_coeffs(
            FilterType::Split128k,
            DsdRate::Dsd64,
            DsdModulator::EcDepth2,
            None,
            ec2_tweaks,
        )
        .expect("production EC-2 table");
        assert_eq!(ec2.name, "CRFB7_EC_OSR64_OBG144");
        assert_eq!(ec2.coeffs.obg, CRFB7_EC_OSR64_OBG144.obg);

        let ecbeam_tweaks = DsdExperimentTweaks::default().with_production_policy_defaults(
            FilterType::Split128k,
            DsdRate::Dsd64,
            DsdModulator::EcBeam,
        );
        let ecbeam = select_modulator_coeffs(
            FilterType::Split128k,
            DsdRate::Dsd64,
            DsdModulator::EcBeam,
            None,
            ecbeam_tweaks,
        )
        .expect("production EcBeam table");
        assert_eq!(ecbeam.name, "CRFB_OSR64_OBG165");
        assert_eq!(ecbeam.coeffs.obg, CRFB_OSR64_OBG165.obg);

        let ecbeam2_default = select_modulator_coeffs(
            FilterType::Split128k,
            DsdRate::Dsd64,
            DsdModulator::EcBeam2,
            None,
            DsdExperimentTweaks::default(),
        )
        .expect("default EcBeam2 table");
        assert_eq!(ecbeam2_default.name, "ECBEAM2_OSR64_OBG164_INPUT468_V1");
        assert_eq!(ecbeam2_default.coeffs.obg, 1.64);
        assert_eq!(ecbeam2_default.coeffs.input_peak, 0.467_858_988_519_470_7);

        let ecbeam2_dsd128_config = ecbeam2_production_config();
        let ecbeam2_dsd128 = select_modulator_coeffs(
            FilterType::Split128k,
            DsdRate::Dsd128,
            DsdModulator::EcBeam2,
            None,
            DsdExperimentTweaks {
                ecbeam2_config: Some(ecbeam2_dsd128_config),
                ..DsdExperimentTweaks::default()
            },
        )
        .expect("production DSD128 EcBeam2 table");
        assert_eq!(ecbeam2_dsd128.name, "ECBEAM2_OSR128_OBG164_INPUT468_V1");
        assert_eq!(ecbeam2_dsd128.coeffs.osr, 128);
        assert_eq!(ecbeam2_dsd128.coeffs.obg, 1.64);

        let ecbeam2_override_error = select_modulator_coeffs(
            FilterType::Split128k,
            DsdRate::Dsd64,
            DsdModulator::EcBeam2,
            Some(&CRFB_OSR64_OBG165),
            DsdExperimentTweaks::default(),
        )
        .err();
        assert_eq!(
            ecbeam2_override_error,
            Some("EcBeam2 coefficient overrides are not supported")
        );

        let custom_seeds = [0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210];
        let custom_tweaks = DsdExperimentTweaks {
            seed_left: Some(custom_seeds[0]),
            seed_right: Some(custom_seeds[1]),
            ..DsdExperimentTweaks::default()
        }
        .with_production_policy_defaults(
            FilterType::Split128k,
            DsdRate::Dsd64,
            DsdModulator::EcDepth2,
        );
        let custom = select_modulator_coeffs(
            FilterType::Split128k,
            DsdRate::Dsd64,
            DsdModulator::EcDepth2,
            Some(&CRFB7_EC_OSR64),
            custom_tweaks,
        )
        .expect("custom-coefficient table");
        assert_eq!(custom.name, "custom_override");
        assert_eq!(custom.coeffs.osr, CRFB7_EC_OSR64.osr);
        assert_eq!(custom.coeffs.obg, CRFB7_EC_OSR64.obg);
        assert_eq!(custom_tweaks.seed_left, Some(custom_seeds[0]));
        assert_eq!(custom_tweaks.seed_right, Some(custom_seeds[1]));

        let standard_override_error = select_modulator_coeffs(
            FilterType::Split128k,
            DsdRate::Dsd64,
            DsdModulator::Standard,
            Some(&CRFB7_EC_OSR64),
            DsdExperimentTweaks::default(),
        )
        .err();
        assert_eq!(
            standard_override_error,
            Some("DSD coefficient override is only supported for EC modulators")
        );
    }

    #[test]
    fn renderer_produces_planar_native_dsd256_bytes() {
        if !CALIBRATED {
            return;
        }
        let mut renderer =
            DsdRenderer::new(FilterType::Minimum16k, 44_100, DsdRate::Dsd256).unwrap();
        let input: Vec<f64> = (0..8192)
            .map(|i| 0.2 * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 44_100.0).sin())
            .collect();
        renderer.upsample(&input, &input);
        let mut l = Vec::new();
        let mut r = Vec::new();
        renderer.modulate_and_pack_native(1.0, &mut l, &mut r);
        renderer.flush_modulators_and_pack_native(&mut l, &mut r);
        assert!(!l.is_empty());
        assert_eq!(l.len(), r.len());
    }

    fn short_ec2_dsd256_input() -> Vec<f64> {
        (0..8192)
            .map(|i| 0.125 * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 44_100.0).sin())
            .collect()
    }

    fn render_short_ec2_native_dsd256(
        input: &[f64],
    ) -> (Vec<u8>, Vec<u8>, DsdLimiterTelemetry, u64, u64) {
        let mut renderer = DsdRenderer::new_with_dsd_modulator(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd256,
            DsdModulator::EcDepth2,
        )
        .unwrap();
        renderer.set_native_order(NativeDsdOrder::MsbFirst);
        assert_eq!(renderer.dsd_modulator(), DsdModulator::EcDepth2);
        assert_eq!(renderer.modulator_mode(), ModulatorMode::Ec);
        assert_eq!(renderer.dop_frame_rate(), 705_600);

        renderer.upsample(input, input);
        let mut l = Vec::new();
        let mut r = Vec::new();
        renderer.modulate_and_pack_native(1.0, &mut l, &mut r);
        renderer.flush_modulators_and_pack_native(&mut l, &mut r);
        renderer.flush_native_with_idle(&mut l, &mut r);
        (
            l,
            r,
            renderer.limiter_telemetry(),
            renderer.stability_resets(),
            renderer.state_clamps(),
        )
    }

    #[test]
    fn ec2_renderer_produces_planar_native_dsd256_bytes() {
        if !CALIBRATED {
            return;
        }
        let input = short_ec2_dsd256_input();
        let (l, r, telemetry, stability_resets, state_clamps) =
            render_short_ec2_native_dsd256(&input);
        assert!(!l.is_empty());
        assert_eq!(l.len(), r.len());
        assert_eq!(stability_resets, 0);
        assert_eq!(state_clamps, 0);
        assert_eq!(telemetry.current_block_limited_samples, 0);
        assert_eq!(telemetry.limited_events, 0);
        assert_eq!(telemetry.limited_samples, 0);
        assert!(telemetry.peak_ratio_max.is_finite());
    }

    #[test]
    fn ec2_renderer_sanitizes_nonfinite_source_samples() {
        if !CALIBRATED {
            return;
        }
        let mut dirty = short_ec2_dsd256_input();
        let mut clean = dirty.clone();
        dirty[7] = f64::NAN;
        dirty[23] = f64::INFINITY;
        dirty[41] = f64::NEG_INFINITY;
        clean[7] = 0.0;
        clean[23] = 0.0;
        clean[41] = 0.0;

        let (dirty_l, dirty_r, telemetry, stability_resets, state_clamps) =
            render_short_ec2_native_dsd256(&dirty);
        let (clean_l, clean_r, _, _, _) = render_short_ec2_native_dsd256(&clean);

        assert_eq!(dirty_l, clean_l);
        assert_eq!(dirty_r, clean_r);
        assert_eq!(stability_resets, 0);
        assert_eq!(state_clamps, 0);
        assert_eq!(telemetry.current_block_limited_samples, 0);
        assert_eq!(telemetry.limited_events, 0);
        assert_eq!(telemetry.limited_samples, 0);
        assert!(telemetry.peak_ratio_max.is_finite());
    }

    #[test]
    fn dsd64_production_ec2_uses_obg144_table_for_tuned_filters() {
        if !CALIBRATED {
            return;
        }
        for filter in [
            FilterType::Minimum16k,
            FilterType::Split128k,
            FilterType::IntegratedPhase128k,
        ] {
            let renderer = DsdRenderer::new_with_dsd_modulator(
                filter,
                44_100,
                DsdRate::Dsd64,
                DsdModulator::EcDepth2,
            )
            .unwrap();
            assert!((renderer.coeffs.obg - 1.44).abs() < 1e-9);
            assert_eq!(renderer.coeffs.osr, 64);
        }

        // Unmeasured combinations keep the OBG 1.40 default table.
        let linear = DsdRenderer::new_with_dsd_modulator(
            FilterType::SincExtreme32k,
            44_100,
            DsdRate::Dsd64,
            DsdModulator::EcDepth2,
        )
        .unwrap();
        assert!((linear.coeffs.obg - 1.40).abs() < 1e-9);

        let standard = DsdRenderer::new_with_dsd_modulator(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd64,
            DsdModulator::Standard,
        )
        .unwrap();
        assert!((standard.coeffs.obg - 1.50).abs() < 1e-9);

        let dsd128 = DsdRenderer::new_with_dsd_modulator(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd128,
            DsdModulator::EcDepth2,
        )
        .unwrap();
        assert_eq!(dsd128.coeffs.osr, 128);
        assert!((dsd128.coeffs.obg - 1.40).abs() < 1e-9);
    }

    fn render_short_dsd64_ec2_with_tweaks(
        input: &[f64],
        tweaks: DsdExperimentTweaks,
    ) -> (Vec<u8>, Vec<u8>, u64, u64) {
        let mut renderer = DsdRenderer::new_with_dsd_modulator_and_experiment_tweaks(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd64,
            DsdModulator::EcDepth2,
            None,
            tweaks,
        )
        .unwrap();
        renderer.set_native_order(NativeDsdOrder::MsbFirst);
        renderer.upsample(input, input);
        let mut l = Vec::new();
        let mut r = Vec::new();
        renderer.modulate_and_pack_native(1.0, &mut l, &mut r);
        renderer.flush_modulators_and_pack_native(&mut l, &mut r);
        renderer.flush_native_with_idle(&mut l, &mut r);
        (l, r, renderer.stability_resets(), renderer.state_clamps())
    }

    #[test]
    fn experiment_tweaks_plumb_stage_weights_and_gated_dither_to_workers() {
        if !CALIBRATED {
            return;
        }
        // Leading silence guarantees idle near-ties for the gated-dither arm.
        let input: Vec<f64> = (0..8192)
            .map(|i| {
                if i < 2048 {
                    0.0
                } else {
                    0.125 * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 44_100.0).sin()
                }
            })
            .collect();
        let (base_l, base_r, _, _) =
            render_short_dsd64_ec2_with_tweaks(&input, DsdExperimentTweaks::default());
        assert!(!base_l.is_empty());

        // Uniform stage weights normalize to exactly 1/7: bit-identical.
        let uniform = DsdExperimentTweaks {
            ec2_pressure_stage_weights: Some([3.0; 7]),
            ..DsdExperimentTweaks::default()
        };
        let (uniform_l, uniform_r, _, _) = render_short_dsd64_ec2_with_tweaks(&input, uniform);
        assert_eq!(uniform_l, base_l);
        assert_eq!(uniform_r, base_r);

        // A skewed profile must reach the worker's modulator and change bits.
        let skewed = DsdExperimentTweaks {
            ec2_pressure_stage_weights: Some([1.6, 1.4, 1.2, 1.0, 0.8, 0.6, 0.4]),
            ..DsdExperimentTweaks::default()
        };
        let (skewed_l, skewed_r, skewed_resets, skewed_clamps) =
            render_short_dsd64_ec2_with_tweaks(&input, skewed);
        assert!(
            skewed_l != base_l || skewed_r != base_r,
            "skewed stage weights did not reach the modulator workers"
        );
        assert_eq!(skewed_resets, 0);
        assert_eq!(skewed_clamps, 0);

        // Gated dither works at production dither scale 0 (it perturbs with the
        // raw TPDF draw); margin-only must stay inert.
        let gated = DsdExperimentTweaks {
            ec_gated_dither_margin: Some(0.2),
            ec_gated_dither_scale: Some(0.25),
            ..DsdExperimentTweaks::default()
        };
        let (gated_l, gated_r, gated_resets, gated_clamps) =
            render_short_dsd64_ec2_with_tweaks(&input, gated);
        assert!(
            gated_l != base_l || gated_r != base_r,
            "gated dither did not reach the modulator workers"
        );
        assert_eq!(gated_resets, 0);
        assert_eq!(gated_clamps, 0);

        let margin_only = DsdExperimentTweaks {
            ec_gated_dither_margin: Some(0.2),
            ..DsdExperimentTweaks::default()
        };
        let (margin_l, margin_r, _, _) = render_short_dsd64_ec2_with_tweaks(&input, margin_only);
        assert_eq!(margin_l, base_l);
        assert_eq!(margin_r, base_r);
    }

    #[test]
    fn renderer_can_force_48_family_sources_to_44k_family_dsd256() {
        if !CALIBRATED {
            return;
        }
        let mut renderer =
            DsdRenderer::new_44k_family(FilterType::Minimum16k, 96_000, DsdRate::Dsd256)
                .expect("48-family source should render to 44.1-family DSD256");
        assert_eq!(renderer.dop_frame_rate(), 705_600);
        let input = vec![0.0; 4096];
        renderer.upsample(&input, &input);
        let mut out = Vec::new();
        renderer.modulate_and_pack(1.0, &mut out);
        assert_eq!(out.len() % 2, 0);
    }

    #[test]
    fn cross_family_dsd256_final_stage_uses_selected_filter() {
        let upsampler = DsdUpsampler::new(
            FilterType::Minimum16k,
            192_000,
            DsdRate::Dsd256.wire_rate_44k_family(),
        );
        let DsdUpsampler::CrossFamily(chain) = upsampler else {
            panic!("48-family source forced to 44.1-family DSD256 should use cross-family chain");
        };

        assert_eq!(
            chain.stage3.source_rate(),
            CrossFamilyDsdChain::HOP_RATE_44K
        );
        assert_eq!(
            chain.stage3.target_rate(),
            DsdRate::Dsd256.wire_rate_44k_family()
        );
        assert!(
            chain.stage3.is_high_latency(),
            "final 176.4k -> DSD256 stage should use the selected high-latency filter"
        );
    }

    #[test]
    fn cross_family_fractional_hop_uses_selected_filter_with_bounded_memory() {
        let upsampler = DsdUpsampler::new(
            FilterType::Minimum16k,
            192_000,
            DsdRate::Dsd256.wire_rate_44k_family(),
        );
        let DsdUpsampler::CrossFamily(chain) = upsampler else {
            panic!("48-family source forced to 44.1-family DSD256 should use cross-family chain");
        };

        assert_eq!(
            chain.stage2.source_rate(),
            CrossFamilyDsdChain::HOP_RATE_48K
        );
        assert_eq!(
            chain.stage2.target_rate(),
            CrossFamilyDsdChain::HOP_RATE_44K
        );
        assert_eq!(chain.stage2.filter_type(), FilterType::Minimum16k);
        assert!(
            chain.stage2.estimated_memory_bytes() < 80 * 1024 * 1024,
            "fractional hop must stay under the bounded polyphase kernel cap"
        );
    }

    #[test]
    fn cross_family_fractional_hop_uses_selected_bridge_for_all_filters() {
        let medium = DsdUpsampler::new(
            FilterType::SincExtreme32k,
            192_000,
            DsdRate::Dsd128.wire_rate_44k_family(),
        );
        let DsdUpsampler::CrossFamily(medium_chain) = medium else {
            panic!("48-family source forced to 44.1-family DSD128 should use cross-family chain");
        };
        assert_eq!(
            medium_chain.stage2.filter_type(),
            FilterType::SincExtreme32k
        );

        let minimum = DsdUpsampler::new(
            FilterType::Minimum16k,
            192_000,
            DsdRate::Dsd128.wire_rate_44k_family(),
        );
        let DsdUpsampler::CrossFamily(minimum_chain) = minimum else {
            panic!("48-family source forced to 44.1-family DSD128 should use cross-family chain");
        };
        assert_eq!(minimum_chain.stage2.filter_type(), FilterType::Minimum16k);
    }

    #[test]
    fn modulator_input_limiter_catches_intersample_overs() {
        let input_peak = DsdRate::Dsd128
            .coeffs_for_mode(ModulatorMode::Ec)
            .input_peak;
        assert_eq!(
            limit_modulator_input(0.5 * input_peak, input_peak),
            0.5 * input_peak
        );
        assert_eq!(
            limit_modulator_input(-0.5 * input_peak, input_peak),
            -0.5 * input_peak
        );

        let full_scale_after_headroom = limit_modulator_input(input_peak, input_peak);
        assert!(
            full_scale_after_headroom <= input_peak
                && full_scale_after_headroom >= input_peak * 0.98,
            "nominal full-scale after DSD headroom should be nearly untouched"
        );

        assert!(limit_modulator_input(2.0, input_peak) <= input_peak);
        assert!(limit_modulator_input(-2.0, input_peak) >= -input_peak);
        assert_eq!(limit_modulator_input(f64::NAN, input_peak), 0.0);
    }

    #[test]
    fn ecbeam2_oracle_input_inspection_shares_production_normalization() {
        let mut renderer = DsdRenderer::new_with_dsd_modulator(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd64,
            DsdModulator::EcBeam2,
        )
        .unwrap();
        renderer.pcm_scratch = vec![0.25, -0.50, 1.25, -1.0, f64::NAN, 0.1];
        let (inspected_l, inspected_r) =
            renderer.ecbeam2_oracle_modulator_input_block(1.0).unwrap();

        let mut production_l = Vec::new();
        let mut production_r = Vec::new();
        let prepared = prepare_modulator_input_planes(
            &renderer.pcm_scratch,
            renderer.coeffs,
            renderer.dsd_modulator,
            renderer.experiment_tweaks,
            1.0,
            &mut production_l,
            &mut production_r,
        );
        assert_eq!(inspected_l, production_l);
        assert_eq!(inspected_r, production_r);
        assert_eq!(inspected_l.len(), 3);
        assert_eq!(inspected_l[2], 0.0);
        assert!(prepared.headroom_gain < 1.0);
        assert!(
            inspected_l
                .iter()
                .chain(&inspected_r)
                .all(|sample| { sample.is_finite() && sample.abs() <= renderer.coeffs.input_peak })
        );
    }

    #[test]
    fn dsd_block_headroom_gain_only_attenuates_to_limiter_knee() {
        let limiter = ModulatorInputLimiter::new(0.5);
        assert_eq!(block_headroom_gain(0.25, limiter.knee_start()), 1.0);
        assert_eq!(block_headroom_gain(0.0, limiter.knee_start()), 1.0);
        assert_eq!(block_headroom_gain(f64::NAN, limiter.knee_start()), 1.0);

        let gain = block_headroom_gain(1.0, limiter.knee_start());
        assert!((gain - DSD_LIMITER_KNEE_RATIO * 0.5).abs() < f64::EPSILON);

        let samples = [0.25, -0.5, 1.0];
        let conditioned: Vec<f64> = samples.iter().map(|sample| sample * gain).collect();
        assert!(
            conditioned
                .iter()
                .all(|sample| !limiter.knee_touched(*sample)),
            "block rider should keep finite samples below the per-sample limiter knee"
        );
        assert!((conditioned[0] / conditioned[2] - samples[0] / samples[2]).abs() < f64::EPSILON);
        assert!((conditioned[1] / conditioned[2] - samples[1] / samples[2]).abs() < f64::EPSILON);
    }

    #[test]
    fn dsd_block_headroom_rider_preserves_existing_limiter_telemetry() {
        if !CALIBRATED {
            return;
        }
        let mut renderer = DsdRenderer::new_with_modulator_mode(
            FilterType::SincExtreme32k,
            44_100,
            DsdRate::Dsd128,
            ModulatorMode::Standard,
        )
        .unwrap();
        let input_peak = renderer.coeffs.input_peak;
        renderer.pcm_scratch = vec![0.5, -1.0, 2.0, -0.25];

        assert!(!renderer.modulate(1.0));
        let telemetry = renderer.limiter_telemetry();
        assert!((telemetry.current_block_peak_ratio - 2.0).abs() < 1e-6);
        assert_eq!(telemetry.current_block_limited_samples, 2);
        assert_eq!(telemetry.limited_events, 1);
        assert_eq!(telemetry.limited_samples, 2);

        renderer.collect_in_flight_into_bits();
        assert_eq!(renderer.pcm_l.len(), 2);
        assert_eq!(renderer.pcm_r.len(), 2);
        assert!((renderer.pcm_l[0] - input_peak * 0.5 * 0.475).abs() < 1e-12);
        assert!((renderer.pcm_l[1] - input_peak * 2.0 * 0.475).abs() < 1e-12);
        assert!((renderer.pcm_r[0] + input_peak * 1.0 * 0.475).abs() < 1e-12);
        assert!((renderer.pcm_r[1] + input_peak * 0.25 * 0.475).abs() < 1e-12);
    }

    #[test]
    fn dsd_source_conditioner_only_scrubs_nonfinite_samples() {
        assert_eq!(source_abs_peak(&[0.0, -0.5, 1.25]), 1.25);
        assert!(
            !source_needs_sanitize(&[0.0, -1.25, 1.25]),
            "finite overs are handled later by the sample-wise modulator limiter"
        );
        assert_eq!(source_abs_peak(&[0.25, f64::NAN]), 0.25);
        assert!(source_needs_sanitize(&[0.0, f64::NAN]));
        assert!(source_needs_sanitize(&[f64::INFINITY]));

        assert_eq!(sanitize_source_sample(f64::NAN), 0.0);
        assert_eq!(sanitize_source_sample(f64::NEG_INFINITY), 0.0);
        assert_eq!(sanitize_source_sample(0.25), 0.25);

        let mut scratch = Vec::new();
        fill_sanitized_source_scratch(&mut scratch, &[0.5, f64::NAN, f64::INFINITY, -0.25]);
        let capacity = scratch.capacity();
        assert_eq!(scratch, vec![0.5, 0.0, 0.0, -0.25]);

        fill_sanitized_source_scratch(&mut scratch, &[1.25]);
        assert_eq!(scratch, vec![1.25]);
        assert!(
            scratch.capacity() >= capacity,
            "sanitizer scratch should be reusable across blocks"
        );
    }

    #[test]
    fn current_bits_are_truncated_to_equal_lengths_before_packing() {
        let mut renderer =
            DsdRenderer::new(FilterType::SincExtreme32k, 44_100, DsdRate::Dsd128).unwrap();
        renderer.bits_l = vec![1; 48];
        renderer.bits_r = vec![0; 32];

        assert!(renderer.truncate_current_bits_to_equal_len());
        assert_eq!(renderer.bits_l.len(), 32);
        assert_eq!(renderer.bits_r.len(), 32);
        let telemetry = renderer.truncation_telemetry();
        assert_eq!(telemetry.events, 1);
        assert_eq!(telemetry.discarded_left_bits, 16);
        assert_eq!(telemetry.discarded_right_bits, 0);
        assert_eq!(telemetry.last_left_len, 48);
        assert_eq!(telemetry.last_right_len, 32);
        assert_eq!(telemetry.last_kept_len, 32);

        renderer.bits_l.clear();
        renderer.bits_r = vec![0; 16];
        assert!(!renderer.truncate_current_bits_to_equal_len());
        assert!(renderer.bits_l.is_empty());
        assert!(renderer.bits_r.is_empty());
        let telemetry = renderer.truncation_telemetry();
        assert_eq!(telemetry.events, 2);
        assert_eq!(telemetry.discarded_left_bits, 16);
        assert_eq!(telemetry.discarded_right_bits, 16);
        assert_eq!(telemetry.last_left_len, 0);
        assert_eq!(telemetry.last_right_len, 16);
        assert_eq!(telemetry.last_kept_len, 0);
    }

    #[test]
    fn forced_44k_family_dsd128_uses_cross_family_chain() {
        for (dsd_rate, target_rate) in [
            (DsdRate::Dsd128, DsdRate::Dsd128.wire_rate_44k_family()),
            (DsdRate::Dsd256, DsdRate::Dsd256.wire_rate_44k_family()),
        ] {
            for source_rate in [48_000, 96_000, 192_000] {
                let upsampler = DsdUpsampler::new(FilterType::Minimum16k, source_rate, target_rate);
                let DsdUpsampler::CrossFamily(chain) = upsampler else {
                    panic!(
                        "48-family {source_rate} Hz forced to 44.1-family {dsd_rate:?} \
                     should use cross-family chain, not capped polyphase"
                    );
                };
                assert_eq!(chain.target_rate(), target_rate);
                assert_eq!(
                    chain.stage3.source_rate(),
                    CrossFamilyDsdChain::HOP_RATE_44K
                );
            }
        }
    }

    #[test]
    fn dsd256_44k_family_uses_direct_integer_chain() {
        for source_rate in [44_100, 88_200, 176_400] {
            let target_rate = DsdRate::Dsd256
                .wire_rate_for_source(source_rate)
                .expect("44.1-family DSD256 target");
            let upsampler = DsdUpsampler::new(FilterType::Split128k, source_rate, target_rate);
            let DsdUpsampler::Direct(resampler) = upsampler else {
                panic!("44.1-family {source_rate} Hz to DSD256 should use direct integer cascade");
            };
            assert_eq!(resampler.source_rate(), source_rate);
            assert_eq!(resampler.target_rate(), target_rate);
        }
    }

    #[test]
    fn dsd256_pre_modulator_pcm_preserves_filter_identity() {
        let source_rate = 44_100;
        let wire_rate = DsdRate::Dsd256
            .wire_rate_for_source(source_rate)
            .expect("DSD256 wire rate");
        let tone_hz = 18_000.0;
        let frames = 4096;
        let input: Vec<f64> = (0..frames)
            .map(|i| {
                0.25 * (2.0 * std::f64::consts::PI * tone_hz * i as f64 / source_rate as f64).sin()
            })
            .collect();

        let split = capture_upsampled_left_for_test(FilterType::Split128k, &input, DsdRate::Dsd256);
        let extreme =
            capture_upsampled_left_for_test(FilterType::SincExtreme32k, &input, DsdRate::Dsd256);

        let split_amp = projected_tone_amplitude(&split, wire_rate, tone_hz);
        let extreme_amp = projected_tone_amplitude(&extreme, wire_rate, tone_hz);
        let split_vs_extreme = rms_difference(&split, &extreme);

        assert!(
            split_amp > 0.01 && extreme_amp > 0.01,
            "DSD256 pre-modulator route should preserve the 18 kHz tone: split={split_amp}, extreme={extreme_amp}"
        );
        assert!(
            split_vs_extreme > 1.0e-8,
            "Split128k and SincExtreme32k pre-modulator PCM should not collapse to identical output"
        );
    }

    fn capture_upsampled_left_for_test(
        filter: FilterType,
        input: &[f64],
        dsd_rate: DsdRate,
    ) -> Vec<f64> {
        let mut renderer =
            DsdRenderer::new_with_modulator_mode(filter, 44_100, dsd_rate, ModulatorMode::Standard)
                .expect("renderer");
        let mut out = Vec::new();
        for chunk in input.chunks(1024) {
            let block = renderer.upsample(chunk, chunk);
            out.extend(block.chunks_exact(2).map(|frame| frame[0]));
        }
        let tail = renderer.drain_resampler_eof();
        out.extend(tail.chunks_exact(2).map(|frame| frame[0]));
        out
    }

    fn projected_tone_amplitude(samples: &[f64], sample_rate: u32, tone_hz: f64) -> f64 {
        if samples.is_empty() {
            return 0.0;
        }
        let skip = samples.len() / 4;
        let input = &samples[skip..];
        if input.is_empty() {
            return 0.0;
        }
        let mut re = 0.0;
        let mut im = 0.0;
        let mut window_sum = 0.0;
        for (idx, sample) in input.iter().enumerate() {
            let window = 0.5
                - 0.5
                    * (2.0 * std::f64::consts::PI * idx as f64 / (input.len() - 1).max(1) as f64)
                        .cos();
            let phase = 2.0 * std::f64::consts::PI * tone_hz * idx as f64 / sample_rate as f64;
            re += sample * window * phase.cos();
            im -= sample * window * phase.sin();
            window_sum += window;
        }
        2.0 * (re * re + im * im).sqrt() / window_sum.max(f64::MIN_POSITIVE)
    }

    fn rms_difference(left: &[f64], right: &[f64]) -> f64 {
        let len = left.len().min(right.len());
        if len == 0 {
            return 0.0;
        }
        let skip = len / 4;
        let len = len - skip;
        if len == 0 {
            return 0.0;
        }
        let sum_sq: f64 = left[skip..skip + len]
            .iter()
            .zip(&right[skip..skip + len])
            .map(|(a, b)| {
                let diff = a - b;
                diff * diff
            })
            .sum();
        (sum_sq / len as f64).sqrt()
    }

    #[test]
    fn dsd_upsampler_drain_eof_reaches_nominal_frame_count() {
        fn nominal_frames(input_frames: usize, source_rate: u32, target_rate: u32) -> usize {
            let numerator = (input_frames as u128) * (target_rate as u128);
            numerator.div_ceil(source_rate as u128) as usize
        }
        fn expected_dsd_upsampler_frames(
            input_frames: usize,
            source_rate: u32,
            target_rate: u32,
        ) -> usize {
            if (target_rate == DsdRate::Dsd128.wire_rate_44k_family()
                || target_rate == DsdRate::Dsd256.wire_rate_44k_family())
                && matches!(source_rate, 48_000 | 96_000 | 192_000)
            {
                let hop48_frames = if source_rate == CrossFamilyDsdChain::HOP_RATE_48K {
                    input_frames
                } else {
                    nominal_frames(input_frames, source_rate, CrossFamilyDsdChain::HOP_RATE_48K)
                };
                let hop44_frames = nominal_frames(
                    hop48_frames,
                    CrossFamilyDsdChain::HOP_RATE_48K,
                    CrossFamilyDsdChain::HOP_RATE_44K,
                );
                nominal_frames(hop44_frames, CrossFamilyDsdChain::HOP_RATE_44K, target_rate)
            } else {
                nominal_frames(input_frames, source_rate, target_rate)
            }
        }

        for (source_rate, target_rate) in [
            (44_100, DsdRate::Dsd128.wire_rate_44k_family()),
            (48_000, DsdRate::Dsd128.wire_rate_44k_family()),
            (44_100, DsdRate::Dsd256.wire_rate_44k_family()),
            (48_000, DsdRate::Dsd256.wire_rate_44k_family()),
        ] {
            let frames = 1024usize;
            let input: Vec<f64> = (0..frames)
                .map(|i| {
                    0.2 * (2.0 * std::f64::consts::PI * 997.0 * i as f64 / source_rate as f64).sin()
                })
                .collect();
            let mut upsampler =
                DsdUpsampler::new(FilterType::SincExtreme32k, source_rate, target_rate);
            upsampler.input(&input, &input);

            let mut output = Vec::new();
            upsampler.process(&mut output);
            upsampler.drain_eof(&mut output);
            assert_eq!(
                output.len() / 2,
                expected_dsd_upsampler_frames(frames, source_rate, target_rate),
                "{source_rate}->{target_rate} DSD upsampler EOF drain did not reach nominal length"
            );

            let before_second_drain = output.len();
            assert_eq!(upsampler.drain_eof(&mut output), 0);
            assert_eq!(output.len(), before_second_drain);
        }
    }

    #[test]
    fn renderer_produces_planar_native_dsd128_bytes() {
        if !CALIBRATED {
            return;
        }
        let mut renderer =
            DsdRenderer::new(FilterType::Minimum16k, 48_000, DsdRate::Dsd128).unwrap();
        let input = vec![0.0; 8192];
        renderer.upsample(&input, &input);
        let mut l = Vec::new();
        let mut r = Vec::new();
        renderer.modulate_and_pack_native(1.0, &mut l, &mut r);
        renderer.flush_modulators_and_pack_native(&mut l, &mut r);
        assert!(!l.is_empty());
        assert_eq!(l.len(), r.len());
    }

    #[test]
    fn eos_flush_emits_ec_lookahead_tail_with_equal_channel_lengths() {
        if !CALIBRATED {
            return;
        }
        let mut renderer = DsdRenderer::new_with_modulator_mode(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd128,
            ModulatorMode::Ec,
        )
        .unwrap();
        let input: Vec<f64> = (0..8192)
            .map(|i| 0.3 * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 44_100.0).sin())
            .collect();
        let mut out = Vec::new();
        renderer.render(&input, &input, &mut out);

        // The flush emits the pipelined block plus the EC lookahead tail.
        assert!(
            renderer.flush_modulators(),
            "EC flush should emit the held block and tail"
        );
        assert!(!renderer.bits_l.is_empty());
        assert_eq!(renderer.bits_l.len(), renderer.bits_r.len());

        // Second flush is a no-op: block and tail were consumed.
        assert!(!renderer.flush_modulators());
    }

    #[test]
    fn eos_flush_packs_through_dop_and_native_packers() {
        if !CALIBRATED {
            return;
        }
        let mut renderer =
            DsdRenderer::new(FilterType::Minimum16k, 44_100, DsdRate::Dsd128).unwrap();
        let input = vec![0.1; 8192];
        let mut out = Vec::new();
        renderer.render(&input, &input, &mut out);
        let before = out.len();
        renderer.flush_modulators_and_pack(&mut out);
        // The tail is shorter than a 16-bit DoP frame for the default lookahead
        // depth, so it may stay buffered in the packer — but the output must stay
        // interleaved-stereo aligned and never shrink.
        assert!(out.len() >= before);
        assert_eq!(out.len() % 2, 0);

        let mut renderer =
            DsdRenderer::new(FilterType::Minimum16k, 44_100, DsdRate::Dsd128).unwrap();
        renderer.upsample(&input, &input);
        let mut l = Vec::new();
        let mut r = Vec::new();
        renderer.modulate_and_pack_native(1.0, &mut l, &mut r);
        renderer.flush_modulators_and_pack_native(&mut l, &mut r);
        renderer.flush_native_with_idle(&mut l, &mut r);
        assert_eq!(l.len(), r.len());
    }

    #[test]
    fn standard_mode_flush_emits_only_the_pipelined_block() {
        if !CALIBRATED {
            return;
        }
        let mut renderer = DsdRenderer::new_with_modulator_mode(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd128,
            ModulatorMode::Standard,
        )
        .unwrap();
        let input = vec![0.1; 8192];
        let mut out = Vec::new();
        renderer.render(&input, &input, &mut out);
        // First flush returns the block the pipeline was holding (Standard mode
        // adds no lookahead tail of its own)…
        assert!(renderer.flush_modulators());
        assert_eq!(renderer.bits_l.len(), renderer.bits_r.len());
        // …and a second flush has nothing left to emit.
        assert!(!renderer.flush_modulators());
        let before = out.len();
        renderer.flush_modulators_and_pack(&mut out);
        assert_eq!(out.len(), before);
    }

    fn ecbeam2_test_input(source_rate: u32, frames: usize) -> Vec<f64> {
        (0..frames)
            .map(|index| {
                let phase = 2.0 * std::f64::consts::PI * 997.0 * index as f64 / source_rate as f64;
                0.05 * phase.sin()
            })
            .collect()
    }

    fn render_ecbeam2_native_pass(
        renderer: &mut DsdRenderer,
        input: &[f64],
        chunk_frames: usize,
    ) -> (Vec<u8>, Vec<u8>) {
        let mut left = Vec::new();
        let mut right = Vec::new();
        for chunk in input.chunks(chunk_frames.max(1)) {
            renderer.upsample(chunk, chunk);
            renderer.modulate_and_pack_native(1.0, &mut left, &mut right);
        }
        renderer.drain_resampler_eof();
        renderer.modulate_and_pack_native(1.0, &mut left, &mut right);
        renderer.flush_modulators_and_pack_native(&mut left, &mut right);
        renderer.flush_native_with_idle(&mut left, &mut right);
        (left, right)
    }

    fn render_ecbeam2_native(
        source_rate: u32,
        input: &[f64],
        chunk_frames: usize,
    ) -> (Vec<u8>, Vec<u8>, [EcBeam2Diagnostics; 2]) {
        let mut renderer = DsdRenderer::new_with_dsd_modulator(
            FilterType::Minimum16k,
            source_rate,
            DsdRate::Dsd64,
            DsdModulator::EcBeam2,
        )
        .expect("EcBeam2 DSD64 renderer");
        renderer.set_native_order(NativeDsdOrder::MsbFirst);
        let (left, right) = render_ecbeam2_native_pass(&mut renderer, input, chunk_frames);
        let [Some(left_diagnostics), Some(right_diagnostics)] = renderer.ecbeam2_diagnostics()
        else {
            panic!("EcBeam2 renderer did not surface per-channel diagnostics");
        };
        (left, right, [left_diagnostics, right_diagnostics])
    }

    #[test]
    fn ecbeam2_accepts_both_dsd64_wire_families_and_reports_complete_output() {
        if !CALIBRATED {
            return;
        }
        const SOURCE_FRAMES: usize = 33;
        for (source_rate, dop_rate) in [(44_100, 176_400), (48_000, 192_000)] {
            let input = ecbeam2_test_input(source_rate, SOURCE_FRAMES);
            let (left, right, diagnostics) =
                render_ecbeam2_native(source_rate, &input, SOURCE_FRAMES);
            let expected_bits = (SOURCE_FRAMES * 64) as u64;
            let expected_bytes = expected_bits.div_ceil(8) as usize;

            assert_eq!(left.len(), expected_bytes, "{source_rate} Hz output length");
            assert_eq!(
                right.len(),
                expected_bytes,
                "{source_rate} Hz output length"
            );
            for channel in diagnostics {
                assert_eq!(channel.committed_samples, expected_bits);
                assert_eq!(channel.committed_sequence, expected_bits);
                assert!(channel.positive_bits <= channel.committed_samples);
                assert_eq!(channel.all_nonfinite_resets, 0);
                assert_eq!(channel.constraint_escape, 0);
                assert_eq!(channel.output_length_events, 0);
                assert!(channel.committed_output_energy.is_finite());
                assert!(channel.committed_tail_adjusted_energy.is_finite());
                assert!(channel.remaining_tail_energy.is_finite());
            }

            let renderer = DsdRenderer::new_with_dsd_modulator(
                FilterType::Minimum16k,
                source_rate,
                DsdRate::Dsd64,
                DsdModulator::EcBeam2,
            )
            .expect("EcBeam2 DSD64 renderer");
            assert_eq!(renderer.dop_frame_rate(), dop_rate);
            assert_eq!(renderer.dsd_modulator(), DsdModulator::EcBeam2);
            assert_eq!(renderer.modulator_mode(), ModulatorMode::Ec);
        }
    }

    #[test]
    fn ecbeam2_smooth_phase_dsd64_matches_full_scalar_on_both_wire_families() {
        if !CALIBRATED {
            return;
        }
        const SOURCE_FRAMES: usize = 17;
        let config = ecbeam2_production_config();
        for source_rate in [44_100, 48_000] {
            let input = ecbeam2_test_input(source_rate, SOURCE_FRAMES);
            let mut optimized = DsdRenderer::new_with_dsd_modulator(
                FilterType::SmoothPhase128k,
                source_rate,
                DsdRate::Dsd64,
                DsdModulator::EcBeam2,
            )
            .expect("optimized Smooth Phase EcBeam2 DSD64 renderer");
            optimized.set_native_order(NativeDsdOrder::MsbFirst);
            let (optimized_left, optimized_right) =
                render_ecbeam2_native_pass(&mut optimized, &input, 5);

            let mut reference = DsdRenderer::new_with_dsd_modulator_and_experiment_tweaks(
                FilterType::SmoothPhase128k,
                source_rate,
                DsdRate::Dsd64,
                DsdModulator::EcBeam2,
                None,
                DsdExperimentTweaks {
                    ecbeam2_config: Some(config),
                    ecbeam2_full_diagnostics: Some(true),
                    ..DsdExperimentTweaks::default()
                },
            )
            .expect("full scalar Smooth Phase EcBeam2 DSD64 reference renderer");
            reference.set_native_order(NativeDsdOrder::MsbFirst);
            let (reference_left, reference_right) =
                render_ecbeam2_native_pass(&mut reference, &input, 5);

            assert_eq!(optimized_left, reference_left, "{source_rate} Hz left");
            assert_eq!(optimized_right, reference_right, "{source_rate} Hz right");
            assert_eq!(optimized.stability_resets(), 0);
            assert_eq!(optimized.state_clamps(), 0);
            for diagnostics in optimized.ecbeam2_diagnostics() {
                let diagnostics = diagnostics.expect("per-channel EcBeam2 diagnostics");
                assert_eq!(diagnostics.output_length_events, 0);
                assert_eq!(diagnostics.all_nonfinite_resets, 0);
                assert_eq!(diagnostics.constraint_escape, 0);
            }
        }
    }

    #[test]
    fn production_ecbeam2_renders_both_dsd128_wire_families() {
        if !CALIBRATED {
            return;
        }
        const SOURCE_FRAMES: usize = 17;
        let config = ecbeam2_production_config();
        let assert_channel = |reference: &[u8],
                              candidate: &[u8],
                              filter_type: FilterType,
                              source_rate: u32,
                              channel: &str| {
            if let Some(index) = reference
                .iter()
                .zip(candidate)
                .position(|(left, right)| left != right)
            {
                panic!(
                    "{} {source_rate} Hz {channel}: first differing packed byte {index}: reference={:#04x} candidate={:#04x}",
                    filter_type.as_name(),
                    reference[index],
                    candidate[index]
                );
            }
            assert_eq!(
                reference.len(),
                candidate.len(),
                "{} {source_rate} Hz {channel}: packed length",
                filter_type.as_name()
            );
        };
        for filter_type in [
            FilterType::Minimum16k,
            FilterType::Split128k,
            FilterType::SmoothPhase128k,
        ] {
            for source_rate in [44_100, 48_000] {
                let input = ecbeam2_test_input(source_rate, SOURCE_FRAMES);
                let mut renderer = DsdRenderer::new_with_dsd_modulator(
                    filter_type,
                    source_rate,
                    DsdRate::Dsd128,
                    DsdModulator::EcBeam2,
                )
                .expect("production EcBeam2 DSD128 renderer");
                assert_eq!(
                    renderer.coefficient_table_name(),
                    "ECBEAM2_OSR128_OBG164_INPUT468_V1"
                );
                assert_eq!(
                    renderer
                        .effective_experiment_tweaks()
                        .ecbeam2_full_diagnostics,
                    Some(false)
                );
                renderer.set_native_order(NativeDsdOrder::MsbFirst);
                let (left, right) = render_ecbeam2_native_pass(&mut renderer, &input, 5);
                let expected_bits = (SOURCE_FRAMES * 128) as u64;
                let expected_bytes = expected_bits.div_ceil(8) as usize;
                assert_eq!(left.len(), expected_bytes, "{source_rate} Hz left output");
                assert_eq!(right.len(), expected_bytes, "{source_rate} Hz right output");

                let mut reference = DsdRenderer::new_with_dsd_modulator_and_experiment_tweaks(
                    filter_type,
                    source_rate,
                    DsdRate::Dsd128,
                    DsdModulator::EcBeam2,
                    None,
                    DsdExperimentTweaks {
                        ecbeam2_config: Some(config),
                        ecbeam2_full_diagnostics: Some(true),
                        ..DsdExperimentTweaks::default()
                    },
                )
                .expect("full scalar EcBeam2 DSD128 reference renderer");
                reference.set_native_order(NativeDsdOrder::MsbFirst);
                let (reference_left, reference_right) =
                    render_ecbeam2_native_pass(&mut reference, &input, 5);
                assert_channel(&reference_left, &left, filter_type, source_rate, "left");
                assert_channel(&reference_right, &right, filter_type, source_rate, "right");

                for diagnostics in renderer.ecbeam2_diagnostics() {
                    let diagnostics = diagnostics.expect("per-channel EcBeam2 diagnostics");
                    assert_eq!(diagnostics.committed_samples, expected_bits);
                    assert_eq!(diagnostics.committed_sequence, expected_bits);
                    assert_eq!(diagnostics.all_nonfinite_resets, 0);
                    assert_eq!(diagnostics.constraint_escape, 0);
                    assert_eq!(diagnostics.output_length_events, 0);
                }
            }
        }
    }

    #[test]
    fn ecbeam2_selects_internal_rate_policy_without_fallback() {
        if !CALIBRATED {
            return;
        }
        let config = ecbeam2_production_config();
        let renderer = DsdRenderer::new_with_dsd_modulator_and_experiment_tweaks(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd128,
            DsdModulator::EcBeam2,
            None,
            DsdExperimentTweaks {
                ecbeam2_config: Some(config),
                ..DsdExperimentTweaks::default()
            },
        )
        .expect("production EcBeam2 DSD128 renderer");
        assert_eq!(
            renderer.coefficient_table_name(),
            "ECBEAM2_OSR128_OBG164_INPUT468_V1"
        );
        assert_eq!(renderer.coefficient_osr(), 128);
        assert_eq!(renderer.coefficient_obg(), 1.64);
        assert_eq!(renderer.modulator_input_peak(), 0.467_858_988_519_470_7);

        let production = DsdRenderer::new_with_dsd_modulator(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd128,
            DsdModulator::EcBeam2,
        )
        .expect("normal EcBeam2 DSD128 renderer");
        assert_eq!(
            production.coefficient_table_name(),
            "ECBEAM2_OSR128_OBG164_INPUT468_V1"
        );
        assert_eq!(production.coefficient_osr(), 128);
        assert_eq!(production.modulator_input_peak(), 0.467_858_988_519_470_7);
        assert_eq!(
            production.effective_experiment_tweaks().ecbeam2_config,
            Some(config)
        );
        assert_eq!(
            production
                .effective_experiment_tweaks()
                .ecbeam2_full_diagnostics,
            Some(false)
        );

        let dsd64 = DsdRenderer::new_with_dsd_modulator_and_experiment_tweaks(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd64,
            DsdModulator::EcBeam2,
            None,
            DsdExperimentTweaks {
                ecbeam2_config: Some(config),
                ..DsdExperimentTweaks::default()
            },
        )
        .expect("DSD64 EcBeam2 renderer must select its internal rate policy");
        assert_eq!(
            dsd64.coefficient_table_name(),
            "ECBEAM2_OSR64_OBG164_INPUT468_V1"
        );
        assert_eq!(dsd64.coefficient_osr(), 64);
        assert_eq!(dsd64.modulator_input_peak(), 0.467_858_988_519_470_7);

        let dsd256 = DsdRenderer::new_with_dsd_modulator_and_experiment_tweaks(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd256,
            DsdModulator::EcBeam2,
            None,
            DsdExperimentTweaks {
                ecbeam2_config: Some(config),
                ..DsdExperimentTweaks::default()
            },
        )
        .expect("DSD256 EcBeam2 renderer must select its OSR256/OBG1.64 policy");
        assert_eq!(
            dsd256.coefficient_table_name(),
            "ECBEAM2_OSR256_OBG164_INPUT468_V1"
        );
        assert_eq!(dsd256.coefficient_osr(), 256);
        assert_eq!(dsd256.coefficient_obg(), 1.64);
        assert_eq!(dsd256.modulator_input_peak(), 0.467_858_988_519_470_7);

        let error = DsdRenderer::new_with_dsd_modulator(
            FilterType::SincExtreme32k,
            44_100,
            DsdRate::Dsd64,
            DsdModulator::EcBeam2,
        )
        .err();
        assert_eq!(
            error,
            Some("7th Order Search supports only the four selectable 128k filters")
        );

        let error = DsdRenderer::new_with_dsd_modulator_and_experiment_tweaks(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd64,
            DsdModulator::EcBeam2,
            None,
            DsdExperimentTweaks {
                ecbeam2_config: Some(EcBeam2ExperimentConfig {
                    quantizer_regularizer: -1.0,
                    ..EcBeam2ExperimentConfig::default()
                }),
                ..DsdExperimentTweaks::default()
            },
        )
        .err();
        assert_eq!(
            error,
            Some("EcBeam2 quantizer regularizer must be finite and non-negative")
        );

        let error = DsdRenderer::new_with_dsd_modulator_and_experiment_tweaks(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd64,
            DsdModulator::EcBeam2,
            None,
            DsdExperimentTweaks {
                ecbeam2_config: Some(EcBeam2ExperimentConfig {
                    quantizer_regularizer: f64::from_bits(
                        EcBeam2ExperimentConfig::MAX_QUANTIZER_REGULARIZER.to_bits() + 1,
                    ),
                    ..EcBeam2ExperimentConfig::default()
                }),
                ..DsdExperimentTweaks::default()
            },
        )
        .err();
        assert_eq!(
            error,
            Some("EcBeam2 quantizer regularizer must be finite and between 0 and 4")
        );

        let error = DsdRenderer::new_with_dsd_modulator_and_isi_penalty(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd64,
            DsdModulator::EcBeam2,
            0.001,
        )
        .err();
        assert_eq!(error, Some("EcBeam2 requires zero ISI compensation"));

        for invalid in [-0.001, f64::NAN, f64::INFINITY] {
            let error = DsdRenderer::new_with_dsd_modulator_and_isi_penalty(
                FilterType::Minimum16k,
                44_100,
                DsdRate::Dsd64,
                DsdModulator::EcBeam2,
                invalid,
            )
            .err();
            assert_eq!(error, Some("EcBeam2 requires zero ISI compensation"));
        }
    }

    #[test]
    fn ecbeam2_renderer_is_chunk_invariant_across_flush() {
        if !CALIBRATED {
            return;
        }
        let input = ecbeam2_test_input(44_100, 37);
        let (whole_left, whole_right, whole_diagnostics) =
            render_ecbeam2_native(44_100, &input, input.len());
        let (chunked_left, chunked_right, chunked_diagnostics) =
            render_ecbeam2_native(44_100, &input, 5);

        assert_eq!(chunked_left, whole_left);
        assert_eq!(chunked_right, whole_right);
        assert_eq!(chunked_diagnostics, whole_diagnostics);
    }

    #[test]
    fn ecbeam2_renderer_reset_restarts_state_and_preserves_output_length() {
        if !CALIBRATED {
            return;
        }
        let input = ecbeam2_test_input(48_000, 35);
        let mut renderer = DsdRenderer::new_with_dsd_modulator(
            FilterType::Minimum16k,
            48_000,
            DsdRate::Dsd64,
            DsdModulator::EcBeam2,
        )
        .expect("EcBeam2 renderer");
        renderer.set_native_order(NativeDsdOrder::MsbFirst);

        let first = render_ecbeam2_native_pass(&mut renderer, &input, 7);
        let [Some(first_left), Some(first_right)] = renderer.ecbeam2_diagnostics() else {
            panic!("missing first-pass EcBeam2 diagnostics");
        };
        renderer.reset();
        assert_eq!(renderer.ecbeam2_diagnostics(), [None, None]);
        let second = render_ecbeam2_native_pass(&mut renderer, &input, 7);
        let [Some(second_left), Some(second_right)] = renderer.ecbeam2_diagnostics() else {
            panic!("missing second-pass EcBeam2 diagnostics");
        };

        assert_eq!(second, first);
        assert_eq!(second.0.len(), input.len() * 8);
        assert_eq!(second.0.len(), second.1.len());
        assert_eq!(
            second_left.committed_state_epoch,
            first_left.committed_state_epoch + 1
        );
        assert_eq!(
            second_right.committed_state_epoch,
            first_right.committed_state_epoch + 1
        );
        assert_eq!(first_left.committed_samples, (input.len() * 64) as u64);
        assert_eq!(first_right.committed_samples, (input.len() * 64) as u64);
        assert_eq!(second_left.committed_samples, (input.len() * 64) as u64);
        assert_eq!(second_right.committed_samples, (input.len() * 64) as u64);
        assert_eq!(second_left.output_length_events, 0);
        assert_eq!(second_right.output_length_events, 0);
        assert!(second_left.maximum_segment_identity_error < 1.0e-8);
        assert!(second_right.maximum_segment_identity_error < 1.0e-8);
    }

    fn ecbeam2_peak_ratio_for_input_gain(input_gain: f64) -> f32 {
        let input = ecbeam2_test_input(44_100, 33);
        let mut renderer = DsdRenderer::new_with_dsd_modulator(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd64,
            DsdModulator::EcBeam2,
        )
        .expect("EcBeam2 renderer");
        renderer.upsample(&input, &input);
        renderer.modulate(input_gain);
        renderer.drain_resampler_eof();
        renderer.modulate(input_gain);
        renderer.flush_modulators();
        renderer.limiter_telemetry().peak_ratio_max
    }

    #[test]
    fn ecbeam2_renderer_enforces_minus_two_db_as_a_gain_ceiling() {
        if !CALIBRATED {
            return;
        }
        let at_unity = ecbeam2_peak_ratio_for_input_gain(1.0);
        let boosted = ecbeam2_peak_ratio_for_input_gain(2.0);
        let exactly_minus_two = ecbeam2_peak_ratio_for_input_gain(
            10.0f64.powf(DSD64_ECBEAM2_REQUIRED_HEADROOM_DB / 20.0),
        );
        let quieter = ecbeam2_peak_ratio_for_input_gain(10.0f64.powf(-6.0 / 20.0));

        assert!(at_unity > 0.0);
        assert!((boosted - at_unity).abs() <= 1.0e-6);
        assert!((exactly_minus_two - at_unity).abs() <= 1.0e-6);
        let expected_quieter_ratio = 10.0f32.powf(-4.0 / 20.0);
        assert!((quieter / at_unity - expected_quieter_ratio).abs() <= 1.0e-5);
    }

    #[test]
    fn ecbeam2_and_production_ecbeam_keep_diagnostics_and_tweaks_isolated() {
        if !CALIBRATED {
            return;
        }
        let ecbeam2_defaults = DsdExperimentTweaks::default().with_production_policy_defaults(
            FilterType::Minimum16k,
            DsdRate::Dsd64,
            DsdModulator::EcBeam2,
        );
        assert_eq!(ecbeam2_defaults.ec_beam_search, None);
        assert_eq!(ecbeam2_defaults.ec_beam_terminal_weight, None);
        assert_eq!(ecbeam2_defaults.ec_beam_reconstruction_error_weight, None);

        let input = ecbeam2_test_input(44_100, 17);
        let mut production = DsdRenderer::new_with_dsd_modulator(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd64,
            DsdModulator::EcBeam,
        )
        .expect("production EcBeam renderer");
        let mut production_left = Vec::new();
        let mut production_right = Vec::new();
        production.upsample(&input, &input);
        production.modulate_and_pack_native(1.0, &mut production_left, &mut production_right);
        production.drain_resampler_eof();
        production.modulate_and_pack_native(1.0, &mut production_left, &mut production_right);
        production.flush_modulators_and_pack_native(&mut production_left, &mut production_right);
        assert_eq!(production.ecbeam2_diagnostics(), [None, None]);
        assert!(production.beam_diagnostics().iter().all(Option::is_some));

        let mut ecbeam2 = DsdRenderer::new_with_dsd_modulator(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd64,
            DsdModulator::EcBeam2,
        )
        .expect("EcBeam2 renderer");
        let mut ecbeam2_left = Vec::new();
        let mut ecbeam2_right = Vec::new();
        ecbeam2.upsample(&input, &input);
        ecbeam2.modulate_and_pack_native(1.0, &mut ecbeam2_left, &mut ecbeam2_right);
        ecbeam2.drain_resampler_eof();
        ecbeam2.modulate_and_pack_native(1.0, &mut ecbeam2_left, &mut ecbeam2_right);
        ecbeam2.flush_modulators();
        assert!(ecbeam2.ecbeam2_diagnostics().iter().all(Option::is_some));
        assert_eq!(ecbeam2.beam_diagnostics(), [None, None]);
        assert_eq!(ecbeam2.beam_reconstruction_diagnostics(), [None, None]);
        assert_eq!(ecbeam2.beam_periodicity_diagnostics(), [None, None]);
    }

    #[cfg(not(feature = "ecbeam2_observer"))]
    #[test]
    fn shadow_a1_renderer_requires_the_non_default_observer_feature() {
        let result = DsdRenderer::new_with_dsd_modulator_and_experiment_tweaks(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd64,
            DsdModulator::EcBeam,
            None,
            DsdExperimentTweaks {
                ecbeam2_config: Some(EcBeam2ExperimentConfig {
                    run_mode: EcBeam2RunMode::ShadowA1,
                    ..EcBeam2ExperimentConfig::default()
                }),
                ..DsdExperimentTweaks::default()
            },
        );
        assert_eq!(
            result.err(),
            Some("EcBeam2 ShadowA1 requires building with the ecbeam2_observer feature")
        );
    }

    #[cfg(feature = "ecbeam2_observer")]
    #[test]
    fn shadow_a1_renderer_routes_observer_without_changing_production_bits() {
        if !CALIBRATED {
            return;
        }
        let input = ecbeam2_test_input(44_100, 41);
        let mut production = DsdRenderer::new_with_dsd_modulator(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd64,
            DsdModulator::EcBeam,
        )
        .unwrap();
        let mut shadow = DsdRenderer::new_with_dsd_modulator_and_experiment_tweaks(
            FilterType::Minimum16k,
            44_100,
            DsdRate::Dsd64,
            DsdModulator::EcBeam,
            None,
            DsdExperimentTweaks {
                ecbeam2_config: Some(EcBeam2ExperimentConfig {
                    run_mode: EcBeam2RunMode::ShadowA1,
                    diagnostic_window: Some(
                        crate::audio::dsd::delta_sigma::EcBeam2DiagnosticWindow {
                            start_sequence: 64,
                            end_sequence: 128,
                        },
                    ),
                    ..EcBeam2ExperimentConfig::default()
                }),
                ..DsdExperimentTweaks::default()
            },
        )
        .unwrap();
        production.set_native_order(NativeDsdOrder::MsbFirst);
        shadow.set_native_order(NativeDsdOrder::MsbFirst);
        let production_bits = render_ecbeam2_native_pass(&mut production, &input, 7);
        let shadow_bits = render_ecbeam2_native_pass(&mut shadow, &input, 7);
        assert_eq!(shadow_bits, production_bits);
        assert_eq!(production.ecbeam2_diagnostics(), [None, None]);
        let [Some(left), Some(right)] = shadow.ecbeam2_diagnostics() else {
            panic!("ShadowA1 diagnostics were not routed through the renderer");
        };
        assert!(left.committed_samples > 0 && right.committed_samples > 0);
        assert_eq!(left.diagnostic_window_samples, 64);
        assert_eq!(right.diagnostic_window_samples, 64);
        assert_eq!(left.a1_frontier_events, 64);
        assert_eq!(right.a1_frontier_events, 64);
        assert_eq!(left.best_fourth_margin_samples, 64);
        assert_eq!(right.a1_best_fourth_margin_samples, 64);
        assert!(left.maximum_ultrasonic_ema.is_finite());
        assert!(right.maximum_signed_error_ema.is_finite());
        assert!(shadow.beam_diagnostics().iter().all(Option::is_some));
    }

    #[test]
    fn wire_rate_is_family_locked() {
        // 44.1 family always lands on 5.6448 MHz regardless of source rate.
        assert_eq!(
            DsdRate::Dsd128.wire_rate_for_source(44_100),
            Some(5_644_800)
        );
        assert_eq!(
            DsdRate::Dsd128.wire_rate_for_source(88_200),
            Some(5_644_800)
        );
        assert_eq!(
            DsdRate::Dsd128.wire_rate_for_source(176_400),
            Some(5_644_800)
        );
        // 48 family.
        assert_eq!(
            DsdRate::Dsd128.wire_rate_for_source(48_000),
            Some(6_144_000)
        );
        assert_eq!(
            DsdRate::Dsd128.wire_rate_for_source(96_000),
            Some(6_144_000)
        );
        assert_eq!(
            DsdRate::Dsd128.wire_rate_for_source(192_000),
            Some(6_144_000)
        );
        // DSD256.
        assert_eq!(
            DsdRate::Dsd256.wire_rate_for_source(44_100),
            Some(11_289_600)
        );
        assert_eq!(
            DsdRate::Dsd256.wire_rate_for_source(48_000),
            Some(12_288_000)
        );
        // Out-of-family sources rejected (e.g. 22.05 kHz isn't a 44.1 multiple
        // that fits the cascade with a power-of-two ratio).
        assert_eq!(DsdRate::Dsd128.wire_rate_for_source(22_050), None);
        assert_eq!(DsdRate::Dsd128.wire_rate_for_source(64_000), None);
        // 384 kHz is in the 48 family; DSD128 still lands at 6.144 MHz (16× cascade).
        assert_eq!(
            DsdRate::Dsd128.wire_rate_for_source(384_000),
            Some(6_144_000)
        );
        // Sources at or above the wire rate are rejected (no upsampling needed/possible).
        assert_eq!(DsdRate::Dsd128.wire_rate_for_source(6_144_000), None);
        assert_eq!(DsdRate::Dsd128.wire_rate_for_source(0), None);
        // DoP frame rate is DSD rate / 16 — used as the WASAPI PCM rate.
        assert_eq!(DsdRate::dop_frame_rate(5_644_800), 352_800);
        assert_eq!(DsdRate::dop_frame_rate(11_289_600), 705_600);
    }
}
