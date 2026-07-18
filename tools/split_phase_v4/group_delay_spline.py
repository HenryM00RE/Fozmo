from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import numpy as np
from scipy import interpolate, linalg


@dataclass(frozen=True)
class ConstrainedDelaySpline:
    degree: int
    knots: np.ndarray
    particular: np.ndarray
    nullspace: np.ndarray
    constraint_residual: float

    @property
    def controls(self) -> int:
        return self.knots.size - self.degree - 1

    @property
    def free_coordinates(self) -> int:
        return self.nullspace.shape[1]

    def coefficients_and_low_delay(self, free: np.ndarray) -> tuple[np.ndarray, float]:
        state = self.particular + self.nullspace @ np.asarray(free, dtype=np.float64)
        return state[:-1], float(state[-1])

    def evaluate(self, frequency_hz: np.ndarray, free: np.ndarray, derivative: int = 0) -> np.ndarray:
        coefficients, _ = self.coefficients_and_low_delay(free)
        coordinate = np.log(np.asarray(frequency_hz, dtype=np.float64))
        return interpolate.BSpline(self.knots, coefficients, self.degree).derivative(derivative)(coordinate)


def _basis(knots: np.ndarray, degree: int, coordinate: np.ndarray, derivative: int = 0) -> np.ndarray:
    controls = knots.size - degree - 1
    result = np.empty((coordinate.size, controls), dtype=np.float64)
    for index in range(controls):
        coefficient = np.zeros(controls, dtype=np.float64)
        coefficient[index] = 1.0
        result[:, index] = interpolate.BSpline(knots, coefficient, degree).derivative(derivative)(coordinate)
    return result


def build_constrained_spline(
    minimum_frequency_hz: np.ndarray,
    minimum_delay: np.ndarray,
    controls: int = 24,
    degree: int = 5,
) -> ConstrainedDelaySpline:
    if controls < 2 * (degree + 1):
        raise ValueError("too few controls for a clamped degree-five spline")
    lo_hz = 3000.0
    hi_hz = 14000.0
    lo = np.log(lo_hz)
    hi = np.log(hi_hz)
    interior_count = controls - degree - 1
    interior = np.linspace(lo, hi, interior_count + 2)[1:-1]
    knots = np.concatenate((np.full(degree + 1, lo), interior, np.full(degree + 1, hi)))
    log_frequency = np.log(np.asarray(minimum_frequency_hz, dtype=np.float64))
    minimum = np.asarray(minimum_delay, dtype=np.float64)
    minimum_slope = np.gradient(minimum, log_frequency, edge_order=2)
    minimum_curvature = np.gradient(minimum_slope, log_frequency, edge_order=2)
    hi_delay = float(np.interp(hi, log_frequency, minimum))
    hi_slope = float(np.interp(hi, log_frequency, minimum_slope))
    hi_curvature = float(np.interp(hi, log_frequency, minimum_curvature))
    rows = []
    right = []
    for derivative, target in ((0, 0.0), (1, 0.0), (2, 0.0)):
        row = np.zeros(controls + 1, dtype=np.float64)
        row[:controls] = _basis(knots, degree, np.asarray([lo]), derivative)[0]
        if derivative == 0:
            row[-1] = -1.0
        rows.append(row)
        right.append(target)
    for derivative, target in ((0, hi_delay), (1, hi_slope), (2, hi_curvature)):
        row = np.zeros(controls + 1, dtype=np.float64)
        row[:controls] = _basis(knots, degree, np.asarray([hi]), derivative)[0]
        rows.append(row)
        right.append(target)
    # Exact phase closure from DC to 14 kHz. Below 3 kHz tau_D is the low-delay variable.
    integration_frequency = np.linspace(max(float(minimum_frequency_hz[0]), 1.0e-6), hi_hz, 8193)
    omega = 2.0 * np.pi * integration_frequency / 88_200.0
    closure = np.zeros(controls + 1, dtype=np.float64)
    transition_mask = integration_frequency >= lo_hz
    transition_basis = _basis(knots, degree, np.log(integration_frequency[transition_mask]), 0)
    for index in range(controls):
        values = np.zeros(integration_frequency.size, dtype=np.float64)
        values[transition_mask] = transition_basis[:, index]
        closure[index] = np.trapz(values, omega)
    low_values = (~transition_mask).astype(np.float64)
    closure[-1] = np.trapz(low_values, omega)
    rows.append(closure)
    right.append(float(np.trapz(np.interp(integration_frequency, minimum_frequency_hz, minimum), omega)))
    constraint = np.asarray(rows)
    target = np.asarray(right)
    particular, *_ = np.linalg.lstsq(constraint, target, rcond=None)
    nullspace = linalg.null_space(constraint)
    residual = float(np.max(np.abs(constraint @ particular - target)))
    if residual > 5.0e-10:
        raise RuntimeError("group-delay equality nullspace construction failed")
    return ConstrainedDelaySpline(degree, knots, particular, nullspace, residual)


def optimize_coordinates(model: ConstrainedDelaySpline, c_prior_frequency: np.ndarray, c_prior_delay: np.ndarray, prior_weight: float = 1.0e-3) -> tuple[np.ndarray, dict[str, Any]]:
    sample_frequency = np.geomspace(3000.0, 14000.0, 2048)
    basis = _basis(model.knots, model.degree, np.log(sample_frequency), 0)
    curvature_basis = _basis(model.knots, model.degree, np.log(sample_frequency), 2)
    base_coefficients = model.particular[:-1]
    free_coefficients = model.nullspace[:-1]
    prior = np.interp(sample_frequency, c_prior_frequency, c_prior_delay)
    operator = np.vstack((curvature_basis @ free_coefficients, np.sqrt(prior_weight) * basis @ free_coefficients))
    target = np.concatenate((-(curvature_basis @ base_coefficients), np.sqrt(prior_weight) * (prior - basis @ base_coefficients)))
    free, *_ = np.linalg.lstsq(operator, target, rcond=None)
    realized_curvature = curvature_basis @ (base_coefficients + free_coefficients @ free)
    report = {
        "degree": model.degree,
        "controls": model.controls,
        "free_coordinates": model.free_coordinates,
        "constraint_residual": model.constraint_residual,
        "c_prior_weight": prior_weight,
        "maximum_physical_curvature_samples_per_ln_hz_squared": float(np.max(np.abs(realized_curvature))),
        "controls_remain_live_for_outer_optimization": True,
    }
    return free, report


def save(model: ConstrainedDelaySpline, free: np.ndarray, report: dict[str, Any], work_dir: Path) -> None:
    work_dir.mkdir(parents=True, exist_ok=True)
    np.savez(work_dir / "group_delay_spline.npz", degree=model.degree, knots=model.knots, particular=model.particular, nullspace=model.nullspace, free=free)
    (work_dir / "group_delay_spline.json").write_text(json.dumps(report, indent=2) + "\n")
