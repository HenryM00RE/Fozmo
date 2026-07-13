#!/usr/bin/env python3
"""Generate, fit, and verify the fixed DSD64 EcBeam2 error profiles.

The runtime profiles are sixth-order (three-biquad) stable elliptic spectral
factors.  Reconstruction and ultrasonic factors are fitted independently in
linear *power* against the quality harness' 24--32 kHz cosine transition.  The
two fits converge on the same poles and complementary numerators, providing a
useful independent check that their power responses partition unity.

The checked-in SOS values, deterministic constrained refitter, independent
cepstral spectral-factor oracle, and report/Rust emitter use only the Python
standard library. Pass ``--rust`` to emit the constants used by
``beam_error_profile.rs`` or ``--refit`` to rerun a bounded coordinate fit
from the frozen seed. Runtime realizations project boundary spectral zeros a
fixed distance inside the unit circle, making every emitted profile strictly
stable and minimum phase without materially changing the fitted power curve.
"""

from __future__ import annotations

import argparse
import cmath
import math
from dataclasses import dataclass
from typing import Iterable, Sequence


ORDER = 6
STATE_COUNT = 6
WIRE_RATES = (2_822_400, 3_072_000)
FIT_MAX_HZ = 64_000.0
FIT_POINTS = 8192
REPORT_POINTS = 8192
REFIT_POINTS = 1024
STRICT_ZERO_RADIUS = 1.0 - 1.0e-9
SPECTRAL_ORACLE_SIZE = 4096


Matrix = list[list[float]]
Vector = list[float]
Sos = tuple[tuple[float, float, float, float, float], ...]


# Each row is (b0, b1, b2, a1, a2); a0 is normalised to one.  These are the
# frozen seed for `--refit` (seed 123). Exact DC/Nyquist boundary zeros are
# projected deterministically to `STRICT_ZERO_RADIUS` before fitting,
# verification, state-space conversion, or Rust emission.
FITTED_SOS: dict[int, dict[str, Sos]] = {
    2_822_400: {
        "reconstruction": (
            (
                5.61278362790229145e-03,
                -1.08282462817275386e-02,
                5.61278362790228972e-03,
                -1.91063651971834347e00,
                9.13150880993971459e-01,
            ),
            (
                1.0,
                -1.98919004536526867e00,
                1.00000000000000044e00,
                -1.95118381064717705e00,
                9.54317484274044614e-01,
            ),
            (
                1.0,
                -1.99353282852963387e00,
                9.99999999999999778e-01,
                -1.98379785563998090e00,
                9.87324313773209283e-01,
            ),
        ),
        "ultrasonic": (
            (
                9.27555018525684227e-01,
                -1.85490817815694276e00,
                9.27555018525684449e-01,
                -1.91063652764281922e00,
                9.13150888536684247e-01,
            ),
            (
                1.0,
                -1.99855358335489663e00,
                9.99999999999999889e-01,
                -1.95118381471113778e00,
                9.54317488186435803e-01,
            ),
            (
                1.0,
                -1.99758025442552078e00,
                9.99999999999999778e-01,
                -1.98379785679207821e00,
                9.87324314917642387e-01,
            ),
        ),
    },
    3_072_000: {
        "reconstruction": (
            (
                5.63147945118966636e-03,
                -1.09256132385085903e-02,
                5.63147945118966722e-03,
                -1.91778854463143800e00,
                9.19918595812096340e-01,
            ),
            (
                1.0,
                -1.99087349708565386e00,
                1.0,
                -1.95529934114817827e00,
                9.57949557543435359e-01,
            ),
            (
                1.0,
                -1.99454079388612704e00,
                1.00000000000000022e00,
                -1.98536977919256308e00,
                9.88348145412093482e-01,
            ),
        ),
        "ultrasonic": (
            (
                9.33239451089434846e-01,
                -1.86630747036313593e00,
                9.33239451089434624e-01,
                -1.91778854901113149e00,
                9.19918599961038308e-01,
            ),
            (
                1.0,
                -1.99877905797222621e00,
                9.99999999999999667e-01,
                -1.95529934157211582e00,
                9.57949557857762257e-01,
            ),
            (
                1.0,
                -1.99795741750142763e00,
                9.99999999999999778e-01,
                -1.98536977893971778e00,
                9.88348145144395729e-01,
            ),
        ),
    },
}


@dataclass(frozen=True)
class StateSpace:
    a: Matrix
    b: Vector
    c: Vector
    d: float
    p: Matrix
    h: Vector
    q: float


def target_amplitude(frequency_hz: float) -> float:
    if frequency_hz <= 24_000.0:
        return 1.0
    if frequency_hz >= 32_000.0:
        return 0.0
    phase = math.pi * (frequency_hz - 24_000.0) / 8_000.0
    return 0.5 + 0.5 * math.cos(phase)


def target_reconstruction_power(frequency_hz: float) -> float:
    amplitude = target_amplitude(frequency_hz)
    return amplitude * amplitude


def response_power(sos: Sos, frequency_hz: float, wire_rate: int) -> float:
    phase = 2.0 * math.pi * frequency_hz / wire_rate
    z1 = cmath.exp(complex(0.0, -phase))
    z2 = z1 * z1
    response = complex(1.0, 0.0)
    for b0, b1, b2, a1, a2 in sos:
        response *= (b0 + b1 * z1 + b2 * z2) / (1.0 + a1 * z1 + a2 * z2)
    return response.real * response.real + response.imag * response.imag


def target_power(kind: str, frequency_hz: float) -> float:
    reconstruction = target_reconstruction_power(frequency_hz)
    if kind == "reconstruction":
        return reconstruction
    if kind == "ultrasonic":
        return 1.0 - reconstruction
    raise ValueError(f"unknown profile kind: {kind}")


def quadratic_roots(a: float, b: float, c: float) -> tuple[complex, complex]:
    if a == 0.0:
        raise ArithmeticError("SOS leading coefficient must be nonzero")
    discriminant = cmath.sqrt(b * b - 4.0 * a * c)
    return ((-b + discriminant) / (2.0 * a), (-b - discriminant) / (2.0 * a))


def strict_minimum_phase_sos(sos: Sos) -> Sos:
    """Project only boundary numerator roots into the open unit disk.

    Pole locations are never altered here: unstable seed rows are rejected.
    The tiny fixed projection preserves the intended DC/Nyquist nulls to
    numerical precision while satisfying the strict minimum-phase contract.
    """

    projected = []
    for b0, b1, b2, a1, a2 in sos:
        poles = quadratic_roots(1.0, a1, a2)
        if max(map(abs, poles)) >= 1.0:
            raise ArithmeticError("profile seed contains an unstable SOS pole")
        zeros = list(quadratic_roots(b0, b1, b2))
        for index, zero in enumerate(zeros):
            radius = abs(zero)
            if radius >= STRICT_ZERO_RADIUS:
                zeros[index] = zero / radius * STRICT_ZERO_RADIUS
        projected_b1 = (-b0 * (zeros[0] + zeros[1])).real
        projected_b2 = (b0 * zeros[0] * zeros[1]).real
        projected.append((b0, projected_b1, projected_b2, a1, a2))
    return tuple(projected)


def maximum_root_radius(sos: Sos, numerator: bool) -> float:
    maximum = 0.0
    for b0, b1, b2, a1, a2 in sos:
        roots = (
            quadratic_roots(b0, b1, b2)
            if numerator
            else quadratic_roots(1.0, a1, a2)
        )
        maximum = max(maximum, *(abs(root) for root in roots))
    return maximum


def fit_cost(kind: str, wire_rate: int, sos: Sos, points: int = FIT_POINTS) -> float:
    error = 0.0
    for index in range(points + 1):
        frequency = FIT_MAX_HZ * index / points
        delta = response_power(sos, frequency, wire_rate) - target_power(kind, frequency)
        error += delta * delta
    return error / (points + 1)


def valid_constrained_sos(sos: Sos) -> bool:
    return all(
        all(math.isfinite(value) for value in row)
        and row[0] > 0.0
        and max(map(abs, quadratic_roots(1.0, row[3], row[4]))) < 1.0
        and max(map(abs, quadratic_roots(row[0], row[1], row[2]))) < 1.0
        for row in sos
    )


def deterministic_refit(kind: str, wire_rate: int, seed: Sos) -> Sos:
    """Small, reproducible stability/minimum-phase constrained SOS fit.

    The frozen fit is already close to its optimum, so a bounded coordinate
    search is preferable to a dependency-heavy optimizer here. Every proposal
    is rejected unless all poles and zeros remain strictly inside the unit
    circle; accepted moves must reduce linear power error.
    """

    current = [list(row) for row in strict_minimum_phase_sos(seed)]
    best = fit_cost(kind, wire_rate, tuple(tuple(row) for row in current), REFIT_POINTS)
    for sweep in range(5):
        scale = 0.5**sweep
        for section in range(len(current)):
            for coefficient in range(5):
                original = current[section][coefficient]
                base_step = max(abs(original) * 2.0e-5, 2.0e-9)
                if coefficient >= 3:
                    base_step = 2.0e-6
                for direction in (-1.0, 1.0):
                    current[section][coefficient] = original + direction * scale * base_step
                    proposal = tuple(tuple(row) for row in current)
                    if not valid_constrained_sos(proposal):
                        continue
                    cost = fit_cost(kind, wire_rate, proposal, REFIT_POINTS)
                    if cost < best:
                        best = cost
                        original = current[section][coefficient]
                current[section][coefficient] = original
    result = tuple(tuple(row) for row in current)
    if not valid_constrained_sos(result):
        raise AssertionError("constrained refit escaped its feasible set")
    return result


def cascade_step(sos: Sos, state: Sequence[float], value: float) -> tuple[Vector, float]:
    next_state: Vector = []
    for section, (b0, b1, b2, a1, a2) in enumerate(sos):
        s1 = state[2 * section]
        s2 = state[2 * section + 1]
        output = b0 * value + s1
        next_state.extend((b1 * value - a1 * output + s2, b2 * value - a2 * output))
        value = output
    return next_state, value


def cascade_state_space(sos: Sos) -> tuple[Matrix, Vector, Vector, float]:
    zero = [0.0] * STATE_COUNT
    b, d = cascade_step(sos, zero, 1.0)
    a = [[0.0] * STATE_COUNT for _ in range(STATE_COUNT)]
    c: Vector = []
    for column in range(STATE_COUNT):
        basis = [0.0] * STATE_COUNT
        basis[column] = 1.0
        next_state, output = cascade_step(sos, basis, 0.0)
        c.append(output)
        for row in range(STATE_COUNT):
            a[row][column] = next_state[row]
    return a, b, c, d


def solve(matrix: Matrix, rhs: Vector) -> Vector:
    size = len(rhs)
    augmented = [matrix[row][:] + [rhs[row]] for row in range(size)]
    for column in range(size):
        pivot = max(range(column, size), key=lambda row: abs(augmented[row][column]))
        augmented[column], augmented[pivot] = augmented[pivot], augmented[column]
        divisor = augmented[column][column]
        if abs(divisor) < 1.0e-30:
            raise ArithmeticError("singular generator matrix")
        for row in range(column + 1, size):
            factor = augmented[row][column] / divisor
            if factor == 0.0:
                continue
            for index in range(column, size + 1):
                augmented[row][index] -= factor * augmented[column][index]
    result = [0.0] * size
    for row in range(size - 1, -1, -1):
        remainder = augmented[row][size] - sum(
            augmented[row][column] * result[column] for column in range(row + 1, size)
        )
        result[row] = remainder / augmented[row][row]
    return result


def observability_gramian(a: Matrix, c: Vector) -> Matrix:
    # Solve P - A^T P A = C^T C as a 36-variable linear system.
    equation: Matrix = []
    rhs: Vector = []
    for i in range(STATE_COUNT):
        for j in range(STATE_COUNT):
            row: Vector = []
            for k in range(STATE_COUNT):
                for ell in range(STATE_COUNT):
                    identity = 1.0 if (k == i and ell == j) else 0.0
                    row.append(identity - a[k][i] * a[ell][j])
            equation.append(row)
            rhs.append(c[i] * c[j])
    flat = solve(equation, rhs)
    return [
        [0.5 * (flat[i * STATE_COUNT + j] + flat[j * STATE_COUNT + i]) for j in range(STATE_COUNT)]
        for i in range(STATE_COUNT)
    ]


def cholesky_lower(matrix: Matrix) -> Matrix:
    lower = [[0.0] * STATE_COUNT for _ in range(STATE_COUNT)]
    for row in range(STATE_COUNT):
        for column in range(row + 1):
            value = matrix[row][column] - sum(
                lower[row][index] * lower[column][index] for index in range(column)
            )
            if row == column:
                if value <= 0.0:
                    raise ArithmeticError("profile Gramian is not positive definite")
                lower[row][column] = math.sqrt(value)
            else:
                lower[row][column] = value / lower[column][column]
    return lower


def transpose(matrix: Matrix) -> Matrix:
    return [list(column) for column in zip(*matrix)]


def matmul(left: Matrix, right: Matrix) -> Matrix:
    return [
        [sum(left[row][k] * right[k][column] for k in range(STATE_COUNT)) for column in range(STATE_COUNT)]
        for row in range(STATE_COUNT)
    ]


def matvec(matrix: Matrix, vector: Vector) -> Vector:
    return [sum(value * vector[column] for column, value in enumerate(row)) for row in matrix]


def inverse(matrix: Matrix) -> Matrix:
    columns = []
    for column in range(STATE_COUNT):
        basis = [0.0] * STATE_COUNT
        basis[column] = 1.0
        columns.append(solve([row[:] for row in matrix], basis))
    return transpose(columns)


def dot(left: Sequence[float], right: Sequence[float]) -> float:
    return sum(a * b for a, b in zip(left, right))


def balanced_state_space(sos: Sos) -> StateSpace:
    a, b, c, d = cascade_state_space(sos)
    gramian = observability_gramian(a, c)

    # With P = L L^T and R = L^T, y = R x makes the runtime tail value y^T y.
    transform = transpose(cholesky_lower(gramian))
    inverse_transform = inverse(transform)
    balanced_a = matmul(matmul(transform, a), inverse_transform)
    balanced_b = matvec(transform, b)
    balanced_c = [dot(c, [inverse_transform[row][column] for row in range(STATE_COUNT)]) for column in range(STATE_COUNT)]
    identity = [[1.0 if row == column else 0.0 for column in range(STATE_COUNT)] for row in range(STATE_COUNT)]
    h = [
        sum(balanced_a[row][column] * balanced_b[row] for row in range(STATE_COUNT))
        + balanced_c[column] * d
        for column in range(STATE_COUNT)
    ]
    q = d * d + dot(balanced_b, balanced_b)
    return StateSpace(balanced_a, balanced_b, balanced_c, d, identity, h, q)


def max_lyapunov_residual(profile: StateSpace) -> float:
    maximum = 0.0
    for i in range(STATE_COUNT):
        for j in range(STATE_COUNT):
            reconstructed = profile.c[i] * profile.c[j] + sum(
                profile.a[row][i] * profile.a[row][j] for row in range(STATE_COUNT)
            )
            maximum = max(maximum, abs(profile.p[i][j] - reconstructed))
    return maximum


def impulse_energy(profile: StateSpace, samples: int = 200_000) -> tuple[float, float]:
    state = [0.0] * STATE_COUNT
    energy = 0.0
    value = 1.0
    for _ in range(samples):
        output = dot(profile.c, state) + profile.d * value
        energy += output * output
        state = [
            dot(profile.a[row], state) + profile.b[row] * value
            for row in range(STATE_COUNT)
        ]
        value = 0.0
        if max(abs(item) for item in state) < 1.0e-15:
            break
    return energy, dot(state, state)


def matrix_rank(matrix: Matrix, tolerance: float = 1.0e-10) -> int:
    work = [row[:] for row in matrix]
    rows = len(work)
    columns = len(work[0]) if rows else 0
    rank = 0
    for column in range(columns):
        pivot = max(range(rank, rows), key=lambda row: abs(work[row][column]), default=rank)
        if rank >= rows or abs(work[pivot][column]) <= tolerance:
            continue
        work[rank], work[pivot] = work[pivot], work[rank]
        divisor = work[rank][column]
        for index in range(column, columns):
            work[rank][index] /= divisor
        for row in range(rows):
            if row == rank:
                continue
            factor = work[row][column]
            for index in range(column, columns):
                work[row][index] -= factor * work[rank][index]
        rank += 1
        if rank == rows:
            break
    return rank


def matrix_one_norm(matrix: Matrix) -> float:
    return max(sum(abs(matrix[row][column]) for row in range(len(matrix))) for column in range(len(matrix[0])))


def matrix_condition_one(matrix: Matrix) -> float:
    return matrix_one_norm(matrix) * matrix_one_norm(inverse(matrix))


def controllability_matrix(profile: StateSpace) -> Matrix:
    columns = []
    vector = profile.b[:]
    for _ in range(STATE_COUNT):
        columns.append(vector)
        vector = matvec(profile.a, vector)
    return transpose(columns)


def observability_matrix(profile: StateSpace) -> Matrix:
    rows = []
    vector = profile.c[:]
    for _ in range(STATE_COUNT):
        rows.append(vector)
        vector = [
            sum(vector[row] * profile.a[row][column] for row in range(STATE_COUNT))
            for column in range(STATE_COUNT)
        ]
    return rows


def sos_impulse(sos: Sos, samples: int) -> Vector:
    state = [0.0] * STATE_COUNT
    result = []
    for sample in range(samples):
        state, output = cascade_step(sos, state, 1.0 if sample == 0 else 0.0)
        result.append(output)
    return result


def state_space_impulse(profile: StateSpace, samples: int) -> Vector:
    state = [0.0] * STATE_COUNT
    result = []
    for sample in range(samples):
        value = 1.0 if sample == 0 else 0.0
        result.append(dot(profile.c, state) + profile.d * value)
        state = [dot(profile.a[row], state) + profile.b[row] * value for row in range(STATE_COUNT)]
    return result


def realization_agreement(sos: Sos, profile: StateSpace) -> tuple[float, float]:
    sos_response = sos_impulse(sos, 4096)
    state_response = state_space_impulse(profile, 4096)
    impulse_error = max(abs(left - right) for left, right in zip(sos_response, state_response))

    signal = [
        0.53 * math.sin(index * 0.17320508075688773)
        + 0.17 * math.cos(index * 0.06180339887498949)
        for index in range(384)
    ]
    cascade_state = [0.0] * STATE_COUNT
    profile_state = [0.0] * STATE_COUNT
    convolution_error = 0.0
    for sample, value in enumerate(signal):
        cascade_state, cascade_output = cascade_step(sos, cascade_state, value)
        profile_output = dot(profile.c, profile_state) + profile.d * value
        profile_state = [
            dot(profile.a[row], profile_state) + profile.b[row] * value
            for row in range(STATE_COUNT)
        ]
        convolution_output = sum(
            sos_response[lag] * signal[sample - lag] for lag in range(sample + 1)
        )
        convolution_error = max(
            convolution_error,
            abs(cascade_output - profile_output),
            abs(cascade_output - convolution_output),
        )
    return impulse_error, convolution_error


def fft(values: Sequence[complex], inverse_transform: bool = False) -> list[complex]:
    """Dependency-free radix-2 FFT used only by the spectral-factor oracle."""

    size = len(values)
    if size == 0 or size & (size - 1):
        raise ValueError("FFT size must be a nonzero power of two")
    result = [complex(value) for value in values]
    target = 0
    for source in range(1, size):
        bit = size >> 1
        while target & bit:
            target ^= bit
            bit >>= 1
        target ^= bit
        if source < target:
            result[source], result[target] = result[target], result[source]
    length = 2
    sign = 1.0 if inverse_transform else -1.0
    while length <= size:
        root = cmath.exp(complex(0.0, sign * 2.0 * math.pi / length))
        for start in range(0, size, length):
            twiddle = 1.0 + 0.0j
            half = length // 2
            for offset in range(half):
                even = result[start + offset]
                odd = result[start + offset + half] * twiddle
                result[start + offset] = even + odd
                result[start + offset + half] = even - odd
                twiddle *= root
        length *= 2
    if inverse_transform:
        result = [value / size for value in result]
    return result


def cepstral_spectral_factor_power(kind: str, wire_rate: int) -> Vector:
    """Independent minimum-phase factor of a sampled nonnegative power curve."""

    size = SPECTRAL_ORACLE_SIZE
    floor = 1.0e-14
    target = [
        max(
            target_power(kind, min(index, size - index) * wire_rate / size),
            floor,
        )
        for index in range(size)
    ]
    log_magnitude = [complex(0.5 * math.log(value), 0.0) for value in target]
    cepstrum = fft(log_magnitude, inverse_transform=True)
    causal = [0.0j] * size
    causal[0] = cepstrum[0]
    causal[size // 2] = cepstrum[size // 2]
    for index in range(1, size // 2):
        causal[index] = 2.0 * cepstrum[index]
    log_factor = fft(causal)
    factor = [cmath.exp(value) for value in log_factor]
    return [value.real * value.real + value.imag * value.imag for value in factor]


def representative_spectrum_objective_error(kind: str, wire_rate: int, sos: Sos) -> float:
    spectra = (
        ((1_000.0, 1.0), (12_000.0, 0.4), (19_000.0, 0.2)),
        ((23_500.0, 0.3), (26_000.0, 1.0), (30_500.0, 0.6)),
        ((18_000.0, 0.2), (40_000.0, 1.0), (96_000.0, 0.5)),
    )
    maximum = 0.0
    for spectrum in spectra:
        expected = sum(weight * target_power(kind, frequency) for frequency, weight in spectrum)
        observed = sum(weight * response_power(sos, frequency, wire_rate) for frequency, weight in spectrum)
        scale = sum(weight for _, weight in spectrum)
        maximum = max(maximum, abs(observed - expected) / scale)
    return maximum


def format_vector(values: Iterable[float], indent: str) -> str:
    return "[\n" + "".join(f"{indent}{value:.17e},\n" for value in values) + indent[:-4] + "]"


def format_matrix(matrix: Matrix, indent: str) -> str:
    rows = []
    for row in matrix:
        rows.append(indent + "[" + ", ".join(f"{value:.17e}" for value in row) + "],\n")
    return "[\n" + "".join(rows) + indent[:-4] + "]"


def emit_profile(name: str, profile: StateSpace) -> None:
    print(f"const {name}: BeamErrorProfile = BeamErrorProfile {{")
    print(f"    a: {format_matrix(profile.a, '        ')},")
    print(f"    b: {format_vector(profile.b, '        ')},")
    print(f"    c: {format_vector(profile.c, '        ')},")
    print(f"    d: {profile.d:.17e},")
    print("    p: IDENTITY_PROFILE_GRAMIAN,")
    print(f"    h: {format_vector(profile.h, '        ')},")
    print(f"    q: {profile.q:.17e},")
    print("};")


def profiles(wire_rate: int) -> tuple[Sos, Sos, StateSpace, StateSpace]:
    reconstruction_sos = strict_minimum_phase_sos(
        FITTED_SOS[wire_rate]["reconstruction"]
    )
    ultrasonic_sos = strict_minimum_phase_sos(FITTED_SOS[wire_rate]["ultrasonic"])
    return (
        reconstruction_sos,
        ultrasonic_sos,
        balanced_state_space(reconstruction_sos),
        balanced_state_space(ultrasonic_sos),
    )


def report() -> None:
    for wire_rate in WIRE_RATES:
        reconstruction_sos, ultrasonic_sos, reconstruction, ultrasonic = profiles(wire_rate)
        maximum_partition_error = 0.0
        squared_partition_error = 0.0
        transition_partition_max = 0.0
        transition_partition_squared = 0.0
        transition_partition_count = 0
        maximum_reconstruction_error = 0.0
        maximum_ultrasonic_error = 0.0
        squared_reconstruction_error = 0.0
        squared_ultrasonic_error = 0.0
        band_errors = {
            kind: {
                band: [0.0, 0.0, 0]
                for band in ("passband", "transition", "stopband")
            }
            for kind in ("reconstruction", "ultrasonic")
        }
        for index in range(REPORT_POINTS + 1):
            frequency = 0.5 * wire_rate * index / REPORT_POINTS
            low = response_power(reconstruction_sos, frequency, wire_rate)
            high = response_power(ultrasonic_sos, frequency, wire_rate)
            partition_error = abs(low + high - 1.0)
            reconstruction_error = abs(low - target_power("reconstruction", frequency))
            ultrasonic_error = abs(high - target_power("ultrasonic", frequency))
            maximum_partition_error = max(maximum_partition_error, partition_error)
            squared_partition_error += partition_error * partition_error
            maximum_reconstruction_error = max(
                maximum_reconstruction_error, reconstruction_error
            )
            maximum_ultrasonic_error = max(maximum_ultrasonic_error, ultrasonic_error)
            squared_reconstruction_error += reconstruction_error * reconstruction_error
            squared_ultrasonic_error += ultrasonic_error * ultrasonic_error
            band = (
                "passband"
                if frequency <= 24_000.0
                else "transition"
                if frequency < 32_000.0
                else "stopband"
            )
            for kind, error in (
                ("reconstruction", reconstruction_error),
                ("ultrasonic", ultrasonic_error),
            ):
                stats = band_errors[kind][band]
                stats[0] = max(stats[0], error)
                stats[1] += error * error
                stats[2] += 1
            if band == "transition":
                transition_partition_max = max(transition_partition_max, partition_error)
                transition_partition_squared += partition_error * partition_error
                transition_partition_count += 1
        reconstruction_energy, reconstruction_tail = impulse_energy(reconstruction)
        ultrasonic_energy, ultrasonic_tail = impulse_energy(ultrasonic)
        reconstruction_controllability = controllability_matrix(reconstruction)
        reconstruction_observability = observability_matrix(reconstruction)
        ultrasonic_controllability = controllability_matrix(ultrasonic)
        ultrasonic_observability = observability_matrix(ultrasonic)
        reconstruction_agreement = realization_agreement(reconstruction_sos, reconstruction)
        ultrasonic_agreement = realization_agreement(ultrasonic_sos, ultrasonic)
        low_impulse = sos_impulse(reconstruction_sos, 16_384)
        high_impulse = sos_impulse(ultrasonic_sos, 16_384)
        autocorrelation_error = 0.0
        for lag in range(129):
            correlation = sum(
                low_impulse[index] * low_impulse[index + lag]
                + high_impulse[index] * high_impulse[index + lag]
                for index in range(len(low_impulse) - lag)
            )
            expected = 1.0 if lag == 0 else 0.0
            autocorrelation_error = max(autocorrelation_error, abs(correlation - expected))
        reconstruction_oracle = cepstral_spectral_factor_power("reconstruction", wire_rate)
        ultrasonic_oracle = cepstral_spectral_factor_power("ultrasonic", wire_rate)
        reconstruction_oracle_error = 0.0
        ultrasonic_oracle_error = 0.0
        for index in range(SPECTRAL_ORACLE_SIZE // 2 + 1):
            frequency = index * wire_rate / SPECTRAL_ORACLE_SIZE
            reconstruction_oracle_error = max(
                reconstruction_oracle_error,
                abs(
                    response_power(reconstruction_sos, frequency, wire_rate)
                    - reconstruction_oracle[index]
                ),
            )
            ultrasonic_oracle_error = max(
                ultrasonic_oracle_error,
                abs(
                    response_power(ultrasonic_sos, frequency, wire_rate)
                    - ultrasonic_oracle[index]
                ),
            )
        print(f"wire_rate={wire_rate}")
        print(
            "  reconstruction_fit_mse="
            f"{fit_cost('reconstruction', wire_rate, reconstruction_sos):.9e} "
            f"max={maximum_reconstruction_error:.9e} "
            f"rms={math.sqrt(squared_reconstruction_error / (REPORT_POINTS + 1)):.9e}"
        )
        print(
            "  ultrasonic_fit_mse="
            f"{fit_cost('ultrasonic', wire_rate, ultrasonic_sos):.9e} "
            f"max={maximum_ultrasonic_error:.9e} "
            f"rms={math.sqrt(squared_ultrasonic_error / (REPORT_POINTS + 1)):.9e}"
        )
        print(
            "  complementary_power_max="
            f"{maximum_partition_error:.9e} rms={math.sqrt(squared_partition_error / (REPORT_POINTS + 1)):.9e} "
            f"transition_max={transition_partition_max:.9e} "
            f"transition_rms={math.sqrt(transition_partition_squared / transition_partition_count):.9e}"
        )
        for kind in ("reconstruction", "ultrasonic"):
            formatted = []
            for band in ("passband", "transition", "stopband"):
                maximum, squared, count = band_errors[kind][band]
                formatted.append(
                    f"{band}_max={maximum:.6e} {band}_rms={math.sqrt(squared / count):.6e}"
                )
            print(f"  {kind}_power_error_by_band " + " ".join(formatted))
        print(
            "  reconstruction_lyapunov_residual="
            f"{max_lyapunov_residual(reconstruction):.9e} impulse={reconstruction_energy:.12e} "
            f"q={reconstruction.q:.12e} tail={reconstruction_tail:.3e}"
        )
        print(
            "  ultrasonic_lyapunov_residual="
            f"{max_lyapunov_residual(ultrasonic):.9e} impulse={ultrasonic_energy:.12e} "
            f"q={ultrasonic.q:.12e} tail={ultrasonic_tail:.3e}"
        )
        print(
            "  reconstruction_realization "
            f"pole_radius={maximum_root_radius(reconstruction_sos, False):.12f} "
            f"zero_radius={maximum_root_radius(reconstruction_sos, True):.12f} "
            f"p_cond1={matrix_condition_one(reconstruction.p):.6e} "
            f"controllability_rank={matrix_rank(reconstruction_controllability)} "
            f"observability_rank={matrix_rank(reconstruction_observability)} "
            f"controllability_cond1={matrix_condition_one(reconstruction_controllability):.6e} "
            f"observability_cond1={matrix_condition_one(reconstruction_observability):.6e} "
            f"sos_state_max={reconstruction_agreement[0]:.3e} "
            f"convolution_max={reconstruction_agreement[1]:.3e}"
        )
        print(
            "  ultrasonic_realization "
            f"pole_radius={maximum_root_radius(ultrasonic_sos, False):.12f} "
            f"zero_radius={maximum_root_radius(ultrasonic_sos, True):.12f} "
            f"p_cond1={matrix_condition_one(ultrasonic.p):.6e} "
            f"controllability_rank={matrix_rank(ultrasonic_controllability)} "
            f"observability_rank={matrix_rank(ultrasonic_observability)} "
            f"controllability_cond1={matrix_condition_one(ultrasonic_controllability):.6e} "
            f"observability_cond1={matrix_condition_one(ultrasonic_observability):.6e} "
            f"sos_state_max={ultrasonic_agreement[0]:.3e} "
            f"convolution_max={ultrasonic_agreement[1]:.3e}"
        )
        print(
            "  oracle_checks "
            f"spectral_factor_reconstruction_max={reconstruction_oracle_error:.6e} "
            f"spectral_factor_ultrasonic_max={ultrasonic_oracle_error:.6e} "
            f"autocorrelation_partition_max={autocorrelation_error:.6e} "
            f"representative_reconstruction_objective_error_max="
            f"{representative_spectrum_objective_error('reconstruction', wire_rate, reconstruction_sos):.6e} "
            f"representative_ultrasonic_objective_error_max="
            f"{representative_spectrum_objective_error('ultrasonic', wire_rate, ultrasonic_sos):.6e}"
        )

        # These are generator failures, not soft report warnings: checked-in
        # runtime constants must satisfy the declared mathematical contract.
        for sos, profile, controllability, observability, agreement in (
            (
                reconstruction_sos,
                reconstruction,
                reconstruction_controllability,
                reconstruction_observability,
                reconstruction_agreement,
            ),
            (
                ultrasonic_sos,
                ultrasonic,
                ultrasonic_controllability,
                ultrasonic_observability,
                ultrasonic_agreement,
            ),
        ):
            assert maximum_root_radius(sos, False) < 1.0
            assert maximum_root_radius(sos, True) < 1.0
            assert matrix_rank(controllability) == STATE_COUNT
            assert matrix_rank(observability) == STATE_COUNT
            assert matrix_condition_one(controllability) < 1.0e12
            assert matrix_condition_one(observability) < 1.0e12
            assert matrix_condition_one(profile.p) <= 1.0 + 1.0e-12
            assert max_lyapunov_residual(profile) < 1.0e-8
            assert max(agreement) < 1.0e-9
        assert maximum_partition_error < 1.0e-5
        assert autocorrelation_error < 1.0e-5


def emit_rust() -> None:
    print("// Generated by tools/gen_beam_error_profiles.py --rust; do not hand edit.")
    for wire_rate in WIRE_RATES:
        _, _, reconstruction, ultrasonic = profiles(wire_rate)
        print(
            f"// wire_rate={wire_rate}; sixth-order strict-minimum-phase linear-power fit"
        )
        suffix = "2822400" if wire_rate == 2_822_400 else "3072000"
        emit_profile(f"RECONSTRUCTION_{suffix}", reconstruction)
        emit_profile(f"ULTRASONIC_{suffix}", ultrasonic)


def refit_report() -> None:
    for wire_rate in WIRE_RATES:
        for kind in ("reconstruction", "ultrasonic"):
            seed = strict_minimum_phase_sos(FITTED_SOS[wire_rate][kind])
            fitted = deterministic_refit(kind, wire_rate, seed)
            print(
                f"wire_rate={wire_rate} kind={kind} "
                f"seed_mse={fit_cost(kind, wire_rate, seed):.12e} "
                f"refit_mse={fit_cost(kind, wire_rate, fitted):.12e}"
            )
            for row in fitted:
                print("  (" + ", ".join(f"{value:.17e}" for value in row) + "),")


def main() -> None:
    parser = argparse.ArgumentParser()
    output = parser.add_mutually_exclusive_group()
    output.add_argument("--rust", action="store_true", help="emit checked-in Rust constants")
    output.add_argument(
        "--refit",
        action="store_true",
        help="run the deterministic stability/minimum-phase constrained SOS refit",
    )
    args = parser.parse_args()
    if args.rust:
        emit_rust()
    elif args.refit:
        refit_report()
    else:
        report()


if __name__ == "__main__":
    main()
