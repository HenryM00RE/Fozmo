use super::coeff_math::*;
use super::diagnostics::*;
use super::modulator::*;

impl CrfbModulator {
    pub(super) fn process_ec_block<const SPARSE: bool, const CHILD: u8>(
        &mut self,
        input: &[f64],
        out_bits: &mut Vec<u8>,
    ) {
        self.pending.extend_from_slice(input);
        let horizon = self.lookahead_depth.max(1) - 1;
        if self.pending.len() <= horizon {
            return;
        }
        let ready = self.pending.len() - horizon;
        let pending = std::mem::take(&mut self.pending);
        // The root carry lives in a block-local so the hot loop never round-trips
        // it through the struct field.
        let mut carry = self.carried_root.take();
        for idx in 0..ready {
            let future = &pending[idx + 1..idx + 1 + horizon];
            self.process_ec_buffered_sample::<SPARSE, CHILD>(
                pending[idx],
                future,
                &mut carry,
                out_bits,
            );
        }
        self.carried_root = carry;
        self.pending = pending;
        self.pending.drain(..ready);
    }

    pub(super) fn process_ec_buffered_sample<const SPARSE: bool, const CHILD: u8>(
        &mut self,
        u: f64,
        future: &[f64],
        carry: &mut Option<RootCarry>,
        out_bits: &mut Vec<u8>,
    ) {
        let (v, mut next, clean_commit) = self.process_ec_sample::<SPARSE, CHILD>(u, future, carry);
        if !self.commit_ec_sample(v, &mut next, clean_commit, out_bits) {
            *carry = None;
        }
    }

    pub(super) fn ec_root_total_for_child<const SPARSE: bool, const DEPTH4: bool>(
        &self,
        ordered: &[(f64, f64); 2],
        shared2_norm: &[f64; 8],
        y2_shared: f64,
        future: &[f64],
        future_dither: &[f64],
    ) -> (f64, f64) {
        let lookahead_discount = self.ec_lookahead_discount();
        let mut best_score = f64::INFINITY;
        let mut best_v = 0.0;
        for &(v1, c1) in ordered {
            if !c1.is_finite() || c1 >= best_score {
                continue;
            }
            let f1 = compensated_feedback(self.prev_v, v1, self.isi_penalty);
            let child_bias = self.updated_dc_bias(self.dc_bias, v1);
            let child = if DEPTH4 {
                self.ec_node3::<SPARSE>(
                    shared2_norm,
                    y2_shared,
                    f1,
                    v1,
                    child_bias,
                    &future[1..],
                    self.future_dither_tail(future_dither),
                    f64::INFINITY,
                )
            } else {
                self.ec_leaf_pair_best(shared2_norm, y2_shared, f1, v1, child_bias, future_dither)
            };
            let total = lookahead_discount.mul_add(child, c1);
            if total < best_score {
                best_score = total;
                best_v = v1;
            }
        }
        if best_v == 0.0 {
            (1.0, best_score)
        } else {
            (best_v, best_score)
        }
    }

    /// Returns the chosen bit value and its raw next state; `carry` is consumed
    /// for this root and overwritten with this sample's expansion (the caller
    /// invalidates it again if the commit isn't clean).
    pub(super) fn process_ec_sample<const SPARSE: bool, const CHILD: u8>(
        &mut self,
        u: f64,
        future: &[f64],
        carry: &mut Option<RootCarry>,
    ) -> (f64, [f64; 8], bool) {
        // The previous sample's expansion already contains this root's base state
        // and loop output up to the committed feedback's affine term (`u` is the
        // same `future[0]` the expansion was built from, by pending-buffer
        // continuity), so the root matvec usually collapses to two affine steps.
        let (base1_norm, y) = match &*carry {
            Some((shared2_norm, y2_shared, f)) => (
                affine8(shared2_norm, &self.a_bv_norm, *f),
                f.mul_add(self.c_bv, *y2_shared),
            ),
            None => {
                let state_norm = mul8(&self.state, &self.inverse_state_limit);
                (
                    self.predict_base_norm::<SPARSE>(&state_norm, u),
                    self.loop_output_norm::<SPARSE>(&state_norm, u),
                )
            }
        };
        let dither = self.next_dither();
        let y_quantized = dither.mul_add(self.dither_scale, y);

        let expand = self.lookahead_depth > 1 && !future.is_empty();
        let (future_dither, future_dither_len) = if expand {
            self.peek_future_dither(future.len())
        } else {
            ([0.0; MAX_EC_FUTURE_DITHER], 0)
        };
        let future_dither = &future_dither[..future_dither_len];
        // Shared depth-2 expansion: everything not affine in the feedback f1.
        // Eager here — the best root candidate descends unless its score is
        // non-finite, so the expansion is essentially always needed.
        let (shared2_norm, y2_shared) = if expand {
            (
                self.predict_base_norm::<SPARSE>(&base1_norm, future[0]),
                self.loop_output_norm::<SPARSE>(&base1_norm, future[0]),
            )
        } else {
            ([0.0; 8], 0.0)
        };
        let ([mut c_plus, mut c_minus], root_hot, pressure, pressure_tapered) =
            if self.lookahead_depth == 1 {
                let (scores, hot) = self.ec_depth1_candidate_score_pair_with_hot(
                    y_quantized,
                    &base1_norm,
                    self.prev_v,
                    self.dc_bias,
                );
                (scores, hot, max_abs7(&base1_norm), false)
            } else {
                let (s, t, hot, pressure) =
                    score_pair_dots_pressure(&base1_norm, &self.bv_norm, &self.knee_thr_sq);
                let (pressure_weight, pressure_tapered) = self.ec2_pressure_weight_for(pressure);
                let (scores, hot) = self.ec_candidate_score_pair_from_dots_with_weights(
                    y_quantized,
                    &base1_norm,
                    self.prev_v,
                    self.dc_bias,
                    s,
                    t,
                    hot,
                    pressure_weight,
                    self.ec2_weights.transition_weight,
                    self.ec2_weights.dc_weight,
                );
                (scores, hot, pressure, pressure_tapered)
            };
        // Ambiguity-gated comparator dither (Workstream G2). On a root near-tie,
        // re-score the committed quantizer with the drawn comparator dither so
        // idle limit cycles get broken; away from ties the argmin is decisive
        // and the dither is never applied, so program material is untouched. The
        // dither only perturbs the root quantizer cost, so the depth-2 expansion
        // below still descends from the same base state — no child re-expansion
        // is needed. Disabled by default (`gated_dither_active()` false).
        if self.gated_dither_active()
            && score_relative_margin(c_plus, c_minus) <= self.gated_dither_margin
        {
            let y_gated = dither.mul_add(self.gated_dither_scale, y);
            let [g_plus, g_minus] = self.ec_root_score_pair(y_gated, &base1_norm);
            c_plus = g_plus;
            c_minus = g_minus;
        }
        // Descend the better-scored candidate first: the incumbent it produces
        // prunes the other branch more often than the old sign heuristic did.
        let ordered = if c_plus <= c_minus {
            [(1.0, c_plus), (-1.0, c_minus)]
        } else {
            [(-1.0, c_minus), (1.0, c_plus)]
        };
        let lookahead_discount = self.ec_lookahead_discount();

        let mut best_score = f64::INFINITY;
        let mut best_v = 0.0; // 0.0 = no candidate accepted yet
        let mut total_plus = f64::INFINITY;
        let mut total_minus = f64::INFINITY;
        for (v1, c1) in ordered {
            // All cost terms are non-negative, so total >= c1: pruning is admissible.
            if !c1.is_finite() || c1 >= best_score {
                continue;
            }

            let total = if expand {
                let f1 = compensated_feedback(self.prev_v, v1, self.isi_penalty);
                // Same admissible pre-descent prune as the interior nodes.
                let y_child = f1.mul_add(self.c_bv, y2_shared);
                let y_child_quantized = self.quantized_future_loop_output(y_child, future_dither);
                let qe_min = y_child_quantized.abs() - 1.0;
                let child_lb = self.ec2_weights.quantizer_weight * qe_min * qe_min;
                if lookahead_discount.mul_add(child_lb, c1) >= best_score {
                    continue;
                }
                let child_bias = self.updated_dc_bias(self.dc_bias, v1);
                let child_bound = (best_score - c1) / lookahead_discount;
                // The node fns self-truncate on short futures, so dispatch is by
                // the const child class alone; dead arms vanish per instantiation,
                // keeping each depth's root lean while its own chain stays inlined.
                let child = match CHILD {
                    1 => self.ec_leaf_pair_best(
                        &shared2_norm,
                        y2_shared,
                        f1,
                        v1,
                        child_bias,
                        future_dither,
                    ),
                    2 => self.ec_node2::<SPARSE>(
                        &shared2_norm,
                        y2_shared,
                        f1,
                        v1,
                        child_bias,
                        &future[1..],
                        self.future_dither_tail(future_dither),
                        child_bound,
                    ),
                    3 => self.ec_node3::<SPARSE>(
                        &shared2_norm,
                        y2_shared,
                        f1,
                        v1,
                        child_bias,
                        &future[1..],
                        self.future_dither_tail(future_dither),
                        child_bound,
                    ),
                    _ => self.ec_best_descendant_score::<SPARSE>(
                        &shared2_norm,
                        y2_shared,
                        f1,
                        v1,
                        child_bias,
                        &future[1..],
                        self.future_dither_tail(future_dither),
                        self.lookahead_depth - 1,
                        child_bound,
                    ),
                };
                lookahead_discount.mul_add(child, c1)
            } else {
                c1
            };
            if total < best_score {
                best_score = total;
                best_v = v1;
            }
            if v1 > 0.0 {
                total_plus = total;
            } else {
                total_minus = total;
            }
        }

        if CHILD == 2
            && let Some(margin) = self.ec_depth3_guard_margin().filter(|_| expand)
        {
            let (depth2_v, depth2_score) = self.ec_root_total_for_child::<SPARSE, false>(
                &ordered,
                &shared2_norm,
                y2_shared,
                future,
                future_dither,
            );
            if depth2_v != best_v
                && depth2_score.is_finite()
                && (!best_score.is_finite() || best_score + margin >= depth2_score)
            {
                best_v = depth2_v;
                best_score = depth2_score;
            }
        }

        if best_v == 0.0 {
            // Both candidate scores were non-finite (pathological state). Fall back
            // to +1 so the commit path's stability probe can do its job.
            best_v = 1.0;
        }
        let root_winner = select_ec_candidate(c_plus, c_minus).0;
        let root_margin = score_relative_margin(total_plus, total_minus);
        let near_tie = self.ec2_weights.ambiguity_margin > 0.0
            && root_margin <= self.ec2_weights.ambiguity_margin;
        let mut ambiguity_override = false;
        if self.ec2_policy.uses_ambiguity_pressure() && near_tie {
            let plus_risk = self.ec2_candidate_risk(&base1_norm, 1.0, self.prev_v, self.dc_bias);
            let minus_risk = self.ec2_candidate_risk(&base1_norm, -1.0, self.prev_v, self.dc_bias);
            let safer_v = if plus_risk <= minus_risk { 1.0 } else { -1.0 };
            if safer_v != best_v {
                best_v = safer_v;
                best_score = if best_v > 0.0 {
                    total_plus
                } else {
                    total_minus
                };
                ambiguity_override = true;
            }
        }
        let f_best = compensated_feedback(self.prev_v, best_v, self.isi_penalty);
        let best_next = affine8(&mul8(&base1_norm, &self.state_limit8), &self.bv, f_best);
        *carry = expand.then_some((shared2_norm, y2_shared, f_best));

        // If the root scorer's knee gate stayed cold, every root candidate is
        // proven below the soft knee in normalized space. That is stricter than
        // the hard clamp limit, so commit can skip its clamp scan. Keep the full
        // stabilizer for non-finite search results and rare hot/knee cases.
        let clean_commit = !root_hot && best_score.is_finite() && f_best.abs() <= 1.0;

        let dc_bias_decay = self.dc_bias_decay;
        if let Some(trace) = &mut self.ec2_decision_trace {
            let committed_state_norm = mul8(&best_next, &self.inverse_state_limit);
            let committed_state_pressure = max_abs7(&committed_state_norm);
            let committed_state_energy = committed_state_norm[..7]
                .iter()
                .map(|value| value * value)
                .sum();
            let committed_state_stage_abs = [
                committed_state_norm[0].abs(),
                committed_state_norm[1].abs(),
                committed_state_norm[2].abs(),
                committed_state_norm[3].abs(),
                committed_state_norm[4].abs(),
                committed_state_norm[5].abs(),
                committed_state_norm[6].abs(),
            ];
            trace.record(Ec2DecisionTraceEvent {
                pressure,
                root_margin,
                root_hot,
                near_tie,
                ambiguity_override,
                pressure_tapered,
                chosen: best_v,
                root_winner,
                best_score,
                quantizer_error_abs: (y_quantized - best_v).abs(),
                dc_bias_abs: (dc_bias_decay * self.dc_bias + (1.0 - dc_bias_decay) * best_v).abs(),
                dither_abs: dither.abs(),
                committed_state_pressure,
                committed_state_energy,
                committed_state_stage_abs,
                f_best_abs: f_best.abs(),
            });
        }

        (best_v, best_next, clean_commit)
    }

    /// Best candidate score of a terminal search node (no further lookahead):
    /// works purely in normalized state space, so the raw state is never built.
    #[inline]
    pub(super) fn ec_leaf_pair_best(
        &self,
        shared_norm: &[f64; 8],
        y_shared: f64,
        f_parent: f64,
        parent_v: f64,
        dc_bias: f64,
        future_dither: &[f64],
    ) -> f64 {
        let base_norm = affine8(shared_norm, &self.a_bv_norm, f_parent);
        let y = f_parent.mul_add(self.c_bv, y_shared);
        let y_quantized = self.quantized_future_loop_output(y, future_dither);
        let [c_plus, c_minus] =
            self.ec_future_candidate_score_pair(y_quantized, &base_norm, parent_v, dc_bias);
        // f64::min ignores a NaN side, so one non-finite candidate still loses
        // to the finite one; both non-finite yields the old INFINITY sentinel.
        let best = c_plus.min(c_minus);
        if best.is_finite() {
            best
        } else {
            f64::INFINITY
        }
    }

    /// Scores both quantizer candidates (`+1` first, then `-1`) for one node.
    ///
    /// Works entirely in normalized state space: with `bn = bv ∘ inv_limit`
    /// precomputed, each candidate's normalized state is one FMA per integrator
    /// (`base_norm + f·bn`) and the raw candidate state never needs to be
    /// materialized. The two lanes are interleaved so their FMA chains pipeline
    /// independently.
    #[inline(always)]
    pub(super) fn ec_candidate_score_pair(
        &self,
        y_quantized: f64,
        base_norm: &[f64; 8],
        prev_v: f64,
        dc_bias: f64,
    ) -> [f64; 2] {
        self.ec_candidate_score_pair_with_hot(y_quantized, base_norm, prev_v, dc_bias)
            .0
    }

    #[inline(always)]
    pub(super) fn ec_candidate_score_pair_with_hot(
        &self,
        y_quantized: f64,
        base_norm: &[f64; 8],
        prev_v: f64,
        dc_bias: f64,
    ) -> ([f64; 2], bool) {
        let (s, t, hot) = score_pair_dots(base_norm, &self.bv_norm, &self.knee_thr_sq);
        self.ec_candidate_score_pair_from_dots(y_quantized, base_norm, prev_v, dc_bias, s, t, hot)
    }

    #[inline(always)]
    pub(super) fn ec_depth1_candidate_score_pair_with_hot(
        &self,
        y_quantized: f64,
        base_norm: &[f64; 8],
        prev_v: f64,
        dc_bias: f64,
    ) -> ([f64; 2], bool) {
        let (s, t, hot) = score_pair_dots(base_norm, &self.bv_norm, &self.knee_thr_sq);
        self.ec_candidate_score_pair_from_dots_with_weights(
            y_quantized,
            base_norm,
            prev_v,
            dc_bias,
            s,
            t,
            hot,
            EC_DEPTH1_STATE_PRESSURE_WEIGHT,
            EC_DEPTH1_TRANSITION_WEIGHT,
            EC_DC_BIAS_WEIGHT,
        )
    }

    #[inline(always)]
    #[allow(clippy::needless_range_loop)]
    pub(super) fn ec_depth1_no_isi_score_pair_with_hot(
        &self,
        y_quantized: f64,
        base_norm: &[f64; 8],
        prev_v: f64,
        dc_bias: f64,
    ) -> ([f64; 2], bool) {
        let (s, t, hot) = score_pair_dots(base_norm, &self.bv_norm, &self.knee_thr_sq);
        if !hot {
            let wpc = EC_DEPTH1_STATE_PRESSURE_WEIGHT * EC_STATE_PRESSURE_INV_COUNT;
            let d = self.dc_bias_decay * dc_bias;
            let e = 1.0 - self.dc_bias_decay;
            let common = EC_QUANTIZER_ERROR_WEIGHT * (y_quantized * y_quantized + 1.0)
                + wpc * (s + self.bv_norm_sq_sum)
                + EC_DEPTH1_TRANSITION_WEIGHT * 0.5
                + EC_DC_BIAS_WEIGHT * (d * d + e * e);
            let delta = 2.0
                * (wpc * t - EC_QUANTIZER_ERROR_WEIGHT * y_quantized + EC_DC_BIAS_WEIGHT * d * e)
                - EC_DEPTH1_TRANSITION_WEIGHT * 0.5 * prev_v;
            return ([common + delta, common - delta], hot);
        }

        let calc_lane = |v: f64| -> f64 {
            let pressure = v.mul_add(v.mul_add(self.bv_norm_sq_sum, 2.0 * t), s);
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
            let transition = if v != prev_v { 1.0 } else { 0.0 };
            let next_bias = self.updated_dc_bias(dc_bias, v);
            EC_QUANTIZER_ERROR_WEIGHT * quantizer_error * quantizer_error
                + EC_DEPTH1_STATE_PRESSURE_WEIGHT * (pressure * EC_STATE_PRESSURE_INV_COUNT)
                + EC_STATE_LIMIT_WEIGHT * limit_penalty
                + EC_DEPTH1_TRANSITION_WEIGHT * transition
                + EC_DC_BIAS_WEIGHT * next_bias * next_bias
        };
        ([calc_lane(1.0), calc_lane(-1.0)], hot)
    }

    #[inline(always)]
    pub(super) fn ec_candidate_score_pair_with_hot_and_pressure(
        &self,
        y_quantized: f64,
        base_norm: &[f64; 8],
        prev_v: f64,
        dc_bias: f64,
    ) -> ([f64; 2], bool, f64) {
        let (s, t, hot, pressure) =
            score_pair_dots_pressure(base_norm, &self.bv_norm, &self.knee_thr_sq);
        let (scores, hot) = self.ec_candidate_score_pair_from_dots(
            y_quantized,
            base_norm,
            prev_v,
            dc_bias,
            s,
            t,
            hot,
        );
        (scores, hot, pressure)
    }

    #[inline(always)]
    // Candidate scoring is a hot DSP helper; keeping scalar inputs separate avoids packing overhead.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn ec_candidate_score_pair_from_dots(
        &self,
        y_quantized: f64,
        base_norm: &[f64; 8],
        prev_v: f64,
        dc_bias: f64,
        s: f64,
        t: f64,
        hot: bool,
    ) -> ([f64; 2], bool) {
        let (pressure_weight, _) = self.ec2_pressure_weight_for(max_abs7(base_norm));
        self.ec_candidate_score_pair_from_dots_with_weights(
            y_quantized,
            base_norm,
            prev_v,
            dc_bias,
            s,
            t,
            hot,
            pressure_weight,
            self.ec2_weights.transition_weight,
            self.ec2_weights.dc_weight,
        )
    }

    #[inline(always)]
    // Weighted candidate scoring mirrors the math terms directly for the hot modulator path.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::needless_range_loop)]
    pub(super) fn ec_candidate_score_pair_from_dots_with_weights(
        &self,
        y_quantized: f64,
        base_norm: &[f64; 8],
        prev_v: f64,
        dc_bias: f64,
        s: f64,
        t: f64,
        hot: bool,
        state_pressure_weight: f64,
        transition_weight: f64,
        dc_bias_weight: f64,
    ) -> ([f64; 2], bool) {
        // The candidate state is `b + f·bn` with `bn` constant, so both pressures
        // come from two shared dot products: Σ(b + f·bn)² = S + f·(2T + f·Σbn²).
        // `hot` is the rare-knee gate: for |f| <= 1 the candidate peak state is
        // bounded by |b_i| + |bn_i|, thresholded per lane via `knee_thr_sq`.
        // Per-stage pressure re-weights the state-pressure quadratic. Off by
        // default: the `else` arms reproduce the uniform `pressure · 1/7` scalar
        // path bit-for-bit (`wpc` is a single scalar and the multiply order is
        // preserved), so shipped bitstreams are unchanged.
        if !hot && self.isi_penalty == 0.0 {
            // Symmetric fast path (the shipping default): with f = v = ±1 and no
            // knee penalty, score(v) expands exactly to `common + v·delta`.
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
            let d = self.dc_bias_decay * dc_bias;
            let e = 1.0 - self.dc_bias_decay;
            let common = self.ec2_weights.quantizer_weight * (y_quantized * y_quantized + 1.0)
                + wpc * (ps + pq)
                + transition_weight * 0.5
                + dc_bias_weight * (d * d + e * e);
            let delta = 2.0
                * (wpc * pt - self.ec2_weights.quantizer_weight * y_quantized
                    + dc_bias_weight * d * e)
                - transition_weight * 0.5 * prev_v;
            return ([common + delta, common - delta], hot);
        }

        let f_plus = compensated_feedback(prev_v, 1.0, self.isi_penalty);
        let f_minus = compensated_feedback(prev_v, -1.0, self.isi_penalty);
        // Pressure dots are the same for both feedback lanes (they only depend on
        // the shared base state), so hoist the weighted variant out of the lane
        // closure. The `weighted` flag also selects the pressure-cost scaling so
        // the default keeps its exact `pressure * 1/7` associativity.
        let weighted_pressure = self.pressure_stage_weighted;
        let (ps, pt, pq) = if weighted_pressure {
            let (s_w, t_w) = self.weighted_pressure_dots(base_norm);
            (s_w, t_w, self.pbv_sq_sum)
        } else {
            (s, t, self.bv_norm_sq_sum)
        };
        let calc_lane = |v: f64, f: f64| -> f64 {
            let pressure = f.mul_add(f.mul_add(pq, 2.0 * pt), ps);
            let limit_penalty = if hot {
                let mut penalty = 0.0;
                for i in 0..7 {
                    let normalized = f.mul_add(self.bv_norm[i], base_norm[i]).abs();
                    let approach = ((normalized - EC_STATE_LIMIT_SOFT_KNEE)
                        * EC_STATE_LIMIT_SOFT_KNEE_INV_SPAN)
                        .max(0.0);
                    let overflow = (normalized - 1.0).max(0.0);
                    penalty = approach.mul_add(approach, penalty) + 24.0 * overflow * overflow;
                }
                penalty
            } else {
                0.0
            };
            let quantizer_error = y_quantized - v;
            let transition = if v != prev_v { 1.0 } else { 0.0 };
            let next_bias = self.updated_dc_bias(dc_bias, v);
            let pressure_cost = if weighted_pressure {
                state_pressure_weight * pressure
            } else {
                state_pressure_weight * (pressure * EC_STATE_PRESSURE_INV_COUNT)
            };
            self.ec2_weights.quantizer_weight * quantizer_error * quantizer_error
                + pressure_cost
                + self.ec2_weights.limit_weight * limit_penalty
                + transition_weight * transition
                + dc_bias_weight * next_bias * next_bias
        };
        ([calc_lane(1.0, f_plus), calc_lane(-1.0, f_minus)], hot)
    }

    /// Future lookahead nodes should predict audio-domain quantizer behavior
    /// without letting internal-state comfort dominate the root decision. The
    /// committed root still uses the full scorer; descendants keep only
    /// quantizer error plus near-limit protection.
    #[inline(always)]
    #[allow(clippy::needless_range_loop)]
    pub(super) fn ec_future_candidate_score_pair(
        &self,
        y_quantized: f64,
        base_norm: &[f64; 8],
        prev_v: f64,
        dc_bias: f64,
    ) -> [f64; 2] {
        match self.future_scorer {
            EcFutureScorer::Full
            | EcFutureScorer::FullDiscount40
            | EcFutureScorer::FullDiscount25
            | EcFutureScorer::FullDiscount10
            | EcFutureScorer::FullDepth3Guard0001
            | EcFutureScorer::FullDepth3Guard001
            | EcFutureScorer::FullDepth3Guard01
            | EcFutureScorer::FullDepth3Guard05
            | EcFutureScorer::FullDepth3Guard10 => {
                if self.coeffs.osr <= 128 {
                    return self.ec_candidate_score_pair(y_quantized, base_norm, prev_v, dc_bias);
                }
            }
            EcFutureScorer::QuantizerOnly => {
                let common = self.ec2_weights.quantizer_weight * (y_quantized * y_quantized + 1.0);
                let delta = -2.0 * self.ec2_weights.quantizer_weight * y_quantized;
                return [common + delta, common - delta];
            }
            EcFutureScorer::QuarterPressureNoDcTransition => {
                let (s, t, hot) = score_pair_dots(base_norm, &self.bv_norm, &self.knee_thr_sq);
                return self
                    .ec_candidate_score_pair_from_dots_with_weights(
                        y_quantized,
                        base_norm,
                        prev_v,
                        dc_bias,
                        s,
                        t,
                        hot,
                        self.ec2_weights.pressure_weight * 0.25,
                        0.0,
                        0.0,
                    )
                    .0;
            }
            EcFutureScorer::QuantizerLimit => {}
        }

        let (_, _, hot) = score_pair_dots(base_norm, &self.bv_norm, &self.knee_thr_sq);
        if !hot {
            let common = self.ec2_weights.quantizer_weight * (y_quantized * y_quantized + 1.0);
            let delta = -2.0 * self.ec2_weights.quantizer_weight * y_quantized;
            return [common + delta, common - delta];
        }

        let f_plus = compensated_feedback(prev_v, 1.0, self.isi_penalty);
        let f_minus = compensated_feedback(prev_v, -1.0, self.isi_penalty);
        let calc_lane = |v: f64, f: f64| -> f64 {
            let mut limit_penalty = 0.0;
            for i in 0..7 {
                let normalized = f.mul_add(self.bv_norm[i], base_norm[i]).abs();
                let approach = ((normalized - EC_STATE_LIMIT_SOFT_KNEE)
                    * EC_STATE_LIMIT_SOFT_KNEE_INV_SPAN)
                    .max(0.0);
                let overflow = (normalized - 1.0).max(0.0);
                limit_penalty =
                    approach.mul_add(approach, limit_penalty) + 24.0 * overflow * overflow;
            }
            let quantizer_error = y_quantized - v;
            self.ec2_weights.quantizer_weight * quantizer_error * quantizer_error
                + self.ec2_weights.limit_weight * limit_penalty
        };
        [calc_lane(1.0, f_plus), calc_lane(-1.0, f_minus)]
    }

    /// Best achievable (discounted) score of the subtree rooted one sample after a
    /// parent that emitted `parent_v` with compensated feedback `f_parent`.
    ///
    /// `shared` / `y_shared` are the feedback-independent parts of the node's base
    /// state and loop output; the affine `f_parent·A·bv` / `f_parent·c·bv` terms are
    /// added here. Branches whose cost already meets `bound` are pruned (admissible:
    /// every cost term is non-negative).
    /// One interior search node, generic over how a child subtree is scored so
    /// the common depths compile to a single straight-line function (see
    /// [`ec_node2`](Self::ec_node2)/[`ec_node3`](Self::ec_node3)): measured, the
    /// call/stack overhead of dynamic recursion cost more than the node math.
    ///
    /// `child` receives `(self, shared_next_norm, y_shared_next, f, v, dc_bias,
    /// future_tail, future_dither_tail, bound)` and must tolerate short or empty
    /// tails (every node fn checks emptiness before expanding, so the tree
    /// truncates naturally at end of stream).
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    pub(super) fn ec_interior_node<const SPARSE: bool>(
        &self,
        shared_norm: &[f64; 8],
        y_shared: f64,
        f_parent: f64,
        parent_v: f64,
        dc_bias: f64,
        future: &[f64],
        future_dither: &[f64],
        bound: f64,
        child: impl Fn(&Self, &[f64; 8], f64, f64, f64, f64, &[f64], &[f64], f64) -> f64,
    ) -> f64 {
        let base_norm = affine8(shared_norm, &self.a_bv_norm, f_parent);
        let y = f_parent.mul_add(self.c_bv, y_shared);

        let y_quantized = self.quantized_future_loop_output(y, future_dither);
        let [c_plus, c_minus] =
            self.ec_future_candidate_score_pair(y_quantized, &base_norm, parent_v, dc_bias);
        // Best-first descent tightens the incumbent before the other branch is
        // considered, so the admissible prune fires more often.
        let ordered = if c_plus <= c_minus {
            [(1.0, c_plus), (-1.0, c_minus)]
        } else {
            [(-1.0, c_minus), (1.0, c_plus)]
        };

        let expand = !future.is_empty();
        // Eager expansion: ~94% of entered interior nodes descend (measured), and
        // the expansion depends only on node-entry values, so computing it up
        // front overlaps the matvec with the candidate scoring instead of
        // serializing behind it. (A lazy variant measured slower.)
        let (shared_next_norm, y_shared_next) = if expand {
            (
                self.predict_base_norm::<SPARSE>(&base_norm, future[0]),
                self.loop_output_norm::<SPARSE>(&base_norm, future[0]),
            )
        } else {
            ([0.0; 8], 0.0)
        };

        let mut best = f64::INFINITY;
        let lookahead_discount = self.ec_lookahead_discount();
        for (v, c) in ordered {
            let cutoff = best.min(bound);
            if !c.is_finite() || c >= cutoff {
                continue;
            }
            let total = if expand {
                let f = compensated_feedback(parent_v, v, self.isi_penalty);
                // Admissible pre-descent prune: the child's quantizer-error term
                // alone is at least w_q·(|y_child| − 1)², and its loop output is
                // one FMA away — skip subtrees that can't beat the cutoff.
                let y_child = f.mul_add(self.c_bv, y_shared_next);
                let future_dither_tail = self.future_dither_tail(future_dither);
                let y_child_quantized =
                    self.quantized_future_loop_output(y_child, future_dither_tail);
                let qe_min = y_child_quantized.abs() - 1.0;
                let child_lb = self.ec2_weights.quantizer_weight * qe_min * qe_min;
                if lookahead_discount.mul_add(child_lb, c) >= cutoff {
                    continue;
                }
                let child_bias = self.updated_dc_bias(dc_bias, v);
                let child_best = child(
                    self,
                    &shared_next_norm,
                    y_shared_next,
                    f,
                    v,
                    child_bias,
                    &future[1..],
                    future_dither_tail,
                    (cutoff - c) / lookahead_discount,
                );
                lookahead_discount.mul_add(child_best, c)
            } else {
                c
            };
            if total < best {
                best = total;
            }
        }
        best
    }

    /// Interior node whose children are terminal: the bottom two levels of the
    /// search, fully inlined.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    pub(super) fn ec_node2<const SPARSE: bool>(
        &self,
        shared_norm: &[f64; 8],
        y_shared: f64,
        f_parent: f64,
        parent_v: f64,
        dc_bias: f64,
        future: &[f64],
        future_dither: &[f64],
        bound: f64,
    ) -> f64 {
        self.ec_interior_node::<SPARSE>(
            shared_norm,
            y_shared,
            f_parent,
            parent_v,
            dc_bias,
            future,
            future_dither,
            bound,
            |s, shared_norm, y_shared, f, v, bias, _future, future_dither, _bound| {
                s.ec_leaf_pair_best(shared_norm, y_shared, f, v, bias, future_dither)
            },
        )
    }

    /// Interior node three levels above the deepest leaves — the top of the
    /// depth-4 hot path, fully inlined.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    pub(super) fn ec_node3<const SPARSE: bool>(
        &self,
        shared_norm: &[f64; 8],
        y_shared: f64,
        f_parent: f64,
        parent_v: f64,
        dc_bias: f64,
        future: &[f64],
        future_dither: &[f64],
        bound: f64,
    ) -> f64 {
        self.ec_interior_node::<SPARSE>(
            shared_norm,
            y_shared,
            f_parent,
            parent_v,
            dc_bias,
            future,
            future_dither,
            bound,
            |s, shared_norm, y_shared, f, v, bias, future, future_dither, bound| {
                s.ec_node2::<SPARSE>(
                    shared_norm,
                    y_shared,
                    f,
                    v,
                    bias,
                    future,
                    future_dither,
                    bound,
                )
            },
        )
    }

    /// Dynamic-depth interior node for searches deeper than the specialized
    /// chain (`lookahead_depth > 4`); bottoms out into the inlined node fns.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn ec_best_descendant_score<const SPARSE: bool>(
        &self,
        shared_norm: &[f64; 8],
        y_shared: f64,
        f_parent: f64,
        parent_v: f64,
        dc_bias: f64,
        future: &[f64],
        future_dither: &[f64],
        depth_left: usize,
        bound: f64,
    ) -> f64 {
        debug_assert!(depth_left >= 4);
        self.ec_interior_node::<SPARSE>(
            shared_norm,
            y_shared,
            f_parent,
            parent_v,
            dc_bias,
            future,
            future_dither,
            bound,
            |s, shared_norm, y_shared, f, v, bias, future, future_dither, bound| {
                if depth_left == 4 {
                    s.ec_node3::<SPARSE>(
                        shared_norm,
                        y_shared,
                        f,
                        v,
                        bias,
                        future,
                        future_dither,
                        bound,
                    )
                } else {
                    s.ec_best_descendant_score::<SPARSE>(
                        shared_norm,
                        y_shared,
                        f,
                        v,
                        bias,
                        future,
                        future_dither,
                        depth_left - 1,
                        bound,
                    )
                }
            },
        )
    }

    /// `x' = A_norm·x + bu_norm·u` in normalized state space — NEON row-pair
    /// kernel for the sparse CRFB pattern on aarch64, dense fallback otherwise.
    #[inline(always)]
    pub(super) fn predict_base_norm<const SPARSE: bool>(
        &self,
        state_norm: &[f64; 8],
        u: f64,
    ) -> [f64; 8] {
        #[cfg(target_arch = "aarch64")]
        if SPARSE {
            return self.predict_base_norm_sparse_neon(state_norm, u);
        }
        predict_base_next_state8::<SPARSE>(state_norm, &self.a_rows_norm, &self.bu_norm, u)
    }

    /// Loop output `y = c·x + d1·u` from the normalized state (`c_norm = c ∘ D`).
    #[inline(always)]
    pub(super) fn loop_output_norm<const SPARSE: bool>(
        &self,
        state_norm: &[f64; 8],
        u: f64,
    ) -> f64 {
        if SPARSE {
            self.coeffs
                .d1
                .mul_add(u, self.c_row_norm[6] * state_norm[6])
        } else {
            self.coeffs
                .d1
                .mul_add(u, dot8(&self.c_row_norm, state_norm))
        }
    }

    /// Sparse CRFB expansion matvec: row 0 scalar, the three resonator row pairs
    /// as 2-lane column FMAs over the precomputed `a_pair_cols` packing.
    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    pub(super) fn predict_base_norm_sparse_neon(&self, state: &[f64; 8], u: f64) -> [f64; 8] {
        use core::arch::aarch64::*;
        let mut next = [0.0f64; 8];
        next[0] = self.bu_norm[0].mul_add(u, self.a_rows_norm[0][0] * state[0]);
        // SAFETY: NEON is baseline on aarch64; all loads/stores use in-bounds
        // array pointers ([f64; 2] pairs and the padded [f64; 8] state).
        unsafe {
            for p in 0..3 {
                let cols = &self.a_pair_cols[p];
                // Measured: splitting this serial FMA chain into two + add is
                // slower on M-series; keep the single chain.
                let mut acc = vmulq_n_f64(vld1q_f64(cols[3].as_ptr()), u);
                acc = vfmaq_n_f64(acc, vld1q_f64(cols[0].as_ptr()), state[2 * p]);
                acc = vfmaq_n_f64(acc, vld1q_f64(cols[1].as_ptr()), state[2 * p + 1]);
                acc = vfmaq_n_f64(acc, vld1q_f64(cols[2].as_ptr()), state[2 * p + 2]);
                vst1q_f64(next.as_mut_ptr().add(2 * p + 1), acc);
            }
        }
        next
    }
}
