from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
from typing import Any

import numpy as np

from .analytic_group_delay import physical_log_derivatives
from .baseline import _load_c, _metrics
from .character_minimax import matrix_free_lawson, save as save_character
from .certify import _ratio_metrics, _resample_target
from .cleanup_socp import optimize_all, project_cleanup_equalities
from .compare_abcd import compare
from .group_delay_spline import ConstrainedDelaySpline, build_constrained_spline, optimize_coordinates, save as save_spline
from .magnitude_sdp import evaluate_power
from .rational_minimax import optimize_both
from .spectral_factor import factor
from .support_search import search


def _spectrum_for_coordinates(
    model: ConstrainedDelaySpline,
    free: np.ndarray,
    frequency: np.ndarray,
    minimum_delay: np.ndarray,
    minimum_phase: np.ndarray,
    magnitude: np.ndarray,
    origin: int,
) -> tuple[np.ndarray, np.ndarray, dict[str, float]]:
    delay = minimum_delay.copy()
    low = frequency < 3000.0
    transition = (frequency >= 3000.0) & (frequency <= 14000.0)
    _, low_delay = model.coefficients_and_low_delay(free)
    delay[low] = low_delay
    delay[transition] = model.evaluate(frequency[transition], free)
    omega = np.linspace(0.0, np.pi, delay.size)
    phase = np.zeros(delay.size, dtype=np.float64)
    phase[1:] = -np.cumsum(0.5 * (delay[1:] + delay[:-1]) * np.diff(omega))
    join = int(np.searchsorted(frequency, 14_000.0))
    closure_error = float(phase[join] - minimum_phase[join])
    closure_region = (frequency >= 3000.0) & (frequency <= 14000.0)
    closure_t = (np.log(frequency[closure_region]) - math.log(3000.0)) / (math.log(14000.0) - math.log(3000.0))
    closure_shape = closure_t**4 * (35.0 + closure_t * (-84.0 + closure_t * (70.0 - 20.0 * closure_t)))
    phase[closure_region] -= closure_error * closure_shape
    phase[join:] = minimum_phase[join:]
    delay[1:-1] = -np.gradient(phase, omega, edge_order=2)[1:-1]
    target = magnitude * np.exp(1j * (phase - omega * origin))
    target[0] = target[0].real
    target[-1] = target[-1].real
    reliable = (frequency >= 1.0) & (frequency <= 20_000.0)
    slope, curvature = physical_log_derivatives(frequency[reliable], delay[reliable])
    return target, delay, {
        "numerical_phase_closure_error_before_exact_correction_rad": closure_error,
        "target_group_delay_slope_max_abs_samples_per_ln_hz": float(np.max(np.abs(slope))),
        "target_group_delay_curvature_max_abs_samples_per_ln_hz_squared": float(np.max(np.abs(curvature))),
    }


def _temporal_objectives(coefficients: np.ndarray, fft_len: int) -> tuple[float, float]:
    response = np.fft.rfft(coefficients, n=fft_len)
    frequency = np.linspace(0.0, 44_100.0, response.size)
    peak = int(np.argmax(np.abs(coefficients)))
    results = []
    for lo_hz, hi_hz in ((3000.0, 14_000.0), (14_000.0, 20_000.0)):
        band = np.fft.irfft(
            response * ((frequency >= lo_hz) & (frequency <= hi_hz)), n=fft_len
        )[: coefficients.size]
        results.append(
            float(np.dot(band[:peak], band[:peak]))
            / max(float(np.dot(band, band)), 1.0e-300)
        )
    return results[0], results[1]


def _target_from_joint_delay(root: Path, work_dir: Path, fft_len: int) -> tuple[np.ndarray, dict[str, Any], dict[str, Any]]:
    magnitude_data = np.load(work_dir / "magnitude_order_512.npz")
    autocorrelation = magnitude_data["autocorrelation"]
    factor_report = factor(autocorrelation, fft_len, work_dir)
    factor_coefficients = np.load(work_dir / "spectral_factor_coefficients.npy")
    minimum_response = np.fft.rfft(factor_coefficients, n=fft_len)
    weighted = np.fft.rfft(np.arange(factor_coefficients.size, dtype=np.float64) * factor_coefficients, n=fft_len)
    minimum_delay = np.real(weighted / minimum_response)
    frequency = np.linspace(0.0, 44_100.0, minimum_response.size)
    reliable = (frequency >= 1.0) & (frequency <= 20_000.0)
    model = build_constrained_spline(frequency[reliable], minimum_delay[reliable], controls=24, degree=5)
    character_c, _, origin_c, _ = _load_c(root)
    c_response = np.fft.rfft(character_c, n=fft_len)
    c_weighted = np.fft.rfft(np.arange(character_c.size, dtype=np.float64) * character_c, n=fft_len)
    c_delay = np.real(c_weighted / c_response) - origin_c
    prior_frequency = frequency[(frequency >= 3000.0) & (frequency <= 14000.0)]
    prior_delay = c_delay[(frequency >= 3000.0) & (frequency <= 14000.0)]
    free, spline_report = optimize_coordinates(model, prior_frequency, prior_delay, prior_weight=1.0e-3)
    minimum_phase = np.unwrap(np.angle(minimum_response))
    power = evaluate_power(autocorrelation, fft_len)
    magnitude = np.sqrt(np.maximum(power, 0.0))
    origin = origin_c
    target, delay, coordinate_metrics = _spectrum_for_coordinates(
        model, free, frequency, minimum_delay, minimum_phase, magnitude, origin
    )
    spline_report.update({
        "exact_closure_correction": "C3 endpoint-flat transition phase correction",
        "outer_optimization_status": "initial constrained coordinates checkpointed for the outer joint loop",
        **coordinate_metrics,
    })
    save_spline(model, free, spline_report, work_dir)
    np.save(work_dir / "target_group_delay.npy", delay)
    np.save(work_dir / "target_spectrum.npy", target)
    (work_dir / "alignment.json").write_text(json.dumps({"full_rate_origin": origin, "phase0_prepad": 131_072 - (origin + 1) // 2, "phase1_prepad": 131_072 - origin // 2, "decimation_prepad": 262_144 - origin}, indent=2) + "\n")
    state = {
        "model": model,
        "free": free,
        "frequency": frequency,
        "minimum_delay": minimum_delay,
        "minimum_phase": minimum_phase,
        "magnitude": magnitude,
        "origin_c": origin_c,
        "prior_frequency": prior_frequency,
        "prior_delay": prior_delay,
    }
    return target, {"factor": factor_report, "spline": spline_report}, state


def build(root: Path, work_dir: Path, fft_len: int = 1_048_576) -> dict[str, Any]:
    target, target_report, joint_state = _target_from_joint_delay(root, work_dir, fft_len)
    character_c, cleanups_c, origin_c, _ = _load_c(root)
    periodic = np.fft.irfft(target, n=fft_len)
    support_candidate, origin, support_report = search(periodic, target, 262_145, origin_c, work_dir / "support_search.json")
    omega = np.linspace(0.0, np.pi, target.size)
    target = target * np.exp(1j * omega * (origin_c - origin))
    target[0] = target[0].real
    target[-1] = target[-1].real
    np.save(work_dir / "target_spectrum.npy", target)
    (work_dir / "alignment.json").write_text(
        json.dumps(
            {
                "full_rate_origin": origin,
                "phase0_prepad": 131_072 - (origin + 1) // 2,
                "phase1_prepad": 131_072 - origin // 2,
                "decimation_prepad": 262_144 - origin,
            },
            indent=2,
        )
        + "\n"
    )
    initial_candidates = [("c_warm_start", character_c), ("sdp_target_support", support_candidate)]
    optimized_candidates = []
    for label, initial in initial_candidates:
        candidate, report = matrix_free_lawson(initial, target, fft_len, iterations=40)
        optimized_candidates.append((tuple(report["final_score"]), label, candidate, report))
    _, selected_label, character, character_report = min(optimized_candidates, key=lambda value: value[0])
    character_report["selected_initialization"] = selected_label
    character_report["group_delay_free_coordinates_live"] = True
    initial_temporal = _temporal_objectives(character, fft_len)
    initial_score = tuple(character_report["final_score"])
    selected_free = joint_state["free"]
    selected_delay = np.load(work_dir / "target_group_delay.npy")
    selected_spline_report = target_report["spline"]

    def joint_key(
        score: tuple[float, ...],
        temporal: tuple[float, float],
        curvature: float,
    ) -> tuple[float, ...]:
        return (
            score[0],
            score[1],
            score[2],
            score[3],
            score[4],
            temporal[0],
            curvature,
            temporal[1],
            score[6],
            score[7],
        )

    incumbent_joint_key = joint_key(
        initial_score,
        initial_temporal,
        selected_spline_report["target_group_delay_curvature_max_abs_samples_per_ln_hz_squared"],
    )
    group_delay_outer_history = []
    phase_shift = joint_state["origin_c"] - origin
    omega = np.linspace(0.0, np.pi, target.size)
    for prior_weight in (1.0e-2, 1.0e-4, 0.0):
        candidate_free, candidate_spline_report = optimize_coordinates(
            joint_state["model"],
            joint_state["prior_frequency"],
            joint_state["prior_delay"],
            prior_weight=prior_weight,
        )
        raw_target, candidate_delay, coordinate_metrics = _spectrum_for_coordinates(
            joint_state["model"],
            candidate_free,
            joint_state["frequency"],
            joint_state["minimum_delay"],
            joint_state["minimum_phase"],
            joint_state["magnitude"],
            joint_state["origin_c"],
        )
        candidate_target = raw_target * np.exp(1j * omega * phase_shift)
        candidate_target[0] = candidate_target[0].real
        candidate_target[-1] = candidate_target[-1].real
        candidate_character, candidate_character_report = matrix_free_lawson(
            character,
            candidate_target,
            fft_len,
            iterations=12,
            trust_radius=2.0e-4,
        )
        candidate_score = tuple(candidate_character_report["final_score"])
        candidate_temporal = _temporal_objectives(candidate_character, fft_len)
        candidate_key = joint_key(
            candidate_score,
            candidate_temporal,
            coordinate_metrics["target_group_delay_curvature_max_abs_samples_per_ln_hz_squared"],
        )
        accepted = candidate_key < incumbent_joint_key
        group_delay_outer_history.append(
            {
                "c_prior_weight": prior_weight,
                "accepted": accepted,
                "joint_key": list(candidate_key),
                "coordinate_metrics": coordinate_metrics,
                "character_iterations": candidate_character_report["iterations"],
            }
        )
        if accepted:
            incumbent_joint_key = candidate_key
            character = candidate_character
            character_report = candidate_character_report
            character_report["selected_initialization"] = selected_label
            character_report["group_delay_free_coordinates_live"] = True
            target = candidate_target
            selected_free = candidate_free
            selected_delay = candidate_delay
            candidate_spline_report.update(coordinate_metrics)
            selected_spline_report = candidate_spline_report
    selected_spline_report["outer_optimization_status"] = "coordinates participated in the character/FIR block acceptance loop"
    selected_spline_report["outer_history"] = group_delay_outer_history
    character_report["outer_joint_key"] = list(incumbent_joint_key)
    target_report["spline"] = selected_spline_report
    save_spline(joint_state["model"], selected_free, selected_spline_report, work_dir)
    np.save(work_dir / "target_group_delay.npy", selected_delay)
    np.save(work_dir / "target_spectrum.npy", target)
    save_character(character, character_report, work_dir)
    cleanup_values = [stage.canonical for stage in cleanups_c]
    proposed_cleanups, cleanup_report = optimize_all(cleanup_values, work_dir)
    response = np.fft.rfft(character, n=fft_len)
    frequency = np.linspace(0.0, 44_100.0, response.size)
    omega = np.linspace(0.0, np.pi, response.size)
    target_response = _resample_target(target, omega, origin)

    def cleanup_key(values: list[np.ndarray]) -> tuple[tuple[float, ...], dict[str, Any]]:
        metrics = _ratio_metrics(response, target_response, frequency, omega, origin, values, fft_len)
        ripple = metrics["worst_2x_256x_passband_ripple_db"]
        complex_error = metrics["worst_2x_256x_composite_complex_error"]
        hard = max(ripple / 1.0e-7 - 1.0, complex_error / 8.0e-9 - 1.0, 0.0)
        return (hard, complex_error, ripple), metrics

    cleanups = [value.copy() for value in cleanup_values]
    incumbent_key, incumbent_ratio_metrics = cleanup_key(cleanups)
    for stage_index, proposed in enumerate(proposed_cleanups):
        accepted = False
        accepted_alpha = 0.0
        candidate_key = incumbent_key
        candidate_metrics = incumbent_ratio_metrics
        for alpha in (1.0, 0.5, 0.25, 0.125, 0.0625):
            blended = project_cleanup_equalities(
                cleanups[stage_index] + alpha * (proposed - cleanups[stage_index])
            )
            trial = list(cleanups)
            trial[stage_index] = blended
            trial_key, trial_metrics = cleanup_key(trial)
            if trial_key < incumbent_key:
                cleanups = trial
                candidate_key = trial_key
                candidate_metrics = trial_metrics
                accepted = True
                accepted_alpha = alpha
                break
        stage_report = cleanup_report["stages"][stage_index]
        stage_report["isolated_socp_accepted"] = stage_report["accepted"]
        stage_report["complete_cascade_accepted"] = accepted
        stage_report["complete_cascade_step"] = accepted_alpha
        stage_report["complete_cascade_score"] = list(candidate_key)
        stage_report["complete_cascade_ratio_metrics"] = candidate_metrics
        stage_report["accepted"] = accepted
        if accepted:
            incumbent_key = candidate_key
            incumbent_ratio_metrics = candidate_metrics
    cleanup_report["complete_cascade_final_score"] = list(incumbent_key)
    cleanup_report["complete_cascade_ratio_metrics"] = incumbent_ratio_metrics
    np.savez(work_dir / "cleanup_optimized.npz", **{"stage_" + str(index): value for index, value in enumerate(cleanups, start=1)})
    (work_dir / "cleanup_socp.json").write_text(json.dumps(cleanup_report, indent=2) + "\n")
    rational_147_160, rational_160_147, rational_report = optimize_both(root / "assets/filters/split_phase_v3", target, origin, work_dir)
    d_metrics = _metrics(character, [type(cleanups_c[0])(value) for value in cleanups], origin, target)
    comparison = compare(root, d_metrics, character, rational_report)
    comparison["d_metrics"] = d_metrics
    (work_dir / "comparison_abcd.json").write_text(json.dumps(comparison, indent=2) + "\n")
    outer_history = [
        {"block": "group_delay_character_outer_loop", "accepted": any(item["accepted"] for item in group_delay_outer_history), "controls_live": True, "history": group_delay_outer_history},
        {"block": "character_lawson", "accepted": True, "selected_initialization": selected_label},
        {"block": "cleanup_socp", "accepted_stages": sum(item["accepted"] for item in cleanup_report["stages"])},
        {"block": "rational_joint_minimax", "accepted": True},
        {"block": "simultaneous_jax_polish", "accepted": False, "reason": "not run; production acceptance relies on the constrained reduced-coordinate block loop"},
    ]
    summary = {
        "identity": "SplitPhase128kV4",
        "target": target_report,
        "support": support_report,
        "character": character_report,
        "cleanup": cleanup_report,
        "rational": rational_report,
        "comparison": comparison,
        "outer_joint_history": outer_history,
        "best_feasible_incumbent_checkpointed": True,
    }
    (work_dir / "build_report.json").write_text(json.dumps(summary, indent=2) + "\n")
    return summary


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--fft-len", type=int, default=1_048_576)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    print(json.dumps(build(root, arguments.work_dir or root / "tools/split_phase_v4/work", arguments.fft_len), indent=2))
