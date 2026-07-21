from __future__ import annotations

import argparse
import hashlib
import json
import math
from dataclasses import asdict
from pathlib import Path
from typing import Any

import numpy as np
from scipy import optimize

from .e3_p7_cleanup_search import _frequency_metrics
from .e3_p7_counterfactual import (
    cleanup_counterfactual_residual,
    default_training_fixtures,
    fixture_contract,
    interval_metrics,
)
from .e3_phase_search import FFT_LENGTH, CHARACTER_RATE_HZ, _cascade_character_and_cleanup, _read_f64le, _timing_metrics
from .evaluate_e3_packets import PACKET_FREQUENCIES_HZ, _measure_packet


IDENTITY = "SplitPhase128kE3-P7-magnitude-sensitivity-and-feasibility"
CONTROL_FREQUENCIES_HZ = np.asarray(
    (15_000.0, 18_000.0, 19_000.0, 20_000.0, 20_500.0, 21_000.0, 21_500.0, 22_050.0),
    dtype=np.float64,
)
FINITE_DIFFERENCE_STEPS_DB = np.asarray(
    (1.0e-4, 2.0e-4, 5.0e-4, 1.0e-3, 1.0e-2, 5.0e-2, 2.5e-1, 1.0),
    dtype=np.float64,
)
FAMILY_BOUNDS_DB = {
    "neutral": (
        np.asarray((-1.0e-4, -2.0e-4, -1.0e-3, -1.0e-3, -0.5, -2.0, -5.0, -10.0)),
        np.asarray((1.0e-4, 2.0e-4, 1.0e-3, 1.0e-3, 0.5, 2.0, 5.0, 8.0)),
    ),
    "micro_apodized": (
        np.asarray((-1.0e-4, -1.0e-3, -5.0e-3, -5.0e-3, -1.0, -3.0, -8.0, -12.0)),
        np.asarray((1.0e-4, 1.0e-3, 0.0, 0.0, 1.0, 3.0, 8.0, 8.0)),
    ),
}


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _basis(frequency_hz: np.ndarray) -> np.ndarray:
    basis = np.zeros((frequency_hz.size, CONTROL_FREQUENCIES_HZ.size), dtype=np.float64)
    for interval in range(CONTROL_FREQUENCIES_HZ.size - 1):
        low = CONTROL_FREQUENCIES_HZ[interval]
        high = CONTROL_FREQUENCIES_HZ[interval + 1]
        active = (frequency_hz >= low) & (frequency_hz <= high)
        x = np.clip((frequency_hz[active] - low) / (high - low), 0.0, 1.0)
        smooth = x**3 * (10.0 + x * (-15.0 + 6.0 * x))
        basis[active, interval] = 1.0 - smooth
        basis[active, interval + 1] = smooth
    basis[frequency_hz > CONTROL_FREQUENCIES_HZ[-1], -1] = 1.0
    return basis


def _realize_character(
    anchor: np.ndarray,
    anchor_spectrum: np.ndarray,
    magnitude_basis: np.ndarray,
    controls_db: np.ndarray,
    anchor_sum: float,
) -> tuple[np.ndarray, dict[str, float]]:
    delta_db = magnitude_basis @ controls_db
    target = anchor_spectrum * np.power(10.0, delta_db / 20.0)
    target[0] = complex(float(target[0].real), 0.0)
    target[-1] = complex(float(target[-1].real), 0.0)
    periodic = np.fft.irfft(target, FFT_LENGTH)
    candidate = periodic[: anchor.size].copy()
    omitted = float(np.dot(periodic[anchor.size :], periodic[anchor.size :])) / max(
        float(np.dot(periodic, periodic)), 1.0e-300
    )
    candidate *= anchor_sum / float(math.fsum(float(value) for value in candidate))
    realized = np.fft.rfft(candidate, FFT_LENGTH)
    frequency = np.fft.rfftfreq(FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ)
    realized_delta = 20.0 * np.log10(
        np.maximum(np.abs(realized), 1.0e-300)
        / np.maximum(np.abs(anchor_spectrum), 1.0e-300)
    )
    return candidate, {
        "omitted_periodic_energy_ratio": omitted,
        "maximum_realized_delta_db_0_15khz": float(
            np.max(np.abs(realized_delta[frequency <= 15_000.0]))
        ),
        "maximum_realized_delta_db_15_18khz": float(
            np.max(np.abs(realized_delta[(frequency >= 15_000.0) & (frequency <= 18_000.0)]))
        ),
        "maximum_realized_delta_db_18_20khz": float(
            np.max(np.abs(realized_delta[(frequency >= 18_000.0) & (frequency <= 20_000.0)]))
        ),
    }


def _evaluate(
    character: np.ndarray,
    cleanup: np.ndarray,
    baseline_response: np.ndarray,
    fixtures: tuple,
) -> dict[str, Any]:
    response = _cascade_character_and_cleanup(character, cleanup)
    timing = asdict(_timing_metrics(response))
    packets = {
        str(int(frequency)): asdict(_measure_packet(response, frequency))
        for frequency in PACKET_FREQUENCIES_HZ
    }
    counterfactual = {}
    for fixture in fixtures:
        residual = cleanup_counterfactual_residual(character, cleanup, fixture)
        counterfactual[fixture.name] = interval_metrics(residual)
    return {
        "timing": timing,
        "packets": packets,
        "counterfactual": counterfactual,
        "frequency": _frequency_metrics(response, baseline_response),
    }


def _vector(result: dict[str, Any]) -> tuple[np.ndarray, list[str], np.ndarray]:
    values: list[float] = []
    names: list[str] = []
    scales: list[float] = []
    for fixture, intervals in result["counterfactual"].items():
        for interval in intervals[:2]:
            values.append(interval["residual_rms_dbfs"])
            names.append(f"counterfactual/{fixture}/{interval['start_ms']:.0f}-{interval['end_ms']:.0f}ms")
            scales.append(0.03)
    timing_scales = {
        "maximum_pre_lobe_db_peak": 0.05,
        "pre_energy_db_total": 0.02,
        "maximum_post_lobe_db_peak": 0.05,
        "post_energy_db_total": 0.02,
        "main_lobe_width_us": 0.20,
        "step_overshoot_percent": 0.05,
        "step_undershoot_percent": 0.05,
        "decay_120_ms": 0.20,
    }
    for name, scale in timing_scales.items():
        values.append(result["timing"][name])
        names.append(f"timing/{name}")
        scales.append(scale)
    for frequency, packet in result["packets"].items():
        values.append(packet["onset_pre_echo_energy_db_total"])
        names.append(f"packet/{frequency}/onset_pre_echo_energy_db_total")
        scales.append(0.10)
    values.append(result["frequency"]["maximum_stopband_db_22k05_nyquist"])
    names.append("frequency/maximum_stopband_db_22k05_nyquist")
    scales.append(1.0)
    return np.asarray(values), names, np.asarray(scales)


def _linearized_trials(
    baseline: np.ndarray,
    jacobian: np.ndarray,
    names: list[str],
    family: str,
) -> list[dict[str, Any]]:
    lower, upper = FAMILY_BOUNDS_DB[family]
    index = {name: offset for offset, name in enumerate(names)}
    guarded = {
        "timing/maximum_pre_lobe_db_peak": -22.5,
        "timing/pre_energy_db_total": -4.85,
        "timing/main_lobe_width_us": 62.5,
        "timing/step_overshoot_percent": 9.22,
        "timing/decay_120_ms": 7.0,
        "frequency/maximum_stopband_db_22k05_nyquist": -150.0,
    }
    a_ub = []
    b_ub = []
    for name, limit in guarded.items():
        row = index[name]
        a_ub.append(jacobian[row])
        b_ub.append(limit - baseline[row])
    for name, row in index.items():
        if name.startswith("packet/"):
            a_ub.append(jacobian[row])
            b_ub.append(0.10)
    counter_rows = [offset for name, offset in index.items() if name.startswith("counterfactual/")]
    trials = []
    objectives = {
        "transition_mean": np.mean(jacobian[counter_rows], axis=0),
        "post_lobe": jacobian[index["timing/maximum_post_lobe_db_peak"]],
        "post_energy": jacobian[index["timing/post_energy_db_total"]],
        "undershoot": jacobian[index["timing/step_undershoot_percent"]],
        "width": jacobian[index["timing/main_lobe_width_us"]],
    }
    for objective_name, objective in objectives.items():
        result = optimize.linprog(
            objective,
            A_ub=np.asarray(a_ub),
            b_ub=np.asarray(b_ub),
            bounds=list(zip(lower, upper, strict=True)),
            method="highs",
        )
        controls = result.x if result.success else np.zeros_like(lower)
        predicted = jacobian @ controls
        trials.append(
            {
                "identifier": f"{family}-{objective_name}",
                "objective": objective_name,
                "solver_success": bool(result.success),
                "solver_message": str(result.message),
                "controls_db": controls.tolist(),
                "predicted_objective_movement": float(objective @ controls),
                "predicted_worst_counterfactual_movement_db": float(
                    np.max(predicted[counter_rows])
                ),
                "predicted_mean_counterfactual_movement_db": float(
                    np.mean(predicted[counter_rows])
                ),
                "predicted_metrics": {
                    name: float(predicted[offset])
                    for name, offset in index.items()
                    if name.startswith("timing/") or name.startswith("frequency/")
                },
            }
        )
    # Epsilon-constraint minimax trial: minimize the worst linearized movement
    # across every 0-2/2-5 ms training cell instead of trading one fixture for
    # a better mean.
    minimax_a = np.zeros((len(a_ub) + len(counter_rows), lower.size + 1), dtype=np.float64)
    minimax_b = np.empty(minimax_a.shape[0], dtype=np.float64)
    minimax_a[: len(a_ub), : lower.size] = np.asarray(a_ub)
    minimax_b[: len(a_ub)] = np.asarray(b_ub)
    for row_index, counter_row in enumerate(counter_rows, start=len(a_ub)):
        minimax_a[row_index, : lower.size] = jacobian[counter_row]
        minimax_a[row_index, -1] = -1.0
        minimax_b[row_index] = 0.0
    minimax_objective = np.zeros(lower.size + 1, dtype=np.float64)
    minimax_objective[-1] = 1.0
    minimax = optimize.linprog(
        minimax_objective,
        A_ub=minimax_a,
        b_ub=minimax_b,
        bounds=[*zip(lower, upper, strict=True), (None, None)],
        method="highs",
    )
    controls = minimax.x[: lower.size] if minimax.success else np.zeros_like(lower)
    predicted = jacobian @ controls
    trials.append(
        {
            "identifier": f"{family}-transition_minimax",
            "objective": "transition_minimax",
            "solver_success": bool(minimax.success),
            "solver_message": str(minimax.message),
            "controls_db": controls.tolist(),
            "predicted_objective_movement": float(minimax.fun) if minimax.success else 0.0,
            "predicted_worst_counterfactual_movement_db": float(np.max(predicted[counter_rows])),
            "predicted_mean_counterfactual_movement_db": float(np.mean(predicted[counter_rows])),
            "predicted_metrics": {
                name: float(predicted[offset])
                for name, offset in index.items()
                if name.startswith("timing/") or name.startswith("frequency/")
            },
        }
    )
    return trials


def build(root: Path, work_dir: Path) -> dict[str, Any]:
    character_path = root / "tools/split_phase_v6/baselines/e3-p6d-local-0145.f64le"
    cleanup_path = root / "assets/filters/split_phase_e2v3/cleanup_stage_1.f64le"
    character = _read_f64le(character_path)
    cleanup = _read_f64le(cleanup_path)
    anchor_spectrum = np.fft.rfft(character, FFT_LENGTH)
    frequency = np.fft.rfftfreq(FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ)
    magnitude_basis = _basis(frequency)
    anchor_sum = float(math.fsum(float(value) for value in character))
    baseline_response = _cascade_character_and_cleanup(character, cleanup)
    fixtures = default_training_fixtures()
    baseline_result = _evaluate(character, cleanup, baseline_response, fixtures)
    baseline_vector, names, scales = _vector(baseline_result)

    derivatives = np.empty((baseline_vector.size, CONTROL_FREQUENCIES_HZ.size), dtype=np.float64)
    coordinate_reports = []
    for coordinate, step_db in enumerate(FINITE_DIFFERENCE_STEPS_DB):
        evaluations = []
        vectors = []
        for sign in (-1.0, 1.0):
            controls = np.zeros(CONTROL_FREQUENCIES_HZ.size, dtype=np.float64)
            controls[coordinate] = sign * step_db
            candidate, realization = _realize_character(
                character, anchor_spectrum, magnitude_basis, controls, anchor_sum
            )
            measured = _evaluate(candidate, cleanup, baseline_response, fixtures)
            vector, measured_names, _ = _vector(measured)
            if measured_names != names:
                raise RuntimeError("sensitivity result vector changed shape")
            vectors.append(vector)
            evaluations.append(
                {
                    "sign": sign,
                    "controls_db": controls.tolist(),
                    "character_sha256": _sha256_bytes(np.asarray(candidate, dtype="<f8").tobytes()),
                    "realization": realization,
                    "measured_delta": {
                        name: float(value - reference)
                        for name, value, reference in zip(names, vector, baseline_vector, strict=True)
                    },
                }
            )
        derivatives[:, coordinate] = (vectors[1] - vectors[0]) / (2.0 * step_db)
        coordinate_reports.append(
            {
                "control_frequency_hz": float(CONTROL_FREQUENCIES_HZ[coordinate]),
                "finite_difference_step_db": float(step_db),
                "negative": evaluations[0],
                "positive": evaluations[1],
            }
        )

    normalized = derivatives / scales[:, None]
    _, singular_values, right_vectors = np.linalg.svd(normalized, full_matrices=False)
    linearized = []
    for family in FAMILY_BOUNDS_DB:
        linearized.extend(_linearized_trials(baseline_vector, derivatives, names, family))

    exact_trials = []
    trial_dir = work_dir / "linearized-trials"
    trial_dir.mkdir(parents=True, exist_ok=True)
    for trial in linearized:
        if not trial["solver_success"]:
            continue
        family = trial["identifier"].split("-", 1)[0]
        passband_limit = 2.1e-4 if family == "neutral" else 1.01e-3
        for scale in (0.10, 0.25, 0.50, 0.75, 1.0):
            controls = scale * np.asarray(trial["controls_db"], dtype=np.float64)
            candidate, realization = _realize_character(
                character, anchor_spectrum, magnitude_basis, controls, anchor_sum
            )
            measured = _evaluate(candidate, cleanup, baseline_response, fixtures)
            vector, _, _ = _vector(measured)
            delta = vector - baseline_vector
            counter_rows = [offset for offset, name in enumerate(names) if name.startswith("counterfactual/")]
            packet_deltas = {
                frequency: measured["packets"][frequency]["onset_pre_echo_energy_db_total"]
                - baseline_result["packets"][frequency]["onset_pre_echo_energy_db_total"]
                for frequency in measured["packets"]
            }
            timing = measured["timing"]
            passes = bool(
                timing["maximum_pre_lobe_db_peak"] <= -22.5
                and timing["pre_energy_db_total"] <= -4.85
                and timing["main_lobe_width_us"] <= 62.5
                and timing["step_overshoot_percent"] <= 9.22
                and timing["decay_120_ms"] is not None
                and timing["decay_120_ms"] <= 7.0
                and max(packet_deltas.values()) <= 0.10
                and measured["frequency"]["maximum_passband_delta_db_0_18khz"] <= passband_limit
                and measured["frequency"]["maximum_stopband_db_22k05_nyquist"] <= -150.0
            )
            payload = np.asarray(candidate, dtype="<f8").tobytes()
            identifier = f"{trial['identifier']}-s{scale:.2f}"
            path = trial_dir / f"{identifier}.f64le"
            path.write_bytes(payload)
            exact_trials.append(
                {
                    **trial,
                    "identifier": identifier,
                    "scale": scale,
                    "scaled_controls_db": controls.tolist(),
                    "character_file": str(path.relative_to(work_dir)).replace("\\", "/"),
                    "character_sha256": _sha256_bytes(payload),
                    "realization": realization,
                    "passes_hard_gates": passes,
                    "worst_counterfactual_delta_db": float(np.max(delta[counter_rows])),
                    "mean_counterfactual_delta_db": float(np.mean(delta[counter_rows])),
                    "timing_delta_vs_p6": {
                        key: timing[key] - baseline_result["timing"][key]
                        for key in timing
                        if timing[key] is not None and baseline_result["timing"][key] is not None
                    },
                    "packet_delta_db_vs_p6": packet_deltas,
                    "frequency": measured["frequency"],
                }
            )

    report = {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "source": {
            "character": str(character_path.relative_to(root)).replace("\\", "/"),
            "character_sha256": _sha256_bytes(character_path.read_bytes()),
            "cleanup_stage_1_sha256": _sha256_bytes(cleanup_path.read_bytes()),
        },
        "parameterization": {
            "control_frequencies_hz": CONTROL_FREQUENCIES_HZ.tolist(),
            "finite_difference_steps_db": FINITE_DIFFERENCE_STEPS_DB.tolist(),
            "basis": "compact quintic partition of unity, zero below 15 kHz, final control held above 22.05 kHz",
            "families": {
                name: {"lower_db": lower.tolist(), "upper_db": upper.tolist()}
                for name, (lower, upper) in FAMILY_BOUNDS_DB.items()
            },
        },
        "training_fixtures": [fixture_contract(fixture) for fixture in fixtures],
        "result_names": names,
        "effect_scales": scales.tolist(),
        "baseline": baseline_result,
        "jacobian_per_control_db": derivatives.tolist(),
        "normalized_singular_values": singular_values.tolist(),
        "normalized_right_singular_vectors": right_vectors.tolist(),
        "coordinates": coordinate_reports,
        "linearized_trials": linearized,
        "exact_linearized_trials": exact_trials,
    }
    work_dir.mkdir(parents=True, exist_ok=True)
    (work_dir / "e3_p7_magnitude_sensitivity.json").write_text(
        json.dumps(report, indent=2) + "\n", encoding="utf-8"
    )
    return report


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--work-dir", type=Path)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    work_dir = (
        arguments.work_dir or root / "tools/split_phase_v6/work-e3-p7-magnitude"
    ).resolve()
    report = build(root, work_dir)
    exact = report["exact_linearized_trials"]
    print(
        json.dumps(
            {
                "coordinate_count": len(report["coordinates"]),
                "linearized_trial_count": len(exact),
                "hard_gate_trial_count": sum(trial["passes_hard_gates"] for trial in exact),
                "best_worst_counterfactual_delta_db": min(
                    (trial["worst_counterfactual_delta_db"] for trial in exact), default=None
                ),
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
