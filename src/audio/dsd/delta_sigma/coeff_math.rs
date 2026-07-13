use super::modulator::{
    EC_DC_BIAS_DECAY, EC_DC_BIAS_WEIGHT, EC_QUANTIZER_ERROR_WEIGHT, EC_STATE_LIMIT_SOFT_KNEE,
    EC_STATE_LIMIT_SOFT_KNEE_INV_SPAN, EC_STATE_LIMIT_SOFT_KNEE_SQ, EC_STATE_LIMIT_WEIGHT,
    EC_STATE_PRESSURE_INV_COUNT, EC_STATE_PRESSURE_WEIGHT, EC_TRANSITION_WEIGHT,
};
use crate::audio::dsd::dsd_coeffs::ModulatorCoeffs;

/// `max_i |a[i]|` over the 7 live lanes.
#[inline(always)]
pub(super) fn max_abs7(a: &[f64; 8]) -> f64 {
    let mut peak = a[0].abs();
    for &lane in &a[1..7] {
        peak = peak.max(lane.abs());
    }
    peak
}

#[inline(always)]
pub(super) fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

#[inline(always)]
pub(super) fn score_relative_margin(a: f64, b: f64) -> f64 {
    if !a.is_finite() || !b.is_finite() {
        return f64::INFINITY;
    }
    (a - b).abs() / (0.5 * (a.abs() + b.abs())).max(1.0e-12)
}

#[inline(always)]
pub(super) fn candidate_pressure(state_norm: &[f64; 8]) -> f64 {
    let mut pressure = 0.0;
    for lane in &state_norm[..7] {
        pressure += lane * lane;
    }
    pressure * EC_STATE_PRESSURE_INV_COUNT
}

#[inline(always)]
pub(super) fn dot8(a: &[f64; 8], b: &[f64; 8]) -> f64 {
    // Two interleaved FMA chains: halves the dependency-chain latency vs a single
    // serial accumulator while keeping deterministic results.
    let mut acc0 = 0.0;
    let mut acc1 = 0.0;
    for k in (0..8).step_by(2) {
        acc0 = a[k].mul_add(b[k], acc0);
        acc1 = a[k + 1].mul_add(b[k + 1], acc1);
    }
    acc0 + acc1
}

/// CRFB band-sparsity pattern: `true` marks the columns row `i` of `A` may use.
/// Row 0 is the first integrator; each subsequent resonator pair only couples to
/// its predecessor and its own resonance partner.
const CRFB_A_PATTERN: [[bool; 7]; 7] = [
    [true, false, false, false, false, false, false],
    [true, true, true, false, false, false, false],
    [true, true, true, false, false, false, false],
    [false, false, true, true, true, false, false],
    [false, false, true, true, true, false, false],
    [false, false, false, false, true, true, true],
    [false, false, false, false, true, true, true],
];

/// True when the table fits the CRFB band pattern with a single nonzero `c[6]`,
/// i.e. the sparse hot-path kernels compute exactly the same products as the
/// dense ones.
#[allow(clippy::needless_range_loop)]
pub(super) fn matches_crfb_sparsity(coeffs: &ModulatorCoeffs) -> bool {
    for i in 0..7 {
        for k in 0..7 {
            if !CRFB_A_PATTERN[i][k] && coeffs.a[i][k] != 0.0 {
                return false;
            }
        }
    }
    coeffs.c[..6].iter().all(|&c| c == 0.0)
}

/// `x' = A·x + bu·u` without the feedback term. The sparse kernel hand-unrolls
/// the CRFB band pattern (19 nonzeros instead of 49).
#[inline(always)]
pub(super) fn predict_base_next_state8<const SPARSE: bool>(
    state: &[f64; 8],
    a: &[[f64; 8]; 7],
    bu: &[f64; 7],
    u: f64,
) -> [f64; 8] {
    let mut next = [0.0f64; 8];
    if SPARSE {
        let [s0, s1, s2, s3, s4, s5, s6, _] = *state;
        next[0] = bu[0].mul_add(u, a[0][0] * s0);
        next[1] = bu[1].mul_add(u, a[1][2].mul_add(s2, a[1][1].mul_add(s1, a[1][0] * s0)));
        next[2] = bu[2].mul_add(u, a[2][2].mul_add(s2, a[2][1].mul_add(s1, a[2][0] * s0)));
        next[3] = bu[3].mul_add(u, a[3][4].mul_add(s4, a[3][3].mul_add(s3, a[3][2] * s2)));
        next[4] = bu[4].mul_add(u, a[4][4].mul_add(s4, a[4][3].mul_add(s3, a[4][2] * s2)));
        next[5] = bu[5].mul_add(u, a[5][6].mul_add(s6, a[5][5].mul_add(s5, a[5][4] * s4)));
        next[6] = bu[6].mul_add(u, a[6][6].mul_add(s6, a[6][5].mul_add(s5, a[6][4] * s4)));
    } else {
        for i in 0..7 {
            next[i] = bu[i].mul_add(u, dot8(&a[i], state));
        }
    }
    next
}

/// Shared dot products of the candidate-score pressure, computed over the
/// zero-padded 8-lane arrays (lane 7 is structurally zero in every input):
/// returns `(S, T, hot)` with `S = Σ b²`, `T = Σ b·bn`, and `hot` true iff some
/// candidate's peak normalized state can clear the soft knee — exactly
/// `b_i² > thr_sq_i` for some `i` (see `knee_thr_sq`), which reuses the already
/// computed squares instead of an abs/FMA/max chain.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub(super) fn score_pair_dots(b: &[f64; 8], bn: &[f64; 8], thr_sq: &[f64; 8]) -> (f64, f64, bool) {
    use core::arch::aarch64::*;
    // SAFETY: NEON is baseline on aarch64; all loads come from in-bounds array
    // pointers.
    unsafe {
        let b0 = vld1q_f64(b.as_ptr());
        let b1 = vld1q_f64(b.as_ptr().add(2));
        let b2 = vld1q_f64(b.as_ptr().add(4));
        let b3 = vld1q_f64(b.as_ptr().add(6));

        let q0 = vmulq_f64(b0, b0);
        let q1 = vmulq_f64(b1, b1);
        let q2 = vmulq_f64(b2, b2);
        let q3 = vmulq_f64(b3, b3);
        let s = vaddvq_f64(vaddq_f64(vaddq_f64(q0, q1), vaddq_f64(q2, q3)));

        let n0 = vld1q_f64(bn.as_ptr());
        let n1 = vld1q_f64(bn.as_ptr().add(2));
        let n2 = vld1q_f64(bn.as_ptr().add(4));
        let n3 = vld1q_f64(bn.as_ptr().add(6));
        let t_a = vfmaq_f64(vmulq_f64(b0, n0), b2, n2);
        let t_b = vfmaq_f64(vmulq_f64(b1, n1), b3, n3);
        let t = vaddvq_f64(vaddq_f64(t_a, t_b));

        let h0 = vcgtq_f64(q0, vld1q_f64(thr_sq.as_ptr()));
        let h1 = vcgtq_f64(q1, vld1q_f64(thr_sq.as_ptr().add(2)));
        let h2 = vcgtq_f64(q2, vld1q_f64(thr_sq.as_ptr().add(4)));
        let h3 = vcgtq_f64(q3, vld1q_f64(thr_sq.as_ptr().add(6)));
        let any = vorrq_u64(vorrq_u64(h0, h1), vorrq_u64(h2, h3));
        let hot = (vgetq_lane_u64(any, 0) | vgetq_lane_u64(any, 1)) != 0;

        (s, t, hot)
    }
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub(super) fn score_pair_dots_pressure(
    b: &[f64; 8],
    bn: &[f64; 8],
    thr_sq: &[f64; 8],
) -> (f64, f64, bool, f64) {
    let (s, t, hot) = score_pair_dots(b, bn, thr_sq);
    (s, t, hot, max_abs7(b))
}

#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx2",
    target_feature = "fma"
))]
#[inline(always)]
pub(super) fn score_pair_dots(b: &[f64; 8], bn: &[f64; 8], thr_sq: &[f64; 8]) -> (f64, f64, bool) {
    use core::arch::x86_64::*;
    // SAFETY: this function is compiled only when AVX2+FMA are enabled for the
    // target; all loads/stores use in-bounds array pointers.
    unsafe {
        let b0 = _mm256_loadu_pd(b.as_ptr());
        let b1 = _mm256_loadu_pd(b.as_ptr().add(4));

        let q0 = _mm256_mul_pd(b0, b0);
        let q1 = _mm256_mul_pd(b1, b1);

        let n0 = _mm256_loadu_pd(bn.as_ptr());
        let n1 = _mm256_loadu_pd(bn.as_ptr().add(4));
        let t0 = _mm256_mul_pd(b0, n0);
        let t1 = _mm256_mul_pd(b1, n1);

        let th0 = _mm256_loadu_pd(thr_sq.as_ptr());
        let th1 = _mm256_loadu_pd(thr_sq.as_ptr().add(4));
        let hot0 = _mm256_cmp_pd::<{ _CMP_GT_OQ }>(q0, th0);
        let hot1 = _mm256_cmp_pd::<{ _CMP_GT_OQ }>(q1, th1);
        let hot = _mm256_movemask_pd(_mm256_or_pd(hot0, hot1)) != 0;

        let mut lanes = [0.0f64; 4];
        _mm256_storeu_pd(lanes.as_mut_ptr(), _mm256_add_pd(q0, q1));
        let s = (lanes[0] + lanes[1]) + (lanes[2] + lanes[3]);
        _mm256_storeu_pd(lanes.as_mut_ptr(), _mm256_add_pd(t0, t1));
        let t = (lanes[0] + lanes[1]) + (lanes[2] + lanes[3]);

        (s, t, hot)
    }
}

#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx2",
    target_feature = "fma"
))]
#[inline(always)]
pub(super) fn score_pair_dots_pressure(
    b: &[f64; 8],
    bn: &[f64; 8],
    thr_sq: &[f64; 8],
) -> (f64, f64, bool, f64) {
    let (s, t, hot) = score_pair_dots(b, bn, thr_sq);
    (s, t, hot, max_abs7(b))
}

#[cfg(all(
    not(target_arch = "aarch64"),
    not(all(
        target_arch = "x86_64",
        target_feature = "avx2",
        target_feature = "fma"
    ))
))]
#[inline(always)]
pub(super) fn score_pair_dots(b: &[f64; 8], bn: &[f64; 8], thr_sq: &[f64; 8]) -> (f64, f64, bool) {
    let mut s = 0.0f64;
    let mut t = 0.0f64;
    let mut hot = false;
    for i in 0..8 {
        let q = b[i] * b[i];
        s += q;
        t = b[i].mul_add(bn[i], t);
        hot |= q > thr_sq[i];
    }
    (s, t, hot)
}

#[cfg(all(
    not(target_arch = "aarch64"),
    not(all(
        target_arch = "x86_64",
        target_feature = "avx2",
        target_feature = "fma"
    ))
))]
#[inline(always)]
pub(super) fn score_pair_dots_pressure(
    b: &[f64; 8],
    bn: &[f64; 8],
    thr_sq: &[f64; 8],
) -> (f64, f64, bool, f64) {
    let mut s = 0.0f64;
    let mut t = 0.0f64;
    let mut hot = false;
    let mut pressure = b[0].abs();
    for i in 0..8 {
        let q = b[i] * b[i];
        s += q;
        t = b[i].mul_add(bn[i], t);
        hot |= q > thr_sq[i];
        if (1..7).contains(&i) {
            pressure = pressure.max(b[i].abs());
        }
    }
    (s, t, hot, pressure)
}

/// `base + f·col` over the padded 8-lane arrays (lane 7 is structurally zero in
/// every input, so the result keeps a zero lane 7).
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub(super) fn affine8(base: &[f64; 8], col: &[f64; 8], f: f64) -> [f64; 8] {
    use core::arch::aarch64::*;
    let mut out = [0.0f64; 8];
    // SAFETY: NEON is baseline on aarch64; all loads/stores are in-bounds.
    unsafe {
        for k in (0..8).step_by(2) {
            let v = vfmaq_n_f64(
                vld1q_f64(base.as_ptr().add(k)),
                vld1q_f64(col.as_ptr().add(k)),
                f,
            );
            vst1q_f64(out.as_mut_ptr().add(k), v);
        }
    }
    out
}

#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx2",
    target_feature = "fma"
))]
#[inline(always)]
pub(super) fn affine8(base: &[f64; 8], col: &[f64; 8], f: f64) -> [f64; 8] {
    use core::arch::x86_64::*;
    let mut out = [0.0f64; 8];
    // SAFETY: this function is compiled only when AVX2+FMA are enabled for the
    // target; all loads/stores use in-bounds array pointers.
    unsafe {
        let vf = _mm256_set1_pd(f);
        let b0 = _mm256_loadu_pd(base.as_ptr());
        let c0 = _mm256_loadu_pd(col.as_ptr());
        let r0 = _mm256_fmadd_pd(vf, c0, b0);
        _mm256_storeu_pd(out.as_mut_ptr(), r0);

        let b1 = _mm256_loadu_pd(base.as_ptr().add(4));
        let c1 = _mm256_loadu_pd(col.as_ptr().add(4));
        let r1 = _mm256_fmadd_pd(vf, c1, b1);
        _mm256_storeu_pd(out.as_mut_ptr().add(4), r1);
    }
    out
}

#[cfg(all(
    not(target_arch = "aarch64"),
    not(all(
        target_arch = "x86_64",
        target_feature = "avx2",
        target_feature = "fma"
    ))
))]
#[inline(always)]
pub(super) fn affine8(base: &[f64; 8], col: &[f64; 8], f: f64) -> [f64; 8] {
    let mut out = [0.0f64; 8];
    for i in 0..8 {
        out[i] = f.mul_add(col[i], base[i]);
    }
    out
}

/// Element-wise product over the padded 8-lane arrays.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub(super) fn mul8(a: &[f64; 8], b: &[f64; 8]) -> [f64; 8] {
    use core::arch::aarch64::*;
    let mut out = [0.0f64; 8];
    // SAFETY: NEON is baseline on aarch64; all loads/stores are in-bounds.
    unsafe {
        for k in (0..8).step_by(2) {
            let v = vmulq_f64(vld1q_f64(a.as_ptr().add(k)), vld1q_f64(b.as_ptr().add(k)));
            vst1q_f64(out.as_mut_ptr().add(k), v);
        }
    }
    out
}

#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx2",
    target_feature = "fma"
))]
#[inline(always)]
pub(super) fn mul8(a: &[f64; 8], b: &[f64; 8]) -> [f64; 8] {
    use core::arch::x86_64::*;
    let mut out = [0.0f64; 8];
    // SAFETY: this function is compiled only when AVX2+FMA are enabled for the
    // target; all loads/stores use in-bounds array pointers.
    unsafe {
        let r0 = _mm256_mul_pd(_mm256_loadu_pd(a.as_ptr()), _mm256_loadu_pd(b.as_ptr()));
        let r1 = _mm256_mul_pd(
            _mm256_loadu_pd(a.as_ptr().add(4)),
            _mm256_loadu_pd(b.as_ptr().add(4)),
        );
        _mm256_storeu_pd(out.as_mut_ptr(), r0);
        _mm256_storeu_pd(out.as_mut_ptr().add(4), r1);
    }
    out
}

#[cfg(all(
    not(target_arch = "aarch64"),
    not(all(
        target_arch = "x86_64",
        target_feature = "avx2",
        target_feature = "fma"
    ))
))]
#[inline(always)]
pub(super) fn mul8(a: &[f64; 8], b: &[f64; 8]) -> [f64; 8] {
    let mut out = [0.0f64; 8];
    for i in 0..8 {
        out[i] = a[i] * b[i];
    }
    out
}

/// `(base_norm * state_limit) + f*bv` over padded 8-lane arrays.
///
/// This keeps the same rounding order as `affine8(&mul8(...), ...)`: first
/// denormalize the predicted state, then use that rounded value as the FMA
/// accumulator.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub(super) fn denormalized_feedback8(
    base_norm: &[f64; 8],
    state_limit: &[f64; 8],
    bv: &[f64; 8],
    f: f64,
) -> [f64; 8] {
    use core::arch::aarch64::*;
    let mut out = [0.0f64; 8];
    // SAFETY: NEON is baseline on aarch64; all loads/stores are in-bounds.
    unsafe {
        for k in (0..8).step_by(2) {
            let base = vmulq_f64(
                vld1q_f64(base_norm.as_ptr().add(k)),
                vld1q_f64(state_limit.as_ptr().add(k)),
            );
            let v = vfmaq_n_f64(base, vld1q_f64(bv.as_ptr().add(k)), f);
            vst1q_f64(out.as_mut_ptr().add(k), v);
        }
    }
    out
}

#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx2",
    target_feature = "fma"
))]
#[inline(always)]
pub(super) fn denormalized_feedback8(
    base_norm: &[f64; 8],
    state_limit: &[f64; 8],
    bv: &[f64; 8],
    f: f64,
) -> [f64; 8] {
    use core::arch::x86_64::*;
    let mut out = [0.0f64; 8];
    // SAFETY: this function is compiled only when AVX2+FMA are enabled for the
    // target; all loads/stores use in-bounds array pointers.
    unsafe {
        let vf = _mm256_set1_pd(f);
        let base0 = _mm256_mul_pd(
            _mm256_loadu_pd(base_norm.as_ptr()),
            _mm256_loadu_pd(state_limit.as_ptr()),
        );
        let bv0 = _mm256_loadu_pd(bv.as_ptr());
        let r0 = _mm256_fmadd_pd(vf, bv0, base0);
        _mm256_storeu_pd(out.as_mut_ptr(), r0);

        let base1 = _mm256_mul_pd(
            _mm256_loadu_pd(base_norm.as_ptr().add(4)),
            _mm256_loadu_pd(state_limit.as_ptr().add(4)),
        );
        let bv1 = _mm256_loadu_pd(bv.as_ptr().add(4));
        let r1 = _mm256_fmadd_pd(vf, bv1, base1);
        _mm256_storeu_pd(out.as_mut_ptr().add(4), r1);
    }
    out
}

#[cfg(all(
    not(target_arch = "aarch64"),
    not(all(
        target_arch = "x86_64",
        target_feature = "avx2",
        target_feature = "fma"
    ))
))]
#[inline(always)]
pub(super) fn denormalized_feedback8(
    base_norm: &[f64; 8],
    state_limit: &[f64; 8],
    bv: &[f64; 8],
    f: f64,
) -> [f64; 8] {
    let mut out = [0.0f64; 8];
    for i in 0..8 {
        let base = base_norm[i] * state_limit[i];
        out[i] = f.mul_add(bv[i], base);
    }
    out
}

#[inline]
pub fn compensated_feedback(prev_v: f64, v: f64, isi_penalty: f64) -> f64 {
    if v != prev_v {
        v * (1.0 - isi_penalty)
    } else {
        v
    }
}

pub fn updated_dc_bias(previous: f64, v: f64) -> f64 {
    EC_DC_BIAS_DECAY * previous + (1.0 - EC_DC_BIAS_DECAY) * v
}

/// One-pole decay giving the DC tracker a −3 dB corner at `corner_hz` for a
/// modulator running at `wire_rate`: `a = 1 − 2π·f_c/fs`. A corner at or below
/// 20 Hz makes the tracker a true DC servo with a rate-invariant time constant;
/// the legacy [`EC_DC_BIAS_DECAY`] corresponds to ~225 Hz at DSD64's 2.8224 MHz.
pub fn dc_bias_decay_for_corner_hz(corner_hz: f64, wire_rate: u32) -> f64 {
    if !corner_hz.is_finite() || corner_hz <= 0.0 || wire_rate == 0 {
        return EC_DC_BIAS_DECAY;
    }
    (1.0 - core::f64::consts::TAU * corner_hz / wire_rate as f64).clamp(0.0, 1.0 - f64::EPSILON)
}

pub fn ec_candidate_score(
    y_quantized: f64,
    v: f64,
    next: &[f64; 7],
    state_limit: &[f64; 7],
    prev_v: f64,
    dc_bias: f64,
) -> f64 {
    let mut inverse_state_limit = [0.0; 8];
    for i in 0..7 {
        inverse_state_limit[i] = if state_limit[i].is_finite() && state_limit[i] > 0.0 {
            1.0 / state_limit[i]
        } else {
            0.0
        };
    }
    ec_candidate_score8(
        y_quantized,
        v,
        &pad_state7(next),
        &inverse_state_limit,
        prev_v,
        dc_bias,
    )
}

#[inline]
pub(super) fn ec_candidate_score8(
    y_quantized: f64,
    v: f64,
    next: &[f64; 8],
    inverse_state_limit: &[f64; 8],
    prev_v: f64,
    dc_bias: f64,
) -> f64 {
    let quantizer_error = y_quantized - v;
    let mut pressure = 0.0;
    let mut peak_sq = 0.0f64;
    // Measured: the 7-lane loop beats the zero-padded 8-lane form on M-series.
    for i in 0..7 {
        let normalized = next[i] * inverse_state_limit[i];
        let sq = normalized * normalized;
        pressure += sq;
        peak_sq = peak_sq.max(sq);
    }
    // The knee/overflow penalty is exactly zero unless some integrator is past
    // the soft knee — rare in normal operation, so gate the expensive loop on
    // the peak squared-normalized state instead of paying it per candidate.
    let limit_penalty = if peak_sq > EC_STATE_LIMIT_SOFT_KNEE_SQ {
        let mut penalty = 0.0;
        for i in 0..7 {
            let normalized = next[i].abs() * inverse_state_limit[i];
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
    pressure *= EC_STATE_PRESSURE_INV_COUNT;

    let transition = if v != prev_v { 1.0 } else { 0.0 };
    let next_bias = updated_dc_bias(dc_bias, v);

    EC_QUANTIZER_ERROR_WEIGHT * quantizer_error * quantizer_error
        + EC_STATE_PRESSURE_WEIGHT * pressure
        + EC_STATE_LIMIT_WEIGHT * limit_penalty
        + EC_TRANSITION_WEIGHT * transition
        + EC_DC_BIAS_WEIGHT * next_bias * next_bias
}

pub(super) fn pad_state7(state: &[f64; 7]) -> [f64; 8] {
    let mut padded = [0.0; 8];
    padded[..7].copy_from_slice(state);
    padded
}

#[inline(always)]
pub(super) fn select_ec_candidate(c_plus: f64, c_minus: f64) -> (f64, bool) {
    if c_plus.is_finite() && (!c_minus.is_finite() || c_plus <= c_minus) {
        (1.0, true)
    } else if c_minus.is_finite() {
        (-1.0, true)
    } else {
        // Both candidate scores were non-finite (pathological state). Match the
        // EC root fallback so the commit stabilizer can reset/clamp as needed
        // while still emitting one bit.
        (1.0, false)
    }
}
