use realfft::num_complex::Complex64;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
#[cfg(feature = "research-filter-assets")]
use sha2::{Digest, Sha256};
use std::f64::consts::PI;
use std::sync::{Arc, OnceLock};
#[cfg(feature = "research-filter-assets")]
use std::{fs, path::Path};

const DEFAULT_LATENCY_BUDGET_MS: f64 = 100.0;
const POLYPHASE_PHASES: usize = 4096;
const MAX_EXACT_RATIONAL_PHASE_DEN: usize = 1280;
const MAX_PHASE_AWARE_RATIONAL_PHASE_DEN: usize = 160;
const MAX_COEFFICIENT_TABLE_BYTES: usize = 16 * 1024 * 1024;
const MAX_POLYPHASE_COEFFICIENT_TABLE_BYTES: usize = 128 * 1024 * 1024;
const MAX_SIMD_DIRECT_TAPS: usize = 257;
const HOMOMORPHIC_MAG_FLOOR_REL: f64 = 1e-12;
const PHASE_CONVERTED_TAIL_FADE_FRACTION: f64 = 0.01;
const EOF_DRAIN_ZERO_BLOCK_FRAMES: usize = 4096;
const MINIMUM16K_PRODUCTION_CUTOFF: f64 = 0.467621;
const MINIMUM16K_PRODUCTION_BETA: f64 = 20.47325;
const MINIMUM16K_PRODUCTION_TAIL_FADE: f64 = 0.007617;
const MINIMUM16K_PRODUCTION_MAG_FLOOR_REL: f64 = 1.13771845358e-12;
const LINEAR128K_TAPS_TOTAL: usize = 131_073;
const MINIMUM_COMPACT_BRANCH_TAPS: usize = 131_071;
const MINIMUM_COMPACT_IMPULSE_SAMPLES: usize = 262_141;
const MINIMUM_COMPACT_FFT_MULTIPLIER: usize = 8;
const MINIMUM_COMPACT_CLEANUP_BETA: f64 = 20.47325;
const MINIMUM_COMPACT_PRODUCTION_STOP_GAIN: f64 = 6.309_573_444_801_93e-8; // -144.0 dB
const MINIMUM_COMPACT_TAIL_FADE_FRACTION: f64 = 512.0 / MINIMUM_COMPACT_IMPULSE_SAMPLES as f64;

#[derive(Clone, Copy)]
struct MinimumCompactParams {
    pass_edge_2x: f64,
    stop_edge_2x: f64,
    stop_gain: f64,
    tail_fade_samples: usize,
}

const MINIMUM_COMPACT_PRODUCTION_PARAMS: MinimumCompactParams = MinimumCompactParams {
    pass_edge_2x: 20_200.0 / 88_200.0,
    stop_edge_2x: 22_050.0 / 88_200.0,
    stop_gain: MINIMUM_COMPACT_PRODUCTION_STOP_GAIN,
    tail_fade_samples: 512,
};

const LINEAR128K_PRODUCTION_CUTOFF: f64 = 0.465333;
const LINEAR128K_PRODUCTION_BETA: f64 = 23.12088;
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/filters/split_phase_e3/generated.rs"
));

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

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum FilterType {
    #[serde(alias = "SincExtreme32k")]
    LinearPhase128k,
    Minimum16k,
    #[serde(
        alias = "MinimumPhase128k",
        alias = "MinimumPhase128kV2",
        alias = "MinimumPhase128kV3",
        alias = "MinimumPhase128kV4",
        alias = "MinimumPhase128kV5",
        alias = "MinimumPhaseCompact128kV2"
    )]
    MinimumPhaseCompact128k,
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
        alias = "Split128k-Tap",
        alias = "Split128k",
        alias = "Split128kV2",
        alias = "SplitPhase128kV3",
        alias = "SplitPhase128kV4",
        alias = "SplitPhase128kE2v3",
        alias = "SplitPhase128kV5E2v3",
        alias = "IntegratedPhase128k",
        alias = "IntegratedPhase128kV2",
        alias = "IntegratedPhase128kV3",
        alias = "IntegratedPhase128kV4",
        alias = "SmoothPhase128k",
        alias = "SplitPhaseB",
        alias = "split-phase-b"
    )]
    SplitPhase128kE3,
}

pub const DEFAULT_FILTER_TYPE: FilterType = FilterType::SplitPhase128kE3;
pub const DEFAULT_FILTER_NAME: &str = "SplitPhase128kE3";

impl FilterType {
    pub fn as_id(self) -> u32 {
        match self {
            FilterType::LinearPhase128k => 33,
            FilterType::Minimum16k => 15,
            FilterType::MinimumPhaseCompact128k => 30,
            FilterType::SplitPhase128kE3 => 38,
        }
    }

    pub fn from_id(id: u32) -> Option<Self> {
        match id {
            6 | 33 => Some(FilterType::LinearPhase128k),
            15 => Some(FilterType::Minimum16k),
            26..=31 => Some(FilterType::MinimumPhaseCompact128k),
            0 | 2 | 11 | 16..=25 | 32 | 34..=38 => Some(FilterType::SplitPhase128kE3),
            _ => None,
        }
    }

    pub fn as_name(self) -> &'static str {
        match self {
            FilterType::LinearPhase128k => "LinearPhase128k",
            FilterType::Minimum16k => "Minimum16k",
            FilterType::MinimumPhaseCompact128k => "MinimumPhaseCompact128k",
            FilterType::SplitPhase128kE3 => "SplitPhase128kE3",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "LinearPhase128k" | "SincExtreme32k" => Some(FilterType::LinearPhase128k),
            "Minimum16k" => Some(FilterType::Minimum16k),
            "MinimumPhaseCompact128k"
            | "MinimumPhaseCompact128kV2"
            | "MinimumPhase128k"
            | "MinimumPhase128kV2"
            | "MinimumPhase128kV3"
            | "MinimumPhase128kV4"
            | "MinimumPhase128kV5" => Some(FilterType::MinimumPhaseCompact128k),
            "SplitPhase128kE3"
            | "SplitPhaseE3"
            | "split-phase-e3"
            | "SplitPhaseB"
            | "split-phase-b"
            | "Split128k"
            | "Split128kTap"
            | "Split128k-Tap"
            | "Split128kV2"
            | "SplitPhase128kV3"
            | "Split128kV3"
            | "SplitPhase128kV4"
            | "Split128kV4"
            | "SplitPhase128kE2v3"
            | "SplitPhase128kV5E2v3"
            | "IntegratedPhase128k"
            | "IntegratedPhase"
            | "IntegratedPhase128kV2"
            | "IntegratedPhase128kV3"
            | "IntegratedPhase128kV4"
            | "SmoothPhase128k"
            | "Linear"
            | "SincMedium"
            | "SincExperimental1m"
            | "Mixed16k"
            | "Perfect"
            | "Split16k"
            | "Split16kDsd128"
            | "Split16kDsd128Apod"
            | "Split16k-DSD128"
            | "Split32k"
            | "Split32kTap"
            | "Split32k-Tap" => Some(FilterType::SplitPhase128kE3),
            _ => None,
        }
    }

    fn cutoff(self) -> f64 {
        match self {
            FilterType::LinearPhase128k => env_f64("FOZMO_LINEAR128K_CUTOFF")
                .unwrap_or(LINEAR128K_PRODUCTION_CUTOFF)
                .clamp(0.40, 0.49),
            FilterType::Minimum16k => env_f64("FOZMO_MINIMUM16K_CUTOFF")
                .unwrap_or(MINIMUM16K_PRODUCTION_CUTOFF)
                .clamp(0.40, 0.49),
            FilterType::MinimumPhaseCompact128k => {
                MINIMUM_COMPACT_PRODUCTION_PARAMS.stop_edge_2x * 2.0
            }
            FilterType::SplitPhase128kE3 => 0.0,
        }
    }

    fn beta(self) -> f64 {
        match self {
            FilterType::LinearPhase128k => env_f64("FOZMO_LINEAR128K_BETA")
                .unwrap_or(LINEAR128K_PRODUCTION_BETA)
                .clamp(8.0, 32.0),
            FilterType::Minimum16k => env_f64("FOZMO_MINIMUM16K_BETA")
                .unwrap_or(MINIMUM16K_PRODUCTION_BETA)
                .clamp(8.0, 32.0),
            FilterType::MinimumPhaseCompact128k => MINIMUM_COMPACT_CLEANUP_BETA,
            FilterType::SplitPhase128kE3 => 0.0,
        }
    }

    fn character_beta(self) -> Option<f64> {
        match self {
            Self::MinimumPhaseCompact128k => None,
            _ => Some(self.beta()),
        }
    }

    fn cleanup_beta(self) -> f64 {
        match self {
            Self::MinimumPhaseCompact128k => MINIMUM_COMPACT_CLEANUP_BETA,
            _ => self.beta(),
        }
    }

    fn is_high_latency(self) -> bool {
        true
    }

    fn requires_phase_aware_kernel(self) -> bool {
        true
    }

    fn uses_frozen_coefficients(self) -> bool {
        self == Self::SplitPhase128kE3
    }
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
    MinimumPhaseCompact128k,
    FrozenSplitPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharacterCoefficientSource {
    Procedural,
    Frozen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupCoefficientSource {
    Procedural,
    Frozen { stage_index: u8 },
}

#[derive(Debug, Clone)]
pub enum StageSpec {
    Character2x {
        taps_total: usize,
        cutoff: f64,
        beta: f64,
        engine: EngineKind,
        phase_mode: PhaseMode,
        coefficient_source: CharacterCoefficientSource,
    },
    CleanupHalfband2x {
        taps_total: usize,
        beta: f64,
        cutoff: f64,
        // Even phase is a pure delay. The odd branch is still represented as dense
        // coefficients; prototype-level zero-tap sparsity is a later optimization.
        engine: EngineKind,
        coefficient_source: CleanupCoefficientSource,
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
            StageSpec::Character2x {
                coefficient_source: CharacterCoefficientSource::Frozen,
                ..
            } => split_phase_e3_assets()
                .alignment
                .full_rate_origin
                .div_ceil(2),
            StageSpec::Character2x { phase_mode, .. } => match phase_mode {
                PhaseMode::Linear => linear_group_delay,
                PhaseMode::Minimum | PhaseMode::MinimumPhaseCompact128k => 0,
                PhaseMode::FrozenSplitPhase => {
                    unreachable!("frozen split-phase latency uses its exported alignment contract")
                }
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
                if ratio.is_power_of_two() && ratio > 1 && ratio <= 512 {
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
                            && (phase_den > MAX_PHASE_AWARE_RATIONAL_PHASE_DEN
                                || (filter_type.uses_frozen_coefficients()
                                    && !frozen_rational_asset_supported(
                                        step_num, phase_den,
                                    )))
                        {
                            if filter_type.uses_frozen_coefficients() {
                                eprintln!(
                                    "resampler: {} has no frozen table for exact ratio {} -> {}; using a generic linear-phase rational kernel",
                                    filter_type.as_name(), source_rate, target_rate
                                );
                            } else {
                                eprintln!(
                                    "resampler: {} phase profile is not preserved for exact ratio {} -> {} (phase denominator {} exceeds {}); using a generic linear-phase rational kernel",
                                    filter_type.as_name(),
                                    source_rate,
                                    target_rate,
                                    phase_den,
                                    MAX_PHASE_AWARE_RATIONAL_PHASE_DEN
                                );
                            }
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
                if self.filter_type.uses_frozen_coefficients() {
                    frozen_rational_asset_supported(ratio_num as usize, ratio_den as usize)
                } else {
                    !self.filter_type.requires_phase_aware_kernel()
                        || ratio_den as usize <= MAX_PHASE_AWARE_RATIONAL_PHASE_DEN
                }
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
    if !ratio.is_power_of_two() || !(2..=512).contains(&ratio) {
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

fn frozen_rational_asset_supported(step_num: usize, phase_den: usize) -> bool {
    matches!((step_num, phase_den), (147, 160) | (160, 147))
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
        FilterType::SplitPhase128kE3 => 131_073,
        FilterType::MinimumPhaseCompact128k => MINIMUM_COMPACT_BRANCH_TAPS,
        FilterType::LinearPhase128k => LINEAR128K_TAPS_TOTAL,
        FilterType::Minimum16k => 16_385,
    };
    let engine = EngineKind::PartitionedFft {
        partition_frames: 4096,
    };
    let coefficient_source = if family.uses_frozen_coefficients() {
        CharacterCoefficientSource::Frozen
    } else {
        CharacterCoefficientSource::Procedural
    };
    StageSpec::Character2x {
        taps_total,
        cutoff: if coefficient_source == CharacterCoefficientSource::Procedural {
            family.cutoff()
        } else {
            0.0
        },
        beta: if coefficient_source == CharacterCoefficientSource::Procedural {
            family
                .character_beta()
                .unwrap_or(MINIMUM_COMPACT_CLEANUP_BETA)
        } else {
            0.0
        },
        engine,
        phase_mode: phase_mode_for_filter(family),
        coefficient_source,
    }
}

fn phase_mode_for_filter(family: FilterType) -> PhaseMode {
    match family {
        FilterType::SplitPhase128kE3 => PhaseMode::FrozenSplitPhase,
        FilterType::MinimumPhaseCompact128k => PhaseMode::MinimumPhaseCompact128k,
        FilterType::Minimum16k => PhaseMode::Minimum,
        FilterType::LinearPhase128k => PhaseMode::Linear,
    }
}

fn cleanup_stage_spec(stage_idx: usize, family: FilterType) -> StageSpec {
    // Tap-count taper. Stages ≥ 3 in a long cascade (≥ 32×) suppress images
    // sitting well above the audible band — typically > 1.4 MHz — so a short
    // kernel is plenty. Keeping the count ≤ MAX_SIMD_DIRECT_TAPS (257) lets
    // the AVX2/FMA direct path stay engaged for every late stage.
    let taps_total = match stage_idx {
        1 => 255,
        2 => 127,
        3 => 63,
        _ => 31,
    };
    StageSpec::CleanupHalfband2x {
        taps_total,
        beta: if family.uses_frozen_coefficients() {
            0.0
        } else {
            family.cleanup_beta()
        },
        // Long apodizing filters deliberately keep cleanups at 0.5: their
        // character stage already provides the anti-image margin, so cleanups
        // can take the true-halfband structure (even branch an exact delay).
        cutoff: 0.5,
        engine: EngineKind::DirectSimd,
        coefficient_source: if family.uses_frozen_coefficients() {
            CleanupCoefficientSource::Frozen {
                // Frozen bundles were certified through 256x (seven cleanup
                // stages). At 512x, reuse the terminal halfband: the audio
                // band occupies half its former normalized width, so this is
                // a conservative extension of the certified response rather
                // than a looser procedural fallback.
                stage_index: stage_idx.min(7) as u8,
            }
        } else {
            CleanupCoefficientSource::Procedural
        },
    }
}

fn estimate_plan_memory_bytes(stages: &[StageSpec]) -> usize {
    let engine_bytes: usize = stages
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
        .sum();
    let frozen_asset_bytes = stages
        .iter()
        .find_map(|stage| match stage {
            StageSpec::Character2x {
                coefficient_source: CharacterCoefficientSource::Frozen,
                ..
            } => Some(frozen_total_asset_coefficients() * size_of::<f64>()),
            _ => None,
        })
        .unwrap_or(0);
    engine_bytes + frozen_asset_bytes
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
                coefficient_source,
            } => {
                let half_width = taps_total / 2;
                let (phase0_coeffs, phase1_coeffs, prepad0, prepad1) = match coefficient_source {
                    CharacterCoefficientSource::Procedural => {
                        build_character_polyphase_pair(half_width, *beta, *cutoff, *phase_mode)
                    }
                    CharacterCoefficientSource::Frozen => frozen_character_polyphase_pair(),
                };
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
                coefficient_source,
            } => {
                let half_width = taps_total / 2;
                let phase1_coeffs = match coefficient_source {
                    CleanupCoefficientSource::Procedural => {
                        build_phase_coefficients(half_width, 0.5, *beta, *cutoff)
                    }
                    CleanupCoefficientSource::Frozen { stage_index } => {
                        frozen_cleanup_odd_branch(*stage_index)
                    }
                };
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
        let mut estimated_memory_bytes = steps
            .iter()
            .map(DownsampleStep::estimated_memory_bytes)
            .sum();
        if filter_type.uses_frozen_coefficients() {
            estimated_memory_bytes += frozen_total_asset_coefficients() * size_of::<f64>();
        }
        let high_latency = filter_type.is_high_latency() || latency_ms > DEFAULT_LATENCY_BUDGET_MS;

        Some(Self {
            steps,
            source_rate,
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
    engine: Box<dyn FirEngine>,
    next_filtered_parity: usize,
    filtered_l: Vec<f64>,
    filtered_r: Vec<f64>,
    latency_ms: f64,
    estimated_memory_bytes: usize,
}

impl DecimateBy2Stage {
    fn new(_filter_type: FilterType, spec: &StageSpec, input_rate: u32) -> Self {
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
    if let StageSpec::Character2x {
        coefficient_source: CharacterCoefficientSource::Frozen,
        ..
    } = spec
    {
        let assets = split_phase_e3_assets();
        let mut impulse = assets.character.to_vec();
        impulse.reverse();
        return (impulse, assets.alignment.decimation_prepad);
    }
    if let StageSpec::CleanupHalfband2x {
        coefficient_source: CleanupCoefficientSource::Frozen { stage_index },
        ..
    } = spec
    {
        let assets = split_phase_e3_assets();
        let mut impulse = assets
            .cleanups
            .get(usize::from(*stage_index).saturating_sub(1))
            .unwrap_or_else(|| panic!("invalid frozen split-phase cleanup stage {stage_index}"))
            .to_vec();
        impulse.reverse();
        let prepad = impulse.len() / 2;
        return (impulse, prepad);
    }
    let (mut impulse, origin) = match spec {
        StageSpec::Character2x {
            taps_total,
            cutoff,
            beta,
            phase_mode,
            coefficient_source,
            ..
        } => {
            debug_assert_eq!(*coefficient_source, CharacterCoefficientSource::Procedural);
            let half_width = taps_total / 2;
            let impulse = match phase_mode {
                PhaseMode::MinimumPhaseCompact128k => minimum_phase_compact_impulse(),
                _ => {
                    let proto = build_full_rate_2x_prototype(half_width, *beta, *cutoff);
                    match phase_mode {
                        PhaseMode::Linear => proto,
                        PhaseMode::Minimum => minimum_phase_impulse(&proto),
                        PhaseMode::MinimumPhaseCompact128k => unreachable!(),
                        PhaseMode::FrozenSplitPhase => {
                            unreachable!("frozen split phase bypasses procedural generation")
                        }
                    }
                }
            };
            let origin = dominant_impulse_index(&impulse);
            (impulse, origin)
        }
        StageSpec::CleanupHalfband2x {
            taps_total,
            beta,
            cutoff,
            coefficient_source,
            ..
        } => {
            debug_assert_eq!(*coefficient_source, CleanupCoefficientSource::Procedural);
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

    fn base_half_width(_filter_type: FilterType) -> usize {
        512
    }

    fn fractional_half_width(
        filter_type: FilterType,
        source_rate: u32,
        target_rate: u32,
        phase_count: usize,
    ) -> usize {
        let base = Self::base_half_width(filter_type);
        let base = if phase_count <= MAX_PHASE_AWARE_RATIONAL_PHASE_DEN
            && matches!(filter_type, FilterType::MinimumPhaseCompact128k)
        {
            base.saturating_mul(2)
        } else {
            base
        };
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
        let (base_cutoff, beta) = if self.filter_type.uses_frozen_coefficients() {
            // Unsupported capped paths are intentionally generic linear phase;
            // they do not read V1/V2 tuning environment variables.
            (LINEAR128K_PRODUCTION_CUTOFF, LINEAR128K_PRODUCTION_BETA)
        } else {
            (self.filter_type.cutoff(), self.filter_type.beta())
        };
        let cutoff = if self.target_rate < self.source_rate {
            base_cutoff * self.target_rate as f64 / self.source_rate as f64
        } else {
            base_cutoff
        };

        self.coefficients =
            build_polyphase_coefficient_table(self.half_width, self.phase_count, beta, cutoff);
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
    coefficients: Arc<[f64]>,
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
            coefficients: Arc::from(Vec::<f64>::new()),
            buffer_l: vec![0.0; half_width],
            buffer_r: vec![0.0; half_width],
            current_time_num: half_width * phase_den,
        };
        resampler.precompute_coefficients();
        resampler
    }

    fn estimated_memory_bytes(&self) -> usize {
        let coefficient_bytes = if self.filter_type.uses_frozen_coefficients()
            && frozen_rational_asset_supported(self.step_num, self.phase_den)
        {
            frozen_total_asset_coefficients() * size_of::<f64>()
        } else {
            self.coefficients.len() * size_of::<f64>()
        };
        coefficient_bytes + (self.buffer_l.capacity() + self.buffer_r.capacity()) * size_of::<f64>()
    }

    fn precompute_coefficients(&mut self) {
        if self.filter_type.uses_frozen_coefficients() {
            let assets = split_phase_e3_assets();
            self.coefficients = match (self.step_num, self.phase_den) {
                (147, 160) => {
                    debug_assert_eq!(self.half_width, 512);
                    assets.rational_tables.phase_147_160.clone()
                }
                (160, 147) => {
                    debug_assert_eq!(self.half_width, 1_024);
                    assets.rational_tables.phase_160_147.clone()
                }
                _ => build_exact_polyphase_coefficient_table_for_filter(
                    self.filter_type,
                    self.half_width,
                    self.phase_den,
                    if self.target_rate < self.source_rate {
                        LINEAR128K_PRODUCTION_CUTOFF * self.target_rate as f64
                            / self.source_rate as f64
                    } else {
                        LINEAR128K_PRODUCTION_CUTOFF
                    },
                    self.source_rate,
                    self.target_rate,
                )
                .into(),
            };
            return;
        }
        let base_cutoff = self.filter_type.cutoff();
        let cutoff = if self.target_rate < self.source_rate {
            base_cutoff * self.target_rate as f64 / self.source_rate as f64
        } else {
            base_cutoff
        };
        self.coefficients = build_exact_polyphase_coefficient_table_for_filter(
            self.filter_type,
            self.half_width,
            self.phase_den,
            cutoff,
            self.source_rate,
            self.target_rate,
        )
        .into();
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
        PhaseMode::MinimumPhaseCompact128k => {
            let minimum = minimum_phase_compact_impulse();
            debug_assert_eq!(minimum.len(), 4 * half_width + 1);
            let (phase0, phase1) = split_full_rate_impulse_into_reversed_branches(&minimum);
            let prepad0 = Some(phase0.len().saturating_sub(1));
            let prepad1 = Some(phase1.len().saturating_sub(1));
            (phase0, phase1, prepad0, prepad1)
        }
        PhaseMode::FrozenSplitPhase => {
            unreachable!("frozen split phase bypasses procedural character generation")
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

fn env_f64(name: &str) -> Option<f64> {
    std::env::var(name).ok()?.parse::<f64>().ok()
}

fn decode_f64le_asset(bytes: &[u8], expected_coefficients: usize, label: &str) -> Arc<[f64]> {
    let expected_bytes = expected_coefficients
        .checked_mul(size_of::<f64>())
        .expect("filter asset byte length overflow");
    assert_eq!(
        bytes.len(),
        expected_bytes,
        "{label} coefficient asset length mismatch"
    );
    bytes
        .chunks_exact(size_of::<f64>())
        .map(|chunk| {
            let encoded: [u8; 8] = chunk.try_into().expect("f64 asset chunk size");
            f64::from_le_bytes(encoded)
        })
        .collect::<Vec<_>>()
        .into()
}

#[derive(Clone, Copy)]
struct FrozenAlignment {
    full_rate_origin: usize,
    phase0_prepad: usize,
    phase1_prepad: usize,
    decimation_prepad: usize,
}

struct FrozenRationalTables {
    phase_147_160: Arc<[f64]>,
    phase_160_147: Arc<[f64]>,
}

struct FrozenFilterAssetBundle {
    character: Arc<[f64]>,
    cleanups: [Arc<[f64]>; 7],
    rational_tables: FrozenRationalTables,
    alignment: FrozenAlignment,
}

static SPLIT_PHASE_E3_ASSETS: OnceLock<FrozenFilterAssetBundle> = OnceLock::new();

#[cfg(feature = "research-filter-assets")]
#[derive(Clone, Debug, PartialEq)]
pub struct ResearchE3CharacterMetadata {
    pub sha256: String,
    pub coefficient_count: usize,
    pub dc_sum: f64,
}

#[cfg(feature = "research-filter-assets")]
static RESEARCH_E3_CHARACTER: OnceLock<(Arc<[f64]>, ResearchE3CharacterMetadata)> = OnceLock::new();

/// Install one research E3 character before constructing any E3 resampler.
///
/// This API is deliberately absent from normal builds. A process may install
/// exactly one hash-addressed candidate, and the frozen E2v3 cleanup/rational
/// assets remain unchanged.
#[cfg(feature = "research-filter-assets")]
pub fn install_research_e3_character_file(
    path: &Path,
    expected_sha256: &str,
) -> Result<ResearchE3CharacterMetadata, String> {
    if SPLIT_PHASE_E3_ASSETS.get().is_some() {
        return Err(
            "E3 assets were initialized before the research candidate was installed".into(),
        );
    }
    if RESEARCH_E3_CHARACTER.get().is_some() {
        return Err("a research E3 character is already installed in this process".into());
    }
    let expected = expected_sha256.trim().to_ascii_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("expected research character SHA-256 must contain 64 hex digits".into());
    }
    let bytes = fs::read(path).map_err(|error| {
        format!(
            "could not read research E3 character {}: {error}",
            path.display()
        )
    })?;
    let expected_bytes = SPLIT_PHASE_E3_CHARACTER_COEFFICIENTS * size_of::<f64>();
    if bytes.len() != expected_bytes {
        return Err(format!(
            "research E3 character has {} bytes, expected {expected_bytes}",
            bytes.len()
        ));
    }
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != expected {
        return Err(format!(
            "research E3 character SHA-256 mismatch: expected {expected}, got {actual}"
        ));
    }
    let coefficients = bytes
        .chunks_exact(size_of::<f64>())
        .enumerate()
        .map(|(index, chunk)| {
            let encoded: [u8; 8] = chunk.try_into().expect("f64 asset chunk size");
            let value = f64::from_le_bytes(encoded);
            if value.is_finite() {
                Ok(value)
            } else {
                Err(format!(
                    "research E3 character contains a non-finite value at coefficient {index}"
                ))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let dc_sum = coefficients.iter().sum::<f64>();
    if (dc_sum - 1.0).abs() > 1.0e-12 {
        return Err(format!(
            "research E3 character DC sum {dc_sum:.17} differs from 1.0 by more than 1e-12"
        ));
    }
    let metadata = ResearchE3CharacterMetadata {
        sha256: actual,
        coefficient_count: coefficients.len(),
        dc_sum,
    };
    RESEARCH_E3_CHARACTER
        .set((coefficients.into(), metadata.clone()))
        .map_err(|_| "a research E3 character is already installed".to_string())?;
    Ok(metadata)
}

fn split_phase_e3_character_asset() -> Arc<[f64]> {
    #[cfg(feature = "research-filter-assets")]
    if let Some((character, _)) = RESEARCH_E3_CHARACTER.get() {
        return Arc::clone(character);
    }
    decode_f64le_asset(
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/filters/split_phase_e3/character_full_rate.f64le"
        )),
        SPLIT_PHASE_E3_CHARACTER_COEFFICIENTS,
        "Split Phase B character",
    )
}

fn split_phase_e3_assets() -> &'static FrozenFilterAssetBundle {
    SPLIT_PHASE_E3_ASSETS.get_or_init(|| FrozenFilterAssetBundle {
        character: split_phase_e3_character_asset(),
        cleanups: [
            decode_f64le_asset(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/filters/split_phase_e3/cleanup_stage_1.f64le"
                )),
                SPLIT_PHASE_E3_CLEANUP_COEFFICIENTS[0],
                "Split Phase B cleanup stage 1",
            ),
            decode_f64le_asset(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/filters/split_phase_e3/cleanup_stage_2.f64le"
                )),
                SPLIT_PHASE_E3_CLEANUP_COEFFICIENTS[1],
                "Split Phase B cleanup stage 2",
            ),
            decode_f64le_asset(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/filters/split_phase_e3/cleanup_stage_3.f64le"
                )),
                SPLIT_PHASE_E3_CLEANUP_COEFFICIENTS[2],
                "Split Phase B cleanup stage 3",
            ),
            decode_f64le_asset(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/filters/split_phase_e3/cleanup_stage_4.f64le"
                )),
                SPLIT_PHASE_E3_CLEANUP_COEFFICIENTS[3],
                "Split Phase B cleanup stage 4",
            ),
            decode_f64le_asset(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/filters/split_phase_e3/cleanup_stage_5.f64le"
                )),
                SPLIT_PHASE_E3_CLEANUP_COEFFICIENTS[4],
                "Split Phase B cleanup stage 5",
            ),
            decode_f64le_asset(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/filters/split_phase_e3/cleanup_stage_6.f64le"
                )),
                SPLIT_PHASE_E3_CLEANUP_COEFFICIENTS[5],
                "Split Phase B cleanup stage 6",
            ),
            decode_f64le_asset(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/filters/split_phase_e3/cleanup_stage_7.f64le"
                )),
                SPLIT_PHASE_E3_CLEANUP_COEFFICIENTS[6],
                "Split Phase B cleanup stage 7",
            ),
        ],
        rational_tables: FrozenRationalTables {
            phase_147_160: decode_f64le_asset(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/filters/split_phase_e3/rational_147_160.f64le"
                )),
                SPLIT_PHASE_E3_RATIONAL_147_160_COEFFICIENTS,
                "Split Phase B rational 147/160",
            ),
            phase_160_147: decode_f64le_asset(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/filters/split_phase_e3/rational_160_147.f64le"
                )),
                SPLIT_PHASE_E3_RATIONAL_160_147_COEFFICIENTS,
                "Split Phase B rational 160/147",
            ),
        },
        alignment: FrozenAlignment {
            full_rate_origin: SPLIT_PHASE_E3_FULL_RATE_ORIGIN,
            phase0_prepad: SPLIT_PHASE_E3_PHASE0_PREPAD,
            phase1_prepad: SPLIT_PHASE_E3_PHASE1_PREPAD,
            decimation_prepad: SPLIT_PHASE_E3_DECIMATION_PREPAD,
        },
    })
}

fn frozen_total_asset_coefficients() -> usize {
    let assets = split_phase_e3_assets();
    assets.character.len()
        + assets
            .cleanups
            .iter()
            .map(|values| values.len())
            .sum::<usize>()
        + assets.rational_tables.phase_147_160.len()
        + assets.rational_tables.phase_160_147.len()
}

fn frozen_character_polyphase_pair() -> (Vec<f64>, Vec<f64>, Option<usize>, Option<usize>) {
    let assets = split_phase_e3_assets();
    let mut phase0 = assets
        .character
        .iter()
        .step_by(2)
        .map(|coefficient| 2.0 * coefficient)
        .collect::<Vec<_>>();
    let mut phase1 = assets
        .character
        .iter()
        .skip(1)
        .step_by(2)
        .map(|coefficient| 2.0 * coefficient)
        .collect::<Vec<_>>();
    phase1.push(0.0);
    phase0.reverse();
    phase1.reverse();
    debug_assert_eq!(phase0.len(), 131_073);
    debug_assert_eq!(phase1.len(), 131_073);
    (
        phase0,
        phase1,
        Some(assets.alignment.phase0_prepad),
        Some(assets.alignment.phase1_prepad),
    )
}

fn frozen_cleanup_odd_branch(stage_index: u8) -> Vec<f64> {
    let assets = split_phase_e3_assets();
    let canonical = assets
        .cleanups
        .get(usize::from(stage_index).saturating_sub(1))
        .unwrap_or_else(|| panic!("invalid frozen split-phase cleanup stage {stage_index}"));
    let mut odd = canonical
        .iter()
        .skip(1)
        .step_by(2)
        .map(|coefficient| 2.0 * coefficient)
        .collect::<Vec<_>>();
    odd.push(0.0);
    odd.reverse();
    odd
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
    let inv = planner.plan_fft_inverse(fft_len);
    let mut minimum_spectrum = minimum_phase_spectrum_from_magnitude(magnitude, fft_len);
    let mut min_phase = inv.make_output_vec();
    inv.process(&mut minimum_spectrum, &mut min_phase)
        .expect("inverse FFT plan should match the allocated buffers");
    for x in min_phase.iter_mut() {
        *x *= scale;
    }

    min_phase.truncate(output_len);
    apply_raised_cosine_tail_fade(&mut min_phase, tail_fade_samples);
    normalize_coefficients(&mut min_phase);
    min_phase
}

#[allow(clippy::needless_range_loop)]
fn minimum_phase_spectrum_from_magnitude(magnitude: &[f64], fft_len: usize) -> Vec<Complex64> {
    assert_eq!(magnitude.len(), fft_len / 2 + 1);
    let scale = 1.0 / fft_len as f64;
    let mut planner = RealFftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(fft_len);
    let inv = planner.plan_fft_inverse(fft_len);
    let mut log_spectrum = magnitude
        .iter()
        .map(|mag| Complex64::new(mag.max(f64::MIN_POSITIVE).ln(), 0.0))
        .collect::<Vec<_>>();

    // IFFT to cepstrum (real-valued in time domain).
    let mut cepstrum = inv.make_output_vec();
    inv.process(&mut log_spectrum, &mut cepstrum)
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
    folded_spec
}

fn smootherstep7(x: f64) -> f64 {
    let t = x.clamp(0.0, 1.0);
    let t2 = t * t;
    let t4 = t2 * t2;
    t4 * (35.0 + t * (-84.0 + t * (70.0 - 20.0 * t)))
}

fn compact_minimum_magnitude(frequency: f64, params: MinimumCompactParams) -> f64 {
    if frequency <= params.pass_edge_2x {
        return 1.0;
    }
    if frequency < params.stop_edge_2x {
        let x = (frequency - params.pass_edge_2x) / (params.stop_edge_2x - params.pass_edge_2x);
        let blend = smootherstep7(x);
        return params.stop_gain + (1.0 - params.stop_gain) * (1.0 - blend);
    }
    params.stop_gain
}

fn minimum_compact_params() -> MinimumCompactParams {
    MINIMUM_COMPACT_PRODUCTION_PARAMS
}

fn minimum_phase_compact_impulse() -> Vec<f64> {
    static PRODUCTION: OnceLock<Vec<f64>> = OnceLock::new();
    let params = minimum_compact_params();
    PRODUCTION
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
    source_rate: u32,
    target_rate: u32,
) -> Vec<f64> {
    if filter_type.uses_frozen_coefficients() {
        return build_exact_polyphase_coefficient_table(
            half_width,
            phase_den,
            LINEAR128K_PRODUCTION_BETA,
            cutoff,
        );
    }
    let beta = filter_type.beta();
    let phase_mode = phase_mode_for_filter(filter_type);
    if filter_type.requires_phase_aware_kernel() && phase_den <= MAX_PHASE_AWARE_RATIONAL_PHASE_DEN
    {
        build_phase_aware_exact_polyphase_coefficient_table(
            half_width,
            phase_den,
            beta,
            cutoff,
            phase_mode,
            source_rate,
            target_rate,
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
    source_rate: u32,
    target_rate: u32,
) -> Vec<f64> {
    if phase_mode == PhaseMode::MinimumPhaseCompact128k {
        let impulse =
            minimum_phase_compact_rational_impulse(half_width, phase_den, source_rate, target_rate);
        return deinterleave_rational_impulse_into_rows(&impulse, half_width, phase_den);
    }

    let prototype = build_full_rate_rational_prototype(half_width, phase_den, beta, cutoff);
    let impulse = match phase_mode {
        PhaseMode::Linear => prototype,
        PhaseMode::Minimum => minimum16k_phase_impulse(&prototype),
        PhaseMode::MinimumPhaseCompact128k => unreachable!(),
        PhaseMode::FrozenSplitPhase => {
            unreachable!("frozen split phase uses a frozen rational table")
        }
    };

    deinterleave_rational_impulse_into_rows(&impulse, half_width, phase_den)
}

fn minimum_phase_compact_rational_impulse(
    half_width: usize,
    phase_den: usize,
    source_rate: u32,
    target_rate: u32,
) -> Vec<f64> {
    let mut params = minimum_compact_params();
    // Each fine-grid polyphase component has approximately 1/phase_den of
    // the prototype's DC gain and is normalized back to unity after
    // deinterleaving. Compensate the flat stop floor for that gain.
    params.stop_gain /= phase_den as f64;
    let num_taps = 2 * half_width + 1;
    let output_len = num_taps
        .checked_mul(phase_den)
        .expect("compact rational impulse length overflow");
    let padded_len = output_len
        .checked_mul(MINIMUM_COMPACT_FFT_MULTIPLIER)
        .expect("compact rational FFT length overflow");
    let fft_len = padded_len.next_power_of_two().max(8);
    let anti_alias_scale = if target_rate < source_rate {
        target_rate as f64 / source_rate as f64
    } else {
        1.0
    };
    let magnitude = (0..=fft_len / 2)
        .map(|bin| {
            let fine_frequency = bin as f64 / fft_len as f64;
            let compact_frequency = fine_frequency * phase_den as f64 / (2.0 * anti_alias_scale);
            compact_minimum_magnitude(compact_frequency, params)
        })
        .collect::<Vec<_>>();
    let max_tail_fade = output_len / 4;
    let scaled_tail_fade =
        ((output_len as f64) * MINIMUM_COMPACT_TAIL_FADE_FRACTION).round() as usize;
    let tail_fade_samples = if max_tail_fade >= 8 {
        scaled_tail_fade.clamp(8, max_tail_fade)
    } else {
        max_tail_fade
    };

    minimum_phase_from_magnitude(&magnitude, fft_len, output_len, tail_fade_samples)
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
