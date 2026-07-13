//! Read-only EcBeam production-frontier observer used by EcBeam2 quality tools.
//!
//! This entire module is behind the non-default `ecbeam2_observer` feature.
//! The production beam remains the sole chooser: the hooks consume the child
//! scratch and final Top-M order only after the production kernel has chosen
//! them. Observer values are stored in separate state and can never feed back
//! into production metrics, survivor ordering, stabilization, or dispatch.

use core::fmt;
use std::collections::VecDeque;

use super::beam_error_profile::{
    BeamErrorProfile, BeamErrorProfileState, BeamErrorProfiles, StreamingQuantile,
    profiles_for_wire_rate,
};
use super::coeff_math::{compensated_feedback, dc_bias_decay_for_corner_hz};
use super::ec_beam::BeamState;
use super::ec_beam2::{EcBeam2DiagnosticWindow, RollingEnergy, ecbeam2_v1_coefficients_match};
use super::modulator::{
    CrfbModulator, Ec2LongFilterPolicy, Ec2PolicyWeights, EcBeamClampPolicy, EcBeamMetricMode,
    EcFutureScorer, MAX_BEAM_COMMIT_HORIZON, MAX_BEAM_WIDTH, ModulatorMode,
};

pub const ECBEAM2_OBSERVER_MAX_PARENTS: usize = 16;
pub const ECBEAM2_OBSERVER_MAX_CHILDREN: usize = 32;
const MAX_EVENT_CAPACITY: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcBeam2ObserverConfig {
    /// Actual DSD64 wire rate. Only 2.8224 and 3.072 MHz have checked-in profiles.
    pub wire_rate: u32,
    /// Capture full parent/child frontier events. Renderer calibration uses the
    /// snapshot-only default so the observer does not copy multi-kilobyte
    /// public event records at every DSD sample.
    pub capture_events: bool,
    /// Bounded event queue capacity. Zero disables capture; positive values
    /// are clamped to `1..=4096`.
    pub event_capacity: usize,
    /// Optional half-open committed input-sequence range for measurement-only
    /// diagnostics. Frontier/profile state and health counters remain active
    /// across the prefix.
    pub diagnostic_window: Option<EcBeam2DiagnosticWindow>,
}

impl Default for EcBeam2ObserverConfig {
    fn default() -> Self {
        Self {
            wire_rate: 2_822_400,
            capture_events: false,
            event_capacity: 0,
            diagnostic_window: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcBeam2ObserverError {
    BeamInactive,
    BeamAlreadyAdvanced,
    NotProductionA1,
    InvalidDiagnosticWindow,
    UnsupportedOsr(u32),
    UnsupportedWireRate(u32),
}

impl fmt::Display for EcBeam2ObserverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BeamInactive => write!(formatter, "EcBeam must be active before observing it"),
            Self::BeamAlreadyAdvanced => write!(
                formatter,
                "EcBeam2 observer must be enabled on a fresh, unbuffered EcBeam frontier"
            ),
            Self::NotProductionA1 => write!(
                formatter,
                "EcBeam2 ShadowA1 requires the canonical production EcBeam A1 configuration"
            ),
            Self::InvalidDiagnosticWindow => write!(
                formatter,
                "EcBeam2 observer diagnostic window must be a non-empty half-open range"
            ),
            Self::UnsupportedOsr(osr) => {
                write!(
                    formatter,
                    "EcBeam2 observer requires DSD64 coefficients (got OSR {osr})"
                )
            }
            Self::UnsupportedWireRate(rate) => write!(
                formatter,
                "EcBeam2 observer supports only 2822400 and 3072000 Hz (got {rate})"
            ),
        }
    }
}

impl std::error::Error for EcBeam2ObserverError {}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EcBeam2ObservedParent {
    pub identity: u64,
    pub state: [f64; 8],
    pub production_metric: f64,
    pub previous_output: f64,
    pub history: u64,
    pub ecbeam2_reconstruction_metric: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EcBeam2ObservedChild {
    pub identity: u64,
    pub parent_identity: u64,
    pub parent_index: u8,
    pub bit: u8,
    pub history: u64,
    pub production_metric: f64,
    pub production_rank_metric: f64,
    /// Signed single-count reconstruction increment.
    pub reconstruction_increment: f64,
    pub reconstruction_path_metric: f64,
    /// Nonnegative causal ultrasonic power used for calibration constraints.
    pub ultrasonic_power: f64,
    /// Wire-rate-correct 10 ms EMA of causal ultrasonic power.
    pub ultrasonic_ema: f64,
    pub signed_error: f64,
    /// Wire-rate-correct 10 ms EMA of `v - u`.
    pub signed_error_ema: f64,
    pub maximum_normalized_state: f64,
    /// EcBeam2 hard-limit constraint values for this child. The observer only
    /// reports/ranks them; production EcBeam remains authoritative.
    pub maximum_state_overflow: f64,
    pub squared_state_overflow: f64,
    pub state_feasible: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EcBeam2ObservedMapping {
    pub survivor_identity: u64,
    pub parent_identity: u64,
    pub child_index: u8,
    pub bit: u8,
    pub history: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcBeam2ObservedCommitKind {
    Delayed,
    Flush,
    Recovery,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EcBeam2ObservedCommit {
    pub kind: EcBeam2ObservedCommitKind,
    pub epoch: u64,
    pub source_frontier_sequence: u64,
    pub input_sequence: u64,
    pub bit: u8,
    pub input: f64,
    pub signed_error: f64,
    pub reconstruction_output_energy: f64,
    pub reconstruction_tail_increment: f64,
    pub remaining_reconstruction_tail: f64,
    pub ultrasonic_power: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EcBeam2FrontierEvent {
    pub epoch: u64,
    pub sequence: u64,
    /// The exact post-gain/headroom/limiter value presented to production EcBeam.
    pub input: f64,
    pub parent_count: usize,
    pub parents: [EcBeam2ObservedParent; ECBEAM2_OBSERVER_MAX_PARENTS],
    pub child_count: usize,
    pub children: [EcBeam2ObservedChild; ECBEAM2_OBSERVER_MAX_CHILDREN],
    /// Actual authoritative production Top-M mappings, in production rank order.
    pub selected_count: usize,
    pub selected: [EcBeam2ObservedMapping; ECBEAM2_OBSERVER_MAX_PARENTS],
    /// Actual survivor mappings after delayed-bit disagree pruning.
    pub post_prune_count: usize,
    pub post_prune: [EcBeam2ObservedMapping; ECBEAM2_OBSERVER_MAX_PARENTS],
    pub production_best_child: u8,
    pub ecbeam2_best_child: u8,
    pub best_child_disagrees: bool,
    pub top_m_disagrees: bool,
    pub production_best_fourth_margin: f64,
    pub ecbeam2_best_fourth_margin: f64,
    pub commit: Option<EcBeam2ObservedCommit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcBeam2ObserverResetEvent {
    pub previous_epoch: u64,
    pub epoch: u64,
    pub next_sequence: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EcBeam2ObserverEvent {
    Frontier(Box<EcBeam2FrontierEvent>),
    Commit(EcBeam2ObservedCommit),
    Reset(EcBeam2ObserverResetEvent),
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EcBeam2ObserverSnapshot {
    pub epoch: u64,
    pub next_sequence: u64,
    pub frontier_events: u64,
    pub delayed_commits: u64,
    pub flush_commits: u64,
    pub recovery_commits: u64,
    pub invalid_input_substitutions: u64,
    pub committed_positive_bits: u64,
    pub resets: u64,
    pub dropped_events: u64,
    pub desynchronizations: u64,
    pub best_child_disagreements: u64,
    pub top_m_disagreements: u64,
    pub diagnostic_window_enabled: bool,
    pub diagnostic_window_start_sequence: u64,
    pub diagnostic_window_end_sequence: u64,
    pub diagnostic_window_samples: u64,
    pub diagnostic_window_positive_bits: u64,
    pub diagnostic_window_frontier_samples: u64,
    pub diagnostic_window_starting_tail_energy: f64,
    pub diagnostic_window_remaining_tail_energy: f64,
    pub production_best_fourth_margin_last: f64,
    pub production_minimum_best_fourth_margin: f64,
    pub production_maximum_best_fourth_margin: f64,
    pub ecbeam2_best_fourth_margin_last: f64,
    pub ecbeam2_minimum_best_fourth_margin: f64,
    pub ecbeam2_maximum_best_fourth_margin: f64,
    pub maximum_selected_ultrasonic_ema: f64,
    pub maximum_selected_signed_error_ema: f64,
    pub selected_ultrasonic_ema_p999: f64,
    pub selected_ultrasonic_ema_p9999: f64,
    pub selected_signed_error_ema_abs_p999: f64,
    pub selected_signed_error_ema_abs_p9999: f64,
    /// EMAs replayed from the actual delayed/flush/recovery A1 bitstream. These
    /// are the only fields used to freeze calibration budgets.
    pub maximum_committed_ultrasonic_ema: f64,
    pub maximum_committed_signed_error_ema: f64,
    pub committed_ultrasonic_ema_p999: f64,
    pub committed_ultrasonic_ema_p9999: f64,
    pub committed_signed_error_ema_abs_p999: f64,
    pub committed_signed_error_ema_abs_p9999: f64,
    pub committed_reconstruction_output_energy: f64,
    pub committed_reconstruction_tail_adjusted_energy: f64,
    pub remaining_reconstruction_tail: f64,
    pub maximum_reconstruction_tail: f64,
    pub maximum_abs_reconstruction_output: f64,
    pub committed_ultrasonic_energy: f64,
    pub maximum_ultrasonic_power: f64,
    pub maximum_reconstruction_1ms_energy: f64,
    pub maximum_reconstruction_10ms_energy: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct ObserverSurvivor {
    identity: u64,
    reconstruction_state: BeamErrorProfileState,
    ultrasonic_state: BeamErrorProfileState,
    reconstruction_metric: f64,
    ultrasonic_ema: f64,
    signed_error_ema: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct PendingInput {
    sequence: u64,
    value: f64,
}

#[derive(Debug, Clone)]
pub(super) struct ProductionEcBeam2Observer {
    reconstruction_profile: BeamErrorProfile,
    ultrasonic_profile: BeamErrorProfile,
    survivors: [ObserverSurvivor; MAX_BEAM_WIDTH],
    survivors_len: usize,
    committed_reconstruction_state: BeamErrorProfileState,
    committed_ultrasonic_state: BeamErrorProfileState,
    committed_ultrasonic_ema: f64,
    committed_signed_error_ema: f64,
    reconstruction_1ms_energy: RollingEnergy,
    reconstruction_10ms_energy: RollingEnergy,
    pending_inputs: VecDeque<PendingInput>,
    events: VecDeque<EcBeam2ObserverEvent>,
    event_capacity: usize,
    capture_events: bool,
    diagnostic_window: Option<EcBeam2DiagnosticWindow>,
    ema_beta_10ms: f64,
    selected_ultrasonic_ema_p999: StreamingQuantile,
    selected_ultrasonic_ema_p9999: StreamingQuantile,
    selected_signed_error_ema_p999: StreamingQuantile,
    selected_signed_error_ema_p9999: StreamingQuantile,
    committed_ultrasonic_ema_p999: StreamingQuantile,
    committed_ultrasonic_ema_p9999: StreamingQuantile,
    committed_signed_error_ema_p999: StreamingQuantile,
    committed_signed_error_ema_p9999: StreamingQuantile,
    next_identity: u64,
    snapshot: EcBeam2ObserverSnapshot,
}

impl ProductionEcBeam2Observer {
    fn new(
        profiles: BeamErrorProfiles,
        capture_events: bool,
        event_capacity: usize,
        wire_rate: u32,
        diagnostic_window: Option<EcBeam2DiagnosticWindow>,
    ) -> Self {
        let capture_events = capture_events && event_capacity > 0;
        let mut survivors = [ObserverSurvivor::default(); MAX_BEAM_WIDTH];
        survivors[0].identity = 1;
        Self {
            reconstruction_profile: profiles.reconstruction,
            ultrasonic_profile: profiles.ultrasonic,
            survivors,
            survivors_len: 1,
            committed_reconstruction_state: BeamErrorProfileState::default(),
            committed_ultrasonic_state: BeamErrorProfileState::default(),
            committed_ultrasonic_ema: 0.0,
            committed_signed_error_ema: 0.0,
            reconstruction_1ms_energy: RollingEnergy::new((wire_rate as usize + 500) / 1_000),
            reconstruction_10ms_energy: RollingEnergy::new(wire_rate as usize / 100),
            pending_inputs: VecDeque::with_capacity(MAX_BEAM_COMMIT_HORIZON),
            events: VecDeque::new(),
            event_capacity: if capture_events {
                event_capacity.clamp(1, MAX_EVENT_CAPACITY)
            } else {
                0
            },
            capture_events,
            diagnostic_window,
            ema_beta_10ms: (-1.0 / (wire_rate as f64 * 0.010)).exp(),
            selected_ultrasonic_ema_p999: StreamingQuantile::new(0.999),
            selected_ultrasonic_ema_p9999: StreamingQuantile::new(0.9999),
            selected_signed_error_ema_p999: StreamingQuantile::new(0.999),
            selected_signed_error_ema_p9999: StreamingQuantile::new(0.9999),
            committed_ultrasonic_ema_p999: StreamingQuantile::new(0.999),
            committed_ultrasonic_ema_p9999: StreamingQuantile::new(0.9999),
            committed_signed_error_ema_p999: StreamingQuantile::new(0.999),
            committed_signed_error_ema_p9999: StreamingQuantile::new(0.9999),
            next_identity: 2,
            snapshot: EcBeam2ObserverSnapshot {
                diagnostic_window_enabled: diagnostic_window.is_some(),
                diagnostic_window_start_sequence: diagnostic_window
                    .map_or(0, |window| window.start_sequence),
                diagnostic_window_end_sequence: diagnostic_window
                    .map_or(0, |window| window.end_sequence),
                ..EcBeam2ObserverSnapshot::default()
            },
        }
    }

    #[inline]
    fn diagnostic_sequence_selected(&self, sequence: u64) -> bool {
        self.diagnostic_window
            .is_none_or(|window| window.contains(sequence))
    }

    fn push_event(&mut self, event: EcBeam2ObserverEvent) {
        if !self.capture_events || self.event_capacity == 0 {
            return;
        }
        if self.events.len() == self.event_capacity {
            let _ = self.events.pop_front();
            self.snapshot.dropped_events = self.snapshot.dropped_events.wrapping_add(1);
        }
        self.events.push_back(event);
    }

    pub(super) fn reseed(&mut self) {
        let previous_epoch = self.snapshot.epoch;
        self.snapshot.epoch = self.snapshot.epoch.wrapping_add(1);
        self.snapshot.resets = self.snapshot.resets.wrapping_add(1);
        self.snapshot.next_sequence = 0;
        self.snapshot.delayed_commits = 0;
        self.snapshot.flush_commits = 0;
        self.snapshot.recovery_commits = 0;
        self.snapshot.committed_positive_bits = 0;
        self.snapshot.frontier_events = 0;
        self.snapshot.best_child_disagreements = 0;
        self.snapshot.top_m_disagreements = 0;
        self.snapshot.diagnostic_window_samples = 0;
        self.snapshot.diagnostic_window_positive_bits = 0;
        self.snapshot.diagnostic_window_frontier_samples = 0;
        self.snapshot.diagnostic_window_starting_tail_energy = 0.0;
        self.snapshot.diagnostic_window_remaining_tail_energy = 0.0;
        self.snapshot.production_best_fourth_margin_last = 0.0;
        self.snapshot.production_minimum_best_fourth_margin = 0.0;
        self.snapshot.production_maximum_best_fourth_margin = 0.0;
        self.snapshot.ecbeam2_best_fourth_margin_last = 0.0;
        self.snapshot.ecbeam2_minimum_best_fourth_margin = 0.0;
        self.snapshot.ecbeam2_maximum_best_fourth_margin = 0.0;
        self.snapshot.maximum_selected_ultrasonic_ema = 0.0;
        self.snapshot.maximum_selected_signed_error_ema = 0.0;
        self.snapshot.selected_ultrasonic_ema_p999 = 0.0;
        self.snapshot.selected_ultrasonic_ema_p9999 = 0.0;
        self.snapshot.selected_signed_error_ema_abs_p999 = 0.0;
        self.snapshot.selected_signed_error_ema_abs_p9999 = 0.0;
        self.snapshot.maximum_committed_ultrasonic_ema = 0.0;
        self.snapshot.maximum_committed_signed_error_ema = 0.0;
        self.snapshot.committed_ultrasonic_ema_p999 = 0.0;
        self.snapshot.committed_ultrasonic_ema_p9999 = 0.0;
        self.snapshot.committed_signed_error_ema_abs_p999 = 0.0;
        self.snapshot.committed_signed_error_ema_abs_p9999 = 0.0;
        self.snapshot.committed_reconstruction_output_energy = 0.0;
        self.snapshot.committed_reconstruction_tail_adjusted_energy = 0.0;
        self.snapshot.remaining_reconstruction_tail = 0.0;
        self.snapshot.maximum_reconstruction_tail = 0.0;
        self.snapshot.maximum_abs_reconstruction_output = 0.0;
        self.snapshot.committed_ultrasonic_energy = 0.0;
        self.snapshot.maximum_ultrasonic_power = 0.0;
        self.snapshot.maximum_reconstruction_1ms_energy = 0.0;
        self.snapshot.maximum_reconstruction_10ms_energy = 0.0;
        self.survivors = [ObserverSurvivor::default(); MAX_BEAM_WIDTH];
        self.survivors[0].identity = self.allocate_identity();
        self.survivors_len = 1;
        self.committed_reconstruction_state = BeamErrorProfileState::default();
        self.committed_ultrasonic_state = BeamErrorProfileState::default();
        self.committed_ultrasonic_ema = 0.0;
        self.committed_signed_error_ema = 0.0;
        self.reconstruction_1ms_energy.reset();
        self.reconstruction_10ms_energy.reset();
        self.selected_ultrasonic_ema_p999 = StreamingQuantile::new(0.999);
        self.selected_ultrasonic_ema_p9999 = StreamingQuantile::new(0.9999);
        self.selected_signed_error_ema_p999 = StreamingQuantile::new(0.999);
        self.selected_signed_error_ema_p9999 = StreamingQuantile::new(0.9999);
        self.committed_ultrasonic_ema_p999 = StreamingQuantile::new(0.999);
        self.committed_ultrasonic_ema_p9999 = StreamingQuantile::new(0.9999);
        self.committed_signed_error_ema_p999 = StreamingQuantile::new(0.999);
        self.committed_signed_error_ema_p9999 = StreamingQuantile::new(0.9999);
        self.pending_inputs.clear();
        self.push_event(EcBeam2ObserverEvent::Reset(EcBeam2ObserverResetEvent {
            previous_epoch,
            epoch: self.snapshot.epoch,
            next_sequence: self.snapshot.next_sequence,
        }));
    }

    /// Restart only the provisional production frontier after a normal beam
    /// flush. Production EcBeam preserves its committed CRFB state across this
    /// operation, so the observer must likewise preserve its independently
    /// replayed profile states, epoch, sequence, and cumulative diagnostics.
    pub(super) fn reseed_frontier_after_flush(&mut self) {
        let identity = self.allocate_identity();
        self.survivors = [ObserverSurvivor::default(); MAX_BEAM_WIDTH];
        self.survivors[0] = ObserverSurvivor {
            identity,
            reconstruction_state: self.committed_reconstruction_state,
            ultrasonic_state: self.committed_ultrasonic_state,
            reconstruction_metric: 0.0,
            ultrasonic_ema: self.committed_ultrasonic_ema,
            signed_error_ema: self.committed_signed_error_ema,
        };
        self.survivors_len = 1;
        self.pending_inputs.clear();
    }

    /// A production safety recovery resets only CRFB/frontier state. The
    /// emitted reconstruction, ultrasonic, and EMA histories remain physical
    /// stream state and seed the new observer frontier unchanged.
    pub(super) fn reseed_frontier_after_recovery(&mut self) {
        self.snapshot.epoch = self.snapshot.epoch.wrapping_add(1);
        self.snapshot.resets = self.snapshot.resets.wrapping_add(1);
        self.reseed_frontier_after_flush();
    }

    fn allocate_identity(&mut self) -> u64 {
        let identity = self.next_identity;
        self.next_identity = self.next_identity.wrapping_add(1).max(1);
        identity
    }

    fn align_survivors(&mut self, production_len: usize) {
        if self.survivors_len == production_len {
            return;
        }
        self.snapshot.desynchronizations = self.snapshot.desynchronizations.wrapping_add(1);
        self.survivors = [ObserverSurvivor::default(); MAX_BEAM_WIDTH];
        for index in 0..production_len.min(MAX_BEAM_WIDTH) {
            self.survivors[index].identity = self.allocate_identity();
        }
        self.survivors_len = production_len.min(MAX_BEAM_WIDTH);
        self.pending_inputs.clear();
    }

    fn record_commit(
        &mut self,
        kind: EcBeam2ObservedCommitKind,
        source_frontier_sequence: u64,
        input: PendingInput,
        bit: u8,
    ) -> EcBeam2ObservedCommit {
        let measure = self.diagnostic_sequence_selected(input.sequence);
        let output = if bit == 1 { 1.0 } else { -1.0 };
        let error = output - input.value;
        let starting_reconstruction_tail = self
            .reconstruction_profile
            .remaining_zero_input_energy(&self.committed_reconstruction_state);
        let reconstruction_output = self
            .reconstruction_profile
            .output(&self.committed_reconstruction_state, error);
        let reconstruction_output_energy = reconstruction_output * reconstruction_output;
        let reconstruction_tail_increment = self
            .reconstruction_profile
            .tail_adjusted_energy_increment(&self.committed_reconstruction_state, error);
        self.reconstruction_profile
            .advance(&mut self.committed_reconstruction_state, error);
        let remaining_reconstruction_tail = self
            .reconstruction_profile
            .remaining_zero_input_energy(&self.committed_reconstruction_state);
        let ultrasonic_output = self
            .ultrasonic_profile
            .advance(&mut self.committed_ultrasonic_state, error);
        let ultrasonic_power = ultrasonic_output * ultrasonic_output;
        let one_minus_beta = 1.0 - self.ema_beta_10ms;
        self.committed_ultrasonic_ema = self.ema_beta_10ms.mul_add(
            self.committed_ultrasonic_ema,
            one_minus_beta * ultrasonic_power,
        );
        self.committed_signed_error_ema = self
            .ema_beta_10ms
            .mul_add(self.committed_signed_error_ema, one_minus_beta * error);

        match kind {
            EcBeam2ObservedCommitKind::Delayed => {
                self.snapshot.delayed_commits = self.snapshot.delayed_commits.wrapping_add(1)
            }
            EcBeam2ObservedCommitKind::Flush => {
                self.snapshot.flush_commits = self.snapshot.flush_commits.wrapping_add(1)
            }
            EcBeam2ObservedCommitKind::Recovery => {
                self.snapshot.recovery_commits = self.snapshot.recovery_commits.wrapping_add(1)
            }
        }
        self.snapshot.committed_positive_bits = self
            .snapshot
            .committed_positive_bits
            .wrapping_add(u64::from(bit == 1));
        self.snapshot.remaining_reconstruction_tail = remaining_reconstruction_tail;
        self.reconstruction_1ms_energy
            .observe(reconstruction_output_energy);
        self.reconstruction_10ms_energy
            .observe(reconstruction_output_energy);
        if measure {
            let signed_error_ema_abs = self.committed_signed_error_ema.abs();
            self.committed_ultrasonic_ema_p999
                .observe(self.committed_ultrasonic_ema);
            self.committed_ultrasonic_ema_p9999
                .observe(self.committed_ultrasonic_ema);
            self.committed_signed_error_ema_p999
                .observe(signed_error_ema_abs);
            self.committed_signed_error_ema_p9999
                .observe(signed_error_ema_abs);
            self.snapshot.diagnostic_window_samples =
                self.snapshot.diagnostic_window_samples.wrapping_add(1);
            self.snapshot.diagnostic_window_positive_bits = self
                .snapshot
                .diagnostic_window_positive_bits
                .wrapping_add(u64::from(bit == 1));
            if self.snapshot.diagnostic_window_samples == 1 {
                self.snapshot.diagnostic_window_starting_tail_energy = starting_reconstruction_tail;
            }
            self.snapshot.diagnostic_window_remaining_tail_energy = remaining_reconstruction_tail;
            self.snapshot.committed_reconstruction_output_energy += reconstruction_output_energy;
            self.snapshot.committed_reconstruction_tail_adjusted_energy +=
                reconstruction_tail_increment;
            self.snapshot.maximum_reconstruction_tail = self
                .snapshot
                .maximum_reconstruction_tail
                .max(remaining_reconstruction_tail);
            self.snapshot.maximum_abs_reconstruction_output = self
                .snapshot
                .maximum_abs_reconstruction_output
                .max(reconstruction_output.abs());
            self.snapshot.committed_ultrasonic_energy += ultrasonic_power;
            self.snapshot.maximum_ultrasonic_power =
                self.snapshot.maximum_ultrasonic_power.max(ultrasonic_power);
            self.snapshot.maximum_committed_ultrasonic_ema = self
                .snapshot
                .maximum_committed_ultrasonic_ema
                .max(self.committed_ultrasonic_ema);
            self.snapshot.maximum_committed_signed_error_ema = self
                .snapshot
                .maximum_committed_signed_error_ema
                .max(signed_error_ema_abs);
            self.snapshot.committed_ultrasonic_ema_p999 =
                self.committed_ultrasonic_ema_p999.estimate();
            self.snapshot.committed_ultrasonic_ema_p9999 =
                self.committed_ultrasonic_ema_p9999.estimate();
            self.snapshot.committed_signed_error_ema_abs_p999 =
                self.committed_signed_error_ema_p999.estimate();
            self.snapshot.committed_signed_error_ema_abs_p9999 =
                self.committed_signed_error_ema_p9999.estimate();
            self.snapshot.maximum_reconstruction_1ms_energy = self
                .snapshot
                .maximum_reconstruction_1ms_energy
                .max(self.reconstruction_1ms_energy.current());
            self.snapshot.maximum_reconstruction_10ms_energy = self
                .snapshot
                .maximum_reconstruction_10ms_energy
                .max(self.reconstruction_10ms_energy.current());
        }

        EcBeam2ObservedCommit {
            kind,
            epoch: self.snapshot.epoch,
            source_frontier_sequence,
            input_sequence: input.sequence,
            bit,
            input: input.value,
            signed_error: error,
            reconstruction_output_energy,
            reconstruction_tail_increment,
            remaining_reconstruction_tail,
            ultrasonic_power,
        }
    }
}

impl CrfbModulator {
    fn matches_production_ecbeam_a1(&self, wire_rate: u32) -> bool {
        const PRESSURE_WEIGHTS: [f64; 7] =
            [0.4375, 0.4375, 0.65625, 0.875, 1.09375, 1.53125, 1.96875];
        let pressure_sum: f64 = PRESSURE_WEIGHTS.iter().sum();
        let expected_pressure = PRESSURE_WEIGHTS.map(|weight| weight / pressure_sum);
        let expected_policy = Ec2PolicyWeights {
            quantizer_weight: 0.8,
            pressure_weight: 2.75,
            limit_weight: 80.0,
            transition_weight: 0.002,
            dc_weight: 0.04,
            lookahead_discount: 0.8,
            ambiguity_margin: 0.0,
            pressure_taper_start: 0.60,
            pressure_taper_strength: 0.0,
        };
        let Some(beam) = self.beam.as_ref() else {
            return false;
        };
        self.mode == ModulatorMode::Ec
            && ecbeam2_v1_coefficients_match(self.coeffs)
            && beam.m == 4
            && beam.n == 8
            && !self.effective_dither_active()
            && self.common_side_dither.is_none()
            && self.gated_dither_margin == 0.0
            && self.gated_dither_scale == 0.0
            && self.isi_penalty == 0.0
            && self.lookahead_depth == 2
            && self.future_scorer == EcFutureScorer::QuantizerOnly
            && self.ec2_policy == Ec2LongFilterPolicy::AmbiguityPressure
            && self.ec2_weights == expected_policy
            && self.pressure_stage_weighted
            && self.pressure_stage_weight[..7]
                .iter()
                .zip(expected_pressure)
                .all(|(actual, expected)| actual.to_bits() == expected.to_bits())
            && self.dc_bias_decay.to_bits()
                == dc_bias_decay_for_corner_hz(20.0, wire_rate).to_bits()
            && beam.terminal_weight.to_bits() == 0.3f64.to_bits()
            && beam.alternation_weight.to_bits() == 0.0005f64.to_bits()
            && beam.alternation_rank_weight == 0.0
            && beam.alternation_threshold == 0.0
            && beam.filtered_error_weight == 0.0
            && beam.filtered_error_rank_weight == 0.0
            && beam.reconstruction_error_weight == 0.0
            && beam.pressure_deadzone == 0.0
            && beam.periodicity_weight == 0.0
            && beam.metric_mode == EcBeamMetricMode::HybridRankNudged
            && beam.pressure_accum_scale == 1.0
            && beam.pressure_rank_scale == 0.0
            && beam.dc_accum_scale == 1.0
            && beam.dc_rank_scale == 0.0
            && beam.clamp_policy == EcBeamClampPolicy::LegacyClampAndContinue
            && !beam.collect_metric_diagnostics
    }

    /// Enable the read-only production EcBeam frontier observer.
    ///
    /// It must be attached after `set_beam_search` and before any input is
    /// consumed. Enabling it never changes kernel eligibility.
    pub fn enable_ecbeam2_observer(
        &mut self,
        config: EcBeam2ObserverConfig,
    ) -> Result<(), EcBeam2ObserverError> {
        if self.coeffs.osr != 64 {
            return Err(EcBeam2ObserverError::UnsupportedOsr(self.coeffs.osr));
        }
        if config
            .diagnostic_window
            .is_some_and(|window| !window.is_valid())
        {
            return Err(EcBeam2ObserverError::InvalidDiagnosticWindow);
        }
        let profiles = profiles_for_wire_rate(config.wire_rate)
            .map_err(|_| EcBeam2ObserverError::UnsupportedWireRate(config.wire_rate))?;
        let beam = self
            .beam
            .as_ref()
            .ok_or(EcBeam2ObserverError::BeamInactive)?;
        if beam.sample_index != 0
            || beam.buffered != 0
            || beam.parents_len != 1
            || beam.diagnostics.emit_count != 0
        {
            return Err(EcBeam2ObserverError::BeamAlreadyAdvanced);
        }
        if !self.matches_production_ecbeam_a1(config.wire_rate) {
            return Err(EcBeam2ObserverError::NotProductionA1);
        }
        let beam = self
            .beam
            .as_mut()
            .expect("beam presence was validated above");
        beam.ecbeam2_observer = Some(ProductionEcBeam2Observer::new(
            profiles,
            config.capture_events,
            config.event_capacity,
            config.wire_rate,
            config.diagnostic_window,
        ));
        Ok(())
    }

    pub fn disable_ecbeam2_observer(&mut self) {
        if let Some(beam) = &mut self.beam {
            beam.ecbeam2_observer = None;
        }
    }

    pub fn ecbeam2_observer_snapshot(&self) -> Option<EcBeam2ObserverSnapshot> {
        self.beam
            .as_ref()
            .and_then(|beam| beam.ecbeam2_observer.as_ref())
            .map(|observer| observer.snapshot)
    }

    pub fn drain_ecbeam2_observer_events(&mut self) -> Vec<EcBeam2ObserverEvent> {
        self.beam
            .as_mut()
            .and_then(|beam| beam.ecbeam2_observer.as_mut())
            .map(|observer| observer.events.drain(..).collect())
            .unwrap_or_default()
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn observe_authoritative_frontier(
    beam: &mut BeamState,
    input: f64,
    parent_bank: usize,
    children: usize,
    selected_order: &[u8],
    bv_norm: &[f64; 8],
    state_limit8: &[f64; 8],
    isi_penalty: f64,
    normalized_state_kernel: bool,
) {
    let Some(mut observer) = beam.ecbeam2_observer.take() else {
        return;
    };
    observer.align_survivors(beam.parents_len);

    let sequence = observer.snapshot.next_sequence;
    let epoch = observer.snapshot.epoch;
    let capture_event = observer.capture_events && observer.diagnostic_sequence_selected(sequence);
    let parent_count = beam.parents_len.min(ECBEAM2_OBSERVER_MAX_PARENTS);
    let child_count = children.min(ECBEAM2_OBSERVER_MAX_CHILDREN);
    let selected_count = selected_order.len().min(ECBEAM2_OBSERVER_MAX_PARENTS);
    let mut parents = [EcBeam2ObservedParent::default(); ECBEAM2_OBSERVER_MAX_PARENTS];
    let captured_parent_count = if capture_event { parent_count } else { 0 };
    for (parent_index, observed_parent) in parents[..captured_parent_count].iter_mut().enumerate() {
        let production = beam.parents[parent_bank][parent_index];
        let state = if normalized_state_kernel {
            let mut state = [0.0; 8];
            for stage in 0..7 {
                state[stage] =
                    beam.m4n8_norm_state[parent_bank][stage][parent_index] * state_limit8[stage];
            }
            state
        } else {
            production.state
        };
        *observed_parent = EcBeam2ObservedParent {
            identity: observer.survivors[parent_index].identity,
            state,
            production_metric: production.metric,
            previous_output: production.prev_v,
            history: production.bits,
            ecbeam2_reconstruction_metric: observer.survivors[parent_index].reconstruction_metric,
        };
    }

    let mut observed_children = [EcBeam2ObservedChild::default(); ECBEAM2_OBSERVER_MAX_CHILDREN];
    let mut next_children = [ObserverSurvivor::default(); ECBEAM2_OBSERVER_MAX_CHILDREN];
    for child_index in 0..child_count {
        let parent_index = beam.child_parent[child_index] as usize;
        if parent_index >= observer.survivors_len {
            continue;
        }
        let parent = observer.survivors[parent_index];
        let output = beam.child_v[child_index];
        let error = output - input;
        let reconstruction_increment = observer
            .reconstruction_profile
            .tail_adjusted_energy_increment(&parent.reconstruction_state, error);
        let reconstruction_state = observer
            .reconstruction_profile
            .next_state(&parent.reconstruction_state, error);
        let ultrasonic_output = observer
            .ultrasonic_profile
            .output(&parent.ultrasonic_state, error);
        let ultrasonic_power = ultrasonic_output * ultrasonic_output;
        let ultrasonic_state = observer
            .ultrasonic_profile
            .next_state(&parent.ultrasonic_state, error);
        let one_minus_beta = 1.0 - observer.ema_beta_10ms;
        let ultrasonic_ema = observer
            .ema_beta_10ms
            .mul_add(parent.ultrasonic_ema, one_minus_beta * ultrasonic_power);
        let signed_error_ema = observer
            .ema_beta_10ms
            .mul_add(parent.signed_error_ema, one_minus_beta * error);
        let identity = observer.allocate_identity();
        let reconstruction_path_metric = parent.reconstruction_metric + reconstruction_increment;
        let production_parent = beam.parents[parent_bank][parent_index];
        let feedback = compensated_feedback(production_parent.prev_v, output, isi_penalty);
        let mut maximum_normalized_state = 0.0f64;
        let mut squared_state_overflow = 0.0f64;
        for (stage, feedback_coefficient) in bv_norm.iter().copied().take(7).enumerate() {
            let base = if normalized_state_kernel {
                beam.m4n8_base_norm[stage][parent_index]
            } else {
                beam.bases[parent_index][stage]
            };
            let normalized = feedback.mul_add(feedback_coefficient, base).abs();
            maximum_normalized_state = maximum_normalized_state.max(normalized);
            let overflow = (normalized - 1.0).max(0.0);
            squared_state_overflow += overflow * overflow;
        }
        let maximum_state_overflow = (maximum_normalized_state - 1.0).max(0.0);
        next_children[child_index] = ObserverSurvivor {
            identity,
            reconstruction_state,
            ultrasonic_state,
            reconstruction_metric: reconstruction_path_metric,
            ultrasonic_ema,
            signed_error_ema,
        };
        observed_children[child_index] = EcBeam2ObservedChild {
            identity,
            parent_identity: parent.identity,
            parent_index: parent_index as u8,
            bit: u8::from(output > 0.0),
            history: beam.child_bits[child_index],
            production_metric: beam.child_metric[child_index],
            production_rank_metric: beam.child_rank_metric[child_index],
            reconstruction_increment,
            reconstruction_path_metric,
            ultrasonic_power,
            ultrasonic_ema,
            signed_error: error,
            signed_error_ema,
            maximum_normalized_state,
            maximum_state_overflow,
            squared_state_overflow,
            state_feasible: maximum_state_overflow == 0.0,
        };
    }

    let has_state_feasible = observed_children[..child_count]
        .iter()
        .any(|child| child.state_feasible);
    let mut objective_order = [0u8; ECBEAM2_OBSERVER_MAX_CHILDREN];
    for index in 0..child_count {
        objective_order[index] = index as u8;
        let mut position = index;
        while position > 0
            && ecbeam2_child_precedes(
                &observed_children[objective_order[position] as usize],
                &observed_children[objective_order[position - 1] as usize],
                has_state_feasible,
            )
        {
            objective_order.swap(position, position - 1);
            position -= 1;
        }
    }

    let mut selected = [EcBeam2ObservedMapping::default(); ECBEAM2_OBSERVER_MAX_PARENTS];
    let mut next_survivors = [ObserverSurvivor::default(); MAX_BEAM_WIDTH];
    for (rank, &child_u8) in selected_order.iter().take(selected_count).enumerate() {
        let child_index = child_u8 as usize;
        let child = observed_children[child_index];
        selected[rank] = mapping_for_child(child_index, child);
        next_survivors[rank] = next_children[child_index];
    }

    let production_best_child = selected_order.first().copied().unwrap_or(0);
    let ecbeam2_best_child = objective_order[0];
    let ecbeam2_selected_count = if has_state_feasible {
        observed_children[..child_count]
            .iter()
            .filter(|child| child.state_feasible)
            .count()
            .min(selected_count)
    } else {
        1
    };
    let production_best = observed_children[production_best_child as usize];
    let measure_frontier = observer.diagnostic_sequence_selected(sequence);
    if measure_frontier {
        observer.snapshot.diagnostic_window_frontier_samples = observer
            .snapshot
            .diagnostic_window_frontier_samples
            .wrapping_add(1);
        observer.snapshot.maximum_selected_ultrasonic_ema = observer
            .snapshot
            .maximum_selected_ultrasonic_ema
            .max(production_best.ultrasonic_ema);
        observer.snapshot.maximum_selected_signed_error_ema = observer
            .snapshot
            .maximum_selected_signed_error_ema
            .max(production_best.signed_error_ema.abs());
        observer
            .selected_ultrasonic_ema_p999
            .observe(production_best.ultrasonic_ema);
        observer
            .selected_ultrasonic_ema_p9999
            .observe(production_best.ultrasonic_ema);
        observer
            .selected_signed_error_ema_p999
            .observe(production_best.signed_error_ema.abs());
        observer
            .selected_signed_error_ema_p9999
            .observe(production_best.signed_error_ema.abs());
        observer.snapshot.selected_ultrasonic_ema_p999 =
            observer.selected_ultrasonic_ema_p999.estimate();
        observer.snapshot.selected_ultrasonic_ema_p9999 =
            observer.selected_ultrasonic_ema_p9999.estimate();
        observer.snapshot.selected_signed_error_ema_abs_p999 =
            observer.selected_signed_error_ema_p999.estimate();
        observer.snapshot.selected_signed_error_ema_abs_p9999 =
            observer.selected_signed_error_ema_p9999.estimate();
    }
    let best_child_disagrees = production_best_child != ecbeam2_best_child;
    let mut top_m_disagrees = ecbeam2_selected_count != selected_count;
    for &candidate in &objective_order[..ecbeam2_selected_count] {
        if !selected_order[..selected_count].contains(&candidate) {
            top_m_disagrees = true;
            break;
        }
    }
    let fourth = selected_count.saturating_sub(1).min(3);
    let production_best_fourth_margin = if selected_count > 1 {
        let best = observed_children[production_best_child as usize].production_rank_metric;
        observed_children[selected_order[fourth] as usize].production_rank_metric - best
    } else {
        0.0
    };
    let ecbeam2_best_fourth_margin = if ecbeam2_selected_count > 1 {
        let best = observed_children[ecbeam2_best_child as usize].reconstruction_path_metric;
        let objective_fourth = ecbeam2_selected_count.saturating_sub(1).min(3);
        observed_children[objective_order[objective_fourth] as usize].reconstruction_path_metric
            - best
    } else {
        0.0
    };
    if measure_frontier {
        observer.snapshot.production_best_fourth_margin_last = production_best_fourth_margin;
        observer.snapshot.production_minimum_best_fourth_margin =
            if observer.snapshot.diagnostic_window_frontier_samples == 1 {
                production_best_fourth_margin
            } else {
                observer
                    .snapshot
                    .production_minimum_best_fourth_margin
                    .min(production_best_fourth_margin)
            };
        observer.snapshot.production_maximum_best_fourth_margin = observer
            .snapshot
            .production_maximum_best_fourth_margin
            .max(production_best_fourth_margin);
        observer.snapshot.ecbeam2_best_fourth_margin_last = ecbeam2_best_fourth_margin;
        observer.snapshot.ecbeam2_minimum_best_fourth_margin =
            if observer.snapshot.diagnostic_window_frontier_samples == 1 {
                ecbeam2_best_fourth_margin
            } else {
                observer
                    .snapshot
                    .ecbeam2_minimum_best_fourth_margin
                    .min(ecbeam2_best_fourth_margin)
            };
        observer.snapshot.ecbeam2_maximum_best_fourth_margin = observer
            .snapshot
            .ecbeam2_maximum_best_fourth_margin
            .max(ecbeam2_best_fourth_margin);
    }

    observer.pending_inputs.push_back(PendingInput {
        sequence,
        value: input,
    });
    while observer.pending_inputs.len() > MAX_BEAM_COMMIT_HORIZON {
        let _ = observer.pending_inputs.pop_front();
        observer.snapshot.desynchronizations = observer.snapshot.desynchronizations.wrapping_add(1);
    }

    let will_commit = beam.buffered + 1 == beam.n;
    let commit_bit = if will_commit {
        let shift = beam.n - 1;
        ((observed_children[production_best_child as usize].history >> shift) & 1) as u8
    } else {
        0
    };
    let mut post_prune = [EcBeam2ObservedMapping::default(); ECBEAM2_OBSERVER_MAX_PARENTS];
    let mut post_prune_survivors = [ObserverSurvivor::default(); MAX_BEAM_WIDTH];
    let mut post_prune_count = 0usize;
    for rank in 0..selected_count {
        if !will_commit || ((selected[rank].history >> (beam.n - 1)) & 1) as u8 == commit_bit {
            post_prune[post_prune_count] = selected[rank];
            post_prune_survivors[post_prune_count] = next_survivors[rank];
            post_prune_count += 1;
        }
    }
    observer.survivors = post_prune_survivors;
    observer.survivors_len = post_prune_count;

    let commit = if will_commit {
        let commit_input = observer.pending_inputs.pop_front().unwrap_or(PendingInput {
            sequence,
            value: input,
        });
        Some(observer.record_commit(
            EcBeam2ObservedCommitKind::Delayed,
            sequence,
            commit_input,
            commit_bit,
        ))
    } else {
        None
    };

    observer.snapshot.next_sequence = observer.snapshot.next_sequence.wrapping_add(1);
    observer.snapshot.frontier_events = observer.snapshot.frontier_events.wrapping_add(1);
    if measure_frontier {
        observer.snapshot.best_child_disagreements = observer
            .snapshot
            .best_child_disagreements
            .wrapping_add(u64::from(best_child_disagrees));
        observer.snapshot.top_m_disagreements = observer
            .snapshot
            .top_m_disagreements
            .wrapping_add(u64::from(top_m_disagrees));
    }
    let capture_standalone_commit = observer.capture_events
        && !capture_event
        && commit
            .is_some_and(|commit| observer.diagnostic_sequence_selected(commit.input_sequence));
    if capture_standalone_commit {
        observer.push_event(EcBeam2ObserverEvent::Commit(
            commit.expect("standalone commit presence was checked"),
        ));
    }
    if capture_event {
        observer.push_event(EcBeam2ObserverEvent::Frontier(Box::new(
            EcBeam2FrontierEvent {
                epoch,
                sequence,
                input,
                parent_count,
                parents,
                child_count,
                children: observed_children,
                selected_count,
                selected,
                post_prune_count,
                post_prune,
                production_best_child,
                ecbeam2_best_child,
                best_child_disagrees,
                top_m_disagrees,
                production_best_fourth_margin,
                ecbeam2_best_fourth_margin,
                commit,
            },
        )));
    }
    beam.ecbeam2_observer = Some(observer);
}

pub(super) fn observe_buffered_flush(beam: &mut BeamState) {
    let Some(mut observer) = beam.ecbeam2_observer.take() else {
        return;
    };
    if beam.buffered > 0 {
        let best = beam.parents[beam.parents_bank][0];
        for shift in (0..beam.buffered).rev() {
            let bit = ((best.bits >> shift) & 1) as u8;
            let pending = observer.pending_inputs.pop_front().unwrap_or_else(|| {
                observer.snapshot.desynchronizations =
                    observer.snapshot.desynchronizations.wrapping_add(1);
                PendingInput {
                    sequence: observer.snapshot.next_sequence,
                    value: 0.0,
                }
            });
            let commit = observer.record_commit(
                EcBeam2ObservedCommitKind::Flush,
                pending.sequence,
                pending,
                bit,
            );
            if observer.capture_events && observer.diagnostic_sequence_selected(pending.sequence) {
                observer.push_event(EcBeam2ObserverEvent::Commit(commit));
            }
        }
    }
    observer.pending_inputs.clear();
    beam.ecbeam2_observer = Some(observer);
}

/// Replay the fixed `+1` bit emitted by a production EcBeam safety recovery.
/// The caller supplies the real finite input when available, or the declared
/// `u = 0` diagnostic substitute for a non-finite input.
pub(super) fn observe_recovery_bit(
    beam: &mut BeamState,
    input: f64,
    invalid_input_substitution: bool,
) {
    let Some(mut observer) = beam.ecbeam2_observer.take() else {
        return;
    };
    debug_assert!(input.is_finite());
    let sequence = observer.snapshot.next_sequence;
    let pending = PendingInput {
        sequence,
        value: input,
    };
    let commit = observer.record_commit(EcBeam2ObservedCommitKind::Recovery, sequence, pending, 1);
    observer.snapshot.invalid_input_substitutions = observer
        .snapshot
        .invalid_input_substitutions
        .wrapping_add(u64::from(invalid_input_substitution));
    observer.snapshot.next_sequence = observer.snapshot.next_sequence.wrapping_add(1);
    if observer.capture_events && observer.diagnostic_sequence_selected(sequence) {
        observer.push_event(EcBeam2ObserverEvent::Commit(commit));
    }
    beam.ecbeam2_observer = Some(observer);
}

#[inline]
fn mapping_for_child(child_index: usize, child: EcBeam2ObservedChild) -> EcBeam2ObservedMapping {
    EcBeam2ObservedMapping {
        survivor_identity: child.identity,
        parent_identity: child.parent_identity,
        child_index: child_index as u8,
        bit: child.bit,
        history: child.history,
    }
}

#[inline]
fn ecbeam2_child_precedes(
    left: &EcBeam2ObservedChild,
    right: &EcBeam2ObservedChild,
    has_state_feasible: bool,
) -> bool {
    if left.state_feasible != right.state_feasible {
        return left.state_feasible;
    }
    if !has_state_feasible {
        match left
            .maximum_state_overflow
            .total_cmp(&right.maximum_state_overflow)
            .then_with(|| {
                left.squared_state_overflow
                    .total_cmp(&right.squared_state_overflow)
            }) {
            core::cmp::Ordering::Less => return true,
            core::cmp::Ordering::Greater => return false,
            core::cmp::Ordering::Equal => {}
        }
    }
    match left
        .reconstruction_path_metric
        .total_cmp(&right.reconstruction_path_metric)
    {
        core::cmp::Ordering::Less => true,
        core::cmp::Ordering::Greater => false,
        core::cmp::Ordering::Equal => left.history > right.history,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observer_ecbeam2_order_is_feasibility_first_and_uses_repair_key() {
        let feasible = EcBeam2ObservedChild {
            reconstruction_path_metric: 10.0,
            state_feasible: true,
            history: 1,
            ..EcBeam2ObservedChild::default()
        };
        let low_objective_but_infeasible = EcBeam2ObservedChild {
            reconstruction_path_metric: -10.0,
            maximum_state_overflow: 0.01,
            squared_state_overflow: 0.0001,
            state_feasible: false,
            history: 2,
            ..EcBeam2ObservedChild::default()
        };
        assert!(ecbeam2_child_precedes(
            &feasible,
            &low_objective_but_infeasible,
            true
        ));

        let larger_overflow = EcBeam2ObservedChild {
            maximum_state_overflow: 0.02,
            squared_state_overflow: 0.00001,
            reconstruction_path_metric: -20.0,
            state_feasible: false,
            ..EcBeam2ObservedChild::default()
        };
        assert!(ecbeam2_child_precedes(
            &low_objective_but_infeasible,
            &larger_overflow,
            false
        ));
    }
}
