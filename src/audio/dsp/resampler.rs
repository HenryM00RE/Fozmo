use realfft::num_complex::Complex64;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::f64::consts::PI;
use std::sync::{Arc, OnceLock};

const DEFAULT_LATENCY_BUDGET_MS: f64 = 100.0;
const POLYPHASE_PHASES: usize = 4096;
const MAX_EXACT_RATIONAL_PHASE_DEN: usize = 1280;
const MAX_PHASE_AWARE_RATIONAL_PHASE_DEN: usize = 160;
const MAX_COEFFICIENT_TABLE_BYTES: usize = 16 * 1024 * 1024;
const MAX_POLYPHASE_COEFFICIENT_TABLE_BYTES: usize = 128 * 1024 * 1024;
const MAX_SIMD_DIRECT_TAPS: usize = 257;
const MIXED_PHASE_UNWRAP_MAG_FLOOR_REL: f64 = 1e-6;
const HOMOMORPHIC_MAG_FLOOR_REL: f64 = 1e-12;
const PHASE_CONVERTED_TAIL_FADE_FRACTION: f64 = 0.01;
const EOF_DRAIN_ZERO_BLOCK_FRAMES: usize = 4096;
const MINIMUM16K_PRODUCTION_CUTOFF: f64 = 0.467621;
const MINIMUM16K_PRODUCTION_BETA: f64 = 20.47325;
const MINIMUM16K_PRODUCTION_TAIL_FADE: f64 = 0.007617;
const MINIMUM16K_PRODUCTION_MAG_FLOOR_REL: f64 = 1.13771845358e-12;
const LINEAR128K_TAPS_TOTAL: usize = 131_073;
const MINIMUM128K_TAPS_TOTAL: usize = 131_071;
const MINIMUM128K_PROFILE_1_BETA: f64 = MINIMUM16K_PRODUCTION_BETA;
const MINIMUM128K_PROFILE_2_BETA: f64 = 80.0;
const MINIMUM128K_PROFILE_3_BETA: f64 = 160.0;
const MINIMUM128K_PROFILE_4_BETA: f64 = 256.0;
const MINIMUM_COMPACT_BRANCH_TAPS: usize = 131_071;
const MINIMUM_COMPACT_IMPULSE_SAMPLES: usize = 262_141;
const MINIMUM_COMPACT_FFT_MULTIPLIER: usize = 8;
const MINIMUM_COMPACT_CLEANUP_BETA: f64 = 20.47325;

#[derive(Clone, Copy)]
struct MinimumCompactTrebleTaper {
    start_2x: f64,
    end_2x: f64,
    attenuation_db: f64,
}

#[derive(Clone, Copy)]
struct MinimumCompactParams {
    pass_edge_2x: f64,
    stop_edge_2x: f64,
    stop_gain: f64,
    nyquist_gain: f64,
    tail_fade_samples: usize,
    treble_taper: Option<MinimumCompactTrebleTaper>,
}

const MINIMUM_COMPACT_ORIGINAL_PARAMS: MinimumCompactParams = MinimumCompactParams {
    pass_edge_2x: 20_200.0 / 88_200.0,
    stop_edge_2x: 22_050.0 / 88_200.0,
    stop_gain: 3.162_277_660_168_379e-8,
    nyquist_gain: 1.0e-15,
    tail_fade_samples: 512,
    treble_taper: None,
};

const MINIMUM_COMPACT_BALANCED_PARAMS: MinimumCompactParams = MinimumCompactParams {
    pass_edge_2x: 20_200.0 / 88_200.0,
    stop_edge_2x: 22_050.0 / 88_200.0,
    stop_gain: 1.778_279_410_038_922_8e-8,
    nyquist_gain: 1.0e-15,
    tail_fade_samples: 512,
    treble_taper: None,
};

const SMOOTH_PHASE_PARAMS: MinimumCompactParams = MinimumCompactParams {
    pass_edge_2x: 20_200.0 / 88_200.0,
    stop_edge_2x: 22_050.0 / 88_200.0,
    stop_gain: 1.778_279_410_038_922_8e-8,
    nyquist_gain: 1.0e-15,
    tail_fade_samples: 512,
    treble_taper: Some(MinimumCompactTrebleTaper {
        start_2x: 14_500.0 / 88_200.0,
        end_2x: 18_500.0 / 88_200.0,
        attenuation_db: 0.55,
    }),
};

/// Split-phase blend split points, as fractions of the 2x prototype rate
/// (so they track the source rate; values below are for a 44.1 kHz source).
/// Below F_LO the phase is purely linear (constant group delay), above F_HI
/// purely minimum phase, with a smootherstep ramp in log-frequency between.
const SPLIT_PHASE_BLEND_F_LO: f64 = 3_000.0 / 88_200.0;
const SPLIT_PHASE_BLEND_F_HI: f64 = 14_000.0 / 88_200.0;
const SPLIT128K_PRODUCTION_CUTOFF: f64 = 0.465333;
const SPLIT128K_PRODUCTION_BETA: f64 = 23.12088;
const SPLIT128K_PRODUCTION_BLEND_FLOOR: f64 = 0.038155;
const SPLIT128K_PRODUCTION_CAUSALITY_SHIFT_SCALE: f64 = 1.040606;
const SPLIT128K_PRODUCTION_TAIL_FADE: f64 = 0.005621;
/// Frequency-split blends create sharper phase curvature around the split
/// points, so the reconstruction uses heavy FFT padding.
const SPLIT_PHASE_FFT_MULTIPLIER: usize = 32;
const INTEGRATED128K_TAPS_TOTAL: usize = 131_071;
const INTEGRATED128K_PRODUCTION_CUTOFF: f64 = 0.468_750;
const INTEGRATED128K_PRODUCTION_BETA: f64 = 22.400;
const INTEGRATED_PHASE_TRANSITION_F_LO: f64 = 4_000.0 / 88_200.0;
const INTEGRATED_PHASE_TRANSITION_F_HI: f64 = 15_500.0 / 88_200.0;
const INTEGRATED_PHASE_V2_TRANSITION_F_LO: f64 = 3_750.0 / 88_200.0;
const INTEGRATED_PHASE_V2_TRANSITION_F_HI: f64 = 15_250.0 / 88_200.0;
const INTEGRATED_PHASE_V3_TRANSITION_F_LO: f64 = 3_500.0 / 88_200.0;
const INTEGRATED_PHASE_V3_TRANSITION_F_HI: f64 = 14_750.0 / 88_200.0;
const INTEGRATED_PHASE_V4_TRANSITION_F_LO: f64 = 3_250.0 / 88_200.0;
const INTEGRATED_PHASE_V4_TRANSITION_F_HI: f64 = 14_250.0 / 88_200.0;
const INTEGRATED128K_PRODUCTION_CAUSALITY_SHIFT_SCALE: f64 = 1.02;
const INTEGRATED128K_PRODUCTION_TAIL_FADE: f64 = 0.0048;
const INTEGRATED128K_MINIMUM_TAIL_FADE: f64 = 0.0075;
const INTEGRATED128K_MINIMUM_MAG_FLOOR_REL: f64 = 3.0e-14;
const INTEGRATED128K_PHASE_FLOOR_REL: f64 = 1.0e-7;
const INTEGRATED_PHASE_FFT_MULTIPLIER: usize = 32;

#[derive(Clone, Copy)]
struct MinimumPhaseParams {
    tail_fade_fraction: f64,
    mag_floor_rel: f64,
}

impl Default for MinimumPhaseParams {
    fn default() -> Self {
        Self {
            tail_fade_fraction: PHASE_CONVERTED_TAIL_FADE_FRACTION,
            mag_floor_rel: HOMOMORPHIC_MAG_FLOOR_REL,
        }
    }
}

#[derive(Clone, Copy)]
struct SplitPhaseParams {
    split_f_lo: f64,
    split_f_hi: f64,
    low_blend_floor: f64,
    causality_shift_scale: f64,
    tail_fade_fraction: f64,
}

impl Default for SplitPhaseParams {
    fn default() -> Self {
        Self {
            split_f_lo: SPLIT_PHASE_BLEND_F_LO,
            split_f_hi: SPLIT_PHASE_BLEND_F_HI,
            low_blend_floor: 0.0,
            causality_shift_scale: 1.0,
            tail_fade_fraction: PHASE_CONVERTED_TAIL_FADE_FRACTION,
        }
    }
}

#[derive(Clone, Copy)]
struct IntegratedPhaseParams {
    transition_f_lo: f64,
    transition_f_hi: f64,
    causality_shift_scale: f64,
    tail_fade_fraction: f64,
    phase_floor_rel: f64,
    minimum_phase: MinimumPhaseParams,
}

impl Default for IntegratedPhaseParams {
    fn default() -> Self {
        Self {
            transition_f_lo: INTEGRATED_PHASE_TRANSITION_F_LO,
            transition_f_hi: INTEGRATED_PHASE_TRANSITION_F_HI,
            causality_shift_scale: INTEGRATED128K_PRODUCTION_CAUSALITY_SHIFT_SCALE,
            tail_fade_fraction: INTEGRATED128K_PRODUCTION_TAIL_FADE,
            phase_floor_rel: INTEGRATED128K_PHASE_FLOOR_REL,
            minimum_phase: MinimumPhaseParams {
                tail_fade_fraction: INTEGRATED128K_MINIMUM_TAIL_FADE,
                mag_floor_rel: INTEGRATED128K_MINIMUM_MAG_FLOOR_REL,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegratedPhaseProfile {
    One,
    Two,
    Three,
    Four,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinimumPhase128kProfile {
    One,
    Two,
    Three,
    Four,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinimumCompactProfile {
    Original,
    Balanced,
    Smooth,
}

impl MinimumPhase128kProfile {
    fn beta(self) -> f64 {
        match self {
            Self::One => MINIMUM128K_PROFILE_1_BETA,
            Self::Two => MINIMUM128K_PROFILE_2_BETA,
            Self::Three => MINIMUM128K_PROFILE_3_BETA,
            Self::Four => MINIMUM128K_PROFILE_4_BETA,
        }
    }
}

impl IntegratedPhaseProfile {
    pub(crate) fn transition(self) -> (f64, f64) {
        match self {
            Self::One => (
                INTEGRATED_PHASE_TRANSITION_F_LO,
                INTEGRATED_PHASE_TRANSITION_F_HI,
            ),
            Self::Two => (
                INTEGRATED_PHASE_V2_TRANSITION_F_LO,
                INTEGRATED_PHASE_V2_TRANSITION_F_HI,
            ),
            Self::Three => (
                INTEGRATED_PHASE_V3_TRANSITION_F_LO,
                INTEGRATED_PHASE_V3_TRANSITION_F_HI,
            ),
            Self::Four => (
                INTEGRATED_PHASE_V4_TRANSITION_F_LO,
                INTEGRATED_PHASE_V4_TRANSITION_F_HI,
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum FilterType {
    SincExtreme32k,
    LinearPhase128k,
    Minimum16k,
    #[serde(
        alias = "Linear",
        alias = "SincMedium",
        alias = "SincExperimental1m",
        alias = "Mixed16k",
        alias = "Perfect",
        alias = "Split16k",
        alias = "Split16kDsd128",
        alias = "Split16kDsd128Apod",
        alias = "Split16k-DSD128",
        alias = "Split32k",
        alias = "Split32kTap",
        alias = "Split32k-Tap",
        alias = "Split128kTap",
        alias = "Split128k-Tap"
    )]
    Split128k,
    IntegratedPhase128k,
    IntegratedPhase128kV2,
    IntegratedPhase128kV3,
    IntegratedPhase128kV4,
    MinimumPhase128k,
    MinimumPhase128kV2,
    MinimumPhase128kV3,
    MinimumPhase128kV4,
    #[serde(alias = "MinimumPhase128kV5")]
    MinimumPhaseCompact128k,
    MinimumPhaseCompact128kV2,
    SmoothPhase128k,
}

pub const DEFAULT_FILTER_TYPE: FilterType = FilterType::Split128k;
pub const DEFAULT_FILTER_NAME: &str = "Split128k";

impl FilterType {
    pub fn as_id(self) -> u32 {
        match self {
            FilterType::SincExtreme32k => 6,
            FilterType::LinearPhase128k => 33,
            FilterType::Minimum16k => 15,
            FilterType::Split128k => 21,
            FilterType::IntegratedPhase128k => 22,
            FilterType::IntegratedPhase128kV2 => 23,
            FilterType::IntegratedPhase128kV3 => 24,
            FilterType::IntegratedPhase128kV4 => 25,
            FilterType::MinimumPhase128k => 26,
            FilterType::MinimumPhase128kV2 => 27,
            FilterType::MinimumPhase128kV3 => 28,
            FilterType::MinimumPhase128kV4 => 29,
            FilterType::MinimumPhaseCompact128k => 30,
            FilterType::MinimumPhaseCompact128kV2 => 31,
            FilterType::SmoothPhase128k => 32,
        }
    }

    pub fn from_id(id: u32) -> Option<Self> {
        match id {
            6 => Some(FilterType::SincExtreme32k),
            33 => Some(FilterType::LinearPhase128k),
            15 => Some(FilterType::Minimum16k),
            0 | 2 | 11 | 16 | 17 | 18 | 19 | 20 => Some(FilterType::Split128k),
            21 => Some(FilterType::Split128k),
            22 => Some(FilterType::IntegratedPhase128k),
            23 => Some(FilterType::IntegratedPhase128kV2),
            24 => Some(FilterType::IntegratedPhase128kV3),
            25 => Some(FilterType::IntegratedPhase128kV4),
            26 => Some(FilterType::MinimumPhase128k),
            27 => Some(FilterType::MinimumPhase128kV2),
            28 => Some(FilterType::MinimumPhase128kV3),
            29 => Some(FilterType::MinimumPhase128kV4),
            30 => Some(FilterType::MinimumPhaseCompact128k),
            31 => Some(FilterType::MinimumPhaseCompact128kV2),
            32 => Some(FilterType::SmoothPhase128k),
            _ => None,
        }
    }

    pub fn as_name(self) -> &'static str {
        match self {
            FilterType::SincExtreme32k => "SincExtreme32k",
            FilterType::LinearPhase128k => "LinearPhase128k",
            FilterType::Minimum16k => "Minimum16k",
            FilterType::Split128k => "Split128k",
            FilterType::IntegratedPhase128k => "IntegratedPhase128k",
            FilterType::IntegratedPhase128kV2 => "IntegratedPhase128kV2",
            FilterType::IntegratedPhase128kV3 => "IntegratedPhase128kV3",
            FilterType::IntegratedPhase128kV4 => "IntegratedPhase128kV4",
            FilterType::MinimumPhase128k => "MinimumPhase128k",
            FilterType::MinimumPhase128kV2 => "MinimumPhase128kV2",
            FilterType::MinimumPhase128kV3 => "MinimumPhase128kV3",
            FilterType::MinimumPhase128kV4 => "MinimumPhase128kV4",
            FilterType::MinimumPhaseCompact128k => "MinimumPhaseCompact128k",
            FilterType::MinimumPhaseCompact128kV2 => "MinimumPhaseCompact128kV2",
            FilterType::SmoothPhase128k => "SmoothPhase128k",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "SincExtreme32k" => Some(FilterType::SincExtreme32k),
            "LinearPhase128k" => Some(FilterType::LinearPhase128k),
            "Minimum16k" => Some(FilterType::Minimum16k),
            "Split128k" | "Split128kTap" | "Split128k-Tap" => Some(FilterType::Split128k),
            "IntegratedPhase128k" | "IntegratedPhase" => Some(FilterType::IntegratedPhase128k),
            "IntegratedPhase128kV2" => Some(FilterType::IntegratedPhase128kV2),
            "IntegratedPhase128kV3" => Some(FilterType::IntegratedPhase128kV3),
            "IntegratedPhase128kV4" => Some(FilterType::IntegratedPhase128kV4),
            "MinimumPhase128k" => Some(FilterType::MinimumPhase128k),
            "MinimumPhase128kV2" => Some(FilterType::MinimumPhase128kV2),
            "MinimumPhase128kV3" => Some(FilterType::MinimumPhase128kV3),
            "MinimumPhase128kV4" => Some(FilterType::MinimumPhase128kV4),
            "MinimumPhaseCompact128k" | "MinimumPhase128kV5" => {
                Some(FilterType::MinimumPhaseCompact128k)
            }
            "MinimumPhaseCompact128kV2" => Some(FilterType::MinimumPhaseCompact128kV2),
            "SmoothPhase128k" => Some(FilterType::SmoothPhase128k),
            "Linear" | "SincMedium" | "SincExperimental1m" | "Mixed16k" | "Perfect"
            | "Split16k" | "Split16kDsd128" | "Split16kDsd128Apod" | "Split16k-DSD128"
            | "Split32k" | "Split32kTap" | "Split32k-Tap" => Some(FilterType::Split128k),
            _ => None,
        }
    }

    fn cutoff(self) -> f64 {
        match self {
            FilterType::SincExtreme32k => 0.454,
            FilterType::LinearPhase128k => env_f64("FOZMO_LINEAR128K_CUTOFF")
                .unwrap_or(SPLIT128K_PRODUCTION_CUTOFF)
                .clamp(0.40, 0.49),
            FilterType::Minimum16k => env_f64("FOZMO_MINIMUM16K_CUTOFF")
                .unwrap_or(MINIMUM16K_PRODUCTION_CUTOFF)
                .clamp(0.40, 0.49),
            FilterType::MinimumPhase128k
            | FilterType::MinimumPhase128kV2
            | FilterType::MinimumPhase128kV3
            | FilterType::MinimumPhase128kV4 => MINIMUM16K_PRODUCTION_CUTOFF,
            FilterType::MinimumPhaseCompact128k | FilterType::SmoothPhase128k => {
                MINIMUM_COMPACT_ORIGINAL_PARAMS.stop_edge_2x * 2.0
            }
            FilterType::MinimumPhaseCompact128kV2 => MINIMUM16K_PRODUCTION_CUTOFF,
            FilterType::Split128k => env_f64("FOZMO_SPLIT128K_CUTOFF")
                .unwrap_or(SPLIT128K_PRODUCTION_CUTOFF)
                .clamp(0.40, 0.49),
            FilterType::IntegratedPhase128k
            | FilterType::IntegratedPhase128kV2
            | FilterType::IntegratedPhase128kV3
            | FilterType::IntegratedPhase128kV4 => env_f64("FOZMO_INTEGRATED128K_CUTOFF")
                .unwrap_or(INTEGRATED128K_PRODUCTION_CUTOFF)
                .clamp(0.40, 0.49),
        }
    }

    fn beta(self) -> f64 {
        match self {
            FilterType::SincExtreme32k => 19.5,
            FilterType::LinearPhase128k => env_f64("FOZMO_LINEAR128K_BETA")
                .unwrap_or(SPLIT128K_PRODUCTION_BETA)
                .clamp(8.0, 32.0),
            FilterType::Minimum16k => env_f64("FOZMO_MINIMUM16K_BETA")
                .unwrap_or(MINIMUM16K_PRODUCTION_BETA)
                .clamp(8.0, 32.0),
            filter @ (FilterType::MinimumPhase128k
            | FilterType::MinimumPhase128kV2
            | FilterType::MinimumPhase128kV3
            | FilterType::MinimumPhase128kV4) => filter
                .minimum_phase128k_profile()
                .expect("Minimum Phase 128k filter must have a profile")
                .beta(),
            FilterType::MinimumPhaseCompact128k | FilterType::SmoothPhase128k => {
                MINIMUM_COMPACT_CLEANUP_BETA
            }
            FilterType::MinimumPhaseCompact128kV2 => MINIMUM16K_PRODUCTION_BETA,
            FilterType::Split128k => env_f64("FOZMO_SPLIT128K_BETA")
                .unwrap_or(SPLIT128K_PRODUCTION_BETA)
                .clamp(8.0, 32.0),
            FilterType::IntegratedPhase128k
            | FilterType::IntegratedPhase128kV2
            | FilterType::IntegratedPhase128kV3
            | FilterType::IntegratedPhase128kV4 => env_f64("FOZMO_INTEGRATED128K_BETA")
                .unwrap_or(INTEGRATED128K_PRODUCTION_BETA)
                .clamp(8.0, 32.0),
        }
    }

    fn character_beta(self) -> Option<f64> {
        match self {
            Self::MinimumPhaseCompact128k | Self::SmoothPhase128k => None,
            _ => Some(self.beta()),
        }
    }

    fn cleanup_beta(self) -> f64 {
        match self {
            Self::MinimumPhase128k
            | Self::MinimumPhase128kV2
            | Self::MinimumPhase128kV3
            | Self::MinimumPhase128kV4
            | Self::MinimumPhaseCompact128k
            | Self::MinimumPhaseCompact128kV2
            | Self::SmoothPhase128k => MINIMUM_COMPACT_CLEANUP_BETA,
            _ => self.beta(),
        }
    }

    fn is_high_latency(self) -> bool {
        true
    }

    fn requires_phase_aware_kernel(self) -> bool {
        matches!(
            self,
            Self::LinearPhase128k
                | Self::Minimum16k
                | Self::Split128k
                | Self::IntegratedPhase128k
                | Self::IntegratedPhase128kV2
                | Self::IntegratedPhase128kV3
                | Self::IntegratedPhase128kV4
                | Self::MinimumPhase128k
                | Self::MinimumPhase128kV2
                | Self::MinimumPhase128kV3
                | Self::MinimumPhase128kV4
                | Self::MinimumPhaseCompact128k
                | Self::MinimumPhaseCompact128kV2
                | Self::SmoothPhase128k
        )
    }

    pub(crate) fn uses_long_filter_dsd_defaults(self) -> bool {
        self.requires_phase_aware_kernel()
    }

    pub(crate) fn uses_split_family_dsd_defaults(self) -> bool {
        matches!(
            self,
            Self::Split128k
                | Self::IntegratedPhase128k
                | Self::IntegratedPhase128kV2
                | Self::IntegratedPhase128kV3
                | Self::IntegratedPhase128kV4
        )
    }

    pub(crate) fn integrated_phase_profile(self) -> Option<IntegratedPhaseProfile> {
        match self {
            Self::IntegratedPhase128k => Some(IntegratedPhaseProfile::One),
            Self::IntegratedPhase128kV2 => Some(IntegratedPhaseProfile::Two),
            Self::IntegratedPhase128kV3 => Some(IntegratedPhaseProfile::Three),
            Self::IntegratedPhase128kV4 => Some(IntegratedPhaseProfile::Four),
            _ => None,
        }
    }

    pub(crate) fn minimum_phase128k_profile(self) -> Option<MinimumPhase128kProfile> {
        match self {
            Self::MinimumPhase128k => Some(MinimumPhase128kProfile::One),
            Self::MinimumPhase128kV2 => Some(MinimumPhase128kProfile::Two),
            Self::MinimumPhase128kV3 => Some(MinimumPhase128kProfile::Three),
            Self::MinimumPhase128kV4 => Some(MinimumPhase128kProfile::Four),
            _ => None,
        }
    }

    fn minimum_compact_profile(self) -> Option<MinimumCompactProfile> {
        match self {
            Self::MinimumPhaseCompact128k => Some(MinimumCompactProfile::Original),
            Self::SmoothPhase128k => Some(MinimumCompactProfile::Smooth),
            _ => None,
        }
    }
}

#[allow(dead_code)]
pub(crate) fn integrated_phase_transition_hz(filter: FilterType) -> Option<(f64, f64)> {
    let (f_lo, f_hi) = filter.integrated_phase_profile()?.transition();
    Some((f_lo * 88_200.0, f_hi * 88_200.0))
}

#[allow(dead_code)]
pub(crate) fn integrated_phase_weight_at_hz(filter: FilterType, frequency_hz: f64) -> Option<f64> {
    let profile = filter.integrated_phase_profile()?;
    Some(integrated_phase_weight(
        frequency_hz / 88_200.0,
        integrated128k_phase_params(profile),
    ))
}

#[allow(dead_code)]
pub(crate) fn integrated_phase_analysis_impulse(
    filter: FilterType,
    taps_total: usize,
) -> Option<Vec<f64>> {
    let profile = filter.integrated_phase_profile()?;
    let proto = build_full_rate_2x_prototype(taps_total / 2, filter.beta(), filter.cutoff());
    Some(integrated_phase_impulse_with_params(
        &proto,
        integrated128k_phase_params(profile),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    /// Fast f64 AVX2 path for short direct filters only. Long filters fall back to scalar
    /// f64 accumulation or FFT so the 120-135 dB target is preserved end to end.
    DirectSimd,
    PartitionedFft {
        partition_frames: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PhaseMode {
    Linear,
    Minimum,
    MinimumPhase128k(MinimumPhase128kProfile),
    MinimumPhaseCompact128k(MinimumCompactProfile),
    /// 131,073-tap Split128k long split-phase profile.
    SplitPhase128k,
    IntegratedPhase128k(IntegratedPhaseProfile),
}

#[derive(Debug, Clone)]
pub enum StageSpec {
    Character2x {
        taps_total: usize,
        cutoff: f64,
        beta: f64,
        engine: EngineKind,
        phase_mode: PhaseMode,
    },
    CleanupHalfband2x {
        taps_total: usize,
        beta: f64,
        cutoff: f64,
        // Even phase is a pure delay. The odd branch is still represented as dense
        // coefficients; prototype-level zero-tap sparsity is a later optimization.
        engine: EngineKind,
    },
}

impl StageSpec {
    fn taps_total(&self) -> usize {
        match self {
            StageSpec::Character2x { taps_total, .. }
            | StageSpec::CleanupHalfband2x { taps_total, .. } => *taps_total,
        }
    }

    fn engine(&self) -> EngineKind {
        match self {
            StageSpec::Character2x { engine, .. } | StageSpec::CleanupHalfband2x { engine, .. } => {
                *engine
            }
        }
    }

    fn engine_count(&self) -> usize {
        match self {
            StageSpec::Character2x { .. } => 2,
            StageSpec::CleanupHalfband2x { .. } => 1,
        }
    }

    fn latency_frames_at_stage_rate(&self) -> usize {
        // Group delay depends on the phase mode: linear-phase kernels delay by
        // half the filter length, minimum-phase kernels are front-loaded and
        // report ~0, and mixed-phase kernels sit in between in proportion to
        // the linear weight.
        let linear_group_delay = (self.taps_total() - 1) / 2;
        let group_delay = match self {
            StageSpec::CleanupHalfband2x { .. } => linear_group_delay,
            StageSpec::Character2x { phase_mode, .. } => match phase_mode {
                PhaseMode::Linear => linear_group_delay,
                PhaseMode::Minimum
                | PhaseMode::MinimumPhase128k(_)
                | PhaseMode::MinimumPhaseCompact128k(_) => 0,
                // Split128k uses a delay-matched linear reference instead of
                // the symmetric prototype's center, so it is front-loaded in
                // the pipeline. Its small residual low-band delay is filter
                // character, as it is for pure minimum phase.
                PhaseMode::SplitPhase128k => 0,
                PhaseMode::IntegratedPhase128k(_) => 0,
            },
        };
        let partition = match self.engine() {
            EngineKind::DirectSimd => 0,
            EngineKind::PartitionedFft { partition_frames } => partition_frames,
        };
        group_delay + partition
    }
}

#[derive(Debug, Clone)]
pub struct StagePlan {
    pub stages: Vec<StageSpec>,
    pub latency_source_frames: usize,
    pub latency_ms: f64,
    pub high_latency: bool,
    pub estimated_memory_bytes: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StagePlanError {
    InvalidRate,
    NonIntegerRatio,
    UnsupportedIntegerRatio(u32),
    LatencyBudgetExceeded { latency_ms: f64, budget_ms: f64 },
    MemoryBudgetExceeded { bytes: usize, budget_bytes: usize },
}

pub struct SincResampler {
    filter_type: FilterType,
    source_rate: u32,
    target_rate: u32,
    path: ResamplerPath,
    input_frames: usize,
    output_frames: usize,
    eof_drained: bool,
}

enum ResamplerPath {
    Integer(IntegerCascade),
    Downsample(DownsampleChain),
    Fractional(PolyphaseResampler),
    Rational(RationalPolyphaseResampler),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResamplerPathKind {
    IntegerCascade,
    DownsampleChain,
    ExactRational,
    CappedFractional,
}

impl ResamplerPathKind {
    pub fn as_id(self) -> u32 {
        match self {
            Self::IntegerCascade => 1,
            Self::DownsampleChain => 2,
            Self::ExactRational => 3,
            Self::CappedFractional => 4,
        }
    }

    pub fn from_id(id: u32) -> Option<Self> {
        match id {
            1 => Some(Self::IntegerCascade),
            2 => Some(Self::DownsampleChain),
            3 => Some(Self::ExactRational),
            4 => Some(Self::CappedFractional),
            _ => None,
        }
    }

    pub fn as_name(self) -> &'static str {
        match self {
            Self::IntegerCascade => "integer_cascade",
            Self::DownsampleChain => "downsample_chain",
            Self::ExactRational => "exact_rational",
            Self::CappedFractional => "capped_fractional",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ResamplerRuntimeInfo {
    pub path_kind: ResamplerPathKind,
    pub uses_capped_fallback: bool,
    pub phase_profile_preserved: bool,
    pub source_rate: u32,
    pub target_rate: u32,
    pub ratio_num: u32,
    pub ratio_den: u32,
    pub latency_ms: f64,
    pub estimated_memory_bytes: usize,
}

impl SincResampler {
    pub fn new(filter_type: FilterType, source_rate: u32, target_rate: u32) -> Self {
        Self::new_with_capped_polyphase_warning(filter_type, source_rate, target_rate, true)
    }

    /// Build an exact 160/147 rational bridge for the fixed 192 kHz -> 176.4 kHz
    /// transition used by 48 kHz-family sources in DSD256 compatibility mode.
    pub(crate) fn new_exact_160_147_without_capped_polyphase_warning(
        filter_type: FilterType,
        source_rate: u32,
        target_rate: u32,
    ) -> Self {
        debug_assert_eq!(source_rate, 192_000);
        debug_assert_eq!(target_rate, 176_400);
        Self {
            filter_type,
            source_rate,
            target_rate,
            path: ResamplerPath::Rational(RationalPolyphaseResampler::new(
                filter_type,
                source_rate,
                target_rate,
                160,
                147,
            )),
            input_frames: 0,
            output_frames: 0,
            eof_drained: false,
        }
    }

    fn new_with_capped_polyphase_warning(
        filter_type: FilterType,
        source_rate: u32,
        target_rate: u32,
        warn_on_capped_polyphase: bool,
    ) -> Self {
        let planned_path = integer_ratio(source_rate, target_rate)
            .and_then(|ratio| {
                if ratio.is_power_of_two() && ratio > 1 && ratio <= 256 {
                    build_integer_stage_plan(
                        source_rate,
                        target_rate,
                        filter_type,
                        DEFAULT_LATENCY_BUDGET_MS,
                    )
                    .ok()
                    .map(IntegerCascade::new)
                    .map(ResamplerPath::Integer)
                } else {
                    None
                }
            })
            .or_else(|| {
                DownsampleChain::new(filter_type, source_rate, target_rate)
                    .map(ResamplerPath::Downsample)
            })
            .or_else(|| {
                exact_rational_bridge_plan(filter_type, source_rate, target_rate).map(
                    |(step_num, phase_den)| {
                        if warn_on_capped_polyphase
                            && filter_type.requires_phase_aware_kernel()
                            && phase_den > MAX_PHASE_AWARE_RATIONAL_PHASE_DEN
                        {
                            eprintln!(
                                "resampler: {} phase profile is not preserved for exact ratio {} -> {} (phase denominator {} exceeds {}); using a generic linear-phase rational kernel",
                                filter_type.as_name(),
                                source_rate,
                                target_rate,
                                phase_den,
                                MAX_PHASE_AWARE_RATIONAL_PHASE_DEN
                            );
                        }
                        ResamplerPath::Rational(RationalPolyphaseResampler::new(
                            filter_type,
                            source_rate,
                            target_rate,
                            step_num,
                            phase_den,
                        ))
                    },
                )
            });

        let path = planned_path.unwrap_or_else(|| {
            if warn_on_capped_polyphase {
                eprintln!(
                    "resampler: {} uses capped polyphase mode for unsupported ratio {} -> {}; long/32k tap counts only apply to integer-ratio cascades and supported downsample chains",
                    filter_type.as_name(),
                    source_rate,
                    target_rate
                );
                if filter_type.requires_phase_aware_kernel() {
                    eprintln!(
                        "resampler: {} phase profile is not preserved for unsupported ratio {} -> {}; using a generic linear-phase fractional kernel",
                        filter_type.as_name(), source_rate, target_rate
                    );
                }
            }
            ResamplerPath::Fractional(PolyphaseResampler::new(
                filter_type,
                source_rate,
                target_rate,
            ))
        });

        Self {
            filter_type,
            source_rate,
            target_rate,
            path,
            input_frames: 0,
            output_frames: 0,
            eof_drained: false,
        }
    }

    pub fn filter_type(&self) -> FilterType {
        self.filter_type
    }

    pub fn source_rate(&self) -> u32 {
        self.source_rate
    }

    pub fn target_rate(&self) -> u32 {
        self.target_rate
    }

    pub fn runtime_info(&self) -> ResamplerRuntimeInfo {
        let (path_kind, ratio_num, ratio_den) = match &self.path {
            ResamplerPath::Integer(_) => {
                let (num, den) = reduced_ratio(self.source_rate, self.target_rate);
                (ResamplerPathKind::IntegerCascade, num, den)
            }
            ResamplerPath::Downsample(_) => {
                let (num, den) = reduced_ratio(self.source_rate, self.target_rate);
                (ResamplerPathKind::DownsampleChain, num, den)
            }
            ResamplerPath::Fractional(_) => {
                let (num, den) = reduced_ratio(self.source_rate, self.target_rate);
                (ResamplerPathKind::CappedFractional, num, den)
            }
            ResamplerPath::Rational(rational) => (
                ResamplerPathKind::ExactRational,
                rational.step_num as u32,
                rational.phase_den as u32,
            ),
        };

        let phase_profile_preserved = match path_kind {
            ResamplerPathKind::IntegerCascade | ResamplerPathKind::DownsampleChain => true,
            ResamplerPathKind::ExactRational => {
                !self.filter_type.requires_phase_aware_kernel()
                    || ratio_den as usize <= MAX_PHASE_AWARE_RATIONAL_PHASE_DEN
            }
            ResamplerPathKind::CappedFractional => !self.filter_type.requires_phase_aware_kernel(),
        };
        ResamplerRuntimeInfo {
            path_kind,
            uses_capped_fallback: path_kind == ResamplerPathKind::CappedFractional,
            phase_profile_preserved,
            source_rate: self.source_rate,
            target_rate: self.target_rate,
            ratio_num,
            ratio_den,
            latency_ms: self.latency_ms(),
            estimated_memory_bytes: self.estimated_memory_bytes(),
        }
    }

    pub fn latency_ms(&self) -> f64 {
        match &self.path {
            ResamplerPath::Integer(cascade) => cascade.plan.latency_ms,
            ResamplerPath::Downsample(chain) => chain.latency_ms,
            ResamplerPath::Fractional(polyphase) => {
                polyphase.half_width as f64 / self.source_rate.max(1) as f64 * 1000.0
            }
            ResamplerPath::Rational(rational) => {
                rational.half_width as f64 / self.source_rate.max(1) as f64 * 1000.0
            }
        }
    }

    pub fn estimated_memory_bytes(&self) -> usize {
        match &self.path {
            ResamplerPath::Integer(cascade) => cascade.plan.estimated_memory_bytes,
            ResamplerPath::Downsample(chain) => chain.estimated_memory_bytes,
            ResamplerPath::Fractional(polyphase) => polyphase.estimated_memory_bytes(),
            ResamplerPath::Rational(rational) => rational.estimated_memory_bytes(),
        }
    }

    pub fn is_high_latency(&self) -> bool {
        match &self.path {
            ResamplerPath::Integer(cascade) => cascade.plan.high_latency,
            ResamplerPath::Downsample(chain) => chain.high_latency,
            ResamplerPath::Fractional(_) => false,
            ResamplerPath::Rational(rational) => rational.filter_type.is_high_latency(),
        }
    }

    pub fn input(&mut self, samples_l: &[f64], samples_r: &[f64]) {
        let frames = samples_l.len().min(samples_r.len());
        if frames > 0 {
            self.input_frames = self.input_frames.saturating_add(frames);
            self.eof_drained = false;
        }
        match &mut self.path {
            ResamplerPath::Integer(cascade) => cascade.input(samples_l, samples_r),
            ResamplerPath::Downsample(chain) => chain.input(samples_l, samples_r),
            ResamplerPath::Fractional(polyphase) => polyphase.input(samples_l, samples_r),
            ResamplerPath::Rational(rational) => rational.input(samples_l, samples_r),
        }
    }

    pub fn process(&mut self, output: &mut Vec<f64>) -> usize {
        let frames = match &mut self.path {
            ResamplerPath::Integer(cascade) => cascade.process(output),
            ResamplerPath::Downsample(chain) => chain.process(output),
            ResamplerPath::Fractional(polyphase) => polyphase.process(output),
            ResamplerPath::Rational(rational) => rational.process(output),
        };
        self.output_frames = self.output_frames.saturating_add(frames);
        frames
    }

    pub fn drain_eof(&mut self, output: &mut Vec<f64>) -> usize {
        if self.eof_drained {
            return 0;
        }

        let expected_output_frames = self.expected_output_frames();
        if self.output_frames >= expected_output_frames {
            self.eof_drained = true;
            return 0;
        }

        let missing_output_frames = expected_output_frames - self.output_frames;
        let rate_zero_frames = ceil_mul_div_usize(
            missing_output_frames,
            self.source_rate as usize,
            self.target_rate as usize,
        );
        let max_zero_frames = rate_zero_frames
            .saturating_add(self.path.flush_lookahead_source_frames(self.source_rate))
            .saturating_add(EOF_DRAIN_ZERO_BLOCK_FRAMES);

        let mut zero_frames_sent = 0usize;
        let mut frames_written = 0usize;
        let mut zeros = Vec::new();
        let mut block = Vec::new();
        while self.output_frames < expected_output_frames && zero_frames_sent < max_zero_frames {
            let frames = (max_zero_frames - zero_frames_sent).min(EOF_DRAIN_ZERO_BLOCK_FRAMES);
            zeros.clear();
            zeros.resize(frames, 0.0);
            self.path.input(&zeros, &zeros);
            zero_frames_sent += frames;

            block.clear();
            let generated = self.path.process(&mut block);
            if generated == 0 {
                continue;
            }

            let remaining = expected_output_frames - self.output_frames;
            let keep = generated.min(remaining);
            output.extend_from_slice(&block[..keep * 2]);
            frames_written += keep;
            self.output_frames += keep;

            if generated > keep {
                self.output_frames = expected_output_frames;
                break;
            }
        }

        debug_assert_eq!(
            self.output_frames, expected_output_frames,
            "resampler EOF drain failed to reach nominal output frame count"
        );
        self.eof_drained = true;
        frames_written
    }

    pub fn reset(&mut self) {
        match &mut self.path {
            ResamplerPath::Integer(cascade) => cascade.reset(),
            ResamplerPath::Downsample(chain) => chain.reset(),
            ResamplerPath::Fractional(polyphase) => polyphase.reset(),
            ResamplerPath::Rational(rational) => rational.reset(),
        }
        self.input_frames = 0;
        self.output_frames = 0;
        self.eof_drained = false;
    }

    fn expected_output_frames(&self) -> usize {
        ceil_mul_div_usize(
            self.input_frames,
            self.target_rate as usize,
            self.source_rate as usize,
        )
    }
}

impl ResamplerPath {
    fn input(&mut self, samples_l: &[f64], samples_r: &[f64]) {
        match self {
            ResamplerPath::Integer(cascade) => cascade.input(samples_l, samples_r),
            ResamplerPath::Downsample(chain) => chain.input(samples_l, samples_r),
            ResamplerPath::Fractional(polyphase) => polyphase.input(samples_l, samples_r),
            ResamplerPath::Rational(rational) => rational.input(samples_l, samples_r),
        }
    }

    fn process(&mut self, output: &mut Vec<f64>) -> usize {
        match self {
            ResamplerPath::Integer(cascade) => cascade.process(output),
            ResamplerPath::Downsample(chain) => chain.process(output),
            ResamplerPath::Fractional(polyphase) => polyphase.process(output),
            ResamplerPath::Rational(rational) => rational.process(output),
        }
    }

    fn flush_lookahead_source_frames(&self, source_rate: u32) -> usize {
        match self {
            ResamplerPath::Integer(cascade) => cascade.flush_lookahead_source_frames(),
            ResamplerPath::Downsample(chain) => chain.flush_lookahead_source_frames(source_rate),
            ResamplerPath::Fractional(polyphase) => polyphase.flush_lookahead_source_frames(),
            ResamplerPath::Rational(rational) => rational.flush_lookahead_source_frames(),
        }
    }
}

pub fn build_integer_stage_plan(
    source_rate: u32,
    target_rate: u32,
    family: FilterType,
    latency_budget_ms: f64,
) -> Result<StagePlan, StagePlanError> {
    if source_rate == 0 || target_rate == 0 {
        return Err(StagePlanError::InvalidRate);
    }
    let ratio = integer_ratio(source_rate, target_rate).ok_or(StagePlanError::NonIntegerRatio)?;
    if !ratio.is_power_of_two() || !(2..=256).contains(&ratio) {
        return Err(StagePlanError::UnsupportedIntegerRatio(ratio));
    }

    let stage_count = ratio.trailing_zeros() as usize;
    let mut stages = Vec::with_capacity(stage_count);
    stages.push(first_stage_spec(family));
    for idx in 1..stage_count {
        stages.push(cleanup_stage_spec(idx, family));
    }

    let mut latency_ms = 0.0;
    let mut latency_source_frames = 0usize;
    let mut stage_rate = source_rate as f64;
    for (idx, stage) in stages.iter().enumerate() {
        let frames = stage.latency_frames_at_stage_rate();
        latency_ms += frames as f64 / stage_rate * 1000.0;
        latency_source_frames += frames / (1usize << idx);
        stage_rate *= 2.0;
    }

    let estimated_memory_bytes = estimate_plan_memory_bytes(&stages);
    if estimated_memory_bytes > MAX_COEFFICIENT_TABLE_BYTES && !family.is_high_latency() {
        return Err(StagePlanError::MemoryBudgetExceeded {
            bytes: estimated_memory_bytes,
            budget_bytes: MAX_COEFFICIENT_TABLE_BYTES,
        });
    }

    let high_latency = family.is_high_latency() || latency_ms > latency_budget_ms;
    if latency_ms > latency_budget_ms && !family.is_high_latency() {
        return Err(StagePlanError::LatencyBudgetExceeded {
            latency_ms,
            budget_ms: latency_budget_ms,
        });
    }

    Ok(StagePlan {
        stages,
        latency_source_frames,
        latency_ms,
        high_latency,
        estimated_memory_bytes,
    })
}

fn integer_ratio(source_rate: u32, target_rate: u32) -> Option<u32> {
    if source_rate > 0 && target_rate > source_rate && target_rate.is_multiple_of(source_rate) {
        Some(target_rate / source_rate)
    } else {
        None
    }
}

fn exact_rational_bridge_plan(
    filter_type: FilterType,
    source_rate: u32,
    target_rate: u32,
) -> Option<(usize, usize)> {
    if source_rate == 0
        || target_rate == 0
        || source_rate == target_rate
        || !is_standard_audio_family_crossing(source_rate, target_rate)
    {
        return None;
    }

    let gcd = gcd_u32(source_rate, target_rate);
    let step_num = (source_rate / gcd) as usize;
    let phase_den = (target_rate / gcd) as usize;
    if phase_den == 0 || phase_den > MAX_EXACT_RATIONAL_PHASE_DEN {
        return None;
    }

    let half_width =
        PolyphaseResampler::fractional_half_width(filter_type, source_rate, target_rate, phase_den);
    let table_bytes = exact_rational_table_bytes(half_width, phase_den)?;
    (table_bytes <= MAX_POLYPHASE_COEFFICIENT_TABLE_BYTES).then_some((step_num, phase_den))
}

fn exact_rational_table_bytes(half_width: usize, phase_den: usize) -> Option<usize> {
    let num_taps = 2usize.checked_mul(half_width)?.checked_add(1)?;
    phase_den
        .checked_mul(num_taps)?
        .checked_mul(size_of::<f64>())
}

fn is_standard_audio_family_crossing(source_rate: u32, target_rate: u32) -> bool {
    matches!(audio_rate_family(source_rate), Some(44_100) | Some(48_000))
        && matches!(audio_rate_family(target_rate), Some(44_100) | Some(48_000))
}

fn audio_rate_family(rate: u32) -> Option<u32> {
    if rate >= 44_100 && rate.is_multiple_of(44_100) {
        Some(44_100)
    } else if rate >= 48_000 && rate.is_multiple_of(48_000) {
        Some(48_000)
    } else {
        None
    }
}

fn reduced_ratio(source_rate: u32, target_rate: u32) -> (u32, u32) {
    let gcd = gcd_u32(source_rate, target_rate);
    (source_rate / gcd, target_rate / gcd)
}

fn ceil_mul_div_usize(value: usize, multiplier: usize, divisor: usize) -> usize {
    if value == 0 || multiplier == 0 || divisor == 0 {
        return 0;
    }
    let numerator = (value as u128).saturating_mul(multiplier as u128);
    let quotient = numerator.div_ceil(divisor as u128);
    quotient.min(usize::MAX as u128) as usize
}

fn gcd_u32(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a.max(1)
}

fn first_stage_spec(family: FilterType) -> StageSpec {
    let taps_total = match family {
        FilterType::Split128k => 131_073,
        FilterType::IntegratedPhase128k
        | FilterType::IntegratedPhase128kV2
        | FilterType::IntegratedPhase128kV3
        | FilterType::IntegratedPhase128kV4 => INTEGRATED128K_TAPS_TOTAL,
        FilterType::MinimumPhase128k
        | FilterType::MinimumPhase128kV2
        | FilterType::MinimumPhase128kV3
        | FilterType::MinimumPhase128kV4 => MINIMUM128K_TAPS_TOTAL,
        FilterType::MinimumPhaseCompact128k
        | FilterType::MinimumPhaseCompact128kV2
        | FilterType::SmoothPhase128k => MINIMUM_COMPACT_BRANCH_TAPS,
        FilterType::LinearPhase128k => LINEAR128K_TAPS_TOTAL,
        FilterType::SincExtreme32k => 32769,
        FilterType::Minimum16k => 16_385,
    };
    let engine = match family {
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
        | FilterType::SmoothPhase128k => EngineKind::PartitionedFft {
            partition_frames: 4096,
        },
        FilterType::SincExtreme32k => EngineKind::PartitionedFft {
            partition_frames: 2048,
        },
    };
    StageSpec::Character2x {
        taps_total,
        cutoff: family.cutoff(),
        beta: family
            .character_beta()
            .unwrap_or(MINIMUM_COMPACT_CLEANUP_BETA),
        engine,
        phase_mode: phase_mode_for_filter(family),
    }
}

fn phase_mode_for_filter(family: FilterType) -> PhaseMode {
    match family {
        FilterType::Split128k => PhaseMode::SplitPhase128k,
        filter @ (FilterType::IntegratedPhase128k
        | FilterType::IntegratedPhase128kV2
        | FilterType::IntegratedPhase128kV3
        | FilterType::IntegratedPhase128kV4) => PhaseMode::IntegratedPhase128k(
            filter
                .integrated_phase_profile()
                .expect("Integrated Phase filter must have a profile"),
        ),
        filter @ (FilterType::MinimumPhase128k
        | FilterType::MinimumPhase128kV2
        | FilterType::MinimumPhase128kV3
        | FilterType::MinimumPhase128kV4) => PhaseMode::MinimumPhase128k(
            filter
                .minimum_phase128k_profile()
                .expect("Minimum Phase 128k filter must have a profile"),
        ),
        FilterType::MinimumPhaseCompact128kV2 => {
            PhaseMode::MinimumPhase128k(MinimumPhase128kProfile::One)
        }
        filter @ (FilterType::MinimumPhaseCompact128k | FilterType::SmoothPhase128k) => {
            PhaseMode::MinimumPhaseCompact128k(
                filter
                    .minimum_compact_profile()
                    .expect("Minimum Phase Compact filter must have a profile"),
            )
        }
        FilterType::Minimum16k => PhaseMode::Minimum,
        FilterType::SincExtreme32k | FilterType::LinearPhase128k => PhaseMode::Linear,
    }
}

fn cleanup_stage_spec(stage_idx: usize, family: FilterType) -> StageSpec {
    // Tap-count taper. Stages ≥ 3 in a long cascade (≥ 32×) suppress images
    // sitting well above the audible band — typically > 1.4 MHz — so a short
    // kernel is plenty. Keeping the count ≤ MAX_SIMD_DIRECT_TAPS (257) lets
    // the AVX2/FMA direct path stay engaged for every late stage.
    let taps_total = match family {
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
        | FilterType::SmoothPhase128k => match stage_idx {
            1 => 255,
            2 => 127,
            3 => 63,
            _ => 31,
        },
        FilterType::SincExtreme32k => match stage_idx {
            1 => 127,
            2 => 63,
            3 | 4 => 31,
            _ => 15,
        },
    };
    StageSpec::CleanupHalfband2x {
        taps_total,
        beta: family.cleanup_beta(),
        // Long apodizing filters deliberately keep cleanups at 0.5: their
        // character stage already provides the anti-image margin, so cleanups
        // can take the true-halfband structure (even branch an exact delay).
        cutoff: 0.5,
        engine: EngineKind::DirectSimd,
    }
}

fn estimate_plan_memory_bytes(stages: &[StageSpec]) -> usize {
    stages
        .iter()
        .map(|stage| {
            let coeff_bytes = match stage.engine() {
                EngineKind::DirectSimd => match stage {
                    StageSpec::Character2x { taps_total, .. } => taps_total * 2 * size_of::<f64>(),
                    StageSpec::CleanupHalfband2x { taps_total, .. } => {
                        taps_total * size_of::<f64>()
                    }
                },
                EngineKind::PartitionedFft { .. } => 0,
            };
            let fft_bytes = match stage.engine() {
                EngineKind::DirectSimd => 0,
                EngineKind::PartitionedFft { partition_frames } => {
                    let fft_len = (partition_frames + stage.taps_total() - 1).next_power_of_two();
                    stage.engine_count()
                        * 2
                        * (fft_len * size_of::<f64>() + (fft_len / 2 + 1) * size_of::<Complex64>())
                }
            };
            coeff_bytes + fft_bytes
        })
        .sum()
}

trait FirEngine {
    fn reset(&mut self);
    fn process_stereo(
        &mut self,
        input_l: &[f64],
        input_r: &[f64],
        output_l: &mut Vec<f64>,
        output_r: &mut Vec<f64>,
    );
}

struct DirectFirEngine {
    coeffs: Vec<f64>,
    /// Number of zero samples pre-loaded into the buffer on reset. For a
    /// symmetric (linear-phase) kernel this is `coeffs.len() / 2`, so the
    /// kernel's center aligns with input sample 0 and the engine has zero
    /// net group delay. For a causal min-phase kernel (where `h[0]` is the
    /// dominant tap, stored at `coeffs[taps-1]` because the engine
    /// convolves oldest-sample × `coeffs[0]`), this is `coeffs.len() - 1`,
    /// so the dominant tap aligns with input sample 0 on the very first
    /// output.
    prepad: usize,
    prefer_simd: bool,
    buffer_l: Vec<f64>,
    buffer_r: Vec<f64>,
}

impl DirectFirEngine {
    fn with_prepad(coeffs: Vec<f64>, prefer_simd: bool, prepad: Option<usize>) -> Self {
        let prepad = prepad.unwrap_or(coeffs.len() / 2);
        let mut engine = Self {
            coeffs,
            prepad,
            prefer_simd,
            buffer_l: Vec::new(),
            buffer_r: Vec::new(),
        };
        engine.reset();
        engine
    }

    fn convolve_scalar(samples: &[f64], coeffs: &[f64]) -> f64 {
        samples.iter().zip(coeffs).map(|(&s, &c)| s * c).sum()
    }

    fn convolve_fast(&self, samples: &[f64]) -> f64 {
        if self.prefer_simd {
            convolve_simd_or_scalar(samples, &self.coeffs)
        } else {
            Self::convolve_scalar(samples, &self.coeffs)
        }
    }
}

impl FirEngine for DirectFirEngine {
    fn reset(&mut self) {
        self.buffer_l.clear();
        self.buffer_r.clear();
        self.buffer_l.resize(self.prepad, 0.0);
        self.buffer_r.resize(self.prepad, 0.0);
    }

    fn process_stereo(
        &mut self,
        input_l: &[f64],
        input_r: &[f64],
        output_l: &mut Vec<f64>,
        output_r: &mut Vec<f64>,
    ) {
        self.buffer_l.extend_from_slice(input_l);
        self.buffer_r.extend_from_slice(input_r);

        let input_len = self.buffer_l.len();
        let taps = self.coeffs.len();
        if input_len < taps {
            return;
        }

        let frames = input_len - (taps - 1);
        output_l.reserve(frames);
        output_r.reserve(frames);
        for start in 0..frames {
            output_l.push(self.convolve_fast(&self.buffer_l[start..start + taps]));
            output_r.push(self.convolve_fast(&self.buffer_r[start..start + taps]));
        }

        if frames > 0 {
            self.buffer_l.copy_within(frames.., 0);
            self.buffer_l.truncate(self.buffer_l.len() - frames);
            self.buffer_r.copy_within(frames.., 0);
            self.buffer_r.truncate(self.buffer_r.len() - frames);
        }
    }
}

fn convolve_simd_or_scalar(samples: &[f64], coeffs: &[f64]) -> f64 {
    #[cfg(target_arch = "aarch64")]
    {
        // AArch64 guarantees Advanced SIMD/FP for the targets this crate ships
        // on, so no runtime feature probe is needed.
        unsafe { convolve_neon_f64(samples, coeffs) }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            // FMA fuses mul+add into a single rounded op: ~2x throughput vs.
            // separate _mm256_mul_pd + _mm256_add_pd, plus better precision
            // (one rounding instead of two). Every x86-64 CPU shipped since
            // ~2013 with AVX2 also has FMA3, but they're distinct CPU
            // features so they're detected separately.
            if std::arch::is_x86_feature_detected!("avx2")
                && std::arch::is_x86_feature_detected!("fma")
            {
                // SAFETY: the kernel only uses unaligned loads within slice bounds,
                // and this branch is guarded by runtime CPU feature detection.
                return unsafe { convolve_avx2_fma_f64(samples, coeffs) };
            }
            if std::arch::is_x86_feature_detected!("avx2") {
                // SAFETY: same as above; AVX2-only fallback for the rare CPU
                // with AVX2 but no FMA.
                return unsafe { convolve_avx2_f64(samples, coeffs) };
            }
        }
        DirectFirEngine::convolve_scalar(samples, coeffs)
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn convolve_neon_f64(samples: &[f64], coeffs: &[f64]) -> f64 {
    use std::arch::aarch64::*;

    let mut i = 0;
    let len = samples.len().min(coeffs.len());
    // SAFETY: this kernel is only called on AArch64 targets where NEON is available.
    let (mut acc0, mut acc1, mut acc2, mut acc3) = unsafe {
        (
            vdupq_n_f64(0.0),
            vdupq_n_f64(0.0),
            vdupq_n_f64(0.0),
            vdupq_n_f64(0.0),
        )
    };
    let chunks = len / 8 * 8;

    while i < chunks {
        // SAFETY: `chunks` is capped at the shared slice length and advances in complete SIMD lanes.
        let (s0, c0, s1, c1, s2, c2, s3, c3) = unsafe {
            (
                vld1q_f64(samples.as_ptr().add(i)),
                vld1q_f64(coeffs.as_ptr().add(i)),
                vld1q_f64(samples.as_ptr().add(i + 2)),
                vld1q_f64(coeffs.as_ptr().add(i + 2)),
                vld1q_f64(samples.as_ptr().add(i + 4)),
                vld1q_f64(coeffs.as_ptr().add(i + 4)),
                vld1q_f64(samples.as_ptr().add(i + 6)),
                vld1q_f64(coeffs.as_ptr().add(i + 6)),
            )
        };
        // SAFETY: this kernel is only called on AArch64 targets where NEON is available.
        (acc0, acc1, acc2, acc3) = unsafe {
            (
                vfmaq_f64(acc0, s0, c0),
                vfmaq_f64(acc1, s1, c1),
                vfmaq_f64(acc2, s2, c2),
                vfmaq_f64(acc3, s3, c3),
            )
        };
        i += 8;
    }

    // SAFETY: this kernel is only called on AArch64 targets where NEON is available.
    let acc = unsafe { vaddq_f64(vaddq_f64(acc0, acc1), vaddq_f64(acc2, acc3)) };
    // SAFETY: this kernel is only called on AArch64 targets where NEON is available.
    let mut sum = unsafe { vgetq_lane_f64::<0>(acc) + vgetq_lane_f64::<1>(acc) };
    for idx in i..len {
        sum += samples[idx] * coeffs[idx];
    }
    sum
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn convolve_avx2_fma_f64(samples: &[f64], coeffs: &[f64]) -> f64 {
    use std::arch::x86_64::*;

    let mut i = 0;
    let len = samples.len().min(coeffs.len());
    let mut acc0 = _mm256_setzero_pd();
    let mut acc1 = _mm256_setzero_pd();
    let chunks = len / 8 * 8;

    while i < chunks {
        // SAFETY: `chunks` is capped at the shared slice length and AVX2/FMA are enabled for this fn.
        let (s0, c0, s1, c1) = unsafe {
            (
                _mm256_loadu_pd(samples.as_ptr().add(i)),
                _mm256_loadu_pd(coeffs.as_ptr().add(i)),
                _mm256_loadu_pd(samples.as_ptr().add(i + 4)),
                _mm256_loadu_pd(coeffs.as_ptr().add(i + 4)),
            )
        };
        acc0 = _mm256_fmadd_pd(s0, c0, acc0);
        acc1 = _mm256_fmadd_pd(s1, c1, acc1);
        i += 8;
    }

    let acc = _mm256_add_pd(acc0, acc1);
    let mut lanes = [0.0f64; 4];
    // SAFETY: `lanes` has exactly one unaligned AVX register worth of f64 storage.
    unsafe { _mm256_storeu_pd(lanes.as_mut_ptr(), acc) };
    let mut sum = lanes.iter().sum::<f64>();
    for idx in i..len {
        sum += samples[idx] * coeffs[idx];
    }
    sum
}

#[cfg(target_arch = "x86")]
#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn convolve_avx2_fma_f64(samples: &[f64], coeffs: &[f64]) -> f64 {
    use std::arch::x86::*;

    let mut i = 0;
    let len = samples.len().min(coeffs.len());
    let mut acc0 = _mm256_setzero_pd();
    let mut acc1 = _mm256_setzero_pd();
    let chunks = len / 8 * 8;

    while i < chunks {
        // SAFETY: `chunks` is capped at the shared slice length and AVX2/FMA are enabled for this fn.
        let (s0, c0, s1, c1) = unsafe {
            (
                _mm256_loadu_pd(samples.as_ptr().add(i)),
                _mm256_loadu_pd(coeffs.as_ptr().add(i)),
                _mm256_loadu_pd(samples.as_ptr().add(i + 4)),
                _mm256_loadu_pd(coeffs.as_ptr().add(i + 4)),
            )
        };
        acc0 = _mm256_fmadd_pd(s0, c0, acc0);
        acc1 = _mm256_fmadd_pd(s1, c1, acc1);
        i += 8;
    }

    let acc = _mm256_add_pd(acc0, acc1);
    let mut lanes = [0.0f64; 4];
    // SAFETY: `lanes` has exactly one unaligned AVX register worth of f64 storage.
    unsafe { _mm256_storeu_pd(lanes.as_mut_ptr(), acc) };
    let mut sum = lanes.iter().sum::<f64>();
    for idx in i..len {
        sum += samples[idx] * coeffs[idx];
    }
    sum
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn convolve_avx2_f64(samples: &[f64], coeffs: &[f64]) -> f64 {
    use std::arch::x86_64::*;

    let mut i = 0;
    let len = samples.len().min(coeffs.len());
    let mut acc0 = _mm256_setzero_pd();
    let mut acc1 = _mm256_setzero_pd();
    let chunks = len / 8 * 8;

    while i < chunks {
        // SAFETY: `chunks` is capped at the shared slice length and AVX2 is enabled for this fn.
        let (s0, c0, s1, c1) = unsafe {
            (
                _mm256_loadu_pd(samples.as_ptr().add(i)),
                _mm256_loadu_pd(coeffs.as_ptr().add(i)),
                _mm256_loadu_pd(samples.as_ptr().add(i + 4)),
                _mm256_loadu_pd(coeffs.as_ptr().add(i + 4)),
            )
        };
        acc0 = _mm256_add_pd(acc0, _mm256_mul_pd(s0, c0));
        acc1 = _mm256_add_pd(acc1, _mm256_mul_pd(s1, c1));
        i += 8;
    }

    let acc = _mm256_add_pd(acc0, acc1);
    let mut lanes = [0.0f64; 4];
    // SAFETY: `lanes` has exactly one unaligned AVX register worth of f64 storage.
    unsafe { _mm256_storeu_pd(lanes.as_mut_ptr(), acc) };
    let mut sum = lanes.iter().sum::<f64>();
    for idx in i..len {
        sum += samples[idx] * coeffs[idx];
    }
    sum
}

#[cfg(target_arch = "x86")]
#[target_feature(enable = "avx2")]
unsafe fn convolve_avx2_f64(samples: &[f64], coeffs: &[f64]) -> f64 {
    use std::arch::x86::*;

    let mut i = 0;
    let len = samples.len().min(coeffs.len());
    let mut acc0 = _mm256_setzero_pd();
    let mut acc1 = _mm256_setzero_pd();
    let chunks = len / 8 * 8;

    while i < chunks {
        // SAFETY: `chunks` is capped at the shared slice length and AVX2 is enabled for this fn.
        let (s0, c0, s1, c1) = unsafe {
            (
                _mm256_loadu_pd(samples.as_ptr().add(i)),
                _mm256_loadu_pd(coeffs.as_ptr().add(i)),
                _mm256_loadu_pd(samples.as_ptr().add(i + 4)),
                _mm256_loadu_pd(coeffs.as_ptr().add(i + 4)),
            )
        };
        acc0 = _mm256_add_pd(acc0, _mm256_mul_pd(s0, c0));
        acc1 = _mm256_add_pd(acc1, _mm256_mul_pd(s1, c1));
        i += 8;
    }

    let acc = _mm256_add_pd(acc0, acc1);
    let mut lanes = [0.0f64; 4];
    // SAFETY: `lanes` has exactly one unaligned AVX register worth of f64 storage.
    unsafe { _mm256_storeu_pd(lanes.as_mut_ptr(), acc) };
    let mut sum = lanes.iter().sum::<f64>();
    for idx in i..len {
        sum += samples[idx] * coeffs[idx];
    }
    sum
}

struct BlockFftFirEngine {
    taps: usize,
    /// See `DirectFirEngine::prepad`.
    prepad: usize,
    partition_frames: usize,
    fft_len: usize,
    forward: Arc<dyn RealToComplex<f64>>,
    inverse: Arc<dyn ComplexToReal<f64>>,
    kernel_spectrum: Vec<Complex64>,
    buffer_l: Vec<f64>,
    buffer_r: Vec<f64>,
    fft_input: Vec<f64>,
    spectrum: Vec<Complex64>,
    time: Vec<f64>,
}

impl BlockFftFirEngine {
    fn with_prepad(coeffs: Vec<f64>, partition_frames: usize, prepad: Option<usize>) -> Self {
        let taps = coeffs.len();
        let prepad = prepad.unwrap_or(taps / 2);
        let fft_len = (partition_frames + taps - 1).next_power_of_two();
        let mut planner = RealFftPlanner::<f64>::new();
        let forward = planner.plan_fft_forward(fft_len);
        let inverse = planner.plan_fft_inverse(fft_len);

        let mut kernel_input = vec![0.0; fft_len];
        for (idx, coeff) in coeffs.iter().rev().enumerate() {
            kernel_input[idx] = *coeff;
        }
        let mut kernel_spectrum = forward.make_output_vec();
        forward
            .process(&mut kernel_input, &mut kernel_spectrum)
            .expect("FFT kernel planning should produce compatible buffers");

        let mut engine = Self {
            taps,
            prepad,
            partition_frames,
            fft_len,
            forward,
            inverse,
            kernel_spectrum,
            buffer_l: Vec::new(),
            buffer_r: Vec::new(),
            fft_input: vec![0.0; fft_len],
            spectrum: Vec::new(),
            time: Vec::new(),
        };
        engine.spectrum = engine.forward.make_output_vec();
        engine.time = engine.inverse.make_output_vec();
        engine.reset();
        engine
    }

    fn process_prepared_channel(&mut self, output: &mut Vec<f64>) {
        self.forward
            .process(&mut self.fft_input, &mut self.spectrum)
            .expect("FFT input/output buffers should match the planned size");
        for (bin, kernel) in self.spectrum.iter_mut().zip(&self.kernel_spectrum) {
            *bin *= *kernel;
        }

        self.inverse
            .process(&mut self.spectrum, &mut self.time)
            .expect("inverse FFT buffers should match the planned size");
        let scale = 1.0 / self.fft_len as f64;
        let offset = self.taps - 1;
        output.reserve(self.partition_frames);
        for sample in &self.time[offset..offset + self.partition_frames] {
            output.push(*sample * scale);
        }
    }

    fn prepare_fft_input_from(buffer: &[f64], needed: usize, fft_input: &mut [f64]) {
        fft_input.fill(0.0);
        fft_input[..needed].copy_from_slice(&buffer[..needed]);
    }
}

impl FirEngine for BlockFftFirEngine {
    fn reset(&mut self) {
        self.buffer_l.clear();
        self.buffer_r.clear();
        self.buffer_l.resize(self.prepad, 0.0);
        self.buffer_r.resize(self.prepad, 0.0);
    }

    fn process_stereo(
        &mut self,
        input_l: &[f64],
        input_r: &[f64],
        output_l: &mut Vec<f64>,
        output_r: &mut Vec<f64>,
    ) {
        self.buffer_l.extend_from_slice(input_l);
        self.buffer_r.extend_from_slice(input_r);

        let needed = self.partition_frames + self.taps - 1;
        debug_assert!(
            self.partition_frames <= self.taps,
            "overlap-save extraction assumes partition size does not exceed FIR length"
        );
        while self.buffer_l.len() >= needed {
            Self::prepare_fft_input_from(&self.buffer_l, needed, &mut self.fft_input);
            self.process_prepared_channel(output_l);
            Self::prepare_fft_input_from(&self.buffer_r, needed, &mut self.fft_input);
            self.process_prepared_channel(output_r);
            self.buffer_l.copy_within(self.partition_frames.., 0);
            self.buffer_l
                .truncate(self.buffer_l.len() - self.partition_frames);
            self.buffer_r.copy_within(self.partition_frames.., 0);
            self.buffer_r
                .truncate(self.buffer_r.len() - self.partition_frames);
        }
    }
}

struct TwoXStage {
    phase0: Option<Box<dyn FirEngine>>,
    phase1: Box<dyn FirEngine>,
    even_delay_l: Vec<f64>,
    even_delay_r: Vec<f64>,
    half_width: usize,
    scratch_even_l: Vec<f64>,
    scratch_even_r: Vec<f64>,
    scratch_odd_l: Vec<f64>,
    scratch_odd_r: Vec<f64>,
}

impl TwoXStage {
    fn new(spec: &StageSpec) -> Self {
        let (phase0, phase1, half_width) = match spec {
            StageSpec::Character2x {
                taps_total,
                cutoff,
                beta,
                engine,
                phase_mode,
            } => {
                let half_width = taps_total / 2;
                let (phase0_coeffs, phase1_coeffs, prepad0, prepad1) =
                    build_character_polyphase_pair(half_width, *beta, *cutoff, *phase_mode);
                (
                    Some(build_engine_with_prepad(phase0_coeffs, *engine, prepad0)),
                    build_engine_with_prepad(phase1_coeffs, *engine, prepad1),
                    half_width,
                )
            }
            StageSpec::CleanupHalfband2x {
                taps_total,
                beta,
                cutoff,
                engine,
            } => {
                let half_width = taps_total / 2;
                let phase1_coeffs = build_phase_coefficients(half_width, 0.5, *beta, *cutoff);
                (None, build_engine(phase1_coeffs, *engine), half_width)
            }
        };

        let mut stage = Self {
            phase0,
            phase1,
            even_delay_l: Vec::new(),
            even_delay_r: Vec::new(),
            half_width,
            scratch_even_l: Vec::new(),
            scratch_even_r: Vec::new(),
            scratch_odd_l: Vec::new(),
            scratch_odd_r: Vec::new(),
        };
        stage.reset();
        stage
    }

    fn input(&mut self, input_l: &[f64], input_r: &[f64]) {
        // Mixed-phase origins can land on an odd full-rate sample, giving one
        // polyphase branch a one-frame prepad advantage. Preserve that pending
        // output across calls; clearing it here makes the result depend on the
        // caller's chunk boundaries and drops one branch sample per block.
        let pending_odd_frames = self.scratch_odd_l.len().min(self.scratch_odd_r.len());
        self.phase1.process_stereo(
            input_l,
            input_r,
            &mut self.scratch_odd_l,
            &mut self.scratch_odd_r,
        );

        if let Some(phase0) = &mut self.phase0 {
            phase0.process_stereo(
                input_l,
                input_r,
                &mut self.scratch_even_l,
                &mut self.scratch_even_r,
            );
        } else {
            self.even_delay_l.extend_from_slice(input_l);
            self.even_delay_r.extend_from_slice(input_r);
            let frames = self
                .scratch_odd_l
                .len()
                .min(self.scratch_odd_r.len())
                .saturating_sub(pending_odd_frames);
            if self.even_delay_l.len() >= self.half_width + frames {
                self.scratch_even_l.extend_from_slice(
                    &self.even_delay_l[self.half_width..self.half_width + frames],
                );
                self.scratch_even_r.extend_from_slice(
                    &self.even_delay_r[self.half_width..self.half_width + frames],
                );
                if frames > 0 {
                    self.even_delay_l.copy_within(frames.., 0);
                    self.even_delay_l.truncate(self.even_delay_l.len() - frames);
                    self.even_delay_r.copy_within(frames.., 0);
                    self.even_delay_r.truncate(self.even_delay_r.len() - frames);
                }
            }
        }
    }

    fn process(&mut self, output_l: &mut Vec<f64>, output_r: &mut Vec<f64>) -> usize {
        let frames = self
            .scratch_even_l
            .len()
            .min(self.scratch_even_r.len())
            .min(self.scratch_odd_l.len())
            .min(self.scratch_odd_r.len());
        output_l.reserve(frames * 2);
        output_r.reserve(frames * 2);
        for idx in 0..frames {
            output_l.push(self.scratch_even_l[idx]);
            output_r.push(self.scratch_even_r[idx]);
            output_l.push(self.scratch_odd_l[idx]);
            output_r.push(self.scratch_odd_r[idx]);
        }
        // Consume only paired frames. Any one-frame branch lead remains queued
        // until the other phase produces its matching output.
        self.scratch_even_l.drain(..frames);
        self.scratch_even_r.drain(..frames);
        self.scratch_odd_l.drain(..frames);
        self.scratch_odd_r.drain(..frames);
        frames
    }

    fn reset(&mut self) {
        if let Some(phase0) = &mut self.phase0 {
            phase0.reset();
        }
        self.phase1.reset();
        self.even_delay_l.clear();
        self.even_delay_r.clear();
        if self.phase0.is_none() {
            self.even_delay_l.resize(self.half_width, 0.0);
            self.even_delay_r.resize(self.half_width, 0.0);
        }
        self.scratch_even_l.clear();
        self.scratch_even_r.clear();
        self.scratch_odd_l.clear();
        self.scratch_odd_r.clear();
    }
}

fn build_engine(coeffs: Vec<f64>, engine: EngineKind) -> Box<dyn FirEngine> {
    build_engine_with_prepad(coeffs, engine, None)
}

fn build_engine_with_prepad(
    coeffs: Vec<f64>,
    engine: EngineKind,
    prepad: Option<usize>,
) -> Box<dyn FirEngine> {
    match engine {
        EngineKind::DirectSimd => {
            let use_simd = coeffs.len() <= MAX_SIMD_DIRECT_TAPS;
            Box::new(DirectFirEngine::with_prepad(coeffs, use_simd, prepad))
        }
        EngineKind::PartitionedFft { partition_frames } => Box::new(
            BlockFftFirEngine::with_prepad(coeffs, partition_frames, prepad),
        ),
    }
}

struct IntegerCascade {
    plan: StagePlan,
    stages: Vec<TwoXStage>,
    scratch_l: Vec<f64>,
    scratch_r: Vec<f64>,
    stage_out_l: Vec<f64>,
    stage_out_r: Vec<f64>,
}

impl IntegerCascade {
    fn new(plan: StagePlan) -> Self {
        let stages = plan.stages.iter().map(TwoXStage::new).collect();
        Self {
            plan,
            stages,
            scratch_l: Vec::new(),
            scratch_r: Vec::new(),
            stage_out_l: Vec::new(),
            stage_out_r: Vec::new(),
        }
    }

    fn input(&mut self, samples_l: &[f64], samples_r: &[f64]) {
        if let Some(first) = self.stages.first_mut() {
            first.input(samples_l, samples_r);
        }
    }

    fn process(&mut self, output: &mut Vec<f64>) -> usize {
        if self.stages.is_empty() {
            return 0;
        }

        self.scratch_l.clear();
        self.scratch_r.clear();
        self.stages[0].process(&mut self.scratch_l, &mut self.scratch_r);

        for stage_idx in 1..self.stages.len() {
            if self.scratch_l.is_empty() {
                return 0;
            }
            self.stages[stage_idx].input(&self.scratch_l, &self.scratch_r);
            self.stage_out_l.clear();
            self.stage_out_r.clear();
            self.stages[stage_idx].process(&mut self.stage_out_l, &mut self.stage_out_r);
            std::mem::swap(&mut self.scratch_l, &mut self.stage_out_l);
            std::mem::swap(&mut self.scratch_r, &mut self.stage_out_r);
        }

        let frames = self.scratch_l.len().min(self.scratch_r.len());
        output.reserve(frames * 2);
        for idx in 0..frames {
            output.push(self.scratch_l[idx]);
            output.push(self.scratch_r[idx]);
        }
        frames
    }

    fn reset(&mut self) {
        for stage in &mut self.stages {
            stage.reset();
        }
        self.scratch_l.clear();
        self.scratch_r.clear();
        self.stage_out_l.clear();
        self.stage_out_r.clear();
    }

    fn flush_lookahead_source_frames(&self) -> usize {
        self.plan.latency_source_frames.saturating_add(2)
    }
}

struct DownsampleChain {
    steps: Vec<DownsampleStep>,
    source_rate: u32,
    #[cfg(test)]
    target_rate: u32,
    latency_ms: f64,
    high_latency: bool,
    estimated_memory_bytes: usize,
    scratch: Vec<f64>,
    stage_out: Vec<f64>,
    plane_l: Vec<f64>,
    plane_r: Vec<f64>,
}

impl DownsampleChain {
    fn new(filter_type: FilterType, source_rate: u32, target_rate: u32) -> Option<Self> {
        if source_rate == 0 || target_rate == 0 || source_rate <= target_rate {
            return None;
        }

        let steps = build_downsample_steps(filter_type, source_rate, target_rate)?;
        if steps.is_empty() {
            return None;
        }

        let latency_ms = steps.iter().map(DownsampleStep::latency_ms).sum();
        let estimated_memory_bytes = steps
            .iter()
            .map(DownsampleStep::estimated_memory_bytes)
            .sum();
        let high_latency = filter_type.is_high_latency() || latency_ms > DEFAULT_LATENCY_BUDGET_MS;

        Some(Self {
            steps,
            source_rate,
            #[cfg(test)]
            target_rate,
            latency_ms,
            high_latency,
            estimated_memory_bytes,
            scratch: Vec::new(),
            stage_out: Vec::new(),
            plane_l: Vec::new(),
            plane_r: Vec::new(),
        })
    }

    fn input(&mut self, samples_l: &[f64], samples_r: &[f64]) {
        if let Some(first) = self.steps.first_mut() {
            first.input(samples_l, samples_r);
        }
    }

    fn process(&mut self, output: &mut Vec<f64>) -> usize {
        let Some(first) = self.steps.first_mut() else {
            return 0;
        };

        self.scratch.clear();
        first.process(&mut self.scratch);

        for idx in 1..self.steps.len() {
            if !self.scratch.is_empty() {
                feed_interleaved_to_downsample_step(
                    &mut self.steps[idx],
                    &self.scratch,
                    &mut self.plane_l,
                    &mut self.plane_r,
                );
            }
            self.stage_out.clear();
            self.steps[idx].process(&mut self.stage_out);
            std::mem::swap(&mut self.scratch, &mut self.stage_out);
        }

        let frames = self.scratch.len() / 2;
        output.extend_from_slice(&self.scratch);
        frames
    }

    fn reset(&mut self) {
        for step in &mut self.steps {
            step.reset();
        }
        self.scratch.clear();
        self.stage_out.clear();
        self.plane_l.clear();
        self.plane_r.clear();
    }

    fn flush_lookahead_source_frames(&self, fallback_source_rate: u32) -> usize {
        let source_rate = if self.source_rate != 0 {
            self.source_rate
        } else {
            fallback_source_rate
        }
        .max(1);
        (self.latency_ms / 1000.0 * source_rate as f64).ceil() as usize + 2
    }
}

fn feed_interleaved_to_downsample_step(
    step: &mut DownsampleStep,
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
    step.input(plane_l, plane_r);
}

fn build_downsample_steps(
    filter_type: FilterType,
    source_rate: u32,
    target_rate: u32,
) -> Option<Vec<DownsampleStep>> {
    build_same_family_downsample_steps(filter_type, source_rate, target_rate)
}

fn build_same_family_downsample_steps(
    filter_type: FilterType,
    source_rate: u32,
    target_rate: u32,
) -> Option<Vec<DownsampleStep>> {
    if !valid_power_two_downsample(source_rate, target_rate) {
        return None;
    }

    let plan = build_integer_stage_plan(
        target_rate,
        source_rate,
        filter_type,
        DEFAULT_LATENCY_BUDGET_MS,
    )
    .ok()?;

    let mut input_rate = source_rate;
    let mut steps = Vec::with_capacity(plan.stages.len());
    for spec in plan.stages.iter().rev() {
        steps.push(DownsampleStep::Decimate2(DecimateBy2Stage::new(
            filter_type,
            spec,
            input_rate,
        )));
        input_rate /= 2;
    }
    Some(steps)
}

fn valid_power_two_downsample(source_rate: u32, target_rate: u32) -> bool {
    if source_rate <= target_rate || target_rate == 0 || !source_rate.is_multiple_of(target_rate) {
        return false;
    }
    let ratio = source_rate / target_rate;
    ratio.is_power_of_two() && (2..=256).contains(&ratio)
}

enum DownsampleStep {
    Decimate2(DecimateBy2Stage),
}

impl DownsampleStep {
    fn input(&mut self, samples_l: &[f64], samples_r: &[f64]) {
        match self {
            Self::Decimate2(stage) => stage.input(samples_l, samples_r),
        }
    }

    fn process(&mut self, output: &mut Vec<f64>) -> usize {
        match self {
            Self::Decimate2(stage) => stage.process(output),
        }
    }

    fn reset(&mut self) {
        match self {
            Self::Decimate2(stage) => stage.reset(),
        }
    }

    fn latency_ms(&self) -> f64 {
        match self {
            Self::Decimate2(stage) => stage.latency_ms,
        }
    }

    fn estimated_memory_bytes(&self) -> usize {
        match self {
            Self::Decimate2(stage) => stage.estimated_memory_bytes,
        }
    }
}

struct DecimateBy2Stage {
    #[cfg(test)]
    filter_type: FilterType,
    #[cfg(test)]
    spec: StageSpec,
    #[cfg(test)]
    input_rate: u32,
    #[cfg(test)]
    output_rate: u32,
    engine: Box<dyn FirEngine>,
    next_filtered_parity: usize,
    filtered_l: Vec<f64>,
    filtered_r: Vec<f64>,
    latency_ms: f64,
    estimated_memory_bytes: usize,
}

impl DecimateBy2Stage {
    fn new(filter_type: FilterType, spec: &StageSpec, input_rate: u32) -> Self {
        #[cfg(not(test))]
        let _ = filter_type;

        let (coeffs, prepad) = build_decimation_coefficients(spec);
        let taps = coeffs.len();
        let engine = spec.engine();
        let latency_frames = taps
            .saturating_sub(1)
            .saturating_sub(prepad)
            .saturating_add(engine_partition_frames(engine));
        let latency_ms = latency_frames as f64 / input_rate.max(1) as f64 * 1000.0;
        let estimated_memory_bytes = estimate_single_engine_memory_bytes(taps, engine);

        Self {
            #[cfg(test)]
            filter_type,
            #[cfg(test)]
            spec: spec.clone(),
            #[cfg(test)]
            input_rate,
            #[cfg(test)]
            output_rate: input_rate / 2,
            engine: build_engine_with_prepad(coeffs, engine, Some(prepad)),
            next_filtered_parity: 0,
            filtered_l: Vec::new(),
            filtered_r: Vec::new(),
            latency_ms,
            estimated_memory_bytes,
        }
    }

    fn input(&mut self, samples_l: &[f64], samples_r: &[f64]) {
        self.filtered_l.clear();
        self.filtered_r.clear();
        self.engine.process_stereo(
            samples_l,
            samples_r,
            &mut self.filtered_l,
            &mut self.filtered_r,
        );
    }

    fn process(&mut self, output: &mut Vec<f64>) -> usize {
        let frames = self.filtered_l.len().min(self.filtered_r.len());
        output.reserve(frames * 2);
        let mut frames_written = 0;
        for idx in 0..frames {
            if self.next_filtered_parity == 0 {
                output.push(self.filtered_l[idx]);
                output.push(self.filtered_r[idx]);
                frames_written += 1;
            }
            self.next_filtered_parity ^= 1;
        }
        self.filtered_l.clear();
        self.filtered_r.clear();
        frames_written
    }

    fn reset(&mut self) {
        self.engine.reset();
        self.next_filtered_parity = 0;
        self.filtered_l.clear();
        self.filtered_r.clear();
    }
}

fn build_decimation_coefficients(spec: &StageSpec) -> (Vec<f64>, usize) {
    let (mut impulse, origin) = match spec {
        StageSpec::Character2x {
            taps_total,
            cutoff,
            beta,
            phase_mode,
            ..
        } => {
            let half_width = taps_total / 2;
            let proto = build_full_rate_2x_prototype(half_width, *beta, *cutoff);
            let impulse = match phase_mode {
                PhaseMode::Linear => proto,
                PhaseMode::Minimum => minimum_phase_impulse(&proto),
                PhaseMode::MinimumPhase128k(_) => {
                    minimum_phase_impulse_with_params(&proto, minimum128k_phase_params())
                }
                PhaseMode::MinimumPhaseCompact128k(profile) => {
                    minimum_phase_compact_impulse(*profile)
                }
                PhaseMode::SplitPhase128k => {
                    split_phase_impulse_with_params(&proto, split128k_phase_params())
                }
                PhaseMode::IntegratedPhase128k(profile) => integrated_phase_impulse_with_params(
                    &proto,
                    integrated128k_phase_params(*profile),
                ),
            };
            let origin = dominant_impulse_index(&impulse);
            (impulse, origin)
        }
        StageSpec::CleanupHalfband2x {
            taps_total,
            beta,
            cutoff,
            ..
        } => {
            let half_width = taps_total / 2;
            let impulse = build_full_rate_2x_prototype(half_width, *beta, *cutoff);
            let origin = dominant_impulse_index(&impulse);
            (impulse, origin)
        }
    };

    let prepad = prepad_for_full_rate_origin(impulse.len(), origin);
    impulse.reverse();
    normalize_coefficients(&mut impulse);
    (impulse, prepad)
}

fn prepad_for_full_rate_origin(taps: usize, origin: usize) -> usize {
    taps.saturating_sub(1)
        .saturating_sub(origin.min(taps.saturating_sub(1)))
}

fn engine_partition_frames(engine: EngineKind) -> usize {
    match engine {
        EngineKind::DirectSimd => 0,
        EngineKind::PartitionedFft { partition_frames } => partition_frames,
    }
}

fn estimate_single_engine_memory_bytes(taps: usize, engine: EngineKind) -> usize {
    match engine {
        EngineKind::DirectSimd => taps * size_of::<f64>(),
        EngineKind::PartitionedFft { partition_frames } => {
            let fft_len = (partition_frames + taps - 1).next_power_of_two();
            2 * (fft_len * size_of::<f64>() + (fft_len / 2 + 1) * size_of::<Complex64>())
        }
    }
}

struct PolyphaseResampler {
    filter_type: FilterType,
    source_rate: u32,
    target_rate: u32,
    ratio: f64,
    half_width: usize,
    phase_count: usize,
    coefficients: Vec<f64>,
    buffer_l: Vec<f64>,
    buffer_r: Vec<f64>,
    current_time: f64,
}

impl PolyphaseResampler {
    fn new(filter_type: FilterType, source_rate: u32, target_rate: u32) -> Self {
        let ratio = source_rate as f64 / target_rate as f64;
        let phase_count = POLYPHASE_PHASES;
        let half_width =
            Self::fractional_half_width(filter_type, source_rate, target_rate, phase_count);
        let mut resampler = Self {
            filter_type,
            source_rate,
            target_rate,
            ratio,
            half_width,
            phase_count,
            coefficients: Vec::new(),
            buffer_l: vec![0.0; half_width],
            buffer_r: vec![0.0; half_width],
            current_time: half_width as f64,
        };
        resampler.precompute_coefficients();
        resampler
    }

    fn base_half_width(filter_type: FilterType) -> usize {
        match filter_type {
            FilterType::SincExtreme32k
            | FilterType::LinearPhase128k
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
            | FilterType::SmoothPhase128k => 512,
        }
    }

    fn fractional_half_width(
        filter_type: FilterType,
        source_rate: u32,
        target_rate: u32,
        phase_count: usize,
    ) -> usize {
        let base = Self::base_half_width(filter_type);
        if target_rate == 0 || target_rate >= source_rate {
            return base;
        }

        let downsample_scale = source_rate.div_ceil(target_rate) as usize;
        let desired = base.saturating_mul(downsample_scale.max(1));
        desired.min(max_polyphase_half_width(phase_count)).max(1)
    }

    fn estimated_memory_bytes(&self) -> usize {
        self.coefficients.len() * size_of::<f64>()
            + (self.buffer_l.capacity() + self.buffer_r.capacity()) * size_of::<f64>()
    }

    fn precompute_coefficients(&mut self) {
        // When downsampling, the anti-alias cutoff must sit below the TARGET
        // Nyquist, not the source's. The kernel is built on the source-rate
        // grid, so scale the normalized cutoff by target/source (e.g. 192k ->
        // 176.4k with a 0.48 cutoff becomes 0.441, keeping 84.7-92.2 kHz
        // source content from folding back below 88.2 kHz).
        let cutoff = if self.target_rate < self.source_rate {
            self.filter_type.cutoff() * self.target_rate as f64 / self.source_rate as f64
        } else {
            self.filter_type.cutoff()
        };

        self.coefficients = build_polyphase_coefficient_table(
            self.half_width,
            self.phase_count,
            self.filter_type.beta(),
            cutoff,
        );
    }

    fn input(&mut self, samples_l: &[f64], samples_r: &[f64]) {
        self.buffer_l.extend_from_slice(samples_l);
        self.buffer_r.extend_from_slice(samples_r);
    }

    fn process(&mut self, output: &mut Vec<f64>) -> usize {
        let input_len = self.buffer_l.len();
        if input_len <= 2 * self.half_width {
            return 0;
        }

        let mut frames_written = 0;
        let estimated_frames = (((input_len - self.half_width) as f64 - self.current_time)
            / self.ratio)
            .max(0.0) as usize
            + 1;
        output.reserve(estimated_frames * 2);

        let num_taps = 2 * self.half_width + 1;
        let phase_scale = self.phase_count as f64;

        while (self.current_time as usize) + self.half_width < input_len {
            let idx = self.current_time as usize;
            if idx < self.half_width {
                break;
            }
            let tau = self.current_time - idx as f64;
            let phase_pos = tau * phase_scale;
            let phase_idx = phase_pos.floor() as usize;
            let phase_frac = phase_pos - phase_idx as f64;

            if phase_idx >= self.phase_count {
                break;
            }
            if idx + self.half_width >= input_len {
                break;
            }

            let start_offset = idx - self.half_width;
            let src_l = &self.buffer_l[start_offset..start_offset + num_taps];
            let src_r = &self.buffer_r[start_offset..start_offset + num_taps];
            let coeff_offset0 = phase_idx * num_taps;
            let coeff_offset1 = (phase_idx + 1) * num_taps;
            let coeffs0 = &self.coefficients[coeff_offset0..coeff_offset0 + num_taps];
            let coeffs1 = &self.coefficients[coeff_offset1..coeff_offset1 + num_taps];

            let mut acc_l = 0.0f64;
            let mut acc_r = 0.0f64;
            for tap in 0..num_taps {
                let c = coeffs0[tap] + (coeffs1[tap] - coeffs0[tap]) * phase_frac;
                acc_l += src_l[tap] * c;
                acc_r += src_r[tap] * c;
            }

            output.push(acc_l);
            output.push(acc_r);
            frames_written += 1;
            self.current_time += self.ratio;
        }

        let consumed_idx = self.current_time as usize;
        if consumed_idx > self.half_width {
            let keep_from = consumed_idx - self.half_width;
            self.buffer_l.copy_within(keep_from.., 0);
            self.buffer_l.truncate(self.buffer_l.len() - keep_from);
            self.buffer_r.copy_within(keep_from.., 0);
            self.buffer_r.truncate(self.buffer_r.len() - keep_from);
            self.current_time -= keep_from as f64;
        }

        frames_written
    }

    fn reset(&mut self) {
        self.buffer_l.clear();
        self.buffer_r.clear();
        self.buffer_l.resize(self.half_width, 0.0);
        self.buffer_r.resize(self.half_width, 0.0);
        self.current_time = self.half_width as f64;
    }

    fn flush_lookahead_source_frames(&self) -> usize {
        self.half_width
            .saturating_add(ceil_mul_div_usize(
                self.source_rate as usize,
                1,
                self.target_rate as usize,
            ))
            .saturating_add(2)
    }
}

struct RationalPolyphaseResampler {
    filter_type: FilterType,
    source_rate: u32,
    target_rate: u32,
    step_num: usize,
    phase_den: usize,
    half_width: usize,
    coefficients: Vec<f64>,
    buffer_l: Vec<f64>,
    buffer_r: Vec<f64>,
    current_time_num: usize,
}

impl RationalPolyphaseResampler {
    fn new(
        filter_type: FilterType,
        source_rate: u32,
        target_rate: u32,
        step_num: usize,
        phase_den: usize,
    ) -> Self {
        let half_width = PolyphaseResampler::fractional_half_width(
            filter_type,
            source_rate,
            target_rate,
            phase_den,
        );
        let mut resampler = Self {
            filter_type,
            source_rate,
            target_rate,
            step_num,
            phase_den,
            half_width,
            coefficients: Vec::new(),
            buffer_l: vec![0.0; half_width],
            buffer_r: vec![0.0; half_width],
            current_time_num: half_width * phase_den,
        };
        resampler.precompute_coefficients();
        resampler
    }

    fn estimated_memory_bytes(&self) -> usize {
        self.coefficients.len() * size_of::<f64>()
            + (self.buffer_l.capacity() + self.buffer_r.capacity()) * size_of::<f64>()
    }

    fn precompute_coefficients(&mut self) {
        let cutoff = if self.target_rate < self.source_rate {
            self.filter_type.cutoff() * self.target_rate as f64 / self.source_rate as f64
        } else {
            self.filter_type.cutoff()
        };
        self.coefficients = build_exact_polyphase_coefficient_table_for_filter(
            self.filter_type,
            self.half_width,
            self.phase_den,
            cutoff,
        );
    }

    fn input(&mut self, samples_l: &[f64], samples_r: &[f64]) {
        self.buffer_l.extend_from_slice(samples_l);
        self.buffer_r.extend_from_slice(samples_r);
    }

    fn process(&mut self, output: &mut Vec<f64>) -> usize {
        let input_len = self.buffer_l.len();
        if input_len <= 2 * self.half_width {
            return 0;
        }

        let mut frames_written = 0;
        let current_time = self.current_time_num as f64 / self.phase_den as f64;
        let ratio = self.step_num as f64 / self.phase_den as f64;
        let estimated_frames =
            (((input_len - self.half_width) as f64 - current_time) / ratio).max(0.0) as usize + 1;
        output.reserve(estimated_frames * 2);

        let num_taps = 2 * self.half_width + 1;
        // EOF tail policy: emit only while the FIR window is backed by
        // caller-supplied samples. Explicit zero input flushes tails;
        // implicit flushing would change gapless carry behavior.
        while self.current_time_num / self.phase_den + self.half_width < input_len {
            let idx = self.current_time_num / self.phase_den;
            if idx < self.half_width {
                break;
            }
            if idx + self.half_width >= input_len {
                break;
            }

            let phase = self.current_time_num % self.phase_den;
            let start_offset = idx - self.half_width;
            let src_l = &self.buffer_l[start_offset..start_offset + num_taps];
            let src_r = &self.buffer_r[start_offset..start_offset + num_taps];
            let coeff_offset = phase * num_taps;
            let coeffs = &self.coefficients[coeff_offset..coeff_offset + num_taps];

            let mut acc_l = 0.0f64;
            let mut acc_r = 0.0f64;
            for tap in 0..num_taps {
                let c = coeffs[tap];
                acc_l += src_l[tap] * c;
                acc_r += src_r[tap] * c;
            }

            output.push(acc_l);
            output.push(acc_r);
            frames_written += 1;
            self.current_time_num += self.step_num;
        }

        let consumed_idx = self.current_time_num / self.phase_den;
        if consumed_idx > self.half_width {
            let keep_from = consumed_idx - self.half_width;
            self.buffer_l.copy_within(keep_from.., 0);
            self.buffer_l.truncate(self.buffer_l.len() - keep_from);
            self.buffer_r.copy_within(keep_from.., 0);
            self.buffer_r.truncate(self.buffer_r.len() - keep_from);
            self.current_time_num -= keep_from * self.phase_den;
        }

        frames_written
    }

    fn reset(&mut self) {
        self.buffer_l.clear();
        self.buffer_r.clear();
        self.buffer_l.resize(self.half_width, 0.0);
        self.buffer_r.resize(self.half_width, 0.0);
        self.current_time_num = self.half_width * self.phase_den;
    }

    fn flush_lookahead_source_frames(&self) -> usize {
        self.half_width
            .saturating_add(self.step_num.div_ceil(self.phase_den).max(1))
            .saturating_add(2)
    }
}

fn build_character_polyphase_pair(
    half_width: usize,
    beta: f64,
    cutoff: f64,
    phase_mode: PhaseMode,
) -> (Vec<f64>, Vec<f64>, Option<usize>, Option<usize>) {
    match phase_mode {
        PhaseMode::Linear => (
            build_phase_coefficients(half_width, 0.0, beta, cutoff),
            build_phase_coefficients(half_width, 0.5, beta, cutoff),
            None,
            None,
        ),
        PhaseMode::Minimum => {
            let (phase0, phase1) = build_minimum_phase_polyphase_pair(half_width, beta, cutoff);
            let prepad0 = Some(phase0.len().saturating_sub(1));
            let prepad1 = Some(phase1.len().saturating_sub(1));
            (phase0, phase1, prepad0, prepad1)
        }
        PhaseMode::MinimumPhase128k(_) => {
            let proto = build_full_rate_2x_prototype(half_width, beta, cutoff);
            let minimum = minimum_phase_impulse_with_params(&proto, minimum128k_phase_params());
            let (phase0, phase1) = split_full_rate_impulse_into_reversed_branches(&minimum);
            // Match the established Minimum16k alignment contract: retain the
            // intrinsic causal delay instead of peak-aligning a pure minimum-
            // phase kernel as the shifted hybrid families require.
            let prepad0 = Some(phase0.len().saturating_sub(1));
            let prepad1 = Some(phase1.len().saturating_sub(1));
            (phase0, phase1, prepad0, prepad1)
        }
        PhaseMode::MinimumPhaseCompact128k(profile) => {
            let minimum = minimum_phase_compact_impulse(profile);
            debug_assert_eq!(minimum.len(), 4 * half_width + 1);
            let (phase0, phase1) = split_full_rate_impulse_into_reversed_branches(&minimum);
            let prepad0 = Some(phase0.len().saturating_sub(1));
            let prepad1 = Some(phase1.len().saturating_sub(1));
            (phase0, phase1, prepad0, prepad1)
        }
        PhaseMode::SplitPhase128k => {
            let proto = build_full_rate_2x_prototype(half_width, beta, cutoff);
            let split = split_phase_impulse_with_params(&proto, split128k_phase_params());
            let origin = dominant_impulse_index(&split);
            let (phase0, phase1) = split_full_rate_impulse_into_reversed_branches(&split);
            let prepad0 = Some(prepad_for_global_origin(phase0.len(), origin, 0));
            let prepad1 = Some(prepad_for_global_origin(phase1.len(), origin, 1));
            (phase0, phase1, prepad0, prepad1)
        }
        PhaseMode::IntegratedPhase128k(profile) => {
            let proto = build_full_rate_2x_prototype(half_width, beta, cutoff);
            let integrated =
                integrated_phase_impulse_with_params(&proto, integrated128k_phase_params(profile));
            let origin = dominant_impulse_index(&integrated);
            let (phase0, phase1) = split_full_rate_impulse_into_reversed_branches(&integrated);
            let prepad0 = Some(prepad_for_global_origin(phase0.len(), origin, 0));
            let prepad1 = Some(prepad_for_global_origin(phase1.len(), origin, 1));
            (phase0, phase1, prepad0, prepad1)
        }
    }
}

/// Build a linear-phase 2x-rate prototype (Kaiser-windowed sinc) covering the
/// same support as the polyphase pair `build_phase_coefficients(half_width, 0.0|0.5, ...)`.
fn build_full_rate_2x_prototype(half_width: usize, beta: f64, cutoff: f64) -> Vec<f64> {
    // Prototype lives at the 2x output rate. Positions step by 0.5 from
    // -half_width to +half_width inclusive => 4*half_width + 1 samples.
    let proto_len = 4 * half_width + 1;
    let n_max = half_width as f64;
    let mut proto = vec![0.0; proto_len];
    for (idx, slot) in proto.iter_mut().enumerate() {
        let pos = (idx as f64) * 0.5 - n_max;
        *slot = 2.0 * cutoff * sinc(2.0 * cutoff * pos) * kaiser_window(pos, n_max, beta);
    }
    normalize_coefficients(&mut proto);
    proto
}

/// Convert the whole 2x prototype to minimum phase, then deinterleave into
/// even-index (phase 0) and odd-index (phase 1) branches.
///
/// The min-phase transform runs on the WHOLE prototype rather than per polyphase
/// branch — see RESAMPLER_ROADMAP.md step 5 for why per-branch conversion
/// produces audible artifacts.
fn build_minimum_phase_polyphase_pair(
    half_width: usize,
    beta: f64,
    cutoff: f64,
) -> (Vec<f64>, Vec<f64>) {
    let proto = build_full_rate_2x_prototype(half_width, beta, cutoff);
    // Magnitude doubles at the output rate because we collapsed a 2x interpolation
    // kernel; the per-branch normalize_coefficients at the end restores per-phase
    // unit DC gain.

    let min_phase = minimum16k_phase_impulse(&proto);
    split_full_rate_impulse_into_reversed_branches(&min_phase)
}

fn split_full_rate_impulse_into_reversed_branches(impulse: &[f64]) -> (Vec<f64>, Vec<f64>) {
    // Deinterleave: phase 0 = even-index samples (2*half_width + 1 of them),
    // phase 1 = odd-index samples (2*half_width of them, padded to match).
    let per_phase_len = impulse.len() / 2 + 1;
    let mut phase0 = vec![0.0; per_phase_len];
    let mut phase1 = vec![0.0; per_phase_len];
    for (i, &sample) in impulse.iter().enumerate() {
        if i % 2 == 0 {
            let dst = i / 2;
            if dst < per_phase_len {
                phase0[dst] = sample;
            }
        } else {
            let dst = i / 2;
            if dst < per_phase_len {
                phase1[dst] = sample;
            }
        }
    }

    // The FIR engines read coeffs[0] against the OLDEST buffered sample and
    // coeffs[last] against the NEWEST. Convolution y[n] = sum h[k]·x[n-k] needs
    // h[0] (the dominant min-phase peak) to multiply the newest sample, so the
    // impulse must be stored REVERSED. Linear-phase Kaiser is symmetric and
    // doesn't notice; a causal min-phase impulse stored forward becomes a
    // max-phase filter (pre-ringing piled before every transient, audibly
    // smooth-but-lifeless treble).
    phase0.reverse();
    phase1.reverse();

    normalize_coefficients(&mut phase0);
    normalize_coefficients(&mut phase1);
    (phase0, phase1)
}

fn dominant_impulse_index(impulse: &[f64]) -> usize {
    impulse
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .map(|(idx, _)| idx)
        .unwrap_or(0)
}

fn prepad_for_global_origin(branch_len: usize, global_origin: usize, phase_index: usize) -> usize {
    let branch_origin = global_origin.saturating_sub(phase_index).div_ceil(2);
    let branch_origin = branch_origin.min(branch_len.saturating_sub(1));
    branch_len.saturating_sub(1).saturating_sub(branch_origin)
}

/// Minimum-phase weight for the split-phase filter at a normalized 2x-prototype
/// frequency (0..0.5): 0.0 (pure linear phase) up to F_LO, 1.0 (pure minimum
/// phase) from F_HI, smootherstep in log-frequency between.
#[cfg(test)]
fn split_phase_blend_weight(freq_norm_2x: f64) -> f64 {
    split_phase_blend_weight_with_params(freq_norm_2x, SplitPhaseParams::default())
}

fn split_phase_blend_weight_with_params(freq_norm_2x: f64, params: SplitPhaseParams) -> f64 {
    if freq_norm_2x <= params.split_f_lo {
        params.low_blend_floor
    } else if freq_norm_2x >= params.split_f_hi {
        1.0
    } else {
        let t = (freq_norm_2x.ln() - params.split_f_lo.ln())
            / (params.split_f_hi.ln() - params.split_f_lo.ln());
        let smooth = t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
        params.low_blend_floor + (1.0 - params.low_blend_floor) * smooth
    }
}

/// Frequency-split phase reconstruction for the split-phase filter. The magnitude
/// is the linear prototype's at every bin; only the phase is synthesized:
/// pure linear phase (exactly constant group delay) below F_LO, pure minimum
/// phase above F_HI, smootherstep blend in log-frequency between.
///
/// The linear-phase reference is NOT the symmetric prototype's phase. The
/// prototype's group delay is half the kernel length, and blending that in
/// below F_LO only — while the treble goes minimum phase — would lag the low
/// band behind the treble by that half-length. A constant phase blend
/// would turn the symmetric phase into
/// benign uniform latency, but a frequency-DEPENDENT blend turns it into
/// inter-band dispersion. Instead the reference slope is matched to the
/// minimum-phase filter's own mean group delay across [0, F_LO], so the low
/// band keeps an exactly-constant group delay, stays time-aligned with the
/// minimum-phase treble, and the whole kernel remains front-loaded.
fn split_phase_impulse_with_params(linear_phase: &[f64], params: SplitPhaseParams) -> Vec<f64> {
    let n_lp = linear_phase.len();
    let fft_len = (n_lp * SPLIT_PHASE_FFT_MULTIPLIER)
        .next_power_of_two()
        .max(8);
    let minimum = minimum_phase_impulse_with_fft_len(linear_phase, fft_len);
    let linear_spectrum = real_spectrum(linear_phase, fft_len);
    let minimum_spectrum = real_spectrum(&minimum, fft_len);
    let peak_magnitude = linear_spectrum
        .iter()
        .fold(0.0_f64, |peak, bin| peak.max(bin.norm()));
    let phase_floor = peak_magnitude * MIXED_PHASE_UNWRAP_MAG_FLOOR_REL;
    let minimum_phase_unwrapped = unwrap_spectrum_phase_with_floor(&minimum_spectrum, phase_floor);

    // Mean minimum-phase group delay (in 2x-prototype samples) over the pure
    // linear-phase region: with phase(0) = 0, the chord slope to the F_LO bin
    // is the exact band average. Matching the reference to it makes the
    // blended phase continuous (zero phase gap) entering the ramp.
    let lo_bin = ((params.split_f_lo * fft_len as f64).round() as usize)
        .clamp(1, minimum_phase_unwrapped.len() - 1);
    let reference_delay =
        -minimum_phase_unwrapped[lo_bin] * fft_len as f64 / (2.0 * PI * lo_bin as f64);

    // Bulk causality shift. The exactly-linear-phase low band needs
    // symmetric time support around its group delay, and the blend ramp's
    // log-frequency smoothness gives that lobe a slowly decaying left tail.
    // Centered at the minimum-phase low-band delay (~10 samples) the tail
    // wraps to negative time and is amputated by the truncation below, which
    // ripples the passband (±0.02 dB) and sets a poor broadband leakage
    // floor (~-65 dB). A constant delay on the WHOLE spectrum is
    // dispersion-free and the dominant-tap prepad alignment cancels it
    // downstream, so it costs no pipeline latency. The n_lp/64 shift buys a
    // deep leakage floor while the impulse pre-peak window stays under 2 ms.
    // Kept even so the Nyquist bin stays real.
    let causality_shift = (n_lp / 64) as f64 * params.causality_shift_scale;

    let mut split_spectrum = linear_spectrum.clone();
    for (idx, bin) in split_spectrum.iter_mut().enumerate() {
        let magnitude = linear_spectrum[idx].norm();
        let freq_norm_2x = idx as f64 / fft_len as f64;
        // Below the magnitude floor the band is stopband: keep causal
        // (minimum-phase) energy rather than symmetric.
        let blend = if magnitude > phase_floor {
            split_phase_blend_weight_with_params(freq_norm_2x, params)
        } else {
            1.0
        };
        let linear_reference_phase = -2.0 * PI * freq_norm_2x * reference_delay;
        let phase = (1.0 - blend) * linear_reference_phase + blend * minimum_phase_unwrapped[idx]
            - 2.0 * PI * freq_norm_2x * causality_shift;
        *bin = Complex64::from_polar(magnitude, phase);
    }
    if let Some(dc) = split_spectrum.first_mut() {
        dc.im = 0.0;
    }
    if let Some(nyquist) = split_spectrum.last_mut() {
        nyquist.im = 0.0;
    }

    let mut split = inverse_real_spectrum(&mut split_spectrum, fft_len, n_lp);
    apply_raised_cosine_tail_fade_with_fraction(&mut split, params.tail_fade_fraction);
    normalize_coefficients(&mut split);
    split
}

fn split128k_phase_params() -> SplitPhaseParams {
    static PARAMS: OnceLock<SplitPhaseParams> = OnceLock::new();
    *PARAMS.get_or_init(|| {
        let mut params = SplitPhaseParams {
            low_blend_floor: SPLIT128K_PRODUCTION_BLEND_FLOOR,
            causality_shift_scale: SPLIT128K_PRODUCTION_CAUSALITY_SHIFT_SCALE,
            tail_fade_fraction: SPLIT128K_PRODUCTION_TAIL_FADE,
            ..SplitPhaseParams::default()
        };
        if let Some(value) = env_f64("FOZMO_SPLIT128K_F_LO_HZ") {
            params.split_f_lo = (value / 88_200.0).clamp(1.0 / 88_200.0, params.split_f_hi * 0.95);
        }
        if let Some(value) = env_f64("FOZMO_SPLIT128K_F_HI_HZ") {
            params.split_f_hi = (value / 88_200.0).clamp(params.split_f_lo * 1.05, 0.49);
        }
        if let Some(value) = env_f64("FOZMO_SPLIT128K_BLEND_FLOOR") {
            params.low_blend_floor = value.clamp(0.0, 0.25);
        }
        if let Some(value) = env_f64("FOZMO_SPLIT128K_CAUSALITY_SHIFT_SCALE") {
            params.causality_shift_scale = value.clamp(0.25, 2.0);
        }
        if let Some(value) = env_f64("FOZMO_SPLIT128K_TAIL_FADE") {
            params.tail_fade_fraction = value.clamp(0.001, 0.10);
        }
        params
    })
}

fn integrated_phase_weight(freq: f64, params: IntegratedPhaseParams) -> f64 {
    if freq <= params.transition_f_lo {
        return 0.0;
    }
    if freq >= params.transition_f_hi {
        return 1.0;
    }
    let t = (freq.ln() - params.transition_f_lo.ln())
        / (params.transition_f_hi.ln() - params.transition_f_lo.ln());
    let t2 = t * t;
    let t4 = t2 * t2;
    t4 * (35.0 + t * (-84.0 + t * (70.0 - 20.0 * t)))
}

fn kahan_add(sum: &mut f64, compensation: &mut f64, value: f64) {
    let adjusted = value - *compensation;
    let next = *sum + adjusted;
    *compensation = (next - *sum) - adjusted;
    *sum = next;
}

fn integrated_phase_from_unwrapped_minimum(
    minimum_phase: &[f64],
    fft_len: usize,
    params: IntegratedPhaseParams,
) -> Vec<f64> {
    if minimum_phase.len() < 2 {
        return minimum_phase.to_vec();
    }

    let mut numerator = 0.0;
    let mut numerator_compensation = 0.0;
    let mut denominator = 0.0;
    let mut denominator_compensation = 0.0;
    for k in 1..minimum_phase.len() {
        let freq_mid = (k as f64 - 0.5) / fft_len as f64;
        let a = 1.0 - integrated_phase_weight(freq_mid, params);
        let increment = minimum_phase[k] - minimum_phase[k - 1];
        kahan_add(&mut numerator, &mut numerator_compensation, a * increment);
        kahan_add(&mut denominator, &mut denominator_compensation, a);
    }

    if !denominator.is_finite() || denominator <= f64::EPSILON || !numerator.is_finite() {
        return minimum_phase.to_vec();
    }
    let reference_increment = numerator / denominator;
    if !reference_increment.is_finite() {
        return minimum_phase.to_vec();
    }

    let mut target = Vec::with_capacity(minimum_phase.len());
    target.push(minimum_phase[0]);
    for k in 1..minimum_phase.len() {
        let freq_mid = (k as f64 - 0.5) / fft_len as f64;
        let weight = integrated_phase_weight(freq_mid, params);
        let minimum_increment = minimum_phase[k] - minimum_phase[k - 1];
        let target_increment = (1.0 - weight) * reference_increment + weight * minimum_increment;
        target.push(target[k - 1] + target_increment);
    }

    let join_bin = ((params.transition_f_hi * fft_len as f64 + 0.5).ceil() as usize)
        .min(minimum_phase.len() - 1);
    let join_error_rad = target[join_bin] - minimum_phase[join_bin];
    debug_assert!(
        join_error_rad.abs() < 1.0e-8,
        "Integrated Phase failed to rejoin minimum phase: {join_error_rad} rad"
    );
    target
}

fn nearest_even_delay_samples(value: f64) -> usize {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    ((value / 2.0).round() as usize).saturating_mul(2)
}

fn integrated_phase_impulse_with_params(
    linear_phase: &[f64],
    params: IntegratedPhaseParams,
) -> Vec<f64> {
    let n_lp = linear_phase.len();
    let padded = n_lp
        .checked_mul(INTEGRATED_PHASE_FFT_MULTIPLIER)
        .expect("Integrated Phase FFT length overflow");
    let fft_len = padded.next_power_of_two().max(8);
    let minimum =
        minimum_phase_impulse_with_fft_len_and_params(linear_phase, fft_len, params.minimum_phase);
    let linear_spectrum = real_spectrum(linear_phase, fft_len);
    let minimum_spectrum = real_spectrum(&minimum, fft_len);
    let peak = linear_spectrum
        .iter()
        .fold(0.0_f64, |acc, bin| acc.max(bin.norm()));
    let minimum_phase =
        unwrap_spectrum_phase_with_floor(&minimum_spectrum, peak * params.phase_floor_rel);
    let target_phase = integrated_phase_from_unwrapped_minimum(&minimum_phase, fft_len, params);
    let shift =
        nearest_even_delay_samples(n_lp as f64 / 64.0 * params.causality_shift_scale) as f64;

    let mut spectrum = linear_spectrum.clone();
    for (k, bin) in spectrum.iter_mut().enumerate() {
        let freq = k as f64 / fft_len as f64;
        let phase = target_phase[k] - 2.0 * PI * freq * shift;
        *bin = Complex64::from_polar(linear_spectrum[k].norm(), phase);
    }
    spectrum[0].im = 0.0;
    if let Some(nyquist) = spectrum.last_mut() {
        nyquist.im = 0.0;
    }

    let mut impulse = inverse_real_spectrum(&mut spectrum, fft_len, n_lp);
    apply_raised_cosine_tail_fade_with_fraction(&mut impulse, params.tail_fade_fraction);
    normalize_coefficients(&mut impulse);
    debug_assert!(impulse.iter().all(|sample| sample.is_finite()));
    impulse
}

fn integrated128k_phase_params(profile: IntegratedPhaseProfile) -> IntegratedPhaseParams {
    static PROFILE_1: OnceLock<IntegratedPhaseParams> = OnceLock::new();
    static PROFILE_2: OnceLock<IntegratedPhaseParams> = OnceLock::new();
    static PROFILE_3: OnceLock<IntegratedPhaseParams> = OnceLock::new();
    static PROFILE_4: OnceLock<IntegratedPhaseParams> = OnceLock::new();
    let cell = match profile {
        IntegratedPhaseProfile::One => &PROFILE_1,
        IntegratedPhaseProfile::Two => &PROFILE_2,
        IntegratedPhaseProfile::Three => &PROFILE_3,
        IntegratedPhaseProfile::Four => &PROFILE_4,
    };
    *cell.get_or_init(|| {
        let mut params = IntegratedPhaseParams::default();
        (params.transition_f_lo, params.transition_f_hi) = profile.transition();
        // Preserve the original tuning hooks for profile 1 only. Applying
        // them globally would collapse the numbered listening candidates.
        if profile == IntegratedPhaseProfile::One {
            if let Some(value) = env_f64("FOZMO_INTEGRATED128K_F_LO_HZ") {
                params.transition_f_lo =
                    (value / 88_200.0).clamp(1.0 / 88_200.0, params.transition_f_hi * 0.95);
            }
            if let Some(value) = env_f64("FOZMO_INTEGRATED128K_F_HI_HZ") {
                params.transition_f_hi =
                    (value / 88_200.0).clamp(params.transition_f_lo * 1.05, 0.49);
            }
        }
        if let Some(value) = env_f64("FOZMO_INTEGRATED128K_CAUSALITY_SHIFT_SCALE") {
            params.causality_shift_scale = value.clamp(0.25, 2.0);
        }
        if let Some(value) = env_f64("FOZMO_INTEGRATED128K_TAIL_FADE") {
            params.tail_fade_fraction = value.clamp(0.001, 0.10);
        }
        if let Some(value) = env_f64("FOZMO_INTEGRATED128K_PHASE_FLOOR_REL") {
            params.phase_floor_rel = value.clamp(1.0e-12, 1.0e-3);
        }
        if let Some(value) = env_f64("FOZMO_INTEGRATED128K_MIN_TAIL_FADE") {
            params.minimum_phase.tail_fade_fraction = value.clamp(0.001, 0.10);
        }
        if let Some(value) = env_f64("FOZMO_INTEGRATED128K_MIN_MAG_FLOOR_REL") {
            params.minimum_phase.mag_floor_rel = value.clamp(1.0e-16, 1.0e-6);
        }
        params
    })
}

fn minimum16k_phase_params() -> MinimumPhaseParams {
    static PARAMS: OnceLock<MinimumPhaseParams> = OnceLock::new();
    *PARAMS.get_or_init(|| {
        let mut params = MinimumPhaseParams {
            tail_fade_fraction: MINIMUM16K_PRODUCTION_TAIL_FADE,
            mag_floor_rel: MINIMUM16K_PRODUCTION_MAG_FLOOR_REL,
        };
        if let Some(value) = env_f64("FOZMO_MINIMUM16K_TAIL_FADE") {
            params.tail_fade_fraction = value.clamp(0.001, 0.10);
        }
        if let Some(value) = env_f64("FOZMO_MINIMUM16K_MAG_FLOOR_REL") {
            params.mag_floor_rel = value.clamp(1.0e-16, 1.0e-6);
        }
        params
    })
}

fn minimum128k_phase_params() -> MinimumPhaseParams {
    MinimumPhaseParams {
        tail_fade_fraction: MINIMUM16K_PRODUCTION_TAIL_FADE,
        mag_floor_rel: MINIMUM16K_PRODUCTION_MAG_FLOOR_REL,
    }
}

fn env_f64(name: &str) -> Option<f64> {
    std::env::var(name).ok()?.parse::<f64>().ok()
}

fn real_spectrum(samples: &[f64], fft_len: usize) -> Vec<Complex64> {
    let mut planner = RealFftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(fft_len);
    let mut time_buf = vec![0.0_f64; fft_len];
    time_buf[..samples.len()].copy_from_slice(samples);
    let mut spectrum = forward.make_output_vec();
    forward
        .process(&mut time_buf, &mut spectrum)
        .expect("forward FFT plan should match the allocated buffers");
    spectrum
}

fn inverse_real_spectrum(
    spectrum: &mut [Complex64],
    fft_len: usize,
    output_len: usize,
) -> Vec<f64> {
    let mut planner = RealFftPlanner::<f64>::new();
    let inverse = planner.plan_fft_inverse(fft_len);
    let mut impulse = inverse.make_output_vec();
    inverse
        .process(spectrum, &mut impulse)
        .expect("inverse FFT plan should match the allocated buffers");
    let scale = 1.0 / fft_len as f64;
    for sample in impulse.iter_mut() {
        *sample *= scale;
    }
    impulse.truncate(output_len);
    impulse
}

fn unwrap_spectrum_phase_with_floor(spectrum: &[Complex64], magnitude_floor: f64) -> Vec<f64> {
    let mut unwrapped = Vec::with_capacity(spectrum.len());
    let mut offset = 0.0_f64;
    let mut previous = 0.0_f64;
    let mut have_previous = false;
    for bin in spectrum {
        if bin.norm() <= magnitude_floor {
            unwrapped.push(previous);
            continue;
        }

        let phase = bin.arg();
        if !have_previous {
            previous = phase;
            have_previous = true;
            unwrapped.push(phase);
            continue;
        }

        let mut candidate = phase + offset;
        let mut delta = candidate - previous;
        while delta > PI {
            offset -= 2.0 * PI;
            candidate = phase + offset;
            delta = candidate - previous;
        }
        while delta <= -PI {
            offset += 2.0 * PI;
            candidate = phase + offset;
            delta = candidate - previous;
        }
        unwrapped.push(candidate);
        previous = candidate;
    }
    unwrapped
}

/// Cepstral minimum-phase reconstruction:
///   FFT -> log|H| -> IFFT -> fold causal cepstrum -> FFT -> exp -> IFFT.
/// The output has the same magnitude response as `linear_phase` but a causal,
/// front-loaded impulse with no symmetric pre-ringing tail.
fn minimum_phase_impulse(linear_phase: &[f64]) -> Vec<f64> {
    minimum_phase_impulse_with_params(linear_phase, MinimumPhaseParams::default())
}

fn minimum16k_phase_impulse(linear_phase: &[f64]) -> Vec<f64> {
    minimum_phase_impulse_with_params(linear_phase, minimum16k_phase_params())
}

fn minimum_phase_impulse_with_params(linear_phase: &[f64], params: MinimumPhaseParams) -> Vec<f64> {
    let n_lp = linear_phase.len();
    // Heavy zero-padding minimises time-aliasing in the cepstrum. 8x is
    // standard practice for audio-grade min-phase reconstruction.
    let fft_len = (n_lp * 8).next_power_of_two().max(8);
    minimum_phase_impulse_with_fft_len_and_params(linear_phase, fft_len, params)
}

fn minimum_phase_impulse_with_fft_len(linear_phase: &[f64], fft_len: usize) -> Vec<f64> {
    minimum_phase_impulse_with_fft_len_and_params(
        linear_phase,
        fft_len,
        MinimumPhaseParams::default(),
    )
}

fn minimum_phase_impulse_with_fft_len_and_params(
    linear_phase: &[f64],
    fft_len: usize,
    params: MinimumPhaseParams,
) -> Vec<f64> {
    let n_lp = linear_phase.len();
    let mut planner = RealFftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(fft_len);
    let mut time_buf = vec![0.0_f64; fft_len];
    time_buf[..n_lp].copy_from_slice(linear_phase);
    let mut spectrum = fwd.make_output_vec();
    fwd.process(&mut time_buf, &mut spectrum)
        .expect("forward FFT plan should match the allocated buffers");

    // Step: log magnitude (phase is discarded — cepstrum is real). The floor
    // is relative to the spectrum peak (like the mixed-phase unwrap floor) so
    // deep stopbands aren't silently clipped when the prototype's overall
    // scale changes; the absolute fallback only guards the all-zero input.
    let peak_magnitude = spectrum
        .iter()
        .fold(0.0_f64, |peak, bin| peak.max(bin.norm()));
    let magnitude_floor = (peak_magnitude * params.mag_floor_rel).max(f64::MIN_POSITIVE);
    let magnitude = spectrum
        .iter()
        .map(|bin| bin.norm().max(magnitude_floor))
        .collect::<Vec<_>>();
    let fade_samples = ((n_lp as f64) * params.tail_fade_fraction)
        .round()
        .clamp(8.0, 2048.0) as usize;
    minimum_phase_from_magnitude(&magnitude, fft_len, n_lp, fade_samples)
}

#[allow(clippy::needless_range_loop)]
fn minimum_phase_from_magnitude(
    magnitude: &[f64],
    fft_len: usize,
    output_len: usize,
    tail_fade_samples: usize,
) -> Vec<f64> {
    assert_eq!(magnitude.len(), fft_len / 2 + 1);
    assert!(output_len <= fft_len);
    let scale = 1.0 / fft_len as f64;
    let mut planner = RealFftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(fft_len);
    let inv = planner.plan_fft_inverse(fft_len);
    let mut spectrum = magnitude
        .iter()
        .map(|mag| Complex64::new(mag.max(f64::MIN_POSITIVE).ln(), 0.0))
        .collect::<Vec<_>>();

    // IFFT to cepstrum (real-valued in time domain).
    let mut cepstrum = inv.make_output_vec();
    inv.process(&mut spectrum, &mut cepstrum)
        .expect("inverse FFT plan should match the allocated buffers");
    for x in cepstrum.iter_mut() {
        *x *= scale;
    }

    // Fold: keep DC and Nyquist as-is, double the strictly-causal bins,
    // zero out the anti-causal half. This is the discrete-time Hilbert
    // transform of the cepstrum and converts it to a min-phase cepstrum.
    let half = fft_len / 2;
    for i in 1..half {
        cepstrum[i] *= 2.0;
    }
    for x in cepstrum.iter_mut().skip(half + 1) {
        *x = 0.0;
    }

    // FFT back, then exponentiate per bin.
    let mut folded_spec = fwd.make_output_vec();
    fwd.process(&mut cepstrum, &mut folded_spec)
        .expect("forward FFT plan should match the allocated buffers");
    for bin in folded_spec.iter_mut() {
        *bin = bin.exp();
    }

    let mut min_phase = inv.make_output_vec();
    inv.process(&mut folded_spec, &mut min_phase)
        .expect("inverse FFT plan should match the allocated buffers");
    for x in min_phase.iter_mut() {
        *x *= scale;
    }

    min_phase.truncate(output_len);
    apply_raised_cosine_tail_fade(&mut min_phase, tail_fade_samples);
    normalize_coefficients(&mut min_phase);
    min_phase
}

fn planck_step(x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let z = 1.0 / x - 1.0 / (1.0 - x);
    if z >= 0.0 {
        let e = (-z).exp();
        e / (1.0 + e)
    } else {
        1.0 / (1.0 + z.exp())
    }
}

fn db_to_gain(db: f64) -> f64 {
    10.0f64.powf(db / 20.0)
}

fn compact_treble_gain(frequency: f64, params: MinimumCompactParams) -> f64 {
    let Some(taper) = params.treble_taper else {
        return 1.0;
    };
    if frequency <= taper.start_2x {
        return 1.0;
    }
    if frequency >= taper.end_2x {
        return db_to_gain(-taper.attenuation_db);
    }
    let x = (frequency - taper.start_2x) / (taper.end_2x - taper.start_2x);
    db_to_gain(-taper.attenuation_db * planck_step(x))
}

fn compact_minimum_magnitude(frequency: f64, params: MinimumCompactParams) -> f64 {
    let pass_edge_gain = compact_treble_gain(params.pass_edge_2x, params);
    if frequency <= params.pass_edge_2x {
        return compact_treble_gain(frequency, params);
    }
    if frequency < params.stop_edge_2x {
        let x = (frequency - params.pass_edge_2x) / (params.stop_edge_2x - params.pass_edge_2x);
        return params.stop_gain + (pass_edge_gain - params.stop_gain) * (1.0 - planck_step(x));
    }
    let y = (frequency - params.stop_edge_2x) / (0.5 - params.stop_edge_2x);
    let blend = planck_step(y);
    ((1.0 - blend) * params.stop_gain.ln() + blend * params.nyquist_gain.ln()).exp()
}

fn minimum_phase_compact_impulse(profile: MinimumCompactProfile) -> Vec<f64> {
    static ORIGINAL: OnceLock<Vec<f64>> = OnceLock::new();
    static BALANCED: OnceLock<Vec<f64>> = OnceLock::new();
    static SMOOTH: OnceLock<Vec<f64>> = OnceLock::new();
    let (cache, params) = match profile {
        MinimumCompactProfile::Original => (&ORIGINAL, MINIMUM_COMPACT_ORIGINAL_PARAMS),
        MinimumCompactProfile::Balanced => (&BALANCED, MINIMUM_COMPACT_BALANCED_PARAMS),
        MinimumCompactProfile::Smooth => (&SMOOTH, SMOOTH_PHASE_PARAMS),
    };
    cache
        .get_or_init(|| {
            let fft_len = 524_288 * MINIMUM_COMPACT_FFT_MULTIPLIER;
            debug_assert_eq!(fft_len, 4_194_304);
            let magnitude = (0..=fft_len / 2)
                .map(|bin| compact_minimum_magnitude(bin as f64 / fft_len as f64, params))
                .collect::<Vec<_>>();
            minimum_phase_from_magnitude(
                &magnitude,
                fft_len,
                MINIMUM_COMPACT_IMPULSE_SAMPLES,
                params.tail_fade_samples,
            )
        })
        .clone()
}

fn apply_raised_cosine_tail_fade(samples: &mut [f64], fade_samples: usize) {
    let fade_len = fade_samples.min(samples.len() / 4);
    if fade_len < 2 {
        return;
    }
    let start = samples.len() - fade_len;
    let denom = (fade_len - 1) as f64;
    for (idx, sample) in samples[start..].iter_mut().enumerate() {
        let t = idx as f64 / denom;
        *sample *= 0.5 * (1.0 + (PI * t).cos());
    }
}

fn apply_raised_cosine_tail_fade_with_fraction(samples: &mut [f64], fraction: f64) {
    let max_fade_len = samples.len() / 4;
    let fade_len = ((samples.len() as f64) * fraction).round() as usize;
    let fade_len = fade_len.clamp(8, 2048).min(max_fade_len);
    apply_raised_cosine_tail_fade(samples, fade_len);
}

fn max_polyphase_half_width(phase_count: usize) -> usize {
    let max_taps =
        MAX_POLYPHASE_COEFFICIENT_TABLE_BYTES / size_of::<f64>() / (phase_count + 1).max(1);
    max_taps.saturating_sub(1) / 2
}

fn build_polyphase_coefficient_table(
    half_width: usize,
    phase_count: usize,
    beta: f64,
    cutoff: f64,
) -> Vec<f64> {
    let num_taps = 2 * half_width + 1;
    let mut coefficients = vec![0.0; (phase_count + 1) * num_taps];
    let n_max = half_width as f64;

    for p in 0..=phase_count {
        let tau = p as f64 / phase_count as f64;
        let phase_offset = p * num_taps;
        let row = &mut coefficients[phase_offset..phase_offset + num_taps];
        for (idx, coeff) in row.iter_mut().enumerate() {
            let n_offset = (idx as f64 - n_max) - tau;
            *coeff = windowed_sinc_sample(n_offset, n_max, beta, cutoff);
        }
        normalize_coefficients(row);
    }

    coefficients
}

fn build_exact_polyphase_coefficient_table(
    half_width: usize,
    phase_den: usize,
    beta: f64,
    cutoff: f64,
) -> Vec<f64> {
    let num_taps = 2 * half_width + 1;
    let mut coefficients = vec![0.0; phase_den * num_taps];
    let n_max = half_width as f64;

    for p in 0..phase_den {
        let tau = p as f64 / phase_den as f64;
        let phase_offset = p * num_taps;
        let row = &mut coefficients[phase_offset..phase_offset + num_taps];
        for (idx, coeff) in row.iter_mut().enumerate() {
            let n_offset = (idx as f64 - n_max) - tau;
            *coeff = windowed_sinc_sample(n_offset, n_max, beta, cutoff);
        }
        normalize_coefficients(row);
    }

    coefficients
}

fn build_exact_polyphase_coefficient_table_for_filter(
    filter_type: FilterType,
    half_width: usize,
    phase_den: usize,
    cutoff: f64,
) -> Vec<f64> {
    let beta = filter_type.beta();
    let phase_mode = phase_mode_for_filter(filter_type);
    if filter_type.requires_phase_aware_kernel() && phase_den <= MAX_PHASE_AWARE_RATIONAL_PHASE_DEN
    {
        build_phase_aware_exact_polyphase_coefficient_table(
            half_width, phase_den, beta, cutoff, phase_mode,
        )
    } else {
        build_exact_polyphase_coefficient_table(half_width, phase_den, beta, cutoff)
    }
}

fn build_phase_aware_exact_polyphase_coefficient_table(
    half_width: usize,
    phase_den: usize,
    beta: f64,
    cutoff: f64,
    phase_mode: PhaseMode,
) -> Vec<f64> {
    let prototype = build_full_rate_rational_prototype(half_width, phase_den, beta, cutoff);
    let split_scale = 2.0 / phase_den as f64;
    let impulse = match phase_mode {
        PhaseMode::Linear => prototype,
        PhaseMode::Minimum => minimum16k_phase_impulse(&prototype),
        PhaseMode::MinimumPhase128k(_) => {
            minimum_phase_impulse_with_params(&prototype, minimum128k_phase_params())
        }
        // Exact rational bridges have a phase grid other than the Compact
        // prototype's fixed 2x grid. Retain a pure minimum-phase kernel there;
        // the direct Planck target is used by the intended integer cascades.
        PhaseMode::MinimumPhaseCompact128k(_) => {
            minimum_phase_impulse_with_params(&prototype, minimum128k_phase_params())
        }
        PhaseMode::SplitPhase128k => {
            let mut params = split128k_phase_params();
            params.split_f_lo *= split_scale;
            params.split_f_hi *= split_scale;
            split_phase_impulse_with_params(&prototype, params)
        }
        PhaseMode::IntegratedPhase128k(profile) => {
            let mut params = integrated128k_phase_params(profile);
            let frequency_scale = 2.0 / phase_den as f64;
            params.transition_f_lo *= frequency_scale;
            params.transition_f_hi *= frequency_scale;
            integrated_phase_impulse_with_params(&prototype, params)
        }
    };

    deinterleave_rational_impulse_into_rows(&impulse, half_width, phase_den)
}

fn build_full_rate_rational_prototype(
    half_width: usize,
    phase_den: usize,
    beta: f64,
    cutoff: f64,
) -> Vec<f64> {
    let num_taps = 2 * half_width + 1;
    let proto_len = num_taps * phase_den;
    let n_max = half_width as f64;
    let phase_span = (phase_den.saturating_sub(1)) as f64 / phase_den as f64;
    let mut proto = vec![0.0; proto_len];
    for (idx, slot) in proto.iter_mut().enumerate() {
        let pos = idx as f64 / phase_den as f64 - n_max - phase_span;
        *slot = windowed_sinc_sample(pos, n_max, beta, cutoff);
    }
    normalize_coefficients(&mut proto);
    proto
}

fn deinterleave_rational_impulse_into_rows(
    impulse: &[f64],
    half_width: usize,
    phase_den: usize,
) -> Vec<f64> {
    let num_taps = 2 * half_width + 1;
    let mut coefficients = vec![0.0; phase_den * num_taps];

    for phase in 0..phase_den {
        let row_offset = phase * num_taps;
        let fine_phase = phase_den - 1 - phase;
        let row = &mut coefficients[row_offset..row_offset + num_taps];
        for (tap, coeff) in row.iter_mut().enumerate() {
            let src = tap * phase_den + fine_phase;
            if src < impulse.len() {
                *coeff = impulse[src];
            }
        }
        row.reverse();
        normalize_coefficients(row);
    }

    coefficients
}

fn build_phase_coefficients(half_width: usize, phase: f64, beta: f64, cutoff: f64) -> Vec<f64> {
    let num_taps = 2 * half_width + 1;
    let n_max = half_width as f64;
    let mut coeffs = vec![0.0; num_taps];

    for (idx, coeff) in coeffs.iter_mut().enumerate() {
        let n_offset = (idx as f64 - n_max) - phase;
        *coeff = windowed_sinc_sample(n_offset, n_max, beta, cutoff);
    }

    normalize_coefficients(&mut coeffs);
    coeffs
}

fn windowed_sinc_sample(n_offset: f64, n_max: f64, beta: f64, cutoff: f64) -> f64 {
    2.0 * cutoff * sinc(2.0 * cutoff * n_offset) * kaiser_window(n_offset, n_max, beta)
}

fn normalize_coefficients(coeffs: &mut [f64]) {
    // Kahan-compensated sum: at 32k+ taps and -180 dB stopband targets,
    // naive summation noise (~n·eps) is no longer safely below the
    // coefficient floor.
    let mut sum = 0.0_f64;
    let mut compensation = 0.0_f64;
    for &coeff in coeffs.iter() {
        let y = coeff - compensation;
        let t = sum + y;
        compensation = (t - sum) - y;
        sum = t;
    }
    if sum.abs() > 1e-12 {
        for coeff in coeffs {
            *coeff /= sum;
        }
    }
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let mut k = 1.0;
    while term > 1e-15 * sum {
        term *= (x * x) / (4.0 * k * k);
        sum += term;
        k += 1.0;
    }
    sum
}

fn kaiser_window(n: f64, n_max: f64, beta: f64) -> f64 {
    if n.abs() > n_max {
        0.0
    } else {
        let x = n / n_max;
        bessel_i0(beta * (1.0 - x * x).sqrt()) / bessel_i0(beta)
    }
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.0
    } else {
        let px = PI * x;
        px.sin() / px
    }
}

#[cfg(test)]
#[path = "resampler_tests.rs"]
mod tests;
