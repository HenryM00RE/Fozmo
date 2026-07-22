use crate::audio::dsd::dsd_coeffs::ModulatorCoeffs;

#[inline(always)]
pub(super) fn dot8(a: &[f64; 8], b: &[f64; 8]) -> f64 {
    let mut acc0 = 0.0;
    let mut acc1 = 0.0;
    for k in (0..8).step_by(2) {
        acc0 = a[k].mul_add(b[k], acc0);
        acc1 = a[k + 1].mul_add(b[k + 1], acc1);
    }
    acc0 + acc1
}

/// CRFB band-sparsity pattern. Each resonator pair only couples to its
/// predecessor and its own resonance partner.
const CRFB_A_PATTERN: [[bool; 7]; 7] = [
    [true, false, false, false, false, false, false],
    [true, true, true, false, false, false, false],
    [true, true, true, false, false, false, false],
    [false, false, true, true, true, false, false],
    [false, false, true, true, true, false, false],
    [false, false, false, false, true, true, true],
    [false, false, false, false, true, true, true],
];

#[allow(clippy::needless_range_loop)]
pub(super) fn matches_crfb_sparsity(coeffs: &ModulatorCoeffs) -> bool {
    for i in 0..7 {
        for k in 0..7 {
            if !CRFB_A_PATTERN[i][k] && coeffs.a[i][k] != 0.0 {
                return false;
            }
        }
    }
    coeffs.c[..6].iter().all(|&coefficient| coefficient == 0.0)
}

/// `x' = A*x + bu*u` without the feedback term.
#[inline(always)]
pub(super) fn predict_base_next_state8<const SPARSE: bool>(
    state: &[f64; 8],
    a: &[[f64; 8]; 7],
    bu: &[f64; 7],
    u: f64,
) -> [f64; 8] {
    let mut next = [0.0; 8];
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

/// `base + feedback*column` over the padded eight-lane state.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub(super) fn affine8(base: &[f64; 8], column: &[f64; 8], feedback: f64) -> [f64; 8] {
    use core::arch::aarch64::*;
    let mut out = [0.0; 8];
    // SAFETY: NEON is baseline on aarch64 and every two-lane access is in bounds.
    unsafe {
        for k in (0..8).step_by(2) {
            let value = vfmaq_n_f64(
                vld1q_f64(base.as_ptr().add(k)),
                vld1q_f64(column.as_ptr().add(k)),
                feedback,
            );
            vst1q_f64(out.as_mut_ptr().add(k), value);
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
pub(super) fn affine8(base: &[f64; 8], column: &[f64; 8], feedback: f64) -> [f64; 8] {
    use core::arch::x86_64::*;
    let mut out = [0.0; 8];
    // SAFETY: compiled only for AVX2+FMA; all loads and stores are in bounds.
    unsafe {
        let factor = _mm256_set1_pd(feedback);
        for k in [0, 4] {
            let result = _mm256_fmadd_pd(
                factor,
                _mm256_loadu_pd(column.as_ptr().add(k)),
                _mm256_loadu_pd(base.as_ptr().add(k)),
            );
            _mm256_storeu_pd(out.as_mut_ptr().add(k), result);
        }
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
pub(super) fn affine8(base: &[f64; 8], column: &[f64; 8], feedback: f64) -> [f64; 8] {
    core::array::from_fn(|index| feedback.mul_add(column[index], base[index]))
}

/// Element-wise product over the padded eight-lane state.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub(super) fn mul8(a: &[f64; 8], b: &[f64; 8]) -> [f64; 8] {
    use core::arch::aarch64::*;
    let mut out = [0.0; 8];
    // SAFETY: NEON is baseline on aarch64 and every two-lane access is in bounds.
    unsafe {
        for k in (0..8).step_by(2) {
            let value = vmulq_f64(vld1q_f64(a.as_ptr().add(k)), vld1q_f64(b.as_ptr().add(k)));
            vst1q_f64(out.as_mut_ptr().add(k), value);
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
    let mut out = [0.0; 8];
    // SAFETY: compiled only for AVX2+FMA; all loads and stores are in bounds.
    unsafe {
        for k in [0, 4] {
            let result = _mm256_mul_pd(
                _mm256_loadu_pd(a.as_ptr().add(k)),
                _mm256_loadu_pd(b.as_ptr().add(k)),
            );
            _mm256_storeu_pd(out.as_mut_ptr().add(k), result);
        }
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
    core::array::from_fn(|index| a[index] * b[index])
}

/// `(base_norm * state_limit) + feedback*bv`. Multiplication is deliberately
/// rounded before the FMA so scalar and SIMD EcBeam2 paths agree.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub(super) fn denormalized_feedback8(
    base_norm: &[f64; 8],
    state_limit: &[f64; 8],
    bv: &[f64; 8],
    feedback: f64,
) -> [f64; 8] {
    use core::arch::aarch64::*;
    let mut out = [0.0; 8];
    // SAFETY: NEON is baseline on aarch64 and every two-lane access is in bounds.
    unsafe {
        for k in (0..8).step_by(2) {
            let base = vmulq_f64(
                vld1q_f64(base_norm.as_ptr().add(k)),
                vld1q_f64(state_limit.as_ptr().add(k)),
            );
            let value = vfmaq_n_f64(base, vld1q_f64(bv.as_ptr().add(k)), feedback);
            vst1q_f64(out.as_mut_ptr().add(k), value);
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
    feedback: f64,
) -> [f64; 8] {
    use core::arch::x86_64::*;
    let mut out = [0.0; 8];
    // SAFETY: compiled only for AVX2+FMA; all loads and stores are in bounds.
    unsafe {
        let factor = _mm256_set1_pd(feedback);
        for k in [0, 4] {
            let base = _mm256_mul_pd(
                _mm256_loadu_pd(base_norm.as_ptr().add(k)),
                _mm256_loadu_pd(state_limit.as_ptr().add(k)),
            );
            let result = _mm256_fmadd_pd(factor, _mm256_loadu_pd(bv.as_ptr().add(k)), base);
            _mm256_storeu_pd(out.as_mut_ptr().add(k), result);
        }
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
    feedback: f64,
) -> [f64; 8] {
    core::array::from_fn(|index| {
        let base = base_norm[index] * state_limit[index];
        feedback.mul_add(bv[index], base)
    })
}
