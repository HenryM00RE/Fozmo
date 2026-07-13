use super::coeff_math::*;
use super::diagnostics::*;
use super::dither::*;
use super::ec_beam::*;
use super::stability::*;

use crate::audio::dsd::dsd_coeffs::{CALIBRATED, ModulatorCoeffs};

pub(super) const QUANTIZER_DITHER_SCALE: f64 = 1.0 / 256.0;

/// EC high-pass TPDF uses the same comparator step reference as the standard path,
/// but at a reduced level for lower idle-noise energy.
///
/// Dither shapes are variance-matched: at equal `dither_scale`, white and
/// high-pass TPDF inject equal total power, so shape A/Bs compare spectrum
/// only. The high-pass normalization used to preserve raw first-difference
/// power (2x white variance at the same scale); these multipliers carry the
/// compensating sqrt(2) so the shipped default's effective dither signal is
/// unchanged sample-for-sample.
pub const EC_DITHER_SCALE_MULTIPLIER: f64 = 0.375 * core::f64::consts::SQRT_2;
/// DSD128 EC runs closer to tonal idle/spur patterns at production headroom; a
/// slightly lower high-pass TPDF scale measured better across -3/-4/-5 dB.
/// Carries the same sqrt(2) variance-parity compensation as
/// [`EC_DITHER_SCALE_MULTIPLIER`].
pub const EC_DSD128_DITHER_SCALE_MULTIPLIER: f64 = 0.30 * core::f64::consts::SQRT_2;

/// Transition-loss estimate for a known lossy DAC profile. This is *not* applied by
/// default: pre-compensating a transition loss the playback DAC doesn't have would
/// inject a transition-density-correlated error instead of removing one. Set it via
/// [`CrfbModulator::set_isi_penalty`] from a DAC profile.
pub const DEFAULT_ISI_PENALTY: f64 = 0.01;

pub const EC_QUANTIZER_ERROR_WEIGHT: f64 = 0.80;
pub const EC_STATE_PRESSURE_WEIGHT: f64 = 2.50;
pub const EC_STATE_LIMIT_WEIGHT: f64 = 80.0;
pub const EC_TRANSITION_WEIGHT: f64 = 0.002;
pub const EC_DC_BIAS_WEIGHT: f64 = 0.04;
pub const EC_DC_BIAS_DECAY: f64 = 0.9995;
pub const EC_LOOKAHEAD_DISCOUNT: f64 = 0.6;
pub const EC_BEAM_FILTERED_ERROR_WEIGHT: f64 = 0.0;
pub const EC_BEAM_FILTERED_ERROR_RANK_WEIGHT: f64 = 0.0;
pub const EC_BEAM_RECONSTRUCTION_ERROR_WEIGHT: f64 = 0.0;
pub const EC_BEAM_PRESSURE_DEADZONE: f64 = 0.0;
pub const EC_BEAM_PERIODICITY_WEIGHT: f64 = 0.0;
pub const EC_BEAM_CLAMP_PENALTY_WEIGHT: f64 = EC_STATE_LIMIT_WEIGHT * 1_000.0;
pub(super) const EC_BEAM_PERIODICITY_DEFAULT_LAGS: [u8; MAX_BEAM_PERIODICITY_LAGS] = [2, 3, 4, 0];
pub(super) const EC_BEAM_PERIODICITY_DEFAULT_LAG_COUNT: usize = 3;
pub(super) const EC_BEAM_PERIODICITY_DEFAULT_WINDOW: usize = 16;
/// A depth-1 EC search has no future sample to validate a root decision. Keep
/// these weights separated for empirical sweeps; the shipped `EcDepth2` defaults
/// intentionally match the depth-2+ root scorer after measurement showed small
/// blind offsets can hurt worst-window behavior.
pub const EC_DEPTH1_STATE_PRESSURE_WEIGHT: f64 = EC_STATE_PRESSURE_WEIGHT;
pub const EC_DEPTH1_TRANSITION_WEIGHT: f64 = EC_TRANSITION_WEIGHT;
/// Default EC search depth: the committed bit plus one lookahead sample.
pub const DEFAULT_EC_LOOKAHEAD_DEPTH: usize = 2;
/// Cap on the EC search depth. Cost grows ~2^depth before pruning; published
/// trellis-shaping results show diminishing returns past ~depth 4–6.
pub const MAX_EC_LOOKAHEAD_DEPTH: usize = 8;
pub(super) const MAX_EC_FUTURE_DITHER: usize = MAX_EC_LOOKAHEAD_DEPTH - 1;

/// EcBeam prototype caps (docs/dev/7th-order-ecm-m-algorithm.md §21.1): beam width
/// `m` and delayed-commit horizon `n`. `n ≤ 48` keeps the packed per-survivor
/// bit history in a `u64`.
pub(super) const MAX_BEAM_WIDTH: usize = 16;
pub(super) const MAX_BEAM_COMMIT_HORIZON: usize = 48;
pub(super) const MAX_BEAM_PERIODICITY_LAGS: usize = 4;

pub(super) const EC_STATE_LIMIT_SOFT_KNEE: f64 = 0.82;
pub(super) const EC_STATE_PRESSURE_INV_COUNT: f64 = 1.0 / 7.0;
pub(super) const EC_STATE_LIMIT_SOFT_KNEE_INV_SPAN: f64 = 1.0 / (1.0 - EC_STATE_LIMIT_SOFT_KNEE);
pub(super) const EC_STATE_LIMIT_SOFT_KNEE_SQ: f64 =
    EC_STATE_LIMIT_SOFT_KNEE * EC_STATE_LIMIT_SOFT_KNEE;

/// Root-carry tuple: `(shared2_norm, y2_shared, f_committed)` — the previous
/// sample's eager root expansion (normalized space) plus the committed feedback
/// factor.
pub(super) type RootCarry = ([f64; 8], f64, f64);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Ec2LongFilterPolicy {
    #[default]
    Off,
    PressureTaper,
    AmbiguityPressure,
    Combined,
    DiagnosticDepth3Rescue,
}

impl Ec2LongFilterPolicy {
    pub fn as_name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::PressureTaper => "pressure-taper",
            Self::AmbiguityPressure => "ambiguity-pressure",
            Self::Combined => "combined",
            Self::DiagnosticDepth3Rescue => "diagnostic-depth3-rescue",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "pressure-taper" | "pressure_taper" | "pressure" => Some(Self::PressureTaper),
            "ambiguity-pressure" | "ambiguity_pressure" | "ambiguity" => {
                Some(Self::AmbiguityPressure)
            }
            "combined" | "both" => Some(Self::Combined),
            "diagnostic-depth3-rescue" | "diagnostic_depth3_rescue" | "depth3-rescue" => {
                Some(Self::DiagnosticDepth3Rescue)
            }
            _ => None,
        }
    }

    pub(super) fn uses_pressure_taper(self) -> bool {
        matches!(self, Self::PressureTaper | Self::Combined)
    }

    pub(super) fn uses_ambiguity_pressure(self) -> bool {
        matches!(self, Self::AmbiguityPressure | Self::Combined)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ec2PolicyWeights {
    pub quantizer_weight: f64,
    pub pressure_weight: f64,
    pub limit_weight: f64,
    pub transition_weight: f64,
    pub dc_weight: f64,
    pub lookahead_discount: f64,
    pub ambiguity_margin: f64,
    pub pressure_taper_start: f64,
    pub pressure_taper_strength: f64,
}

impl Default for Ec2PolicyWeights {
    fn default() -> Self {
        Self {
            quantizer_weight: EC_QUANTIZER_ERROR_WEIGHT,
            pressure_weight: EC_STATE_PRESSURE_WEIGHT,
            limit_weight: EC_STATE_LIMIT_WEIGHT,
            transition_weight: EC_TRANSITION_WEIGHT,
            dc_weight: EC_DC_BIAS_WEIGHT,
            lookahead_discount: EC_LOOKAHEAD_DISCOUNT,
            ambiguity_margin: 0.0,
            pressure_taper_start: 0.60,
            pressure_taper_strength: 0.0,
        }
    }
}

/// Modulator decision engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModulatorMode {
    /// Original hard-sign CRFB quantizer.
    Standard,
    /// Lookahead EC-style compensated quantizer.
    Ec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DitherShape {
    WhiteTpdf,
    HighPassTpdf,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EcBeamClampPolicy {
    /// Preserve the original prototype behavior: score children first, keep the
    /// winners, then clamp materialized survivor states.
    #[default]
    LegacyClampAndContinue,
    /// Keep clamped children feasible but make hard-limit excursions very
    /// expensive before Top-M selection.
    PenalizeClamp,
    /// Treat any candidate with normalized state beyond +/-1.0 as infeasible.
    RejectHardLimit,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EcBeamMetricMode {
    /// Survivor pruning uses only accumulated path metric plus deterministic
    /// exact tie-breaks.
    PathConsistent,
    /// Legacy EcBeam behavior: accumulated path metric survives, while opt-in
    /// terminal/alternation/filter nudges may affect current frontier ranking.
    #[default]
    HybridRankNudged,
    /// Experimental split scoring for pressure/DC terms, with independently
    /// configurable accumulated and rank-only lanes.
    MetricHygiene,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DitherPrng {
    XorShift64,
    Xoshiro256StarStar,
    SplitMix64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcFutureScorer {
    /// Full root scorer for lookahead nodes. NOTE: at `coeffs.osr > 128`
    /// (DSD256) this silently degrades to the quantizer+limit scorer — see
    /// [`CrfbModulator::ec_future_candidate_score_pair`]. Sweeps at DSD256
    /// comparing "Full" against `QuantizerLimit` are comparing near-identical
    /// configurations.
    Full,
    FullDiscount40,
    FullDiscount25,
    FullDiscount10,
    FullDepth3Guard0001,
    FullDepth3Guard001,
    FullDepth3Guard01,
    FullDepth3Guard05,
    FullDepth3Guard10,
    QuantizerOnly,
    QuantizerLimit,
    QuarterPressureNoDcTransition,
}

impl EcFutureScorer {
    /// The scorer actually used at a given oversampling ratio. The `Full`-family
    /// variants silently fall through to the quantizer+limit scorer when
    /// `osr > 128` (see [`CrfbModulator::ec_future_candidate_score_pair`]), so a
    /// DSD256 sweep configured with `Full` is really running `QuantizerLimit`.
    /// This mapping is the single source of truth for that degrade so harness
    /// rows can log the effective scorer instead of the requested one.
    pub fn effective_for_osr(self, osr: u32) -> Self {
        match self {
            Self::Full
            | Self::FullDiscount40
            | Self::FullDiscount25
            | Self::FullDiscount10
            | Self::FullDepth3Guard0001
            | Self::FullDepth3Guard001
            | Self::FullDepth3Guard01
            | Self::FullDepth3Guard05
            | Self::FullDepth3Guard10
                if osr > 128 =>
            {
                Self::QuantizerLimit
            }
            other => other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DsdModulator {
    Standard,
    EcDepth1,
    #[default]
    EcDepth2,
    EcBeam,
    EcBeam2,
    EcDepth3,
    EcDepth4,
    EcDepth8,
    EcDepth4Adaptive,
}

impl DsdModulator {
    pub fn as_id(self) -> u32 {
        match self {
            Self::Standard => 0,
            Self::EcDepth2 => 1,
            Self::EcBeam => 2,
            Self::EcBeam2 => 7,
            Self::EcDepth1
            | Self::EcDepth3
            | Self::EcDepth4
            | Self::EcDepth8
            | Self::EcDepth4Adaptive => 1,
        }
    }

    pub fn from_id(id: u32) -> Self {
        match id {
            0 => Self::Standard,
            1 => Self::EcDepth2,
            2 => Self::EcBeam,
            // Non-current persisted modulator IDs normalize to the production EC mode.
            3..=6 => Self::EcDepth2,
            7 => Self::EcBeam2,
            _ => Self::default(),
        }
    }

    pub fn as_name(self) -> &'static str {
        match self {
            Self::Standard => "Standard",
            Self::EcDepth1 => "EcDepth1",
            Self::EcDepth2 => "EcDepth2",
            Self::EcBeam => "EcBeam",
            Self::EcBeam2 => "EcBeam2",
            Self::EcDepth3 => "EcDepth3",
            Self::EcDepth4 => "EcDepth4",
            Self::EcDepth8 => "EcDepth8",
            Self::EcDepth4Adaptive => "EcDepth4Adaptive",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "standard" => Some(Self::Standard),
            "ecdepth1" | "ec depth 1" | "ec_depth_1" | "ec-1" | "ec1" => Some(Self::EcDepth2),
            "ecdepth2" | "ec depth 2" | "ec_depth_2" | "ec-2" | "ec2" => Some(Self::EcDepth2),
            "ecbeam" | "ec beam" | "ec_beam" | "ec-beam" | "ecb" | "7th order ecb" => {
                Some(Self::EcBeam)
            }
            "ecbeam2" | "ec beam 2" | "ec_beam_2" | "ec-beam-2" | "ecb2" | "7th order ecb2" => {
                Some(Self::EcBeam2)
            }
            "ecdepth3" | "ec depth 3" | "ec_depth_3" | "ec-3" | "ec3" => Some(Self::EcDepth2),
            "ecdepth4" | "ec depth 4" | "ec_depth_4" | "ec-4" | "ec4" => Some(Self::EcDepth2),
            "ecdepth8" | "ec depth 8" | "ec_depth_8" | "ec-8" | "ec8" => Some(Self::EcDepth2),
            "ecdepth4adaptive"
            | "ec depth 4 adaptive"
            | "ec_depth_4_adaptive"
            | "ec-4a"
            | "ec4a" => Some(Self::EcDepth2),
            _ => None,
        }
    }

    pub fn mode(self) -> ModulatorMode {
        match self {
            Self::Standard => ModulatorMode::Standard,
            Self::EcDepth1
            | Self::EcDepth2
            | Self::EcBeam
            | Self::EcBeam2
            | Self::EcDepth3
            | Self::EcDepth4
            | Self::EcDepth8
            | Self::EcDepth4Adaptive => ModulatorMode::Ec,
        }
    }

    pub fn lookahead_depth(self) -> usize {
        match self {
            Self::Standard => 1,
            Self::EcDepth1 => 1,
            Self::EcDepth2 | Self::EcBeam | Self::EcBeam2 => 2,
            Self::EcDepth3 => 3,
            Self::EcDepth4 | Self::EcDepth4Adaptive => 4,
            Self::EcDepth8 => 8,
        }
    }

    pub fn is_adaptive(self) -> bool {
        matches!(self, Self::EcDepth4Adaptive)
    }

    pub fn from_mode(mode: ModulatorMode) -> Self {
        match mode {
            ModulatorMode::Standard => Self::Standard,
            ModulatorMode::Ec => Self::EcDepth2,
        }
    }
}

pub struct CrfbModulator {
    pub(super) state: [f64; 8],
    pub(super) coeffs: &'static ModulatorCoeffs,
    pub(super) a_rows: [[f64; 8]; 7],
    pub(super) bu: [f64; 7],
    /// Feedback input column, zero-padded to 8 so the hot loops run a full
    /// power-of-two lane count (state[7] is pinned to 0, so the padding is inert).
    pub(super) bv: [f64; 8],
    pub(super) c_row: [f64; 8],
    /// `(A·bv) ∘ inverse_state_limit`, precomputed: the affine response of the
    /// next base state to feedback in normalized state space. The whole search
    /// runs in that space (states scaled by the per-integrator inverse limits);
    /// raw states are only rebuilt once per sample at commit.
    pub(super) a_bv_norm: [f64; 8],
    /// `D⁻¹·A·D` (the loop matrix conjugated into normalized space, same
    /// sparsity), for the dense expansion fallback.
    pub(super) a_rows_norm: [[f64; 8]; 7],
    /// `D⁻¹·bu`, the input column in normalized space.
    pub(super) bu_norm: [f64; 7],
    /// `c ∘ D`, the output row in normalized space.
    pub(super) c_row_norm: [f64; 8],
    /// Normalized-space CRFB row pairs packed for the NEON expansion matvec: for
    /// pair `p` (rows `2p+1`, `2p+2`), entries `0..=2` are the column vectors
    /// `[a_norm[2p+1][c], a_norm[2p+2][c]]` for `c = 2p, 2p+1, 2p+2`, and entry
    /// `3` is the `[bu_norm[2p+1], bu_norm[2p+2]]` pair. Only meaningful when
    /// `crfb_sparse`.
    pub(super) a_pair_cols: [[[f64; 2]; 4]; 3],
    /// State limits as a padded column (lane 7 zero), for de-normalizing at
    /// commit.
    pub(super) state_limit8: [f64; 8],
    /// `c·bv`, precomputed: the affine response of the next loop output to feedback.
    pub(super) c_bv: f64,
    pub(super) inverse_state_limit: [f64; 8],
    /// `bv ∘ inverse_state_limit`, precomputed: the feedback column in normalized
    /// state space, so the candidate score never materializes the raw state.
    pub(super) bv_norm: [f64; 8],
    /// `Σ bv_norm²`, precomputed: the feedback-quadratic term of the pressure.
    pub(super) bv_norm_sq_sum: f64,
    /// Per-integrator pressure weights (Workstream: per-stage pressure). The
    /// default state pressure weights all seven stages equally by
    /// [`EC_STATE_PRESSURE_INV_COUNT`]; when `pressure_stage_weighted` is set,
    /// the state-pressure term instead uses `Σ pressure_stage_weight[i]·s_i²`,
    /// letting the search treat early vs late integrators differently. Weights
    /// are normalized to sum to 1.0 so the knob only redistributes pressure and
    /// stays orthogonal to `pressure_weight`. Lane 7 is 0 (padding).
    pub(super) pressure_stage_weight: [f64; 8],
    /// `Σ pressure_stage_weight[i]·bv_norm[i]²`, the per-stage-weighted analog of
    /// `bv_norm_sq_sum`; precomputed when the weights are set.
    pub(super) pbv_sq_sum: f64,
    /// Whether the per-stage pressure weighting is active. `false` keeps the
    /// scalar fast path (bit-identical to the uniform default); only a genuine
    /// non-uniform weight set flips it on.
    pub(super) pressure_stage_weighted: bool,
    /// Per-lane knee-gate thresholds: the soft-knee penalty can be nonzero for
    /// some candidate iff `b_i² > (knee − |bv_norm_i|)²` for some `i` (−1.0 when
    /// `|bv_norm_i| ≥ knee`, i.e. always fire), with `b` the normalized base.
    pub(super) knee_thr_sq: [f64; 8],
    /// Coefficient table matches the CRFB band-sparsity pattern, enabling the
    /// monomorphized sparse kernels.
    pub(super) crfb_sparse: bool,
    pub(super) rng: DitherRng,
    pub(super) common_side_dither: Option<CommonSideDitherState>,
    pub(super) mode: ModulatorMode,
    /// Scale applied to TPDF dither at the quantizer comparator. `next_tpdf`
    /// returns (-1, +1), so this is the peak comparator perturbation.
    pub(super) dither_scale: f64,
    pub(super) dither_shape: DitherShape,
    pub(super) dither_prng: DitherPrng,
    pub(super) dither_leak_alpha: f64,
    pub(super) dither_lf_floor_gamma: f64,
    pub(super) dither_high_pass_norm: f64,
    pub(super) future_scorer: EcFutureScorer,
    pub(super) ec2_policy: Ec2LongFilterPolicy,
    pub(super) ec2_weights: Ec2PolicyWeights,
    /// One-pole decay of the committed-bit DC tracker. The legacy default
    /// [`EC_DC_BIAS_DECAY`] puts the tracker's −3 dB corner *inside* the audio
    /// band and makes it rate-dependent (~225 Hz at DSD64, ~449 Hz at DSD128,
    /// ~898 Hz at DSD256); see [`dc_bias_decay_for_corner_hz`] to place the
    /// corner explicitly.
    pub(super) dc_bias_decay: f64,
    /// Ambiguity-gated comparator dither (Workstream G2). When both are > 0, the
    /// committed root quantizer receives a comparator dither of peak
    /// `gated_dither_scale` *only* on samples where the two root candidate scores
    /// are a near-tie (relative margin ≤ `gated_dither_margin`). Program material
    /// rarely ties, so this breaks idle limit cycles at ~zero in-band cost.
    /// Both default 0.0 (disabled → bit-identical). The perturbation reuses the
    /// already-drawn comparator dither sample, so the RNG stream is unchanged.
    pub(super) gated_dither_margin: f64,
    pub(super) gated_dither_scale: f64,
    pub(super) ec2_decision_trace: Option<Ec2DecisionTrace>,
    /// Transition-loss fraction modeled in the EC feedback. 0.0 = ideal DAC.
    pub(super) isi_penalty: f64,
    /// EC search depth (committed bit + `depth - 1` lookahead samples).
    pub(super) lookahead_depth: usize,
    /// Samples buffered awaiting enough lookahead context (EC mode only).
    pub(super) pending: Vec<f64>,
    /// Root values carried from the previous sample's eager expansion. The next
    /// sample's root base state / loop output are exactly `shared2 + f·A·bv` /
    /// `y2 + f·c·bv`, so the root matvec is skipped entirely. Only valid while
    /// the committed state was neither clamped nor reset (the carry assumes the
    /// unclamped affine state) — invalidated otherwise.
    pub(super) carried_root: Option<RootCarry>,
    pub(super) prev_v: f64,
    pub(super) prev_tpdf: f64,
    pub(super) dc_bias: f64,
    /// Number of times the last-resort safety reset has fired since construction.
    /// Finite overload is handled by per-sample state saturation; non-zero here
    /// means the loop produced non-finite state math.
    pub(super) stability_resets: u64,
    /// Number of finite state overloads handled by per-integrator clamping since
    /// construction.
    pub(super) state_clamps: u64,
    /// EcBeam prototype side-state (docs/dev/7th-order-ecm-m-algorithm.md §21).
    /// `None` (the default) leaves every existing mode's path untouched.
    pub(super) beam: Option<Box<BeamState>>,
}

impl CrfbModulator {
    /// Create a new modulator. Returns an error string if the coefficient table has not
    /// been calibrated (i.e. `tools/gen_crfb.py` has not been run yet).
    pub fn new(coeffs: &'static ModulatorCoeffs, seed: u64) -> Result<Self, &'static str> {
        Self::new_with_mode(coeffs, seed, ModulatorMode::Standard)
    }

    pub fn new_standard(coeffs: &'static ModulatorCoeffs, seed: u64) -> Result<Self, &'static str> {
        Self::new_with_mode(coeffs, seed, ModulatorMode::Standard)
    }

    pub fn new_ec(coeffs: &'static ModulatorCoeffs, seed: u64) -> Result<Self, &'static str> {
        Self::new_with_mode(coeffs, seed, ModulatorMode::Ec)
    }

    pub fn new_with_mode(
        coeffs: &'static ModulatorCoeffs,
        seed: u64,
        mode: ModulatorMode,
    ) -> Result<Self, &'static str> {
        if !CALIBRATED {
            return Err("dsd_coeffs: CRFB table is uncalibrated — run tools/gen_crfb.py");
        }
        let dither_scale = match mode {
            ModulatorMode::Standard => QUANTIZER_DITHER_SCALE,
            ModulatorMode::Ec => {
                let multiplier = if coeffs.osr == 128 {
                    EC_DSD128_DITHER_SCALE_MULTIPLIER
                } else {
                    EC_DITHER_SCALE_MULTIPLIER
                };
                QUANTIZER_DITHER_SCALE * multiplier
            }
        };
        let dither_shape = match mode {
            ModulatorMode::Standard => DitherShape::WhiteTpdf,
            ModulatorMode::Ec => DitherShape::HighPassTpdf,
        };
        // Limits are validated once here so the per-sample commit path never has to.
        let mut inverse_state_limit = [0.0; 8];
        for (inverse, limit) in inverse_state_limit
            .iter_mut()
            .zip(coeffs.state_limit.iter())
        {
            if !limit.is_finite() || *limit <= 0.0 {
                return Err("dsd_coeffs: state_limit entries must be finite and positive");
            }
            *inverse = 1.0 / *limit;
        }
        let mut a_rows = [[0.0; 8]; 7];
        let mut bu = [0.0; 7];
        let mut bv = [0.0; 8];
        let mut c_row = [0.0; 8];
        for i in 0..7 {
            a_rows[i][..7].copy_from_slice(&coeffs.a[i]);
            bu[i] = coeffs.b[i][0];
            bv[i] = coeffs.b[i][1];
            c_row[i] = coeffs.c[i];
        }
        let mut a_bv = [0.0; 8];
        let mut c_bv = 0.0;
        let mut bv_norm = [0.0; 8];
        for i in 0..7 {
            let mut acc = 0.0;
            for k in 0..7 {
                acc = a_rows[i][k].mul_add(bv[k], acc);
            }
            a_bv[i] = acc;
            c_bv = c_row[i].mul_add(bv[i], c_bv);
            bv_norm[i] = bv[i] * inverse_state_limit[i];
        }
        let mut bv_norm_sq_sum = 0.0;
        let mut a_bv_norm = [0.0; 8];
        let mut knee_thr_sq = [f64::INFINITY; 8];
        for i in 0..7 {
            bv_norm_sq_sum += bv_norm[i] * bv_norm[i];
            a_bv_norm[i] = a_bv[i] * inverse_state_limit[i];
            let thr = EC_STATE_LIMIT_SOFT_KNEE - bv_norm[i].abs();
            knee_thr_sq[i] = if thr > 0.0 { thr * thr } else { -1.0 };
        }
        let mut a_rows_norm = [[0.0; 8]; 7];
        let mut bu_norm = [0.0; 7];
        let mut c_row_norm = [0.0; 8];
        let mut state_limit8 = [0.0; 8];
        for i in 0..7 {
            for k in 0..7 {
                a_rows_norm[i][k] = a_rows[i][k] * coeffs.state_limit[k] * inverse_state_limit[i];
            }
            bu_norm[i] = bu[i] * inverse_state_limit[i];
            c_row_norm[i] = c_row[i] * coeffs.state_limit[i];
            state_limit8[i] = coeffs.state_limit[i];
        }
        let mut a_pair_cols = [[[0.0f64; 2]; 4]; 3];
        for p in 0..3 {
            for (j, col) in (2 * p..2 * p + 3).enumerate() {
                a_pair_cols[p][j] = [a_rows_norm[2 * p + 1][col], a_rows_norm[2 * p + 2][col]];
            }
            a_pair_cols[p][3] = [bu_norm[2 * p + 1], bu_norm[2 * p + 2]];
        }
        Ok(Self {
            state: [0.0; 8],
            coeffs,
            a_rows,
            bu,
            bv,
            c_row,
            a_bv_norm,
            a_rows_norm,
            bu_norm,
            c_row_norm,
            a_pair_cols,
            state_limit8,
            c_bv,
            inverse_state_limit,
            bv_norm,
            bv_norm_sq_sum,
            pressure_stage_weight: [EC_STATE_PRESSURE_INV_COUNT; 8],
            pbv_sq_sum: 0.0,
            pressure_stage_weighted: false,
            knee_thr_sq,
            crfb_sparse: matches_crfb_sparsity(coeffs),
            rng: DitherRng::new(seed, DitherPrng::XorShift64),
            common_side_dither: None,
            mode,
            dither_scale,
            dither_shape,
            dither_prng: DitherPrng::XorShift64,
            dither_leak_alpha: 1.0,
            dither_lf_floor_gamma: 0.0,
            dither_high_pass_norm: high_pass_tpdf_norm(1.0, 0.0),
            future_scorer: EcFutureScorer::Full,
            ec2_policy: Ec2LongFilterPolicy::Off,
            ec2_weights: Ec2PolicyWeights::default(),
            dc_bias_decay: EC_DC_BIAS_DECAY,
            gated_dither_margin: 0.0,
            gated_dither_scale: 0.0,
            ec2_decision_trace: None,
            isi_penalty: 0.0,
            lookahead_depth: DEFAULT_EC_LOOKAHEAD_DEPTH,
            pending: Vec::new(),
            carried_root: None,
            prev_v: 1.0,
            prev_tpdf: 0.0,
            dc_bias: 0.0,
            stability_resets: 0,
            state_clamps: 0,
            beam: None,
        })
    }

    /// Reset integrator state but keep the dither RNG running (so successive starts
    /// don't produce identical bitstreams).
    pub fn reset(&mut self) {
        self.hard_reset();
        self.pending.clear();
        if let Some(trace) = &mut self.ec2_decision_trace {
            trace.reset();
        }
        let seed = self.committed_beam_seed();
        if let Some(beam) = &mut self.beam {
            beam.reseed(seed);
        }
    }

    pub fn set_ec2_long_filter_policy(&mut self, policy: Ec2LongFilterPolicy) {
        self.ec2_policy = policy;
        if matches!(policy, Ec2LongFilterPolicy::DiagnosticDepth3Rescue) {
            self.lookahead_depth = self.lookahead_depth.max(3);
        }
    }

    pub fn ec2_long_filter_policy(&self) -> Ec2LongFilterPolicy {
        self.ec2_policy
    }

    /// Override the DC-tracker decay (see the `dc_bias_decay` field docs).
    /// Values outside `[0, 1)` or non-finite are ignored. Compute a decay for
    /// an explicit corner frequency with [`dc_bias_decay_for_corner_hz`].
    pub fn set_dc_bias_decay(&mut self, decay: f64) {
        if decay.is_finite() && (0.0..1.0).contains(&decay) {
            self.dc_bias_decay = decay;
        }
    }

    pub fn dc_bias_decay(&self) -> f64 {
        self.dc_bias_decay
    }

    /// Configure ambiguity-gated comparator dither (see the `gated_dither_*`
    /// field docs). Both values must be `> 0` and finite to take effect; any
    /// non-finite or non-positive value disables that half (0.0), restoring
    /// bit-identical behavior. `margin` is a relative near-tie threshold on the
    /// root candidate scores; `scale` is the peak comparator perturbation.
    pub fn set_gated_dither(&mut self, margin: f64, scale: f64) {
        self.gated_dither_margin = if margin.is_finite() && margin > 0.0 {
            margin
        } else {
            0.0
        };
        self.gated_dither_scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            0.0
        };
    }

    pub fn gated_dither_margin(&self) -> f64 {
        self.gated_dither_margin
    }

    pub fn gated_dither_scale(&self) -> f64 {
        self.gated_dither_scale
    }

    /// Set per-integrator pressure weights (see the `pressure_stage_weight`
    /// field docs). The seven raw weights must be finite and non-negative with a
    /// positive sum; they are normalized to sum to 1.0, so a uniform `[1/7; 7]`
    /// reproduces the default pressure (to within floating-point reassociation)
    /// and any redistribution stays orthogonal to `pressure_weight`. Invalid or
    /// effectively-uniform inputs disable the feature, restoring the scalar
    /// fast path. Recomputes the weighted feedback-quadratic term.
    pub fn set_pressure_stage_weights(&mut self, weights: &[f64; 7]) {
        let sum: f64 = weights.iter().sum();
        let valid =
            weights.iter().all(|w| w.is_finite() && *w >= 0.0) && sum.is_finite() && sum > 0.0;
        if !valid {
            self.pressure_stage_weighted = false;
            return;
        }
        let mut normalized = [0.0f64; 8];
        let mut uniform = true;
        for i in 0..7 {
            normalized[i] = weights[i] / sum;
            if (normalized[i] - EC_STATE_PRESSURE_INV_COUNT).abs() > 1.0e-12 {
                uniform = false;
            }
        }
        if uniform {
            // No redistribution — keep the bit-identical scalar path.
            self.pressure_stage_weighted = false;
            return;
        }
        self.pressure_stage_weight = normalized;
        self.pbv_sq_sum = (0..7)
            .map(|i| normalized[i] * self.bv_norm[i] * self.bv_norm[i])
            .sum();
        self.pressure_stage_weighted = true;
    }

    pub fn pressure_stage_weighted(&self) -> bool {
        self.pressure_stage_weighted
    }

    /// Per-stage-weighted pressure dot products for a candidate base state:
    /// `(Σ w_i·b_i², Σ w_i·b_i·bv_i)`. Only used on the weighted path.
    #[inline(always)]
    #[allow(clippy::needless_range_loop)]
    pub(super) fn weighted_pressure_dots(&self, base_norm: &[f64; 8]) -> (f64, f64) {
        let mut s_w = 0.0;
        let mut t_w = 0.0;
        for i in 0..7 {
            let w = self.pressure_stage_weight[i];
            s_w = w.mul_add(base_norm[i] * base_norm[i], s_w);
            t_w = w.mul_add(base_norm[i] * self.bv_norm[i], t_w);
        }
        (s_w, t_w)
    }

    #[inline(always)]
    pub(super) fn gated_dither_active(&self) -> bool {
        self.gated_dither_margin > 0.0 && self.gated_dither_scale > 0.0
    }

    /// Re-evaluate the two root candidate scores for a given comparator input.
    /// Mirrors the root scoring in the depth-2 step exactly, so calling it with
    /// the step's `y_quantized` reproduces the step's `[c_plus, c_minus]`; used
    /// by the gated-dither path to re-score with a perturbed comparator input.
    #[inline(always)]
    pub(super) fn ec_root_score_pair(&self, y_quantized: f64, base1_norm: &[f64; 8]) -> [f64; 2] {
        if self.lookahead_depth == 1 {
            self.ec_depth1_candidate_score_pair_with_hot(
                y_quantized,
                base1_norm,
                self.prev_v,
                self.dc_bias,
            )
            .0
        } else {
            let (s, t, hot, pressure) =
                score_pair_dots_pressure(base1_norm, &self.bv_norm, &self.knee_thr_sq);
            let (pressure_weight, _) = self.ec2_pressure_weight_for(pressure);
            self.ec_candidate_score_pair_from_dots_with_weights(
                y_quantized,
                base1_norm,
                self.prev_v,
                self.dc_bias,
                s,
                t,
                hot,
                pressure_weight,
                self.ec2_weights.transition_weight,
                self.ec2_weights.dc_weight,
            )
            .0
        }
    }

    #[inline(always)]
    pub(super) fn updated_dc_bias(&self, previous: f64, v: f64) -> f64 {
        self.dc_bias_decay * previous + (1.0 - self.dc_bias_decay) * v
    }

    pub fn set_ec2_policy_weights(&mut self, weights: Ec2PolicyWeights) {
        self.ec2_weights = weights;
    }

    pub fn ec2_policy_weights(&self) -> Ec2PolicyWeights {
        self.ec2_weights
    }

    pub fn set_ec2_decision_trace_window_bits(&mut self, window_bits: Option<usize>) {
        self.ec2_decision_trace = window_bits.map(Ec2DecisionTrace::new);
    }

    pub fn ec2_decision_trace(&self) -> Option<Ec2DecisionTraceSnapshot> {
        self.ec2_decision_trace
            .as_ref()
            .map(Ec2DecisionTrace::snapshot)
    }

    /// Current normalized committed-state pressure `max_i |state_i| / limit_i`.
    ///
    /// While EcBeam is active and has delayed, unflushed decisions, this reports
    /// the outer committed state. Use
    /// [`beam_best_state_pressure`](Self::beam_best_state_pressure) to inspect
    /// the current best beam survivor.
    #[doc(hidden)]
    pub fn state_pressure(&self) -> f64 {
        max_abs7(&mul8(&self.state, &self.inverse_state_limit))
    }

    /// Per-stage pressure of the outer committed state. In active EcBeam mode,
    /// this is not the current best survivor; see
    /// [`beam_best_state_pressure_by_stage`](Self::beam_best_state_pressure_by_stage).
    #[doc(hidden)]
    pub fn state_pressure_by_stage(&self) -> [f64; 7] {
        let mut out = [0.0; 7];
        for (idx, value) in out.iter_mut().enumerate() {
            *value = self.state[idx].abs() * self.inverse_state_limit[idx];
        }
        out
    }

    /// Loop output from the outer committed state. In active EcBeam mode with
    /// buffered samples, this is intentionally not the best survivor's live loop
    /// output; use
    /// [`beam_best_loop_output_for_input`](Self::beam_best_loop_output_for_input)
    /// for that diagnostic.
    #[doc(hidden)]
    pub fn diagnostic_loop_output_for_input(&self, input: f64) -> f64 {
        if self.crfb_sparse {
            self.loop_output::<true>(&self.state, input)
        } else {
            self.loop_output::<false>(&self.state, input)
        }
    }

    pub(super) fn hard_reset(&mut self) {
        self.state = [0.0; 8];
        self.prev_v = 1.0;
        self.reset_dither_history();
        self.dc_bias = 0.0;
        self.carried_root = None;
    }

    pub(super) fn reset_dither_history(&mut self) {
        self.prev_tpdf = 0.0;
        if let Some(common_side) = &mut self.common_side_dither {
            common_side.reset_history();
        }
    }

    pub fn stability_resets(&self) -> u64 {
        self.stability_resets
    }

    pub fn state_clamps(&self) -> u64 {
        self.state_clamps
    }

    pub fn mode(&self) -> ModulatorMode {
        self.mode
    }

    /// Peak TPDF perturbation at the quantizer comparator. Exposed for empirical
    /// idle-tone vs. stability-margin sweeps.
    pub fn set_dither_scale(&mut self, scale: f64) {
        if scale.is_finite() && scale >= 0.0 {
            self.dither_scale = scale;
            self.canonicalize_zero_dither();
        }
    }

    pub fn set_ec_dither_scale_multiplier(&mut self, multiplier: f64) {
        if multiplier.is_finite() && multiplier >= 0.0 {
            self.dither_scale = QUANTIZER_DITHER_SCALE * multiplier;
            self.canonicalize_zero_dither();
        }
    }

    pub fn dither_scale(&self) -> f64 {
        self.dither_scale
    }

    pub fn set_dither_shape(&mut self, shape: DitherShape) {
        self.dither_shape = shape;
        self.reset_dither_history();
    }

    pub fn dither_shape(&self) -> DitherShape {
        self.dither_shape
    }

    pub fn set_dither_prng(&mut self, prng: DitherPrng, seed: u64) {
        self.dither_prng = prng;
        self.rng = DitherRng::new(seed, prng);
        if let Some(common_side) = &mut self.common_side_dither {
            common_side.reseed(prng);
        }
        self.reset_dither_history();
    }

    pub fn dither_prng(&self) -> DitherPrng {
        self.dither_prng
    }

    pub fn set_high_pass_dither_leak_alpha(&mut self, alpha: f64) {
        if alpha.is_finite() && (0.0..=1.0).contains(&alpha) {
            self.dither_leak_alpha = alpha;
            self.dither_high_pass_norm =
                high_pass_tpdf_norm(self.dither_leak_alpha, self.dither_lf_floor_gamma);
            self.reset_dither_history();
        }
    }

    pub fn high_pass_dither_leak_alpha(&self) -> f64 {
        self.dither_leak_alpha
    }

    pub fn set_high_pass_dither_lf_floor_gamma(&mut self, gamma: f64) {
        if gamma.is_finite() && gamma >= 0.0 {
            self.dither_lf_floor_gamma = gamma;
            self.dither_high_pass_norm =
                high_pass_tpdf_norm(self.dither_leak_alpha, self.dither_lf_floor_gamma);
            self.reset_dither_history();
        }
    }

    pub fn high_pass_dither_lf_floor_gamma(&self) -> f64 {
        self.dither_lf_floor_gamma
    }

    pub fn set_future_scorer(&mut self, scorer: EcFutureScorer) {
        self.future_scorer = scorer;
    }

    pub fn future_scorer(&self) -> EcFutureScorer {
        self.future_scorer
    }

    #[inline(always)]
    pub(super) fn ec_lookahead_discount(&self) -> f64 {
        if self.ec2_weights.lookahead_discount != EC_LOOKAHEAD_DISCOUNT {
            return self.ec2_weights.lookahead_discount;
        }
        match self.future_scorer {
            EcFutureScorer::FullDiscount40 => 0.40,
            EcFutureScorer::FullDiscount25 => 0.25,
            EcFutureScorer::FullDiscount10 => 0.10,
            _ => EC_LOOKAHEAD_DISCOUNT,
        }
    }

    #[inline(always)]
    pub(super) fn ec2_pressure_weight_for(&self, pressure: f64) -> (f64, bool) {
        let mut weight = self.ec2_weights.pressure_weight;
        let mut tapered = false;
        if self.ec2_policy.uses_pressure_taper()
            && self.ec2_weights.pressure_taper_strength > 0.0
            && pressure.is_finite()
            && pressure > self.ec2_weights.pressure_taper_start
        {
            let span = (1.0 - self.ec2_weights.pressure_taper_start).max(1.0e-9);
            let t = ((pressure - self.ec2_weights.pressure_taper_start) / span).clamp(0.0, 1.0);
            weight *= 1.0 + self.ec2_weights.pressure_taper_strength * t;
            tapered = true;
        }
        (weight, tapered)
    }

    #[inline(always)]
    pub(super) fn ec2_candidate_risk(
        &self,
        base_norm: &[f64; 8],
        v: f64,
        prev_v: f64,
        dc_bias: f64,
    ) -> f64 {
        let f = compensated_feedback(prev_v, v, self.isi_penalty);
        let candidate = affine8(base_norm, &self.bv_norm, f);
        let pressure = if self.pressure_stage_weighted {
            (0..7).fold(0.0, |acc, i| {
                self.pressure_stage_weight[i].mul_add(candidate[i] * candidate[i], acc)
            })
        } else {
            candidate_pressure(&candidate)
        };
        let transition = if v != prev_v { 1.0 } else { 0.0 };
        let next_bias = self.updated_dc_bias(dc_bias, v);
        pressure
            + self.ec2_weights.transition_weight * transition
            + self.ec2_weights.dc_weight * next_bias * next_bias
    }

    #[inline(always)]
    pub(super) fn ec_depth3_guard_margin(&self) -> Option<f64> {
        match self.future_scorer {
            EcFutureScorer::FullDepth3Guard0001 => Some(0.0001),
            EcFutureScorer::FullDepth3Guard001 => Some(0.001),
            EcFutureScorer::FullDepth3Guard01 => Some(0.01),
            EcFutureScorer::FullDepth3Guard05 => Some(0.05),
            EcFutureScorer::FullDepth3Guard10 => Some(0.10),
            _ => None,
        }
    }

    pub fn set_common_side_dither(
        &mut self,
        common_seed: u64,
        side_seed: u64,
        beta: f64,
        side_sign: f64,
    ) {
        if !self.effective_dither_active() {
            self.common_side_dither = None;
            self.prev_tpdf = 0.0;
            return;
        }
        if let Some(common_side) =
            CommonSideDitherState::new(common_seed, side_seed, self.dither_prng, beta, side_sign)
        {
            self.common_side_dither = Some(common_side);
            self.prev_tpdf = 0.0;
        }
    }

    #[inline(always)]
    pub(super) fn effective_dither_active(&self) -> bool {
        self.dither_scale != 0.0
    }

    fn canonicalize_zero_dither(&mut self) {
        if !self.effective_dither_active() {
            self.common_side_dither = None;
            self.prev_tpdf = 0.0;
        }
    }

    #[inline(always)]
    pub(super) fn next_dither(&mut self) -> f64 {
        self.next_dither_for_shape(self.dither_shape)
    }

    #[inline(always)]
    pub(super) fn next_dither_for_shape(&mut self, shape: DitherShape) -> f64 {
        if let Some(common_side) = &mut self.common_side_dither {
            return common_side.next(
                shape,
                self.dither_leak_alpha,
                self.dither_lf_floor_gamma,
                self.dither_high_pass_norm,
            );
        }
        next_dither_from(
            &mut self.rng,
            shape,
            &mut self.prev_tpdf,
            self.dither_leak_alpha,
            self.dither_lf_floor_gamma,
            self.dither_high_pass_norm,
        )
    }

    #[inline(always)]
    pub(super) fn next_white_dither(&mut self) -> f64 {
        self.next_dither_for_shape(DitherShape::WhiteTpdf)
    }

    #[inline(always)]
    pub(super) fn next_high_pass_dither(&mut self) -> f64 {
        self.next_dither_for_shape(DitherShape::HighPassTpdf)
    }

    #[inline(always)]
    pub(super) fn peek_future_dither(&self, count: usize) -> ([f64; MAX_EC_FUTURE_DITHER], usize) {
        let len = count.min(MAX_EC_FUTURE_DITHER);
        let mut dither = [0.0; MAX_EC_FUTURE_DITHER];
        if let Some(common_side) = &self.common_side_dither {
            let mut common_side = common_side.clone();
            for value in dither.iter_mut().take(len) {
                *value = common_side.next(
                    self.dither_shape,
                    self.dither_leak_alpha,
                    self.dither_lf_floor_gamma,
                    self.dither_high_pass_norm,
                );
            }
        } else {
            let mut rng = self.rng.clone();
            let mut prev_tpdf = self.prev_tpdf;
            for value in dither.iter_mut().take(len) {
                *value = next_dither_from(
                    &mut rng,
                    self.dither_shape,
                    &mut prev_tpdf,
                    self.dither_leak_alpha,
                    self.dither_lf_floor_gamma,
                    self.dither_high_pass_norm,
                );
            }
        }
        (dither, len)
    }

    #[inline(always)]
    pub(super) fn quantized_future_loop_output(&self, y: f64, future_dither: &[f64]) -> f64 {
        future_dither
            .first()
            .map_or(y, |dither| dither.mul_add(self.dither_scale, y))
    }

    #[inline(always)]
    pub(super) fn future_dither_tail<'a>(&self, future_dither: &'a [f64]) -> &'a [f64] {
        future_dither.get(1..).unwrap_or(&[])
    }

    /// Transition-loss fraction the EC feedback model pre-compensates for. Defaults
    /// to 0.0 (ideal DAC); only set nonzero from a known DAC profile — e.g.
    /// [`DEFAULT_ISI_PENALTY`] — since the compensation is baked into the committed
    /// loop state, not just the scoring.
    pub fn set_isi_penalty(&mut self, penalty: f64) {
        if penalty.is_finite() && (0.0..0.5).contains(&penalty) {
            self.isi_penalty = penalty;
        }
    }

    pub fn isi_penalty(&self) -> f64 {
        self.isi_penalty
    }

    /// EC search depth (committed bit + `depth - 1` lookahead samples). Clamped to
    /// `1..=MAX_EC_LOOKAHEAD_DEPTH`. Changing depth changes the modulator's latency;
    /// set it before processing.
    pub fn set_lookahead_depth(&mut self, depth: usize) {
        self.lookahead_depth = depth.clamp(1, MAX_EC_LOOKAHEAD_DEPTH);
    }

    pub fn lookahead_depth(&self) -> usize {
        self.lookahead_depth
    }

    /// Activate the EcBeam prototype (docs/dev/7th-order-ecm-m-algorithm.md §21):
    /// a true delayed-commitment M-algorithm search that replaces the
    /// restart-tree lookahead. EC mode only; Standard mode ignores it.
    ///
    /// `m` (beam width) clamps to `1..=16`, `n` (commit horizon) to `1..=48`;
    /// `m = 1` / `n = 1` exist for the anchor and degenerate tests, not for
    /// sweeping. Output lags input by `n - 1` samples; call
    /// [`flush_into_bits`](Self::flush_into_bits) at end of stream exactly as
    /// with the lookahead modes.
    ///
    /// In beam mode `lookahead_depth`, `lookahead_discount`, the
    /// ambiguity/taper policies ([`Ec2LongFilterPolicy`]), and gated dither do
    /// not apply and are ignored by design (§3). Scoring uses the configured
    /// [`Ec2PolicyWeights`] directly, never the pressure-taper wrapper (§21.3).
    /// The defaults equal the original depth-1 constants, preserving the M=1
    /// anchor. Terminal-cost ranking defaults to 0 (§21.2) and can be enabled
    /// through [`set_beam_terminal_weight`](Self::set_beam_terminal_weight).
    #[doc(hidden)]
    /// Process `input` (f64 PCM, nominally in [-1, +1]) into `out_bits`.
    /// Each output byte is 0 or 1 — the packer converts these to MSB-first frames.
    ///
    /// Caller is responsible for mapping PCM full scale to `coeffs.input_peak`
    /// before calling.
    ///
    /// In EC mode output lags input by `lookahead_depth - 1` samples; call
    /// [`flush_into_bits`](Self::flush_into_bits) at end of stream.
    pub fn process_into_bits(&mut self, input: &[f64], out_bits: &mut Vec<u8>) {
        out_bits.reserve(input.len());
        // EcBeam prototype dispatch — dead when `beam` is None, so every
        // existing mode's bitstream is untouched (§21.1 isolation invariant).
        if self.mode == ModulatorMode::Ec && self.beam.is_some() {
            if self.crfb_sparse {
                self.process_beam_block::<true>(input, out_bits);
            } else {
                self.process_beam_block::<false>(input, out_bits);
            }
            return;
        }
        match (self.mode, self.crfb_sparse) {
            (ModulatorMode::Standard, true) => self.process_standard_block::<true>(input, out_bits),
            (ModulatorMode::Standard, false) => {
                self.process_standard_block::<false>(input, out_bits)
            }
            (ModulatorMode::Ec, true) => self.process_ec_block_dispatch::<true>(input, out_bits),
            (ModulatorMode::Ec, false) => self.process_ec_block_dispatch::<false>(input, out_bits),
        }
    }

    /// Emit any samples still held for lookahead. The tail samples see a shorter
    /// future horizon, exactly as if the stream simply ended. No-op in Standard mode.
    pub fn flush_into_bits(&mut self, out_bits: &mut Vec<u8>) {
        if self.mode == ModulatorMode::Ec && self.beam.is_some() {
            self.flush_beam(out_bits);
            return;
        }
        if self.mode != ModulatorMode::Ec || self.pending.is_empty() {
            return;
        }
        match (self.crfb_sparse, self.lookahead_depth) {
            (true, ..=2) => self.flush_tail::<true, 1>(out_bits),
            (true, 3) => self.flush_tail::<true, 2>(out_bits),
            (true, 4) => self.flush_tail::<true, 3>(out_bits),
            (true, _) => self.flush_tail::<true, 0>(out_bits),
            (false, ..=2) => self.flush_tail::<false, 1>(out_bits),
            (false, 3) => self.flush_tail::<false, 2>(out_bits),
            (false, 4) => self.flush_tail::<false, 3>(out_bits),
            (false, _) => self.flush_tail::<false, 0>(out_bits),
        }
    }

    pub(super) fn flush_tail<const SPARSE: bool, const CHILD: u8>(
        &mut self,
        out_bits: &mut Vec<u8>,
    ) {
        let pending = std::mem::take(&mut self.pending);
        let mut carry = self.carried_root.take();
        for idx in 0..pending.len() {
            self.process_ec_buffered_sample::<SPARSE, CHILD>(
                pending[idx],
                &pending[idx + 1..],
                &mut carry,
                out_bits,
            );
        }
        self.carried_root = carry;
    }

    /// Loop output `y = c·x + d1·u`. The sparse kernel exploits the CRFB
    /// realization's single nonzero `c[6]`.
    #[inline(always)]
    pub(super) fn loop_output<const SPARSE: bool>(&self, state: &[f64; 8], u: f64) -> f64 {
        if SPARSE {
            self.coeffs.d1.mul_add(u, self.c_row[6] * state[6])
        } else {
            self.coeffs.d1.mul_add(u, dot8(&self.c_row, state))
        }
    }

    pub(super) fn process_standard_block<const SPARSE: bool>(
        &mut self,
        input: &[f64],
        out_bits: &mut Vec<u8>,
    ) {
        for &u in input {
            let y = self.loop_output::<SPARSE>(&self.state, u);
            let dither = self.next_dither();
            let y_quantized = dither.mul_add(self.dither_scale, y);
            let v = if y_quantized > 0.0 { 1.0 } else { -1.0 };
            let base = predict_base_next_state8::<SPARSE>(&self.state, &self.a_rows, &self.bu, u);
            let mut next = affine8(&base, &self.bv, v);
            self.commit_standard_sample(v, &mut next, out_bits);
        }
    }

    /// Selects the const `CHILD` class (which node fn scores the root's child
    /// subtree) so each lookahead depth monomorphizes a lean root: 1 = leaf,
    /// 2 = node2, 3 = node3, 0 = dynamic recursion (depth > 4).
    pub(super) fn process_ec_block_dispatch<const SPARSE: bool>(
        &mut self,
        input: &[f64],
        out_bits: &mut Vec<u8>,
    ) {
        match self.lookahead_depth {
            1 if self.pending.is_empty() => self.process_ec_depth1_block::<SPARSE>(input, out_bits),
            ..=2 => self.process_ec_block::<SPARSE, 1>(input, out_bits),
            3 => self.process_ec_block::<SPARSE, 2>(input, out_bits),
            4 => self.process_ec_block::<SPARSE, 3>(input, out_bits),
            _ => self.process_ec_block::<SPARSE, 0>(input, out_bits),
        }
    }

    pub(super) fn commit_standard_sample(
        &mut self,
        v: f64,
        next: &mut [f64; 8],
        out_bits: &mut Vec<u8>,
    ) {
        self.commit_state(next);
        out_bits.push(if v > 0.0 { 1 } else { 0 });
    }

    /// Returns `true` only for a clean commit (no clamp, no reset) — the cases
    /// where the carried root expansion still describes the committed state.
    pub(super) fn commit_ec_sample(
        &mut self,
        v: f64,
        next: &mut [f64; 8],
        clean_commit: bool,
        out_bits: &mut Vec<u8>,
    ) -> bool {
        if clean_commit {
            next[7] = 0.0;
            self.state = *next;
            self.prev_v = v;
            self.dc_bias = self.updated_dc_bias(self.dc_bias, v);
            out_bits.push(if v > 0.0 { 1 } else { 0 });
            return true;
        }

        let mut clean = false;
        match stabilize_state(next, &self.coeffs.state_limit, &self.inverse_state_limit) {
            StateStability::Ok { clamped } => {
                if clamped {
                    self.state_clamps = self.state_clamps.wrapping_add(1);
                } else {
                    clean = true;
                }
                self.state = *next;
                self.prev_v = v;
                self.dc_bias = self.updated_dc_bias(self.dc_bias, v);
            }
            StateStability::Reset => {
                self.hard_reset();
                self.stability_resets = self.stability_resets.wrapping_add(1);
            }
        }
        out_bits.push(if v > 0.0 { 1 } else { 0 });
        clean
    }

    pub(super) fn commit_state(&mut self, next: &mut [f64; 8]) -> bool {
        match stabilize_state(next, &self.coeffs.state_limit, &self.inverse_state_limit) {
            StateStability::Ok { clamped } => {
                if clamped {
                    self.state_clamps = self.state_clamps.wrapping_add(1);
                }
                self.state = *next;
                true
            }
            StateStability::Reset => {
                self.hard_reset();
                self.stability_resets = self.stability_resets.wrapping_add(1);
                false
            }
        }
    }
}
