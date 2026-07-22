#!/usr/bin/env python3
# Generates the 7th-order CRFB ABCD state-space tables used by
# src/audio/dsd/dsd_coeffs.rs.
#
# Run: python3 -m venv tools/.venv && tools/.venv/bin/pip install deltasigma numpy scipy
# Then: tools/.venv/bin/python tools/gen_crfb.py --out src/audio/dsd/dsd_coeffs.rs
#
# Pipeline per (OSR, out-of-band-gain) variant:
#   1. synthesizeNTF(order=7, osr, opt=1, H_inf=obg)  -- optimized (spread) zeros,
#      explicit out-of-band gain instead of the default Lee value.
#   2. realizeNTF(..., 'CRFB') + stuffABCD             -- ABCD state-space form.
#   3. Vectorized 1-bit simulation sweep (DC + sines)  -- measured stable input limit.
#   4. Dynamic-range scaling (diagonal similarity)     -- every integrator normalized
#      so its worst-case swing at the recommended input peak is ~1.0.
#   5. Per-state saturation limits = 1.5x the post-scaling measured maxima.
#
# Each emitted table therefore carries its own measured `input_peak` (the modulator
# input amplitude the loop is guaranteed stable at, with margin) and `state_limit`
# array, replacing the old global -4 dB constant and the single 2.0 clamp.
#
# ABCD form (Schreier delta-sigma toolbox simulateDSM convention):
#   y[n] = C * x[n] + D1 * u[n]
#   v[n] = sign(y[n])
#   x[n+1] = A * x[n] + B * [u[n]; v[n]]

import argparse
import sys
import math
import os
import tempfile
import json
import fractions
import collections
import collections.abc

os.environ.setdefault(
    "MPLCONFIGDIR",
    os.path.join(tempfile.gettempdir(), "fozmo-crfb-matplotlib"),
)
for _thread_env in (
    "OPENBLAS_NUM_THREADS",
    "OMP_NUM_THREADS",
    "MKL_NUM_THREADS",
    "VECLIB_MAXIMUM_THREADS",
    "NUMEXPR_NUM_THREADS",
):
    os.environ[_thread_env] = "1"

# deltasigma 0.2.2 predates several stdlib/NumPy cleanups. Stub the missing
# aliases before importing so the toolbox loads on modern Python + NumPy.
if not hasattr(fractions, "gcd"):
    fractions.gcd = math.gcd  # removed in Python 3.9, lives in math now.
# collections.{Iterable, Mapping, ...} aliases were removed in Python 3.10.
for _abc_name in ("Iterable", "Mapping", "MutableMapping", "Sequence",
                  "MutableSequence", "Container", "Callable"):
    if _abc_name not in vars(collections) and hasattr(collections.abc, _abc_name):
        setattr(collections, _abc_name, getattr(collections.abc, _abc_name))

import numpy as np
# NumPy >= 1.20 deprecated and NumPy 2.0 removed these builtin aliases.
# Restore as plain-builtin shims — deltasigma uses them in type annotations
# and dtype spellings, which the builtins satisfy.
for _attr, _val in (
    ("float", float),
    ("int", int),
    ("bool", bool),
    ("complex", complex),
):
    try:
        getattr(np, _attr)
    except AttributeError:
        setattr(np, _attr, _val)

# numpy.distutils was removed from NumPy wheels on Python 3.12+. deltasigma's
# _config.py only uses it to locate BLAS headers for an optional Cython
# accelerator we never call — stub the surface so the import path succeeds.
import types as _types
if not hasattr(np, "distutils"):
    _np_distutils = _types.ModuleType("numpy.distutils")
    _np_system_info = _types.ModuleType("numpy.distutils.system_info")
    _np_system_info.get_info = lambda _name: {}
    _np_distutils.system_info = _np_system_info
    sys.modules["numpy.distutils"] = _np_distutils
    sys.modules["numpy.distutils.system_info"] = _np_system_info
    np.distutils = _np_distutils

# scipy.signal.step2 was removed (it was deprecated as a duplicate of `step`).
# deltasigma's _pulse module imports it eagerly even though we don't call pulse().
import scipy.signal as _sps
if not hasattr(_sps, "step2"):
    _sps.step2 = _sps.step

# fractions.gcd moved to math.gcd in Python 3.9; deltasigma._utils still reads
# the old location at import time.
import fractions as _fractions
import math as _math
if not hasattr(_fractions, "gcd"):
    _fractions.gcd = _math.gcd

from deltasigma import synthesizeNTF, realizeNTF


def deterministic_complex_lstsq(matrix, rhs, rcond=None):
    """Solve the small CRFB complex least-squares system deterministically."""
    del rcond
    a = np.asarray(matrix, dtype=complex)
    b = np.asarray(rhs, dtype=complex)
    if a.ndim != 2 or b.ndim not in (1, 2) or a.shape[0] != b.shape[0]:
        raise ValueError("deterministic CRFB least-squares shape mismatch")
    vector_rhs = b.ndim == 1
    if vector_rhs:
        b = b[:, None]
    rows, columns = a.shape
    rhs_columns = b.shape[1]
    q = [[0j for _ in range(rows)] for _ in range(columns)]
    r = [[0j for _ in range(columns)] for _ in range(columns)]
    for column in range(columns):
        work = [complex(a[row, column]) for row in range(rows)]
        for previous in range(column):
            projection = 0j
            for row in range(rows):
                projection += q[previous][row].conjugate() * work[row]
            r[previous][column] = projection
            for row in range(rows):
                work[row] -= projection * q[previous][row]
        norm_squared = 0.0
        for value in work:
            norm_squared += value.real * value.real + value.imag * value.imag
        norm = math.sqrt(norm_squared)
        if not math.isfinite(norm) or norm <= 1e-15:
            raise RuntimeError("deterministic CRFB least-squares matrix is rank deficient")
        r[column][column] = complex(norm, 0.0)
        for row in range(rows):
            q[column][row] = work[row] / norm

    projected = [[0j for _ in range(rhs_columns)] for _ in range(columns)]
    for column in range(columns):
        for rhs_column in range(rhs_columns):
            value = 0j
            for row in range(rows):
                value += q[column][row].conjugate() * complex(b[row, rhs_column])
            projected[column][rhs_column] = value

    solution = [[0j for _ in range(rhs_columns)] for _ in range(columns)]
    for rhs_column in range(rhs_columns):
        for row in range(columns - 1, -1, -1):
            value = projected[row][rhs_column]
            for column in range(row + 1, columns):
                value -= r[row][column] * solution[column][rhs_column]
            solution[row][rhs_column] = value / r[row][row]
    result = np.asarray(solution, dtype=complex)
    if vector_rhs:
        result = result[:, 0]
    residual = a @ result - np.asarray(rhs, dtype=complex)
    residuals = np.asarray([float(np.vdot(residual, residual).real)])
    return result, residuals, columns, np.asarray([], dtype=float)


def realize_ntf_crfb_deterministic(ntf):
    original = np.linalg.lstsq
    np.linalg.lstsq = deterministic_complex_lstsq
    try:
        return realizeNTF(ntf, form="CRFB")
    finally:
        np.linalg.lstsq = original


def stuff_abcd_crfb(a, g, b, c):
    """CRFB-only port of deltasigma.stuffABCD.

    Re-implemented here because the toolbox's version uses a list-of-arrays
    fancy-indexing pattern that NumPy 1.23+ broke. Behavior is byte-equivalent
    to the toolbox for the CRFB topology — verified by hand against
    Schreier's MATLAB reference and the toolbox source.
    """
    a = np.asarray(a).reshape((1, -1))
    g = np.asarray(g).reshape((1, -1))
    b = np.asarray(b).reshape((1, -1))
    c = np.asarray(c).reshape((1, -1))
    order = max(a.shape)
    odd = order % 2
    even = 1 - odd
    ABCD = np.zeros((order + 1, order + 2))
    if b.size == 1:
        b = np.hstack((np.atleast_2d(b), np.zeros((1, order))))

    # B1 column = b, B2 column = -a (top order rows).
    ABCD[:, order] = b.ravel()
    ABCD[:order, order + 1] = -a.ravel()

    # Diagonal of the top-left order×order block = 1.
    for i in range(order):
        ABCD[i, i] = 1.0

    # Sub-diagonal of the (order+1)×(order+2) matrix at offset -1:
    # positions (1,0), (2,1), ..., (order, order-1). Pick every other entry
    # starting from `even` and assign the matching c value.
    for k in range(even, order, 2):
        ABCD[k + 1, k] = c[0, k]

    # Super-diagonal of the top-left block (offset +1): pick every other
    # starting from `odd`, assign -g.
    if order > odd:
        for gi, k in enumerate(range(odd, order, 2)):
            ABCD[k, k + 1] = -g[0, gi]

    # Delaying integrator rows get c-weighted feedback from the prior row.
    dly = np.arange(odd + 1, order, 2)
    for r in dly:
        ABCD[r, :] = ABCD[r, :] + c[0, r - 1] * ABCD[r - 1, :]

    return ABCD


ORDER = 7

# Comparator-referred TPDF dither used by the Rust standard quantizer; mirrored in
# the simulations so the measured stability limits include its (small) cost.
DITHER_SCALE = 0.00390625

# Out-of-band gain family generated per OSR. The Lee criterion's 1.5 is the
# conservative anchor; the higher entries trade stable input range for deeper
# in-band suppression.
LOW_RATE_STANDARD_OBG_FAMILY = {
    # OBG 1.64 plants are used by 7th Order Search production. Its DSD64 exact-oracle
    # compatibility path additionally retains the frozen OBG 1.65 plant.
    64: [1.5, 1.64, 1.65],
    128: [1.5, 1.64],
    256: [1.6, 1.64],
}
HIGH_RATE_STANDARD_OBG_FAMILY = {
    512: [1.2, 1.3, 1.4, 1.45, 1.5, 1.6],
    1024: [1.4, 1.5, 1.6],
}
# Standard default table per OSR (must be in STANDARD_OBG_FAMILY and must survive the stability
# battery). DSD128 keeps the proven Lee value; DSD256 spends some OSR headroom
# on the hottest NTF that remains usable for the plain greedy quantizer.
# DSD64 has the least OSR headroom, so it stays on the proven Lee value too.
STANDARD_DEFAULT_OBG = {
    64: 1.5,
    128: 1.5,
    256: 1.6,
    # The DSD512 E2v3 quality sweep favored OBG 1.50 across the weighted
    # coherent-level and hi-res reconstruction metrics. DSD1024 remains on the
    # hottest calibrated table until it receives the same end-to-end sweep.
    512: 1.5,
    1024: 1.6,
}

# OSR64 is appended after the original entries so the shared RNG stream for
# the OSR128/OSR256 jobs is unchanged and their tables regenerate identically.
LOW_RATE_STANDARD_OSRS = [128, 256, 64]

# DSD512/DSD1024 are measurement-only Standard-modulator targets. Generate
# each candidate with its own documented seed, exactly like `--single`, so
# adding them cannot perturb any established Standard table.
HIGH_RATE_STANDARD_OSRS = [512, 1024]
TARGET_OSRS = LOW_RATE_STANDARD_OSRS + HIGH_RATE_STANDARD_OSRS

# Stability sweep: amplitudes tried per stimulus, divergence threshold, length.
SWEEP_AMPS = np.round(np.arange(0.30, 0.99, 0.02), 4)
DIVERGE_LIMIT = 1.0e3
N_SWEEP = 1 << 17
N_XMAX = 1 << 18

# Margins. input_peak = SAFETY * measured stable limit; per-state saturation
# limit = LIMIT_MARGIN * post-scaling measured maximum.
SAFETY = 0.95
LIMIT_MARGIN = 1.5
MIN_USABLE_INPUT_PEAK = 0.08


def batch_simulate_standard(A, b_u, b_v, C, d1, wave_fn, amps, n_steps, rng,
                            diverge=DIVERGE_LIMIT):
    """Simulate the 1-bit loop for K columns at once.

    `wave_fn(n) -> (K,)` unit-amplitude stimulus per column; `amps` scales it.
    Returns (peak (7,K), unstable (K,) bool). Columns that diverge are zeroed
    and flagged; their peaks are not meaningful afterwards.
    """
    k = len(amps)
    x = np.zeros((ORDER, k))
    peak = np.zeros((ORDER, k))
    unstable = np.zeros(k, dtype=bool)
    for n in range(n_steps):
        u = amps * wave_fn(n)
        y = C @ x + d1 * u
        dith = (rng.random(k) + rng.random(k) - 1.0) * DITHER_SCALE
        v = np.where(y + dith > 0.0, 1.0, -1.0)
        x = A @ x + np.outer(b_u, u) + np.outer(b_v, v)
        m = np.max(np.abs(x), axis=0)
        bad = ~np.isfinite(m) | (m > diverge)
        if bad.any():
            unstable |= bad
            x[:, bad] = 0.0
        np.maximum(peak, np.abs(x), out=peak)
    return peak, unstable


def batch_simulate(mode, A, b_u, b_v, C, d1, state_limit, wave_fn, amps, n_steps, rng,
                   diverge=DIVERGE_LIMIT):
    del mode, state_limit
    return batch_simulate_standard(
        A, b_u, b_v, C, d1, wave_fn, amps, n_steps, rng, diverge)


def split_abcd(ABCD):
    A = np.array(ABCD[:ORDER, :ORDER])
    b_u = np.array(ABCD[:ORDER, ORDER])
    b_v = np.array(ABCD[:ORDER, ORDER + 1])
    C = np.array(ABCD[ORDER, :ORDER])
    d1 = float(ABCD[ORDER, ORDER])
    return A, b_u, b_v, C, d1


def stable_input_limit(mode, A, b_u, b_v, C, d1, osr, rng, state_limit=None):
    """Sweep DC + two in-band sines upward; return the largest amplitude that
    stays stable across every stimulus (first-failure semantics per stimulus)."""
    fb = 0.5 / osr
    stimuli = [
        ("dc", None),
        ("sine_lo", 2.0 * math.pi * fb / 3.0),
        ("sine_hi", 2.0 * math.pi * fb * 0.9),
    ]
    n_amps = len(SWEEP_AMPS)
    amps = np.tile(SWEEP_AMPS, len(stimuli))

    def wave_fn(n):
        cols = []
        for _, w in stimuli:
            if w is None:
                cols.append(np.ones(n_amps))
            else:
                cols.append(np.full(n_amps, math.sin(w * n)))
        return np.concatenate(cols)

    if state_limit is None:
        state_limit = np.ones(ORDER)
    _, unstable = batch_simulate(
        mode, A, b_u, b_v, C, d1, state_limit, wave_fn, amps, N_SWEEP, rng)
    limit = float(SWEEP_AMPS[-1])
    detail = {}
    for s_idx, (name, _) in enumerate(stimuli):
        flags = unstable[s_idx * n_amps:(s_idx + 1) * n_amps]
        fail = np.argmax(flags) if flags.any() else None
        if fail is not None and fail == 0:
            stim_limit = float(SWEEP_AMPS[0])  # pathological; flagged below
        elif fail is not None:
            stim_limit = float(SWEEP_AMPS[fail - 1])
        else:
            stim_limit = float(SWEEP_AMPS[-1])
        detail[name] = stim_limit
        limit = min(limit, stim_limit)
    return limit, detail


def measure_state_maxima(mode, A, b_u, b_v, C, d1, osr, peak_amp, rng,
                         state_limit=None, seed_offset=0):
    """Worst-case per-state swing at the recommended operating peak across a
    stimulus battery (DC both polarities, three sines, white noise)."""
    fb = 0.5 / osr
    freqs = [2.0 * math.pi * fb * 0.05,
             2.0 * math.pi * fb / 3.0,
             2.0 * math.pi * fb * 0.9]
    noise_rng = np.random.default_rng(1234 + seed_offset)
    k = 2 + len(freqs) + 2  # DC+, DC-, sines, two noise columns
    amps = np.full(k, peak_amp)

    def wave_fn(n):
        cols = [1.0, -1.0] + [math.sin(w * n) for w in freqs]
        wave = np.array(cols + [0.0, 0.0])
        wave[-2:] = noise_rng.uniform(-1.0, 1.0, 2)
        return wave

    if state_limit is None:
        state_limit = np.ones(ORDER)
    peak, unstable = batch_simulate(
        mode, A, b_u, b_v, C, d1, state_limit, wave_fn, amps, N_XMAX, rng)
    return np.max(peak, axis=1), bool(unstable.any())


def design_variant(mode, osr, obg, rng):
    ntf = synthesizeNTF(order=ORDER, osr=osr, opt=1, H_inf=obg)
    a, g, b, c = realize_ntf_crfb_deterministic(ntf)
    ABCD = stuff_abcd_crfb(a, g, b, c)
    A, b_u, b_v, C, d1 = split_abcd(ABCD)

    design_state_limit = np.ones(ORDER)
    limit, detail = stable_input_limit(
        mode, A, b_u, b_v, C, d1, osr, rng, design_state_limit)
    input_peak = SAFETY * limit

    # Per-state maxima at the operating peak; back the peak off if the battery
    # somehow destabilizes inside the sweep-derived limit.
    while True:
        xmax, blew_up = measure_state_maxima(
            mode, A, b_u, b_v, C, d1, osr, input_peak, rng, design_state_limit)
        if not blew_up:
            break
        input_peak *= 0.9
        if input_peak < MIN_USABLE_INPUT_PEAK:
            raise RuntimeError(f"OSR={osr} OBG={obg}: no usable stable input range")

    # Diagonal similarity transform: x_scaled = T^-1 x with T = diag(xmax).
    # The quantizer input y (and therefore the bitstream) is unchanged.
    T = np.diag(xmax)
    T_inv = np.diag(1.0 / xmax)
    A_s = T_inv @ A @ T
    b_u_s = T_inv @ b_u
    b_v_s = T_inv @ b_v
    C_s = C @ T

    scaled_limit_guess = np.full(ORDER, LIMIT_MARGIN)
    # Re-measure with fresh noise seeds to set saturation limits from data,
    # not from the assumption that scaling worked. Back off the peak after
    # scaling instead of throwing away otherwise useful variants.
    while True:
        xmax_post, blew_up = measure_state_maxima(
            mode, A_s, b_u_s, b_v_s, C_s, d1, osr, input_peak, rng,
            scaled_limit_guess, seed_offset=77)
        if not blew_up and np.max(xmax_post) <= 1.3:
            break
        input_peak *= 0.85
        if input_peak < MIN_USABLE_INPUT_PEAK:
            if blew_up:
                raise RuntimeError(
                    f"OSR={osr} OBG={obg}: scaled system unstable at input_peak")
            raise RuntimeError(
                f"OSR={osr} OBG={obg}: scaling failed, post-scale max {np.max(xmax_post):.3f}")
    state_limit = LIMIT_MARGIN * np.maximum(xmax_post, 0.5)

    while True:
        xmax_post_final, blew_up = measure_state_maxima(
            mode, A_s, b_u_s, b_v_s, C_s, d1, osr, input_peak, rng,
            state_limit, seed_offset=177)
        if not blew_up:
            break
        input_peak *= 0.85
        if input_peak < MIN_USABLE_INPUT_PEAK:
            raise RuntimeError(
                f"OSR={osr} OBG={obg}: final limited system unstable at input_peak")
    state_limit = LIMIT_MARGIN * np.maximum(xmax_post_final, 0.5)

    # NTF figures of merit for the stderr report.
    z, p, kk = ntf[0], ntf[1], ntf[2]
    w_in = np.linspace(0, math.pi / osr, 4096)
    w_full = np.linspace(0, math.pi, 16384)
    _, h_in = _sps.freqz_zpk(z, p, kk, worN=w_in)
    _, h_full = _sps.freqz_zpk(z, p, kk, worN=w_full)
    inband_db = 10.0 * math.log10(float(np.mean(np.abs(h_in) ** 2)) + 1e-300)
    hinf_achieved = float(np.max(np.abs(h_full)))

    report = {
        "osr": osr,
        "obg": obg,
        "hinf_achieved": hinf_achieved,
        "inband_db": inband_db,
        "stable_limit": limit,
        "stable_detail": detail,
        "input_peak": input_peak,
        "xmax_prescale": xmax,
        "xmax_postscale": xmax_post_final,
    }
    return {
        "a": A_s,
        "b": np.column_stack([b_u_s, b_v_s]),
        "c": C_s,
        "d1": d1,
        "state_limit": state_limit,
        "input_peak": input_peak,
        "osr": osr,
        "obg": obg,
        "mode": mode,
    }, report


def fmt_row(row) -> str:
    return ", ".join(f"{v:.17e}" for v in row)


def variant_name(mode, osr, obg):
    del mode
    suffix = f"OSR{osr}_OBG{int(round(obg * 100)):03d}"
    return f"CRFB_{suffix}"


def choose_default(generated, mode, osr, preferences):
    for obg in preferences:
        default = generated.get((mode, osr, obg))
        if default is not None:
            return default
    tried = ", ".join(f"{obg:.2f}" for obg in preferences)
    raise RuntimeError(f"No generated default for {mode} OSR={osr}; tried {tried}")


def emit_variant(coeffs):
    mode = coeffs["mode"]
    name = variant_name(mode, coeffs["osr"], coeffs["obg"])
    out = [
        f"/// 7th-order CRFB, OSR={coeffs['osr']}, out-of-band gain {coeffs['obg']:.2f}, "
        f"input peak {coeffs['input_peak']:.3f}.",
        f"pub const {name}: ModulatorCoeffs = ModulatorCoeffs {{",
        "    a: [",
    ]
    for row in coeffs["a"]:
        out.append(f"        [{fmt_row(row)}],")
    out.extend(["    ],", "    b: ["])
    for row in coeffs["b"]:
        out.append(f"        [{fmt_row(row)}],")
    out.append("    ],")
    out.extend([
        f"    c: [{fmt_row(coeffs['c'])}],",
        f"    d1: {coeffs['d1']:.17e},",
        f"    state_limit: [{fmt_row(coeffs['state_limit'])}],",
        f"    input_peak: {coeffs['input_peak']:.17e},",
        f"    osr: {coeffs['osr']},",
        f"    obg: {coeffs['obg']:.17e},",
        "};",
    ])
    return out


def emit_rust(variants):
    out = []
    out.append("// Generated by tools/gen_crfb.py -- DO NOT EDIT BY HAND.")
    out.append("// Source: python-deltasigma synthesizeNTF(opt=1, H_inf=OBG) + realizeNTF +")
    out.append("// stuffABCD (form='CRFB', order=7), dynamic-range scaled by simulation, with")
    out.append("// measured stable input peaks and per-state saturation limits baked in.")
    out.append("")
    out.append("// Generated tables include the production plants and retained measurement variants.")
    out.append("// Preserve reproducible Python coefficient text.")
    out.append("#![allow(clippy::excessive_precision)]")
    out.append("")
    out.append("/// 7th-order modulator in ABCD state-space form.")
    out.append("///")
    out.append("/// `y = C * x + d1 * u`, `v = sign(y)`, `x' = A * x + B * [u; v]`.")
    out.append("///")
    out.append("/// States are dynamic-range scaled: at `input_peak` every integrator's")
    out.append("/// worst-case measured swing is ~1.0, so `state_limit` is a real overload")
    out.append("/// boundary rather than a guess shared by wildly different integrators.")
    out.append("#[derive(Debug, Clone, Copy)]")
    out.append("pub struct ModulatorCoeffs {")
    out.append("    pub a: [[f64; 7]; 7],")
    out.append("    pub b: [[f64; 2]; 7],")
    out.append("    pub c: [f64; 7],")
    out.append("    pub d1: f64,")
    out.append("    /// Per-state saturation bound: 1.5x the measured worst-case swing at")
    out.append("    /// `input_peak` (post-scaling, so all entries sit near 1.5).")
    out.append("    pub state_limit: [f64; 7],")
    out.append("    /// Measured stable modulator input amplitude (with 0.95 safety margin).")
    out.append("    /// PCM full scale should be mapped to this peak by the caller.")
    out.append("    pub input_peak: f64,")
    out.append("    /// Oversampling ratio the NTF was designed for.")
    out.append("    pub osr: u32,")
    out.append("    /// Out-of-band gain (`H_inf`) requested from synthesizeNTF.")
    out.append("    pub obg: f64,")
    out.append("}")
    out.append("")
    out.append("pub const CALIBRATED: bool = true;")
    out.append("")
    generated = {}
    for coeffs, _ in variants:
        mode = coeffs["mode"]
        name = variant_name(mode, coeffs["osr"], coeffs["obg"])
        generated[(mode, coeffs["osr"], coeffs["obg"])] = name
        out.extend(emit_variant(coeffs))
        out.append("")
    for osr in TARGET_OSRS:
        standard_default = choose_default(
            generated, "standard", osr, [STANDARD_DEFAULT_OBG[osr]])
        out.append(f"/// Standard hard-sign CRFB baseline for OSR={osr}.")
        out.append(f"pub const CRFB7_STANDARD_OSR{osr}: ModulatorCoeffs = {standard_default};")
        out.append("")
    return "\n".join(out)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", default=None,
                        help="write the generated Rust here (default: stdout)")
    parser.add_argument(
        "--single", nargs=3, metavar=("MODE", "OSR", "OBG"),
        help="generate one isolated variant without perturbing the historical bulk job",
    )
    parser.add_argument(
        "--seed", type=lambda value: int(value, 0), default=0xD5D,
        help="deterministic RNG seed for --single (default: 0xD5D)",
    )
    parser.add_argument(
        "--report", default=None,
        help="write a JSON synthesis/calibration report for --single",
    )
    args = parser.parse_args()

    if args.single:
        mode, osr_text, obg_text = args.single
        if mode != "standard":
            parser.error("--single MODE must be standard")
        osr = int(osr_text)
        obg = float(obg_text)
        rng = np.random.default_rng(args.seed)
        coeffs, report = design_variant(mode, osr, obg, rng)
        code = "\n".join([
            "// Isolated variant generated by tools/gen_crfb.py --single; do not hand edit.",
            f"// generator_seed=0x{args.seed:x}",
            *emit_variant(coeffs),
            "",
        ])
        if args.out:
            with open(args.out, "w") as fh:
                fh.write(code)
            print(f"wrote {args.out}", file=sys.stderr)
        else:
            print(code)
        if args.report:
            serializable_report = {
                "schema": "fozmo-crfb-single-generation-v1",
                "generator_seed": args.seed,
                "mode": mode,
                "osr": osr,
                "obg": obg,
                "report": {
                    key: value.tolist() if isinstance(value, np.ndarray) else value
                    for key, value in report.items()
                },
            }
            with open(args.report, "w") as fh:
                json.dump(serializable_report, fh, indent=2, sort_keys=True)
                fh.write("\n")
            print(f"wrote {args.report}", file=sys.stderr)
        return
    if args.report:
        parser.error("--report requires --single")

    rng = np.random.default_rng(0xD5D)
    variants = []
    skipped = []
    print("mode      osr  obg   Hinf  inband(dB)  stableDC  stableSine  input_peak  notes",
          file=sys.stderr)
    jobs = []
    for osr in LOW_RATE_STANDARD_OSRS:
        jobs.extend(("standard", osr, obg) for obg in LOW_RATE_STANDARD_OBG_FAMILY[osr])
    for osr in HIGH_RATE_STANDARD_OSRS:
        jobs.extend(("standard", osr, obg) for obg in HIGH_RATE_STANDARD_OBG_FAMILY[osr])

    for mode, osr, obg in jobs:
        try:
            # High-rate candidates use isolated RNG streams so their checked-in
            # text is reproducible with the matching `--single` invocation.
            job_rng = (
                np.random.default_rng(0xD5D)
                if osr in HIGH_RATE_STANDARD_OSRS
                else rng
            )
            coeffs, report = design_variant(mode, osr, obg, job_rng)
        except Exception as exc:
            skipped.append((mode, osr, obg, str(exc)))
            print(f"{mode:<8}  {osr:>3}  {obg:.2f}  skipped: {exc}", file=sys.stderr)
            continue
        variants.append((coeffs, report))
        d = report["stable_detail"]
        sine_limit = min(d["sine_lo"], d["sine_hi"])
        print(
            f"{mode:<8}  {osr:>3}  {obg:.2f}  {report['hinf_achieved']:.3f}  "
            f"{report['inband_db']:>9.1f}  {d['dc']:>8.2f}  {sine_limit:>10.2f}  "
            f"{report['input_peak']:>10.3f}  "
            f"xmax_pre={np.array2string(report['xmax_prescale'], precision=2)}",
            file=sys.stderr)
    if skipped:
        print("skipped unusable variants:", file=sys.stderr)
        for mode, osr, obg, reason in skipped:
            print(f"  {mode} OSR={osr} OBG={obg:.2f}: {reason}", file=sys.stderr)

    code = emit_rust(variants)
    if args.out:
        with open(args.out, "w") as fh:
            fh.write(code)
        print(f"wrote {args.out}", file=sys.stderr)
    else:
        print(code)


if __name__ == "__main__":
    main()
