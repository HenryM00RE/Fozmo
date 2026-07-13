use super::coeff_math::*;
use super::modulator::*;

impl CrfbModulator {
    pub(super) fn process_ec_depth1_block<const SPARSE: bool>(
        &mut self,
        input: &[f64],
        out_bits: &mut Vec<u8>,
    ) {
        match (self.isi_penalty == 0.0, self.dither_shape) {
            (true, DitherShape::HighPassTpdf) => {
                self.process_ec_depth1_no_isi_block_shaped::<SPARSE, true>(input, out_bits)
            }
            (true, DitherShape::WhiteTpdf) => {
                self.process_ec_depth1_no_isi_block_shaped::<SPARSE, false>(input, out_bits)
            }
            (false, DitherShape::HighPassTpdf) => {
                self.process_ec_depth1_block_shaped::<SPARSE, true>(input, out_bits)
            }
            (false, DitherShape::WhiteTpdf) => {
                self.process_ec_depth1_block_shaped::<SPARSE, false>(input, out_bits)
            }
        }
        self.carried_root = None;
    }

    pub(super) fn process_ec_depth1_no_isi_block_shaped<
        const SPARSE: bool,
        const HIGH_PASS_DITHER: bool,
    >(
        &mut self,
        input: &[f64],
        out_bits: &mut Vec<u8>,
    ) {
        for &u in input {
            self.process_ec_depth1_no_isi_sample_shaped::<SPARSE, HIGH_PASS_DITHER>(u, out_bits);
        }
    }

    pub(super) fn process_ec_depth1_block_shaped<
        const SPARSE: bool,
        const HIGH_PASS_DITHER: bool,
    >(
        &mut self,
        input: &[f64],
        out_bits: &mut Vec<u8>,
    ) {
        for &u in input {
            self.process_ec_depth1_sample_shaped::<SPARSE, HIGH_PASS_DITHER>(u, out_bits);
        }
    }

    pub(super) fn process_ec_depth1_sample<const SPARSE: bool>(
        &mut self,
        u: f64,
        out_bits: &mut Vec<u8>,
    ) {
        match (self.isi_penalty == 0.0, self.dither_shape) {
            (true, DitherShape::HighPassTpdf) => {
                self.process_ec_depth1_no_isi_sample_shaped::<SPARSE, true>(u, out_bits)
            }
            (true, DitherShape::WhiteTpdf) => {
                self.process_ec_depth1_no_isi_sample_shaped::<SPARSE, false>(u, out_bits)
            }
            (false, DitherShape::HighPassTpdf) => {
                self.process_ec_depth1_sample_shaped::<SPARSE, true>(u, out_bits)
            }
            (false, DitherShape::WhiteTpdf) => {
                self.process_ec_depth1_sample_shaped::<SPARSE, false>(u, out_bits)
            }
        }
    }

    pub(super) fn process_ec_depth1_no_isi_sample_shaped<
        const SPARSE: bool,
        const HIGH_PASS_DITHER: bool,
    >(
        &mut self,
        u: f64,
        out_bits: &mut Vec<u8>,
    ) {
        if !u.is_finite() {
            let _ = if HIGH_PASS_DITHER {
                self.next_high_pass_dither()
            } else {
                self.next_white_dither()
            };
            self.hard_reset();
            self.stability_resets = self.stability_resets.wrapping_add(1);
            out_bits.push(1);
            return;
        }

        let state_norm = mul8(&self.state, &self.inverse_state_limit);
        let base1_norm = self.predict_base_norm::<SPARSE>(&state_norm, u);
        let y = self.loop_output_norm::<SPARSE>(&state_norm, u);
        let dither = if HIGH_PASS_DITHER {
            self.next_high_pass_dither()
        } else {
            self.next_white_dither()
        };
        let y_quantized = dither.mul_add(self.dither_scale, y);
        let ([c_plus, c_minus], root_hot) = self.ec_depth1_no_isi_score_pair_with_hot(
            y_quantized,
            &base1_norm,
            self.prev_v,
            self.dc_bias,
        );

        let (best_v, best_score_is_finite) = select_ec_candidate(c_plus, c_minus);

        let mut best_next =
            denormalized_feedback8(&base1_norm, &self.state_limit8, &self.bv, best_v);
        let clean_commit = !root_hot && best_score_is_finite;
        self.commit_ec_sample(best_v, &mut best_next, clean_commit, out_bits);
    }

    pub(super) fn process_ec_depth1_sample_shaped<
        const SPARSE: bool,
        const HIGH_PASS_DITHER: bool,
    >(
        &mut self,
        u: f64,
        out_bits: &mut Vec<u8>,
    ) {
        if !u.is_finite() {
            let _ = if HIGH_PASS_DITHER {
                self.next_high_pass_dither()
            } else {
                self.next_white_dither()
            };
            self.hard_reset();
            self.stability_resets = self.stability_resets.wrapping_add(1);
            out_bits.push(1);
            return;
        }

        let state_norm = mul8(&self.state, &self.inverse_state_limit);
        let base1_norm = self.predict_base_norm::<SPARSE>(&state_norm, u);
        let y = self.loop_output_norm::<SPARSE>(&state_norm, u);
        let dither = if HIGH_PASS_DITHER {
            self.next_high_pass_dither()
        } else {
            self.next_white_dither()
        };
        let y_quantized = dither.mul_add(self.dither_scale, y);
        let ([c_plus, c_minus], root_hot) = self.ec_depth1_candidate_score_pair_with_hot(
            y_quantized,
            &base1_norm,
            self.prev_v,
            self.dc_bias,
        );

        let (best_v, best_score_is_finite) = select_ec_candidate(c_plus, c_minus);

        let f_best = if self.isi_penalty == 0.0 {
            best_v
        } else {
            compensated_feedback(self.prev_v, best_v, self.isi_penalty)
        };
        let mut best_next =
            denormalized_feedback8(&base1_norm, &self.state_limit8, &self.bv, f_best);
        let clean_commit =
            !root_hot && best_score_is_finite && (self.isi_penalty == 0.0 || f_best.abs() <= 1.0);
        self.commit_ec_sample(best_v, &mut best_next, clean_commit, out_bits);
    }
}
