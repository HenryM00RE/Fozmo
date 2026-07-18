from __future__ import annotations

import argparse
import json
import math
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Optional

import cvxpy as cp
import mpmath as mp
import numpy as np
from scipy import signal


@dataclass(frozen=True)
class MagnitudeSpec:
    order: int = 512
    sample_rate_hz: float = 88_200.0
    pass_edge_hz: float = 20_000.0
    stop_edge_hz: float = 22_050.0
    passband_amplitude_ripple: float = 1.0e-7
    stopband_amplitude_db: float = -125.0
    verification_fft_len: int = 8_388_608
    maximum_exchange_rounds: int = 10


def _operator(frequencies_hz: np.ndarray, spec: MagnitudeSpec) -> np.ndarray:
    omega = 2.0 * np.pi * np.asarray(frequencies_hz, dtype=np.float64) / spec.sample_rate_hz
    lag = np.arange(spec.order + 1, dtype=np.float64)
    result = 2.0 * np.cos(omega[:, None] * lag[None, :])
    result[:, 0] = 1.0
    return result


def _initial_grids(spec: MagnitudeSpec) -> dict[str, np.ndarray]:
    nyquist = spec.sample_rate_hz / 2.0
    passband = np.unique(np.concatenate((np.linspace(0.0, spec.pass_edge_hz, 513), np.geomspace(1.0, 3000.0, 128))))
    transition = np.linspace(spec.pass_edge_hz, spec.stop_edge_hz, 257)
    stopband = spec.stop_edge_hz + (nyquist - spec.stop_edge_hz) * 0.5 * (1.0 - np.cos(np.linspace(0.0, np.pi, 513)))
    return {"pass": passband, "transition": transition, "stop": np.unique(stopband)}


def _conventional_warm_gram(spec: MagnitudeSpec) -> np.ndarray:
    # Feasibility initialization only. Export rejects an unchanged seed and
    # the independently accepted PSD-cone iterate is the sole magnitude result.
    impulse = signal.firwin(
        spec.order + 1,
        0.5 * (spec.pass_edge_hz + spec.stop_edge_hz),
        window=("kaiser", 20.0),
        fs=spec.sample_rate_hz,
        scale=True,
    )
    impulse /= math.fsum(float(value) for value in impulse)
    return np.outer(impulse, impulse)


def autocorrelation_from_gram(gram: np.ndarray) -> np.ndarray:
    return np.asarray([np.sum(np.diag(gram, lag)) for lag in range(gram.shape[0])], dtype=np.float64)


def _project_psd_and_dc(gram: np.ndarray) -> tuple[np.ndarray, dict[str, float]]:
    symmetric = 0.5 * (np.asarray(gram, dtype=np.float64) + np.asarray(gram, dtype=np.float64).T)
    eigenvalues, eigenvectors = np.linalg.eigh(symmetric)
    projected = (eigenvectors * np.maximum(eigenvalues, 0.0)) @ eigenvectors.T
    dc = float(np.sum(projected))
    if not np.isfinite(dc) or dc <= 0.0:
        raise RuntimeError("PSD projection produced an invalid DC value")
    projected /= dc
    return projected, {
        "raw_minimum_eigenvalue": float(eigenvalues[0]),
        "negative_eigenvalue_mass_removed": float(np.sum(np.maximum(-eigenvalues, 0.0))),
        "frobenius_projection_distance": float(np.linalg.norm(projected - symmetric)),
    }


def evaluate_power(autocorrelation: np.ndarray, fft_len: int) -> np.ndarray:
    embedded = np.zeros(fft_len, dtype=np.float64)
    order = autocorrelation.size - 1
    embedded[: order + 1] = autocorrelation
    embedded[-order:] = autocorrelation[1:][::-1]
    return np.fft.rfft(embedded).real


def _dense_metrics(autocorrelation: np.ndarray, spec: MagnitudeSpec) -> dict[str, float]:
    power = evaluate_power(autocorrelation, spec.verification_fft_len)
    frequency = np.linspace(0.0, spec.sample_rate_hz / 2.0, power.size)
    passband = power[frequency <= spec.pass_edge_hz]
    transition = power[(frequency >= spec.pass_edge_hz) & (frequency <= spec.stop_edge_hz)]
    stopband = power[frequency >= spec.stop_edge_hz]
    amplitude = np.sqrt(np.maximum(passband, 0.0))
    return {
        "passband_amplitude_ripple": float(np.max(np.abs(amplitude - 1.0))),
        "passband_power_error": float(np.max(np.abs(passband - 1.0))),
        "stopband_power_peak": float(np.max(stopband)),
        "stopband_amplitude_db": float(10.0 * np.log10(max(float(np.max(stopband)), 1.0e-300))),
        "global_minimum_power": float(np.min(power)),
        "transition_maximum_upward_power": float(np.max(np.maximum(np.diff(transition), 0.0))),
    }


def _turning_points(values: np.ndarray) -> np.ndarray:
    if values.size < 3:
        return np.arange(values.size)
    difference = np.diff(values)
    return np.unique(np.concatenate(([0], np.flatnonzero(difference[:-1] * difference[1:] <= 0.0) + 1, [values.size - 1])))


def _exchange(grids: dict[str, np.ndarray], autocorrelation: np.ndarray, spec: MagnitudeSpec) -> tuple[dict[str, np.ndarray], int]:
    power = evaluate_power(autocorrelation, spec.verification_fft_len)
    frequency = np.linspace(0.0, spec.sample_rate_hz / 2.0, power.size)
    pass_indices = np.flatnonzero(frequency <= spec.pass_edge_hz)
    transition_indices = np.flatnonzero((frequency >= spec.pass_edge_hz) & (frequency <= spec.stop_edge_hz))
    stop_indices = np.flatnonzero(frequency >= spec.stop_edge_hz)
    additions = {"pass": [], "transition": [], "stop": []}
    pass_error = np.abs(power[pass_indices] - 1.0)
    for index in _turning_points(pass_error):
        if pass_error[index] > 2.0 * spec.passband_amplitude_ripple:
            additions["pass"].append(float(frequency[pass_indices[index]]))
    stop_power = power[stop_indices]
    stop_gate = 10.0 ** (spec.stopband_amplitude_db / 10.0)
    for index in _turning_points(stop_power):
        if stop_power[index] > stop_gate:
            additions["stop"].append(float(frequency[stop_indices[index]]))
    transition = power[transition_indices]
    upward = np.flatnonzero(np.diff(transition) > 1.0e-13)
    additions["transition"].extend(float(frequency[transition_indices[index + 1]]) for index in upward[:256])
    updated = {key: np.unique(np.concatenate((value, np.asarray(additions[key], dtype=np.float64)))) for key, value in grids.items()}
    return updated, int(sum(updated[key].size - grids[key].size for key in grids))


def _solver_options(
    solver: str,
    scs_backend: str = "indirect",
    scs_accuracy: str = "strict",
) -> dict[str, Any]:
    if solver == "SCS":
        if scs_accuracy == "initial":
            eps_abs = 1.0e-6
            eps_rel = 1.0e-6
            max_iters = 20_000
        elif scs_accuracy == "strict":
            eps_abs = 1.0e-9
            eps_rel = 1.0e-9
            max_iters = 100_000
        else:
            raise ValueError("unsupported SCS accuracy profile: " + scs_accuracy)
        options = {
            "eps_abs": eps_abs,
            "eps_rel": eps_rel,
            "max_iters": max_iters,
            "acceleration_lookback": 20,
            "normalize": True,
            "scale": 1.0,
            "verbose": False,
        }
        if scs_backend == "indirect":
            options["use_indirect"] = True
        elif scs_backend == "gpu":
            options.update({"use_indirect": True, "gpu": True})
        elif scs_backend == "mkl":
            options.update({"use_indirect": False, "mkl": True})
        elif scs_backend == "direct":
            options["use_indirect"] = False
        else:
            raise ValueError("unsupported SCS backend: " + scs_backend)
        return options
    if solver == "CLARABEL":
        return {"max_iter": 1000, "tol_gap_abs": 1.0e-11, "tol_gap_rel": 1.0e-11, "tol_feas": 1.0e-11, "verbose": False}
    return {"verbose": False}


def _solver_audit(problem: cp.Problem) -> dict[str, Any]:
    stats = problem.solver_stats
    extra = stats.extra_stats
    info = extra.get("info", {}) if isinstance(extra, dict) else {}
    primal = info.get("pobj", problem.value)
    dual = info.get("dobj")
    gap = info.get("gap")
    return {
        "status": problem.status,
        "primal_objective": None if primal is None else float(primal),
        "dual_objective": None if dual is None else float(dual),
        "primal_dual_gap": None if gap is None else float(gap),
        "iterations": stats.num_iters,
        "solve_time_seconds": stats.solve_time,
        "solver_name": stats.solver_name,
    }


def _solve_lexicographic(
    spec: MagnitudeSpec,
    grids: dict[str, np.ndarray],
    solver: str,
    warm_gram: Optional[np.ndarray],
    scs_backend: str,
    scs_accuracy: str,
) -> tuple[np.ndarray, dict[str, Any]]:
    dimension = spec.order + 1
    gram = cp.Variable((dimension, dimension), symmetric=True, name="fejer_riesz_gram")
    autocorrelation = cp.Variable(dimension, name="autocorrelation_diagonal_sums")
    pass_power = _operator(grids["pass"], spec) @ autocorrelation
    transition_power = _operator(grids["transition"], spec) @ autocorrelation
    stop_power = _operator(grids["stop"], spec) @ autocorrelation
    pass_error = cp.Variable(nonneg=True, name="pass_power_error")
    stop_peak = cp.Variable(nonneg=True, name="stop_power_peak")
    stop_gate = 10.0 ** (spec.stopband_amplitude_db / 10.0)
    constraints = [
        gram >> 0,
        *[
            autocorrelation[lag] == cp.sum(cp.diag(gram, lag))
            for lag in range(dimension)
        ],
        cp.sum(gram) == 1.0,
        pass_power >= 1.0 - pass_error,
        pass_power <= 1.0 + pass_error,
        pass_error <= 2.01 * spec.passband_amplitude_ripple,
        stop_power >= 0.0,
        stop_power <= stop_peak,
        stop_peak <= stop_gate,
        transition_power[1:] <= transition_power[:-1] + 1.0e-11,
    ]
    if warm_gram is not None:
        gram.value = warm_gram
        autocorrelation.value = autocorrelation_from_gram(warm_gram)
    history = []
    options = _solver_options(solver, scs_backend, scs_accuracy)
    first = cp.Problem(cp.Minimize(pass_error), constraints)
    first.solve(solver=solver, warm_start=True, **options)
    history.append({"objective": "passband_peak_power_error", **_solver_audit(first)})
    if first.status not in {cp.OPTIMAL, cp.OPTIMAL_INACCURATE} or gram.value is None:
        raise RuntimeError("PSD magnitude feasibility/passband solve failed: " + first.status)
    pass_optimum = float(pass_error.value)
    constraints.append(pass_error <= max(pass_optimum * 1.01, pass_optimum + 2.0e-12))
    second = cp.Problem(cp.Minimize(stop_peak), constraints)
    second.solve(solver=solver, warm_start=True, **options)
    history.append({"objective": "stopband_peak_power", **_solver_audit(second)})
    if second.status not in {cp.OPTIMAL, cp.OPTIMAL_INACCURATE} or gram.value is None:
        raise RuntimeError("PSD magnitude stopband solve failed: " + second.status)
    stop_optimum = float(stop_peak.value)
    constraints.append(stop_peak <= max(stop_optimum * 1.01, stop_optimum + 1.0e-15))
    integrated_stop = cp.sum(stop_power) / stop_power.shape[0]
    transition_curvature = cp.sum_squares(transition_power[2:] - 2.0 * transition_power[1:-1] + transition_power[:-2])
    third = cp.Problem(cp.Minimize(integrated_stop + 1.0e-3 * transition_curvature), constraints)
    third.solve(solver=solver, warm_start=True, **options)
    history.append({"objective": "integrated_stopband_then_transition_curvature", **_solver_audit(third)})
    if third.status not in {cp.OPTIMAL, cp.OPTIMAL_INACCURATE} or gram.value is None:
        raise RuntimeError("PSD magnitude energy solve failed: " + third.status)
    return np.asarray(gram.value, dtype=np.float64), {"lexicographic_history": history}


def _high_precision_check(autocorrelation: np.ndarray, spec: MagnitudeSpec, points: int = 256) -> dict[str, float]:
    mp.mp.dps = 50
    worst_discrepancy = mp.mpf("0")
    minimum = mp.inf
    dense = evaluate_power(autocorrelation, spec.verification_fft_len)
    worst_indices = np.argpartition(dense, min(points, dense.size) - 1)[:points]
    frequencies = worst_indices.astype(np.float64) * spec.sample_rate_hz / spec.verification_fft_len
    for frequency in frequencies:
        omega = mp.mpf(str(2.0 * math.pi * frequency / spec.sample_rate_hz))
        high = mp.mpf(str(float(autocorrelation[0])))
        for lag, value in enumerate(autocorrelation[1:], start=1):
            high += 2 * mp.mpf(str(float(value))) * mp.cos(lag * omega)
        ordinary = float(autocorrelation[0] + 2.0 * np.dot(autocorrelation[1:], np.cos(np.arange(1, autocorrelation.size) * float(omega))))
        worst_discrepancy = max(worst_discrepancy, abs(high - mp.mpf(str(ordinary))))
        minimum = min(minimum, high)
    return {
        "points": int(frequencies.size),
        "point_selection": "lowest-power bins from the independent dense grid",
        "decimal_digits": mp.mp.dps,
        "minimum_power": float(minimum),
        "maximum_float64_discrepancy": float(worst_discrepancy),
    }


def solve(
    spec: MagnitudeSpec,
    work_dir: Path,
    solver: str = "auto",
    scs_backend: str = "indirect",
    scs_accuracy: str = "strict",
) -> dict[str, Any]:
    available = set(cp.installed_solvers())
    if solver == "auto":
        if "MOSEK" in available:
            solver = "MOSEK"
        elif spec.order >= 512 and "SCS" in available:
            solver = "SCS"
        else:
            solver = "CLARABEL" if "CLARABEL" in available else "SCS"
    if solver not in available:
        raise RuntimeError("requested genuine PSD-cone solver is unavailable: " + solver)
    grids = _initial_grids(spec)
    seed_gram = _conventional_warm_gram(spec)
    seed_autocorrelation = autocorrelation_from_gram(seed_gram)
    seed_hash = __import__("hashlib").sha256(np.asarray(seed_autocorrelation, dtype="<f8").tobytes()).hexdigest()
    gram: Optional[np.ndarray] = seed_gram
    exchange_history = []
    started = time.time()
    for round_index in range(spec.maximum_exchange_rounds):
        raw_gram, audit = _solve_lexicographic(
            spec,
            grids,
            solver,
            gram,
            scs_backend,
            scs_accuracy,
        )
        gram, projection = _project_psd_and_dc(raw_gram)
        autocorrelation = autocorrelation_from_gram(gram)
        metrics = _dense_metrics(autocorrelation, spec)
        grids, added = _exchange(grids, autocorrelation, spec)
        exchange_history.append({"round": round_index, "grid_sizes": {key: int(value.size) for key, value in grids.items()}, "points_added": added, "dense_verification": metrics, "independent_psd_projection": projection, **audit})
        accepted = metrics["passband_amplitude_ripple"] <= 1.01 * spec.passband_amplitude_ripple and metrics["stopband_amplitude_db"] <= spec.stopband_amplitude_db + 0.05 and metrics["global_minimum_power"] >= -1.0e-12 and metrics["transition_maximum_upward_power"] <= 1.0e-11
        work_dir.mkdir(parents=True, exist_ok=True)
        np.savez(
            work_dir / ("magnitude_order_" + str(spec.order) + "_checkpoint.npz"),
            autocorrelation=autocorrelation,
            gram=gram,
        )
        (work_dir / ("magnitude_order_" + str(spec.order) + "_checkpoint.json")).write_text(
            json.dumps(
                {
                    "completed_exchange_round": round_index,
                    "independently_feasible_on_dense_grid": bool(accepted),
                    "exchange_history": exchange_history,
                },
                indent=2,
            )
            + "\n"
        )
        if added == 0 and accepted:
            break
    if gram is None:
        raise RuntimeError("PSD solver produced no Gram matrix")
    autocorrelation = autocorrelation_from_gram(gram)
    distance_from_seed = float(np.linalg.norm(autocorrelation - seed_autocorrelation))
    metrics = _dense_metrics(autocorrelation, spec)
    eigen_minimum = float(np.linalg.eigvalsh(0.5 * (gram + gram.T))[0])
    equality_residual = max(
        abs(float(np.sum(gram)) - 1.0),
        float(np.max(np.abs(autocorrelation - autocorrelation_from_gram(gram)))),
    )
    high_precision = _high_precision_check(autocorrelation, spec)
    accepted = bool(metrics["passband_amplitude_ripple"] <= 1.01 * spec.passband_amplitude_ripple and metrics["stopband_amplitude_db"] <= spec.stopband_amplitude_db + 0.05 and high_precision["minimum_power"] >= -1.0e-18 and eigen_minimum >= -1.0e-8 and equality_residual <= 1.0e-9 and distance_from_seed > 1.0e-10)
    report = {
        "formulation": "real symmetric Fejer-Riesz Gram matrix with genuine PSD cone",
        "warm_start_exported": False,
        "warm_start_kind": "Kaiser feasibility seed only",
        "warm_start_autocorrelation_sha256": seed_hash,
        "production_distance_from_warm_start_l2": distance_from_seed,
        "production_equals_warm_start": bool(distance_from_seed <= 1.0e-10),
        "conventional_filter_fallback_permitted": False,
        "order": spec.order,
        "solver": solver,
        "solver_backend": scs_backend if solver == "SCS" else "solver default",
        "solver_accuracy_profile": scs_accuracy if solver == "SCS" else "solver default",
        "elapsed_seconds": time.time() - started,
        "exchange_history": exchange_history,
        "active_frequency_sets_hz": {key: value.tolist() for key, value in grids.items()},
        "dense_verification": metrics,
        "psd_minimum_eigenvalue": eigen_minimum,
        "diagonal_sum_equality_residual": equality_residual,
        "high_precision_verification": high_precision,
        "accepted": accepted,
    }
    work_dir.mkdir(parents=True, exist_ok=True)
    np.savez(work_dir / ("magnitude_order_" + str(spec.order) + ".npz"), autocorrelation=autocorrelation, gram=gram)
    (work_dir / ("magnitude_order_" + str(spec.order) + ".json")).write_text(json.dumps(report, indent=2) + "\n")
    if not accepted:
        raise RuntimeError("genuine PSD magnitude candidate failed independent verification")
    return report


def audit_existing(spec: MagnitudeSpec, work_dir: Path) -> dict[str, Any]:
    """Re-audit a checkpoint produced by an older/resumed solver process."""
    path = work_dir / ("magnitude_order_" + str(spec.order) + ".npz")
    data = np.load(path)
    gram = np.asarray(data["gram"], dtype=np.float64)
    autocorrelation = np.asarray(data["autocorrelation"], dtype=np.float64)
    report_path = work_dir / ("magnitude_order_" + str(spec.order) + ".json")
    report = json.loads(report_path.read_text())
    metrics = _dense_metrics(autocorrelation, spec)
    grids = _initial_grids(spec)
    exchange_audit = []
    for round_index in range(spec.maximum_exchange_rounds):
        grids, added = _exchange(grids, autocorrelation, spec)
        exchange_audit.append({"round": round_index, "points_added": added})
        if added == 0:
            break
    high_precision = _high_precision_check(autocorrelation, spec)
    eigen_minimum = float(np.linalg.eigvalsh(0.5 * (gram + gram.T))[0])
    seed = autocorrelation_from_gram(_conventional_warm_gram(spec))
    distance_from_seed = float(np.linalg.norm(autocorrelation - seed))
    equality_residual = max(
        abs(float(np.sum(gram)) - 1.0),
        float(np.max(np.abs(autocorrelation - autocorrelation_from_gram(gram)))),
    )
    accepted = bool(
        metrics["passband_amplitude_ripple"] <= 1.01 * spec.passband_amplitude_ripple
        and metrics["stopband_amplitude_db"] <= spec.stopband_amplitude_db + 0.05
        and metrics["transition_maximum_upward_power"] <= 1.0e-11
        and high_precision["minimum_power"] >= -1.0e-18
        and eigen_minimum >= -1.0e-8
        and equality_residual <= 1.0e-9
        and distance_from_seed > 1.0e-10
        and exchange_audit[-1]["points_added"] == 0
    )
    report.update(
        {
            "dense_verification": metrics,
            "psd_minimum_eigenvalue": eigen_minimum,
            "diagonal_sum_equality_residual": equality_residual,
            "production_distance_from_warm_start_l2": distance_from_seed,
            "high_precision_verification": high_precision,
            "post_resume_exchange_audit": exchange_audit,
            "active_frequency_sets_hz": {key: value.tolist() for key, value in grids.items()},
            "accepted": accepted,
        }
    )
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    if not accepted:
        raise RuntimeError("saved PSD magnitude candidate failed the current independent audit")
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--order", type=int, default=512)
    parser.add_argument("--solver", default="auto")
    parser.add_argument(
        "--scs-backend",
        choices=("indirect", "gpu", "mkl", "direct"),
        default="indirect",
    )
    parser.add_argument(
        "--scs-accuracy",
        choices=("initial", "strict"),
        default="strict",
    )
    parser.add_argument("--verification-fft-len", type=int, default=8_388_608)
    parser.add_argument("--exchange-rounds", type=int, default=10)
    parser.add_argument("--work-dir", type=Path, default=Path(__file__).resolve().parent / "work")
    parser.add_argument("--audit-existing", action="store_true")
    arguments = parser.parse_args()
    specification = MagnitudeSpec(order=arguments.order, verification_fft_len=arguments.verification_fft_len, maximum_exchange_rounds=arguments.exchange_rounds)
    result = audit_existing(specification, arguments.work_dir) if arguments.audit_existing else solve(
        specification,
        arguments.work_dir,
        arguments.solver,
        arguments.scs_backend,
        arguments.scs_accuracy,
    )
    print(json.dumps(result, indent=2))
