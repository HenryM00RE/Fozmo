from __future__ import annotations

import json
import math
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import cvxpy as cp
import numpy as np
from scipy import signal


@dataclass(frozen=True)
class MagnitudeSpec:
    order: int
    sample_rate_hz: float
    pass_edge_hz: float
    stop_edge_hz: float
    verification_fft_len: int
    maximum_exchange_rounds: int
    passband_amplitude_ripple: float = 1.0e-5
    initial_stopband_amplitude: float = 1.0e-6


def _omega(frequency_hz: np.ndarray | float, sample_rate_hz: float) -> np.ndarray:
    return 2.0 * np.pi * np.asarray(frequency_hz, dtype=np.float64) / sample_rate_hz


def _cosine_operator(frequencies_hz: np.ndarray, spec: MagnitudeSpec) -> np.ndarray:
    omega = _omega(frequencies_hz, spec.sample_rate_hz)
    columns = np.arange(spec.order + 1, dtype=np.float64)
    operator = 2.0 * np.cos(omega[:, None] * columns[None, :])
    operator[:, 0] = 1.0
    return operator


def _initial_grids(spec: MagnitudeSpec) -> dict[str, np.ndarray]:
    nyquist = spec.sample_rate_hz / 2.0
    pass_linear = np.linspace(0.0, spec.pass_edge_hz, 2049)
    pass_log = np.geomspace(1.0, 3000.0, 384)
    edge_width = spec.stop_edge_hz - spec.pass_edge_hz
    edge_cluster = np.sin(np.linspace(-np.pi / 2.0, np.pi / 2.0, 1025))
    transition = spec.pass_edge_hz + 0.5 * edge_width * (edge_cluster + 1.0)
    stop_linear = np.linspace(spec.stop_edge_hz, nyquist, 2049)
    stop_chebyshev = spec.stop_edge_hz + 0.5 * (nyquist - spec.stop_edge_hz) * (
        1.0 - np.cos(np.linspace(0.0, np.pi, 1025))
    )
    global_uniform = np.linspace(0.0, nyquist, 8193)
    return {
        "pass": np.unique(np.concatenate((pass_linear, pass_log))),
        "transition": np.unique(transition),
        "stop": np.unique(np.concatenate((stop_linear, stop_chebyshev))),
        "global": global_uniform,
    }


def _remez_autocorrelation(spec: MagnitudeSpec) -> np.ndarray:
    # This is only a numerical warm start for the convex autocorrelation solve.
    # It is never exported as a production coefficient set.
    try:
        taps = signal.remez(
            spec.order + 1,
            [0.0, spec.pass_edge_hz, spec.stop_edge_hz, spec.sample_rate_hz / 2.0],
            [1.0, 0.0],
            weight=[1.0, 1.0e4],
            fs=spec.sample_rate_hz,
            maxiter=200,
            grid_density=32,
        )
    except ValueError:
        taps = signal.firls(
            spec.order + 1,
            [0.0, spec.pass_edge_hz, spec.stop_edge_hz, spec.sample_rate_hz / 2.0],
            [1.0, 1.0, 0.0, 0.0],
            weight=[1.0, 1.0e4],
            fs=spec.sample_rate_hz,
        )
    taps /= math.fsum(float(value) for value in taps)

    # A smooth positive-spectrum incumbent is important when an open-source
    # conic solver reports OPTIMAL_INACCURATE at the roughly 1e-12 power scale.
    # Keep both warm starts and select them by the same dense-grid feasibility
    # ordering used to accept or reject every solver iterate.
    kaiser = signal.firwin(
        spec.order + 1,
        0.5 * (spec.pass_edge_hz + spec.stop_edge_hz),
        window=("kaiser", 20.0),
        pass_zero="lowpass",
        scale=True,
        fs=spec.sample_rate_hz,
    )

    def autocorrelation_for(candidate: np.ndarray) -> np.ndarray:
        candidate = candidate / math.fsum(float(value) for value in candidate)
        result = np.array(
            [
                np.dot(candidate[: candidate.size - lag], candidate[lag:])
                for lag in range(candidate.size)
            ],
            dtype=np.float64,
        )
        result /= result[0] + 2.0 * np.sum(result[1:])
        return result

    candidates = (autocorrelation_for(taps), autocorrelation_for(kaiser))
    return min(candidates, key=lambda value: _feasibility_key(value, spec))


def _feasibility_key(
    autocorrelation: np.ndarray, spec: MagnitudeSpec
) -> tuple[float, ...]:
    power = evaluate_power_spectrum(autocorrelation, spec.verification_fft_len)
    frequencies = np.linspace(0.0, spec.sample_rate_hz / 2.0, power.size)
    pass_power = power[frequencies <= spec.pass_edge_hz]
    transition_power = power[
        (frequencies >= spec.pass_edge_hz) & (frequencies <= spec.stop_edge_hz)
    ]
    stop_power = power[frequencies >= spec.stop_edge_hz]
    ripple = float(
        np.max(np.abs(np.sqrt(np.maximum(pass_power, 0.0)) - 1.0))
    )
    stop_peak = float(np.max(stop_power))
    minimum = float(np.min(power))
    upward = float(np.max(np.maximum(np.diff(transition_power), 0.0)))
    violations = (
        max(ripple / spec.passband_amplitude_ripple - 1.0, 0.0),
        max(stop_peak / spec.initial_stopband_amplitude**2 - 1.0, 0.0),
        max(-minimum / 1.0e-12 - 1.0, 0.0),
        max(upward / 5.0e-12 - 1.0, 0.0),
    )
    return (max(violations), sum(violations), ripple, stop_peak, upward)


def evaluate_power_spectrum(autocorrelation: np.ndarray, fft_len: int) -> np.ndarray:
    embedded = np.zeros(fft_len, dtype=np.float64)
    order = autocorrelation.size - 1
    embedded[: order + 1] = autocorrelation
    embedded[-order:] = autocorrelation[1:][::-1]
    return np.fft.rfft(embedded).real


def _local_extrema(values: np.ndarray) -> np.ndarray:
    if values.size < 3:
        return np.arange(values.size)
    slope = np.diff(values)
    turns = np.flatnonzero(slope[:-1] * slope[1:] <= 0.0) + 1
    return np.unique(np.concatenate(([0], turns, [values.size - 1])))


def _add_exchange_points(
    grids: dict[str, np.ndarray],
    autocorrelation: np.ndarray,
    spec: MagnitudeSpec,
) -> tuple[dict[str, np.ndarray], dict[str, float], int]:
    power = evaluate_power_spectrum(autocorrelation, spec.verification_fft_len)
    frequencies = np.linspace(0.0, spec.sample_rate_hz / 2.0, power.size)
    pass_mask = frequencies <= spec.pass_edge_hz
    transition_mask = (frequencies >= spec.pass_edge_hz) & (frequencies <= spec.stop_edge_hz)
    stop_mask = frequencies >= spec.stop_edge_hz
    pass_error = np.abs(power[pass_mask] - 1.0)
    stop_power = power[stop_mask]
    minimum_power = float(np.min(power))
    transition_power = power[transition_mask]
    upward = np.maximum(np.diff(transition_power), 0.0)
    metrics = {
        "pass_power_error": float(np.max(pass_error)),
        "pass_amplitude_ripple": float(
            np.max(np.abs(np.sqrt(np.maximum(power[pass_mask], 0.0)) - 1.0))
        ),
        "stop_power": float(np.max(stop_power)),
        "stop_amplitude_db": float(10.0 * np.log10(max(float(np.max(stop_power)), 1.0e-300))),
        "minimum_power": minimum_power,
        "transition_max_upward_power": float(np.max(upward)) if upward.size else 0.0,
    }

    additions: dict[str, list[float]] = {key: [] for key in grids}
    pass_indices = np.flatnonzero(pass_mask)
    stop_indices = np.flatnonzero(stop_mask)
    transition_indices = np.flatnonzero(transition_mask)
    for local_index in _local_extrema(pass_error):
        if pass_error[local_index] > 2.02 * spec.passband_amplitude_ripple:
            additions["pass"].append(float(frequencies[pass_indices[local_index]]))
    for local_index in _local_extrema(stop_power):
        if stop_power[local_index] > 1.02 * spec.initial_stopband_amplitude**2:
            additions["stop"].append(float(frequencies[stop_indices[local_index]]))
    negative = np.flatnonzero(power < -1.0e-12)
    additions["global"].extend(float(value) for value in frequencies[negative[:256]])
    violating_upward = np.flatnonzero(upward > 5.0e-12)
    additions["transition"].extend(
        float(frequencies[transition_indices[index + 1]]) for index in violating_upward[:256]
    )

    updated = {
        key: np.unique(np.concatenate((values, np.asarray(additions[key], dtype=np.float64))))
        for key, values in grids.items()
    }
    added = sum(updated[key].size - grids[key].size for key in grids)
    return updated, metrics, int(added)


def _certification_ok(metrics: dict[str, float], spec: MagnitudeSpec) -> bool:
    return bool(
        metrics["pass_amplitude_ripple"]
        <= 1.01 * spec.passband_amplitude_ripple
        and metrics["stop_power"] <= 1.01 * spec.initial_stopband_amplitude**2
        and metrics["minimum_power"] >= -1.0e-12
        and metrics["transition_max_upward_power"] <= 5.05e-12
    )


def _report_metadata(spec: MagnitudeSpec) -> dict[str, Any]:
    return {
        "formulation": "circulant positive-spectrum LMI with adaptive eigenvalue exchange",
        "order": spec.order,
        "verification_fft_len": spec.verification_fft_len,
        "frequency_grid": {
            "passband": "linear plus logarithmic points below 3 kHz",
            "transition": "sine-clustered endpoints",
            "stopband": "linear plus Chebyshev nodes",
            "global_nonnegative": "uniform eigenvalue grid",
            "exchange": "all dense-grid violating local extrema",
        },
    }


def _solve_round(
    spec: MagnitudeSpec,
    grids: dict[str, np.ndarray],
    initial: np.ndarray,
    solver: str,
    verbose: bool,
) -> tuple[np.ndarray, dict[str, Any]]:
    r = cp.Variable(spec.order + 1, name="autocorrelation")
    pass_op = _cosine_operator(grids["pass"], spec)
    transition_op = _cosine_operator(grids["transition"], spec)
    stop_op = _cosine_operator(grids["stop"], spec)
    global_op = _cosine_operator(grids["global"], spec)
    pass_power = pass_op @ r
    transition_power = transition_op @ r
    stop_power = stop_op @ r
    global_power = global_op @ r
    pass_error = cp.Variable(nonneg=True, name="pass_power_error")
    stop_peak = cp.Variable(nonneg=True, name="stop_power_peak")
    dc_row = np.ones(spec.order + 1, dtype=np.float64) * 2.0
    dc_row[0] = 1.0
    lower_limit = (1.0 - spec.passband_amplitude_ripple) ** 2
    upper_limit = (1.0 + spec.passband_amplitude_ripple) ** 2
    constraints = [
        dc_row @ r == 1.0,
        global_power >= 0.0,
        pass_power >= 1.0 - pass_error,
        pass_power <= 1.0 + pass_error,
        pass_power >= lower_limit,
        pass_power <= upper_limit,
        stop_power >= 0.0,
        stop_power <= stop_peak,
        stop_peak <= spec.initial_stopband_amplitude**2,
        transition_power[1:] <= transition_power[:-1] + 5.0e-13,
    ]
    r.value = initial.copy()
    solve_options: dict[str, Any] = {"verbose": verbose}
    if solver == "CLARABEL":
        solve_options.update(
            max_iter=500,
            tol_gap_abs=1.0e-10,
            tol_gap_rel=1.0e-10,
            tol_feas=1.0e-10,
        )

    started = time.time()
    first = cp.Problem(cp.Minimize(pass_error), constraints)
    first.solve(solver=solver, warm_start=True, **solve_options)
    if first.status not in {cp.OPTIMAL, cp.OPTIMAL_INACCURATE}:
        raise RuntimeError(f"passband SDP failed: {first.status}")
    pass_optimum = float(pass_error.value)
    constraints.append(pass_error <= max(pass_optimum * 1.01, pass_optimum + 1.0e-12))

    second = cp.Problem(cp.Minimize(stop_peak), constraints)
    second.solve(solver=solver, warm_start=True, **solve_options)
    if second.status not in {cp.OPTIMAL, cp.OPTIMAL_INACCURATE}:
        raise RuntimeError(f"stopband SDP failed: {second.status}")
    stop_optimum = max(float(stop_peak.value), 0.0)
    constraints.append(stop_peak <= max(stop_optimum * 1.01, stop_optimum + 1.0e-16))

    curvature = cp.diff(transition_power, 2)
    third = cp.Problem(
        cp.Minimize(cp.sum_squares(stop_power) + 1.0e-2 * cp.sum_squares(curvature)),
        constraints,
    )
    third.solve(solver=solver, warm_start=True, **solve_options)
    if third.status not in {cp.OPTIMAL, cp.OPTIMAL_INACCURATE}:
        raise RuntimeError(f"energy/curvature SDP failed: {third.status}")
    stats = third.solver_stats
    extra = getattr(stats, "extra_stats", None)
    return np.asarray(r.value, dtype=np.float64), {
        "solver": solver,
        "status": third.status,
        "pass_objective": pass_optimum,
        "stop_objective": stop_optimum,
        "final_objective": float(third.value),
        "primal_objective": float(third.value),
        "dual_objective": None,
        "primal_dual_gap": None,
        "solve_seconds": time.time() - started,
        "iterations": getattr(stats, "num_iters", None),
        "extra_stats": str(extra) if extra is not None else None,
    }


def solve_magnitude_sdp(
    spec: MagnitudeSpec,
    work_dir: Path,
    solver: str = "auto",
    resume: bool = True,
    verbose: bool = False,
) -> tuple[np.ndarray, dict[str, Any]]:
    work_dir.mkdir(parents=True, exist_ok=True)
    checkpoint = work_dir / f"magnitude_order_{spec.order}.npz"
    report_path = work_dir / f"magnitude_order_{spec.order}.json"
    saved_report: dict[str, Any] | None = None
    if resume and checkpoint.exists() and report_path.exists():
        saved_report = json.loads(report_path.read_text())
        saved = np.load(checkpoint)
        if saved_report.get("certified") is True:
            return np.asarray(saved["autocorrelation"], dtype=np.float64), saved_report

    installed = cp.installed_solvers()
    if solver == "auto":
        selected = "MOSEK" if "MOSEK" in installed else "CLARABEL"
    else:
        selected = solver.upper()
    if selected not in installed:
        raise RuntimeError(f"requested solver {selected} is unavailable; installed={installed}")

    if saved_report is not None:
        saved = np.load(checkpoint)
        grids = {
            "pass": np.asarray(saved["pass_grid"]),
            "transition": np.asarray(saved["transition_grid"]),
            "stop": np.asarray(saved["stop_grid"]),
            "global": np.asarray(saved["global_grid"]),
        }
        autocorrelation = np.asarray(saved["autocorrelation"], dtype=np.float64)
        history = list(saved_report.get("history", []))
        preflight_grids, preflight_verification, preflight_added = _add_exchange_points(
            grids, autocorrelation, spec
        )
        if preflight_added == 0 and _certification_ok(preflight_verification, spec):
            report = {
                **_report_metadata(spec),
                "solver": selected,
                "history": history,
                "resume_preflight_verification": preflight_verification,
                "certified": True,
            }
            report_path.write_text(json.dumps(report, indent=2) + "\n")
            return autocorrelation, report
        grids = preflight_grids
    else:
        grids = _initial_grids(spec)
        autocorrelation = _remez_autocorrelation(spec)
        history = []
    for exchange_round in range(len(history), spec.maximum_exchange_rounds):
        incumbent = autocorrelation
        solved, solve_report = _solve_round(
            spec, grids, autocorrelation, selected, verbose
        )
        solved_key = _feasibility_key(solved, spec)
        incumbent_key = _feasibility_key(incumbent, spec)
        if solved_key <= incumbent_key:
            autocorrelation = solved
            solve_report["iterate_accepted"] = True
        else:
            autocorrelation = incumbent
            solve_report["iterate_accepted"] = False
            solve_report["rejection_reason"] = (
                "dense-grid feasibility/lexicographic ordering regressed"
            )
            solve_report["solver_iterate_key"] = list(solved_key)
            solve_report["retained_incumbent_key"] = list(incumbent_key)
        grids, verification, added = _add_exchange_points(grids, autocorrelation, spec)
        entry = {
            "round": exchange_round,
            "grid_sizes": {key: int(value.size) for key, value in grids.items()},
            "added_points": added,
            "solve": solve_report,
            "verification": verification,
        }
        history.append(entry)
        np.savez_compressed(
            checkpoint,
            autocorrelation=autocorrelation,
            pass_grid=grids["pass"],
            transition_grid=grids["transition"],
            stop_grid=grids["stop"],
            global_grid=grids["global"],
        )
        report_path.write_text(json.dumps({"history": history}, indent=2) + "\n")
        if added == 0 and _certification_ok(verification, spec):
            break
    else:
        raise RuntimeError("magnitude exchange exhausted without dense-grid certification")

    report = {
        **_report_metadata(spec),
        "solver": selected,
        "history": history,
        "certified": True,
    }
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    return autocorrelation, report
