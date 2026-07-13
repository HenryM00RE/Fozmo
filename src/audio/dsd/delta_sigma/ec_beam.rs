use super::coeff_math::*;
use super::diagnostics::{
    BeamDiagnostics, BeamMetricDiagnostics, BeamPeriodicityDiagnostics,
    BeamReconstructionDiagnostics,
};
use super::modulator::*;
use super::stability::*;

const M4N8_TRANSITION_MASK: [u64; 9] = [0, 0, 1, 3, 7, 15, 31, 63, 127];

const fn m4n8_alternation_penalty_table() -> [[f64; 8]; 9] {
    let mut table = [[0.0; 8]; 9];
    let mut len = 2usize;
    while len <= 8 {
        let mut transitions = 0usize;
        while transitions < len {
            let density = transitions as f64 / (len - 1) as f64;
            table[len][transitions] = density * density;
            transitions += 1;
        }
        len += 1;
    }
    table
}

const M4N8_ALTERNATION_PENALTY: [[f64; 8]; 9] = m4n8_alternation_penalty_table();

#[inline(always)]
fn push_beam_bit_unchecked(out_bits: &mut Vec<u8>, bit: u8) {
    debug_assert!(out_bits.len() < out_bits.capacity());
    // SAFETY: `process_into_bits` reserves one slot per input sample before
    // dispatch. EcBeam emits no more than one output per consumed input when
    // carried latency and reset draining are accounted for.
    unsafe {
        let len = out_bits.len();
        out_bits.as_mut_ptr().add(len).write(bit);
        out_bits.set_len(len + 1);
    }
}

/// One EcBeam survivor (docs/dev/7th-order-ecm-m-algorithm.md §21.7).
///
/// Deliberate deviation from the design pseudocode: survivors carry the *raw*
/// committed-space integrator state, not the normalized state. Each survivor
/// therefore re-normalizes at expansion and de-normalizes at materialization,
/// which keeps commit rounding aligned with the rest of the modulator.
#[derive(Debug, Clone, Copy)]
pub(super) struct BeamPath {
    /// Raw integrator state, lane 7 pinned to 0 (same space as `CrfbModulator::state`).
    pub(super) state: [f64; 8],
    /// Accumulated undiscounted path cost, renormalized each sample (§21.7).
    pub(super) metric: f64,
    /// This path's last feedback (transition cost; per-path ISI-ready).
    pub(super) prev_v: f64,
    /// This path's committed-bit DC tracker state.
    pub(super) dc_bias: f64,
    /// Packed bit history, newest in the LSB, masked to the low `n` bits.
    pub(super) bits: u64,
    /// Expansion-time clamp flags, shifted/masked in lockstep with `bits`.
    pub(super) clamp_bits: u64,
}

impl BeamPath {
    pub(super) const INERT: Self = Self {
        state: [0.0; 8],
        metric: 0.0,
        prev_v: 1.0,
        dc_bias: 0.0,
        bits: 0,
        clamp_bits: 0,
    };
}

/// EcBeam decision-trace counters (§13/§20 Q1–Q2), cumulative since
/// construction. Phase 0 keeps these as plain fields; the windowed
/// `BeamDecisionTrace` CSV surface is Phase 1.
#[doc(hidden)]
/// Boxed EcBeam side-state so `CrfbModulator` itself doesn't grow ~4 KB
/// (§21.7). Present only while the beam prototype is activated.
#[derive(Debug, Clone)]
pub(super) struct BeamState {
    /// Beam width, 1..=MAX_BEAM_WIDTH.
    pub(super) m: usize,
    /// Commit horizon: output lags input by `n - 1` samples.
    pub(super) n: usize,
    /// Low-`n`-bits mask for the packed histories (§21.4).
    pub(super) bits_mask: u64,
    pub(super) parents: [[BeamPath; MAX_BEAM_WIDTH]; 2],
    /// Optional per-survivor low-passed quantizer-error state. Kept outside
    /// `BeamPath` so the plain hot path does not copy inactive aux state.
    pub(super) parent_filtered_error: [[[f64; 2]; MAX_BEAM_WIDTH]; 2],
    /// Optional per-survivor reconstruction-error proxy state. This filters
    /// bit-vs-input reconstruction, not comparator/quantizer error.
    pub(super) parent_reconstruction_error: [[[f64; 4]; MAX_BEAM_WIDTH]; 2],
    /// Whether the current parent bank's aux arrays mirror `parents`.
    pub(super) aux_valid: bool,
    pub(super) parents_bank: usize,
    pub(super) parents_len: usize,
    /// Un-emitted bits currently buffered by every survivor (equal beam-wide).
    pub(super) buffered: usize,
    /// Expansions since the last (re)seed; indexes the frontier ring.
    pub(super) sample_index: u64,
    /// Generic-path wrapping index for `frontier_ring`; avoids `% beam.n`.
    pub(super) ring_index: usize,
    /// Newest bit of the instantaneous best child, per recent sample — the
    /// `delayed_flips` reference (§20 Q1).
    pub(super) frontier_ring: [u8; MAX_BEAM_COMMIT_HORIZON],
    /// Rank-only terminal state pressure weight. This is not accumulated into
    /// `BeamPath::metric`; it only nudges the current frontier ordering.
    pub(super) terminal_weight: f64,
    /// Beam-only short-run alternation penalty weight. In the legacy metric it
    /// accumulates into `BeamPath::metric`; in metric-hygiene mode it is
    /// rank-only. The proxy uses only packed recent bits.
    pub(super) alternation_weight: f64,
    /// Rank-only variant of the short-run alternation penalty. This nudges
    /// frontier ordering without accumulating into `BeamPath::metric`.
    pub(super) alternation_rank_weight: f64,
    /// Minimum adjacent-transition density before alternation penalty applies.
    pub(super) alternation_threshold: f64,
    /// Beam-only frequency-weighted path metric. When nonzero, each survivor
    /// carries a tiny low-pass of the quantizer error and accumulates its energy.
    pub(super) filtered_error_weight: f64,
    /// Rank-only variant of the frequency-weighted path metric. This nudges
    /// frontier ordering without accumulating into `BeamPath::metric`.
    pub(super) filtered_error_rank_weight: f64,
    /// Beam-only reconstruction-error proxy metric. When nonzero, each survivor
    /// carries a low-passed audio-band proxy for `v - input` energy and adds it
    /// to the accumulated path metric.
    pub(super) reconstruction_error_weight: f64,
    /// Beam-only pressure-shaping experiment. When nonzero, the accumulated
    /// pressure term scores only per-stage normalized-state excursions above
    /// this threshold. `0.0` keeps the legacy pressure scorer path exactly.
    pub(super) pressure_deadzone: f64,
    /// Beam-only lag-k periodicity penalty weight. Unlike the lag-1
    /// alternation proxy, this scores repeated short-period structure at
    /// selected lags and is accumulated into the survivor metric only when
    /// explicitly enabled.
    pub(super) periodicity_weight: f64,
    pub(super) periodicity_lags: [u8; MAX_BEAM_PERIODICITY_LAGS],
    pub(super) periodicity_lag_count: usize,
    pub(super) periodicity_window: usize,
    /// Which objective Top-M survivor pruning uses.
    pub(super) metric_mode: EcBeamMetricMode,
    /// Experimental split for pressure/DC terms. Accumulated scales affect
    /// `BeamPath::metric`; rank scales only affect current-frontier ordering.
    pub(super) pressure_accum_scale: f64,
    pub(super) pressure_rank_scale: f64,
    pub(super) dc_accum_scale: f64,
    pub(super) dc_rank_scale: f64,
    /// How EcBeam handles candidates whose predicted normalized state exceeds
    /// the hard integrator limits.
    pub(super) clamp_policy: EcBeamClampPolicy,
    /// Full rank-vs-accumulated metric diagnostics require a second Top-M sort;
    /// keep them explicitly opt-in for wall-clock-bound sweeps.
    pub(super) collect_metric_diagnostics: bool,
    pub(super) diagnostics: BeamDiagnostics,
    pub(super) reconstruction_diagnostics: BeamReconstructionDiagnostics,
    pub(super) periodicity_diagnostics: BeamPeriodicityDiagnostics,
    pub(super) metric_diagnostics: BeamMetricDiagnostics,
    // Persistent per-sample expansion scratch. Living here (already boxed)
    // instead of on the stack avoids re-initializing ~4 KB per sample, which
    // measured as a double-digit share of the beam step. Only indices below
    // the per-sample write counts are ever read.
    /// Accumulated path metric stored in `BeamPath::metric`.
    pub(super) child_metric: [f64; 2 * MAX_BEAM_WIDTH],
    /// Selection-only metric; may include opt-in rank nudges that are not
    /// accumulated into the survivor path cost.
    pub(super) child_rank_metric: [f64; 2 * MAX_BEAM_WIDTH],
    pub(super) child_bits: [u64; 2 * MAX_BEAM_WIDTH],
    pub(super) child_parent: [u8; 2 * MAX_BEAM_WIDTH],
    pub(super) child_v: [f64; 2 * MAX_BEAM_WIDTH],
    pub(super) child_filtered_error: [[f64; 2]; 2 * MAX_BEAM_WIDTH],
    pub(super) child_reconstruction_error: [[f64; 4]; 2 * MAX_BEAM_WIDTH],
    pub(super) bases: [[f64; 8]; MAX_BEAM_WIDTH],
    pub(super) base_hot: [bool; MAX_BEAM_WIDTH],
    /// Canonical normalized survivor state for the fixed AArch64 M4/N8 A1
    /// kernel. The stage-major layout lets each NEON lane represent a beam
    /// survivor. Raw `BeamPath::state` is synchronized at block boundaries.
    pub(super) m4n8_norm_state: [[[f64; 4]; 8]; 2],
    pub(super) m4n8_base_norm: [[f64; 4]; 8],
    pub(super) m4n8_norm_valid: bool,
    #[cfg(feature = "ecbeam2_observer")]
    pub(super) ecbeam2_observer: Option<super::ecbeam2_observer::ProductionEcBeam2Observer>,
    #[cfg(test)]
    pub(super) force_generic_path: bool,
}

impl BeamState {
    pub(super) fn new(m: usize, n: usize) -> Self {
        Self {
            m,
            n,
            bits_mask: low_bits_mask(n),
            parents: [[BeamPath::INERT; MAX_BEAM_WIDTH]; 2],
            parent_filtered_error: [[[0.0; 2]; MAX_BEAM_WIDTH]; 2],
            parent_reconstruction_error: [[[0.0; 4]; MAX_BEAM_WIDTH]; 2],
            aux_valid: false,
            parents_bank: 0,
            parents_len: 1,
            buffered: 0,
            sample_index: 0,
            ring_index: 0,
            frontier_ring: [0; MAX_BEAM_COMMIT_HORIZON],
            terminal_weight: 0.0,
            alternation_weight: 0.0,
            alternation_rank_weight: 0.0,
            alternation_threshold: 0.0,
            filtered_error_weight: EC_BEAM_FILTERED_ERROR_WEIGHT,
            filtered_error_rank_weight: EC_BEAM_FILTERED_ERROR_RANK_WEIGHT,
            reconstruction_error_weight: EC_BEAM_RECONSTRUCTION_ERROR_WEIGHT,
            pressure_deadzone: EC_BEAM_PRESSURE_DEADZONE,
            periodicity_weight: EC_BEAM_PERIODICITY_WEIGHT,
            periodicity_lags: EC_BEAM_PERIODICITY_DEFAULT_LAGS,
            periodicity_lag_count: EC_BEAM_PERIODICITY_DEFAULT_LAG_COUNT,
            periodicity_window: EC_BEAM_PERIODICITY_DEFAULT_WINDOW,
            metric_mode: EcBeamMetricMode::default(),
            pressure_accum_scale: 1.0,
            pressure_rank_scale: 0.0,
            dc_accum_scale: 1.0,
            dc_rank_scale: 0.0,
            clamp_policy: EcBeamClampPolicy::default(),
            collect_metric_diagnostics: false,
            diagnostics: BeamDiagnostics {
                min_survivors: m as u64,
                ..BeamDiagnostics::default()
            },
            reconstruction_diagnostics: BeamReconstructionDiagnostics::default(),
            periodicity_diagnostics: BeamPeriodicityDiagnostics::default(),
            metric_diagnostics: BeamMetricDiagnostics::default(),
            child_metric: [0.0; 2 * MAX_BEAM_WIDTH],
            child_rank_metric: [0.0; 2 * MAX_BEAM_WIDTH],
            child_bits: [0; 2 * MAX_BEAM_WIDTH],
            child_parent: [0; 2 * MAX_BEAM_WIDTH],
            child_v: [0.0; 2 * MAX_BEAM_WIDTH],
            child_filtered_error: [[0.0; 2]; 2 * MAX_BEAM_WIDTH],
            child_reconstruction_error: [[0.0; 4]; 2 * MAX_BEAM_WIDTH],
            bases: [[0.0; 8]; MAX_BEAM_WIDTH],
            base_hot: [false; MAX_BEAM_WIDTH],
            m4n8_norm_state: [[[0.0; 4]; 8]; 2],
            m4n8_base_norm: [[0.0; 4]; 8],
            m4n8_norm_valid: false,
            #[cfg(feature = "ecbeam2_observer")]
            ecbeam2_observer: None,
            #[cfg(test)]
            force_generic_path: false,
        }
    }

    /// Collapse the beam to a single survivor at the committed state.
    /// Diagnostics are cumulative and survive reseeds.
    pub(super) fn reseed(&mut self, seed: BeamPath) {
        self.parents[self.parents_bank][0] = seed;
        self.parent_filtered_error[self.parents_bank][0] = [0.0; 2];
        self.parent_reconstruction_error[self.parents_bank][0] = [0.0; 4];
        self.aux_valid = false;
        self.parents_len = 1;
        self.buffered = 0;
        self.sample_index = 0;
        self.ring_index = 0;
        self.m4n8_norm_valid = false;
        #[cfg(feature = "ecbeam2_observer")]
        if let Some(observer) = &mut self.ecbeam2_observer {
            observer.reseed();
        }
    }

    /// Normal flush collapses the production frontier without discontinuing
    /// the committed signal. Keep observer replay state continuous while
    /// resetting the provisional survivor geometry to the committed seed.
    #[cfg(feature = "ecbeam2_observer")]
    pub(super) fn reseed_after_observed_flush(&mut self, seed: BeamPath) {
        let observer = self.ecbeam2_observer.take();
        self.reseed(seed);
        if let Some(mut observer) = observer {
            observer.reseed_frontier_after_flush();
            self.ecbeam2_observer = Some(observer);
        }
    }

    /// Collapse the production frontier after an internal CRFB safety reset
    /// while preserving the observer's independently replayed emitted-stream
    /// profile and EMA state.
    pub(super) fn reseed_after_recovery(&mut self, seed: BeamPath) {
        #[cfg(feature = "ecbeam2_observer")]
        {
            let observer = self.ecbeam2_observer.take();
            self.reseed(seed);
            if let Some(mut observer) = observer {
                observer.reseed_frontier_after_recovery();
                self.ecbeam2_observer = Some(observer);
            }
        }
        #[cfg(not(feature = "ecbeam2_observer"))]
        self.reseed(seed);
    }

    pub(super) fn ensure_aux_state(&mut self) {
        if self.aux_valid {
            return;
        }
        let bank = self.parents_bank;
        for idx in 0..self.parents_len {
            self.parent_filtered_error[bank][idx] = [0.0; 2];
            self.parent_reconstruction_error[bank][idx] = [0.0; 4];
        }
        self.aux_valid = true;
    }
}

/// EcBeam selection key (§21.4): ascending metric, exact metric ties broken by
/// descending packed bits — with `+1 ↔ 1` and newest-in-LSB over a shared
/// emitted prefix, the larger `u64` is the path that chose `+1` at the
/// earliest divergence, matching `select_ec_candidate`'s
/// `c_plus <= c_minus → +1` convention. Returns `true` when `a` ranks at or
/// before `b`; on a full key tie the incumbent (`a`) stays first, which is
/// what keeps the insertion sort stable. Metrics must be finite (non-finite
/// children are filtered before selection).
#[inline(always)]
pub(super) fn beam_rank_at_or_before(
    metric_a: f64,
    bits_a: u64,
    metric_b: f64,
    bits_b: u64,
) -> bool {
    metric_a < metric_b || (metric_a == metric_b && bits_a >= bits_b)
}

#[inline(always)]
pub(super) fn low_bits_mask(width: usize) -> u64 {
    if width >= u64::BITS as usize {
        !0
    } else {
        (1u64 << width) - 1
    }
}

#[inline(always)]
pub(super) fn sort_beam_children(
    children: usize,
    metrics: &[f64],
    bits: &[u64],
    order: &mut [u8; 2 * MAX_BEAM_WIDTH],
) {
    debug_assert!(children <= order.len());
    debug_assert!(children <= metrics.len());
    debug_assert!(children <= bits.len());
    for (idx, slot) in order.iter_mut().enumerate().take(children) {
        *slot = idx as u8;
    }
    for sorted_len in 1..children {
        let key = order[sorted_len];
        let key_metric = metrics[key as usize];
        let key_bits = bits[key as usize];
        let mut pos = sorted_len;
        while pos > 0 {
            let other = order[pos - 1] as usize;
            if beam_rank_at_or_before(metrics[other], bits[other], key_metric, key_bits) {
                break;
            }
            order[pos] = order[pos - 1];
            pos -= 1;
        }
        order[pos] = key;
    }
}

/// Select and fully order only the best four children. This preserves the
/// stable metric/bits ordering of `sort_beam_children` without sorting the
/// discarded tail of the fixed M4 frontier.
#[inline(always)]
pub(super) fn select_top4_beam_children(
    children: usize,
    metrics: &[f64],
    bits: &[u64],
    order: &mut [u8; 4],
) -> usize {
    debug_assert!(children <= metrics.len());
    debug_assert!(children <= bits.len());
    debug_assert!(children <= u8::MAX as usize);
    debug_assert_eq!(order.len(), 4);
    let mut kept = 0usize;
    for idx in 0..children {
        let mut pos = kept;
        while pos > 0 {
            // SAFETY: `pos <= kept <= order.len()`, while `idx` and every
            // previously stored child index are below `children`.
            let other = unsafe { *order.get_unchecked(pos - 1) as usize };
            let other_metric = unsafe { *metrics.get_unchecked(other) };
            let other_bits = unsafe { *bits.get_unchecked(other) };
            let key_metric = unsafe { *metrics.get_unchecked(idx) };
            let key_bits = unsafe { *bits.get_unchecked(idx) };
            if beam_rank_at_or_before(other_metric, other_bits, key_metric, key_bits) {
                break;
            }
            pos -= 1;
        }
        if pos >= order.len() {
            continue;
        }
        let new_kept = (kept + 1).min(order.len());
        let mut shift = new_kept - 1;
        while shift > pos {
            // SAFETY: both indices are below `new_kept <= order.len()`.
            unsafe {
                *order.get_unchecked_mut(shift) = *order.get_unchecked(shift - 1);
            }
            shift -= 1;
        }
        // SAFETY: `pos < order.len()` was checked above.
        unsafe { *order.get_unchecked_mut(pos) = idx as u8 };
        kept = new_kept;
    }
    kept
}

pub(super) fn beam_record_metric_diagnostics(
    beam: &mut BeamState,
    children: usize,
    keep: usize,
    rank_order: &[u8; 2 * MAX_BEAM_WIDTH],
) {
    let has_rank_delta = beam.child_metric[..children]
        .iter()
        .zip(&beam.child_rank_metric[..children])
        .any(|(&metric, &rank_metric)| rank_metric != metric);
    beam.metric_diagnostics.record_deltas(
        &beam.child_metric[..children],
        &beam.child_rank_metric[..children],
    );
    if children == 0 || !has_rank_delta {
        return;
    }

    let mut metric_order = [0u8; 2 * MAX_BEAM_WIDTH];
    sort_beam_children(
        children,
        &beam.child_metric,
        &beam.child_bits,
        &mut metric_order,
    );

    if rank_order[0] != metric_order[0] {
        beam.metric_diagnostics.top_child_changed_by_rank = beam
            .metric_diagnostics
            .top_child_changed_by_rank
            .wrapping_add(1);
    }
    let mut changed = false;
    for &rank_child in &rank_order[..keep] {
        if !metric_order[..keep].contains(&rank_child) {
            changed = true;
            break;
        }
    }
    if changed {
        beam.metric_diagnostics.survivor_set_changed_by_rank = beam
            .metric_diagnostics
            .survivor_set_changed_by_rank
            .wrapping_add(1);
    }
}

#[inline(always)]
pub(super) fn sanitize_beam_metric_scale(scale: f64) -> f64 {
    if scale.is_finite() && scale >= 0.0 {
        scale
    } else {
        0.0
    }
}

#[inline(always)]
pub(super) fn beam_filtered_error_alpha(osr: u32) -> f64 {
    // Coefficient tables currently expose only OSR, not the renderer-selected
    // wire rate. EcBeam's experimental filter proxy therefore documents and
    // uses the 44.1 kHz DSD family explicitly.
    beam_one_pole_alpha_for_wire_rate(20_000.0, 44_100.0 * osr.max(1) as f64)
}

#[inline(always)]
pub(super) fn beam_filtered_error_step(
    mut state: [f64; 2],
    error: f64,
    alpha: f64,
) -> ([f64; 2], f64) {
    state[0] += alpha * (error - state[0]);
    state[1] += alpha * (state[0] - state[1]);
    (state, state[1])
}

#[inline(always)]
pub(super) fn beam_reconstruction_error_alpha(osr: u32) -> f64 {
    // Same 44.1 kHz-family assumption as `beam_filtered_error_alpha`.
    beam_one_pole_alpha_for_wire_rate(22_000.0, 44_100.0 * osr.max(1) as f64)
}

#[inline(always)]
pub(super) fn beam_one_pole_alpha_for_wire_rate(corner_hz: f64, wire_rate: f64) -> f64 {
    1.0 - (-2.0 * core::f64::consts::PI * corner_hz / wire_rate).exp()
}

#[inline(always)]
pub(super) fn beam_reconstruction_error_step(
    mut state: [f64; 4],
    error: f64,
    alpha: f64,
) -> ([f64; 4], f64) {
    // First experimental reconstruction metric: four cascaded one-pole low-pass
    // sections (~4th order total) around the audio band at DSD wire rate. This is
    // intentionally a cheap deterministic proxy for in-band reconstruction error,
    // distinct from the older two-pole comparator/quantizer-error filter above.
    state[0] += alpha * (error - state[0]);
    state[1] += alpha * (state[0] - state[1]);
    state[2] += alpha * (state[1] - state[2]);
    state[3] += alpha * (state[2] - state[3]);
    (state, state[3])
}

#[inline(always)]
pub(super) fn beam_alternation_penalty(bits: u64, len: usize, threshold: f64) -> f64 {
    let len = len.min(8);
    if len < 2 {
        return 0.0;
    }
    let mask = (1u64 << (len - 1)) - 1;
    let transitions = ((bits ^ (bits >> 1)) & mask).count_ones();
    let excess = (transitions as f64 / (len - 1) as f64 - threshold).max(0.0);
    excess * excess
}

#[inline(always)]
pub(super) fn beam_periodicity_penalty(
    bits: u64,
    len: usize,
    window: usize,
    lags: &[u8; MAX_BEAM_PERIODICITY_LAGS],
    lag_count: usize,
) -> f64 {
    let len = len.min(window).min(MAX_BEAM_COMMIT_HORIZON);
    if len < 3 {
        return 0.0;
    }
    let mut penalty = 0.0;
    for &lag_u8 in &lags[..lag_count.min(MAX_BEAM_PERIODICITY_LAGS)] {
        let lag = lag_u8 as usize;
        if lag == 0 || lag >= len {
            continue;
        }
        let count = len - lag;
        let mask = low_bits_mask(count);
        let differences = ((bits ^ (bits >> lag)) & mask).count_ones() as f64;
        let density = differences / count as f64;
        let centered = density - 0.5;
        penalty += centered * centered;
    }
    penalty
}

impl CrfbModulator {
    pub fn set_beam_search(&mut self, m: usize, n: usize) {
        let m = m.clamp(1, MAX_BEAM_WIDTH);
        let n = n.clamp(1, MAX_BEAM_COMMIT_HORIZON);
        let mut beam = Box::new(BeamState::new(m, n));
        beam.reseed(self.committed_beam_seed());
        self.beam = Some(beam);
    }

    /// Set the EcBeam terminal rank cost weight. The term is applied only while
    /// sorting the current frontier (`metric + w·state_pressure`) and is not
    /// accumulated into survivor metrics.
    #[doc(hidden)]
    pub fn set_beam_terminal_weight(&mut self, weight: f64) {
        if let Some(beam) = &mut self.beam {
            beam.terminal_weight = if weight.is_finite() && weight >= 0.0 {
                weight
            } else {
                0.0
            };
        }
    }

    /// Set the EcBeam short-run alternation penalty weight. The proxy is the
    /// squared adjacent-transition density over the recent packed beam bits;
    /// it is intentionally cheap and beam-local.
    #[doc(hidden)]
    pub fn set_beam_alternation_weight(&mut self, weight: f64) {
        if let Some(beam) = &mut self.beam {
            beam.alternation_weight = if weight.is_finite() && weight >= 0.0 {
                weight
            } else {
                0.0
            };
        }
    }

    /// Set a rank-only EcBeam short-run alternation penalty weight.
    #[doc(hidden)]
    pub fn set_beam_alternation_rank_weight(&mut self, weight: f64) {
        if let Some(beam) = &mut self.beam {
            beam.alternation_rank_weight = if weight.is_finite() && weight >= 0.0 {
                weight
            } else {
                0.0
            };
        }
    }

    /// Set the transition-density threshold used by EcBeam alternation
    /// penalties. `0.0` reproduces the original density-squared proxy.
    #[doc(hidden)]
    pub fn set_beam_alternation_threshold(&mut self, threshold: f64) {
        if let Some(beam) = &mut self.beam {
            beam.alternation_threshold = if threshold.is_finite() {
                threshold.clamp(0.0, 1.0)
            } else {
                0.0
            };
        }
    }

    /// Set the EcBeam frequency-weighted error metric weight. The metric uses a
    /// per-survivor two-pole low-pass of the quantizer error; `0.0` is the
    /// default and preserves the original beam metric exactly.
    #[doc(hidden)]
    pub fn set_beam_filtered_error_weight(&mut self, weight: f64) {
        if let Some(beam) = &mut self.beam {
            beam.filtered_error_weight = if weight.is_finite() && weight >= 0.0 {
                weight
            } else {
                0.0
            };
        }
    }

    /// Set the rank-only EcBeam frequency-weighted error metric weight. It uses
    /// the same per-survivor low-passed quantizer-error energy as
    /// [`set_beam_filtered_error_weight`](Self::set_beam_filtered_error_weight),
    /// but affects child ordering only.
    #[doc(hidden)]
    pub fn set_beam_filtered_error_rank_weight(&mut self, weight: f64) {
        if let Some(beam) = &mut self.beam {
            beam.filtered_error_rank_weight = if weight.is_finite() && weight >= 0.0 {
                weight
            } else {
                0.0
            };
        }
    }

    /// Set the EcBeam reconstruction-error proxy metric weight. Unlike
    /// [`set_beam_filtered_error_weight`](Self::set_beam_filtered_error_weight),
    /// this filters an in-band proxy of reconstructed output error (`v - input`)
    /// rather than comparator/quantizer error. `0.0` is the default and preserves
    /// the original beam metric exactly.
    #[doc(hidden)]
    pub fn set_beam_reconstruction_error_weight(&mut self, weight: f64) {
        if let Some(beam) = &mut self.beam {
            beam.reconstruction_error_weight = if weight.is_finite() && weight >= 0.0 {
                weight
            } else {
                0.0
            };
        }
    }

    /// Set the EcBeam accumulated-pressure dead-zone. With a positive threshold,
    /// pressure scoring ignores normal program-region state motion and only
    /// accumulates per-stage excess energy above `abs(state_norm) > deadzone`.
    /// `0.0` is the default and preserves the original beam metric exactly.
    #[doc(hidden)]
    pub fn set_beam_pressure_deadzone(&mut self, deadzone: f64) {
        if let Some(beam) = &mut self.beam {
            beam.pressure_deadzone = if deadzone.is_finite() {
                deadzone.clamp(0.0, 1.0)
            } else {
                0.0
            };
        }
    }

    /// Set the EcBeam lag-k periodicity penalty weight. The penalty uses
    /// packed recent beam bits to discourage strong repeated lag-k structure.
    /// `0.0` is the default and preserves the original beam metric exactly.
    #[doc(hidden)]
    pub fn set_beam_periodicity_weight(&mut self, weight: f64) {
        if let Some(beam) = &mut self.beam {
            beam.periodicity_weight = if weight.is_finite() && weight >= 0.0 {
                weight
            } else {
                0.0
            };
        }
    }

    /// Set lag offsets for the EcBeam periodicity penalty. Invalid lags are
    /// ignored; an empty effective list falls back to `[2, 3, 4]`.
    #[doc(hidden)]
    pub fn set_beam_periodicity_lags(&mut self, lags: &[u8]) {
        if let Some(beam) = &mut self.beam {
            let mut selected = [0u8; MAX_BEAM_PERIODICITY_LAGS];
            let mut count = 0usize;
            for &lag in lags {
                if lag == 0 || lag as usize >= MAX_BEAM_COMMIT_HORIZON {
                    continue;
                }
                if selected[..count].contains(&lag) {
                    continue;
                }
                selected[count] = lag;
                count += 1;
                if count == MAX_BEAM_PERIODICITY_LAGS {
                    break;
                }
            }
            if count == 0 {
                beam.periodicity_lags = EC_BEAM_PERIODICITY_DEFAULT_LAGS;
                beam.periodicity_lag_count = EC_BEAM_PERIODICITY_DEFAULT_LAG_COUNT;
            } else {
                selected[..count].sort_unstable();
                beam.periodicity_lags = selected;
                beam.periodicity_lag_count = count;
            }
        }
    }

    /// Set the recent-bit window used by the EcBeam periodicity penalty.
    #[doc(hidden)]
    pub fn set_beam_periodicity_window(&mut self, window: usize) {
        if let Some(beam) = &mut self.beam {
            beam.periodicity_window = window.clamp(2, MAX_BEAM_COMMIT_HORIZON);
        }
    }

    /// Set experimental pressure/DC accumulated-vs-rank split scales and opt in
    /// to the metric-hygiene beam path. A value of `1.0` applies the configured
    /// EC2 weight in that lane; `0.0` disables that lane. When this setter is
    /// not called, EcBeam keeps the legacy accumulated pressure/DC metric and
    /// soft post-selection state clamping.
    #[doc(hidden)]
    pub fn set_beam_auxiliary_metric_scales(
        &mut self,
        pressure_accum_scale: f64,
        pressure_rank_scale: f64,
        dc_accum_scale: f64,
        dc_rank_scale: f64,
    ) {
        if let Some(beam) = &mut self.beam {
            beam.metric_mode = EcBeamMetricMode::MetricHygiene;
            beam.pressure_accum_scale = sanitize_beam_metric_scale(pressure_accum_scale);
            beam.pressure_rank_scale = sanitize_beam_metric_scale(pressure_rank_scale);
            beam.dc_accum_scale = sanitize_beam_metric_scale(dc_accum_scale);
            beam.dc_rank_scale = sanitize_beam_metric_scale(dc_rank_scale);
        }
    }

    /// Set the EcBeam metric mode. Use
    /// [`EcBeamMetricMode::PathConsistent`] to make Top-M survivor pruning
    /// depend only on accumulated path metric and deterministic tie-breaks.
    #[doc(hidden)]
    pub fn set_beam_metric_mode(&mut self, mode: EcBeamMetricMode) {
        if let Some(beam) = &mut self.beam {
            beam.metric_mode = mode;
        }
    }

    #[doc(hidden)]
    pub fn beam_metric_mode(&self) -> Option<EcBeamMetricMode> {
        self.beam.as_ref().map(|beam| beam.metric_mode)
    }

    /// Set how EcBeam handles candidates whose predicted normalized state
    /// exceeds the hard integrator limit. Defaults to
    /// [`EcBeamClampPolicy::LegacyClampAndContinue`] for anchor compatibility.
    #[doc(hidden)]
    pub fn set_beam_clamp_policy(&mut self, policy: EcBeamClampPolicy) {
        if let Some(beam) = &mut self.beam {
            beam.clamp_policy = policy;
        }
    }

    #[doc(hidden)]
    pub fn beam_clamp_policy(&self) -> Option<EcBeamClampPolicy> {
        self.beam.as_ref().map(|beam| beam.clamp_policy)
    }

    /// Enable or disable the expensive rank-vs-accumulated metric diagnostics.
    /// Disabled by default so production sweeps do not pay for the second Top-M
    /// sort unless they are explicitly studying rank-only pruning effects.
    #[doc(hidden)]
    pub fn set_beam_metric_diagnostics_enabled(&mut self, enabled: bool) {
        if let Some(beam) = &mut self.beam {
            beam.collect_metric_diagnostics = enabled;
            if !enabled {
                beam.metric_diagnostics = BeamMetricDiagnostics::default();
            }
        }
    }

    #[doc(hidden)]
    pub fn beam_metric_diagnostics_enabled(&self) -> Option<bool> {
        self.beam
            .as_ref()
            .map(|beam| beam.collect_metric_diagnostics)
    }

    /// Deactivate the EcBeam prototype, restoring the configured lookahead
    /// search. Any delayed bits still buffered are dropped; call
    /// [`clear_beam_search_flushing`](Self::clear_beam_search_flushing) when
    /// delayed bits must be preserved.
    #[doc(hidden)]
    pub fn clear_beam_search(&mut self) {
        self.beam = None;
    }

    /// Flush delayed EcBeam bits, then deactivate beam search.
    #[doc(hidden)]
    pub fn clear_beam_search_flushing(&mut self, out_bits: &mut Vec<u8>) {
        if self.mode == ModulatorMode::Ec && self.beam.is_some() {
            self.flush_beam(out_bits);
        }
        self.beam = None;
    }

    /// `(m, n)` while the EcBeam prototype is active.
    #[doc(hidden)]
    pub fn beam_search(&self) -> Option<(usize, usize)> {
        self.beam.as_ref().map(|beam| (beam.m, beam.n))
    }

    /// EcBeam output latency in samples (`n - 1`) while active.
    #[doc(hidden)]
    pub fn beam_latency_samples(&self) -> Option<usize> {
        self.beam.as_ref().map(|beam| beam.n.saturating_sub(1))
    }

    /// Un-emitted delayed bits currently buffered by EcBeam.
    #[doc(hidden)]
    pub fn beam_buffered_samples(&self) -> Option<usize> {
        self.beam.as_ref().map(|beam| beam.buffered)
    }

    /// Current normalized pressure of the best EcBeam survivor.
    #[doc(hidden)]
    pub fn beam_best_state_pressure(&self) -> Option<f64> {
        self.beam.as_ref().map(|beam| {
            max_abs7(&mul8(
                &beam.parents[beam.parents_bank][0].state,
                &self.inverse_state_limit,
            ))
        })
    }

    /// Per-stage normalized pressure of the best EcBeam survivor.
    #[doc(hidden)]
    pub fn beam_best_state_pressure_by_stage(&self) -> Option<[f64; 7]> {
        self.beam.as_ref().map(|beam| {
            let mut out = [0.0; 7];
            for (idx, value) in out.iter_mut().enumerate() {
                *value = beam.parents[beam.parents_bank][0].state[idx].abs()
                    * self.inverse_state_limit[idx];
            }
            out
        })
    }

    /// Loop output of the best EcBeam survivor for a diagnostic input.
    #[doc(hidden)]
    pub fn beam_best_loop_output_for_input(&self, input: f64) -> Option<f64> {
        self.beam.as_ref().map(|beam| {
            if self.crfb_sparse {
                self.loop_output::<true>(&beam.parents[beam.parents_bank][0].state, input)
            } else {
                self.loop_output::<false>(&beam.parents[beam.parents_bank][0].state, input)
            }
        })
    }

    /// Current EcBeam terminal rank weight.
    #[doc(hidden)]
    pub fn beam_terminal_weight(&self) -> Option<f64> {
        self.beam.as_ref().map(|beam| beam.terminal_weight)
    }

    /// Current EcBeam short-run alternation penalty weight.
    #[doc(hidden)]
    pub fn beam_alternation_weight(&self) -> Option<f64> {
        self.beam.as_ref().map(|beam| beam.alternation_weight)
    }

    /// Current EcBeam rank-only short-run alternation penalty weight.
    #[doc(hidden)]
    pub fn beam_alternation_rank_weight(&self) -> Option<f64> {
        self.beam.as_ref().map(|beam| beam.alternation_rank_weight)
    }

    /// Current EcBeam alternation penalty threshold.
    #[doc(hidden)]
    pub fn beam_alternation_threshold(&self) -> Option<f64> {
        self.beam.as_ref().map(|beam| beam.alternation_threshold)
    }

    /// Current EcBeam frequency-weighted error metric weight.
    #[doc(hidden)]
    pub fn beam_filtered_error_weight(&self) -> Option<f64> {
        self.beam.as_ref().map(|beam| beam.filtered_error_weight)
    }

    /// Current rank-only EcBeam frequency-weighted error metric weight.
    #[doc(hidden)]
    pub fn beam_filtered_error_rank_weight(&self) -> Option<f64> {
        self.beam
            .as_ref()
            .map(|beam| beam.filtered_error_rank_weight)
    }

    /// Current EcBeam reconstruction-error proxy metric weight.
    #[doc(hidden)]
    pub fn beam_reconstruction_error_weight(&self) -> Option<f64> {
        self.beam
            .as_ref()
            .map(|beam| beam.reconstruction_error_weight)
    }

    #[doc(hidden)]
    pub fn beam_pressure_deadzone(&self) -> Option<f64> {
        self.beam.as_ref().map(|beam| beam.pressure_deadzone)
    }

    #[doc(hidden)]
    pub fn beam_periodicity_weight(&self) -> Option<f64> {
        self.beam.as_ref().map(|beam| beam.periodicity_weight)
    }

    #[doc(hidden)]
    pub fn beam_periodicity_lags(&self) -> Option<Vec<u8>> {
        self.beam
            .as_ref()
            .map(|beam| beam.periodicity_lags[..beam.periodicity_lag_count].to_vec())
    }

    #[doc(hidden)]
    pub fn beam_periodicity_lag_count(&self) -> Option<usize> {
        self.beam.as_ref().map(|beam| beam.periodicity_lag_count)
    }

    #[doc(hidden)]
    pub fn beam_periodicity_window(&self) -> Option<usize> {
        self.beam.as_ref().map(|beam| beam.periodicity_window)
    }

    /// Whether the coefficient table matched the CRFB band-sparsity pattern
    /// (the monomorphized sparse hot path). Exposed so benchmarks can assert
    /// they are not silently measuring the dense fallback.
    #[doc(hidden)]
    pub fn crfb_sparse(&self) -> bool {
        self.crfb_sparse
    }

    /// Cumulative EcBeam decision-trace counters (§13/§20).
    #[doc(hidden)]
    pub fn beam_diagnostics(&self) -> Option<BeamDiagnostics> {
        self.beam.as_ref().map(|beam| beam.diagnostics)
    }

    /// Cumulative EcBeam reconstruction-error metric diagnostics.
    #[doc(hidden)]
    pub fn beam_reconstruction_diagnostics(&self) -> Option<BeamReconstructionDiagnostics> {
        self.beam
            .as_ref()
            .map(|beam| beam.reconstruction_diagnostics)
    }

    /// Cumulative EcBeam lag-k periodicity metric diagnostics.
    #[doc(hidden)]
    pub fn beam_periodicity_diagnostics(&self) -> Option<BeamPeriodicityDiagnostics> {
        self.beam.as_ref().map(|beam| beam.periodicity_diagnostics)
    }

    /// Cumulative EcBeam rank-vs-accumulated metric diagnostics.
    #[doc(hidden)]
    pub fn beam_metric_diagnostics(&self) -> Option<BeamMetricDiagnostics> {
        self.beam.as_ref().map(|beam| beam.metric_diagnostics)
    }

    /// A fresh single-survivor beam at the committed modulator state.
    pub(super) fn committed_beam_seed(&self) -> BeamPath {
        BeamPath {
            state: self.state,
            metric: 0.0,
            prev_v: self.prev_v,
            dc_bias: self.dc_bias,
            bits: 0,
            clamp_bits: 0,
        }
    }

    pub(super) fn process_beam_block<const SPARSE: bool>(
        &mut self,
        input: &[f64],
        out_bits: &mut Vec<u8>,
    ) {
        let beam = self.beam.take().expect("beam dispatch requires beam state");
        if SPARSE && self.beam_m4n8_plain_eligible(&beam) {
            self.process_beam_block_m4n8_plain::<SPARSE>(beam, input, out_bits);
        } else if SPARSE && self.beam_m4n8_a1_simd_eligible(&beam) {
            #[cfg(target_arch = "aarch64")]
            self.process_beam_block_m4n8_a1_simd(beam, input, out_bits);
            #[cfg(not(target_arch = "aarch64"))]
            self.process_beam_block_m4n8_ranked::<SPARSE>(beam, input, out_bits);
        } else if SPARSE && self.beam_m4n8_ranked_eligible(&beam) {
            self.process_beam_block_m4n8_ranked::<SPARSE>(beam, input, out_bits);
        } else {
            self.process_beam_block_generic::<SPARSE>(beam, input, out_bits);
        }
    }

    #[inline(always)]
    pub(super) fn beam_m4n8_plain_eligible(&self, beam: &BeamState) -> bool {
        #[cfg(test)]
        if beam.force_generic_path {
            return false;
        }

        beam.m == 4
            && beam.n == 8
            && !self.effective_dither_active()
            && self.isi_penalty == 0.0
            && beam.terminal_weight == 0.0
            && beam.alternation_weight == 0.0
            && beam.alternation_rank_weight == 0.0
            && beam.filtered_error_weight == 0.0
            && beam.filtered_error_rank_weight == 0.0
            && beam.reconstruction_error_weight == 0.0
            && beam.pressure_deadzone == 0.0
            && beam.periodicity_weight == 0.0
            && !matches!(beam.metric_mode, EcBeamMetricMode::MetricHygiene)
            && matches!(beam.clamp_policy, EcBeamClampPolicy::LegacyClampAndContinue)
            && !beam.collect_metric_diagnostics
    }

    #[inline(always)]
    pub(super) fn beam_m4n8_ranked_eligible(&self, beam: &BeamState) -> bool {
        #[cfg(test)]
        if beam.force_generic_path {
            return false;
        }

        beam.m == 4
            && beam.n == 8
            && !self.effective_dither_active()
            && self.isi_penalty == 0.0
            && beam.alternation_rank_weight == 0.0
            && beam.filtered_error_weight == 0.0
            && beam.filtered_error_rank_weight == 0.0
            && beam.reconstruction_error_weight == 0.0
            && beam.pressure_deadzone == 0.0
            && beam.periodicity_weight == 0.0
            && matches!(beam.metric_mode, EcBeamMetricMode::HybridRankNudged)
            && matches!(beam.clamp_policy, EcBeamClampPolicy::LegacyClampAndContinue)
            && !beam.collect_metric_diagnostics
    }

    #[inline(always)]
    pub(super) fn beam_m4n8_a1_simd_eligible(&self, beam: &BeamState) -> bool {
        #[cfg(not(target_arch = "aarch64"))]
        {
            let _ = beam;
            false
        }
        #[cfg(target_arch = "aarch64")]
        {
            self.beam_m4n8_ranked_eligible(beam)
                && self.pressure_stage_weighted
                && beam.terminal_weight > 0.0
                && beam.alternation_weight > 0.0
                && beam.alternation_threshold == 0.0
        }
    }

    #[cfg(test)]
    pub(super) fn set_beam_force_generic_path(&mut self, enabled: bool) {
        if let Some(beam) = &mut self.beam {
            beam.force_generic_path = enabled;
        }
    }

    pub(super) fn process_beam_block_generic<const SPARSE: bool>(
        &mut self,
        mut beam: Box<BeamState>,
        input: &[f64],
        out_bits: &mut Vec<u8>,
    ) {
        beam.m4n8_norm_valid = false;
        // The beam lives in a block-local so the per-sample loop never fights
        // the borrow of `self` (and never round-trips the Box).
        let filtered_error_active =
            beam.filtered_error_weight > 0.0 || beam.filtered_error_rank_weight > 0.0;
        let filtered_error_alpha = if filtered_error_active {
            beam_filtered_error_alpha(self.coeffs.osr)
        } else {
            0.0
        };
        let reconstruction_error_active = beam.reconstruction_error_weight > 0.0;
        let reconstruction_error_alpha = if reconstruction_error_active {
            beam_reconstruction_error_alpha(self.coeffs.osr)
        } else {
            0.0
        };
        if filtered_error_active || reconstruction_error_active {
            beam.ensure_aux_state();
        }
        beam.ring_index = (beam.sample_index % beam.n as u64) as usize;
        let dither_active = self.effective_dither_active();
        if dither_active {
            for &u in input {
                self.process_beam_sample::<SPARSE, true>(
                    &mut beam,
                    u,
                    out_bits,
                    filtered_error_alpha,
                    reconstruction_error_alpha,
                );
            }
        } else {
            for &u in input {
                self.process_beam_sample::<SPARSE, false>(
                    &mut beam,
                    u,
                    out_bits,
                    filtered_error_alpha,
                    reconstruction_error_alpha,
                );
            }
        }
        self.beam = Some(beam);
    }

    pub(super) fn process_beam_block_m4n8_plain<const SPARSE: bool>(
        &mut self,
        mut beam: Box<BeamState>,
        input: &[f64],
        out_bits: &mut Vec<u8>,
    ) {
        beam.m4n8_norm_valid = false;
        debug_assert!(self.beam_m4n8_plain_eligible(&beam));
        debug_assert!(SPARSE);
        for &u in input {
            self.process_beam_sample_m4n8_plain::<SPARSE, false>(&mut beam, u, out_bits);
        }
        self.beam = Some(beam);
    }

    pub(super) fn process_beam_block_m4n8_ranked<const SPARSE: bool>(
        &mut self,
        mut beam: Box<BeamState>,
        input: &[f64],
        out_bits: &mut Vec<u8>,
    ) {
        beam.m4n8_norm_valid = false;
        debug_assert!(self.beam_m4n8_ranked_eligible(&beam));
        debug_assert!(SPARSE);
        for &u in input {
            self.process_beam_sample_m4n8_plain::<SPARSE, true>(&mut beam, u, out_bits);
        }
        self.beam = Some(beam);
    }

    #[cfg(target_arch = "aarch64")]
    pub(super) fn process_beam_block_m4n8_a1_simd(
        &mut self,
        mut beam: Box<BeamState>,
        input: &[f64],
        out_bits: &mut Vec<u8>,
    ) {
        debug_assert!(self.beam_m4n8_a1_simd_eligible(&beam));
        self.ensure_m4n8_normalized_state(&mut beam);
        for &u in input {
            self.process_beam_sample_m4n8_a1_simd(&mut beam, u, out_bits);
        }
        self.sync_m4n8_raw_state(&mut beam);
        self.beam = Some(beam);
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    pub(super) fn ensure_m4n8_normalized_state(&self, beam: &mut BeamState) {
        if beam.m4n8_norm_valid {
            return;
        }
        let bank = beam.parents_bank;
        for stage in 0..8 {
            beam.m4n8_norm_state[bank][stage] = [0.0; 4];
        }
        for parent_idx in 0..beam.parents_len {
            for stage in 0..7 {
                beam.m4n8_norm_state[bank][stage][parent_idx] =
                    beam.parents[bank][parent_idx].state[stage] * self.inverse_state_limit[stage];
            }
        }
        beam.m4n8_norm_valid = true;
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn sync_m4n8_raw_state(&self, beam: &mut BeamState) {
        if !beam.m4n8_norm_valid {
            return;
        }
        let bank = beam.parents_bank;
        for parent_idx in 0..beam.parents_len {
            for stage in 0..7 {
                beam.parents[bank][parent_idx].state[stage] =
                    beam.m4n8_norm_state[bank][stage][parent_idx] * self.state_limit8[stage];
            }
            beam.parents[bank][parent_idx].state[7] = 0.0;
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    pub(super) fn predict_m4n8_a1_frontier(
        &self,
        beam: &mut BeamState,
        parent_bank: usize,
        u: f64,
    ) -> ([f64; 4], [f64; 4], [f64; 4], [bool; 4]) {
        use core::arch::aarch64::*;

        let mut y = [0.0; 4];
        let mut ps = [0.0; 4];
        let mut pt = [0.0; 4];
        let mut hot = [false; 4];
        // SAFETY: NEON is baseline on aarch64. Both two-lane loads in each
        // four-element stage array are in bounds.
        unsafe {
            for offset in [0usize, 2] {
                let s0 = vld1q_f64(beam.m4n8_norm_state[parent_bank][0].as_ptr().add(offset));
                let s1 = vld1q_f64(beam.m4n8_norm_state[parent_bank][1].as_ptr().add(offset));
                let s2 = vld1q_f64(beam.m4n8_norm_state[parent_bank][2].as_ptr().add(offset));
                let s3 = vld1q_f64(beam.m4n8_norm_state[parent_bank][3].as_ptr().add(offset));
                let s4 = vld1q_f64(beam.m4n8_norm_state[parent_bank][4].as_ptr().add(offset));
                let s5 = vld1q_f64(beam.m4n8_norm_state[parent_bank][5].as_ptr().add(offset));
                let s6 = vld1q_f64(beam.m4n8_norm_state[parent_bank][6].as_ptr().add(offset));

                let mut b0 = vdupq_n_f64(self.bu_norm[0] * u);
                b0 = vfmaq_n_f64(b0, s0, self.a_rows_norm[0][0]);
                let mut b1 = vdupq_n_f64(self.bu_norm[1] * u);
                b1 = vfmaq_n_f64(b1, s0, self.a_rows_norm[1][0]);
                b1 = vfmaq_n_f64(b1, s1, self.a_rows_norm[1][1]);
                b1 = vfmaq_n_f64(b1, s2, self.a_rows_norm[1][2]);
                let mut b2 = vdupq_n_f64(self.bu_norm[2] * u);
                b2 = vfmaq_n_f64(b2, s0, self.a_rows_norm[2][0]);
                b2 = vfmaq_n_f64(b2, s1, self.a_rows_norm[2][1]);
                b2 = vfmaq_n_f64(b2, s2, self.a_rows_norm[2][2]);
                let mut b3 = vdupq_n_f64(self.bu_norm[3] * u);
                b3 = vfmaq_n_f64(b3, s2, self.a_rows_norm[3][2]);
                b3 = vfmaq_n_f64(b3, s3, self.a_rows_norm[3][3]);
                b3 = vfmaq_n_f64(b3, s4, self.a_rows_norm[3][4]);
                let mut b4 = vdupq_n_f64(self.bu_norm[4] * u);
                b4 = vfmaq_n_f64(b4, s2, self.a_rows_norm[4][2]);
                b4 = vfmaq_n_f64(b4, s3, self.a_rows_norm[4][3]);
                b4 = vfmaq_n_f64(b4, s4, self.a_rows_norm[4][4]);
                let mut b5 = vdupq_n_f64(self.bu_norm[5] * u);
                b5 = vfmaq_n_f64(b5, s4, self.a_rows_norm[5][4]);
                b5 = vfmaq_n_f64(b5, s5, self.a_rows_norm[5][5]);
                b5 = vfmaq_n_f64(b5, s6, self.a_rows_norm[5][6]);
                let mut b6 = vdupq_n_f64(self.bu_norm[6] * u);
                b6 = vfmaq_n_f64(b6, s4, self.a_rows_norm[6][4]);
                b6 = vfmaq_n_f64(b6, s5, self.a_rows_norm[6][5]);
                b6 = vfmaq_n_f64(b6, s6, self.a_rows_norm[6][6]);
                let bases = [b0, b1, b2, b3, b4, b5, b6];
                for (stage, base) in bases.into_iter().enumerate() {
                    vst1q_f64(beam.m4n8_base_norm[stage].as_mut_ptr().add(offset), base);
                }

                let mut ps_vec = vdupq_n_f64(0.0);
                let mut pt_vec = vdupq_n_f64(0.0);
                let mut hot_vec = vdupq_n_u64(0);
                for (stage, base) in bases.into_iter().enumerate() {
                    let q = vmulq_f64(base, base);
                    ps_vec = vfmaq_n_f64(ps_vec, q, self.pressure_stage_weight[stage]);
                    pt_vec = vfmaq_n_f64(
                        pt_vec,
                        base,
                        self.pressure_stage_weight[stage] * self.bv_norm[stage],
                    );
                    hot_vec =
                        vorrq_u64(hot_vec, vcgtq_f64(q, vdupq_n_f64(self.knee_thr_sq[stage])));
                }
                let y_vec = vfmaq_n_f64(vdupq_n_f64(self.coeffs.d1 * u), s6, self.c_row_norm[6]);
                vst1q_f64(y.as_mut_ptr().add(offset), y_vec);
                vst1q_f64(ps.as_mut_ptr().add(offset), ps_vec);
                vst1q_f64(pt.as_mut_ptr().add(offset), pt_vec);
                hot[offset] = vgetq_lane_u64::<0>(hot_vec) != 0;
                hot[offset + 1] = vgetq_lane_u64::<1>(hot_vec) != 0;
            }
        }
        (y, ps, pt, hot)
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    #[allow(clippy::needless_range_loop, clippy::too_many_arguments)]
    fn beam_m4n8_a1_score_pair(
        &self,
        y_quantized: f64,
        base_norm: Option<&[f64; 8]>,
        parent_prev_v: f64,
        parent_dc_bias: f64,
        ps: f64,
        pt: f64,
        hot: bool,
    ) -> ([f64; 2], [f64; 2]) {
        let pq = self.pbv_sq_sum;
        let pressure_plus = ps + 2.0 * pt + pq;
        let pressure_minus = ps - 2.0 * pt + pq;
        if !hot {
            let d = self.dc_bias_decay * parent_dc_bias;
            let e = 1.0 - self.dc_bias_decay;
            let common = self.ec2_weights.quantizer_weight * (y_quantized * y_quantized + 1.0)
                + self.ec2_weights.pressure_weight * (ps + pq)
                + self.ec2_weights.transition_weight * 0.5
                + self.ec2_weights.dc_weight * (d * d + e * e);
            let delta = 2.0
                * (self.ec2_weights.pressure_weight * pt
                    - self.ec2_weights.quantizer_weight * y_quantized
                    + self.ec2_weights.dc_weight * d * e)
                - self.ec2_weights.transition_weight * 0.5 * parent_prev_v;
            return (
                [common + delta, common - delta],
                [pressure_plus, pressure_minus],
            );
        }

        let base_norm = base_norm.expect("hot A1 score requires base state");
        let calc_lane = |v: f64, pressure: f64| {
            let mut limit_penalty = 0.0;
            for stage in 0..7 {
                let normalized = v.mul_add(self.bv_norm[stage], base_norm[stage]).abs();
                let approach = ((normalized - EC_STATE_LIMIT_SOFT_KNEE)
                    * EC_STATE_LIMIT_SOFT_KNEE_INV_SPAN)
                    .max(0.0);
                let overflow = (normalized - 1.0).max(0.0);
                limit_penalty =
                    approach.mul_add(approach, limit_penalty) + 24.0 * overflow * overflow;
            }
            let quantizer_error = y_quantized - v;
            let transition = if v != parent_prev_v { 1.0 } else { 0.0 };
            let next_bias = self.updated_dc_bias(parent_dc_bias, v);
            self.ec2_weights.quantizer_weight * quantizer_error * quantizer_error
                + self.ec2_weights.pressure_weight * pressure
                + self.ec2_weights.limit_weight * limit_penalty
                + self.ec2_weights.transition_weight * transition
                + self.ec2_weights.dc_weight * next_bias * next_bias
        };
        (
            [
                calc_lane(1.0, pressure_plus),
                calc_lane(-1.0, pressure_minus),
            ],
            [pressure_plus, pressure_minus],
        )
    }

    #[cfg(target_arch = "aarch64")]
    #[allow(clippy::needless_range_loop)]
    fn process_beam_sample_m4n8_a1_simd(
        &mut self,
        beam: &mut BeamState,
        u: f64,
        out_bits: &mut Vec<u8>,
    ) {
        const M: usize = 4;
        const N: usize = 8;
        const BITS_MASK: u64 = 0xff;
        const SHIFT: usize = N - 1;

        if !u.is_finite() {
            self.sync_m4n8_raw_state(beam);
            self.beam_emit_buffered(beam, out_bits);
            self.hard_reset();
            self.stability_resets = self.stability_resets.wrapping_add(1);
            push_beam_bit_unchecked(out_bits, 1);
            beam.diagnostics.emit_count += 1;
            #[cfg(feature = "ecbeam2_observer")]
            super::ecbeam2_observer::observe_recovery_bit(beam, 0.0, true);
            beam.reseed_after_recovery(self.committed_beam_seed());
            self.ensure_m4n8_normalized_state(beam);
            return;
        }

        let parent_bank = beam.parents_bank;
        let materialize_bank = parent_bank ^ 1;
        debug_assert!(parent_bank < 2);
        debug_assert!(beam.parents_len <= M);
        let (y, ps, pt, hot) = self.predict_m4n8_a1_frontier(beam, parent_bank, u);
        let history_len = (beam.buffered + 1).min(N);
        let transition_mask = M4N8_TRANSITION_MASK[history_len];
        let alternation_penalties = &M4N8_ALTERNATION_PENALTY[history_len];
        let alternation_weight = beam.alternation_weight;
        let terminal_weight = beam.terminal_weight;
        let mut children = 0usize;
        for parent_idx in 0..beam.parents_len {
            // Copy only the metadata used by the normalized kernel. Copying a
            // full `BeamPath` would also move its dormant 64-byte raw state.
            let parent_metric = beam.parents[parent_bank][parent_idx].metric;
            let parent_prev_v = beam.parents[parent_bank][parent_idx].prev_v;
            let parent_dc_bias = beam.parents[parent_bank][parent_idx].dc_bias;
            let parent_bits = beam.parents[parent_bank][parent_idx].bits;
            let mut hot_base = [0.0; 8];
            if hot[parent_idx] {
                for stage in 0..7 {
                    hot_base[stage] = beam.m4n8_base_norm[stage][parent_idx];
                }
            }
            let ([c_plus, c_minus], [p_plus, p_minus]) = self.beam_m4n8_a1_score_pair(
                y[parent_idx],
                hot[parent_idx].then_some(&hot_base),
                parent_prev_v,
                parent_dc_bias,
                ps[parent_idx],
                pt[parent_idx],
                hot[parent_idx],
            );
            for (v, legacy_cost, terminal_pressure) in
                [(1.0, c_plus, p_plus), (-1.0, c_minus, p_minus)]
            {
                let bits = ((parent_bits << 1) | u64::from(v > 0.0)) & BITS_MASK;
                let transitions = ((bits ^ (bits >> 1)) & transition_mask).count_ones() as usize;
                let alternation_penalty = alternation_penalties[transitions];
                let metric = parent_metric + legacy_cost + alternation_weight * alternation_penalty;
                let rank_metric = metric + terminal_weight * terminal_pressure;
                if !rank_metric.is_finite() {
                    continue;
                }
                // SAFETY: M4 expands at most eight children and every scratch
                // array has capacity `2 * MAX_BEAM_WIDTH` (32).
                unsafe {
                    *beam.child_metric.get_unchecked_mut(children) = metric;
                    *beam.child_rank_metric.get_unchecked_mut(children) = rank_metric;
                    *beam.child_bits.get_unchecked_mut(children) = bits;
                    *beam.child_parent.get_unchecked_mut(children) = parent_idx as u8;
                    *beam.child_v.get_unchecked_mut(children) = v;
                }
                children += 1;
            }
        }

        if children == 0 {
            beam.diagnostics.beam_all_children_rejected_total = beam
                .diagnostics
                .beam_all_children_rejected_total
                .wrapping_add(1);
            self.sync_m4n8_raw_state(beam);
            self.beam_emit_buffered(beam, out_bits);
            self.hard_reset();
            self.stability_resets = self.stability_resets.wrapping_add(1);
            push_beam_bit_unchecked(out_bits, 1);
            beam.diagnostics.emit_count += 1;
            #[cfg(feature = "ecbeam2_observer")]
            super::ecbeam2_observer::observe_recovery_bit(beam, u, false);
            beam.reseed_after_recovery(self.committed_beam_seed());
            self.ensure_m4n8_normalized_state(beam);
            return;
        }

        let mut order = [0u8; M];
        let keep = select_top4_beam_children(
            children,
            &beam.child_rank_metric,
            &beam.child_bits,
            &mut order,
        );
        #[cfg(feature = "ecbeam2_observer")]
        super::ecbeam2_observer::observe_authoritative_frontier(
            beam,
            u,
            parent_bank,
            children,
            &order[..keep],
            &self.bv_norm,
            &self.state_limit8,
            self.isi_penalty,
            true,
        );
        for (slot, &child_u8) in order[..keep].iter().enumerate() {
            let child = child_u8 as usize;
            // SAFETY: the selector returns only indices below `children`.
            let parent_idx = unsafe { *beam.child_parent.get_unchecked(child) as usize };
            let parent_dc_bias = beam.parents[parent_bank][parent_idx].dc_bias;
            let parent_clamp_bits = beam.parents[parent_bank][parent_idx].clamp_bits;
            let v = unsafe { *beam.child_v.get_unchecked(child) };
            let clamped = if hot[parent_idx] {
                let mut state_norm = [0.0; 8];
                for stage in 0..7 {
                    state_norm[stage] =
                        v.mul_add(self.bv_norm[stage], beam.m4n8_base_norm[stage][parent_idx]);
                }
                match stabilize_normalized_state(&mut state_norm) {
                    StateStability::Ok { clamped } => {
                        for stage in 0..7 {
                            beam.m4n8_norm_state[materialize_bank][stage][slot] = state_norm[stage];
                        }
                        clamped
                    }
                    StateStability::Reset => {
                        for stage in 0..7 {
                            beam.m4n8_norm_state[materialize_bank][stage][slot] = 0.0;
                        }
                        true
                    }
                }
            } else {
                // The cold path is overwhelmingly common. Write directly to
                // the canonical SoA bank instead of building and copying an
                // intermediate `[f64; 8]`.
                for stage in 0..7 {
                    beam.m4n8_norm_state[materialize_bank][stage][slot] =
                        v.mul_add(self.bv_norm[stage], beam.m4n8_base_norm[stage][parent_idx]);
                }
                false
            };
            beam.m4n8_norm_state[materialize_bank][7][slot] = 0.0;
            beam.diagnostics.beam_clamp_total += u64::from(clamped);
            // Normalized SoA state is canonical inside the block. Avoid
            // writing the unused 64-byte raw state for every winner; block-end
            // synchronization fills it once for the live survivors.
            let survivor = &mut beam.parents[materialize_bank][slot];
            survivor.metric = unsafe { *beam.child_metric.get_unchecked(child) };
            survivor.prev_v = v;
            survivor.dc_bias = self.updated_dc_bias(parent_dc_bias, v);
            survivor.bits = unsafe { *beam.child_bits.get_unchecked(child) };
            survivor.clamp_bits = ((parent_clamp_bits << 1) | u64::from(clamped)) & BITS_MASK;
        }
        beam.diagnostics.path_switches +=
            u64::from(unsafe { *beam.child_parent.get_unchecked(order[0] as usize) != 0 });
        beam.parents_bank = materialize_bank;
        beam.aux_valid = false;
        beam.parents_len = keep;

        let ring_slot = (beam.sample_index & 7) as usize;
        beam.frontier_ring[ring_slot] = (beam.parents[beam.parents_bank][0].bits & 1) as u8;
        beam.buffered += 1;
        if beam.buffered == N {
            let parent_bank = beam.parents_bank;
            let best = &beam.parents[parent_bank][0];
            let bit = ((best.bits >> SHIFT) & 1) as u8;
            if (best.clamp_bits >> SHIFT) & 1 == 1 {
                self.state_clamps = self.state_clamps.wrapping_add(1);
                beam.diagnostics.beam_committed_clamp_total =
                    beam.diagnostics.beam_committed_clamp_total.wrapping_add(1);
            }
            push_beam_bit_unchecked(out_bits, bit);
            beam.diagnostics.emit_count += 1;
            let frontier_slot = ((beam.sample_index + 1) & 7) as usize;
            beam.diagnostics.delayed_flips += u64::from(bit != beam.frontier_ring[frontier_slot]);
            let mut kept = 0usize;
            let mut pruned = 0u64;
            for idx in 0..beam.parents_len {
                let parent = beam.parents[parent_bank][idx];
                if ((parent.bits >> SHIFT) & 1) as u8 == bit {
                    if kept != idx {
                        let survivor = &mut beam.parents[parent_bank][kept];
                        survivor.metric = parent.metric;
                        survivor.prev_v = parent.prev_v;
                        survivor.dc_bias = parent.dc_bias;
                        survivor.bits = parent.bits;
                        survivor.clamp_bits = parent.clamp_bits;
                        for stage in 0..8 {
                            beam.m4n8_norm_state[parent_bank][stage][kept] =
                                beam.m4n8_norm_state[parent_bank][stage][idx];
                        }
                    }
                    kept += 1;
                } else {
                    pruned = pruned.wrapping_add(1);
                }
            }
            beam.diagnostics.pruned_total = beam.diagnostics.pruned_total.wrapping_add(pruned);
            beam.parents_len = kept;
            beam.buffered -= 1;
        }
        beam.diagnostics.min_survivors =
            beam.diagnostics.min_survivors.min(beam.parents_len as u64);

        // A common metric offset cannot affect frontier ordering. Renormalize
        // periodically instead of spending a min scan plus four subtractions
        // on every DSD sample; 256 steps remains tiny relative to f64 range and
        // precision for this bounded scorer.
        if beam.sample_index & 0xff == 0xff {
            let parents = &mut beam.parents[beam.parents_bank];
            let mut min_metric = parents[0].metric;
            for parent in &parents[1..beam.parents_len] {
                min_metric = min_metric.min(parent.metric);
            }
            for parent in &mut parents[..beam.parents_len] {
                parent.metric -= min_metric;
            }
        }
        beam.sample_index += 1;
    }

    /// M4/N8 EcBeam hot path for the production zero-dither/zero-ISI case.
    /// `RANKED` adds the A1 terminal rank nudge and accumulated alternation
    /// term while keeping the generic path as the behavior oracle.
    #[allow(clippy::needless_range_loop)]
    pub(super) fn process_beam_sample_m4n8_plain<const SPARSE: bool, const RANKED: bool>(
        &mut self,
        beam: &mut BeamState,
        u: f64,
        out_bits: &mut Vec<u8>,
    ) {
        const PLAIN_M: usize = 4;
        const PLAIN_N: usize = 8;
        const PLAIN_BITS_MASK: u64 = 0xff;
        const PLAIN_SHIFT: usize = PLAIN_N - 1;

        if !u.is_finite() {
            self.beam_emit_buffered(beam, out_bits);
            self.hard_reset();
            self.stability_resets = self.stability_resets.wrapping_add(1);
            out_bits.push(1);
            beam.diagnostics.emit_count += 1;
            #[cfg(feature = "ecbeam2_observer")]
            super::ecbeam2_observer::observe_recovery_bit(beam, 0.0, true);
            beam.reseed_after_recovery(self.committed_beam_seed());
            return;
        }

        let mut children = 0usize;
        let parent_bank = beam.parents_bank;
        let materialize_bank = parent_bank ^ 1;
        for parent_idx in 0..beam.parents_len {
            let parent = beam.parents[parent_bank][parent_idx];
            let state_norm = mul8(&parent.state, &self.inverse_state_limit);
            let base_norm = self.predict_base_norm::<SPARSE>(&state_norm, u);
            let y_quantized = self.loop_output_norm::<SPARSE>(&state_norm, u);
            let (s, t, hot) = score_pair_dots(&base_norm, &self.bv_norm, &self.knee_thr_sq);
            let ([c_plus, c_minus], [p_plus, p_minus]) =
                self.beam_plain_no_isi_score_pair(y_quantized, &base_norm, parent, s, t, hot);
            beam.bases[parent_idx] = base_norm;
            beam.base_hot[parent_idx] = hot;
            for (v, legacy_cost, terminal_pressure) in
                [(1.0, c_plus, p_plus), (-1.0, c_minus, p_minus)]
            {
                let bits = ((parent.bits << 1) | u64::from(v > 0.0)) & PLAIN_BITS_MASK;
                let alternation_penalty = if RANKED && beam.alternation_weight > 0.0 {
                    beam_alternation_penalty(
                        bits,
                        (beam.buffered + 1).min(PLAIN_N),
                        beam.alternation_threshold,
                    )
                } else {
                    0.0
                };
                let metric =
                    parent.metric + legacy_cost + beam.alternation_weight * alternation_penalty;
                if !metric.is_finite() {
                    continue;
                }
                let rank_metric = metric + beam.terminal_weight * terminal_pressure;
                if !rank_metric.is_finite() {
                    continue;
                }
                beam.child_metric[children] = metric;
                beam.child_rank_metric[children] = rank_metric;
                beam.child_bits[children] = bits;
                beam.child_parent[children] = parent_idx as u8;
                beam.child_v[children] = v;
                children += 1;
            }
        }

        if children == 0 {
            beam.diagnostics.beam_all_children_rejected_total = beam
                .diagnostics
                .beam_all_children_rejected_total
                .wrapping_add(1);
            self.beam_emit_buffered(beam, out_bits);
            self.hard_reset();
            self.stability_resets = self.stability_resets.wrapping_add(1);
            out_bits.push(1);
            beam.diagnostics.emit_count += 1;
            #[cfg(feature = "ecbeam2_observer")]
            super::ecbeam2_observer::observe_recovery_bit(beam, u, false);
            beam.reseed_after_recovery(self.committed_beam_seed());
            return;
        }

        let mut order = [0u8; 2 * MAX_BEAM_WIDTH];
        let sort_metric = if RANKED {
            &beam.child_rank_metric
        } else {
            &beam.child_metric
        };
        sort_beam_children(children, sort_metric, &beam.child_bits, &mut order);

        let keep = PLAIN_M.min(children);
        #[cfg(feature = "ecbeam2_observer")]
        super::ecbeam2_observer::observe_authoritative_frontier(
            beam,
            u,
            parent_bank,
            children,
            &order[..keep],
            &self.bv_norm,
            &self.state_limit8,
            self.isi_penalty,
            false,
        );
        for slot in 0..keep {
            let child = order[slot] as usize;
            let parent_idx = beam.child_parent[child] as usize;
            let parent = beam.parents[parent_bank][parent_idx];
            let v = beam.child_v[child];
            let mut state =
                denormalized_feedback8(&beam.bases[parent_idx], &self.state_limit8, &self.bv, v);
            let clamped = if beam.base_hot[parent_idx] {
                match stabilize_state(
                    &mut state,
                    &self.coeffs.state_limit,
                    &self.inverse_state_limit,
                ) {
                    StateStability::Ok { clamped } => clamped,
                    StateStability::Reset => {
                        state = [0.0; 8];
                        true
                    }
                }
            } else {
                false
            };
            beam.diagnostics.beam_clamp_total += u64::from(clamped);
            beam.parents[materialize_bank][slot] = BeamPath {
                state,
                metric: beam.child_metric[child],
                prev_v: v,
                dc_bias: self.updated_dc_bias(parent.dc_bias, v),
                bits: beam.child_bits[child],
                clamp_bits: ((parent.clamp_bits << 1) | u64::from(clamped)) & PLAIN_BITS_MASK,
            };
        }
        beam.diagnostics.path_switches += u64::from(beam.child_parent[order[0] as usize] != 0);
        beam.parents_bank = materialize_bank;
        beam.aux_valid = false;
        beam.parents_len = keep;

        let ring_slot = (beam.sample_index & 7) as usize;
        beam.frontier_ring[ring_slot] = (beam.parents[beam.parents_bank][0].bits & 1) as u8;

        beam.buffered += 1;
        if beam.buffered == PLAIN_N {
            let parent_bank = beam.parents_bank;
            let compact_bank = parent_bank ^ 1;
            let best = &beam.parents[parent_bank][0];
            let bit = ((best.bits >> PLAIN_SHIFT) & 1) as u8;
            if (best.clamp_bits >> PLAIN_SHIFT) & 1 == 1 {
                self.state_clamps = self.state_clamps.wrapping_add(1);
                beam.diagnostics.beam_committed_clamp_total =
                    beam.diagnostics.beam_committed_clamp_total.wrapping_add(1);
            }
            out_bits.push(bit);
            beam.diagnostics.emit_count += 1;
            let frontier_slot = ((beam.sample_index + 1) & 7) as usize;
            beam.diagnostics.delayed_flips += u64::from(bit != beam.frontier_ring[frontier_slot]);
            let mut kept = 0usize;
            let mut pruned = 0u64;
            let mut needs_compaction = false;
            for idx in 0..beam.parents_len {
                let parent = beam.parents[parent_bank][idx];
                if ((parent.bits >> PLAIN_SHIFT) & 1) as u8 == bit {
                    if needs_compaction {
                        beam.parents[compact_bank][kept] = parent;
                    }
                    kept += 1;
                } else {
                    if !needs_compaction {
                        for prefix in 0..kept {
                            beam.parents[compact_bank][prefix] = beam.parents[parent_bank][prefix];
                        }
                    }
                    needs_compaction = true;
                    pruned = pruned.wrapping_add(1);
                }
            }
            if needs_compaction {
                beam.parents_bank = compact_bank;
            }
            beam.diagnostics.pruned_total = beam.diagnostics.pruned_total.wrapping_add(pruned);
            beam.parents_len = kept;
            beam.buffered -= 1;
        }
        beam.diagnostics.min_survivors =
            beam.diagnostics.min_survivors.min(beam.parents_len as u64);

        let parents = &mut beam.parents[beam.parents_bank];
        let mut min_metric = parents[0].metric;
        for parent in &parents[1..beam.parents_len] {
            min_metric = min_metric.min(parent.metric);
        }
        for parent in &mut parents[..beam.parents_len] {
            parent.metric -= min_metric;
        }
        beam.sample_index += 1;
    }

    #[inline(always)]
    #[allow(clippy::needless_range_loop)]
    pub(super) fn beam_plain_no_isi_score_pair(
        &self,
        y_quantized: f64,
        base_norm: &[f64; 8],
        parent: BeamPath,
        s: f64,
        t: f64,
        hot: bool,
    ) -> ([f64; 2], [f64; 2]) {
        let state_pressure_weight = self.ec2_weights.pressure_weight;
        let transition_weight = self.ec2_weights.transition_weight;
        let dc_bias_weight = self.ec2_weights.dc_weight;

        if !hot {
            let (wpc, ps, pt, pq) = if self.pressure_stage_weighted {
                let (s_w, t_w) = self.weighted_pressure_dots(base_norm);
                (state_pressure_weight, s_w, t_w, self.pbv_sq_sum)
            } else {
                (
                    state_pressure_weight * EC_STATE_PRESSURE_INV_COUNT,
                    s,
                    t,
                    self.bv_norm_sq_sum,
                )
            };
            let d = self.dc_bias_decay * parent.dc_bias;
            let e = 1.0 - self.dc_bias_decay;
            let common = self.ec2_weights.quantizer_weight * (y_quantized * y_quantized + 1.0)
                + wpc * (ps + pq)
                + transition_weight * 0.5
                + dc_bias_weight * (d * d + e * e);
            let delta = 2.0
                * (wpc * pt - self.ec2_weights.quantizer_weight * y_quantized
                    + dc_bias_weight * d * e)
                - transition_weight * 0.5 * parent.prev_v;
            let pressure_plus = if self.pressure_stage_weighted {
                ps + 2.0 * pt + pq
            } else {
                (ps + 2.0 * pt + pq) * EC_STATE_PRESSURE_INV_COUNT
            };
            let pressure_minus = if self.pressure_stage_weighted {
                ps - 2.0 * pt + pq
            } else {
                (ps - 2.0 * pt + pq) * EC_STATE_PRESSURE_INV_COUNT
            };
            return (
                [common + delta, common - delta],
                [pressure_plus, pressure_minus],
            );
        }

        let weighted_pressure = self.pressure_stage_weighted;
        let (ps, pt, pq) = if weighted_pressure {
            let (s_w, t_w) = self.weighted_pressure_dots(base_norm);
            (s_w, t_w, self.pbv_sq_sum)
        } else {
            (s, t, self.bv_norm_sq_sum)
        };
        let calc_lane = |v: f64| -> (f64, f64) {
            let pressure = v.mul_add(v.mul_add(pq, 2.0 * pt), ps);
            let terminal_pressure = if weighted_pressure {
                pressure
            } else {
                pressure * EC_STATE_PRESSURE_INV_COUNT
            };
            let mut limit_penalty = 0.0;
            for i in 0..7 {
                let normalized = v.mul_add(self.bv_norm[i], base_norm[i]).abs();
                let approach = ((normalized - EC_STATE_LIMIT_SOFT_KNEE)
                    * EC_STATE_LIMIT_SOFT_KNEE_INV_SPAN)
                    .max(0.0);
                let overflow = (normalized - 1.0).max(0.0);
                limit_penalty =
                    approach.mul_add(approach, limit_penalty) + 24.0 * overflow * overflow;
            }
            let quantizer_error = y_quantized - v;
            let transition = if v != parent.prev_v { 1.0 } else { 0.0 };
            let next_bias = self.updated_dc_bias(parent.dc_bias, v);
            let pressure_cost = if weighted_pressure {
                state_pressure_weight * pressure
            } else {
                state_pressure_weight * (pressure * EC_STATE_PRESSURE_INV_COUNT)
            };
            let cost = self.ec2_weights.quantizer_weight * quantizer_error * quantizer_error
                + pressure_cost
                + self.ec2_weights.limit_weight * limit_penalty
                + transition_weight * transition
                + dc_bias_weight * next_bias * next_bias;
            (cost, terminal_pressure)
        };
        let (cost_plus, pressure_plus) = calc_lane(1.0);
        let (cost_minus, pressure_minus) = calc_lane(-1.0);
        ([cost_plus, cost_minus], [pressure_plus, pressure_minus])
    }

    /// One EcBeam step (§21.8): expand every survivor against the shared
    /// per-sample dither, keep the best `m` children, commit the oldest bit of
    /// the best path once the horizon is full, prune disagreeing survivors.
    #[allow(clippy::needless_range_loop)]
    pub(super) fn process_beam_sample<const SPARSE: bool, const DITHER: bool>(
        &mut self,
        beam: &mut BeamState,
        u: f64,
        out_bits: &mut Vec<u8>,
        filtered_error_alpha: f64,
        reconstruction_error_alpha: f64,
    ) {
        if !u.is_finite() {
            // Mirror the depth-1 non-finite guard exactly (§21.3). Dithered
            // paths draw once so the RNG stream position stays equal to
            // samples consumed (§21.5); the D0 specialization has no active
            // stream to preserve.
            if DITHER {
                let _ = self.next_dither();
            }
            self.beam_emit_buffered(beam, out_bits);
            self.hard_reset();
            self.stability_resets = self.stability_resets.wrapping_add(1);
            out_bits.push(1);
            beam.diagnostics.emit_count += 1;
            #[cfg(feature = "ecbeam2_observer")]
            super::ecbeam2_observer::observe_recovery_bit(beam, 0.0, true);
            beam.reseed_after_recovery(self.committed_beam_seed());
            return;
        }

        let dither = if DITHER { self.next_dither() } else { 0.0 };
        let filtered_error_active =
            beam.filtered_error_weight > 0.0 || beam.filtered_error_rank_weight > 0.0;
        let reconstruction_error_active = beam.reconstruction_error_weight > 0.0;
        let pressure_deadzone_active = beam.pressure_deadzone > 0.0;
        let beam_pressure_weight = if pressure_deadzone_active {
            0.0
        } else {
            self.ec2_weights.pressure_weight
        };
        let periodicity_active = beam.periodicity_weight > 0.0 && beam.periodicity_lag_count > 0;
        // Reconstruction proxy compares candidate bit output against nominal
        // normalized full-scale input, not the coefficient-table `input_peak`
        // domain used by the loop state.
        let normalized_input_for_reconstruction_proxy = u.clamp(-1.0, 1.0);

        // Expansion: score all 2M children from M bases. States are only
        // materialized for the selected winners below in the legacy path; the
        // metric-hygiene experiment can opt into feasibility pruning before
        // child selection. Scratch lives on `beam` so nothing is re-initialized
        // per sample.
        let mut children = 0usize;
        let parent_bank = beam.parents_bank;
        let materialize_bank = parent_bank ^ 1;
        let aux_valid = beam.aux_valid;
        for parent_idx in 0..beam.parents_len {
            let parent = beam.parents[parent_bank][parent_idx];
            let parent_filtered_error = if aux_valid {
                beam.parent_filtered_error[parent_bank][parent_idx]
            } else {
                [0.0; 2]
            };
            let parent_reconstruction_error = if aux_valid {
                beam.parent_reconstruction_error[parent_bank][parent_idx]
            } else {
                [0.0; 4]
            };
            let state_norm = mul8(&parent.state, &self.inverse_state_limit);
            let base_norm = self.predict_base_norm::<SPARSE>(&state_norm, u);
            let y = self.loop_output_norm::<SPARSE>(&state_norm, u);
            let y_quantized = if DITHER {
                dither.mul_add(self.dither_scale, y)
            } else {
                y
            };
            let (s, t, hot) = score_pair_dots(&base_norm, &self.bv_norm, &self.knee_thr_sq);
            // Beam weights are intentionally hidden behind the existing
            // experiment-only policy-weight path; the defaults are the Phase 0
            // depth-1 constants, never the pressure-taper wrapper (§21.3).
            let ([c_plus, c_minus], _) = self.ec_candidate_score_pair_from_dots_with_weights(
                y_quantized,
                &base_norm,
                parent.prev_v,
                parent.dc_bias,
                s,
                t,
                hot,
                beam_pressure_weight,
                self.ec2_weights.transition_weight,
                self.ec2_weights.dc_weight,
            );
            beam.bases[parent_idx] = base_norm;
            beam.base_hot[parent_idx] = hot;
            for (v, legacy_cost) in [(1.0, c_plus), (-1.0, c_minus)] {
                let f = compensated_feedback(parent.prev_v, v, self.isi_penalty);
                let hard_limit_penalty = if hot {
                    self.beam_hard_limit_overflow_penalty(&base_norm, f)
                } else {
                    0.0
                };
                let would_clamp = hard_limit_penalty > 0.0;
                if would_clamp {
                    beam.diagnostics.beam_speculative_clamp_total = beam
                        .diagnostics
                        .beam_speculative_clamp_total
                        .wrapping_add(1);
                }
                if matches!(beam.clamp_policy, EcBeamClampPolicy::RejectHardLimit) && would_clamp {
                    beam.diagnostics.beam_rejected_hard_limit_total = beam
                        .diagnostics
                        .beam_rejected_hard_limit_total
                        .wrapping_add(1);
                    continue;
                }
                let clamp_policy_cost =
                    if matches!(beam.clamp_policy, EcBeamClampPolicy::PenalizeClamp) && would_clamp
                    {
                        EC_BEAM_CLAMP_PENALTY_WEIGHT * hard_limit_penalty
                    } else {
                        0.0
                    };
                let deadzone_pressure_cost = if pressure_deadzone_active {
                    self.ec2_weights.pressure_weight
                        * self.beam_deadzone_pressure(&base_norm, f, beam.pressure_deadzone)
                } else {
                    0.0
                };
                let quantizer_error = y_quantized - v;
                let (filtered_error, filtered_error_cost) = if filtered_error_active {
                    let (state, filtered) = beam_filtered_error_step(
                        parent_filtered_error,
                        quantizer_error,
                        filtered_error_alpha,
                    );
                    (state, filtered * filtered)
                } else {
                    (parent_filtered_error, 0.0)
                };
                let (reconstruction_error, reconstruction_error_cost) =
                    if reconstruction_error_active {
                        // Reconstruction-error proxy, not comparator-error filtering:
                        // compare the candidate 1-bit output value against the
                        // nominal input signal in the same +/-1 domain, then score
                        // only its simple audio-band low-passed energy.
                        let (state, filtered) = beam_reconstruction_error_step(
                            parent_reconstruction_error,
                            v - normalized_input_for_reconstruction_proxy,
                            reconstruction_error_alpha,
                        );
                        (state, filtered * filtered)
                    } else {
                        (parent_reconstruction_error, 0.0)
                    };
                let reconstruction_weighted_cost =
                    beam.reconstruction_error_weight * reconstruction_error_cost;
                let bits = ((parent.bits << 1) | u64::from(v > 0.0)) & beam.bits_mask;
                let periodicity_penalty = if periodicity_active {
                    beam_periodicity_penalty(
                        bits,
                        (beam.buffered + 1).min(beam.n),
                        beam.periodicity_window,
                        &beam.periodicity_lags,
                        beam.periodicity_lag_count,
                    )
                } else {
                    0.0
                };
                let periodicity_weighted_cost = beam.periodicity_weight * periodicity_penalty;
                let alternation_penalty =
                    if beam.alternation_weight > 0.0 || beam.alternation_rank_weight > 0.0 {
                        beam_alternation_penalty(
                            bits,
                            (beam.buffered + 1).min(beam.n),
                            beam.alternation_threshold,
                        )
                    } else {
                        0.0
                    };
                let (metric, rank_metric) =
                    if matches!(beam.metric_mode, EcBeamMetricMode::MetricHygiene) {
                        let terminal_pressure = self.beam_terminal_pressure(&base_norm, f);
                        let soft_limit_penalty = if hot {
                            self.beam_soft_limit_penalty(&base_norm, f)
                        } else {
                            0.0
                        };
                        let pressure_metric_pressure = if pressure_deadzone_active {
                            self.beam_deadzone_pressure(&base_norm, f, beam.pressure_deadzone)
                        } else {
                            terminal_pressure
                        };
                        let error_cost =
                            self.ec2_weights.quantizer_weight * quantizer_error * quantizer_error;
                        let metric = parent.metric
                            + error_cost
                            + beam.filtered_error_weight * filtered_error_cost
                            + reconstruction_weighted_cost
                            + periodicity_weighted_cost
                            + self.ec2_weights.limit_weight * soft_limit_penalty
                            + clamp_policy_cost;
                        if !metric.is_finite() {
                            continue;
                        }
                        let transition = if v != parent.prev_v { 1.0 } else { 0.0 };
                        let transition_cost = self.ec2_weights.transition_weight * transition;
                        let next_bias = self.updated_dc_bias(parent.dc_bias, v);
                        let pressure_cost =
                            self.ec2_weights.pressure_weight * pressure_metric_pressure;
                        let dc_cost = self.ec2_weights.dc_weight * next_bias * next_bias;
                        let metric = metric
                            + beam.pressure_accum_scale * pressure_cost
                            + beam.dc_accum_scale * dc_cost
                            + transition_cost;
                        let rank_metric = metric
                            + (beam.pressure_rank_scale * self.ec2_weights.pressure_weight
                                + beam.terminal_weight)
                                * terminal_pressure
                            + beam.dc_rank_scale * dc_cost
                            + (beam.alternation_weight + beam.alternation_rank_weight)
                                * alternation_penalty
                            + beam.filtered_error_rank_weight * filtered_error_cost;
                        (metric, rank_metric)
                    } else {
                        let metric = parent.metric
                            + legacy_cost
                            + deadzone_pressure_cost
                            + beam.alternation_weight * alternation_penalty
                            + beam.filtered_error_weight * filtered_error_cost
                            + reconstruction_weighted_cost
                            + periodicity_weighted_cost
                            + clamp_policy_cost;
                        if !metric.is_finite() {
                            continue;
                        }
                        let terminal_pressure = if beam.terminal_weight > 0.0 {
                            self.beam_terminal_pressure(&base_norm, f)
                        } else {
                            0.0
                        };
                        let rank_metric =
                            if matches!(beam.metric_mode, EcBeamMetricMode::PathConsistent) {
                                metric
                            } else {
                                metric
                                    + beam.terminal_weight * terminal_pressure
                                    + beam.alternation_rank_weight * alternation_penalty
                                    + beam.filtered_error_rank_weight * filtered_error_cost
                            };
                        (metric, rank_metric)
                    };
                if !rank_metric.is_finite() {
                    continue;
                }
                if reconstruction_error_active {
                    beam.reconstruction_diagnostics.record(
                        reconstruction_error_cost,
                        reconstruction_weighted_cost,
                        legacy_cost,
                    );
                }
                if periodicity_active {
                    beam.periodicity_diagnostics.record(
                        periodicity_penalty,
                        periodicity_weighted_cost,
                        legacy_cost,
                    );
                }
                beam.child_metric[children] = metric;
                beam.child_rank_metric[children] = rank_metric;
                beam.child_bits[children] = bits;
                beam.child_parent[children] = parent_idx as u8;
                beam.child_v[children] = v;
                beam.child_filtered_error[children] = filtered_error;
                beam.child_reconstruction_error[children] = reconstruction_error;
                children += 1;
            }
        }

        if children == 0 {
            // All paths dead (§21.6): deterministic hard reset mirroring the
            // depth-1 StateStability::Reset arm. The RNG has already advanced.
            beam.diagnostics.beam_all_children_rejected_total = beam
                .diagnostics
                .beam_all_children_rejected_total
                .wrapping_add(1);
            self.beam_emit_buffered(beam, out_bits);
            self.hard_reset();
            self.stability_resets = self.stability_resets.wrapping_add(1);
            out_bits.push(1);
            beam.diagnostics.emit_count += 1;
            #[cfg(feature = "ecbeam2_observer")]
            super::ecbeam2_observer::observe_recovery_bit(beam, u, false);
            beam.reseed_after_recovery(self.committed_beam_seed());
            return;
        }

        // Top-M selection: stable insertion sort over child indices by rank
        // metric, with packed bits as deterministic tie-break — §21.4.
        // `child_metric` is the accumulated path cost stored in `BeamPath`;
        // `child_rank_metric` may include rank-only terminal/alternation/filter
        // nudges. Descending bits makes an exact tie prefer +1 at the earliest
        // divergence, matching `select_ec_candidate`'s convention.
        let mut order = [0u8; 2 * MAX_BEAM_WIDTH];
        sort_beam_children(
            children,
            &beam.child_rank_metric,
            &beam.child_bits,
            &mut order,
        );
        if beam.collect_metric_diagnostics {
            beam_record_metric_diagnostics(beam, children, beam.m.min(children), &order);
        }

        // Materialize the winners: the legacy path uses the same raw-space
        // construction and saturation as the depth-1 commit. Metric hygiene may
        // have pruned hard-limit violations already; the stabilizer still keeps
        // the commit path deterministic.
        let keep = beam.m.min(children);
        #[cfg(feature = "ecbeam2_observer")]
        super::ecbeam2_observer::observe_authoritative_frontier(
            beam,
            u,
            parent_bank,
            children,
            &order[..keep],
            &self.bv_norm,
            &self.state_limit8,
            self.isi_penalty,
            false,
        );
        for slot in 0..keep {
            let child = order[slot] as usize;
            let parent_idx = beam.child_parent[child] as usize;
            let parent = beam.parents[parent_bank][parent_idx];
            let v = beam.child_v[child];
            let f = compensated_feedback(parent.prev_v, v, self.isi_penalty);
            let mut state =
                denormalized_feedback8(&beam.bases[parent_idx], &self.state_limit8, &self.bv, f);
            let clamped = if beam.base_hot[parent_idx] {
                match stabilize_state(
                    &mut state,
                    &self.coeffs.state_limit,
                    &self.inverse_state_limit,
                ) {
                    StateStability::Ok { clamped } => clamped,
                    // Unreachable for finite legacy metrics and feasible hygiene
                    // children, but kept deterministic regardless.
                    StateStability::Reset => {
                        state = [0.0; 8];
                        true
                    }
                }
            } else {
                false
            };
            beam.diagnostics.beam_clamp_total += u64::from(clamped);
            beam.parents[materialize_bank][slot] = BeamPath {
                state,
                metric: beam.child_metric[child],
                prev_v: v,
                dc_bias: self.updated_dc_bias(parent.dc_bias, v),
                bits: beam.child_bits[child],
                clamp_bits: ((parent.clamp_bits << 1) | u64::from(clamped)) & beam.bits_mask,
            };
            if aux_valid {
                beam.parent_filtered_error[materialize_bank][slot] =
                    beam.child_filtered_error[child];
                beam.parent_reconstruction_error[materialize_bank][slot] =
                    beam.child_reconstruction_error[child];
            }
        }
        beam.diagnostics.path_switches += u64::from(beam.child_parent[order[0] as usize] != 0);
        beam.parents_bank = materialize_bank;
        beam.parents_len = keep;

        // Record the instantaneous best child's newest bit for delayed_flips.
        let ring_slot = beam.ring_index;
        beam.frontier_ring[ring_slot] = (beam.parents[beam.parents_bank][0].bits & 1) as u8;

        beam.buffered += 1;
        if beam.buffered == beam.n {
            // Commit the oldest un-emitted bit of the best survivor; drop
            // every survivor that disagrees (order-preserving compaction —
            // the best survivor always survives, so the beam never empties).
            let shift = beam.n - 1;
            let parent_bank = beam.parents_bank;
            let compact_bank = parent_bank ^ 1;
            let best = &beam.parents[parent_bank][0];
            let bit = ((best.bits >> shift) & 1) as u8;
            if (best.clamp_bits >> shift) & 1 == 1 {
                self.state_clamps = self.state_clamps.wrapping_add(1);
                beam.diagnostics.beam_committed_clamp_total =
                    beam.diagnostics.beam_committed_clamp_total.wrapping_add(1);
            }
            out_bits.push(bit);
            beam.diagnostics.emit_count += 1;
            // The emitted sample was the frontier n-1 samples ago; its ring
            // slot is the next wrapping frontier slot.
            let frontier_slot = if ring_slot + 1 == beam.n {
                0
            } else {
                ring_slot + 1
            };
            beam.diagnostics.delayed_flips += u64::from(bit != beam.frontier_ring[frontier_slot]);
            let mut kept = 0usize;
            let mut pruned = 0u64;
            let mut needs_compaction = false;
            for idx in 0..beam.parents_len {
                let parent = beam.parents[parent_bank][idx];
                if ((parent.bits >> shift) & 1) as u8 == bit {
                    if needs_compaction {
                        beam.parents[compact_bank][kept] = parent;
                        if aux_valid {
                            beam.parent_filtered_error[compact_bank][kept] =
                                beam.parent_filtered_error[parent_bank][idx];
                            beam.parent_reconstruction_error[compact_bank][kept] =
                                beam.parent_reconstruction_error[parent_bank][idx];
                        }
                    }
                    kept += 1;
                } else {
                    if !needs_compaction {
                        for prefix in 0..kept {
                            beam.parents[compact_bank][prefix] = beam.parents[parent_bank][prefix];
                            if aux_valid {
                                beam.parent_filtered_error[compact_bank][prefix] =
                                    beam.parent_filtered_error[parent_bank][prefix];
                                beam.parent_reconstruction_error[compact_bank][prefix] =
                                    beam.parent_reconstruction_error[parent_bank][prefix];
                            }
                        }
                    }
                    needs_compaction = true;
                    pruned = pruned.wrapping_add(1);
                }
            }
            if needs_compaction {
                beam.parents_bank = compact_bank;
            }
            beam.diagnostics.pruned_total = beam.diagnostics.pruned_total.wrapping_add(pruned);
            beam.parents_len = kept;
            beam.buffered -= 1;
        }
        beam.diagnostics.min_survivors =
            beam.diagnostics.min_survivors.min(beam.parents_len as u64);

        // Renormalize parent metrics: subtract the same scalar from every
        // survivor (ordering-invariant) at a fixed point in the loop so it is
        // chunking-invariant; keeps f64 metrics sane over long streams.
        let parents = &mut beam.parents[beam.parents_bank];
        let mut min_metric = parents[0].metric;
        for parent in &parents[1..beam.parents_len] {
            min_metric = min_metric.min(parent.metric);
        }
        for parent in &mut parents[..beam.parents_len] {
            parent.metric -= min_metric;
        }
        beam.sample_index += 1;
        beam.ring_index = if beam.ring_index + 1 == beam.n {
            0
        } else {
            beam.ring_index + 1
        };
    }

    #[inline(always)]
    #[allow(clippy::needless_range_loop)]
    pub(super) fn beam_terminal_pressure(&self, base_norm: &[f64; 8], f: f64) -> f64 {
        if self.pressure_stage_weighted {
            let mut pressure = 0.0;
            for i in 0..7 {
                let z = f.mul_add(self.bv_norm[i], base_norm[i]);
                pressure = self.pressure_stage_weight[i].mul_add(z * z, pressure);
            }
            pressure
        } else {
            let mut state_norm = [0.0; 8];
            for i in 0..7 {
                state_norm[i] = f.mul_add(self.bv_norm[i], base_norm[i]);
            }
            candidate_pressure(&state_norm)
        }
    }

    #[inline(always)]
    #[allow(clippy::needless_range_loop)]
    pub(super) fn beam_hard_limit_overflow_penalty(&self, base_norm: &[f64; 8], f: f64) -> f64 {
        let mut penalty = 0.0;
        for i in 0..7 {
            let z = f.mul_add(self.bv_norm[i], base_norm[i]).abs();
            if !z.is_finite() {
                return f64::INFINITY;
            }
            let overflow = (z - 1.0).max(0.0);
            penalty += overflow * overflow;
        }
        penalty
    }

    #[inline(always)]
    #[allow(clippy::needless_range_loop)]
    pub(super) fn beam_soft_limit_penalty(&self, base_norm: &[f64; 8], f: f64) -> f64 {
        let mut penalty = 0.0;
        for i in 0..7 {
            let z = f.mul_add(self.bv_norm[i], base_norm[i]).abs();
            if !z.is_finite() {
                return f64::INFINITY;
            }
            let approach =
                ((z - EC_STATE_LIMIT_SOFT_KNEE) * EC_STATE_LIMIT_SOFT_KNEE_INV_SPAN).max(0.0);
            let overflow = (z - 1.0).max(0.0);
            penalty = approach.mul_add(approach, penalty) + 24.0 * overflow * overflow;
        }
        penalty
    }

    #[inline(always)]
    #[allow(clippy::needless_range_loop)]
    pub(super) fn beam_deadzone_pressure(
        &self,
        base_norm: &[f64; 8],
        f: f64,
        deadzone: f64,
    ) -> f64 {
        // Reconstruction-error weighting and this pressure dead-zone target
        // different biases. This helper is a pressure-only experiment: score
        // only normalized-state excursions outside the program region, using
        // the same stage weighting as the legacy pressure term.
        if self.pressure_stage_weighted {
            let mut pressure = 0.0;
            for i in 0..7 {
                let z = f.mul_add(self.bv_norm[i], base_norm[i]);
                let excess = (z.abs() - deadzone).max(0.0);
                pressure = self.pressure_stage_weight[i].mul_add(excess * excess, pressure);
            }
            pressure
        } else {
            let mut pressure = 0.0;
            for i in 0..7 {
                let z = f.mul_add(self.bv_norm[i], base_norm[i]);
                let excess = (z.abs() - deadzone).max(0.0);
                pressure += excess * excess;
            }
            pressure * EC_STATE_PRESSURE_INV_COUNT
        }
    }

    /// Emit the best survivor's buffered (un-emitted) bits, oldest first,
    /// crediting committed-path clamps exactly as the per-sample emit does.
    /// Draws no dither (§21.5).
    pub(super) fn beam_emit_buffered(&mut self, beam: &mut BeamState, out_bits: &mut Vec<u8>) {
        #[cfg(feature = "ecbeam2_observer")]
        super::ecbeam2_observer::observe_buffered_flush(beam);
        let best = beam.parents[beam.parents_bank][0];
        for shift in (0..beam.buffered).rev() {
            if (best.clamp_bits >> shift) & 1 == 1 {
                self.state_clamps = self.state_clamps.wrapping_add(1);
                beam.diagnostics.beam_committed_clamp_total =
                    beam.diagnostics.beam_committed_clamp_total.wrapping_add(1);
            }
            out_bits.push(((best.bits >> shift) & 1) as u8);
            beam.diagnostics.emit_count += 1;
        }
        beam.buffered = 0;
    }

    /// Beam-mode flush (§4): emit the best survivor's remaining buffered bits
    /// in order, rebuild the committed modulator state from that survivor for
    /// API consistency, then reset the beam to the committed state. Consumes
    /// no input samples and therefore draws no dither.
    pub(super) fn flush_beam(&mut self, out_bits: &mut Vec<u8>) {
        let mut beam = self.beam.take().expect("flush_beam requires beam state");
        self.beam_emit_buffered(&mut beam, out_bits);
        let best = beam.parents[beam.parents_bank][0];
        self.state = best.state;
        self.prev_v = best.prev_v;
        self.dc_bias = best.dc_bias;
        #[cfg(feature = "ecbeam2_observer")]
        beam.reseed_after_observed_flush(self.committed_beam_seed());
        #[cfg(not(feature = "ecbeam2_observer"))]
        beam.reseed(self.committed_beam_seed());
        self.beam = Some(beam);
    }
}
