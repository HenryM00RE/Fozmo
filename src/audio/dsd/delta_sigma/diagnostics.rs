use serde::Serialize;

use super::coeff_math::finite_or_zero;
use super::modulator::EC_STATE_LIMIT_SOFT_KNEE;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Ec2DecisionTraceSummary {
    pub window_bits: usize,
    pub window_count: usize,
    pub total_commits: u64,
    pub near_tie_count: u64,
    pub ambiguity_override_count: u64,
    pub pressure_taper_count: u64,
    pub root_flip_count: u64,
    pub nonfinite_best_score_count: u64,
    pub f_abs_gt_1_count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Ec2DecisionWindow {
    pub start_bit: u64,
    pub len_bits: u64,
    pub total_commits: u64,
    pub near_tie_count: u64,
    pub ambiguity_override_count: u64,
    pub pressure_taper_count: u64,
    pub pressure_mean: f64,
    pub pressure_max: f64,
    pub pressure_ge_045: u64,
    pub pressure_ge_060: u64,
    pub pressure_ge_072: u64,
    pub pressure_ge_082: u64,
    pub root_margin_mean: f64,
    pub root_margin_min: f64,
    pub root_hot_raw_count: u64,
    pub chosen_plus: u64,
    pub chosen_minus: u64,
    pub transitions: u64,
    pub root_flip_count: u64,
    pub quantizer_error_mean: f64,
    pub quantizer_error_max: f64,
    pub dc_bias_abs_mean: f64,
    pub dc_bias_abs_max: f64,
    pub dither_abs_mean: f64,
    pub dither_abs_max: f64,
    pub committed_state_pressure_mean: f64,
    pub committed_state_pressure_max: f64,
    pub committed_state_energy_mean: f64,
    pub committed_state_energy_max: f64,
    pub committed_state_stage0_max: f64,
    pub committed_state_stage1_max: f64,
    pub committed_state_stage2_max: f64,
    pub committed_state_stage3_max: f64,
    pub committed_state_stage4_max: f64,
    pub committed_state_stage5_max: f64,
    pub committed_state_stage6_max: f64,
    pub nonfinite_best_score_count: u64,
    pub f_abs_gt_1_count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Ec2DecisionTraceSnapshot {
    pub summary: Ec2DecisionTraceSummary,
    pub windows: Vec<Ec2DecisionWindow>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AdaptiveDecisionWindow {
    pub start_bit: u64,
    pub len_bits: u64,
    pub total_commits: u64,
    pub depth4_commits: u64,
    pub trigger_none: u64,
    pub trigger_guard: u64,
    pub trigger_pressure: u64,
    pub trigger_transient: u64,
    pub trigger_ambiguity: u64,
    pub pressure_mean: f64,
    pub pressure_max: f64,
    pub pressure_ge_045: u64,
    pub pressure_ge_060: u64,
    pub pressure_ge_072: u64,
    pub pressure_ge_082: u64,
    pub root_margin_mean: f64,
    pub root_margin_min: f64,
    pub root_hot_raw_count: u64,
    pub guard_hot_count: u64,
    pub chosen_plus: u64,
    pub chosen_minus: u64,
    pub transitions: u64,
    pub root_flip_count: u64,
    pub depth4_root_flip_count: u64,
    pub depth4_shadow_depth2_same_root: u64,
    pub depth4_shadow_depth2_diff_root: u64,
    pub depth4_shadow_score_delta_sum: f64,
    pub depth4_shadow_score_delta_min: f64,
    pub depth4_shadow_score_delta_max: f64,
    pub nonfinite_best_score_count: u64,
    pub clean_policy_mismatch_count: u64,
    pub f_abs_gt_1_count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AdaptiveDecisionTraceSnapshot {
    pub windows: Vec<AdaptiveDecisionWindow>,
}

#[derive(Debug, Clone)]
pub(super) struct Ec2DecisionTrace {
    window_bits: usize,
    windows: Vec<Ec2DecisionWindow>,
    current: Ec2DecisionWindowAccum,
    summary: Ec2DecisionTraceSummary,
}

#[derive(Debug, Clone)]
pub(super) struct Ec2DecisionWindowAccum {
    start_bit: u64,
    total_commits: u64,
    near_tie_count: u64,
    ambiguity_override_count: u64,
    pressure_taper_count: u64,
    pressure_sum: f64,
    pressure_max: f64,
    pressure_ge_045: u64,
    pressure_ge_060: u64,
    pressure_ge_072: u64,
    pressure_ge_082: u64,
    root_margin_sum: f64,
    root_margin_min: f64,
    root_hot_raw_count: u64,
    chosen_plus: u64,
    chosen_minus: u64,
    transitions: u64,
    root_flip_count: u64,
    quantizer_error_sum: f64,
    quantizer_error_max: f64,
    dc_bias_abs_sum: f64,
    dc_bias_abs_max: f64,
    dither_abs_sum: f64,
    dither_abs_max: f64,
    committed_state_pressure_sum: f64,
    committed_state_pressure_max: f64,
    committed_state_energy_sum: f64,
    committed_state_energy_max: f64,
    committed_state_stage_max: [f64; 7],
    nonfinite_best_score_count: u64,
    f_abs_gt_1_count: u64,
    prev_chosen: Option<f64>,
}

impl Default for Ec2DecisionWindowAccum {
    fn default() -> Self {
        Self {
            start_bit: 0,
            total_commits: 0,
            near_tie_count: 0,
            ambiguity_override_count: 0,
            pressure_taper_count: 0,
            pressure_sum: 0.0,
            pressure_max: 0.0,
            pressure_ge_045: 0,
            pressure_ge_060: 0,
            pressure_ge_072: 0,
            pressure_ge_082: 0,
            root_margin_sum: 0.0,
            root_margin_min: f64::INFINITY,
            root_hot_raw_count: 0,
            chosen_plus: 0,
            chosen_minus: 0,
            transitions: 0,
            root_flip_count: 0,
            quantizer_error_sum: 0.0,
            quantizer_error_max: 0.0,
            dc_bias_abs_sum: 0.0,
            dc_bias_abs_max: 0.0,
            dither_abs_sum: 0.0,
            dither_abs_max: 0.0,
            committed_state_pressure_sum: 0.0,
            committed_state_pressure_max: 0.0,
            committed_state_energy_sum: 0.0,
            committed_state_energy_max: 0.0,
            committed_state_stage_max: [0.0; 7],
            nonfinite_best_score_count: 0,
            f_abs_gt_1_count: 0,
            prev_chosen: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Ec2DecisionTraceEvent {
    pub(super) pressure: f64,
    pub(super) root_margin: f64,
    pub(super) root_hot: bool,
    pub(super) near_tie: bool,
    pub(super) ambiguity_override: bool,
    pub(super) pressure_tapered: bool,
    pub(super) chosen: f64,
    pub(super) root_winner: f64,
    pub(super) best_score: f64,
    pub(super) quantizer_error_abs: f64,
    pub(super) dc_bias_abs: f64,
    pub(super) dither_abs: f64,
    pub(super) committed_state_pressure: f64,
    pub(super) committed_state_energy: f64,
    pub(super) committed_state_stage_abs: [f64; 7],
    pub(super) f_best_abs: f64,
}

impl Ec2DecisionTrace {
    pub(super) fn new(window_bits: usize) -> Self {
        let window_bits = window_bits.max(1024);
        Self {
            window_bits,
            windows: Vec::new(),
            current: Ec2DecisionWindowAccum::default(),
            summary: Ec2DecisionTraceSummary {
                window_bits,
                ..Ec2DecisionTraceSummary::default()
            },
        }
    }

    pub(super) fn reset(&mut self) {
        let window_bits = self.window_bits;
        *self = Self::new(window_bits);
    }

    pub(super) fn record(&mut self, event: Ec2DecisionTraceEvent) {
        if self.current.total_commits >= self.window_bits as u64 {
            self.flush_current();
        }
        let pressure = finite_or_zero(event.pressure);
        let root_margin = finite_or_zero(event.root_margin);
        let quantizer_error_abs = finite_or_zero(event.quantizer_error_abs);
        let dc_bias_abs = finite_or_zero(event.dc_bias_abs);
        let dither_abs = finite_or_zero(event.dither_abs);
        let committed_state_pressure = finite_or_zero(event.committed_state_pressure);
        let committed_state_energy = finite_or_zero(event.committed_state_energy);
        let current = &mut self.current;
        current.total_commits += 1;
        current.near_tie_count += u64::from(event.near_tie);
        current.ambiguity_override_count += u64::from(event.ambiguity_override);
        current.pressure_taper_count += u64::from(event.pressure_tapered);
        current.pressure_sum += pressure;
        current.pressure_max = current.pressure_max.max(pressure);
        current.pressure_ge_045 += u64::from(pressure >= 0.45);
        current.pressure_ge_060 += u64::from(pressure >= 0.60);
        current.pressure_ge_072 += u64::from(pressure >= 0.72);
        current.pressure_ge_082 += u64::from(pressure >= EC_STATE_LIMIT_SOFT_KNEE);
        current.root_margin_sum += root_margin;
        current.root_margin_min = current.root_margin_min.min(root_margin);
        current.root_hot_raw_count += u64::from(event.root_hot);
        current.chosen_plus += u64::from(event.chosen > 0.0);
        current.chosen_minus += u64::from(event.chosen <= 0.0);
        current.root_flip_count += u64::from(event.root_winner != event.chosen);
        current.nonfinite_best_score_count += u64::from(!event.best_score.is_finite());
        current.f_abs_gt_1_count += u64::from(event.f_best_abs > 1.0);
        current.quantizer_error_sum += quantizer_error_abs;
        current.quantizer_error_max = current.quantizer_error_max.max(quantizer_error_abs);
        current.dc_bias_abs_sum += dc_bias_abs;
        current.dc_bias_abs_max = current.dc_bias_abs_max.max(dc_bias_abs);
        current.dither_abs_sum += dither_abs;
        current.dither_abs_max = current.dither_abs_max.max(dither_abs);
        current.committed_state_pressure_sum += committed_state_pressure;
        current.committed_state_pressure_max = current
            .committed_state_pressure_max
            .max(committed_state_pressure);
        current.committed_state_energy_sum += committed_state_energy;
        current.committed_state_energy_max = current
            .committed_state_energy_max
            .max(committed_state_energy);
        for (acc, value) in current
            .committed_state_stage_max
            .iter_mut()
            .zip(event.committed_state_stage_abs)
        {
            *acc = (*acc).max(finite_or_zero(value));
        }
        if current
            .prev_chosen
            .replace(event.chosen)
            .is_some_and(|prev| prev != event.chosen)
        {
            current.transitions += 1;
        }
    }

    pub(super) fn snapshot(&self) -> Ec2DecisionTraceSnapshot {
        let mut clone = self.clone();
        clone.flush_current();
        Ec2DecisionTraceSnapshot {
            summary: clone.summary,
            windows: clone.windows,
        }
    }

    pub(super) fn flush_current(&mut self) {
        if self.current.total_commits == 0 {
            return;
        }
        let current = std::mem::take(&mut self.current);
        let total = current.total_commits as f64;
        self.summary.total_commits += current.total_commits;
        self.summary.near_tie_count += current.near_tie_count;
        self.summary.ambiguity_override_count += current.ambiguity_override_count;
        self.summary.pressure_taper_count += current.pressure_taper_count;
        self.summary.root_flip_count += current.root_flip_count;
        self.summary.nonfinite_best_score_count += current.nonfinite_best_score_count;
        self.summary.f_abs_gt_1_count += current.f_abs_gt_1_count;
        self.windows.push(Ec2DecisionWindow {
            start_bit: current.start_bit,
            len_bits: current.total_commits,
            total_commits: current.total_commits,
            near_tie_count: current.near_tie_count,
            ambiguity_override_count: current.ambiguity_override_count,
            pressure_taper_count: current.pressure_taper_count,
            pressure_mean: current.pressure_sum / total,
            pressure_max: current.pressure_max,
            pressure_ge_045: current.pressure_ge_045,
            pressure_ge_060: current.pressure_ge_060,
            pressure_ge_072: current.pressure_ge_072,
            pressure_ge_082: current.pressure_ge_082,
            root_margin_mean: current.root_margin_sum / total,
            root_margin_min: if current.root_margin_min.is_finite() {
                current.root_margin_min
            } else {
                0.0
            },
            root_hot_raw_count: current.root_hot_raw_count,
            chosen_plus: current.chosen_plus,
            chosen_minus: current.chosen_minus,
            transitions: current.transitions,
            root_flip_count: current.root_flip_count,
            quantizer_error_mean: current.quantizer_error_sum / total,
            quantizer_error_max: current.quantizer_error_max,
            dc_bias_abs_mean: current.dc_bias_abs_sum / total,
            dc_bias_abs_max: current.dc_bias_abs_max,
            dither_abs_mean: current.dither_abs_sum / total,
            dither_abs_max: current.dither_abs_max,
            committed_state_pressure_mean: current.committed_state_pressure_sum / total,
            committed_state_pressure_max: current.committed_state_pressure_max,
            committed_state_energy_mean: current.committed_state_energy_sum / total,
            committed_state_energy_max: current.committed_state_energy_max,
            committed_state_stage0_max: current.committed_state_stage_max[0],
            committed_state_stage1_max: current.committed_state_stage_max[1],
            committed_state_stage2_max: current.committed_state_stage_max[2],
            committed_state_stage3_max: current.committed_state_stage_max[3],
            committed_state_stage4_max: current.committed_state_stage_max[4],
            committed_state_stage5_max: current.committed_state_stage_max[5],
            committed_state_stage6_max: current.committed_state_stage_max[6],
            nonfinite_best_score_count: current.nonfinite_best_score_count,
            f_abs_gt_1_count: current.f_abs_gt_1_count,
        });
        self.summary.window_count = self.windows.len();
        self.current.start_bit = self.summary.total_commits;
    }
}

/// EcBeam decision-trace counters (§13/§20 Q1–Q2), cumulative since
/// construction. Phase 0 keeps these as plain fields; the windowed
/// `BeamDecisionTrace` CSV surface is Phase 1.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct BeamDiagnostics {
    /// Delayed bits committed (excludes flush-tail bits).
    pub emit_count: u64,
    /// Candidate transitions evaluated by the active beam search. This is a
    /// work counter only and does not participate in ranking or output.
    pub transition_evaluations: u64,
    /// Samples where the best survivor did not descend from the previous best.
    pub path_switches: u64,
    /// Emits whose bit differs from the instantaneous best child's bit when
    /// that sample was the frontier — 0 over a fixture means the beam
    /// degenerated to an expensive greedy (§20 Q1).
    pub delayed_flips: u64,
    /// Survivors killed by the disagree-prune.
    pub pruned_total: u64,
    /// Post-selection clamps while materializing kept beam survivors. The
    /// public `state_clamps` counts only committed-path clamps.
    pub beam_clamp_total: u64,
    /// Candidate expansions whose normalized state would exceed a hard limit.
    pub beam_speculative_clamp_total: u64,
    /// Buffered/committed best-path bits whose materialized survivor had been
    /// clamped. Mirrors the beam contribution to public `state_clamps`.
    pub beam_committed_clamp_total: u64,
    /// Child candidates rejected before materialization by
    /// [`EcBeamClampPolicy::RejectHardLimit`].
    pub beam_rejected_hard_limit_total: u64,
    /// Samples where every child was rejected or non-finite and the beam reset.
    pub beam_all_children_rejected_total: u64,
    /// Minimum survivor count observed after any prune.
    pub min_survivors: u64,
}

/// EcBeam reconstruction-error metric diagnostics, cumulative since beam
/// construction. These are debug/measurement counters for the experimental
/// reconstruction proxy and are zero when the proxy weight is zero.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BeamReconstructionDiagnostics {
    /// Child expansions that evaluated the reconstruction-error metric.
    pub samples: u64,
    pub filtered_energy_sum: f64,
    pub filtered_energy_max: f64,
    pub weighted_contribution_sum: f64,
    pub weighted_contribution_max: f64,
    pub contribution_to_legacy_ratio_samples: u64,
    pub contribution_to_legacy_ratio_sum: f64,
    pub contribution_to_legacy_ratio_max: f64,
}

impl BeamReconstructionDiagnostics {
    #[inline(always)]
    pub(super) fn record(
        &mut self,
        filtered_energy: f64,
        weighted_contribution: f64,
        legacy_cost: f64,
    ) {
        self.samples = self.samples.wrapping_add(1);
        self.filtered_energy_sum += filtered_energy;
        self.filtered_energy_max = self.filtered_energy_max.max(filtered_energy);
        self.weighted_contribution_sum += weighted_contribution;
        self.weighted_contribution_max = self.weighted_contribution_max.max(weighted_contribution);
        if legacy_cost.is_finite() && legacy_cost.abs() > f64::EPSILON {
            let ratio = weighted_contribution / legacy_cost.abs();
            self.contribution_to_legacy_ratio_samples =
                self.contribution_to_legacy_ratio_samples.wrapping_add(1);
            self.contribution_to_legacy_ratio_sum += ratio;
            self.contribution_to_legacy_ratio_max =
                self.contribution_to_legacy_ratio_max.max(ratio);
        }
    }
}

/// EcBeam lag-k periodicity metric diagnostics, cumulative since beam
/// construction. These counters are zero when the periodicity weight is zero.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BeamPeriodicityDiagnostics {
    /// Child expansions that evaluated the periodicity metric.
    pub samples: u64,
    pub penalty_sum: f64,
    pub penalty_max: f64,
    pub weighted_contribution_sum: f64,
    pub weighted_contribution_max: f64,
    pub contribution_to_legacy_ratio_samples: u64,
    pub contribution_to_legacy_ratio_sum: f64,
    pub contribution_to_legacy_ratio_max: f64,
}

impl BeamPeriodicityDiagnostics {
    #[inline(always)]
    pub(super) fn record(&mut self, penalty: f64, weighted_contribution: f64, legacy_cost: f64) {
        self.samples = self.samples.wrapping_add(1);
        self.penalty_sum += penalty;
        self.penalty_max = self.penalty_max.max(penalty);
        self.weighted_contribution_sum += weighted_contribution;
        self.weighted_contribution_max = self.weighted_contribution_max.max(weighted_contribution);
        if legacy_cost.is_finite() && legacy_cost.abs() > f64::EPSILON {
            let ratio = weighted_contribution / legacy_cost.abs();
            self.contribution_to_legacy_ratio_samples =
                self.contribution_to_legacy_ratio_samples.wrapping_add(1);
            self.contribution_to_legacy_ratio_sum += ratio;
            self.contribution_to_legacy_ratio_max =
                self.contribution_to_legacy_ratio_max.max(ratio);
        }
    }
}

/// EcBeam rank-vs-accumulated metric diagnostics, cumulative since beam
/// construction. These counters are useful when a tuning run uses rank-only
/// nudges and needs to know whether pruning changed because of them.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BeamMetricDiagnostics {
    /// Child candidates admitted into Top-M selection.
    pub samples: u64,
    pub rank_metric_delta_abs_sum: f64,
    pub rank_metric_delta_abs_max: f64,
    /// Samples whose best child differs when ranking by accumulated metric only.
    pub top_child_changed_by_rank: u64,
    /// Samples whose kept survivor set differs when ranking by accumulated
    /// metric only.
    pub survivor_set_changed_by_rank: u64,
}

impl BeamMetricDiagnostics {
    #[inline(always)]
    pub(super) fn record_deltas(&mut self, child_metrics: &[f64], child_rank_metrics: &[f64]) {
        self.samples = self.samples.wrapping_add(child_metrics.len() as u64);
        for (&metric, &rank_metric) in child_metrics.iter().zip(child_rank_metrics) {
            let delta = (rank_metric - metric).abs();
            self.rank_metric_delta_abs_sum += delta;
            self.rank_metric_delta_abs_max = self.rank_metric_delta_abs_max.max(delta);
        }
    }
}
