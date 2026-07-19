from __future__ import annotations

import json
import math
import warnings
from pathlib import Path
from typing import Any

import numpy as np
from scipy import integrate, interpolate, linalg, optimize, signal, stats

warnings.filterwarnings(
    "ignore", message=".*encountered in matmul", category=RuntimeWarning
)


def _open_uniform_knots(x_lo: float, x_hi: float, degree: int, controls: int) -> np.ndarray:
    interior_count = controls - degree - 1
    interior = np.linspace(x_lo, x_hi, interior_count + 2)[1:-1]
    return np.concatenate(
        (np.full(degree + 1, x_lo), interior, np.full(degree + 1, x_hi))
    )


def _basis_derivative(
    knots: np.ndarray, degree: int, controls: int, x: float, derivative: int
) -> np.ndarray:
    result = np.empty(controls, dtype=np.float64)
    for index in range(controls):
        coefficients = np.zeros(controls, dtype=np.float64)
        coefficients[index] = 1.0
        spline = interpolate.BSpline(knots, coefficients, degree, extrapolate=False)
        result[index] = spline.derivative(derivative)(x)
    return result


def _basis_integrals(knots: np.ndarray, degree: int, controls: int) -> np.ndarray:
    x_lo = knots[degree]
    x_hi = knots[-degree - 1]
    values = np.empty(controls, dtype=np.float64)
    for index in range(controls):
        coefficients = np.zeros(controls, dtype=np.float64)
        coefficients[index] = 1.0
        spline = interpolate.BSpline(knots, coefficients, degree, extrapolate=False)
        values[index] = integrate.quad(
            lambda x: float(spline(x)) * math.exp(x),
            x_lo,
            x_hi,
            epsabs=1.0e-13,
            epsrel=1.0e-13,
            limit=200,
        )[0]
    return values


def _constrained_least_squares(
    design: np.ndarray,
    target: np.ndarray,
    equalities: np.ndarray,
    equality_values: np.ndarray,
    ridge: float = 1.0e-12,
) -> np.ndarray:
    gram = equalities @ equalities.T
    particular = equalities.T @ np.linalg.solve(gram, equality_values)
    nullspace = linalg.null_space(equalities)
    if nullspace.shape[1] == 0:
        return particular
    reduced_design = design @ nullspace
    reduced_target = target - design @ particular
    reduced = np.linalg.lstsq(
        np.vstack((reduced_design, math.sqrt(ridge) * np.eye(nullspace.shape[1]))),
        np.concatenate((reduced_target, np.zeros(nullspace.shape[1]))),
        rcond=None,
    )[0]
    return particular + nullspace @ reduced


def design_group_delay(
    minimum_spectrum: np.ndarray,
    magnitude: np.ndarray,
    fft_len: int,
    sample_rate_hz: float,
    split_lo_hz: float,
    split_hi_hz: float,
    degree: int,
    controls: int,
    starts: int,
    seed: int,
    work_dir: Path,
    resume: bool = True,
) -> tuple[np.ndarray, np.ndarray, dict[str, Any]]:
    phase_path = work_dir / "target_residual_phase.npy"
    delay_path = work_dir / "target_group_delay.npy"
    report_path = work_dir / "group_delay.json"
    if resume and phase_path.exists() and delay_path.exists() and report_path.exists():
        return (
            np.load(phase_path, mmap_mode="r"),
            np.load(delay_path, mmap_mode="r"),
            json.loads(report_path.read_text()),
        )

    omega = np.linspace(0.0, np.pi, minimum_spectrum.size)
    minimum_phase = np.unwrap(np.angle(minimum_spectrum))
    reliable = magnitude >= 1.0e-9
    reliable_indices = np.flatnonzero(reliable)
    if reliable_indices.size < 256:
        raise RuntimeError("minimum-phase spectrum has too few reliable phase bins")
    last_reliable = int(reliable_indices[-1])
    phase_for_derivative = minimum_phase.copy()
    phase_for_derivative[last_reliable + 1 :] = minimum_phase[last_reliable]
    delta_omega = np.pi / (minimum_spectrum.size - 1)
    minimum_delay = -signal.savgol_filter(
        phase_for_derivative,
        window_length=129,
        polyorder=7,
        deriv=1,
        delta=delta_omega,
        mode="interp",
    )

    omega_lo = 2.0 * np.pi * split_lo_hz / sample_rate_hz
    omega_hi = 2.0 * np.pi * split_hi_hz / sample_rate_hz
    x_lo = math.log(omega_lo)
    x_hi = math.log(omega_hi)
    knots = _open_uniform_knots(x_lo, x_hi, degree, controls)
    fit_x = np.linspace(x_lo, x_hi, 4096)
    fit_omega = np.exp(fit_x)
    basis = interpolate.BSpline.design_matrix(fit_x, knots, degree).toarray()
    min_delay_fit = np.interp(fit_omega, omega, minimum_delay)
    # Keep the constant low band close to the minimum-phase reference's mean
    # delay. Letting tau0 drift upward makes closure numerically easy but adds
    # broadband pre-ringing and loses the V2 transient Pareto comparison.
    low_reference_delay = float(np.mean(minimum_delay[omega <= omega_lo]))
    min_slope_fit = np.gradient(min_delay_fit, fit_x)
    min_curvature_fit = np.gradient(min_slope_fit, fit_x)
    hi_delay = float(min_delay_fit[-1])
    hi_slope = float(min_slope_fit[-1])
    hi_curvature = float(min_curvature_fit[-1])

    variables = controls + 1
    endpoint_rows = []
    endpoint_values = []
    row = np.zeros(variables)
    row[:controls] = _basis_derivative(knots, degree, controls, x_lo, 0)
    row[-1] = -1.0
    endpoint_rows.append(row)
    endpoint_values.append(0.0)
    for derivative in (1, 2):
        row = np.zeros(variables)
        row[:controls] = _basis_derivative(knots, degree, controls, x_lo, derivative)
        endpoint_rows.append(row)
        endpoint_values.append(0.0)
    for derivative, value in ((0, hi_delay), (1, hi_slope), (2, hi_curvature)):
        row = np.zeros(variables)
        row[:controls] = _basis_derivative(knots, degree, controls, x_hi, derivative)
        endpoint_rows.append(row)
        endpoint_values.append(value)

    closure_row = np.zeros(variables)
    closure_row[:controls] = _basis_integrals(knots, degree, controls)
    closure_row[-1] = omega_lo
    hi_phase = float(np.interp(omega_hi, omega, minimum_phase))
    closure_value = float(minimum_phase[0] - hi_phase)
    equality_matrix = np.vstack((*endpoint_rows, closure_row))
    equality_values = np.asarray((*endpoint_values, closure_value), dtype=np.float64)

    smooth = fit_x - x_lo
    smooth /= x_hi - x_lo
    blend = smooth**4 * (35.0 + smooth * (-84.0 + smooth * (70.0 - 20.0 * smooth)))
    design = np.zeros((basis.shape[0], variables), dtype=np.float64)
    design[:, :controls] = basis
    design[:, -1] = -(1.0 - blend)
    target = blend * min_delay_fit
    initial = _constrained_least_squares(
        design, target, equality_matrix, equality_values, ridge=1.0e-9
    )
    nullspace = linalg.null_space(equality_matrix)
    third_basis = np.vstack(
        [
            _basis_derivative(knots, degree, controls, x, 3)
            for x in np.linspace(x_lo, x_hi, 512)
        ]
    )
    second_basis = np.vstack(
        [
            _basis_derivative(knots, degree, controls, x, 2)
            for x in np.linspace(x_lo, x_hi, 512)
        ]
    )

    def objective(vector: np.ndarray) -> float:
        spline_delay = basis @ vector[:controls]
        baseline = (1.0 - blend) * vector[-1] + blend * min_delay_fit
        residual = spline_delay - baseline
        second = second_basis @ vector[:controls]
        third = third_basis @ vector[:controls]
        slope = np.diff(spline_delay) / np.diff(fit_x)
        slope_tv = np.sum(np.sqrt(np.diff(slope) ** 2 + 1.0e-12))
        lower = np.minimum(vector[-1], min_delay_fit) - 0.25
        upper = np.maximum(vector[-1], min_delay_fit) + 0.25
        overshoot = np.maximum(lower - spline_delay, 0.0) ** 2 + np.maximum(
            spline_delay - upper, 0.0
        ) ** 2
        negative_delay = np.minimum(spline_delay, 0.0)
        causality = min(float(vector[-1]), 0.0) ** 2 + float(
            np.mean(negative_delay**2)
        )
        return float(
            np.mean(residual**2)
            + 1.0e-2 * np.mean(second**2)
            + 2.0e-5 * np.mean(third**2)
            + 1.0e-6 * slope_tv
            + 100.0 * np.mean(overshoot)
            + 1.0e8 * causality
            + 1.0e3 * (float(vector[-1]) - low_reference_delay) ** 2
        )

    sampler = stats.qmc.Sobol(d=max(nullspace.shape[1], 1), scramble=False, seed=seed)
    perturbations = sampler.random_base2(int(math.ceil(math.log2(max(starts, 1)))))[:starts]
    candidates: list[dict[str, Any]] = []
    best = None
    for start in range(starts):
        reduced = np.zeros(nullspace.shape[1], dtype=np.float64)
        if nullspace.shape[1]:
            direction = 2.0 * perturbations[start, : nullspace.shape[1]] - 1.0
            reduced = 0.02 * direction
        result = optimize.minimize(
            lambda value: objective(initial + nullspace @ value),
            reduced,
            method="L-BFGS-B",
            options={"maxiter": 4000, "ftol": 1.0e-15, "gtol": 1.0e-10, "disp": False},
        )
        full_result = initial + nullspace @ result.x
        equality_error = float(
            np.max(np.abs(equality_matrix @ full_result - equality_values))
        )
        candidate = {
            "start": start,
            "success": bool(result.success),
            "status": int(result.status),
            "message": str(result.message),
            "objective": float(result.fun),
            "equality_error": equality_error,
            "iterations": int(result.nit),
            "tau_zero_samples": float(full_result[-1]),
            "minimum_transition_delay_samples": float(
                np.min(basis @ full_result[:controls])
            ),
            "maximum_transition_delay_samples": float(
                np.max(basis @ full_result[:controls])
            ),
        }
        candidates.append(candidate)
        if np.isfinite(result.fun) and equality_error <= 1.0e-9 and (
            best is None or result.fun < best.fun
        ):
            result.full_result = full_result
            best = result
    if best is None:
        raise RuntimeError(f"all group-delay spline starts failed: {candidates}")

    selected = best.full_result
    coefficients = selected[:controls]
    tau_zero = float(selected[-1])
    target_delay = minimum_delay.copy()
    low_mask = omega <= omega_lo
    transition_mask = (omega > omega_lo) & (omega < omega_hi)
    target_delay[low_mask] = tau_zero
    target_delay[transition_mask] = interpolate.BSpline(
        knots, coefficients, degree, extrapolate=False
    )(np.log(omega[transition_mask]))

    target_phase = np.empty_like(omega)
    target_phase[0] = minimum_phase[0]
    target_phase[1:] = target_phase[0] - np.cumsum(
        0.5 * (target_delay[1:] + target_delay[:-1]) * delta_omega
    )
    join_bin = int(np.ceil(omega_hi / delta_omega))
    numerical_join_error = target_phase[join_bin] - minimum_phase[join_bin]
    # Correct integration quadrature error inside the B-spline equality
    # manifold. This changes controls, not the endpoint law or the final phase.
    if abs(numerical_join_error) > 1.0e-12:
        endpoint_matrix = np.vstack(endpoint_rows)
        endpoint_null = linalg.null_space(endpoint_matrix)
        if endpoint_null.shape[1] == 0:
            raise RuntimeError("group-delay endpoint constraints leave no closure direction")
        transition_omega = omega[transition_mask]
        transition_basis = interpolate.BSpline.design_matrix(
            np.log(transition_omega), knots, degree
        ).toarray()
        best_direction = None
        best_integral = 0.0
        for column in range(endpoint_null.shape[1]):
            direction = endpoint_null[:, column]
            direction_delay = np.zeros_like(target_delay)
            direction_delay[low_mask] = direction[-1]
            direction_delay[transition_mask] = transition_basis @ direction[:controls]
            integral_value = float(
                np.sum(
                    0.5
                    * (direction_delay[1 : join_bin + 1] + direction_delay[:join_bin])
                    * delta_omega
                )
            )
            if abs(integral_value) > abs(best_integral):
                best_integral = integral_value
                best_direction = direction
        if best_direction is None or abs(best_integral) < 1.0e-14:
            raise RuntimeError("unable to find a numerically stable phase-closure direction")
        correction = numerical_join_error / best_integral
        corrected = selected + correction * best_direction
        coefficients = corrected[:controls]
        tau_zero = float(corrected[-1])
        target_delay[low_mask] = tau_zero
        target_delay[transition_mask] = interpolate.BSpline(
            knots, coefficients, degree, extrapolate=False
        )(np.log(omega[transition_mask]))
        target_phase[0] = minimum_phase[0]
        target_phase[1:] = target_phase[0] - np.cumsum(
            0.5 * (target_delay[1:] + target_delay[:-1]) * delta_omega
        )
    join_error = float(target_phase[join_bin] - minimum_phase[join_bin])
    if abs(join_error) > 1.0e-10:
        raise RuntimeError(f"group-delay phase closure is {join_error} rad")
    target_phase[join_bin:] = minimum_phase[join_bin:]

    report = {
        "degree": degree,
        "control_values": controls,
        "tau_zero_samples": tau_zero,
        "low_reference_delay_samples": low_reference_delay,
        "coefficients": [float(value) for value in coefficients],
        "knots": [float(value) for value in knots],
        "join_bin": join_bin,
        "join_error_rad": join_error,
        "endpoint_equality_error": float(
            np.max(np.abs(np.vstack(endpoint_rows) @ np.r_[coefficients, tau_zero] - endpoint_values))
        ),
        "starts": candidates,
    }
    np.save(phase_path, target_phase)
    np.save(delay_path, target_delay)
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    return target_phase, target_delay, report
