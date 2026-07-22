use super::coeff_math::{affine8, dot8, matches_crfb_sparsity, predict_base_next_state8};
use super::dither::XorShift64;
use super::stability::{StateStability, stabilize_state};

use crate::audio::dsd::dsd_coeffs::{CALIBRATED, ModulatorCoeffs};

const QUANTIZER_DITHER_SCALE: f64 = 1.0 / 256.0;

/// The two modulator implementations available to playback.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DsdModulator {
    #[default]
    Standard,
    EcBeam2,
}

impl DsdModulator {
    pub fn as_id(self) -> u32 {
        match self {
            Self::Standard => 0,
            Self::EcBeam2 => 7,
        }
    }

    pub fn from_id(id: u32) -> Self {
        match id {
            7 => Self::EcBeam2,
            // IDs 1-6 belonged to retired EC implementations. Treat them as
            // Standard so persisted settings remain safe after an upgrade.
            _ => Self::Standard,
        }
    }

    pub fn as_name(self) -> &'static str {
        match self {
            Self::Standard => "Standard",
            Self::EcBeam2 => "EcBeam2",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "standard" => Some(Self::Standard),
            "ecbeam2"
            | "ec beam 2"
            | "ec_beam_2"
            | "ec-beam-2"
            | "ecb2"
            | "7th order beam"
            | "7th order ecb2"
            | "7th order ecb2 (experimental)"
            | "7th order search" => Some(Self::EcBeam2),
            // Retired names are accepted only as migration aliases. No legacy
            // implementation remains behind these values.
            "ecdepth1"
            | "ec depth 1"
            | "ec_depth_1"
            | "ec-1"
            | "ec1"
            | "ecdepth2"
            | "ec depth 2"
            | "ec_depth_2"
            | "ec-2"
            | "ec2"
            | "ecbeam"
            | "ec beam"
            | "ec_beam"
            | "ec-beam"
            | "ecb"
            | "7th order ecb"
            | "ecdepth3"
            | "ec depth 3"
            | "ec_depth_3"
            | "ec-3"
            | "ec3"
            | "ecdepth4"
            | "ec depth 4"
            | "ec_depth_4"
            | "ec-4"
            | "ec4"
            | "ecdepth8"
            | "ec depth 8"
            | "ec_depth_8"
            | "ec-8"
            | "ec8"
            | "ecdepth4adaptive"
            | "ec depth 4 adaptive"
            | "ec_depth_4_adaptive"
            | "ec-4a"
            | "ec4a" => Some(Self::Standard),
            _ => None,
        }
    }

    /// Diagnostic latency label retained for the playback status surface.
    pub fn lookahead_depth(self) -> usize {
        match self {
            Self::Standard => 1,
            Self::EcBeam2 => 2,
        }
    }
}

/// Shared seventh-order CRFB state core and the standard hard-sign quantizer.
/// EcBeam2 reuses the normalized matrices and state storage, but owns its own
/// candidate search and buffering implementation.
pub struct CrfbModulator {
    pub(super) state: [f64; 8],
    pub(super) coeffs: &'static ModulatorCoeffs,
    a_rows: [[f64; 8]; 7],
    bu: [f64; 7],
    pub(super) bv: [f64; 8],
    c_row: [f64; 8],
    pub(super) a_rows_norm: [[f64; 8]; 7],
    pub(super) bu_norm: [f64; 7],
    pub(super) c_row_norm: [f64; 8],
    /// Packed normalized row pairs for EcBeam2's aarch64 sparse matvec.
    pub(super) a_pair_cols: [[[f64; 2]; 4]; 3],
    pub(super) state_limit8: [f64; 8],
    pub(super) inverse_state_limit: [f64; 8],
    pub(super) bv_norm: [f64; 8],
    pub(super) crfb_sparse: bool,
    rng: XorShift64,
    dither_scale: f64,
    pub(super) prev_v: f64,
    pub(super) stability_resets: u64,
    pub(super) state_clamps: u64,
}

impl CrfbModulator {
    pub fn new(coeffs: &'static ModulatorCoeffs, seed: u64) -> Result<Self, &'static str> {
        if !CALIBRATED {
            return Err("dsd_coeffs: CRFB table is uncalibrated — run tools/gen_crfb.py");
        }

        let mut inverse_state_limit = [0.0; 8];
        for (inverse, limit) in inverse_state_limit.iter_mut().zip(coeffs.state_limit) {
            if !limit.is_finite() || limit <= 0.0 {
                return Err("dsd_coeffs: state_limit entries must be finite and positive");
            }
            *inverse = limit.recip();
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

        let mut a_rows_norm = [[0.0; 8]; 7];
        let mut bu_norm = [0.0; 7];
        let mut c_row_norm = [0.0; 8];
        let mut state_limit8 = [0.0; 8];
        let mut bv_norm = [0.0; 8];
        for i in 0..7 {
            for k in 0..7 {
                a_rows_norm[i][k] = a_rows[i][k] * coeffs.state_limit[k] * inverse_state_limit[i];
            }
            bu_norm[i] = bu[i] * inverse_state_limit[i];
            c_row_norm[i] = c_row[i] * coeffs.state_limit[i];
            state_limit8[i] = coeffs.state_limit[i];
            bv_norm[i] = bv[i] * inverse_state_limit[i];
        }

        let mut a_pair_cols = [[[0.0; 2]; 4]; 3];
        for pair in 0..3 {
            for (index, column) in (2 * pair..2 * pair + 3).enumerate() {
                a_pair_cols[pair][index] = [
                    a_rows_norm[2 * pair + 1][column],
                    a_rows_norm[2 * pair + 2][column],
                ];
            }
            a_pair_cols[pair][3] = [bu_norm[2 * pair + 1], bu_norm[2 * pair + 2]];
        }

        Ok(Self {
            state: [0.0; 8],
            coeffs,
            a_rows,
            bu,
            bv,
            c_row,
            a_rows_norm,
            bu_norm,
            c_row_norm,
            a_pair_cols,
            state_limit8,
            inverse_state_limit,
            bv_norm,
            crfb_sparse: matches_crfb_sparsity(coeffs),
            rng: XorShift64::new(seed),
            dither_scale: QUANTIZER_DITHER_SCALE,
            prev_v: 1.0,
            stability_resets: 0,
            state_clamps: 0,
        })
    }

    /// Reset integrator state while keeping the dither stream moving.
    pub fn reset(&mut self) {
        self.hard_reset();
    }

    pub(super) fn hard_reset(&mut self) {
        self.state = [0.0; 8];
        self.prev_v = 1.0;
    }

    pub fn stability_resets(&self) -> u64 {
        self.stability_resets
    }

    pub fn state_clamps(&self) -> u64 {
        self.state_clamps
    }

    pub fn set_dither_scale(&mut self, scale: f64) {
        if scale.is_finite() && scale >= 0.0 {
            self.dither_scale = scale;
        }
    }

    pub fn process_into_bits(&mut self, input: &[f64], out_bits: &mut Vec<u8>) {
        if self.crfb_sparse {
            self.process_standard_block::<true>(input, out_bits);
        } else {
            self.process_standard_block::<false>(input, out_bits);
        }
    }

    /// The standard quantizer has no buffered tail.
    pub fn flush_into_bits(&mut self, _out_bits: &mut Vec<u8>) {}

    #[inline(always)]
    fn process_standard_block<const SPARSE: bool>(
        &mut self,
        input: &[f64],
        out_bits: &mut Vec<u8>,
    ) {
        for &sample in input {
            let loop_output = self.loop_output::<SPARSE>(&self.state, sample);
            let quantized = self.rng.next_tpdf().mul_add(self.dither_scale, loop_output);
            let output = if quantized > 0.0 { 1.0 } else { -1.0 };
            let base =
                predict_base_next_state8::<SPARSE>(&self.state, &self.a_rows, &self.bu, sample);
            let mut next = affine8(&base, &self.bv, output);
            self.commit_state(&mut next);
            self.prev_v = output;
            out_bits.push(u8::from(output > 0.0));
        }
    }

    #[inline(always)]
    fn loop_output<const SPARSE: bool>(&self, state: &[f64; 8], input: f64) -> f64 {
        if SPARSE {
            self.coeffs.d1.mul_add(input, self.c_row[6] * state[6])
        } else {
            self.coeffs.d1.mul_add(input, dot8(&self.c_row, state))
        }
    }

    pub(super) fn predict_base_norm<const SPARSE: bool>(
        &self,
        state_norm: &[f64; 8],
        input: f64,
    ) -> [f64; 8] {
        #[cfg(target_arch = "aarch64")]
        if SPARSE {
            return self.predict_base_norm_sparse_neon(state_norm, input);
        }
        predict_base_next_state8::<SPARSE>(state_norm, &self.a_rows_norm, &self.bu_norm, input)
    }

    #[inline(always)]
    pub(super) fn loop_output_norm<const SPARSE: bool>(
        &self,
        state_norm: &[f64; 8],
        input: f64,
    ) -> f64 {
        if SPARSE {
            self.coeffs
                .d1
                .mul_add(input, self.c_row_norm[6] * state_norm[6])
        } else {
            self.coeffs
                .d1
                .mul_add(input, dot8(&self.c_row_norm, state_norm))
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn predict_base_norm_sparse_neon(&self, state: &[f64; 8], input: f64) -> [f64; 8] {
        use core::arch::aarch64::*;
        let mut next = [0.0; 8];
        next[0] = self.bu_norm[0].mul_add(input, self.a_rows_norm[0][0] * state[0]);
        // SAFETY: NEON is baseline on aarch64 and all pair accesses are in bounds.
        unsafe {
            for pair in 0..3 {
                let columns = &self.a_pair_cols[pair];
                let mut value = vmulq_n_f64(vld1q_f64(columns[3].as_ptr()), input);
                value = vfmaq_n_f64(value, vld1q_f64(columns[0].as_ptr()), state[2 * pair]);
                value = vfmaq_n_f64(value, vld1q_f64(columns[1].as_ptr()), state[2 * pair + 1]);
                value = vfmaq_n_f64(value, vld1q_f64(columns[2].as_ptr()), state[2 * pair + 2]);
                vst1q_f64(next.as_mut_ptr().add(2 * pair + 1), value);
            }
        }
        next
    }

    fn commit_state(&mut self, next: &mut [f64; 8]) {
        match stabilize_state(next, &self.coeffs.state_limit, &self.inverse_state_limit) {
            StateStability::Ok { clamped } => {
                self.state_clamps = self.state_clamps.wrapping_add(u64::from(clamped));
                self.state = *next;
            }
            StateStability::Reset => {
                self.hard_reset();
                self.stability_resets = self.stability_resets.wrapping_add(1);
            }
        }
    }
}
