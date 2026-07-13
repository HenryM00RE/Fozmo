use super::beam_error_profile::{
    BeamErrorProfile, MAX_BEAM_ERROR_PROFILE_STATES, StreamingQuantile, profiles_for_wire_rate,
};
use super::coeff_math::{denormalized_feedback8, mul8};
use super::modulator::{CrfbModulator, ModulatorMode};
use super::stability::{StateStability, stabilize_state};
use crate::audio::dsd::dsd_coeffs::CRFB_OSR64_OBG165;
use crate::audio::dsd::dsd_coeffs::ModulatorCoeffs;

const ECBEAM2_WIDTH: usize = 4;
const ECBEAM2_HORIZON: usize = 8;
const ECBEAM2_CHILDREN: usize = 2 * ECBEAM2_WIDTH;
const ECBEAM2_EXACT_MAX_HORIZON: usize = 16;
const ERROR_EMA_SECONDS: f64 = 0.010;

/// Stable value identity for the one CRFB plant admitted by the v1
/// experiment. Comparing coefficient bits (rather than an address) also
/// admits an independently linked copy of the generated constant while
/// rejecting arbitrary OSR64 overrides.
pub(crate) fn ecbeam2_v1_coefficients_match(coeffs: &ModulatorCoeffs) -> bool {
    let expected = &CRFB_OSR64_OBG165;
    coeffs.osr == expected.osr
        && coeffs.obg.to_bits() == expected.obg.to_bits()
        && coeffs.input_peak.to_bits() == expected.input_peak.to_bits()
        && coeffs.d1.to_bits() == expected.d1.to_bits()
        && coeffs
            .a
            .iter()
            .flatten()
            .zip(expected.a.iter().flatten())
            .all(|(left, right)| left.to_bits() == right.to_bits())
        && coeffs
            .b
            .iter()
            .flatten()
            .zip(expected.b.iter().flatten())
            .all(|(left, right)| left.to_bits() == right.to_bits())
        && coeffs
            .c
            .iter()
            .zip(expected.c.iter())
            .all(|(left, right)| left.to_bits() == right.to_bits())
        && coeffs
            .state_limit
            .iter()
            .zip(expected.state_limit.iter())
            .all(|(left, right)| left.to_bits() == right.to_bits())
}

/// EcBeam2 always uses the fixed DSD64 reconstruction experiment described by
/// `Harness24To32V1`. Additional profiles must be introduced as distinct,
/// versioned identifiers rather than silently changing these coefficients.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EcBeam2ProfileId {
    #[default]
    Harness24To32V1,
}

/// `ShadowA1` is implemented by the production-frontier observer, not by the
/// isolated active engine. This keeps production EcBeam as the authoritative
/// chooser and avoids promising arithmetic identity from a second A1 engine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EcBeam2RunMode {
    #[default]
    Active,
    ShadowA1,
}

/// Half-open committed-modulator sequence range used by quality tooling.
///
/// The engine always advances its CRFB, reconstruction, ultrasonic, and EMA
/// states before and through this range. Only reported measurement energies,
/// extrema, and quantiles are gated, so a difficult window retains its real
/// warmed-up starting state without attributing prefix extrema to the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcBeam2DiagnosticWindow {
    pub start_sequence: u64,
    pub end_sequence: u64,
}

impl EcBeam2DiagnosticWindow {
    #[inline]
    pub(crate) fn contains(self, sequence: u64) -> bool {
        self.start_sequence <= sequence && sequence < self.end_sequence
    }

    #[inline]
    pub(crate) fn is_valid(self) -> bool {
        self.start_sequence < self.end_sequence
    }
}

/// Experiment-only controls for the isolated EcBeam2 objective.
///
/// Every zero/`None` value is inert. In particular, the reconstruction-only
/// control remains available; state potential, barrier, and raw-quantizer terms
/// cannot enter the metric unless a candidate explicitly enables them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EcBeam2ExperimentConfig {
    pub run_mode: EcBeam2RunMode,
    pub profile: EcBeam2ProfileId,
    /// Weight on the telescoping normalized CRFB state potential delta.
    pub state_terminal_weight: f64,
    /// Normalized barrier knee. This is not a terminal cost.
    pub state_deadzone: f64,
    pub state_deadzone_weight: f64,
    pub quantizer_regularizer: f64,
    pub ultrasonic_budget: Option<f64>,
    pub signed_error_budget: Option<f64>,
    /// Optional quality-tool measurement boundary in the post-filter,
    /// post-headroom, post-limiter `u` sequence domain.
    pub diagnostic_window: Option<EcBeam2DiagnosticWindow>,
}

impl Default for EcBeam2ExperimentConfig {
    fn default() -> Self {
        Self {
            run_mode: EcBeam2RunMode::Active,
            profile: EcBeam2ProfileId::Harness24To32V1,
            state_terminal_weight: 0.0,
            state_deadzone: 0.0,
            state_deadzone_weight: 0.0,
            quantizer_regularizer: 0.0,
            ultrasonic_budget: None,
            signed_error_budget: None,
            diagnostic_window: None,
        }
    }
}

impl EcBeam2ExperimentConfig {
    /// Validate the exact effective research configuration. Invalid values are
    /// rejected instead of being silently clamped or disabling a constraint.
    pub fn validated(self) -> Result<Self, &'static str> {
        if !self.state_terminal_weight.is_finite() || self.state_terminal_weight < 0.0 {
            return Err("EcBeam2 state-terminal weight must be finite and non-negative");
        }
        if !self.state_deadzone.is_finite() || !(0.0..=1.0).contains(&self.state_deadzone) {
            return Err("EcBeam2 state dead-zone must be finite and in 0..=1");
        }
        if !self.state_deadzone_weight.is_finite() || self.state_deadzone_weight < 0.0 {
            return Err("EcBeam2 state dead-zone weight must be finite and non-negative");
        }
        if self.state_deadzone_weight > 0.0 && self.state_deadzone >= 1.0 {
            return Err("EcBeam2 enabled state barrier requires a knee below the hard limit");
        }
        if !self.quantizer_regularizer.is_finite() || self.quantizer_regularizer < 0.0 {
            return Err("EcBeam2 quantizer regularizer must be finite and non-negative");
        }
        if self
            .ultrasonic_budget
            .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            return Err("EcBeam2 ultrasonic budget must be finite and positive");
        }
        if self
            .signed_error_budget
            .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            return Err("EcBeam2 signed-error budget must be finite and positive");
        }
        if self
            .diagnostic_window
            .is_some_and(|window| !window.is_valid())
        {
            return Err("EcBeam2 diagnostic window must be a non-empty half-open range");
        }
        Ok(self)
    }
}

/// Path-consistent objective totals. State-terminal is already weighted; its
/// raw telescoping delta is reported separately by the exact oracle.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EcBeam2ObjectiveComponents {
    pub reconstruction: f64,
    pub state_terminal: f64,
    pub state_barrier: f64,
    pub quantizer_regularizer: f64,
}

impl EcBeam2ObjectiveComponents {
    #[inline]
    pub fn total(self) -> f64 {
        self.reconstruction + self.state_terminal + self.state_barrier + self.quantizer_regularizer
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EcBeam2ScaleDistribution {
    pub median: f64,
    pub p95: f64,
    pub p99: f64,
    pub maximum: f64,
}

/// Cumulative diagnostics for the actual committed EcBeam2 output path.
/// Candidate-expansion counters are intentionally kept separate from replayed
/// energy so provisional best-path switches cannot be mistaken for output.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EcBeam2Diagnostics {
    pub committed_samples: u64,
    pub positive_bits: u64,
    /// Samples and positive bits inside the configured diagnostic range. With
    /// no range configured these equal the cumulative committed counters.
    pub diagnostic_window_enabled: bool,
    pub diagnostic_window_start_sequence: u64,
    pub diagnostic_window_end_sequence: u64,
    pub diagnostic_window_samples: u64,
    pub diagnostic_window_positive_bits: u64,
    pub diagnostic_window_starting_tail_energy: f64,
    pub diagnostic_window_remaining_tail_energy: f64,
    /// ShadowA1-only production frontier count and objective disagreements.
    /// Active EcBeam2 leaves these at zero because it never reimplements or
    /// runs the production A1 chooser in parallel.
    pub a1_frontier_events: u64,
    pub a1_best_child_disagreements: u64,
    pub a1_top_m_disagreements: u64,
    pub a1_frontier_maximum_ultrasonic_ema: f64,
    pub a1_frontier_maximum_signed_error_ema: f64,
    pub a1_frontier_ultrasonic_ema_p999: f64,
    pub a1_frontier_ultrasonic_ema_p9999: f64,
    pub a1_frontier_signed_error_ema_abs_p999: f64,
    pub a1_frontier_signed_error_ema_abs_p9999: f64,
    pub path_switches: u64,
    pub pruned_total: u64,
    pub min_survivors: u64,
    pub constraint_escape: u64,
    pub state_repair_fallback: u64,
    pub first_constraint_escape_sequence: Option<u64>,
    pub first_state_repair_sequence: Option<u64>,
    pub last_constraint_escape_sequence: Option<u64>,
    pub last_state_repair_sequence: Option<u64>,
    pub maximum_state_overflow: f64,
    pub maximum_budget_violation: f64,
    pub maximum_consecutive_constraint_escapes: u64,
    pub maximum_consecutive_state_repairs: u64,
    pub ultrasonic_budget_escape_count: u64,
    pub signed_error_budget_escape_count: u64,
    pub both_budget_escape_count: u64,
    pub state_repair_stage_counts: [u64; 7],
    pub maximum_normalized_state_by_stage: [f64; 7],
    pub reconstruction_increment_scale: EcBeam2ScaleDistribution,
    pub state_terminal_delta_scale: EcBeam2ScaleDistribution,
    pub state_barrier_raw_scale: EcBeam2ScaleDistribution,
    pub quantizer_error_squared_scale: EcBeam2ScaleDistribution,
    pub all_nonfinite_resets: u64,
    /// ShadowA1-only frontier/input-ring alignment failures. Any nonzero value
    /// invalidates calibration because replay may have substituted an input.
    pub observer_desynchronizations: u64,
    /// Non-finite input samples whose emitted recovery bit is replayed with the
    /// declared diagnostic substitute `u = 0`.
    pub invalid_input_substitutions: u64,
    pub committed_output_energy: f64,
    pub committed_tail_adjusted_energy: f64,
    pub remaining_tail_energy: f64,
    pub maximum_tail_energy: f64,
    pub committed_ultrasonic_energy: f64,
    pub maximum_ultrasonic_power: f64,
    pub maximum_ultrasonic_ema: f64,
    pub maximum_signed_error_ema: f64,
    pub ultrasonic_ema_p999: f64,
    pub ultrasonic_ema_p9999: f64,
    pub signed_error_ema_abs_p999: f64,
    pub signed_error_ema_abs_p9999: f64,
    pub maximum_reconstruction_1ms_ema: f64,
    pub maximum_reconstruction_10ms_ema: f64,
    pub maximum_reconstruction_1ms_energy: f64,
    pub maximum_reconstruction_10ms_energy: f64,
    pub maximum_abs_reconstruction_output: f64,
    pub best_fourth_margin_last: f64,
    pub minimum_best_fourth_margin: f64,
    pub maximum_best_fourth_margin: f64,
    pub best_fourth_margin_samples: u64,
    pub a1_best_fourth_margin_last: f64,
    pub a1_minimum_best_fourth_margin: f64,
    pub a1_maximum_best_fourth_margin: f64,
    pub a1_best_fourth_margin_samples: u64,
    pub predicted_segments_recorded: u64,
    pub matched_complete_segments: u64,
    pub changed_before_commit_segments: u64,
    pub maximum_segment_identity_error: f64,
    pub output_length_events: u64,
    pub committed_sequence: u64,
    pub committed_state_epoch: u64,
}

/// Exact-search result for one frozen difficult window.
///
/// The oracle starts from the active engine's current best survivor and does
/// not mutate it.  `chosen_sequence` stores the first decision in its most
/// significant used bit, matching the packed-history tie-break used by the
/// runtime beam.
#[derive(Debug, Clone, PartialEq)]
pub struct EcBeam2ExactOracleReport {
    pub horizon: usize,
    pub complete_sequences: usize,
    pub chosen_first_bit: u8,
    pub chosen_sequence: u16,
    pub sequence_objective: f64,
    pub objective_components: EcBeam2ObjectiveComponents,
    pub reconstruction_objective: f64,
    pub starting_state_potential: f64,
    pub terminal_state_potential: f64,
    pub state_terminal_delta: f64,
    pub state_terminal_cost: f64,
    pub state_barrier_raw: f64,
    pub state_barrier_cost: f64,
    pub quantizer_error_energy: f64,
    pub quantizer_regularizer_cost: f64,
    pub total_objective: f64,
    pub starting_tail_energy: f64,
    pub causal_reconstruction_energy: f64,
    pub remaining_tail_energy: f64,
    pub tail_adjusted_energy: f64,
    pub causal_ultrasonic_energy: f64,
    pub maximum_state_overflow: f64,
    pub maximum_budget_violation: f64,
    pub state_feasible: bool,
    pub budgets_feasible: bool,
    pub constraint_escapes: u64,
    pub state_repairs: u64,
    pub reconstructed_output: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EcBeam2OracleComparison {
    pub m4n8_first_bit: u8,
    pub exact: EcBeam2ExactOracleReport,
    pub first_bit_disagrees: bool,
    pub prefix_constraint_escapes: u64,
    pub prefix_state_repairs: u64,
    pub prefix_all_nonfinite_resets: u64,
    pub prefix_invalid_input_substitutions: u64,
    pub prefix_output_length_events: u64,
}

/// Candidate-specific active-prefix state shared by all exact horizons for a
/// difficult window. Preparing this once prevents N8/N12/N16 from reaching
/// subtly different starting states and makes prefix health part of candidate
/// qualification rather than a reconstruction-only precondition.
#[derive(Debug, Clone)]
pub struct EcBeam2OracleSeed {
    wire_rate: u32,
    random_seed: u64,
    config: EcBeam2ExperimentConfig,
    start: EcBeam2Path,
    prefix_diagnostics: EcBeam2Diagnostics,
}

/// Advance the requested candidate through the complete active prefix once.
pub fn prepare_ecbeam2_oracle_seed(
    wire_rate: u32,
    seed: u64,
    prefix: &[f64],
    config: EcBeam2ExperimentConfig,
) -> Result<EcBeam2OracleSeed, &'static str> {
    let mut modulator = EcBeam2Modulator::new(&CRFB_OSR64_OBG165, seed, wire_rate, config)?;
    let mut discarded_commits = Vec::new();
    modulator.process_into_bits(prefix, &mut discarded_commits);
    Ok(EcBeam2OracleSeed {
        wire_rate,
        random_seed: seed,
        config,
        start: modulator.parents[modulator.parents_bank][0],
        prefix_diagnostics: modulator.diagnostics(),
    })
}

/// Run one horizon from a previously prepared, candidate-specific prefix.
pub fn run_ecbeam2_exact_oracle_from_seed(
    seed: &EcBeam2OracleSeed,
    window: &[f64],
) -> Result<EcBeam2OracleComparison, &'static str> {
    let mut exact_modulator = EcBeam2Modulator::new(
        &CRFB_OSR64_OBG165,
        seed.random_seed,
        seed.wire_rate,
        seed.config,
    )?;
    exact_modulator.reseed_for_oracle(seed.start);
    let exact = exact_modulator.exact_horizon_oracle(window)?;

    // M4/N8 and the exact search start from the identical collapsed CRFB,
    // profile, EMA, and previous-output state. Neither can revise the prefix.
    let mut beam_modulator = EcBeam2Modulator::new(
        &CRFB_OSR64_OBG165,
        seed.random_seed,
        seed.wire_rate,
        seed.config,
    )?;
    beam_modulator.reseed_for_oracle(seed.start);
    let mut beam_bits = Vec::new();
    beam_modulator.process_into_bits(window, &mut beam_bits);
    let m4n8_first_bit = *beam_bits
        .first()
        .ok_or("EcBeam2 oracle window did not materialize its first M4/N8 decision")?;
    Ok(EcBeam2OracleComparison {
        m4n8_first_bit,
        first_bit_disagrees: m4n8_first_bit != exact.chosen_first_bit,
        exact,
        prefix_constraint_escapes: seed.prefix_diagnostics.constraint_escape,
        prefix_state_repairs: seed.prefix_diagnostics.state_repair_fallback,
        prefix_all_nonfinite_resets: seed.prefix_diagnostics.all_nonfinite_resets,
        prefix_invalid_input_substitutions: seed.prefix_diagnostics.invalid_input_substitutions,
        prefix_output_length_events: seed.prefix_diagnostics.output_length_events,
    })
}

/// Run one exact N8/N12/N16 oracle from the same isolated M4/N8 state reached
/// by an already-normalized modulator-input prefix.
///
/// `prefix` and `window` are in the precise EcBeam2 `u` domain: post gain,
/// headroom, and limiter, with `v` represented as ±1. The helper uses the same
/// OBG1.65 DSD64 CRFB table as the active renderer, consumes the prefix without
/// flushing its delayed frontier, and leaves production EcBeam untouched.
pub fn run_ecbeam2_exact_oracle(
    wire_rate: u32,
    seed: u64,
    prefix: &[f64],
    window: &[f64],
    config: EcBeam2ExperimentConfig,
) -> Result<EcBeam2OracleComparison, &'static str> {
    let seed = prepare_ecbeam2_oracle_seed(wire_rate, seed, prefix, config)?;
    run_ecbeam2_exact_oracle_from_seed(&seed, window)
}

#[derive(Debug, Clone, Copy)]
struct EcBeam2Path {
    state: [f64; 8],
    reconstruction_state: [f64; MAX_BEAM_ERROR_PROFILE_STATES],
    ultrasonic_state: [f64; MAX_BEAM_ERROR_PROFILE_STATES],
    metric: f64,
    prev_v: f64,
    bits: u8,
    ultrasonic_ema: f64,
    signed_error_ema: f64,
}

impl EcBeam2Path {
    const INERT: Self = Self {
        state: [0.0; 8],
        reconstruction_state: [0.0; MAX_BEAM_ERROR_PROFILE_STATES],
        ultrasonic_state: [0.0; MAX_BEAM_ERROR_PROFILE_STATES],
        metric: 0.0,
        prev_v: 1.0,
        bits: 0,
        ultrasonic_ema: 0.0,
        signed_error_ema: 0.0,
    };
}

#[derive(Debug, Clone, Copy)]
struct Child {
    parent: u8,
    v: f64,
    bits: u8,
    metric: f64,
    maximum_state_overflow: f64,
    squared_state_overflow: f64,
    budget_violation: f64,
    ultrasonic_violation: f64,
    signed_error_violation: f64,
    state_feasible: bool,
    budgets_feasible: bool,
    ultrasonic_ema: f64,
    signed_error_ema: f64,
    base_norm: [f64; 8],
    reconstruction_increment: f64,
    state_terminal_delta: f64,
    state_barrier_raw: f64,
    quantizer_error_squared: f64,
}

#[derive(Debug, Clone, Copy)]
struct ExactPath {
    state: [f64; 8],
    reconstruction_state: [f64; MAX_BEAM_ERROR_PROFILE_STATES],
    ultrasonic_state: [f64; MAX_BEAM_ERROR_PROFILE_STATES],
    metric: f64,
    objective_components: EcBeam2ObjectiveComponents,
    state_terminal_delta: f64,
    state_barrier_raw: f64,
    quantizer_error_energy: f64,
    prev_v: f64,
    history: u16,
    causal_reconstruction_energy: f64,
    causal_ultrasonic_energy: f64,
    ultrasonic_ema: f64,
    signed_error_ema: f64,
    maximum_state_overflow: f64,
    maximum_budget_violation: f64,
    constraint_escapes: u64,
    state_repairs: u64,
    state_feasible: bool,
    budgets_feasible: bool,
    reconstructed_output: [f64; ECBEAM2_EXACT_MAX_HORIZON],
}

#[derive(Debug, Clone, Copy)]
struct ObjectiveScaleTracker {
    median: StreamingQuantile,
    p95: StreamingQuantile,
    p99: StreamingQuantile,
    maximum: f64,
}

impl ObjectiveScaleTracker {
    fn new() -> Self {
        Self {
            median: StreamingQuantile::new(0.5),
            p95: StreamingQuantile::new(0.95),
            p99: StreamingQuantile::new(0.99),
            maximum: 0.0,
        }
    }

    fn observe(&mut self, value: f64) {
        if !value.is_finite() {
            return;
        }
        let value = value.abs();
        self.median.observe(value);
        self.p95.observe(value);
        self.p99.observe(value);
        self.maximum = self.maximum.max(value);
    }

    fn snapshot(&self) -> EcBeam2ScaleDistribution {
        EcBeam2ScaleDistribution {
            median: self.median.estimate(),
            p95: self.p95.estimate(),
            p99: self.p99.estimate(),
            maximum: self.maximum,
        }
    }
}

#[inline]
fn normalized_state_potential(state: &[f64; 8]) -> f64 {
    state[..7].iter().map(|value| value * value).sum::<f64>() / 7.0
}

#[inline]
fn normalized_state_barrier(state: &[f64; 8], knee: f64) -> f64 {
    if knee >= 1.0 {
        return 0.0;
    }
    let inverse_span = 1.0 / (1.0 - knee);
    state[..7]
        .iter()
        .map(|value| ((value.abs() - knee).max(0.0) * inverse_span).powi(2))
        .sum::<f64>()
        / 7.0
}

#[derive(Debug, Clone, Copy)]
struct SegmentPrediction {
    epoch: u64,
    start_sequence: u64,
    length: u8,
    bits: u8,
    matched: bool,
    predicted_increment: f64,
    starting_output_energy: f64,
    starting_tail_energy: f64,
}

#[derive(Debug, Clone)]
pub(super) struct RollingEnergy {
    samples: Vec<f64>,
    index: usize,
    filled: usize,
    sum: f64,
    maximum: f64,
}

impl RollingEnergy {
    pub(super) fn new(window_samples: usize) -> Self {
        Self {
            samples: vec![0.0; window_samples.max(1)],
            index: 0,
            filled: 0,
            sum: 0.0,
            maximum: 0.0,
        }
    }

    pub(super) fn observe(&mut self, energy: f64) {
        let replaced = if self.filled == self.samples.len() {
            self.samples[self.index]
        } else {
            self.filled += 1;
            0.0
        };
        self.samples[self.index] = energy;
        self.index += 1;
        if self.index == self.samples.len() {
            self.index = 0;
        }
        self.sum += energy - replaced;
        self.maximum = self.maximum.max(self.sum);
    }

    pub(super) fn reset(&mut self) {
        self.samples.fill(0.0);
        self.index = 0;
        self.filled = 0;
        self.sum = 0.0;
        self.maximum = 0.0;
    }

    pub(super) fn maximum(&self) -> f64 {
        self.maximum
    }

    pub(super) fn current(&self) -> f64 {
        self.sum
    }
}

impl Child {
    const INERT: Self = Self {
        parent: 0,
        v: 1.0,
        bits: 0,
        metric: 0.0,
        maximum_state_overflow: 0.0,
        squared_state_overflow: 0.0,
        budget_violation: 0.0,
        ultrasonic_violation: 0.0,
        signed_error_violation: 0.0,
        state_feasible: false,
        budgets_feasible: false,
        ultrasonic_ema: 0.0,
        signed_error_ema: 0.0,
        base_norm: [0.0; 8],
        reconstruction_increment: 0.0,
        state_terminal_delta: 0.0,
        state_barrier_raw: 0.0,
        quantizer_error_squared: 0.0,
    };
}

/// Isolated, fixed M4/N8 tail-aware beam modulator.
pub(crate) struct EcBeam2Modulator {
    core: CrfbModulator,
    reconstruction_profile: BeamErrorProfile,
    ultrasonic_profile: BeamErrorProfile,
    config: EcBeam2ExperimentConfig,
    parents: [[EcBeam2Path; ECBEAM2_WIDTH]; 2],
    parents_bank: usize,
    parents_len: usize,
    buffered: usize,
    input_buffer: [f64; ECBEAM2_HORIZON],
    ema_beta_10ms: f64,
    ema_beta_1ms: f64,
    committed_reconstruction_state: [f64; MAX_BEAM_ERROR_PROFILE_STATES],
    committed_ultrasonic_state: [f64; MAX_BEAM_ERROR_PROFILE_STATES],
    committed_ultrasonic_ema: f64,
    committed_signed_error_ema: f64,
    committed_reconstruction_1ms_ema: f64,
    committed_reconstruction_10ms_ema: f64,
    reconstruction_1ms_energy: RollingEnergy,
    reconstruction_10ms_energy: RollingEnergy,
    ultrasonic_ema_p999: StreamingQuantile,
    ultrasonic_ema_p9999: StreamingQuantile,
    signed_error_ema_p999: StreamingQuantile,
    signed_error_ema_p9999: StreamingQuantile,
    reconstruction_increment_scale: ObjectiveScaleTracker,
    state_terminal_delta_scale: ObjectiveScaleTracker,
    state_barrier_raw_scale: ObjectiveScaleTracker,
    quantizer_error_squared_scale: ObjectiveScaleTracker,
    consecutive_constraint_escapes: u64,
    consecutive_state_repairs: u64,
    segment_predictions: [Option<SegmentPrediction>; ECBEAM2_HORIZON],
    pending_output_balance: usize,
    replayed_output_energy: f64,
    diagnostics: EcBeam2Diagnostics,
}

impl EcBeam2Modulator {
    pub(crate) fn new(
        coeffs: &'static ModulatorCoeffs,
        seed: u64,
        wire_rate: u32,
        config: EcBeam2ExperimentConfig,
    ) -> Result<Self, &'static str> {
        let config = config.validated()?;
        if !matches!(config.run_mode, EcBeam2RunMode::Active) {
            return Err("EcBeam2 ShadowA1 requires the production-frontier observer");
        }
        if !ecbeam2_v1_coefficients_match(coeffs) {
            return Err("EcBeam2 v1 requires the DSD64 OBG1.65 CRFB coefficient table");
        }
        let profiles = profiles_for_wire_rate(wire_rate)
            .map_err(|_| "EcBeam2 supports only 2.8224/3.072 MHz DSD64 wire rates")?;
        let mut core = CrfbModulator::new_with_mode(coeffs, seed, ModulatorMode::Ec)?;
        core.set_dither_scale(0.0);
        core.set_isi_penalty(0.0);
        let beta = |seconds: f64| (-1.0 / (wire_rate as f64 * seconds)).exp();
        let mut this = Self {
            core,
            reconstruction_profile: profiles.reconstruction,
            ultrasonic_profile: profiles.ultrasonic,
            config,
            parents: [[EcBeam2Path::INERT; ECBEAM2_WIDTH]; 2],
            parents_bank: 0,
            parents_len: 1,
            buffered: 0,
            input_buffer: [0.0; ECBEAM2_HORIZON],
            ema_beta_10ms: beta(ERROR_EMA_SECONDS),
            ema_beta_1ms: beta(0.001),
            committed_reconstruction_state: [0.0; MAX_BEAM_ERROR_PROFILE_STATES],
            committed_ultrasonic_state: [0.0; MAX_BEAM_ERROR_PROFILE_STATES],
            committed_ultrasonic_ema: 0.0,
            committed_signed_error_ema: 0.0,
            committed_reconstruction_1ms_ema: 0.0,
            committed_reconstruction_10ms_ema: 0.0,
            reconstruction_1ms_energy: RollingEnergy::new((wire_rate as usize + 500) / 1_000),
            reconstruction_10ms_energy: RollingEnergy::new(wire_rate as usize / 100),
            ultrasonic_ema_p999: StreamingQuantile::new(0.999),
            ultrasonic_ema_p9999: StreamingQuantile::new(0.9999),
            signed_error_ema_p999: StreamingQuantile::new(0.999),
            signed_error_ema_p9999: StreamingQuantile::new(0.9999),
            reconstruction_increment_scale: ObjectiveScaleTracker::new(),
            state_terminal_delta_scale: ObjectiveScaleTracker::new(),
            state_barrier_raw_scale: ObjectiveScaleTracker::new(),
            quantizer_error_squared_scale: ObjectiveScaleTracker::new(),
            consecutive_constraint_escapes: 0,
            consecutive_state_repairs: 0,
            segment_predictions: [None; ECBEAM2_HORIZON],
            pending_output_balance: 0,
            replayed_output_energy: 0.0,
            diagnostics: EcBeam2Diagnostics {
                min_survivors: ECBEAM2_WIDTH as u64,
                diagnostic_window_enabled: config.diagnostic_window.is_some(),
                diagnostic_window_start_sequence: config
                    .diagnostic_window
                    .map_or(0, |window| window.start_sequence),
                diagnostic_window_end_sequence: config
                    .diagnostic_window
                    .map_or(0, |window| window.end_sequence),
                ..EcBeam2Diagnostics::default()
            },
        };
        this.reseed_from_core();
        Ok(this)
    }

    pub(crate) fn process_into_bits(&mut self, input: &[f64], out_bits: &mut Vec<u8>) {
        out_bits.reserve(input.len());
        for &u in input {
            self.process_sample(u, out_bits);
        }
    }

    pub(crate) fn flush_into_bits(&mut self, out_bits: &mut Vec<u8>) {
        if self.buffered == 0 {
            return;
        }
        let best = self.parents[self.parents_bank][0];
        self.record_segment_prediction(best, self.buffered);
        for index in 0..self.buffered {
            let shift = self.buffered - 1 - index;
            let bit = (best.bits >> shift) & 1;
            self.commit_bit(bit, self.input_buffer[index], out_bits);
        }
        self.core.state = best.state;
        self.core.prev_v = best.prev_v;
        self.buffered = 0;
        self.reseed_from_core();
        if self.pending_output_balance != 0 {
            self.diagnostics.output_length_events =
                self.diagnostics.output_length_events.wrapping_add(1);
            self.pending_output_balance = 0;
        }
    }

    pub(crate) fn reset(&mut self) {
        if self.pending_output_balance != 0 {
            self.diagnostics.output_length_events =
                self.diagnostics.output_length_events.wrapping_add(1);
            self.pending_output_balance = 0;
        }
        self.core.reset();
        self.buffered = 0;
        self.input_buffer = [0.0; ECBEAM2_HORIZON];
        self.committed_reconstruction_state = [0.0; MAX_BEAM_ERROR_PROFILE_STATES];
        self.committed_ultrasonic_state = [0.0; MAX_BEAM_ERROR_PROFILE_STATES];
        self.committed_ultrasonic_ema = 0.0;
        self.committed_signed_error_ema = 0.0;
        self.committed_reconstruction_1ms_ema = 0.0;
        self.committed_reconstruction_10ms_ema = 0.0;
        self.reconstruction_1ms_energy.reset();
        self.reconstruction_10ms_energy.reset();
        self.ultrasonic_ema_p999 = StreamingQuantile::new(0.999);
        self.ultrasonic_ema_p9999 = StreamingQuantile::new(0.9999);
        self.signed_error_ema_p999 = StreamingQuantile::new(0.999);
        self.signed_error_ema_p9999 = StreamingQuantile::new(0.9999);
        self.reconstruction_increment_scale = ObjectiveScaleTracker::new();
        self.state_terminal_delta_scale = ObjectiveScaleTracker::new();
        self.state_barrier_raw_scale = ObjectiveScaleTracker::new();
        self.quantizer_error_squared_scale = ObjectiveScaleTracker::new();
        self.consecutive_constraint_escapes = 0;
        self.consecutive_state_repairs = 0;
        self.replayed_output_energy = 0.0;
        self.segment_predictions = [None; ECBEAM2_HORIZON];
        // An explicit stream reset starts a new profile/measurement epoch.
        // Health/fallback counters remain cumulative, but all quantities that
        // telescope against the now-zero profile state restart coherently.
        self.diagnostics.committed_samples = 0;
        self.diagnostics.positive_bits = 0;
        self.diagnostics.diagnostic_window_samples = 0;
        self.diagnostics.diagnostic_window_positive_bits = 0;
        self.diagnostics.diagnostic_window_starting_tail_energy = 0.0;
        self.diagnostics.diagnostic_window_remaining_tail_energy = 0.0;
        self.diagnostics.committed_output_energy = 0.0;
        self.diagnostics.committed_tail_adjusted_energy = 0.0;
        self.diagnostics.remaining_tail_energy = 0.0;
        self.diagnostics.maximum_tail_energy = 0.0;
        self.diagnostics.committed_ultrasonic_energy = 0.0;
        self.diagnostics.maximum_ultrasonic_power = 0.0;
        self.diagnostics.maximum_ultrasonic_ema = 0.0;
        self.diagnostics.maximum_signed_error_ema = 0.0;
        self.diagnostics.ultrasonic_ema_p999 = 0.0;
        self.diagnostics.ultrasonic_ema_p9999 = 0.0;
        self.diagnostics.signed_error_ema_abs_p999 = 0.0;
        self.diagnostics.signed_error_ema_abs_p9999 = 0.0;
        self.diagnostics.maximum_reconstruction_1ms_ema = 0.0;
        self.diagnostics.maximum_reconstruction_10ms_ema = 0.0;
        self.diagnostics.maximum_reconstruction_1ms_energy = 0.0;
        self.diagnostics.maximum_reconstruction_10ms_energy = 0.0;
        self.diagnostics.maximum_abs_reconstruction_output = 0.0;
        self.diagnostics.best_fourth_margin_last = 0.0;
        self.diagnostics.minimum_best_fourth_margin = 0.0;
        self.diagnostics.maximum_best_fourth_margin = 0.0;
        self.diagnostics.best_fourth_margin_samples = 0;
        self.diagnostics.reconstruction_increment_scale = EcBeam2ScaleDistribution::default();
        self.diagnostics.state_terminal_delta_scale = EcBeam2ScaleDistribution::default();
        self.diagnostics.state_barrier_raw_scale = EcBeam2ScaleDistribution::default();
        self.diagnostics.quantizer_error_squared_scale = EcBeam2ScaleDistribution::default();
        self.diagnostics.a1_best_fourth_margin_last = 0.0;
        self.diagnostics.a1_minimum_best_fourth_margin = 0.0;
        self.diagnostics.a1_maximum_best_fourth_margin = 0.0;
        self.diagnostics.a1_best_fourth_margin_samples = 0;
        self.diagnostics.predicted_segments_recorded = 0;
        self.diagnostics.matched_complete_segments = 0;
        self.diagnostics.changed_before_commit_segments = 0;
        self.diagnostics.maximum_segment_identity_error = 0.0;
        self.diagnostics.committed_sequence = 0;
        self.diagnostics.committed_state_epoch =
            self.diagnostics.committed_state_epoch.wrapping_add(1);
        self.reseed_from_core();
    }

    pub(crate) fn stability_resets(&self) -> u64 {
        self.core.stability_resets()
    }

    pub(crate) fn state_clamps(&self) -> u64 {
        self.core.state_clamps()
    }

    pub(crate) fn diagnostics(&self) -> EcBeam2Diagnostics {
        EcBeam2Diagnostics {
            reconstruction_increment_scale: self.reconstruction_increment_scale.snapshot(),
            state_terminal_delta_scale: self.state_terminal_delta_scale.snapshot(),
            state_barrier_raw_scale: self.state_barrier_raw_scale.snapshot(),
            quantizer_error_squared_scale: self.quantizer_error_squared_scale.snapshot(),
            ..self.diagnostics
        }
    }

    #[inline]
    fn diagnostic_sequence_selected(&self, sequence: u64) -> bool {
        self.config
            .diagnostic_window
            .is_none_or(|window| window.contains(sequence))
    }

    /// Exhaustively search a frozen N8/N12/N16 input window from the current
    /// best survivor.  This is deliberately separate from the M4 runtime
    /// search: it is a quality oracle and never participates in rendering.
    ///
    /// Feasibility is resolved at each depth using the same hierarchy as the
    /// active engine.  A budget-only escape or hard-limit repair therefore
    /// collapses that exact frontier to the deterministic least-violating
    /// path, exactly as the renderer would at that sample.
    pub(crate) fn exact_horizon_oracle(
        &self,
        input: &[f64],
    ) -> Result<EcBeam2ExactOracleReport, &'static str> {
        if !matches!(input.len(), 8 | 12 | 16) {
            return Err("EcBeam2 exact oracle supports only N8, N12, or N16");
        }
        if input.iter().any(|sample| !sample.is_finite()) {
            return Err("EcBeam2 exact oracle requires finite input samples");
        }

        let start = self.parents[self.parents_bank][0];
        let starting_state_potential =
            normalized_state_potential(&mul8(&start.state, &self.core.inverse_state_limit));
        let mut paths = vec![ExactPath {
            state: start.state,
            reconstruction_state: start.reconstruction_state,
            ultrasonic_state: start.ultrasonic_state,
            metric: 0.0,
            objective_components: EcBeam2ObjectiveComponents::default(),
            state_terminal_delta: 0.0,
            state_barrier_raw: 0.0,
            quantizer_error_energy: 0.0,
            prev_v: start.prev_v,
            history: 0,
            causal_reconstruction_energy: 0.0,
            causal_ultrasonic_energy: 0.0,
            ultrasonic_ema: start.ultrasonic_ema,
            signed_error_ema: start.signed_error_ema,
            maximum_state_overflow: 0.0,
            maximum_budget_violation: 0.0,
            constraint_escapes: 0,
            state_repairs: 0,
            state_feasible: true,
            budgets_feasible: true,
            reconstructed_output: [0.0; ECBEAM2_EXACT_MAX_HORIZON],
        }];
        let one_minus_beta = 1.0 - self.ema_beta_10ms;

        for (depth, &u) in input.iter().enumerate() {
            let mut expanded = Vec::with_capacity(paths.len() * 2);
            for parent in &paths {
                let state_norm = mul8(&parent.state, &self.core.inverse_state_limit);
                let parent_state_potential = normalized_state_potential(&state_norm);
                let base_norm = if self.core.crfb_sparse {
                    self.core.predict_base_norm::<true>(&state_norm, u)
                } else {
                    self.core.predict_base_norm::<false>(&state_norm, u)
                };
                let y = if self.core.crfb_sparse {
                    self.core.loop_output_norm::<true>(&state_norm, u)
                } else {
                    self.core.loop_output_norm::<false>(&state_norm, u)
                };

                for v in [1.0, -1.0] {
                    let e = v - u;
                    let reconstruction_output = self
                        .reconstruction_profile
                        .output(&parent.reconstruction_state, e);
                    let reconstruction_delta = self
                        .reconstruction_profile
                        .tail_adjusted_energy_increment(&parent.reconstruction_state, e);
                    let reconstruction_state = self
                        .reconstruction_profile
                        .next_state(&parent.reconstruction_state, e);
                    let ultrasonic_output =
                        self.ultrasonic_profile.output(&parent.ultrasonic_state, e);
                    let ultrasonic_power = ultrasonic_output * ultrasonic_output;
                    let ultrasonic_state = self
                        .ultrasonic_profile
                        .next_state(&parent.ultrasonic_state, e);
                    let ultrasonic_ema = self
                        .ema_beta_10ms
                        .mul_add(parent.ultrasonic_ema, one_minus_beta * ultrasonic_power);
                    let signed_error_ema = self
                        .ema_beta_10ms
                        .mul_add(parent.signed_error_ema, one_minus_beta * e);

                    let mut next_state = denormalized_feedback8(
                        &base_norm,
                        &self.core.state_limit8,
                        &self.core.bv,
                        v,
                    );
                    let mut maximum_state_overflow = 0.0f64;
                    let mut squared_state_overflow = 0.0f64;
                    let mut finite = reconstruction_delta.is_finite()
                        && reconstruction_output.is_finite()
                        && ultrasonic_ema.is_finite()
                        && signed_error_ema.is_finite();
                    let mut state_probe = 0.0;
                    for stage in 0..7 {
                        let normalized = v.mul_add(self.core.bv_norm[stage], base_norm[stage]);
                        state_probe += next_state[stage];
                        finite &= normalized.is_finite() && next_state[stage].is_finite();
                        let overflow = (normalized.abs() - 1.0).max(0.0);
                        maximum_state_overflow = maximum_state_overflow.max(overflow);
                        squared_state_overflow += overflow * overflow;
                    }
                    finite &= state_probe.is_finite();
                    if !finite {
                        continue;
                    }
                    let ultrasonic_violation = self
                        .config
                        .ultrasonic_budget
                        .map(|budget| (ultrasonic_ema / budget - 1.0).max(0.0))
                        .unwrap_or(0.0);
                    let signed_violation = self
                        .config
                        .signed_error_budget
                        .map(|budget| (signed_error_ema.abs() / budget - 1.0).max(0.0))
                        .unwrap_or(0.0);
                    let budget_violation = ultrasonic_violation.max(signed_violation);
                    let state_feasible = maximum_state_overflow == 0.0;
                    if !state_feasible {
                        match stabilize_state(
                            &mut next_state,
                            &self.core.coeffs.state_limit,
                            &self.core.inverse_state_limit,
                        ) {
                            StateStability::Ok { .. } => {}
                            StateStability::Reset => next_state = [0.0; 8],
                        }
                    }
                    let next_state_norm = mul8(&next_state, &self.core.inverse_state_limit);
                    let state_terminal_delta =
                        normalized_state_potential(&next_state_norm) - parent_state_potential;
                    let state_barrier_raw =
                        normalized_state_barrier(&next_state_norm, self.config.state_deadzone);
                    let quantizer_error_squared = (y - v).powi(2);
                    let objective_components = EcBeam2ObjectiveComponents {
                        reconstruction: parent.objective_components.reconstruction
                            + reconstruction_delta,
                        state_terminal: parent.objective_components.state_terminal
                            + self.config.state_terminal_weight * state_terminal_delta,
                        state_barrier: parent.objective_components.state_barrier
                            + self.config.state_deadzone_weight * state_barrier_raw,
                        quantizer_regularizer: parent.objective_components.quantizer_regularizer
                            + self.config.quantizer_regularizer * quantizer_error_squared,
                    };
                    let metric = objective_components.total();
                    if !metric.is_finite() {
                        continue;
                    }
                    let mut reconstructed_output = parent.reconstructed_output;
                    reconstructed_output[depth] = reconstruction_output;
                    expanded.push((
                        ExactPath {
                            state: next_state,
                            reconstruction_state,
                            ultrasonic_state,
                            metric,
                            objective_components,
                            state_terminal_delta: parent.state_terminal_delta
                                + state_terminal_delta,
                            state_barrier_raw: parent.state_barrier_raw + state_barrier_raw,
                            quantizer_error_energy: parent.quantizer_error_energy
                                + quantizer_error_squared,
                            prev_v: v,
                            history: (parent.history << 1) | u16::from(v > 0.0),
                            causal_reconstruction_energy: parent.causal_reconstruction_energy
                                + reconstruction_output * reconstruction_output,
                            causal_ultrasonic_energy: parent.causal_ultrasonic_energy
                                + ultrasonic_power,
                            ultrasonic_ema,
                            signed_error_ema,
                            maximum_state_overflow: parent
                                .maximum_state_overflow
                                .max(maximum_state_overflow),
                            maximum_budget_violation: parent
                                .maximum_budget_violation
                                .max(budget_violation),
                            constraint_escapes: parent.constraint_escapes,
                            state_repairs: parent.state_repairs,
                            state_feasible,
                            budgets_feasible: budget_violation == 0.0,
                            reconstructed_output,
                        },
                        maximum_state_overflow,
                        squared_state_overflow,
                        budget_violation,
                    ));
                }
            }
            if expanded.is_empty() {
                return Err("EcBeam2 exact oracle found no finite child");
            }

            let has_fully_feasible = expanded
                .iter()
                .any(|(path, _, _, _)| path.state_feasible && path.budgets_feasible);
            let has_state_feasible = expanded.iter().any(|(path, _, _, _)| path.state_feasible);
            if has_fully_feasible {
                expanded.retain(|(path, _, _, _)| path.state_feasible && path.budgets_feasible);
                paths = expanded.into_iter().map(|(path, _, _, _)| path).collect();
            } else {
                if has_state_feasible {
                    expanded.retain(|(path, _, _, _)| path.state_feasible);
                }
                expanded.sort_by(|left, right| {
                    let left_path = &left.0;
                    let right_path = &right.0;
                    let left_key = if has_state_feasible {
                        (0.0, 0.0, left.3)
                    } else {
                        (left.1, left.2, left.3)
                    };
                    let right_key = if has_state_feasible {
                        (0.0, 0.0, right.3)
                    } else {
                        (right.1, right.2, right.3)
                    };
                    left_key
                        .partial_cmp(&right_key)
                        .unwrap_or(core::cmp::Ordering::Equal)
                        .then_with(|| {
                            left_path
                                .metric
                                .partial_cmp(&right_path.metric)
                                .unwrap_or(core::cmp::Ordering::Equal)
                        })
                        .then_with(|| right_path.history.cmp(&left_path.history))
                });
                let mut winner = expanded.swap_remove(0).0;
                if has_state_feasible {
                    winner.constraint_escapes = winner.constraint_escapes.wrapping_add(1);
                } else {
                    winner.state_repairs = winner.state_repairs.wrapping_add(1);
                }
                paths = vec![winner];
            }
        }

        paths.sort_by(|left, right| {
            left.metric
                .partial_cmp(&right.metric)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then_with(|| right.history.cmp(&left.history))
        });
        let complete_sequences = paths.len();
        let winner = paths.into_iter().next().expect("non-empty exact frontier");
        let starting_tail_energy = self
            .reconstruction_profile
            .remaining_zero_input_energy(&start.reconstruction_state);
        let tail = self
            .reconstruction_profile
            .remaining_zero_input_energy(&winner.reconstruction_state);
        let terminal_state_potential =
            normalized_state_potential(&mul8(&winner.state, &self.core.inverse_state_limit));
        let chosen_first_bit = ((winner.history >> (input.len() - 1)) & 1) as u8;
        Ok(EcBeam2ExactOracleReport {
            horizon: input.len(),
            complete_sequences,
            chosen_first_bit,
            chosen_sequence: winner.history,
            sequence_objective: winner.metric,
            objective_components: winner.objective_components,
            reconstruction_objective: winner.objective_components.reconstruction,
            starting_state_potential,
            terminal_state_potential,
            state_terminal_delta: winner.state_terminal_delta,
            state_terminal_cost: winner.objective_components.state_terminal,
            state_barrier_raw: winner.state_barrier_raw,
            state_barrier_cost: winner.objective_components.state_barrier,
            quantizer_error_energy: winner.quantizer_error_energy,
            quantizer_regularizer_cost: winner.objective_components.quantizer_regularizer,
            total_objective: winner.objective_components.total(),
            starting_tail_energy,
            causal_reconstruction_energy: winner.causal_reconstruction_energy,
            remaining_tail_energy: tail,
            tail_adjusted_energy: winner.causal_reconstruction_energy + tail - starting_tail_energy,
            causal_ultrasonic_energy: winner.causal_ultrasonic_energy,
            maximum_state_overflow: winner.maximum_state_overflow,
            maximum_budget_violation: winner.maximum_budget_violation,
            state_feasible: winner.maximum_state_overflow == 0.0 && winner.state_repairs == 0,
            budgets_feasible: winner.maximum_budget_violation == 0.0
                && winner.constraint_escapes == 0,
            constraint_escapes: winner.constraint_escapes,
            state_repairs: winner.state_repairs,
            reconstructed_output: winner.reconstructed_output[..input.len()].to_vec(),
        })
    }

    fn reseed_from_core(&mut self) {
        self.parents_bank = 0;
        self.parents_len = 1;
        self.parents[0][0] = EcBeam2Path {
            state: self.core.state,
            reconstruction_state: self.committed_reconstruction_state,
            ultrasonic_state: self.committed_ultrasonic_state,
            prev_v: self.core.prev_v,
            ultrasonic_ema: self.committed_ultrasonic_ema,
            signed_error_ema: self.committed_signed_error_ema,
            ..EcBeam2Path::INERT
        };
    }

    /// Collapse a fresh engine onto one frozen path so M4/N8 and an exact
    /// horizon comparison begin from precisely the same CRFB, profile, EMA,
    /// and previous-output state. This is oracle tooling only.
    fn reseed_for_oracle(&mut self, start: EcBeam2Path) {
        self.core.state = start.state;
        self.core.prev_v = start.prev_v;
        self.committed_reconstruction_state = start.reconstruction_state;
        self.committed_ultrasonic_state = start.ultrasonic_state;
        self.committed_ultrasonic_ema = start.ultrasonic_ema;
        self.committed_signed_error_ema = start.signed_error_ema;
        self.diagnostics.remaining_tail_energy = self
            .reconstruction_profile
            .remaining_zero_input_energy(&start.reconstruction_state);
        self.parents = [[EcBeam2Path::INERT; ECBEAM2_WIDTH]; 2];
        self.parents_bank = 0;
        self.parents_len = 1;
        self.parents[0][0] = EcBeam2Path {
            metric: 0.0,
            bits: 0,
            ..start
        };
        self.buffered = 0;
        self.input_buffer = [0.0; ECBEAM2_HORIZON];
        self.pending_output_balance = 0;
        self.segment_predictions = [None; ECBEAM2_HORIZON];
    }

    fn process_sample(&mut self, u: f64, out_bits: &mut Vec<u8>) {
        self.pending_output_balance = self.pending_output_balance.saturating_add(1);
        if !u.is_finite() {
            self.emit_buffered_best(out_bits);
            self.core.hard_reset();
            self.core.stability_resets = self.core.stability_resets.wrapping_add(1);
            self.diagnostics.all_nonfinite_resets =
                self.diagnostics.all_nonfinite_resets.wrapping_add(1);
            self.diagnostics.invalid_input_substitutions =
                self.diagnostics.invalid_input_substitutions.wrapping_add(1);
            // The physical reconstruction/ultrasonic filters do not reset when
            // the CRFB safety state does. Replay the actual fixed recovery bit
            // using the declared finite substitute for an invalid input.
            self.commit_bit(1, 0.0, out_bits);
            self.reseed_from_core();
            return;
        }

        let parent_bank = self.parents_bank;
        let mut children = [Child::INERT; ECBEAM2_CHILDREN];
        let mut child_count = 0usize;
        let one_minus_beta = 1.0 - self.ema_beta_10ms;
        for parent_index in 0..self.parents_len {
            let parent = self.parents[parent_bank][parent_index];
            let state_norm = mul8(&parent.state, &self.core.inverse_state_limit);
            let parent_state_potential = normalized_state_potential(&state_norm);
            let base_norm = if self.core.crfb_sparse {
                self.core.predict_base_norm::<true>(&state_norm, u)
            } else {
                self.core.predict_base_norm::<false>(&state_norm, u)
            };
            let y = if self.core.crfb_sparse {
                self.core.loop_output_norm::<true>(&state_norm, u)
            } else {
                self.core.loop_output_norm::<false>(&state_norm, u)
            };
            for v in [1.0, -1.0] {
                let e = v - u;
                let mut candidate_state =
                    denormalized_feedback8(&base_norm, &self.core.state_limit8, &self.core.bv, v);
                let reconstruction_delta = self
                    .reconstruction_profile
                    .tail_adjusted_energy_increment(&parent.reconstruction_state, e);
                let ultrasonic_output = self.ultrasonic_profile.output(&parent.ultrasonic_state, e);
                let ultrasonic_power = ultrasonic_output * ultrasonic_output;
                let ultrasonic_ema = self
                    .ema_beta_10ms
                    .mul_add(parent.ultrasonic_ema, one_minus_beta * ultrasonic_power);
                let signed_error_ema = self
                    .ema_beta_10ms
                    .mul_add(parent.signed_error_ema, one_minus_beta * e);

                let mut maximum_state_overflow = 0.0f64;
                let mut squared_state_overflow = 0.0f64;
                let mut finite = reconstruction_delta.is_finite()
                    && ultrasonic_ema.is_finite()
                    && signed_error_ema.is_finite();
                let mut state_probe = 0.0;
                for stage in 0..7 {
                    let state = v.mul_add(self.core.bv_norm[stage], base_norm[stage]);
                    state_probe += candidate_state[stage];
                    finite &= state.is_finite() && candidate_state[stage].is_finite();
                    let overflow = (state.abs() - 1.0).max(0.0);
                    maximum_state_overflow = maximum_state_overflow.max(overflow);
                    squared_state_overflow += overflow * overflow;
                }
                finite &= state_probe.is_finite();
                if !finite {
                    continue;
                }
                let ultrasonic_violation = self
                    .config
                    .ultrasonic_budget
                    .map(|budget| (ultrasonic_ema / budget - 1.0).max(0.0))
                    .unwrap_or(0.0);
                let signed_violation = self
                    .config
                    .signed_error_budget
                    .map(|budget| (signed_error_ema.abs() / budget - 1.0).max(0.0))
                    .unwrap_or(0.0);
                let budget_violation = ultrasonic_violation.max(signed_violation);
                let state_feasible = maximum_state_overflow == 0.0;
                if !state_feasible {
                    match stabilize_state(
                        &mut candidate_state,
                        &self.core.coeffs.state_limit,
                        &self.core.inverse_state_limit,
                    ) {
                        StateStability::Ok { .. } => {}
                        StateStability::Reset => candidate_state = [0.0; 8],
                    }
                }
                let effective_state_norm = mul8(&candidate_state, &self.core.inverse_state_limit);
                let state_terminal_delta =
                    normalized_state_potential(&effective_state_norm) - parent_state_potential;
                let state_barrier_raw =
                    normalized_state_barrier(&effective_state_norm, self.config.state_deadzone);
                let quantizer_error_squared = (y - v).powi(2);
                let metric = parent.metric
                    + reconstruction_delta
                    + self.config.state_terminal_weight * state_terminal_delta
                    + self.config.state_deadzone_weight * state_barrier_raw
                    + self.config.quantizer_regularizer * quantizer_error_squared;
                if !metric.is_finite() {
                    continue;
                }
                children[child_count] = Child {
                    parent: parent_index as u8,
                    v,
                    bits: (parent.bits << 1) | u8::from(v > 0.0),
                    metric,
                    maximum_state_overflow,
                    squared_state_overflow,
                    budget_violation,
                    ultrasonic_violation,
                    signed_error_violation: signed_violation,
                    state_feasible,
                    budgets_feasible: budget_violation == 0.0,
                    ultrasonic_ema,
                    signed_error_ema,
                    base_norm,
                    reconstruction_increment: reconstruction_delta,
                    state_terminal_delta,
                    state_barrier_raw,
                    quantizer_error_squared,
                };
                child_count += 1;
            }
        }

        if child_count == 0 {
            self.emit_buffered_best(out_bits);
            self.core.hard_reset();
            self.core.stability_resets = self.core.stability_resets.wrapping_add(1);
            self.diagnostics.all_nonfinite_resets =
                self.diagnostics.all_nonfinite_resets.wrapping_add(1);
            // `u` is finite here: replay the recovery bit against the real
            // sample so committed profile state remains the emitted stream.
            self.commit_bit(1, u, out_bits);
            self.reseed_from_core();
            return;
        }

        let has_fully_feasible = children[..child_count]
            .iter()
            .any(|child| child.state_feasible && child.budgets_feasible);
        let has_state_feasible = children[..child_count]
            .iter()
            .any(|child| child.state_feasible);
        let mut order = [0u8; ECBEAM2_CHILDREN];
        let mut order_len = 0usize;
        for (index, child) in children[..child_count].iter().copied().enumerate() {
            let eligible = if has_fully_feasible {
                child.state_feasible && child.budgets_feasible
            } else if has_state_feasible {
                child.state_feasible
            } else {
                true
            };
            if eligible {
                order[order_len] = index as u8;
                order_len += 1;
            }
        }
        self.sort_children(&children, &mut order[..order_len], has_fully_feasible);
        let frontier_sequence = self
            .diagnostics
            .committed_sequence
            .wrapping_add(self.buffered as u64);
        if order_len >= ECBEAM2_WIDTH && self.diagnostic_sequence_selected(frontier_sequence) {
            let best_metric = children[order[0] as usize].metric;
            let fourth_metric = children[order[ECBEAM2_WIDTH - 1] as usize].metric;
            let margin = (fourth_metric - best_metric).max(0.0);
            self.diagnostics.best_fourth_margin_last = margin;
            self.diagnostics.minimum_best_fourth_margin =
                if self.diagnostics.best_fourth_margin_samples == 0 {
                    margin
                } else {
                    self.diagnostics.minimum_best_fourth_margin.min(margin)
                };
            self.diagnostics.maximum_best_fourth_margin =
                self.diagnostics.maximum_best_fourth_margin.max(margin);
            self.diagnostics.best_fourth_margin_samples =
                self.diagnostics.best_fourth_margin_samples.wrapping_add(1);
        }

        let fallback = !has_fully_feasible;
        let selected = children[order[0] as usize];
        self.reconstruction_increment_scale
            .observe(selected.reconstruction_increment);
        self.state_terminal_delta_scale
            .observe(selected.state_terminal_delta);
        self.state_barrier_raw_scale
            .observe(selected.state_barrier_raw);
        self.quantizer_error_squared_scale
            .observe(selected.quantizer_error_squared);
        self.diagnostics.maximum_state_overflow = self
            .diagnostics
            .maximum_state_overflow
            .max(selected.maximum_state_overflow);
        self.diagnostics.maximum_budget_violation = self
            .diagnostics
            .maximum_budget_violation
            .max(selected.budget_violation);
        for stage in 0..7 {
            let normalized = selected
                .v
                .mul_add(self.core.bv_norm[stage], selected.base_norm[stage]);
            self.diagnostics.maximum_normalized_state_by_stage[stage] =
                self.diagnostics.maximum_normalized_state_by_stage[stage].max(normalized.abs());
        }
        if fallback {
            order_len = order_len.min(1);
            if has_state_feasible {
                self.diagnostics.constraint_escape =
                    self.diagnostics.constraint_escape.wrapping_add(1);
                self.diagnostics.first_constraint_escape_sequence = self
                    .diagnostics
                    .first_constraint_escape_sequence
                    .or(Some(frontier_sequence));
                self.diagnostics.last_constraint_escape_sequence = Some(frontier_sequence);
                self.consecutive_constraint_escapes =
                    self.consecutive_constraint_escapes.wrapping_add(1);
                self.consecutive_state_repairs = 0;
                self.diagnostics.maximum_consecutive_constraint_escapes = self
                    .diagnostics
                    .maximum_consecutive_constraint_escapes
                    .max(self.consecutive_constraint_escapes);
                let ultrasonic_failed = selected.ultrasonic_violation > 0.0;
                let signed_failed = selected.signed_error_violation > 0.0;
                match (ultrasonic_failed, signed_failed) {
                    (true, true) => {
                        self.diagnostics.both_budget_escape_count =
                            self.diagnostics.both_budget_escape_count.wrapping_add(1);
                    }
                    (true, false) => {
                        self.diagnostics.ultrasonic_budget_escape_count = self
                            .diagnostics
                            .ultrasonic_budget_escape_count
                            .wrapping_add(1);
                    }
                    (false, true) => {
                        self.diagnostics.signed_error_budget_escape_count = self
                            .diagnostics
                            .signed_error_budget_escape_count
                            .wrapping_add(1);
                    }
                    (false, false) => {}
                }
            } else {
                self.diagnostics.state_repair_fallback =
                    self.diagnostics.state_repair_fallback.wrapping_add(1);
                self.diagnostics.first_state_repair_sequence = self
                    .diagnostics
                    .first_state_repair_sequence
                    .or(Some(frontier_sequence));
                self.diagnostics.last_state_repair_sequence = Some(frontier_sequence);
                self.consecutive_state_repairs = self.consecutive_state_repairs.wrapping_add(1);
                self.consecutive_constraint_escapes = 0;
                self.diagnostics.maximum_consecutive_state_repairs = self
                    .diagnostics
                    .maximum_consecutive_state_repairs
                    .max(self.consecutive_state_repairs);
                for stage in 0..7 {
                    let normalized = selected
                        .v
                        .mul_add(self.core.bv_norm[stage], selected.base_norm[stage]);
                    if normalized.abs() > 1.0 {
                        self.diagnostics.state_repair_stage_counts[stage] =
                            self.diagnostics.state_repair_stage_counts[stage].wrapping_add(1);
                    }
                }
            }
        } else {
            self.consecutive_constraint_escapes = 0;
            self.consecutive_state_repairs = 0;
        }

        let materialize_bank = parent_bank ^ 1;
        let keep = order_len.min(ECBEAM2_WIDTH);
        for slot in 0..keep {
            let child = children[order[slot] as usize];
            let parent = self.parents[parent_bank][child.parent as usize];
            let e = child.v - u;
            let mut state = denormalized_feedback8(
                &child.base_norm,
                &self.core.state_limit8,
                &self.core.bv,
                child.v,
            );
            if !child.state_feasible {
                match stabilize_state(
                    &mut state,
                    &self.core.coeffs.state_limit,
                    &self.core.inverse_state_limit,
                ) {
                    StateStability::Ok { clamped } => {
                        self.core.state_clamps =
                            self.core.state_clamps.wrapping_add(u64::from(clamped));
                    }
                    StateStability::Reset => {
                        self.core.hard_reset();
                        state = self.core.state;
                        self.core.stability_resets = self.core.stability_resets.wrapping_add(1);
                    }
                }
            }
            self.parents[materialize_bank][slot] = EcBeam2Path {
                state,
                reconstruction_state: self
                    .reconstruction_profile
                    .next_state(&parent.reconstruction_state, e),
                ultrasonic_state: self
                    .ultrasonic_profile
                    .next_state(&parent.ultrasonic_state, e),
                metric: child.metric,
                prev_v: child.v,
                bits: child.bits,
                ultrasonic_ema: child.ultrasonic_ema,
                signed_error_ema: child.signed_error_ema,
            };
        }
        self.diagnostics.path_switches = self
            .diagnostics
            .path_switches
            .wrapping_add(u64::from(children[order[0] as usize].parent != 0));
        self.parents_bank = materialize_bank;
        self.parents_len = keep;
        self.input_buffer[self.buffered] = u;
        self.buffered += 1;

        if self.buffered == ECBEAM2_HORIZON {
            let best = self.parents[self.parents_bank][0];
            self.record_segment_prediction(best, ECBEAM2_HORIZON);
            self.commit_oldest(out_bits);
        }
        self.diagnostics.min_survivors =
            self.diagnostics.min_survivors.min(self.parents_len as u64);
        self.renormalize_metrics();
    }

    fn sort_children(
        &self,
        children: &[Child; ECBEAM2_CHILDREN],
        order: &mut [u8],
        feasible: bool,
    ) {
        for sorted in 1..order.len() {
            let key = order[sorted];
            let mut position = sorted;
            while position > 0
                && self.child_before(
                    children[key as usize],
                    children[order[position - 1] as usize],
                    feasible,
                )
            {
                order[position] = order[position - 1];
                position -= 1;
            }
            order[position] = key;
        }
    }

    fn child_before(&self, left: Child, right: Child, feasible: bool) -> bool {
        if !feasible {
            let left_key = (
                left.maximum_state_overflow,
                left.squared_state_overflow,
                left.budget_violation,
            );
            let right_key = (
                right.maximum_state_overflow,
                right.squared_state_overflow,
                right.budget_violation,
            );
            if left_key != right_key {
                return left_key < right_key;
            }
        }
        left.metric < right.metric || (left.metric == right.metric && left.bits > right.bits)
    }

    fn commit_oldest(&mut self, out_bits: &mut Vec<u8>) {
        let best = self.parents[self.parents_bank][0];
        let bit = (best.bits >> (ECBEAM2_HORIZON - 1)) & 1;
        let input = self.input_buffer[0];
        self.commit_bit(bit, input, out_bits);

        let parent_bank = self.parents_bank;
        let compact_bank = parent_bank ^ 1;
        let mut kept = 0usize;
        for index in 0..self.parents_len {
            let parent = self.parents[parent_bank][index];
            if ((parent.bits >> (ECBEAM2_HORIZON - 1)) & 1) == bit {
                self.parents[compact_bank][kept] = parent;
                kept += 1;
            } else {
                self.diagnostics.pruned_total = self.diagnostics.pruned_total.wrapping_add(1);
            }
        }
        self.parents_bank = compact_bank;
        self.parents_len = kept;
        self.buffered -= 1;
        self.input_buffer.copy_within(1..ECBEAM2_HORIZON, 0);
        self.input_buffer[self.buffered] = 0.0;
    }

    fn commit_bit(&mut self, bit: u8, u: f64, out_bits: &mut Vec<u8>) {
        let sequence = self.diagnostics.committed_sequence;
        let measure = self.diagnostic_sequence_selected(sequence);
        let v = if bit == 1 { 1.0 } else { -1.0 };
        let e = v - u;
        let starting_tail = self
            .reconstruction_profile
            .remaining_zero_input_energy(&self.committed_reconstruction_state);
        let output = self
            .reconstruction_profile
            .output(&self.committed_reconstruction_state, e);
        let instantaneous = output * output;
        let increment = self
            .reconstruction_profile
            .tail_adjusted_energy_increment(&self.committed_reconstruction_state, e);
        self.reconstruction_profile
            .advance(&mut self.committed_reconstruction_state, e);
        let tail = self
            .reconstruction_profile
            .remaining_zero_input_energy(&self.committed_reconstruction_state);
        let ultrasonic = self
            .ultrasonic_profile
            .output(&self.committed_ultrasonic_state, e);
        let ultrasonic_power = ultrasonic * ultrasonic;
        self.ultrasonic_profile
            .advance(&mut self.committed_ultrasonic_state, e);
        let one_minus_10ms = 1.0 - self.ema_beta_10ms;
        let one_minus_1ms = 1.0 - self.ema_beta_1ms;
        self.committed_signed_error_ema = self
            .ema_beta_10ms
            .mul_add(self.committed_signed_error_ema, one_minus_10ms * e);
        self.committed_reconstruction_1ms_ema = self.ema_beta_1ms.mul_add(
            self.committed_reconstruction_1ms_ema,
            one_minus_1ms * instantaneous,
        );
        self.committed_reconstruction_10ms_ema = self.ema_beta_10ms.mul_add(
            self.committed_reconstruction_10ms_ema,
            one_minus_10ms * instantaneous,
        );
        self.reconstruction_1ms_energy.observe(instantaneous);
        self.reconstruction_10ms_energy.observe(instantaneous);
        self.committed_ultrasonic_ema = self.ema_beta_10ms.mul_add(
            self.committed_ultrasonic_ema,
            one_minus_10ms * ultrasonic_power,
        );
        let signed_error_ema_abs = self.committed_signed_error_ema.abs();
        if measure {
            self.ultrasonic_ema_p999
                .observe(self.committed_ultrasonic_ema);
            self.ultrasonic_ema_p9999
                .observe(self.committed_ultrasonic_ema);
            self.signed_error_ema_p999.observe(signed_error_ema_abs);
            self.signed_error_ema_p9999.observe(signed_error_ema_abs);
        }

        out_bits.push(bit);
        self.pending_output_balance = self.pending_output_balance.saturating_sub(1);
        self.replayed_output_energy += instantaneous;
        self.diagnostics.committed_samples = self.diagnostics.committed_samples.wrapping_add(1);
        self.diagnostics.positive_bits = self
            .diagnostics
            .positive_bits
            .wrapping_add(u64::from(bit == 1));
        self.diagnostics.remaining_tail_energy = tail;
        if measure {
            self.diagnostics.diagnostic_window_samples =
                self.diagnostics.diagnostic_window_samples.wrapping_add(1);
            self.diagnostics.diagnostic_window_positive_bits = self
                .diagnostics
                .diagnostic_window_positive_bits
                .wrapping_add(u64::from(bit == 1));
            if self.diagnostics.diagnostic_window_samples == 1 {
                self.diagnostics.diagnostic_window_starting_tail_energy = starting_tail;
            }
            self.diagnostics.diagnostic_window_remaining_tail_energy = tail;
            self.diagnostics.committed_output_energy += instantaneous;
            self.diagnostics.committed_tail_adjusted_energy += increment;
            self.diagnostics.maximum_tail_energy = self.diagnostics.maximum_tail_energy.max(tail);
            self.diagnostics.committed_ultrasonic_energy += ultrasonic_power;
            self.diagnostics.maximum_ultrasonic_power = self
                .diagnostics
                .maximum_ultrasonic_power
                .max(ultrasonic_power);
            self.diagnostics.maximum_ultrasonic_ema = self
                .diagnostics
                .maximum_ultrasonic_ema
                .max(self.committed_ultrasonic_ema);
            self.diagnostics.maximum_signed_error_ema = self
                .diagnostics
                .maximum_signed_error_ema
                .max(signed_error_ema_abs);
            self.diagnostics.ultrasonic_ema_p999 = self.ultrasonic_ema_p999.estimate();
            self.diagnostics.ultrasonic_ema_p9999 = self.ultrasonic_ema_p9999.estimate();
            self.diagnostics.signed_error_ema_abs_p999 = self.signed_error_ema_p999.estimate();
            self.diagnostics.signed_error_ema_abs_p9999 = self.signed_error_ema_p9999.estimate();
            self.diagnostics.maximum_reconstruction_1ms_ema = self
                .diagnostics
                .maximum_reconstruction_1ms_ema
                .max(self.committed_reconstruction_1ms_ema);
            self.diagnostics.maximum_reconstruction_10ms_ema = self
                .diagnostics
                .maximum_reconstruction_10ms_ema
                .max(self.committed_reconstruction_10ms_ema);
            self.diagnostics.maximum_reconstruction_1ms_energy = self
                .diagnostics
                .maximum_reconstruction_1ms_energy
                .max(self.reconstruction_1ms_energy.current());
            self.diagnostics.maximum_reconstruction_10ms_energy = self
                .diagnostics
                .maximum_reconstruction_10ms_energy
                .max(self.reconstruction_10ms_energy.current());
            self.diagnostics.maximum_abs_reconstruction_output = self
                .diagnostics
                .maximum_abs_reconstruction_output
                .max(output.abs());
        }
        self.diagnostics.committed_sequence = self.diagnostics.committed_sequence.wrapping_add(1);
        self.update_segment_predictions(bit);
    }

    fn emit_buffered_best(&mut self, out_bits: &mut Vec<u8>) {
        if self.buffered == 0 {
            return;
        }
        let best = self.parents[self.parents_bank][0];
        self.record_segment_prediction(best, self.buffered);
        for index in 0..self.buffered {
            let shift = self.buffered - 1 - index;
            let bit = (best.bits >> shift) & 1;
            self.commit_bit(bit, self.input_buffer[index], out_bits);
        }
        self.buffered = 0;
        self.input_buffer = [0.0; ECBEAM2_HORIZON];
    }

    fn record_segment_prediction(&mut self, best: EcBeam2Path, length: usize) {
        if length == 0 || length > ECBEAM2_HORIZON {
            return;
        }
        let Some(slot) = self.segment_predictions.iter().position(Option::is_none) else {
            debug_assert!(false, "EcBeam2 segment prediction ring overflow");
            return;
        };
        let mut state = self.committed_reconstruction_state;
        let mut predicted_increment = 0.0;
        for index in 0..length {
            let shift = length - 1 - index;
            let v = if ((best.bits >> shift) & 1) == 1 {
                1.0
            } else {
                -1.0
            };
            let error = v - self.input_buffer[index];
            predicted_increment += self
                .reconstruction_profile
                .tail_adjusted_energy_increment(&state, error);
            state = self.reconstruction_profile.next_state(&state, error);
        }
        let bits_mask = if length == 8 {
            u8::MAX
        } else {
            (1u8 << length) - 1
        };
        self.segment_predictions[slot] = Some(SegmentPrediction {
            epoch: self.diagnostics.committed_state_epoch,
            start_sequence: self.diagnostics.committed_sequence,
            length: length as u8,
            bits: best.bits & bits_mask,
            matched: true,
            predicted_increment,
            starting_output_energy: self.replayed_output_energy,
            starting_tail_energy: self.diagnostics.remaining_tail_energy,
        });
        self.diagnostics.predicted_segments_recorded =
            self.diagnostics.predicted_segments_recorded.wrapping_add(1);
    }

    fn update_segment_predictions(&mut self, committed_bit: u8) {
        let committed_sequence = self.diagnostics.committed_sequence;
        for index in 0..self.segment_predictions.len() {
            let Some(mut prediction) = self.segment_predictions[index] else {
                continue;
            };
            if prediction.epoch != self.diagnostics.committed_state_epoch {
                self.diagnostics.changed_before_commit_segments = self
                    .diagnostics
                    .changed_before_commit_segments
                    .wrapping_add(1);
                self.segment_predictions[index] = None;
                continue;
            }
            let emitted_index = committed_sequence
                .saturating_sub(1)
                .saturating_sub(prediction.start_sequence);
            if emitted_index < u64::from(prediction.length) {
                let shift = usize::from(prediction.length) - 1 - emitted_index as usize;
                prediction.matched &= ((prediction.bits >> shift) & 1) == committed_bit;
            }
            let end_sequence = prediction
                .start_sequence
                .wrapping_add(u64::from(prediction.length));
            if committed_sequence == end_sequence {
                if prediction.matched {
                    let observed = self.replayed_output_energy - prediction.starting_output_energy
                        + self.diagnostics.remaining_tail_energy
                        - prediction.starting_tail_energy;
                    let identity_error = (prediction.predicted_increment - observed).abs();
                    self.diagnostics.maximum_segment_identity_error = self
                        .diagnostics
                        .maximum_segment_identity_error
                        .max(identity_error);
                    self.diagnostics.matched_complete_segments =
                        self.diagnostics.matched_complete_segments.wrapping_add(1);
                } else {
                    self.diagnostics.changed_before_commit_segments = self
                        .diagnostics
                        .changed_before_commit_segments
                        .wrapping_add(1);
                }
                self.segment_predictions[index] = None;
            } else {
                self.segment_predictions[index] = Some(prediction);
            }
        }
    }

    fn renormalize_metrics(&mut self) {
        if self.parents_len == 0 {
            return;
        }
        let parents = &mut self.parents[self.parents_bank];
        let mut minimum = parents[0].metric;
        for parent in &parents[1..self.parents_len] {
            minimum = minimum.min(parent.metric);
        }
        for parent in &mut parents[..self.parents_len] {
            parent.metric -= minimum;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dsd::dsd_coeffs::CRFB_OSR64_OBG165;

    #[test]
    fn ecbeam2_rejects_unsupported_wire_rates_and_shadow_mode() {
        assert!(
            EcBeam2Modulator::new(
                &CRFB_OSR64_OBG165,
                1,
                5_644_800,
                EcBeam2ExperimentConfig::default(),
            )
            .is_err()
        );
        assert!(
            EcBeam2Modulator::new(
                &CRFB_OSR64_OBG165,
                1,
                2_822_400,
                EcBeam2ExperimentConfig {
                    run_mode: EcBeam2RunMode::ShadowA1,
                    ..EcBeam2ExperimentConfig::default()
                },
            )
            .is_err()
        );

        let mut wrong_coefficients = CRFB_OSR64_OBG165;
        wrong_coefficients.input_peak = f64::from_bits(wrong_coefficients.input_peak.to_bits() + 1);
        let wrong_coefficients = Box::leak(Box::new(wrong_coefficients));
        assert_eq!(
            EcBeam2Modulator::new(
                wrong_coefficients,
                1,
                2_822_400,
                EcBeam2ExperimentConfig::default(),
            )
            .err()
            .expect("mutated EcBeam2 coefficient table must be rejected"),
            "EcBeam2 v1 requires the DSD64 OBG1.65 CRFB coefficient table"
        );
    }

    #[test]
    fn ecbeam2_configuration_is_fail_closed() {
        for config in [
            EcBeam2ExperimentConfig {
                state_deadzone: f64::NAN,
                ..EcBeam2ExperimentConfig::default()
            },
            EcBeam2ExperimentConfig {
                state_deadzone_weight: -1.0,
                ..EcBeam2ExperimentConfig::default()
            },
            EcBeam2ExperimentConfig {
                quantizer_regularizer: f64::INFINITY,
                ..EcBeam2ExperimentConfig::default()
            },
            EcBeam2ExperimentConfig {
                ultrasonic_budget: Some(0.0),
                ..EcBeam2ExperimentConfig::default()
            },
            EcBeam2ExperimentConfig {
                signed_error_budget: Some(f64::NAN),
                ..EcBeam2ExperimentConfig::default()
            },
            EcBeam2ExperimentConfig {
                diagnostic_window: Some(EcBeam2DiagnosticWindow {
                    start_sequence: 8,
                    end_sequence: 8,
                }),
                ..EcBeam2ExperimentConfig::default()
            },
        ] {
            assert!(config.validated().is_err());
            assert!(EcBeam2Modulator::new(&CRFB_OSR64_OBG165, 1, 2_822_400, config).is_err());
        }
    }

    #[test]
    fn ecbeam2_is_chunk_and_flush_invariant() {
        let input: Vec<f64> = (0..4096)
            .map(|index| 0.2 * (index as f64 * 0.013).sin())
            .collect();
        let run = |chunks: bool| {
            let mut modulator = EcBeam2Modulator::new(
                &CRFB_OSR64_OBG165,
                7,
                2_822_400,
                EcBeam2ExperimentConfig::default(),
            )
            .unwrap();
            let mut bits = Vec::new();
            if chunks {
                for chunk in input.chunks(37) {
                    modulator.process_into_bits(chunk, &mut bits);
                }
            } else {
                modulator.process_into_bits(&input, &mut bits);
            }
            modulator.flush_into_bits(&mut bits);
            (bits, modulator.diagnostics())
        };
        let whole = run(false);
        let chunked = run(true);
        assert_eq!(whole.0, chunked.0);
        assert_eq!(whole.0.len(), input.len());
        assert_eq!(whole.1, chunked.1);
        assert_eq!(whole.1.all_nonfinite_resets, 0);
        assert_eq!(whole.1.output_length_events, 0);
        assert!(whole.1.matched_complete_segments > 0);
        assert!(whole.1.maximum_segment_identity_error < 1.0e-8);
    }

    #[test]
    fn ecbeam2_committed_tail_is_single_counted() {
        let input = vec![0.0; 512];
        let mut modulator = EcBeam2Modulator::new(
            &CRFB_OSR64_OBG165,
            9,
            3_072_000,
            EcBeam2ExperimentConfig::default(),
        )
        .unwrap();
        let mut bits = Vec::new();
        modulator.process_into_bits(&input, &mut bits);
        modulator.flush_into_bits(&mut bits);
        let diagnostics = modulator.diagnostics();
        let expected = diagnostics.committed_output_energy + diagnostics.remaining_tail_energy;
        assert!((diagnostics.committed_tail_adjusted_energy - expected).abs() < 1.0e-8);
    }

    #[test]
    fn ecbeam2_diagnostic_window_measures_only_warmed_committed_samples() {
        let window = EcBeam2DiagnosticWindow {
            start_sequence: 9,
            end_sequence: 23,
        };
        let input: Vec<f64> = (0..40)
            .map(|index| 0.14 * (index as f64 * 0.083).sin())
            .collect();
        let mut modulator = EcBeam2Modulator::new(
            &CRFB_OSR64_OBG165,
            0xD1A6_0057,
            2_822_400,
            EcBeam2ExperimentConfig {
                diagnostic_window: Some(window),
                ..EcBeam2ExperimentConfig::default()
            },
        )
        .unwrap();
        let mut bits = Vec::new();
        modulator.process_into_bits(&input, &mut bits);
        modulator.flush_into_bits(&mut bits);

        let profile = profiles_for_wire_rate(2_822_400).unwrap().reconstruction;
        let mut state = [0.0; MAX_BEAM_ERROR_PROFILE_STATES];
        let mut starting_tail = 0.0;
        let mut ending_tail = 0.0;
        let mut output_energy = 0.0;
        let mut increment_energy = 0.0;
        for (sequence, (&bit, u)) in bits.iter().zip(&input).enumerate() {
            let error = if bit == 1 { 1.0 - u } else { -1.0 - u };
            if sequence as u64 == window.start_sequence {
                starting_tail = profile.remaining_zero_input_energy(&state);
            }
            let output = profile.output(&state, error);
            let increment = profile.tail_adjusted_energy_increment(&state, error);
            profile.advance(&mut state, error);
            if window.contains(sequence as u64) {
                output_energy += output * output;
                increment_energy += increment;
                ending_tail = profile.remaining_zero_input_energy(&state);
            }
        }

        let diagnostics = modulator.diagnostics();
        assert_eq!(diagnostics.committed_samples, input.len() as u64);
        assert_eq!(diagnostics.diagnostic_window_samples, 14);
        assert_eq!(diagnostics.diagnostic_window_start_sequence, 9);
        assert_eq!(diagnostics.diagnostic_window_end_sequence, 23);
        assert!((diagnostics.diagnostic_window_starting_tail_energy - starting_tail).abs() < 1e-12);
        assert!((diagnostics.diagnostic_window_remaining_tail_energy - ending_tail).abs() < 1e-12);
        assert!((diagnostics.committed_output_energy - output_energy).abs() < 1e-12);
        assert!((diagnostics.committed_tail_adjusted_energy - increment_energy).abs() < 1e-12);
        assert!((increment_energy - output_energy - ending_tail + starting_tail).abs() < 1e-10);
    }

    #[test]
    fn ecbeam2_flush_preserves_profile_and_constraint_history() {
        let first: Vec<f64> = (0..73)
            .map(|index| 0.18 * (index as f64 * 0.071).sin())
            .collect();
        let second: Vec<f64> = (0..91)
            .map(|index| 0.11 * (index as f64 * 0.053).cos())
            .collect();
        let wire_rate = 2_822_400;
        let mut modulator = EcBeam2Modulator::new(
            &CRFB_OSR64_OBG165,
            0xF1_057,
            wire_rate,
            EcBeam2ExperimentConfig::default(),
        )
        .unwrap();
        let mut bits = Vec::new();
        modulator.process_into_bits(&first, &mut bits);
        modulator.flush_into_bits(&mut bits);
        assert_eq!(bits.len(), first.len());
        let frontier = modulator.parents[modulator.parents_bank][0];
        assert_eq!(
            frontier.reconstruction_state,
            modulator.committed_reconstruction_state
        );
        assert_eq!(
            frontier.ultrasonic_state,
            modulator.committed_ultrasonic_state
        );
        assert_eq!(frontier.ultrasonic_ema, modulator.committed_ultrasonic_ema);
        assert_eq!(
            frontier.signed_error_ema,
            modulator.committed_signed_error_ema
        );

        modulator.process_into_bits(&second, &mut bits);
        modulator.flush_into_bits(&mut bits);
        let inputs = first.iter().chain(&second).copied().collect::<Vec<_>>();
        assert_eq!(bits.len(), inputs.len());

        let profiles = profiles_for_wire_rate(wire_rate).unwrap();
        let mut reconstruction_state = [0.0; MAX_BEAM_ERROR_PROFILE_STATES];
        let mut ultrasonic_state = [0.0; MAX_BEAM_ERROR_PROFILE_STATES];
        let beta = (-1.0 / (wire_rate as f64 * ERROR_EMA_SECONDS)).exp();
        let mut ultrasonic_ema = 0.0;
        let mut signed_error_ema = 0.0;
        for (&bit, u) in bits.iter().zip(inputs) {
            let error = if bit == 1 { 1.0 - u } else { -1.0 - u };
            profiles
                .reconstruction
                .advance(&mut reconstruction_state, error);
            let ultrasonic = profiles.ultrasonic.advance(&mut ultrasonic_state, error);
            ultrasonic_ema = beta.mul_add(ultrasonic_ema, (1.0 - beta) * ultrasonic * ultrasonic);
            signed_error_ema = beta.mul_add(signed_error_ema, (1.0 - beta) * error);
        }
        assert_eq!(
            modulator.committed_reconstruction_state,
            reconstruction_state
        );
        assert_eq!(modulator.committed_ultrasonic_state, ultrasonic_state);
        assert!((modulator.committed_ultrasonic_ema - ultrasonic_ema).abs() < 1.0e-15);
        assert!((modulator.committed_signed_error_ema - signed_error_ema).abs() < 1.0e-15);
        let diagnostics = modulator.diagnostics();
        assert!(
            (diagnostics.committed_tail_adjusted_energy
                - diagnostics.committed_output_energy
                - diagnostics.remaining_tail_energy)
                .abs()
                < 1.0e-8
        );
    }

    #[test]
    fn ecbeam2_nonfinite_recovery_replays_the_emitted_stream() {
        let input = [0.0, 0.1, f64::NAN, -0.2, f64::INFINITY, 0.05, -0.03];
        let replay_input = [0.0, 0.1, 0.0, -0.2, 0.0, 0.05, -0.03];
        let wire_rate = 3_072_000;
        let mut modulator = EcBeam2Modulator::new(
            &CRFB_OSR64_OBG165,
            0xBAD_F1A7,
            wire_rate,
            EcBeam2ExperimentConfig::default(),
        )
        .unwrap();
        let mut bits = Vec::new();
        modulator.process_into_bits(&input, &mut bits);
        modulator.flush_into_bits(&mut bits);
        assert_eq!(bits.len(), input.len());

        let profiles = profiles_for_wire_rate(wire_rate).unwrap();
        let mut reconstruction_state = [0.0; MAX_BEAM_ERROR_PROFILE_STATES];
        let mut ultrasonic_state = [0.0; MAX_BEAM_ERROR_PROFILE_STATES];
        let mut output_energy = 0.0;
        let mut tail_adjusted_energy = 0.0;
        for (&bit, u) in bits.iter().zip(replay_input) {
            let error = if bit == 1 { 1.0 - u } else { -1.0 - u };
            let output = profiles.reconstruction.output(&reconstruction_state, error);
            output_energy += output * output;
            tail_adjusted_energy += profiles
                .reconstruction
                .tail_adjusted_energy_increment(&reconstruction_state, error);
            profiles
                .reconstruction
                .advance(&mut reconstruction_state, error);
            profiles.ultrasonic.advance(&mut ultrasonic_state, error);
        }
        let diagnostics = modulator.diagnostics();
        assert_eq!(diagnostics.all_nonfinite_resets, 2);
        assert_eq!(diagnostics.invalid_input_substitutions, 2);
        assert_eq!(
            modulator.committed_reconstruction_state,
            reconstruction_state
        );
        assert_eq!(modulator.committed_ultrasonic_state, ultrasonic_state);
        assert!((diagnostics.committed_output_energy - output_energy).abs() < 1.0e-10);
        assert!(
            (diagnostics.committed_tail_adjusted_energy - tail_adjusted_energy).abs() < 1.0e-10
        );
        assert!(
            (tail_adjusted_energy - output_energy - diagnostics.remaining_tail_energy).abs()
                < 1.0e-8
        );
    }

    #[test]
    fn exact_n8_n12_n16_oracle_is_deterministic_and_non_mutating() {
        let modulator = EcBeam2Modulator::new(
            &CRFB_OSR64_OBG165,
            11,
            2_822_400,
            EcBeam2ExperimentConfig::default(),
        )
        .unwrap();
        let initial_state = modulator.parents[modulator.parents_bank][0].state;
        let initial_diagnostics = modulator.diagnostics();
        for horizon in [8usize, 12, 16] {
            let input: Vec<f64> = (0..horizon)
                .map(|index| 0.015 * (index as f64 * 0.37).sin())
                .collect();
            let first = modulator.exact_horizon_oracle(&input).unwrap();
            let second = modulator.exact_horizon_oracle(&input).unwrap();
            assert_eq!(first, second);
            assert_eq!(first.horizon, horizon);
            assert!(first.complete_sequences > 0);
            assert_eq!(first.reconstructed_output.len(), horizon);
            assert_eq!(
                first.state_feasible,
                first.maximum_state_overflow == 0.0 && first.state_repairs == 0
            );
            assert_eq!(
                first.budgets_feasible,
                first.maximum_budget_violation == 0.0 && first.constraint_escapes == 0
            );
            assert_eq!(
                first.chosen_first_bit,
                ((first.chosen_sequence >> (horizon - 1)) & 1) as u8
            );
            assert!(
                (first.tail_adjusted_energy
                    - (first.causal_reconstruction_energy + first.remaining_tail_energy))
                    .abs()
                    < 1.0e-9
            );
        }
        assert_eq!(
            modulator.parents[modulator.parents_bank][0].state,
            initial_state
        );
        assert_eq!(modulator.diagnostics(), initial_diagnostics);
    }

    #[test]
    fn exact_oracle_rejects_unfrozen_horizons_and_nonfinite_windows() {
        let modulator = EcBeam2Modulator::new(
            &CRFB_OSR64_OBG165,
            13,
            3_072_000,
            EcBeam2ExperimentConfig::default(),
        )
        .unwrap();
        assert!(modulator.exact_horizon_oracle(&[0.0; 7]).is_err());
        let mut bad = [0.0; 8];
        bad[3] = f64::NAN;
        assert!(modulator.exact_horizon_oracle(&bad).is_err());
    }

    #[test]
    fn public_exact_oracle_replays_the_same_unflushed_starting_state() {
        let prefix: Vec<f64> = (0..37)
            .map(|index| 0.08 * (index as f64 * 0.071).sin())
            .collect();
        let window: Vec<f64> = (0..12)
            .map(|index| 0.03 * (index as f64 * 0.19).cos())
            .collect();
        let config = EcBeam2ExperimentConfig::default();
        let public = run_ecbeam2_exact_oracle(2_822_400, 0x000A_11CE, &prefix, &window, config)
            .expect("public exact oracle");

        let mut direct =
            EcBeam2Modulator::new(&CRFB_OSR64_OBG165, 0x000A_11CE, 2_822_400, config).unwrap();
        let mut discarded = Vec::new();
        direct.process_into_bits(&prefix, &mut discarded);
        let oracle_start = direct.parents[direct.parents_bank][0];
        let expected = direct.exact_horizon_oracle(&window).unwrap();
        assert!(
            (expected.sequence_objective - expected.tail_adjusted_energy).abs() < 1.0e-9,
            "default exact objective must telescope from a nonzero starting profile state"
        );
        assert_eq!(public.exact, expected);
        let mut singular_beam =
            EcBeam2Modulator::new(&CRFB_OSR64_OBG165, 0x000A_11CE, 2_822_400, config).unwrap();
        singular_beam.reseed_for_oracle(oracle_start);
        let mut singular_bits = Vec::new();
        singular_beam.process_into_bits(&window, &mut singular_bits);
        assert_eq!(public.m4n8_first_bit, singular_bits[0]);
        assert_eq!(
            public.first_bit_disagrees,
            public.m4n8_first_bit != public.exact.chosen_first_bit
        );
    }

    #[test]
    fn normalized_state_terms_have_the_declared_scale() {
        let mut state = [0.0; 8];
        state[0] = 0.70;
        assert_eq!(normalized_state_barrier(&state, 0.70), 0.0);
        state[..7].fill(1.0);
        assert!((normalized_state_potential(&state) - 1.0).abs() < f64::EPSILON);
        assert!((normalized_state_barrier(&state, 0.70) - 1.0).abs() < f64::EPSILON);
        state[..7].fill(0.80);
        assert_eq!(normalized_state_barrier(&state, 0.80), 0.0);
    }

    #[test]
    fn zero_state_controls_are_bit_identical_to_reconstruction_only() {
        let input: Vec<f64> = (0..4096)
            .map(|index| 0.12 * (index as f64 * 0.019).sin())
            .collect();
        let render = |config| {
            let mut modulator =
                EcBeam2Modulator::new(&CRFB_OSR64_OBG165, 0x1D_EA, 2_822_400, config).unwrap();
            let mut bits = Vec::new();
            modulator.process_into_bits(&input, &mut bits);
            modulator.flush_into_bits(&mut bits);
            bits
        };
        let diagnostic_knee = EcBeam2ExperimentConfig {
            state_terminal_weight: 0.0,
            state_deadzone: 0.88,
            state_deadzone_weight: 0.0,
            quantizer_regularizer: 0.0,
            ..EcBeam2ExperimentConfig::default()
        };
        assert_eq!(
            render(EcBeam2ExperimentConfig::default()),
            render(diagnostic_knee)
        );
    }

    #[test]
    fn state_terminal_objective_telescopes_and_components_sum() {
        let prefix: Vec<f64> = (0..91)
            .map(|index| 0.11 * (index as f64 * 0.037).sin())
            .collect();
        let window: Vec<f64> = (0..16)
            .map(|index| 0.04 * (index as f64 * 0.23).cos())
            .collect();
        let config = EcBeam2ExperimentConfig {
            state_terminal_weight: 0.17,
            state_deadzone: 0.80,
            state_deadzone_weight: 0.03,
            quantizer_regularizer: 0.002,
            ..EcBeam2ExperimentConfig::default()
        };
        let report = run_ecbeam2_exact_oracle(3_072_000, 0x51A_B1E, &prefix, &window, config)
            .unwrap()
            .exact;
        let expected_delta = report.terminal_state_potential - report.starting_state_potential;
        assert!((report.state_terminal_delta - expected_delta).abs() < 1.0e-12);
        assert!(
            (report.state_terminal_cost - config.state_terminal_weight * expected_delta).abs()
                < 1.0e-12
        );
        assert!((report.objective_components.total() - report.total_objective).abs() < 1.0e-12);
        assert!((report.sequence_objective - report.total_objective).abs() < 1.0e-12);
    }

    #[test]
    fn reused_oracle_seed_matches_separate_prefix_execution() {
        let prefix: Vec<f64> = (0..73)
            .map(|index| 0.07 * (index as f64 * 0.061).sin())
            .collect();
        let full_window: Vec<f64> = (0..16)
            .map(|index| 0.025 * (index as f64 * 0.17).cos())
            .collect();
        let config = EcBeam2ExperimentConfig {
            state_terminal_weight: 0.08,
            ..EcBeam2ExperimentConfig::default()
        };
        let seed = prepare_ecbeam2_oracle_seed(2_822_400, 0x5EED, &prefix, config).unwrap();
        for horizon in [8usize, 12, 16] {
            let reused =
                run_ecbeam2_exact_oracle_from_seed(&seed, &full_window[..horizon]).unwrap();
            let separate = run_ecbeam2_exact_oracle(
                2_822_400,
                0x5EED,
                &prefix,
                &full_window[..horizon],
                config,
            )
            .unwrap();
            assert_eq!(reused, separate);
        }
    }

    #[test]
    fn ecbeam2_constant_input_has_small_mean_error_with_explicit_regularizer() {
        for level in [-0.30, -0.10, 0.0, 0.10, 0.30] {
            let input = vec![level; 32_768];
            let config = EcBeam2ExperimentConfig {
                // Explicit ablation: the formal reconstruction-only default is
                // still zero. This upper-bound control verifies that the engine
                // can retain CRFB DC balance without a hidden objective term.
                quantizer_regularizer: 0.01,
                ..EcBeam2ExperimentConfig::default()
            };
            let mut modulator =
                EcBeam2Modulator::new(&CRFB_OSR64_OBG165, 17, 2_822_400, config).unwrap();
            let mut bits = Vec::new();
            modulator.process_into_bits(&input, &mut bits);
            modulator.flush_into_bits(&mut bits);
            let mean_error = bits
                .iter()
                .map(|bit| if *bit == 1 { 1.0 - level } else { -1.0 - level })
                .sum::<f64>()
                / bits.len() as f64;
            let diagnostics = modulator.diagnostics();
            assert!(
                mean_error.abs() < 0.03,
                "level {level} produced mean v-u {mean_error}; diagnostics={diagnostics:?}"
            );
            assert_eq!(diagnostics.output_length_events, 0);
        }
    }

    #[test]
    fn ecbeam2_escape_repair_reset_and_output_length_are_deterministic() {
        let run = |input: &[f64], config: EcBeam2ExperimentConfig| {
            let mut modulator =
                EcBeam2Modulator::new(&CRFB_OSR64_OBG165, 19, 3_072_000, config).unwrap();
            let mut bits = Vec::new();
            modulator.process_into_bits(input, &mut bits);
            modulator.flush_into_bits(&mut bits);
            (bits, modulator.diagnostics())
        };

        let escape_config = EcBeam2ExperimentConfig {
            ultrasonic_budget: Some(1.0e-18),
            signed_error_budget: Some(1.0e-18),
            ..EcBeam2ExperimentConfig::default()
        };
        let escape_input = vec![0.0; 64];
        let escape_a = run(&escape_input, escape_config);
        let escape_b = run(&escape_input, escape_config);
        assert_eq!(escape_a, escape_b);
        assert_eq!(escape_a.0.len(), escape_input.len());
        assert!(escape_a.1.constraint_escape > 0);
        assert_eq!(escape_a.1.output_length_events, 0);

        let overload_input = vec![8.0; 64];
        let overload = run(&overload_input, EcBeam2ExperimentConfig::default());
        assert_eq!(overload.0.len(), overload_input.len());
        assert!(overload.1.state_repair_fallback > 0);
        assert_eq!(overload.1.output_length_events, 0);

        let nonfinite_input = [0.0, f64::NAN, 0.1, f64::INFINITY, -0.1];
        let nonfinite = run(&nonfinite_input, EcBeam2ExperimentConfig::default());
        assert_eq!(nonfinite.0.len(), nonfinite_input.len());
        assert_eq!(nonfinite.1.all_nonfinite_resets, 2);
        assert_eq!(nonfinite.1.output_length_events, 0);
    }
}
