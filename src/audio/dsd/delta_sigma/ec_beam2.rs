use super::beam_error_profile::{
    BeamErrorProfile, DSD64_44K_FAMILY_WIRE_RATE, DSD64_48K_FAMILY_WIRE_RATE,
    DSD128_44K_FAMILY_WIRE_RATE, DSD128_48K_FAMILY_WIRE_RATE, MAX_BEAM_ERROR_PROFILE_STATES,
    StreamingQuantile, profiles_for_wire_rate,
};
use super::coeff_math::{denormalized_feedback8, mul8};
use super::modulator::{CrfbModulator, ModulatorMode};
use super::stability::{StateStability, stabilize_state};
use crate::audio::dsd::dsd_coeffs::{
    CRFB_OSR64_OBG164, CRFB_OSR64_OBG165, CRFB_OSR128_OBG164, ModulatorCoeffs,
};
use serde::Serialize;

const ECBEAM2_WIDTH: usize = 4;
const ECBEAM2_HORIZON: usize = 8;
const ECBEAM2_MAX_WIDTH: usize = 8;
const ECBEAM2_MAX_CHILDREN: usize = 2 * ECBEAM2_MAX_WIDTH;
const ECBEAM2_EXACT_MAX_HORIZON: usize = 16;
const ERROR_EMA_SECONDS: f64 = 0.010;

#[cfg(target_arch = "aarch64")]
enum SimdSteadyStep {
    NotHandled,
    Handled,
    RecoverNonfinite,
    CommitSlow,
    Emitted(u8),
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn insert_ecbeam2_top4(
    index: u8,
    metrics: &[f64; 2 * ECBEAM2_WIDTH],
    bits: &[u8; 2 * ECBEAM2_WIDTH],
    order: &mut [u8; ECBEAM2_WIDTH],
    len: &mut usize,
) {
    let index_usize = index as usize;
    let before = |left: usize, right: usize| {
        metrics[left] < metrics[right]
            || (metrics[left] == metrics[right] && bits[left] > bits[right])
    };
    if *len == ECBEAM2_WIDTH && !before(index_usize, order[*len - 1] as usize) {
        return;
    }
    let mut position = (*len).min(ECBEAM2_WIDTH - 1);
    while position > 0 && before(index_usize, order[position - 1] as usize) {
        if position < ECBEAM2_WIDTH {
            order[position] = order[position - 1];
        }
        position -= 1;
    }
    order[position] = index;
    *len = (*len + 1).min(ECBEAM2_WIDTH);
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn sort_ecbeam2_four(
    order: &mut [u8; ECBEAM2_WIDTH],
    metrics: &[f64; 2 * ECBEAM2_WIDTH],
    bits: &[u8; 2 * ECBEAM2_WIDTH],
) {
    let before = |left: u8, right: u8| {
        let left = left as usize;
        let right = right as usize;
        metrics[left] < metrics[right]
            || (metrics[left] == metrics[right] && bits[left] > bits[right])
    };
    for index in 1..ECBEAM2_WIDTH {
        let value = order[index];
        let mut position = index;
        while position > 0 && before(value, order[position - 1]) {
            order[position] = order[position - 1];
            position -= 1;
        }
        order[position] = value;
    }
}

const fn with_input_peak_and_state_limit_scale(
    mut coefficients: ModulatorCoeffs,
    input_peak: f64,
    state_limit_scale: f64,
) -> ModulatorCoeffs {
    coefficients.input_peak = input_peak;
    let mut stage = 0;
    while stage < coefficients.state_limit.len() {
        coefficients.state_limit[stage] *= state_limit_scale;
        stage += 1;
    }
    coefficients
}

// Production loudness plant. Its CRFB realization and OBG1.64 NTF are
// unchanged; only the admitted input calibration and proportional hard-state
// envelope are raised to the clean matched-stress ceiling. This is exactly
// 2 dB below the original EcBeam DSD128 calibration.
const ECBEAM2_OSR128_OBG164_INPUT468: f64 = 0.467_858_988_519_470_7;
const ECBEAM2_OSR128_OBG164_INPUT468_SCALE: f64 =
    ECBEAM2_OSR128_OBG164_INPUT468 / CRFB_OSR128_OBG164.input_peak;
static ECBEAM2_OSR128_OBG164_INPUT468_V1: ModulatorCoeffs = with_input_peak_and_state_limit_scale(
    CRFB_OSR128_OBG164,
    ECBEAM2_OSR128_OBG164_INPUT468,
    ECBEAM2_OSR128_OBG164_INPUT468_SCALE,
);

pub(crate) fn ecbeam2_dsd128_production_coefficients() -> &'static ModulatorCoeffs {
    &ECBEAM2_OSR128_OBG164_INPUT468_V1
}

/// Internal identity for the production coefficient tables plus the legacy
/// DSD64 oracle table retained from `main`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum EcBeam2PlantId {
    #[default]
    Obg164V1,
    Obg164Osr128Input468V1,
    Obg165V1,
}

impl EcBeam2PlantId {
    pub(crate) fn coefficients(self) -> &'static ModulatorCoeffs {
        match self {
            Self::Obg164V1 => &CRFB_OSR64_OBG164,
            Self::Obg164Osr128Input468V1 => &ECBEAM2_OSR128_OBG164_INPUT468_V1,
            Self::Obg165V1 => &CRFB_OSR64_OBG165,
        }
    }

    fn coefficients_match(self, coefficients: &ModulatorCoeffs) -> bool {
        modulator_coefficients_equal(coefficients, self.coefficients())
    }

    fn for_coefficients(coefficients: &ModulatorCoeffs) -> Option<Self> {
        [Self::Obg164V1, Self::Obg164Osr128Input468V1, Self::Obg165V1]
            .into_iter()
            .find(|plant| plant.coefficients_match(coefficients))
    }
}

/// The read-only production-frontier observer remains bound to EcBeam's
/// production OBG1.65 plant. Plant-screen admission applies only to the
/// isolated active EcBeam2 engine.
pub(crate) fn ecbeam2_v1_coefficients_match(coeffs: &ModulatorCoeffs) -> bool {
    modulator_coefficients_equal(coeffs, &CRFB_OSR64_OBG165)
}

fn modulator_coefficients_equal(coeffs: &ModulatorCoeffs, expected: &ModulatorCoeffs) -> bool {
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

/// EcBeam2 uses rate-specific realizations of the fixed physical-frequency
/// reconstruction experiment described by `Harness24To32V1`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EcBeam2ProfileId {
    #[default]
    Harness24To32V1,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct EcBeam2ObjectiveScales {
    reconstruction_abs_p95: f64,
    state_terminal_abs_p95: f64,
    state_barrier_p95: f64,
    quantizer_error_squared_p95: f64,
}

impl EcBeam2ObjectiveScales {
    const RAW: Self = Self {
        reconstruction_abs_p95: 1.0,
        state_terminal_abs_p95: 1.0,
        state_barrier_p95: 1.0,
        quantizer_error_squared_p95: 1.0,
    };
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
    /// Weight on the raw `(y-v)^2` path objective.
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
    pub const MAX_STATE_TERMINAL_WEIGHT: f64 = 1.0e6;
    pub const MAX_STATE_DEADZONE: f64 = 1.0;
    pub const MAX_STATE_DEADZONE_WEIGHT: f64 = 4.0;
    pub const MAX_QUANTIZER_REGULARIZER: f64 = 4.0;
    pub const MAX_ULTRASONIC_BUDGET: f64 = 16.0;
    pub const MAX_SIGNED_ERROR_BUDGET: f64 = 2.0;

    /// Validate the exact effective research configuration. Invalid values are
    /// rejected instead of being silently clamped or disabling a constraint.
    pub fn validated(self) -> Result<Self, &'static str> {
        if !self.state_terminal_weight.is_finite() || self.state_terminal_weight < 0.0 {
            return Err("EcBeam2 state-terminal weight must be finite and non-negative");
        }
        if self.state_terminal_weight > Self::MAX_STATE_TERMINAL_WEIGHT {
            return Err("EcBeam2 state-terminal weight must be finite and between 0 and 1e6");
        }
        if !self.state_deadzone.is_finite()
            || !(0.0..=Self::MAX_STATE_DEADZONE).contains(&self.state_deadzone)
        {
            return Err("EcBeam2 state dead-zone must be finite and in 0..=1");
        }
        if !self.state_deadzone_weight.is_finite() || self.state_deadzone_weight < 0.0 {
            return Err("EcBeam2 state dead-zone weight must be finite and non-negative");
        }
        if self.state_deadzone_weight > Self::MAX_STATE_DEADZONE_WEIGHT {
            return Err("EcBeam2 state dead-zone weight must be finite and between 0 and 4");
        }
        if self.state_deadzone_weight > 0.0 && self.state_deadzone >= 1.0 {
            return Err("EcBeam2 enabled state barrier requires a knee below the hard limit");
        }
        if !self.quantizer_regularizer.is_finite() || self.quantizer_regularizer < 0.0 {
            return Err("EcBeam2 quantizer regularizer must be finite and non-negative");
        }
        if self.quantizer_regularizer > Self::MAX_QUANTIZER_REGULARIZER {
            return Err("EcBeam2 quantizer regularizer must be finite and between 0 and 4");
        }
        if self
            .ultrasonic_budget
            .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            return Err("EcBeam2 ultrasonic budget must be finite and positive");
        }
        if self
            .ultrasonic_budget
            .is_some_and(|value| value > Self::MAX_ULTRASONIC_BUDGET)
        {
            return Err("EcBeam2 ultrasonic budget must be finite, positive, and at most 16");
        }
        if self
            .signed_error_budget
            .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            return Err("EcBeam2 signed-error budget must be finite and positive");
        }
        if self
            .signed_error_budget
            .is_some_and(|value| value > Self::MAX_SIGNED_ERROR_BUDGET)
        {
            return Err("EcBeam2 signed-error budget must be finite, positive, and at most 2");
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

/// Fixed production M4/N8 objective shared by DSD64 and DSD128.
pub(crate) fn ecbeam2_production_config() -> EcBeam2ExperimentConfig {
    EcBeam2ExperimentConfig {
        run_mode: EcBeam2RunMode::Active,
        profile: EcBeam2ProfileId::Harness24To32V1,
        state_terminal_weight: 0.0,
        state_deadzone: 0.0,
        state_deadzone_weight: 0.0,
        quantizer_regularizer: 0.03,
        ultrasonic_budget: None,
        signed_error_budget: None,
        diagnostic_window: None,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct EcBeam2ScaleDistribution {
    pub median: f64,
    pub p95: f64,
    pub p99: f64,
    pub maximum: f64,
}

/// Cumulative diagnostics for the actual committed EcBeam2 output path.
/// Candidate-expansion counters are intentionally kept separate from replayed
/// energy so provisional best-path switches cannot be mistaken for output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct EcBeam2Diagnostics {
    pub committed_samples: u64,
    /// Candidate transitions evaluated by the active C0 search. This is an
    /// observational work counter and never enters the objective.
    pub transition_evaluations: u64,
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
    /// Candidate-frontier events below M4 after the current engine/reset has
    /// first reached width four. The deterministic 1 -> 2 -> 4 startup fill
    /// and post-commit compaction are excluded.
    pub missing_survivor_events_after_initial_fill: u64,
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

/// Run one exact N8/N12/N16 oracle from the same isolated, guard-cold M4/N8
/// state reached by an already-normalized modulator-input prefix.
///
/// `prefix` and `window` are in the precise EcBeam2 `u` domain: post gain,
/// headroom, and limiter, with `v` represented as ±1. The helper uses the same
/// explicitly configured DSD64 research plant as the active renderer, consumes
/// the prefix without flushing its delayed frontier, and leaves production
/// EcBeam untouched.
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

#[derive(Debug, Clone, Copy, PartialEq)]
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
    rank_metric: f64,
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
    path_objective_increment: f64,
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

#[inline]
fn relative_budget_violation(observed: f64, budget: f64) -> f64 {
    if !observed.is_finite() || !budget.is_finite() || budget <= 0.0 {
        return f64::MAX;
    }
    if observed <= budget {
        return 0.0;
    }
    let ratio = observed / budget;
    if ratio.is_finite() {
        (ratio - 1.0).max(0.0)
    } else {
        f64::MAX
    }
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
        rank_metric: 0.0,
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
        path_objective_increment: 0.0,
        state_terminal_delta: 0.0,
        state_barrier_raw: 0.0,
        quantizer_error_squared: 0.0,
    };
}

/// Isolated, fixed M4/N8 tail-aware beam modulator.
pub(crate) struct EcBeam2Modulator {
    core: CrfbModulator,
    wire_rate: u32,
    /// Full qualification telemetry is intentionally orthogonal to every
    /// decision-affecting state transition. Renderer playback may disable it,
    /// while direct/research constructors retain the qualified reference path.
    full_diagnostics: bool,
    reconstruction_profile: BeamErrorProfile,
    ultrasonic_profile: BeamErrorProfile,
    plant: EcBeam2PlantId,
    config: EcBeam2ExperimentConfig,
    objective_scales: EcBeam2ObjectiveScales,
    parents: [[EcBeam2Path; ECBEAM2_MAX_WIDTH]; 2],
    parents_bank: usize,
    parents_len: usize,
    /// Stage-major raw survivor state for the AArch64 M4 kernel. Raw space is
    /// retained deliberately: every normalization and materialization keeps
    /// the same rounding boundary as the frozen production scalar implementation.
    #[cfg(target_arch = "aarch64")]
    simd_raw_state: [[[f64; ECBEAM2_WIDTH]; 8]; 2],
    #[cfg(target_arch = "aarch64")]
    simd_base_norm: [[f64; ECBEAM2_WIDTH]; 8],
    /// Conservative per-stage certificate boundary. If every normalized base
    /// is inside this boundary, both feedback signs are exactly known to stay
    /// finite and within the hard state limit.
    #[cfg(target_arch = "aarch64")]
    simd_both_sign_safe_abs_base: [f64; 8],
    #[cfg(target_arch = "aarch64")]
    simd_y: [f64; ECBEAM2_WIDTH],
    #[cfg(target_arch = "aarch64")]
    simd_reconstruction_state: [[[f64; ECBEAM2_WIDTH]; MAX_BEAM_ERROR_PROFILE_STATES]; 2],
    /// Eight generations of already-computed survivor reconstruction states
    /// and their ancestry. This makes the state after the oldest committed
    /// decision available without replaying a fifth dense profile transition.
    #[cfg(target_arch = "aarch64")]
    simd_reconstruction_history:
        [[[f64; ECBEAM2_WIDTH]; MAX_BEAM_ERROR_PROFILE_STATES]; ECBEAM2_HORIZON],
    #[cfg(target_arch = "aarch64")]
    simd_reconstruction_parent: [[u8; ECBEAM2_WIDTH]; ECBEAM2_HORIZON],
    #[cfg(target_arch = "aarch64")]
    simd_reconstruction_generation: usize,
    #[cfg(target_arch = "aarch64")]
    simd_reconstruction_history_depth: usize,
    #[cfg(target_arch = "aarch64")]
    simd_ultrasonic_state: [[[f64; ECBEAM2_WIDTH]; MAX_BEAM_ERROR_PROFILE_STATES]; 2],
    #[cfg(target_arch = "aarch64")]
    simd_metric: [[f64; ECBEAM2_WIDTH]; 2],
    #[cfg(target_arch = "aarch64")]
    simd_prev_v: [[f64; ECBEAM2_WIDTH]; 2],
    #[cfg(target_arch = "aarch64")]
    simd_bits: [[u8; ECBEAM2_WIDTH]; 2],
    #[cfg(target_arch = "aarch64")]
    simd_reconstruction_increment: [[f64; ECBEAM2_WIDTH]; 2],
    #[cfg(target_arch = "aarch64")]
    simd_ultrasonic_output: [[f64; ECBEAM2_WIDTH]; 2],
    #[cfg(target_arch = "aarch64")]
    simd_maximum_overflow: [[f64; ECBEAM2_WIDTH]; 2],
    #[cfg(target_arch = "aarch64")]
    simd_squared_overflow: [[f64; ECBEAM2_WIDTH]; 2],
    #[cfg(target_arch = "aarch64")]
    simd_candidate_finite: [[bool; ECBEAM2_WIDTH]; 2],
    #[cfg(target_arch = "aarch64")]
    simd_candidate_metric: [[f64; ECBEAM2_WIDTH]; 2],
    #[cfg(target_arch = "aarch64")]
    simd_state_valid: bool,
    #[cfg(test)]
    force_scalar_path: bool,
    survivor_initial_fill_complete: bool,
    buffered: usize,
    input_buffer: [f64; ECBEAM2_HORIZON],
    input_head: usize,
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
    replayed_output_energy: f64,
    diagnostics: EcBeam2Diagnostics,
}

/// Narrow public adapter used by the standalone modulator microbenchmark.
///
/// This deliberately exposes only production playback construction and
/// streaming. The benchmark therefore measures the same lean M4/N8 engine as
/// renderer playback without making the research engine part of the public
/// API.
#[doc(hidden)]
pub struct EcBeam2BenchmarkModulator {
    inner: EcBeam2Modulator,
}

impl EcBeam2BenchmarkModulator {
    fn coefficients(wire_rate: u32) -> Result<&'static ModulatorCoeffs, &'static str> {
        match wire_rate {
            DSD64_44K_FAMILY_WIRE_RATE | DSD64_48K_FAMILY_WIRE_RATE => {
                Ok(EcBeam2PlantId::Obg164V1.coefficients())
            }
            DSD128_44K_FAMILY_WIRE_RATE | DSD128_48K_FAMILY_WIRE_RATE => {
                Ok(ecbeam2_dsd128_production_coefficients())
            }
            _ => Err("EcBeam2 benchmark supports only DSD64 and DSD128 wire rates"),
        }
    }

    pub fn input_peak(wire_rate: u32) -> Result<f64, &'static str> {
        Ok(Self::coefficients(wire_rate)?.input_peak)
    }

    pub fn new_playback(seed: u64, wire_rate: u32) -> Result<Self, &'static str> {
        Ok(Self {
            inner: EcBeam2Modulator::new_with_diagnostics(
                Self::coefficients(wire_rate)?,
                seed,
                wire_rate,
                ecbeam2_production_config(),
                false,
            )?,
        })
    }

    #[inline]
    pub fn process_into_bits(&mut self, input: &[f64], out_bits: &mut Vec<u8>) {
        self.inner.process_into_bits(input, out_bits);
    }

    #[inline]
    pub fn flush_into_bits(&mut self, out_bits: &mut Vec<u8>) {
        self.inner.flush_into_bits(out_bits);
    }

    pub fn state_clamps(&self) -> u64 {
        self.inner.state_clamps()
    }

    pub fn stability_resets(&self) -> u64 {
        self.inner.stability_resets()
    }
}

impl EcBeam2Modulator {
    #[inline]
    fn beam_width(&self) -> usize {
        ECBEAM2_WIDTH
    }

    #[inline(always)]
    fn buffered_input(&self, index: usize) -> f64 {
        debug_assert!(index < self.buffered);
        self.input_buffer[(self.input_head + index) & (ECBEAM2_HORIZON - 1)]
    }

    #[inline(always)]
    fn push_buffered_input(&mut self, input: f64) {
        debug_assert!(self.buffered < ECBEAM2_HORIZON);
        let tail = (self.input_head + self.buffered) & (ECBEAM2_HORIZON - 1);
        self.input_buffer[tail] = input;
        self.buffered += 1;
    }

    #[inline(always)]
    fn remove_oldest_buffered_input(&mut self) {
        debug_assert!(self.buffered != 0);
        self.input_buffer[self.input_head] = 0.0;
        self.input_head = (self.input_head + 1) & (ECBEAM2_HORIZON - 1);
        self.buffered -= 1;
        if self.buffered == 0 {
            self.input_head = 0;
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn simd_m4n8_eligible(&self) -> bool {
        if cfg!(feature = "ecbeam2_observer") {
            return false;
        }
        #[cfg(test)]
        if self.force_scalar_path {
            return false;
        }
        let q_path = self.config.quantizer_regularizer.to_bits();
        !self.full_diagnostics
            && self.beam_width() == ECBEAM2_WIDTH
            && self.core.crfb_sparse
            && matches!(
                self.plant,
                EcBeam2PlantId::Obg164V1
                    | EcBeam2PlantId::Obg164Osr128Input468V1
                    | EcBeam2PlantId::Obg165V1
            )
            && self.config.state_terminal_weight == 0.0
            && self.config.state_deadzone == 0.0
            && self.config.state_deadzone_weight == 0.0
            && (q_path == 0.0f64.to_bits() || q_path == 0.03f64.to_bits())
            && self.config.ultrasonic_budget.is_none()
            && self.config.signed_error_budget.is_none()
            && self.config.diagnostic_window.is_none()
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn ensure_simd_raw_state(&mut self) {
        if self.simd_state_valid {
            return;
        }
        let bank = self.parents_bank;
        self.simd_raw_state[bank] = [[0.0; ECBEAM2_WIDTH]; 8];
        for parent in 0..self.parents_len {
            self.simd_metric[bank][parent] = self.parents[bank][parent].metric;
            self.simd_prev_v[bank][parent] = self.parents[bank][parent].prev_v;
            self.simd_bits[bank][parent] = self.parents[bank][parent].bits;
            for stage in 0..8 {
                self.simd_raw_state[bank][stage][parent] = self.parents[bank][parent].state[stage];
            }
            for stage in 0..MAX_BEAM_ERROR_PROFILE_STATES {
                self.simd_reconstruction_state[bank][stage][parent] =
                    self.parents[bank][parent].reconstruction_state[stage];
                self.simd_ultrasonic_state[bank][stage][parent] =
                    self.parents[bank][parent].ultrasonic_state[stage];
            }
        }
        self.simd_state_valid = true;
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn sync_simd_paths(&mut self) {
        if !self.simd_state_valid {
            return;
        }
        let bank = self.parents_bank;
        for parent in 0..self.parents_len {
            self.parents[bank][parent].metric = self.simd_metric[bank][parent];
            self.parents[bank][parent].prev_v = self.simd_prev_v[bank][parent];
            self.parents[bank][parent].bits = self.simd_bits[bank][parent];
            for stage in 0..8 {
                self.parents[bank][parent].state[stage] = self.simd_raw_state[bank][stage][parent];
            }
            for stage in 0..MAX_BEAM_ERROR_PROFILE_STATES {
                self.parents[bank][parent].reconstruction_state[stage] =
                    self.simd_reconstruction_state[bank][stage][parent];
                self.parents[bank][parent].ultrasonic_state[stage] =
                    self.simd_ultrasonic_state[bank][stage][parent];
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn record_simd_reconstruction_generation(
        &mut self,
        bank: usize,
        selected_parents: [usize; ECBEAM2_WIDTH],
    ) {
        let generation = if self.simd_reconstruction_history_depth == 0 {
            0
        } else {
            (self.simd_reconstruction_generation + 1) & (ECBEAM2_HORIZON - 1)
        };
        self.simd_reconstruction_generation = generation;
        self.simd_reconstruction_history[generation] = self.simd_reconstruction_state[bank];
        for (slot, parent) in selected_parents.into_iter().enumerate() {
            debug_assert!(parent < ECBEAM2_WIDTH);
            self.simd_reconstruction_parent[generation][slot] = parent as u8;
        }
        self.simd_reconstruction_history_depth =
            (self.simd_reconstruction_history_depth + 1).min(ECBEAM2_HORIZON);
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn committed_reconstruction_from_simd_ancestry(&mut self) -> bool {
        if self.simd_reconstruction_history_depth < ECBEAM2_HORIZON {
            return false;
        }
        let mut generation = self.simd_reconstruction_generation;
        let mut slot = 0usize;
        for _ in 0..ECBEAM2_HORIZON - 1 {
            slot = self.simd_reconstruction_parent[generation][slot] as usize;
            debug_assert!(slot < ECBEAM2_WIDTH);
            generation = generation.wrapping_sub(1) & (ECBEAM2_HORIZON - 1);
        }
        for stage in 0..MAX_BEAM_ERROR_PROFILE_STATES {
            self.committed_reconstruction_state[stage] =
                self.simd_reconstruction_history[generation][stage][slot];
        }
        true
    }

    /// Compute four sparse CRFB predictions in parallel. Each NEON lane keeps
    /// the scalar kernel's multiplication/FMA order, including its distinct
    /// row-zero and loop-output expressions.
    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn prepare_simd_frontier_core<const TRACK_CERTIFICATE: bool>(&mut self, u: f64) -> bool {
        use core::arch::aarch64::*;

        self.ensure_simd_raw_state();
        let bank = self.parents_bank;
        let mut all_signs_certified = true;
        // SAFETY: NEON is baseline on AArch64 and every load/store addresses a
        // two-lane region inside a four-element stage array.
        unsafe {
            for offset in [0usize, 2] {
                let normalize = |stage: usize| {
                    vmulq_n_f64(
                        vld1q_f64(self.simd_raw_state[bank][stage].as_ptr().add(offset)),
                        self.core.inverse_state_limit[stage],
                    )
                };
                let s0 = normalize(0);
                let s1 = normalize(1);
                let s2 = normalize(2);
                let s3 = normalize(3);
                let s4 = normalize(4);
                let s5 = normalize(5);
                let s6 = normalize(6);
                let row0_product = vmulq_n_f64(s0, self.core.a_rows_norm[0][0]);
                let b0 = vfmaq_n_f64(row0_product, vdupq_n_f64(self.core.bu_norm[0]), u);
                let mut b1 = vmulq_n_f64(vdupq_n_f64(self.core.bu_norm[1]), u);
                b1 = vfmaq_n_f64(b1, s0, self.core.a_rows_norm[1][0]);
                b1 = vfmaq_n_f64(b1, s1, self.core.a_rows_norm[1][1]);
                b1 = vfmaq_n_f64(b1, s2, self.core.a_rows_norm[1][2]);
                let mut b2 = vmulq_n_f64(vdupq_n_f64(self.core.bu_norm[2]), u);
                b2 = vfmaq_n_f64(b2, s0, self.core.a_rows_norm[2][0]);
                b2 = vfmaq_n_f64(b2, s1, self.core.a_rows_norm[2][1]);
                b2 = vfmaq_n_f64(b2, s2, self.core.a_rows_norm[2][2]);
                let mut b3 = vmulq_n_f64(vdupq_n_f64(self.core.bu_norm[3]), u);
                b3 = vfmaq_n_f64(b3, s2, self.core.a_rows_norm[3][2]);
                b3 = vfmaq_n_f64(b3, s3, self.core.a_rows_norm[3][3]);
                b3 = vfmaq_n_f64(b3, s4, self.core.a_rows_norm[3][4]);
                let mut b4 = vmulq_n_f64(vdupq_n_f64(self.core.bu_norm[4]), u);
                b4 = vfmaq_n_f64(b4, s2, self.core.a_rows_norm[4][2]);
                b4 = vfmaq_n_f64(b4, s3, self.core.a_rows_norm[4][3]);
                b4 = vfmaq_n_f64(b4, s4, self.core.a_rows_norm[4][4]);
                let mut b5 = vmulq_n_f64(vdupq_n_f64(self.core.bu_norm[5]), u);
                b5 = vfmaq_n_f64(b5, s4, self.core.a_rows_norm[5][4]);
                b5 = vfmaq_n_f64(b5, s5, self.core.a_rows_norm[5][5]);
                b5 = vfmaq_n_f64(b5, s6, self.core.a_rows_norm[5][6]);
                let mut b6 = vmulq_n_f64(vdupq_n_f64(self.core.bu_norm[6]), u);
                b6 = vfmaq_n_f64(b6, s4, self.core.a_rows_norm[6][4]);
                b6 = vfmaq_n_f64(b6, s5, self.core.a_rows_norm[6][5]);
                b6 = vfmaq_n_f64(b6, s6, self.core.a_rows_norm[6][6]);
                let bases = [b0, b1, b2, b3, b4, b5, b6];
                let mut both_signs_safe = vdupq_n_u64(u64::MAX);
                for (stage, base) in bases.iter().copied().enumerate() {
                    vst1q_f64(self.simd_base_norm[stage].as_mut_ptr().add(offset), base);
                    both_signs_safe = vandq_u64(
                        both_signs_safe,
                        vcleq_f64(
                            vabsq_f64(base),
                            vdupq_n_f64(self.simd_both_sign_safe_abs_base[stage]),
                        ),
                    );
                }
                let y_product = vmulq_n_f64(s6, self.core.c_row_norm[6]);
                let y = vfmaq_n_f64(y_product, vdupq_n_f64(self.core.coeffs.d1), u);
                let safe0 = vgetq_lane_u64::<0>(both_signs_safe) != 0;
                let safe1 = vgetq_lane_u64::<1>(both_signs_safe) != 0;
                if safe0 && safe1 {
                    for sign in 0..2 {
                        self.simd_maximum_overflow[sign][offset] = 0.0;
                        self.simd_maximum_overflow[sign][offset + 1] = 0.0;
                        self.simd_candidate_finite[sign][offset] = true;
                        self.simd_candidate_finite[sign][offset + 1] = true;
                    }
                } else {
                    if TRACK_CERTIFICATE {
                        all_signs_certified = false;
                    }
                    for (sign, v) in [1.0, -1.0].into_iter().enumerate() {
                        let mut maximum = vdupq_n_f64(0.0);
                        let mut finite = vdupq_n_u64(u64::MAX);
                        for (stage, base) in bases.iter().copied().enumerate() {
                            let candidate =
                                vfmaq_n_f64(base, vdupq_n_f64(self.core.bv_norm[stage]), v);
                            let absolute = vabsq_f64(candidate);
                            finite = vandq_u64(finite, vcleq_f64(absolute, vdupq_n_f64(f64::MAX)));
                            let overflow =
                                vmaxq_f64(vsubq_f64(absolute, vdupq_n_f64(1.0)), vdupq_n_f64(0.0));
                            maximum = vmaxq_f64(maximum, overflow);
                        }
                        vst1q_f64(
                            self.simd_maximum_overflow[sign].as_mut_ptr().add(offset),
                            maximum,
                        );
                        self.simd_candidate_finite[sign][offset] = vgetq_lane_u64::<0>(finite) != 0;
                        self.simd_candidate_finite[sign][offset + 1] =
                            vgetq_lane_u64::<1>(finite) != 0;
                    }
                }
                vst1q_f64(self.simd_y.as_mut_ptr().add(offset), y);
            }
        }
        self.simd_base_norm[7] = [0.0; ECBEAM2_WIDTH];
        self.simd_reconstruction_increment = self
            .reconstruction_profile
            .tail_adjusted_energy_increment_pair4(&self.simd_reconstruction_state[bank], u);
        let parent_metric = self.simd_metric[bank];
        // SAFETY: all vectors load/store two lanes within four-element arrays.
        unsafe {
            for (sign, v) in [1.0, -1.0].into_iter().enumerate() {
                for offset in [0usize, 2] {
                    let y = vld1q_f64(self.simd_y.as_ptr().add(offset));
                    let quantizer_error = vsubq_f64(y, vdupq_n_f64(v));
                    let quantizer_squared = vmulq_f64(quantizer_error, quantizer_error);
                    let quantizer_increment =
                        vmulq_n_f64(quantizer_squared, self.config.quantizer_regularizer);
                    debug_assert_eq!(
                        self.objective_scales.reconstruction_abs_p95.to_bits(),
                        1.0f64.to_bits()
                    );
                    let mut path_increment = vld1q_f64(
                        self.simd_reconstruction_increment[sign]
                            .as_ptr()
                            .add(offset),
                    );
                    path_increment = vaddq_f64(path_increment, vdupq_n_f64(0.0));
                    path_increment = vaddq_f64(path_increment, vdupq_n_f64(0.0));
                    path_increment = vaddq_f64(path_increment, quantizer_increment);
                    let metric = vaddq_f64(
                        vld1q_f64(parent_metric.as_ptr().add(offset)),
                        path_increment,
                    );
                    vst1q_f64(
                        self.simd_candidate_metric[sign].as_mut_ptr().add(offset),
                        metric,
                    );
                    let finite = vcleq_f64(vabsq_f64(metric), vdupq_n_f64(f64::MAX));
                    self.simd_candidate_finite[sign][offset] &= vgetq_lane_u64::<0>(finite) != 0;
                    self.simd_candidate_finite[sign][offset + 1] &=
                        vgetq_lane_u64::<1>(finite) != 0;
                }
            }
        }
        all_signs_certified
    }

    /// Non-inlined entry retained for DSD64 and the exact recovery hierarchy.
    /// DSD128 steady playback calls the always-inlined body above so frontier
    /// preparation and selection form one optimizer-visible kernel.
    #[cfg(target_arch = "aarch64")]
    #[inline(never)]
    fn prepare_simd_frontier(&mut self, u: f64) {
        let _ = self.prepare_simd_frontier_core::<false>(u);
    }

    /// Squared overflow is the second-order tie-breaker only when the entire
    /// frontier violates a hard state limit. Normal feasible playback never
    /// reads it, so retain the exact lane-local accumulation order in this
    /// fallback-only calculation.
    #[cfg(target_arch = "aarch64")]
    #[cold]
    #[inline(never)]
    fn prepare_simd_squared_overflow(&mut self) {
        use core::arch::aarch64::*;
        // SAFETY: every vector access covers two lanes inside a four-parent
        // stage array, and NEON is baseline on AArch64.
        unsafe {
            for (sign, v) in [1.0, -1.0].into_iter().enumerate() {
                for offset in [0usize, 2] {
                    let mut squared = vdupq_n_f64(0.0);
                    for stage in 0..7 {
                        let base = vld1q_f64(self.simd_base_norm[stage].as_ptr().add(offset));
                        let candidate = vfmaq_n_f64(base, vdupq_n_f64(self.core.bv_norm[stage]), v);
                        let overflow = vmaxq_f64(
                            vsubq_f64(vabsq_f64(candidate), vdupq_n_f64(1.0)),
                            vdupq_n_f64(0.0),
                        );
                        squared = vaddq_f64(squared, vmulq_f64(overflow, overflow));
                    }
                    vst1q_f64(
                        self.simd_squared_overflow[sign].as_mut_ptr().add(offset),
                        squared,
                    );
                }
            }
        }
    }

    #[inline(always)]
    fn parent_prediction<const SIMD: bool>(
        &self,
        parent_index: usize,
        parent: EcBeam2Path,
        u: f64,
    ) -> ([f64; 8], [f64; 8], f64) {
        #[cfg(target_arch = "aarch64")]
        if SIMD {
            let bank = self.parents_bank;
            let state = core::array::from_fn(|stage| {
                self.simd_raw_state[bank][stage][parent_index]
                    * self.core.inverse_state_limit[stage]
            });
            let base = core::array::from_fn(|stage| self.simd_base_norm[stage][parent_index]);
            return (state, base, self.simd_y[parent_index]);
        }
        let state = mul8(&parent.state, &self.core.inverse_state_limit);
        let base = if self.core.crfb_sparse {
            self.core.predict_base_norm::<true>(&state, u)
        } else {
            self.core.predict_base_norm::<false>(&state, u)
        };
        let y = if self.core.crfb_sparse {
            self.core.loop_output_norm::<true>(&state, u)
        } else {
            self.core.loop_output_norm::<false>(&state, u)
        };
        (state, base, y)
    }

    pub(crate) fn new(
        coeffs: &'static ModulatorCoeffs,
        seed: u64,
        wire_rate: u32,
        config: EcBeam2ExperimentConfig,
    ) -> Result<Self, &'static str> {
        Self::new_inner(coeffs, seed, wire_rate, config, true)
    }

    pub(crate) fn new_with_diagnostics(
        coeffs: &'static ModulatorCoeffs,
        seed: u64,
        wire_rate: u32,
        config: EcBeam2ExperimentConfig,
        full_diagnostics: bool,
    ) -> Result<Self, &'static str> {
        Self::new_inner(coeffs, seed, wire_rate, config, full_diagnostics)
    }

    fn new_inner(
        coeffs: &'static ModulatorCoeffs,
        seed: u64,
        wire_rate: u32,
        config: EcBeam2ExperimentConfig,
        full_diagnostics: bool,
    ) -> Result<Self, &'static str> {
        let config = config.validated()?;
        if !matches!(config.run_mode, EcBeam2RunMode::Active) {
            return Err("EcBeam2 ShadowA1 requires the production-frontier observer");
        }
        let plant = EcBeam2PlantId::for_coefficients(coeffs)
            .ok_or("EcBeam2 requires a registered production or legacy-oracle coefficient table")?;
        // Keep profile selection exhaustive even while there is only one
        // profile. A future profile variant must not compile while silently
        // continuing to use the original reconstruction/ultrasonic pair.
        let profiles = match config.profile {
            EcBeam2ProfileId::Harness24To32V1 => profiles_for_wire_rate(wire_rate).map_err(|_| {
                "EcBeam2 supports only 2.8224/3.072 MHz DSD64 and 5.6448/6.144 MHz DSD128 wire rates"
            })?,
        };
        let objective_scales = EcBeam2ObjectiveScales::RAW;
        let mut core = CrfbModulator::new_with_mode(coeffs, seed, ModulatorMode::Ec)?;
        core.set_dither_scale(0.0);
        core.set_isi_penalty(0.0);
        #[cfg(target_arch = "aarch64")]
        let simd_both_sign_safe_abs_base = core
            .bv_norm
            .map(|feedback| ((1.0 - 16.0 * f64::EPSILON) - feedback.abs()).next_down());
        let beta = |seconds: f64| (-1.0 / (wire_rate as f64 * seconds)).exp();
        let mut this = Self {
            core,
            wire_rate,
            full_diagnostics,
            reconstruction_profile: profiles.reconstruction,
            ultrasonic_profile: profiles.ultrasonic,
            plant,
            config,
            objective_scales,
            parents: [[EcBeam2Path::INERT; ECBEAM2_MAX_WIDTH]; 2],
            parents_bank: 0,
            parents_len: 1,
            #[cfg(target_arch = "aarch64")]
            simd_raw_state: [[[0.0; ECBEAM2_WIDTH]; 8]; 2],
            #[cfg(target_arch = "aarch64")]
            simd_base_norm: [[0.0; ECBEAM2_WIDTH]; 8],
            #[cfg(target_arch = "aarch64")]
            simd_both_sign_safe_abs_base,
            #[cfg(target_arch = "aarch64")]
            simd_y: [0.0; ECBEAM2_WIDTH],
            #[cfg(target_arch = "aarch64")]
            simd_reconstruction_state: [[[0.0; ECBEAM2_WIDTH]; MAX_BEAM_ERROR_PROFILE_STATES]; 2],
            #[cfg(target_arch = "aarch64")]
            simd_reconstruction_history: [[[0.0; ECBEAM2_WIDTH]; MAX_BEAM_ERROR_PROFILE_STATES];
                ECBEAM2_HORIZON],
            #[cfg(target_arch = "aarch64")]
            simd_reconstruction_parent: [[0; ECBEAM2_WIDTH]; ECBEAM2_HORIZON],
            #[cfg(target_arch = "aarch64")]
            simd_reconstruction_generation: 0,
            #[cfg(target_arch = "aarch64")]
            simd_reconstruction_history_depth: 0,
            #[cfg(target_arch = "aarch64")]
            simd_ultrasonic_state: [[[0.0; ECBEAM2_WIDTH]; MAX_BEAM_ERROR_PROFILE_STATES]; 2],
            #[cfg(target_arch = "aarch64")]
            simd_metric: [[0.0; ECBEAM2_WIDTH]; 2],
            #[cfg(target_arch = "aarch64")]
            simd_prev_v: [[1.0; ECBEAM2_WIDTH]; 2],
            #[cfg(target_arch = "aarch64")]
            simd_bits: [[0; ECBEAM2_WIDTH]; 2],
            #[cfg(target_arch = "aarch64")]
            simd_reconstruction_increment: [[0.0; ECBEAM2_WIDTH]; 2],
            #[cfg(target_arch = "aarch64")]
            simd_ultrasonic_output: [[0.0; ECBEAM2_WIDTH]; 2],
            #[cfg(target_arch = "aarch64")]
            simd_maximum_overflow: [[0.0; ECBEAM2_WIDTH]; 2],
            #[cfg(target_arch = "aarch64")]
            simd_squared_overflow: [[0.0; ECBEAM2_WIDTH]; 2],
            #[cfg(target_arch = "aarch64")]
            simd_candidate_finite: [[true; ECBEAM2_WIDTH]; 2],
            #[cfg(target_arch = "aarch64")]
            simd_candidate_metric: [[0.0; ECBEAM2_WIDTH]; 2],
            #[cfg(target_arch = "aarch64")]
            simd_state_valid: false,
            #[cfg(test)]
            force_scalar_path: false,
            survivor_initial_fill_complete: false,
            buffered: 0,
            input_buffer: [0.0; ECBEAM2_HORIZON],
            input_head: 0,
            ema_beta_10ms: beta(ERROR_EMA_SECONDS),
            ema_beta_1ms: beta(0.001),
            committed_reconstruction_state: [0.0; MAX_BEAM_ERROR_PROFILE_STATES],
            committed_ultrasonic_state: [0.0; MAX_BEAM_ERROR_PROFILE_STATES],
            committed_ultrasonic_ema: 0.0,
            committed_signed_error_ema: 0.0,
            committed_reconstruction_1ms_ema: 0.0,
            committed_reconstruction_10ms_ema: 0.0,
            reconstruction_1ms_energy: RollingEnergy::new(if full_diagnostics {
                (wire_rate as usize + 500) / 1_000
            } else {
                1
            }),
            reconstruction_10ms_energy: RollingEnergy::new(if full_diagnostics {
                wire_rate as usize / 100
            } else {
                1
            }),
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
        let starting_output_len = out_bits.len();
        let starting_buffered = self.buffered;
        out_bits.reserve(input.len());
        #[cfg(target_arch = "aarch64")]
        if self.simd_m4n8_eligible() {
            if self.wire_rate >= DSD128_44K_FAMILY_WIRE_RATE {
                self.process_into_bits_simd_steady(input, out_bits);
            } else {
                for &u in input {
                    self.process_sample_simd::<false>(u, out_bits);
                }
            }
            self.sync_simd_paths();
            debug_assert_eq!(
                out_bits.len() - starting_output_len + self.buffered,
                input.len() + starting_buffered
            );
            return;
        }
        for &u in input {
            self.process_sample(u, out_bits);
        }
        debug_assert_eq!(
            out_bits.len() - starting_output_len + self.buffered,
            input.len() + starting_buffered
        );
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(never)]
    fn process_into_bits_simd_steady(&mut self, input: &[f64], out_bits: &mut Vec<u8>) {
        out_bits.reserve(input.len());
        let mut segment_start = out_bits.len();
        let mut written = 0usize;
        let mut output_ptr = out_bits.as_mut_ptr();
        let mut committed_samples = self.diagnostics.committed_samples;
        let mut positive_bits = self.diagnostics.positive_bits;
        let mut committed_sequence = self.diagnostics.committed_sequence;

        for (index, &u) in input.iter().enumerate() {
            let step = if u.is_finite() && u.abs() <= 2.0 {
                self.try_process_feasible_m4_simd(u)
            } else {
                SimdSteadyStep::NotHandled
            };
            match step {
                SimdSteadyStep::Handled => {}
                SimdSteadyStep::Emitted(bit) => {
                    debug_assert!(segment_start + written < out_bits.capacity());
                    // SAFETY: the block reserved one slot per input before the
                    // loop. Slow-path publication below resets this pointer
                    // after any operation that may reallocate the vector.
                    unsafe {
                        output_ptr.add(segment_start + written).write(bit);
                    }
                    written += 1;
                    committed_samples = committed_samples.wrapping_add(1);
                    positive_bits = positive_bits.wrapping_add(u64::from(bit == 1));
                    committed_sequence = committed_sequence.wrapping_add(1);
                }
                slow => {
                    // Publish the initialized prefix and block-local counters
                    // before entering code that observes the Vec or generic
                    // diagnostic state.
                    unsafe {
                        out_bits.set_len(segment_start + written);
                    }
                    self.diagnostics.committed_samples = committed_samples;
                    self.diagnostics.positive_bits = positive_bits;
                    self.diagnostics.committed_sequence = committed_sequence;

                    match slow {
                        SimdSteadyStep::RecoverNonfinite => {
                            self.recover_nonfinite_frontier(u, out_bits);
                        }
                        SimdSteadyStep::CommitSlow => self.commit_oldest_simd(out_bits),
                        SimdSteadyStep::NotHandled => {
                            if u.is_finite() && u.abs() <= 2.0 {
                                // The failed steady attempt changed only SIMD
                                // scratch, so recomputing preparation in the
                                // exact hierarchy is bit-identical.
                                self.process_frontier_sample_simd::<false>(u, out_bits);
                            } else {
                                self.process_sample_simd::<true>(u, out_bits);
                            }
                        }
                        SimdSteadyStep::Handled | SimdSteadyStep::Emitted(_) => unreachable!(),
                    }

                    committed_samples = self.diagnostics.committed_samples;
                    positive_bits = self.diagnostics.positive_bits;
                    committed_sequence = self.diagnostics.committed_sequence;
                    out_bits.reserve(input.len() - index - 1);
                    segment_start = out_bits.len();
                    written = 0;
                    output_ptr = out_bits.as_mut_ptr();
                }
            }
        }

        // SAFETY: every element in this suffix was initialized exactly once
        // by the direct steady-path store above.
        unsafe {
            out_bits.set_len(segment_start + written);
        }
        self.diagnostics.committed_samples = committed_samples;
        self.diagnostics.positive_bits = positive_bits;
        self.diagnostics.committed_sequence = committed_sequence;
    }

    pub(crate) fn flush_into_bits(&mut self, out_bits: &mut Vec<u8>) {
        if self.buffered == 0 {
            return;
        }
        let starting_output_len = out_bits.len();
        let starting_buffered = self.buffered;
        let best = self.parents[self.parents_bank][0];
        self.record_segment_prediction(best, self.buffered);
        for index in 0..self.buffered {
            let shift = self.buffered - 1 - index;
            let bit = (best.bits >> shift) & 1;
            self.commit_bit(bit, self.buffered_input(index), out_bits);
        }
        self.core.state = best.state;
        self.core.prev_v = best.prev_v;
        self.buffered = 0;
        self.input_head = 0;
        self.reseed_from_core();
        debug_assert_eq!(out_bits.len() - starting_output_len, starting_buffered);
    }

    pub(crate) fn reset(&mut self) {
        if self.buffered != 0 {
            self.diagnostics.output_length_events =
                self.diagnostics.output_length_events.wrapping_add(1);
        }
        self.core.reset();
        self.buffered = 0;
        self.input_buffer = [0.0; ECBEAM2_HORIZON];
        self.input_head = 0;
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
        self.full_diagnostics
            && self
                .config
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
        let start = self.parents[self.parents_bank][0];
        self.exact_horizon_oracle_from_start(start, input)
    }

    fn exact_horizon_oracle_from_start(
        &self,
        start: EcBeam2Path,
        input: &[f64],
    ) -> Result<EcBeam2ExactOracleReport, &'static str> {
        if !matches!(input.len(), 8 | 12 | 16) {
            return Err("EcBeam2 exact oracle supports only N8, N12, or N16");
        }
        if input.iter().any(|sample| !sample.is_finite()) {
            return Err("EcBeam2 exact oracle requires finite input samples");
        }

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
                        && signed_error_ema.is_finite()
                        && reconstruction_state.iter().all(|value| value.is_finite())
                        && ultrasonic_state.iter().all(|value| value.is_finite());
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
                        .map(|budget| relative_budget_violation(ultrasonic_ema, budget))
                        .unwrap_or(0.0);
                    let signed_violation = self
                        .config
                        .signed_error_budget
                        .map(|budget| relative_budget_violation(signed_error_ema.abs(), budget))
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
                    if !quantizer_error_squared.is_finite() {
                        continue;
                    }
                    let quantizer_cost = parent.objective_components.quantizer_regularizer
                        + self.config.quantizer_regularizer * quantizer_error_squared;
                    let objective_components = EcBeam2ObjectiveComponents {
                        reconstruction: parent.objective_components.reconstruction
                            + reconstruction_delta / self.objective_scales.reconstruction_abs_p95,
                        state_terminal: parent.objective_components.state_terminal
                            + self.config.state_terminal_weight * state_terminal_delta
                                / self.objective_scales.state_terminal_abs_p95,
                        state_barrier: parent.objective_components.state_barrier
                            + self.config.state_deadzone_weight * state_barrier_raw
                                / self.objective_scales.state_barrier_p95,
                        quantizer_regularizer: quantizer_cost,
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
        let sequence_objective = winner.metric;
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
            sequence_objective,
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
            total_objective: sequence_objective,
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
        #[cfg(target_arch = "aarch64")]
        {
            self.simd_state_valid = false;
            self.simd_reconstruction_history_depth = 0;
        }
        self.parents_bank = 0;
        self.parents_len = 1;
        self.survivor_initial_fill_complete = false;
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
        #[cfg(target_arch = "aarch64")]
        {
            self.simd_state_valid = false;
            self.simd_reconstruction_history_depth = 0;
        }
        self.core.state = start.state;
        self.core.prev_v = start.prev_v;
        self.committed_reconstruction_state = start.reconstruction_state;
        self.committed_ultrasonic_state = start.ultrasonic_state;
        self.committed_ultrasonic_ema = start.ultrasonic_ema;
        self.committed_signed_error_ema = start.signed_error_ema;
        self.diagnostics.remaining_tail_energy = self
            .reconstruction_profile
            .remaining_zero_input_energy(&start.reconstruction_state);
        self.parents = [[EcBeam2Path::INERT; ECBEAM2_MAX_WIDTH]; 2];
        self.parents_bank = 0;
        self.parents_len = 1;
        self.survivor_initial_fill_complete = false;
        self.parents[0][0] = EcBeam2Path {
            metric: 0.0,
            bits: 0,
            ..start
        };
        self.buffered = 0;
        self.input_buffer = [0.0; ECBEAM2_HORIZON];
        self.input_head = 0;
        self.segment_predictions = [None; ECBEAM2_HORIZON];
    }

    /// Abandon the delayed frontier after a finite input produces no evaluable
    /// child or a retained survivor whose complete numerical state cannot be
    /// represented. This is shared by candidate-evaluation failures and the
    /// later retained-profile-state check so neither path can leave a
    /// partially materialized frontier active.
    fn recover_nonfinite_frontier(&mut self, u: f64, out_bits: &mut Vec<u8>) {
        debug_assert!(u.is_finite());
        #[cfg(target_arch = "aarch64")]
        self.sync_simd_paths();
        self.emit_buffered_best(out_bits);
        self.core.hard_reset();
        self.core.stability_resets = self.core.stability_resets.wrapping_add(1);
        self.diagnostics.all_nonfinite_resets =
            self.diagnostics.all_nonfinite_resets.wrapping_add(1);
        // Replay the recovery bit against the real finite sample so committed
        // profile state remains the emitted stream.
        self.commit_bit(1, u, out_bits);
        self.reseed_from_core();
    }

    fn process_sample(&mut self, u: f64, out_bits: &mut Vec<u8>) {
        self.process_frontier_sample::<false>(u, out_bits);
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn process_sample_simd<const FAST_FEASIBLE: bool>(&mut self, u: f64, out_bits: &mut Vec<u8>) {
        if u.is_finite() && u.abs() > 2.0 {
            // The renderer limiter keeps production samples inside this
            // envelope. Preserve the scalar kernel's complete finite/overflow
            // recovery semantics for direct adversarial inputs.
            self.sync_simd_paths();
            self.simd_state_valid = false;
            self.simd_reconstruction_history_depth = 0;
            self.process_frontier_sample::<false>(u, out_bits);
            return;
        }
        self.process_frontier_sample_simd::<FAST_FEASIBLE>(u, out_bits);
    }

    /// Steady-state playback expansion for the overwhelmingly common case:
    /// four live parents and enough finite, constraint-feasible children to
    /// retain a full root-compatible frontier. The generic recovery hierarchy
    /// remains below for warm-up and overloads.
    /// Keeping this as a separate non-inlined function avoids pulling the
    /// fallback selector and stabilization machinery into the hot loop.
    #[cfg(target_arch = "aarch64")]
    #[allow(clippy::needless_range_loop)]
    #[inline(never)]
    fn try_process_feasible_m4_simd(&mut self, u: f64) -> SimdSteadyStep {
        use core::arch::aarch64::*;

        let all_signs_certified = self.prepare_simd_frontier_core::<true>(u);
        if self.parents_len != ECBEAM2_WIDTH {
            return SimdSteadyStep::NotHandled;
        }

        const CHILDREN: usize = 2 * ECBEAM2_WIDTH;
        let parent_bank = self.parents_bank;
        let mut child_metric = [0.0; CHILDREN];
        let mut child_bits = [0u8; CHILDREN];
        let mut child_feasible = [false; CHILDREN];
        for parent in 0..ECBEAM2_WIDTH {
            let parent_bits = self.simd_bits[parent_bank][parent];
            let positive = 2 * parent;
            child_metric[positive] = self.simd_candidate_metric[0][parent];
            child_metric[positive + 1] = self.simd_candidate_metric[1][parent];
            child_bits[positive] = (parent_bits << 1) | 1;
            child_bits[positive + 1] = parent_bits << 1;
            child_feasible[positive] = self.simd_candidate_finite[0][parent]
                && self.simd_maximum_overflow[0][parent] == 0.0;
            child_feasible[positive + 1] = self.simd_candidate_finite[1][parent]
                && self.simd_maximum_overflow[1][parent] == 0.0;
        }

        let committing = self.buffered + 1 == ECBEAM2_HORIZON;
        let mut order = [0u8; ECBEAM2_WIDTH];
        let mut order_len = 0usize;
        let all_children_feasible = all_signs_certified
            && self.simd_candidate_finite[0]
                .into_iter()
                .all(|finite| finite)
            && self.simd_candidate_finite[1]
                .into_iter()
                .all(|finite| finite);
        if committing && all_children_feasible {
            let before = |left: usize, right: usize| {
                child_metric[left] < child_metric[right]
                    || (child_metric[left] == child_metric[right]
                        && child_bits[left] > child_bits[right])
            };
            let mut best_by_parent = [0u8; ECBEAM2_WIDTH];
            for parent in 0..ECBEAM2_WIDTH {
                let positive = 2 * parent;
                best_by_parent[parent] = if before(positive, positive + 1) {
                    positive as u8
                } else {
                    (positive + 1) as u8
                };
            }
            let mut best = best_by_parent[0] as usize;
            for &candidate in &best_by_parent[1..] {
                let candidate = candidate as usize;
                if before(candidate, best) {
                    best = candidate;
                }
            }
            let chosen_root = (child_bits[best] >> (ECBEAM2_HORIZON - 1)) & 1;
            let mut compatible_parents = [0u8; ECBEAM2_WIDTH];
            let mut compatible_len = 0usize;
            for parent in 0..ECBEAM2_WIDTH {
                let parent_root =
                    (self.simd_bits[parent_bank][parent] >> (ECBEAM2_HORIZON - 2)) & 1;
                if parent_root == chosen_root {
                    compatible_parents[compatible_len] = parent as u8;
                    compatible_len += 1;
                }
            }
            if compatible_len == 2 {
                let first = 2 * compatible_parents[0];
                let second = 2 * compatible_parents[1];
                order = [first, first + 1, second, second + 1];
                sort_ecbeam2_four(&mut order, &child_metric, &child_bits);
                order_len = ECBEAM2_WIDTH;
            } else {
                for &parent in &compatible_parents[..compatible_len] {
                    for index in [2 * parent, 2 * parent + 1] {
                        insert_ecbeam2_top4(
                            index,
                            &child_metric,
                            &child_bits,
                            &mut order,
                            &mut order_len,
                        );
                    }
                }
            }
        } else if committing {
            let Some(mut best) = child_feasible.iter().position(|&feasible| feasible) else {
                return SimdSteadyStep::NotHandled;
            };
            for index in best + 1..CHILDREN {
                if child_feasible[index]
                    && (child_metric[index] < child_metric[best]
                        || (child_metric[index] == child_metric[best]
                            && child_bits[index] > child_bits[best]))
                {
                    best = index;
                }
            }
            let chosen_root = (child_bits[best] >> (ECBEAM2_HORIZON - 1)) & 1;
            for index in 0..CHILDREN {
                if child_feasible[index]
                    && ((child_bits[index] >> (ECBEAM2_HORIZON - 1)) & 1) == chosen_root
                {
                    insert_ecbeam2_top4(
                        index as u8,
                        &child_metric,
                        &child_bits,
                        &mut order,
                        &mut order_len,
                    );
                }
            }
        } else {
            for index in 0..CHILDREN {
                if child_feasible[index] {
                    insert_ecbeam2_top4(
                        index as u8,
                        &child_metric,
                        &child_bits,
                        &mut order,
                        &mut order_len,
                    );
                }
            }
        }
        if order_len != ECBEAM2_WIDTH {
            return SimdSteadyStep::NotHandled;
        }

        if self.consecutive_constraint_escapes != 0 {
            self.consecutive_constraint_escapes = 0;
        }
        if self.consecutive_state_repairs != 0 {
            self.consecutive_state_repairs = 0;
        }

        let materialize_bank = parent_bank ^ 1;
        let mut selected_parents = [0usize; ECBEAM2_WIDTH];
        let mut selected_v = [0.0; ECBEAM2_WIDTH];
        let mut errors = [0.0; ECBEAM2_WIDTH];
        for slot in 0..ECBEAM2_WIDTH {
            let child = order[slot] as usize;
            let parent = child >> 1;
            let v = if child & 1 == 0 { 1.0 } else { -1.0 };
            selected_parents[slot] = parent;
            selected_v[slot] = v;
            errors[slot] = v - u;
        }

        let reconstruction_profile = self.reconstruction_profile;
        if parent_bank == 0 {
            let (source, destination) = self.simd_reconstruction_state.split_at_mut(1);
            let reconstruction_finite = reconstruction_profile.next_state4_selected(
                &source[0],
                selected_parents,
                errors,
                &mut destination[0],
            );
            if !reconstruction_finite.into_iter().all(|finite| finite) {
                return SimdSteadyStep::RecoverNonfinite;
            }
        } else {
            let (destination, source) = self.simd_reconstruction_state.split_at_mut(1);
            let reconstruction_finite = reconstruction_profile.next_state4_selected(
                &source[0],
                selected_parents,
                errors,
                &mut destination[0],
            );
            if !reconstruction_finite.into_iter().all(|finite| finite) {
                return SimdSteadyStep::RecoverNonfinite;
            }
        }

        // Materialize selected CRFB children across survivor lanes. Each lane
        // still rounds base*limit before the feedback FMA, exactly matching
        // `denormalized_feedback8` in the scalar reference path.
        unsafe {
            for offset in [0usize, 2] {
                let p0 = selected_parents[offset];
                let p1 = selected_parents[offset + 1];
                let v = vld1q_f64(selected_v.as_ptr().add(offset));
                for stage in 0..8 {
                    let base = vsetq_lane_f64::<1>(
                        self.simd_base_norm[stage][p1],
                        vdupq_n_f64(self.simd_base_norm[stage][p0]),
                    );
                    let raw_base = vmulq_n_f64(base, self.core.state_limit8[stage]);
                    let state = vfmaq_n_f64(raw_base, v, self.core.bv[stage]);
                    vst1q_f64(
                        self.simd_raw_state[materialize_bank][stage]
                            .as_mut_ptr()
                            .add(offset),
                        state,
                    );
                }
            }
        }
        self.record_simd_reconstruction_generation(materialize_bank, selected_parents);
        let mut minimum_metric = child_metric[order[0] as usize];
        for slot in 1..ECBEAM2_WIDTH {
            minimum_metric = minimum_metric.min(child_metric[order[slot] as usize]);
        }
        for slot in 0..ECBEAM2_WIDTH {
            let child = order[slot] as usize;
            self.simd_metric[materialize_bank][slot] = child_metric[child] - minimum_metric;
            self.simd_prev_v[materialize_bank][slot] = selected_v[slot];
            self.simd_bits[materialize_bank][slot] = child_bits[child];
        }

        self.parents_bank = materialize_bank;
        self.parents_len = ECBEAM2_WIDTH;
        self.push_buffered_input(u);
        if committing {
            let parent_bank = self.parents_bank;
            let bit = (self.simd_bits[parent_bank][0] >> (ECBEAM2_HORIZON - 1)) & 1;
            if self.committed_reconstruction_from_simd_ancestry() {
                self.remove_oldest_buffered_input();
                SimdSteadyStep::Emitted(bit)
            } else {
                SimdSteadyStep::CommitSlow
            }
        } else {
            SimdSteadyStep::Handled
        }
    }

    /// Fixed A0/M4/N8 playback kernel. Eligibility has already excluded every
    /// optional objective, budget, observer, and diagnostic branch, allowing
    /// the frontier to use narrow parallel scratch instead of constructing and
    /// sorting the general path's large `Child` records.
    #[cfg(target_arch = "aarch64")]
    #[allow(clippy::needless_range_loop)]
    #[inline(always)]
    fn process_frontier_sample_simd<const FAST_FEASIBLE: bool>(
        &mut self,
        u: f64,
        out_bits: &mut Vec<u8>,
    ) {
        const CHILDREN: usize = 2 * ECBEAM2_WIDTH;
        if !u.is_finite() {
            self.sync_simd_paths();
            self.emit_buffered_best(out_bits);
            self.core.hard_reset();
            self.core.stability_resets = self.core.stability_resets.wrapping_add(1);
            self.diagnostics.all_nonfinite_resets =
                self.diagnostics.all_nonfinite_resets.wrapping_add(1);
            self.diagnostics.invalid_input_substitutions =
                self.diagnostics.invalid_input_substitutions.wrapping_add(1);
            self.commit_bit(1, 0.0, out_bits);
            self.reseed_from_core();
            return;
        }

        if FAST_FEASIBLE {
            match self.try_process_feasible_m4_simd(u) {
                SimdSteadyStep::Handled => return,
                SimdSteadyStep::Emitted(bit) => {
                    self.commit_bit_simd_lean(bit, out_bits);
                    return;
                }
                SimdSteadyStep::RecoverNonfinite => {
                    self.recover_nonfinite_frontier(u, out_bits);
                    return;
                }
                SimdSteadyStep::CommitSlow => {
                    self.commit_oldest_simd(out_bits);
                    return;
                }
                SimdSteadyStep::NotHandled => {}
            }
        } else {
            self.prepare_simd_frontier(u);
        }
        let parent_bank = self.parents_bank;
        let mut child_parent = [0u8; CHILDREN];
        let mut child_v = [0.0f64; CHILDREN];
        let mut child_bits = [0u8; CHILDREN];
        let mut child_metric = [0.0; CHILDREN];
        let mut child_maximum_overflow = [0.0; CHILDREN];
        let mut child_squared_overflow = [0.0; CHILDREN];
        let mut child_state_feasible = [false; CHILDREN];
        let mut child_count = 0usize;

        for parent_index in 0..self.parents_len {
            let parent_bits = self.simd_bits[parent_bank][parent_index];
            for (sign, v) in [1.0f64, -1.0].into_iter().enumerate() {
                let maximum_overflow = self.simd_maximum_overflow[sign][parent_index];
                if !self.simd_candidate_finite[sign][parent_index] {
                    continue;
                }
                let state_feasible = maximum_overflow == 0.0;
                let metric = self.simd_candidate_metric[sign][parent_index];
                child_parent[child_count] = parent_index as u8;
                child_v[child_count] = v;
                child_bits[child_count] = (parent_bits << 1) | u8::from(v > 0.0);
                child_metric[child_count] = metric;
                child_maximum_overflow[child_count] = maximum_overflow;
                child_state_feasible[child_count] = state_feasible;
                child_count += 1;
            }
        }

        if child_count == 0 {
            self.recover_nonfinite_frontier(u, out_bits);
            return;
        }
        let has_state_feasible = child_state_feasible[..child_count]
            .iter()
            .any(|&value| value);
        let committing = self.buffered + 1 == ECBEAM2_HORIZON;
        let mut order = [0u8; ECBEAM2_WIDTH];
        let mut order_len = 0usize;
        if has_state_feasible {
            if committing {
                let mut best = child_state_feasible[..child_count]
                    .iter()
                    .position(|&feasible| feasible)
                    .expect("EcBeam2 feasible frontier must contain a winner");
                for index in best + 1..child_count {
                    if child_state_feasible[index]
                        && (child_metric[index] < child_metric[best]
                            || (child_metric[index] == child_metric[best]
                                && child_bits[index] > child_bits[best]))
                    {
                        best = index;
                    }
                }
                let chosen_root = (child_bits[best] >> (ECBEAM2_HORIZON - 1)) & 1;
                for index in 0..child_count {
                    if child_state_feasible[index]
                        && ((child_bits[index] >> (ECBEAM2_HORIZON - 1)) & 1) == chosen_root
                    {
                        insert_ecbeam2_top4(
                            index as u8,
                            &child_metric,
                            &child_bits,
                            &mut order,
                            &mut order_len,
                        );
                    }
                }
            } else {
                for index in 0..child_count {
                    if !child_state_feasible[index] {
                        continue;
                    }
                    insert_ecbeam2_top4(
                        index as u8,
                        &child_metric,
                        &child_bits,
                        &mut order,
                        &mut order_len,
                    );
                }
            }
        } else {
            self.prepare_simd_squared_overflow();
            for index in 0..child_count {
                let sign = usize::from(child_v[index] < 0.0);
                child_squared_overflow[index] =
                    self.simd_squared_overflow[sign][child_parent[index] as usize];
            }
            let mut best = 0usize;
            for index in 1..child_count {
                let overflow = (child_maximum_overflow[index], child_squared_overflow[index]);
                let best_overflow = (child_maximum_overflow[best], child_squared_overflow[best]);
                if overflow < best_overflow
                    || (overflow == best_overflow
                        && (child_metric[index] < child_metric[best]
                            || (child_metric[index] == child_metric[best]
                                && child_bits[index] > child_bits[best])))
                {
                    best = index;
                }
            }
            order[0] = best as u8;
            order_len = 1;
        }

        if has_state_feasible {
            if self.consecutive_constraint_escapes != 0 {
                self.consecutive_constraint_escapes = 0;
            }
            if self.consecutive_state_repairs != 0 {
                self.consecutive_state_repairs = 0;
            }
        } else {
            let frontier_sequence = self
                .diagnostics
                .committed_sequence
                .wrapping_add(self.buffered as u64);
            order_len = 1;
            let selected = order[0] as usize;
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
            let parent = child_parent[selected] as usize;
            let v = child_v[selected];
            for stage in 0..7 {
                let normalized =
                    v.mul_add(self.core.bv_norm[stage], self.simd_base_norm[stage][parent]);
                if normalized.abs() > 1.0 {
                    self.diagnostics.state_repair_stage_counts[stage] =
                        self.diagnostics.state_repair_stage_counts[stage].wrapping_add(1);
                }
            }
        }

        let materialize_bank = parent_bank ^ 1;
        let keep = order_len.min(ECBEAM2_WIDTH);
        let mut selected_parents = [0usize; ECBEAM2_WIDTH];
        let mut errors = [0.0; ECBEAM2_WIDTH];
        for slot in 0..keep {
            let child = order[slot] as usize;
            let parent = child_parent[child] as usize;
            selected_parents[slot] = parent;
            errors[slot] = child_v[child] - u;
        }
        let reconstruction_profile = self.reconstruction_profile;
        if parent_bank == 0 {
            let (source, destination) = self.simd_reconstruction_state.split_at_mut(1);
            let reconstruction_finite = reconstruction_profile.next_state4_selected(
                &source[0],
                selected_parents,
                errors,
                &mut destination[0],
            );
            if reconstruction_finite[..keep].iter().any(|&finite| !finite) {
                self.recover_nonfinite_frontier(u, out_bits);
                return;
            }
        } else {
            let (destination, source) = self.simd_reconstruction_state.split_at_mut(1);
            let reconstruction_finite = reconstruction_profile.next_state4_selected(
                &source[0],
                selected_parents,
                errors,
                &mut destination[0],
            );
            if reconstruction_finite[..keep].iter().any(|&finite| !finite) {
                self.recover_nonfinite_frontier(u, out_bits);
                return;
            }
        }
        for slot in 0..keep {
            let child = order[slot] as usize;
            let parent = child_parent[child] as usize;
            let base_norm: [f64; 8] =
                core::array::from_fn(|stage| self.simd_base_norm[stage][parent]);
            let mut state = denormalized_feedback8(
                &base_norm,
                &self.core.state_limit8,
                &self.core.bv,
                child_v[child],
            );
            if !child_state_feasible[child] {
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
            for stage in 0..8 {
                self.simd_raw_state[materialize_bank][stage][slot] = state[stage];
            }
            self.simd_metric[materialize_bank][slot] = child_metric[child];
            self.simd_prev_v[materialize_bank][slot] = child_v[child];
            self.simd_bits[materialize_bank][slot] = child_bits[child];
        }
        self.parents_bank = materialize_bank;
        self.parents_len = keep;
        self.record_simd_reconstruction_generation(materialize_bank, selected_parents);
        self.push_buffered_input(u);
        if self.buffered == ECBEAM2_HORIZON {
            self.commit_oldest_simd(out_bits);
        }
        self.renormalize_metrics_simd();
    }

    /// Commit an already root-conditioned lean SIMD frontier. The selector
    /// guarantees every retained path has the chosen oldest bit, so replaying
    /// the scalar root filter would only update read-only pruning telemetry.
    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn commit_oldest_simd(&mut self, out_bits: &mut Vec<u8>) {
        let parent_bank = self.parents_bank;
        let bit = (self.simd_bits[parent_bank][0] >> (ECBEAM2_HORIZON - 1)) & 1;
        let input = self.buffered_input(0);
        if self.committed_reconstruction_from_simd_ancestry() {
            self.commit_bit_simd_lean(bit, out_bits);
        } else {
            self.commit_bit(bit, input, out_bits);
        }

        // The dedicated selector has already discarded the losing root and
        // materialized the surviving paths in `parent_bank`. Unlike the
        // general scalar commit, there is nothing left to compact or mutate;
        // keeping this bank avoids copying every CRFB/profile lane once per
        // emitted DSD sample.
        self.remove_oldest_buffered_input();
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn commit_bit_simd_lean(&mut self, bit: u8, out_bits: &mut Vec<u8>) {
        debug_assert!(self.simd_m4n8_eligible());
        out_bits.push(bit);
        self.diagnostics.committed_samples = self.diagnostics.committed_samples.wrapping_add(1);
        self.diagnostics.positive_bits = self
            .diagnostics
            .positive_bits
            .wrapping_add(u64::from(bit == 1));
        self.diagnostics.committed_sequence = self.diagnostics.committed_sequence.wrapping_add(1);
    }

    /// Expand one input through the existing transition, feasibility,
    /// objective, ordering, and materialization path. R0 calls this once per
    /// real input; R1 deliberately calls it for every depth of each fresh
    /// receding-horizon search.
    #[allow(clippy::needless_range_loop)]
    fn process_frontier_sample<const SIMD: bool>(&mut self, u: f64, out_bits: &mut Vec<u8>) {
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

        #[cfg(target_arch = "aarch64")]
        if SIMD {
            self.prepare_simd_frontier(u);
        }

        let parent_bank = self.parents_bank;
        let mut children = [Child::INERT; ECBEAM2_MAX_CHILDREN];
        // The lean SIMD path reuses the already-stabilized candidate for a
        // retained winner. The scalar reference deliberately keeps its
        // historical rematerialization path as the behavior oracle.
        let mut candidate_states = [[0.0; 8]; ECBEAM2_MAX_CHILDREN];
        // 0 = feasible/no action, 1 = stabilized/clamped, 2 = reset.
        let mut candidate_stability = [0u8; ECBEAM2_MAX_CHILDREN];
        let mut child_count = 0usize;
        let one_minus_beta = 1.0 - self.ema_beta_10ms;
        for parent_index in 0..self.parents_len {
            let parent = self.parents[parent_bank][parent_index];
            let (state_norm, base_norm, y) =
                self.parent_prediction::<SIMD>(parent_index, parent, u);
            let parent_state_potential = if SIMD {
                0.0
            } else {
                normalized_state_potential(&state_norm)
            };
            for (sign_index, v) in [1.0, -1.0].into_iter().enumerate() {
                self.diagnostics.transition_evaluations =
                    self.diagnostics.transition_evaluations.wrapping_add(1);
                let e = v - u;
                let mut candidate_state =
                    denormalized_feedback8(&base_norm, &self.core.state_limit8, &self.core.bv, v);
                let reconstruction_delta = if SIMD {
                    #[cfg(target_arch = "aarch64")]
                    {
                        self.simd_reconstruction_increment[sign_index][parent_index]
                    }
                    #[cfg(not(target_arch = "aarch64"))]
                    {
                        unreachable!("EcBeam2 SIMD is available only on AArch64")
                    }
                } else {
                    self.reconstruction_profile
                        .tail_adjusted_energy_increment(&parent.reconstruction_state, e)
                };
                let ultrasonic_output = if SIMD {
                    #[cfg(target_arch = "aarch64")]
                    {
                        self.simd_ultrasonic_output[sign_index][parent_index]
                    }
                    #[cfg(not(target_arch = "aarch64"))]
                    {
                        unreachable!("EcBeam2 SIMD is available only on AArch64")
                    }
                } else {
                    self.ultrasonic_profile.output(&parent.ultrasonic_state, e)
                };
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
                    .map(|budget| relative_budget_violation(ultrasonic_ema, budget))
                    .unwrap_or(0.0);
                let signed_violation = self
                    .config
                    .signed_error_budget
                    .map(|budget| relative_budget_violation(signed_error_ema.abs(), budget))
                    .unwrap_or(0.0);
                let budget_violation = ultrasonic_violation.max(signed_violation);
                let state_feasible = maximum_state_overflow == 0.0;
                candidate_stability[child_count] = 0;
                if !state_feasible {
                    match stabilize_state(
                        &mut candidate_state,
                        &self.core.coeffs.state_limit,
                        &self.core.inverse_state_limit,
                    ) {
                        StateStability::Ok { clamped } => {
                            candidate_stability[child_count] = u8::from(clamped);
                        }
                        StateStability::Reset => {
                            candidate_state = [0.0; 8];
                            candidate_stability[child_count] = 2;
                        }
                    }
                }
                let (state_terminal_delta, state_barrier_raw) = if SIMD {
                    // Eligibility requires both state terms to carry zero
                    // weight. Preserve their two +0 additions below without
                    // evaluating read-only diagnostic values.
                    (0.0, 0.0)
                } else {
                    let effective_state_norm =
                        mul8(&candidate_state, &self.core.inverse_state_limit);
                    (
                        normalized_state_potential(&effective_state_norm) - parent_state_potential,
                        normalized_state_barrier(&effective_state_norm, self.config.state_deadzone),
                    )
                };
                let quantizer_error_squared = (y - v).powi(2);
                if !quantizer_error_squared.is_finite() {
                    continue;
                }
                let quantizer_path_increment =
                    self.config.quantizer_regularizer * quantizer_error_squared;
                let path_objective_increment = if SIMD {
                    let mut increment =
                        reconstruction_delta / self.objective_scales.reconstruction_abs_p95;
                    increment += 0.0;
                    increment += 0.0;
                    increment + quantizer_path_increment
                } else {
                    reconstruction_delta / self.objective_scales.reconstruction_abs_p95
                        + self.config.state_terminal_weight * state_terminal_delta
                            / self.objective_scales.state_terminal_abs_p95
                        + self.config.state_deadzone_weight * state_barrier_raw
                            / self.objective_scales.state_barrier_p95
                        + quantizer_path_increment
                };
                let metric = parent.metric + path_objective_increment;
                let rank_metric = metric;
                if !metric.is_finite() || !rank_metric.is_finite() {
                    continue;
                }
                children[child_count] = Child {
                    parent: parent_index as u8,
                    v,
                    bits: (parent.bits << 1) | u8::from(v > 0.0),
                    metric,
                    rank_metric,
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
                    path_objective_increment,
                    state_terminal_delta,
                    state_barrier_raw,
                    quantizer_error_squared,
                };
                if SIMD {
                    candidate_states[child_count] = candidate_state;
                }
                child_count += 1;
            }
        }

        if child_count == 0 {
            self.recover_nonfinite_frontier(u, out_bits);
            return;
        }

        let has_fully_feasible = children[..child_count]
            .iter()
            .any(|child| child.state_feasible && child.budgets_feasible);
        let has_state_feasible = children[..child_count]
            .iter()
            .any(|child| child.state_feasible);
        let mut order = [0u8; ECBEAM2_MAX_CHILDREN];
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
            let best_metric = children[order[0] as usize].rank_metric;
            let fourth_metric = children[order[ECBEAM2_WIDTH - 1] as usize].rank_metric;
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
        if self.diagnostic_sequence_selected(frontier_sequence) {
            self.reconstruction_increment_scale
                .observe(selected.reconstruction_increment);
            self.state_terminal_delta_scale
                .observe(selected.state_terminal_delta);
            self.state_barrier_raw_scale
                .observe(selected.state_barrier_raw);
            self.quantizer_error_squared_scale
                .observe(selected.quantizer_error_squared);
        }
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

        let committing = self.buffered + 1 == ECBEAM2_HORIZON;
        if committing {
            let (compatible_len, chosen_bit, old_top_m_prunes) =
                Self::condition_child_order_for_commit(
                    &children,
                    &mut order,
                    order_len,
                    self.beam_width(),
                );
            let _ = chosen_bit;
            order_len = compatible_len;
            self.diagnostics.pruned_total =
                self.diagnostics.pruned_total.wrapping_add(old_top_m_prunes);
        }
        let materialize_bank = parent_bank ^ 1;
        let keep = order_len.min(self.beam_width());
        #[cfg(target_arch = "aarch64")]
        let (simd_reconstruction_next, simd_ultrasonic_next) = if SIMD {
            let mut reconstruction_parents = [[0.0; ECBEAM2_WIDTH]; MAX_BEAM_ERROR_PROFILE_STATES];
            let mut ultrasonic_parents = [[0.0; ECBEAM2_WIDTH]; MAX_BEAM_ERROR_PROFILE_STATES];
            let mut errors = [0.0; ECBEAM2_WIDTH];
            for slot in 0..keep {
                let child = children[order[slot] as usize];
                let parent = child.parent as usize;
                errors[slot] = child.v - u;
                for stage in 0..MAX_BEAM_ERROR_PROFILE_STATES {
                    reconstruction_parents[stage][slot] =
                        self.simd_reconstruction_state[parent_bank][stage][parent];
                    ultrasonic_parents[stage][slot] =
                        self.simd_ultrasonic_state[parent_bank][stage][parent];
                }
            }
            (
                self.reconstruction_profile
                    .next_state4(&reconstruction_parents, errors),
                self.ultrasonic_profile
                    .next_state4(&ultrasonic_parents, errors),
            )
        } else {
            (
                [[0.0; ECBEAM2_WIDTH]; MAX_BEAM_ERROR_PROFILE_STATES],
                [[0.0; ECBEAM2_WIDTH]; MAX_BEAM_ERROR_PROFILE_STATES],
            )
        };
        for slot in 0..keep {
            let child_index = order[slot] as usize;
            let child = children[child_index];
            let parent = self.parents[parent_bank][child.parent as usize];
            let e = child.v - u;
            let mut state = if SIMD {
                candidate_states[child_index]
            } else {
                denormalized_feedback8(
                    &child.base_norm,
                    &self.core.state_limit8,
                    &self.core.bv,
                    child.v,
                )
            };
            if !child.state_feasible {
                if SIMD {
                    match candidate_stability[child_index] {
                        0 => {}
                        1 => {
                            self.core.state_clamps = self.core.state_clamps.wrapping_add(1);
                        }
                        2 => {
                            self.core.hard_reset();
                            state = self.core.state;
                            self.core.stability_resets = self.core.stability_resets.wrapping_add(1);
                        }
                        _ => unreachable!("invalid EcBeam2 candidate stability marker"),
                    }
                } else {
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
            }
            #[cfg(target_arch = "aarch64")]
            if SIMD {
                for stage in 0..8 {
                    self.simd_raw_state[materialize_bank][stage][slot] = state[stage];
                }
            }
            // These two transitions are already required to materialize a
            // retained survivor. Validate their complete stored state here,
            // rather than carrying both profile states for all eight children
            // through the hot expansion/ranking path.
            let reconstruction_state = if SIMD {
                #[cfg(target_arch = "aarch64")]
                {
                    core::array::from_fn(|stage| simd_reconstruction_next[stage][slot])
                }
                #[cfg(not(target_arch = "aarch64"))]
                {
                    unreachable!("EcBeam2 SIMD is available only on AArch64")
                }
            } else {
                self.reconstruction_profile
                    .next_state(&parent.reconstruction_state, e)
            };
            let ultrasonic_state = if SIMD {
                #[cfg(target_arch = "aarch64")]
                {
                    core::array::from_fn(|stage| simd_ultrasonic_next[stage][slot])
                }
                #[cfg(not(target_arch = "aarch64"))]
                {
                    unreachable!("EcBeam2 SIMD is available only on AArch64")
                }
            } else {
                self.ultrasonic_profile
                    .next_state(&parent.ultrasonic_state, e)
            };
            if reconstruction_state
                .iter()
                .chain(&ultrasonic_state)
                .any(|value| !value.is_finite())
            {
                self.recover_nonfinite_frontier(u, out_bits);
                return;
            }
            #[cfg(target_arch = "aarch64")]
            if SIMD {
                for stage in 0..MAX_BEAM_ERROR_PROFILE_STATES {
                    self.simd_reconstruction_state[materialize_bank][stage][slot] =
                        reconstruction_state[stage];
                    self.simd_ultrasonic_state[materialize_bank][stage][slot] =
                        ultrasonic_state[stage];
                }
            }
            self.parents[materialize_bank][slot] = EcBeam2Path {
                state,
                reconstruction_state,
                ultrasonic_state,
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
        self.observe_survivor_frontier_width();
        self.push_buffered_input(u);

        if self.buffered == ECBEAM2_HORIZON {
            let best = self.parents[self.parents_bank][0];
            self.record_segment_prediction(best, ECBEAM2_HORIZON);
            self.commit_oldest::<SIMD>(out_bits);
        }
        self.diagnostics.min_survivors =
            self.diagnostics.min_survivors.min(self.parents_len as u64);
        self.renormalize_metrics();
    }

    fn observe_survivor_frontier_width(&mut self) {
        if self.parents_len == self.beam_width() {
            self.survivor_initial_fill_complete = true;
        } else if self.survivor_initial_fill_complete {
            self.diagnostics.missing_survivor_events_after_initial_fill = self
                .diagnostics
                .missing_survivor_events_after_initial_fill
                .wrapping_add(1);
        }
    }

    fn sort_children(
        &self,
        children: &[Child; ECBEAM2_MAX_CHILDREN],
        order: &mut [u8],
        feasible: bool,
    ) {
        for sorted in 1..order.len() {
            let key = order[sorted];
            let mut position = sorted;
            while position > 0
                && self.child_before(
                    &children[key as usize],
                    &children[order[position - 1] as usize],
                    feasible,
                )
            {
                order[position] = order[position - 1];
                position -= 1;
            }
            order[position] = key;
        }
    }

    fn child_before(&self, left: &Child, right: &Child, feasible: bool) -> bool {
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
        left.rank_metric < right.rank_metric
            || (left.rank_metric == right.rank_metric && left.bits > right.bits)
    }

    /// Preserve the globally best child's root decision while filling the
    /// committing frontier from the complete ordered eligible set. The
    /// returned prune count deliberately matches the old top-M-then-commit
    /// diagnostic semantics even though incompatible children are no longer
    /// materialized first.
    fn condition_child_order_for_commit(
        children: &[Child; ECBEAM2_MAX_CHILDREN],
        order: &mut [u8],
        order_len: usize,
        beam_width: usize,
    ) -> (usize, u8, u64) {
        debug_assert!(order_len > 0 && order_len <= order.len());
        let chosen_bit = (children[order[0] as usize].bits >> (ECBEAM2_HORIZON - 1)) & 1;
        let old_top_m_prunes = order[..order_len.min(beam_width)]
            .iter()
            .filter(|&&index| {
                ((children[index as usize].bits >> (ECBEAM2_HORIZON - 1)) & 1) != chosen_bit
            })
            .count() as u64;

        let mut compatible_len = 0usize;
        for read in 0..order_len {
            let index = order[read];
            if ((children[index as usize].bits >> (ECBEAM2_HORIZON - 1)) & 1) == chosen_bit {
                order[compatible_len] = index;
                compatible_len += 1;
            }
        }
        debug_assert!(compatible_len > 0);
        (compatible_len, chosen_bit, old_top_m_prunes)
    }

    #[inline]
    fn persistent_path_before(left: EcBeam2Path, right: EcBeam2Path) -> bool {
        left.metric < right.metric || (left.metric == right.metric && left.bits > right.bits)
    }

    fn sort_parents_by_persistent_metric(&mut self) {
        let parents = &mut self.parents[self.parents_bank];
        for sorted in 1..self.parents_len {
            let key = parents[sorted];
            let mut position = sorted;
            while position > 0 && Self::persistent_path_before(key, parents[position - 1]) {
                parents[position] = parents[position - 1];
                position -= 1;
            }
            parents[position] = key;
        }
    }

    fn commit_oldest<const SIMD: bool>(&mut self, out_bits: &mut Vec<u8>) {
        #[cfg(target_arch = "aarch64")]
        let bit = if SIMD {
            (self.simd_bits[self.parents_bank][0] >> (ECBEAM2_HORIZON - 1)) & 1
        } else {
            (self.parents[self.parents_bank][0].bits >> (ECBEAM2_HORIZON - 1)) & 1
        };
        #[cfg(not(target_arch = "aarch64"))]
        let bit = (self.parents[self.parents_bank][0].bits >> (ECBEAM2_HORIZON - 1)) & 1;
        let input = self.buffered_input(0);
        self.commit_bit(bit, input, out_bits);

        let parent_bank = self.parents_bank;
        let compact_bank = parent_bank ^ 1;
        let mut kept = 0usize;
        for index in 0..self.parents_len {
            #[cfg(target_arch = "aarch64")]
            let parent_bits = if SIMD {
                self.simd_bits[parent_bank][index]
            } else {
                self.parents[parent_bank][index].bits
            };
            #[cfg(not(target_arch = "aarch64"))]
            let parent_bits = self.parents[parent_bank][index].bits;
            if ((parent_bits >> (ECBEAM2_HORIZON - 1)) & 1) == bit {
                #[cfg(target_arch = "aarch64")]
                if SIMD {
                    self.simd_metric[compact_bank][kept] = self.simd_metric[parent_bank][index];
                    self.simd_prev_v[compact_bank][kept] = self.simd_prev_v[parent_bank][index];
                    self.simd_bits[compact_bank][kept] = parent_bits;
                    for stage in 0..8 {
                        self.simd_raw_state[compact_bank][stage][kept] =
                            self.simd_raw_state[parent_bank][stage][index];
                    }
                    for stage in 0..MAX_BEAM_ERROR_PROFILE_STATES {
                        self.simd_reconstruction_state[compact_bank][stage][kept] =
                            self.simd_reconstruction_state[parent_bank][stage][index];
                        self.simd_ultrasonic_state[compact_bank][stage][kept] =
                            self.simd_ultrasonic_state[parent_bank][stage][index];
                    }
                } else {
                    self.parents[compact_bank][kept] = self.parents[parent_bank][index];
                }
                #[cfg(not(target_arch = "aarch64"))]
                {
                    self.parents[compact_bank][kept] = self.parents[parent_bank][index];
                }
                kept += 1;
            } else {
                self.diagnostics.pruned_total = self.diagnostics.pruned_total.wrapping_add(1);
            }
        }
        self.parents_bank = compact_bank;
        self.parents_len = kept;
        self.remove_oldest_buffered_input();
    }

    fn commit_bit(&mut self, bit: u8, u: f64, out_bits: &mut Vec<u8>) {
        let sequence = self.diagnostics.committed_sequence;
        let measure = self.diagnostic_sequence_selected(sequence);
        let v = if bit == 1 { 1.0 } else { -1.0 };
        let e = v - u;
        // With both optional budgets absent, the ultrasonic filter and its
        // EMAs are observational only: neither is read by recovery, ranking,
        // stabilization, or a later output decision. The committed
        // reconstruction state is retained because recovery reseeds the beam
        // from it and therefore can affect future bits.
        let decision_only_telemetry = !self.full_diagnostics
            && self.config.ultrasonic_budget.is_none()
            && self.config.signed_error_budget.is_none();
        let (starting_tail, output, instantaneous, increment, tail, ultrasonic) =
            if self.full_diagnostics {
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
                self.ultrasonic_profile
                    .advance(&mut self.committed_ultrasonic_state, e);
                (
                    starting_tail,
                    output,
                    instantaneous,
                    increment,
                    tail,
                    ultrasonic,
                )
            } else if decision_only_telemetry {
                #[cfg(target_arch = "aarch64")]
                {
                    self.committed_reconstruction_state = self
                        .reconstruction_profile
                        .next_state_row_pairs(&self.committed_reconstruction_state, e);
                }
                #[cfg(not(target_arch = "aarch64"))]
                {
                    self.committed_reconstruction_state = self
                        .reconstruction_profile
                        .next_state(&self.committed_reconstruction_state, e);
                }
                (0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
            } else {
                #[cfg(target_arch = "aarch64")]
                let ultrasonic = BeamErrorProfile::advance_profile_pair(
                    &self.reconstruction_profile,
                    &mut self.committed_reconstruction_state,
                    &self.ultrasonic_profile,
                    &mut self.committed_ultrasonic_state,
                    e,
                );
                #[cfg(not(target_arch = "aarch64"))]
                let ultrasonic = {
                    self.committed_reconstruction_state = self
                        .reconstruction_profile
                        .next_state(&self.committed_reconstruction_state, e);
                    let output = self
                        .ultrasonic_profile
                        .output(&self.committed_ultrasonic_state, e);
                    self.committed_ultrasonic_state = self
                        .ultrasonic_profile
                        .next_state(&self.committed_ultrasonic_state, e);
                    output
                };
                (0.0, 0.0, 0.0, 0.0, 0.0, ultrasonic)
            };
        let ultrasonic_power = ultrasonic * ultrasonic;
        let one_minus_10ms = 1.0 - self.ema_beta_10ms;
        if !decision_only_telemetry {
            self.committed_signed_error_ema = self
                .ema_beta_10ms
                .mul_add(self.committed_signed_error_ema, one_minus_10ms * e);
            self.committed_ultrasonic_ema = self.ema_beta_10ms.mul_add(
                self.committed_ultrasonic_ema,
                one_minus_10ms * ultrasonic_power,
            );
        }
        let signed_error_ema_abs = if self.full_diagnostics {
            let one_minus_1ms = 1.0 - self.ema_beta_1ms;
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
            self.committed_signed_error_ema.abs()
        } else {
            0.0
        };
        if measure {
            self.ultrasonic_ema_p999
                .observe(self.committed_ultrasonic_ema);
            self.ultrasonic_ema_p9999
                .observe(self.committed_ultrasonic_ema);
            self.signed_error_ema_p999.observe(signed_error_ema_abs);
            self.signed_error_ema_p9999.observe(signed_error_ema_abs);
        }

        out_bits.push(bit);
        if self.full_diagnostics {
            self.replayed_output_energy += instantaneous;
        }
        self.diagnostics.committed_samples = self.diagnostics.committed_samples.wrapping_add(1);
        self.diagnostics.positive_bits = self
            .diagnostics
            .positive_bits
            .wrapping_add(u64::from(bit == 1));
        if self.full_diagnostics {
            self.diagnostics.remaining_tail_energy = tail;
        }
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
            self.commit_bit(bit, self.buffered_input(index), out_bits);
        }
        self.buffered = 0;
        self.input_buffer = [0.0; ECBEAM2_HORIZON];
        self.input_head = 0;
    }

    fn record_segment_prediction(&mut self, best: EcBeam2Path, length: usize) {
        if !self.full_diagnostics || length == 0 || length > ECBEAM2_HORIZON {
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
            let error = v - self.buffered_input(index);
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
        if !self.full_diagnostics {
            return;
        }
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

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn renormalize_metrics_simd(&mut self) {
        use core::arch::aarch64::*;

        if self.parents_len == 0 {
            return;
        }
        let bank = self.parents_bank;
        let mut minimum = self.simd_metric[bank][0];
        for parent in 1..self.parents_len {
            minimum = minimum.min(self.simd_metric[bank][parent]);
        }
        // SAFETY: both stores cover the fixed four-lane metric array. The
        // scalar minimum scan above retains the reference selection order;
        // subtraction remains lane-local and bit-identical.
        unsafe {
            let minimum = vdupq_n_f64(minimum);
            for offset in [0usize, 2] {
                let metric = vld1q_f64(self.simd_metric[bank].as_ptr().add(offset));
                vst1q_f64(
                    self.simd_metric[bank].as_mut_ptr().add(offset),
                    vsubq_f64(metric, minimum),
                );
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
    use crate::audio::dsd::dsd_coeffs::{CRFB_OSR64_OBG164, CRFB_OSR64_OBG165};

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn ecbeam2_both_sign_certificate_implies_exact_candidate_feasibility() {
        let modulator = EcBeam2Modulator::new_with_diagnostics(
            ecbeam2_dsd128_production_coefficients(),
            0xCE47_1F1C_A7E,
            DSD128_44K_FAMILY_WIRE_RATE,
            EcBeam2ExperimentConfig::default(),
            false,
        )
        .unwrap();
        let mut random = 0xA11C_E5AF_E123_4567u64;
        for stage in 0..7 {
            let boundary = modulator.simd_both_sign_safe_abs_base[stage];
            let feedback = modulator.core.bv_norm[stage];
            for sample in 0..4096 {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                let unit = (random >> 11) as f64 * (1.0 / ((1u64 << 53) as f64));
                let base = if sample == 0 {
                    boundary
                } else if sample == 1 {
                    -boundary
                } else {
                    (2.0 * unit - 1.0) * boundary
                };
                assert!(base.abs() <= boundary);
                for sign in [1.0f64, -1.0] {
                    let candidate = sign.mul_add(feedback, base);
                    assert!(candidate.is_finite());
                    assert!(candidate.abs() <= 1.0);
                }
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn ecbeam2_selected_profile_transition_is_bit_exact_and_reports_finiteness() {
        let modulator = EcBeam2Modulator::new_with_diagnostics(
            ecbeam2_dsd128_production_coefficients(),
            0xF1A1_7E,
            DSD128_44K_FAMILY_WIRE_RATE,
            EcBeam2ExperimentConfig::default(),
            false,
        )
        .unwrap();
        let profile = modulator.reconstruction_profile;
        let states: [[f64; ECBEAM2_WIDTH]; MAX_BEAM_ERROR_PROFILE_STATES] =
            core::array::from_fn(|stage| {
                core::array::from_fn(|lane| {
                    ((stage * ECBEAM2_WIDTH + lane) as f64 - 11.5) * 0.03125
                })
            });
        let selected = [3, 1, 0, 2];
        let errors = [0.75, -1.125, 1.375, -0.625];
        let mut next = [[0.0; ECBEAM2_WIDTH]; MAX_BEAM_ERROR_PROFILE_STATES];
        let finite = profile.next_state4_selected(&states, selected, errors, &mut next);
        assert_eq!(finite, [true; ECBEAM2_WIDTH]);
        for lane in 0..ECBEAM2_WIDTH {
            let parent = core::array::from_fn(|stage| states[stage][selected[lane]]);
            let expected = profile.next_state(&parent, errors[lane]);
            for stage in 0..MAX_BEAM_ERROR_PROFILE_STATES {
                assert_eq!(next[stage][lane].to_bits(), expected[stage].to_bits());
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn ecbeam2_simd_ancestry_commit_matches_direct_transition_bits() {
        let mut modulator = EcBeam2Modulator::new_with_diagnostics(
            ecbeam2_dsd128_production_coefficients(),
            0xA11C_E57A,
            DSD128_44K_FAMILY_WIRE_RATE,
            EcBeam2ExperimentConfig::default(),
            false,
        )
        .unwrap();
        assert!(modulator.simd_m4n8_eligible());
        let profile = modulator.reconstruction_profile;
        let input: Vec<f64> = (0..512)
            .map(|index| 0.31 * (index as f64 * 0.037).sin() + 0.13 * (index as f64 * 0.113).cos())
            .collect();
        let mut bits = Vec::new();
        let mut expected_state = [0.0; MAX_BEAM_ERROR_PROFILE_STATES];
        let mut emitted = 0usize;
        for &sample in &input {
            modulator.process_into_bits(&[sample], &mut bits);
            while emitted < bits.len() {
                let v = if bits[emitted] == 1 { 1.0 } else { -1.0 };
                expected_state = profile.next_state(&expected_state, v - input[emitted]);
                for stage in 0..MAX_BEAM_ERROR_PROFILE_STATES {
                    assert_eq!(
                        modulator.committed_reconstruction_state[stage].to_bits(),
                        expected_state[stage].to_bits(),
                        "emitted sample {emitted}, stage {stage}"
                    );
                }
                emitted += 1;
            }
        }
        assert!(emitted > ECBEAM2_HORIZON);
    }

    #[test]
    fn ecbeam2_rejects_unsupported_wire_rates_and_shadow_mode() {
        assert!(
            EcBeam2Modulator::new(
                &CRFB_OSR64_OBG164,
                1,
                12_288_000,
                EcBeam2ExperimentConfig::default(),
            )
            .is_err()
        );
        assert!(
            EcBeam2Modulator::new(
                &CRFB_OSR64_OBG164,
                1,
                2_822_400,
                EcBeam2ExperimentConfig {
                    run_mode: EcBeam2RunMode::ShadowA1,
                    ..EcBeam2ExperimentConfig::default()
                },
            )
            .is_err()
        );

        let mut wrong_coefficients = CRFB_OSR64_OBG164;
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
            "EcBeam2 requires a registered production or legacy-oracle coefficient table"
        );

        for (coefficients, wire_rate) in [
            (&CRFB_OSR64_OBG164, 2_822_400),
            (&ECBEAM2_OSR128_OBG164_INPUT468_V1, 5_644_800),
            (&CRFB_OSR64_OBG165, 2_822_400),
        ] {
            assert!(
                EcBeam2Modulator::new(
                    coefficients,
                    1,
                    wire_rate,
                    EcBeam2ExperimentConfig::default(),
                )
                .is_ok(),
                "registered coefficient table should construct"
            );
        }
    }

    #[test]
    fn ecbeam2_dsd128_production_coefficients_preserve_obg_and_scale_limits() {
        let coefficients = ecbeam2_dsd128_production_coefficients();
        assert_eq!(coefficients.a, CRFB_OSR128_OBG164.a);
        assert_eq!(coefficients.b, CRFB_OSR128_OBG164.b);
        assert_eq!(coefficients.c, CRFB_OSR128_OBG164.c);
        assert_eq!(coefficients.d1, CRFB_OSR128_OBG164.d1);
        assert_eq!(coefficients.obg, 1.64);
        assert_eq!(coefficients.osr, 128);
        assert_eq!(coefficients.input_peak, ECBEAM2_OSR128_OBG164_INPUT468);
    }

    #[test]
    fn ecbeam2_nonfinite_materialized_profile_state_fails_closed_before_storage() {
        let mut modulator = EcBeam2Modulator::new(
            EcBeam2PlantId::default().coefficients(),
            0x50_7A_7E,
            2_822_400,
            EcBeam2ExperimentConfig::default(),
        )
        .unwrap();

        // This synthetic, finite parent lies outside the reachable operating
        // envelope. Its hot objective increment remains finite for both bits,
        // while one profile recurrence row overflows during materialization.
        // It therefore exercises the exact gap between ranking checks and the
        // state that would previously have been stored in the next frontier.
        let scale = 0.91 * f64::MAX;
        let finite_parent = [scale, -scale, -scale, -scale, -scale, scale];
        assert!(finite_parent.iter().all(|value| value.is_finite()));
        for error in [0.0, -2.0] {
            assert!(
                modulator
                    .reconstruction_profile
                    .tail_adjusted_energy_increment(&finite_parent, error)
                    .is_finite()
            );
            assert!(
                modulator
                    .reconstruction_profile
                    .next_state(&finite_parent, error)
                    .iter()
                    .any(|value| !value.is_finite())
            );
        }
        modulator.parents[modulator.parents_bank][0].reconstruction_state = finite_parent;

        let mut bits = Vec::new();
        modulator.process_into_bits(&[1.0], &mut bits);

        assert_eq!(bits, [1]);
        assert_eq!(modulator.diagnostics().all_nonfinite_resets, 1);
        assert_eq!(modulator.buffered, 0);
        assert_eq!(modulator.parents_len, 1);
        assert!(
            modulator.parents[modulator.parents_bank][..modulator.parents_len]
                .iter()
                .flat_map(|parent| {
                    parent
                        .reconstruction_state
                        .iter()
                        .chain(&parent.ultrasonic_state)
                })
                .all(|value| value.is_finite())
        );
        assert!(
            modulator
                .committed_reconstruction_state
                .iter()
                .chain(&modulator.committed_ultrasonic_state)
                .all(|value| value.is_finite())
        );
    }

    #[test]
    fn ecbeam2_retained_profile_state_checks_preserve_valid_path_bits() {
        let input = [
            0.0, 0.125, -0.125, 0.25, -0.25, 0.375, -0.375, 0.5, -0.5, 0.25, -0.125, 0.0625, 0.0,
            -0.0625, 0.125, -0.25, 0.375, -0.5, 0.375, -0.25, 0.125, -0.0625, 0.0, 0.0625,
        ];
        let mut modulator = EcBeam2Modulator::new(
            EcBeam2PlantId::default().coefficients(),
            0x50_7A_7E,
            2_822_400,
            EcBeam2ExperimentConfig::default(),
        )
        .unwrap();
        let mut bits = Vec::new();
        modulator.process_into_bits(&input, &mut bits);
        modulator.flush_into_bits(&mut bits);

        assert_eq!(
            bits,
            [
                1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0,
            ]
        );
        assert_eq!(modulator.diagnostics().all_nonfinite_resets, 0);
    }

    #[test]
    fn diagnostic_window_excludes_frontier_objective_scale_trackers() {
        let mut modulator = EcBeam2Modulator::new(
            EcBeam2PlantId::default().coefficients(),
            0xD_1A65_CA1E,
            2_822_400,
            EcBeam2ExperimentConfig {
                diagnostic_window: Some(EcBeam2DiagnosticWindow {
                    start_sequence: 10_000,
                    end_sequence: 10_100,
                }),
                ..EcBeam2ExperimentConfig::default()
            },
        )
        .unwrap();
        let input: Vec<f64> = (0..128)
            .map(|index| 0.17 * (index as f64 * 0.071).sin())
            .collect();
        let mut bits = Vec::new();
        modulator.process_into_bits(&input, &mut bits);
        modulator.flush_into_bits(&mut bits);

        let diagnostics = modulator.diagnostics();
        assert_eq!(diagnostics.diagnostic_window_samples, 0);
        assert_eq!(
            diagnostics.reconstruction_increment_scale,
            EcBeam2ScaleDistribution::default()
        );
        assert_eq!(
            diagnostics.state_terminal_delta_scale,
            EcBeam2ScaleDistribution::default()
        );
        assert_eq!(
            diagnostics.state_barrier_raw_scale,
            EcBeam2ScaleDistribution::default()
        );
        assert_eq!(
            diagnostics.quantizer_error_squared_scale,
            EcBeam2ScaleDistribution::default()
        );
    }

    #[test]
    fn ecbeam2_is_chunk_and_flush_invariant() {
        let input: Vec<f64> = (0..4096)
            .map(|index| 0.2 * (index as f64 * 0.013).sin())
            .collect();
        let run = |chunks: bool| {
            let mut modulator = EcBeam2Modulator::new(
                EcBeam2PlantId::default().coefficients(),
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
    fn ecbeam2_full_lean_scalar_and_neon_are_bit_exact() {
        fn assert_bits(reference: &[u8], candidate: &[u8], context: &str) {
            if let Some(index) = reference
                .iter()
                .zip(candidate)
                .position(|(left, right)| left != right)
            {
                panic!(
                    "{context}: first differing emitted sample {index}: reference={} candidate={}",
                    reference[index], candidate[index]
                );
            }
            assert_eq!(
                reference.len(),
                candidate.len(),
                "{context}: emitted length"
            );
        }

        fn packed_digest(bits: &[u8]) -> u64 {
            let mut digest = 0xcbf29ce484222325u64;
            for chunk in bits.chunks(8) {
                let mut byte = 0u8;
                for (index, &bit) in chunk.iter().enumerate() {
                    byte |= bit << (7 - index);
                }
                digest ^= u64::from(byte);
                digest = digest.wrapping_mul(0x100000001b3);
            }
            digest
        }

        fn assert_decision_state(
            reference: &EcBeam2Modulator,
            candidate: &EcBeam2Modulator,
            context: &str,
        ) {
            assert_eq!(reference.parents_len, candidate.parents_len, "{context}");
            for parent in 0..reference.parents_len {
                let left = &reference.parents[reference.parents_bank][parent];
                let right = &candidate.parents[candidate.parents_bank][parent];
                assert_eq!(
                    left.state, right.state,
                    "{context}, parent {parent}, CRFB state"
                );
                assert_eq!(
                    left.reconstruction_state, right.reconstruction_state,
                    "{context}, parent {parent}, reconstruction state"
                );
                assert_eq!(
                    left.metric, right.metric,
                    "{context}, parent {parent}, metric"
                );
                assert_eq!(
                    left.prev_v, right.prev_v,
                    "{context}, parent {parent}, prev_v"
                );
                assert_eq!(left.bits, right.bits, "{context}, parent {parent}, history");
            }
            assert_eq!(reference.buffered, candidate.buffered, "{context}");
            assert_eq!(reference.input_buffer, candidate.input_buffer, "{context}");
            assert_eq!(reference.input_head, candidate.input_head, "{context}");
        }

        fn assert_final_state(
            reference: &EcBeam2Modulator,
            candidate: &EcBeam2Modulator,
            context: &str,
        ) {
            assert_eq!(
                reference.core.state, candidate.core.state,
                "{context}, core state"
            );
            assert_eq!(
                reference.core.prev_v, candidate.core.prev_v,
                "{context}, core prev_v"
            );
            assert_eq!(
                reference.committed_reconstruction_state, candidate.committed_reconstruction_state,
                "{context}, committed reconstruction state"
            );
            assert_eq!(
                reference.stability_resets(),
                candidate.stability_resets(),
                "{context}"
            );
            assert_eq!(
                reference.state_clamps(),
                candidate.state_clamps(),
                "{context}"
            );
            assert_eq!(
                reference.diagnostics.constraint_escape, candidate.diagnostics.constraint_escape,
                "{context}, constraint escapes"
            );
            assert_eq!(
                reference.diagnostics.state_repair_fallback,
                candidate.diagnostics.state_repair_fallback,
                "{context}, state repairs"
            );
            assert_eq!(
                reference.diagnostics.all_nonfinite_resets,
                candidate.diagnostics.all_nonfinite_resets,
                "{context}, non-finite resets"
            );
            assert_eq!(
                reference.diagnostics.invalid_input_substitutions,
                candidate.diagnostics.invalid_input_substitutions,
                "{context}, invalid input substitutions"
            );
            assert_eq!(candidate.diagnostics.output_length_events, 0, "{context}");
        }

        let cases = [
            (&CRFB_OSR64_OBG164, 2_822_400, 0x8168_c3e9_364c_ace3, 1635),
            (&CRFB_OSR64_OBG164, 3_072_000, 0x70f1_1281_770d_c9ad, 1635),
            (
                ecbeam2_dsd128_production_coefficients(),
                5_644_800,
                0xdef1_df05_0202_6094,
                1635,
            ),
            (
                ecbeam2_dsd128_production_coefficients(),
                6_144_000,
                0xee56_43a6_2831_ff48,
                1635,
            ),
        ];
        let config = ecbeam2_production_config();
        let mut input = vec![0.0; 3072];
        input[256..512].fill(0.2);
        input[512] = 0.95;
        input[513] = -0.95;
        for (index, sample) in input[640..1024].iter_mut().enumerate() {
            *sample = 0.42 * (index as f64 * 0.003).sin();
        }
        for (index, sample) in input[1024..1408].iter_mut().enumerate() {
            *sample = 0.38 * (index as f64 * 2.91).sin();
        }
        for (index, sample) in input[1408..1792].iter_mut().enumerate() {
            let phase = index as f64;
            *sample = 0.24 * (phase * 0.013).sin()
                + 0.15 * (phase * 0.071).cos()
                + 0.08 * (phase * 0.37).sin();
        }
        for (index, sample) in input[1792..2176].iter_mut().enumerate() {
            *sample = if index & 1 == 0 { 0.92 } else { -0.92 };
        }
        input[2176..2304].fill(0.31);
        input[2304..2432].fill(0.0);
        let mut noise = 0xA5A5_5A5A_D3C1_B2E7u64;
        for sample in &mut input[2432..] {
            noise ^= noise << 13;
            noise ^= noise >> 7;
            noise ^= noise << 17;
            *sample = ((noise >> 11) as f64 / ((1u64 << 53) as f64) * 2.0 - 1.0) * 0.46;
        }
        input[257] = f64::NAN;
        input[700] = 3.0;
        input[2221] = f64::INFINITY;

        for (coefficients, wire_rate, reference_digest, reference_positive_bits) in cases {
            let mut full =
                EcBeam2Modulator::new(coefficients, 0xB17_EA7, wire_rate, config).unwrap();
            let mut lean_scalar = EcBeam2Modulator::new_with_diagnostics(
                coefficients,
                0xB17_EA7,
                wire_rate,
                config,
                false,
            )
            .unwrap();
            lean_scalar.force_scalar_path = true;
            let mut lean_neon = EcBeam2Modulator::new_with_diagnostics(
                coefficients,
                0xB17_EA7,
                wire_rate,
                config,
                false,
            )
            .unwrap();
            #[cfg(all(target_arch = "aarch64", not(feature = "ecbeam2_observer")))]
            assert!(lean_neon.simd_m4n8_eligible(), "wire rate {wire_rate}");
            let mut full_bits = Vec::new();
            let mut lean_scalar_bits = Vec::new();
            let mut lean_neon_bits = Vec::new();
            let chunks = [1usize, 3, 7, 8, 1024];
            let mut offset = 0usize;
            let mut chunk_index = 0usize;
            while offset < input.len() {
                let end = (offset + chunks[chunk_index % chunks.len()]).min(input.len());
                full.process_into_bits(&input[offset..end], &mut full_bits);
                lean_scalar.process_into_bits(&input[offset..end], &mut lean_scalar_bits);
                lean_neon.process_into_bits(&input[offset..end], &mut lean_neon_bits);
                let scalar_context = format!("wire rate {wire_rate}, input {end}, lean scalar");
                let neon_context = format!("wire rate {wire_rate}, input {end}, lean NEON");
                assert_bits(&full_bits, &lean_scalar_bits, &scalar_context);
                assert_bits(&full_bits, &lean_neon_bits, &neon_context);
                assert_decision_state(&full, &lean_scalar, &scalar_context);
                assert_decision_state(&full, &lean_neon, &neon_context);
                offset = end;
                chunk_index += 1;
            }
            full.flush_into_bits(&mut full_bits);
            lean_scalar.flush_into_bits(&mut lean_scalar_bits);
            lean_neon.flush_into_bits(&mut lean_neon_bits);
            let scalar_context = format!("wire rate {wire_rate}, flushed, lean scalar");
            let neon_context = format!("wire rate {wire_rate}, flushed, lean NEON");
            assert_bits(&full_bits, &lean_scalar_bits, &scalar_context);
            assert_bits(&full_bits, &lean_neon_bits, &neon_context);
            assert_final_state(&full, &lean_scalar, &scalar_context);
            assert_final_state(&full, &lean_neon, &neon_context);
            assert_eq!(
                full_bits.len(),
                input.len(),
                "wire rate {wire_rate}, bit count"
            );
            assert_eq!(
                packed_digest(&full_bits),
                reference_digest,
                "wire rate {wire_rate}, frozen packed reference digest"
            );
            assert_eq!(
                full_bits.iter().map(|&bit| u64::from(bit)).sum::<u64>(),
                reference_positive_bits,
                "wire rate {wire_rate}, frozen positive-bit count"
            );

            for tail in 0..=7 {
                full.reset();
                lean_scalar.reset();
                lean_neon.reset();
                full_bits.clear();
                lean_scalar_bits.clear();
                lean_neon_bits.clear();
                full.process_into_bits(&input[..tail], &mut full_bits);
                lean_scalar.process_into_bits(&input[..tail], &mut lean_scalar_bits);
                lean_neon.process_into_bits(&input[..tail], &mut lean_neon_bits);
                full.flush_into_bits(&mut full_bits);
                lean_scalar.flush_into_bits(&mut lean_scalar_bits);
                lean_neon.flush_into_bits(&mut lean_neon_bits);
                let scalar_context =
                    format!("wire rate {wire_rate}, flush tail {tail}, lean scalar");
                let neon_context = format!("wire rate {wire_rate}, flush tail {tail}, lean NEON");
                assert_eq!(full_bits.len(), tail, "{scalar_context}");
                assert_bits(&full_bits, &lean_scalar_bits, &scalar_context);
                assert_bits(&full_bits, &lean_neon_bits, &neon_context);
                assert_final_state(&full, &lean_scalar, &scalar_context);
                assert_final_state(&full, &lean_neon, &neon_context);
            }
        }
    }

    #[test]
    fn ecbeam2_committed_tail_is_single_counted() {
        let input = vec![0.0; 512];
        let mut modulator = EcBeam2Modulator::new(
            EcBeam2PlantId::default().coefficients(),
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
            EcBeam2PlantId::default().coefficients(),
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
            EcBeam2PlantId::default().coefficients(),
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
    fn ecbeam2_budget_violation_stays_finite_for_subnormal_budgets() {
        let violation = relative_budget_violation(1.0, f64::from_bits(1));
        assert_eq!(violation, f64::MAX);
        assert!(violation.is_finite());
        assert_eq!(relative_budget_violation(0.25, 0.5), 0.0);
        assert_eq!(relative_budget_violation(1.0, 0.5), 1.0);
    }

    #[test]
    fn ecbeam2_nonfinite_recovery_replays_the_emitted_stream() {
        let input = [0.0, 0.1, f64::NAN, -0.2, f64::INFINITY, 0.05, -0.03];
        let replay_input = [0.0, 0.1, 0.0, -0.2, 0.0, 0.05, -0.03];
        let wire_rate = 3_072_000;
        let mut modulator = EcBeam2Modulator::new(
            EcBeam2PlantId::default().coefficients(),
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
            EcBeam2PlantId::default().coefficients(),
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
            EcBeam2PlantId::default().coefficients(),
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
        let config = EcBeam2ExperimentConfig {
            ..EcBeam2ExperimentConfig::default()
        };
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
        let reconstruction_only = EcBeam2ExperimentConfig {
            ..EcBeam2ExperimentConfig::default()
        };
        let diagnostic_knee = EcBeam2ExperimentConfig {
            state_terminal_weight: 0.0,
            state_deadzone: 0.88,
            state_deadzone_weight: 0.0,
            quantizer_regularizer: 0.0,
            ..reconstruction_only
        };
        assert_eq!(render(reconstruction_only), render(diagnostic_knee));
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
            let mut modulator = EcBeam2Modulator::new(
                EcBeam2PlantId::default().coefficients(),
                17,
                2_822_400,
                config,
            )
            .unwrap();
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
            let mut modulator = EcBeam2Modulator::new(
                EcBeam2PlantId::default().coefficients(),
                19,
                3_072_000,
                config,
            )
            .unwrap();
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

    #[test]
    fn missing_survivor_counter_ignores_initial_fill_and_resets_fill_state() {
        let mut modulator = EcBeam2Modulator::new(
            EcBeam2PlantId::default().coefficients(),
            23,
            2_822_400,
            EcBeam2ExperimentConfig::default(),
        )
        .unwrap();
        assert!(!modulator.survivor_initial_fill_complete);
        modulator.parents_len = 2;
        modulator.observe_survivor_frontier_width();
        assert_eq!(
            modulator
                .diagnostics
                .missing_survivor_events_after_initial_fill,
            0
        );
        modulator.parents_len = ECBEAM2_WIDTH;
        modulator.observe_survivor_frontier_width();
        assert!(modulator.survivor_initial_fill_complete);
        modulator.parents_len = 3;
        modulator.observe_survivor_frontier_width();
        assert_eq!(
            modulator
                .diagnostics
                .missing_survivor_events_after_initial_fill,
            1
        );
        modulator.reseed_from_core();
        assert!(!modulator.survivor_initial_fill_complete);
        assert_eq!(
            modulator
                .diagnostics
                .missing_survivor_events_after_initial_fill,
            1
        );
        modulator.parents_len = 2;
        modulator.observe_survivor_frontier_width();
        assert_eq!(
            modulator
                .diagnostics
                .missing_survivor_events_after_initial_fill,
            1
        );
    }
}
