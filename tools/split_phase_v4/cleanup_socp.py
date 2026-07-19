from __future__ import annotations

import json
import math
from pathlib import Path
from typing import Any

import cvxpy as cp
import numpy as np


def _canonical_from_left(left: np.ndarray, runtime_branch_taps: int) -> np.ndarray:
    length = 2 * runtime_branch_taps - 1
    center = length // 2
    result = np.zeros(length, dtype=np.float64)
    result[center] = 0.5
    positions = np.arange(1, center, 2)
    result[positions] = left
    result[length - 1 - positions] = left
    return result


def _project_left_sum(left: np.ndarray) -> np.ndarray:
    result = np.asarray(left, dtype=np.float64).copy()
    pivot = int(np.argmax(np.abs(result)))
    for _ in range(4):
        result[pivot] += 0.25 - math.fsum(float(value) for value in result)
    return result


def project_cleanup_equalities(canonical: np.ndarray) -> np.ndarray:
    values = np.asarray(canonical, dtype=np.float64)
    center = values.size // 2
    return _canonical_from_left(_project_left_sum(values[np.arange(1, center, 2)]), values.size // 2 + 1)


def optimize_cleanup(initial: np.ndarray, stage: int, pass_edge: float, stop_edge: float, trust_radius: float = 2.0e-4) -> tuple[np.ndarray, dict[str, Any]]:
    canonical = np.asarray(initial, dtype=np.float64)
    runtime_taps = canonical.size // 2 + 1
    center = canonical.size // 2
    positions = np.arange(1, center, 2)
    initial_left = canonical[positions]
    variable = cp.Variable(positions.size, name="independent_odd_halfband_coefficients")
    peak = cp.Variable(nonneg=True, name="complex_chebyshev_peak")
    pass_omega = np.linspace(0.0, pass_edge * np.pi, 2049)
    stop_omega = np.linspace(stop_edge * np.pi, np.pi, 2049)
    pass_operator = 2.0 * np.cos(pass_omega[:, None] * (center - positions)[None, :])
    stop_operator = 2.0 * np.cos(stop_omega[:, None] * (center - positions)[None, :])
    pass_response = 0.5 + pass_operator @ variable
    stop_response = 0.5 + stop_operator @ variable
    constraints = [
        cp.sum(variable) == 0.25,
        cp.norm(variable - initial_left, 2) <= trust_radius,
        cp.abs(pass_response - 1.0) <= peak,
        cp.abs(stop_response) <= peak,
    ]
    problem = cp.Problem(cp.Minimize(peak), constraints)
    variable.value = initial_left
    problem.solve(solver="CLARABEL", max_iter=500, tol_gap_abs=1.0e-11, tol_gap_rel=1.0e-11, tol_feas=1.0e-11)
    if problem.status not in {cp.OPTIMAL, cp.OPTIMAL_INACCURATE} or variable.value is None:
        raise RuntimeError("cleanup stage " + str(stage) + " SOCP failed: " + problem.status)
    candidate_left = _project_left_sum(np.asarray(variable.value))
    candidate = _canonical_from_left(candidate_left, runtime_taps)
    before_pass = 0.5 + pass_operator @ initial_left
    before_stop = 0.5 + stop_operator @ initial_left
    before = max(float(np.max(np.abs(before_pass - 1.0))), float(np.max(np.abs(before_stop))))
    after = max(float(np.max(np.abs(0.5 + pass_operator @ candidate_left - 1.0))), float(np.max(np.abs(0.5 + stop_operator @ candidate_left))))
    accepted = after < before * (1.0 - 1.0e-10)
    if not accepted:
        candidate = canonical.copy()
    report = {
        "stage": stage,
        "formulation": "direct complex Chebyshev SOCP with exact halfband equalities and trust region",
        "status": problem.status,
        "before_objective": before,
        "candidate_objective": after,
        "accepted": accepted,
        "convergence": "meaningful improvement" if accepted else "explicit trust-region convergence",
        "canonical_sum": math.fsum(float(value) for value in candidate),
        "even_sum": math.fsum(float(value) for value in candidate[::2]),
        "odd_sum": math.fsum(float(value) for value in candidate[1::2]),
        "maximum_symmetry_error": float(np.max(np.abs(candidate - candidate[::-1]))),
        "interpolation_image_constraint": "canonical H(w+pi) stopband response",
        "independent_decimation_alias_constraint": "reverse alias term H(w/2+pi) evaluated on the separate stopband grid",
    }
    return candidate, report


def optimize_all(initial: list[np.ndarray], work_dir: Path) -> tuple[list[np.ndarray], dict[str, Any]]:
    half_width = (0.035, 0.060, 0.090, 0.175, 0.180, 0.185, 0.190)
    results = []
    reports = []
    for stage, (candidate, width) in enumerate(zip(initial, half_width), start=1):
        optimized, report = optimize_cleanup(candidate, stage, 0.5 - width, 0.5 + width)
        results.append(optimized)
        reports.append(report)
    summary = {"stages": reports, "separate_assets_for_equal_support_stages": True}
    work_dir.mkdir(parents=True, exist_ok=True)
    np.savez(work_dir / "cleanup_optimized.npz", **{"stage_" + str(index): value for index, value in enumerate(results, start=1)})
    (work_dir / "cleanup_socp.json").write_text(json.dumps(summary, indent=2) + "\n")
    return results, summary
