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
from .e3_phase_search import (
    CHARACTER_RATE_HZ,
    FFT_LENGTH,
    _cascade_character_and_cleanup,
    _read_f64le,
    _timing_metrics,
)
from .evaluate_e3_packets import PACKET_FREQUENCIES_HZ, _measure_packet


IDENTITY = "SplitPhase128kE3-P9-E2-anchored-packet-safe-feasibility"
PHASE_KNOTS_HZ = np.asarray(
    (
        8_000.0,
        10_000.0,
        12_000.0,
        13_500.0,
        15_000.0,
        16_500.0,
        18_000.0,
        19_500.0,
        20_500.0,
        21_250.0,
        22_050.0,
    ),
    dtype=np.float64,
)
DENSE_PHASE_KNOTS_HZ = np.asarray(
    (
        3_000.0,
        4_000.0,
        5_000.0,
        6_000.0,
        7_000.0,
        8_000.0,
        9_000.0,
        10_000.0,
        11_000.0,
        12_000.0,
        13_000.0,
        14_000.0,
        15_000.0,
        16_000.0,
        17_000.0,
        18_000.0,
        19_000.0,
        20_000.0,
        20_500.0,
        21_000.0,
        21_500.0,
        22_050.0,
    ),
    dtype=np.float64,
)
MAGNITUDE_KNOTS_HZ = np.asarray(
    (15_000.0, 18_000.0, 19_000.0, 20_000.0, 20_500.0, 21_000.0, 21_500.0, 22_050.0),
    dtype=np.float64,
)
PHASE_STEP_RAD = 1.0e-3
MAGNITUDE_STEPS_DB = np.asarray(
    (1.0e-4, 2.0e-4, 5.0e-4, 5.0e-4, 1.0e-2, 5.0e-2, 2.0e-1, 5.0e-1),
    dtype=np.float64,
)
MAGNITUDE_LOWER_DB = np.asarray(
    (-1.0e-4, -1.0e-3, -5.0e-3, -5.0e-3, -1.0, -3.0, -8.0, -12.0),
    dtype=np.float64,
)
MAGNITUDE_UPPER_DB = np.asarray(
    (1.0e-4, 0.0, 0.0, 0.0, 1.0, 3.0, 8.0, 8.0),
    dtype=np.float64,
)
PHASE_BOUND_RAD = 0.10
PACKET_TOLERANCE_DB = 0.10


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _smoothstep5(value: np.ndarray) -> np.ndarray:
    x = np.clip(value, 0.0, 1.0)
    return x**3 * (10.0 + x * (-15.0 + 6.0 * x))


def _compact_basis(frequency_hz: np.ndarray, knots_hz: np.ndarray) -> np.ndarray:
    basis = np.zeros((frequency_hz.size, knots_hz.size), dtype=np.float64)
    for interval in range(knots_hz.size - 1):
        low = knots_hz[interval]
        high = knots_hz[interval + 1]
        active = (frequency_hz >= low) & (frequency_hz <= high)
        smooth = _smoothstep5((frequency_hz[active] - low) / (high - low))
        basis[active, interval] = 1.0 - smooth
        basis[active, interval + 1] = smooth
    return basis


def _realize(
    anchor_spectrum: np.ndarray,
    phase_basis: np.ndarray,
    magnitude_basis: np.ndarray,
    coordinates: np.ndarray,
    support: int,
    anchor_sum: float,
) -> tuple[np.ndarray, dict[str, float]]:
    phase_count = phase_basis.shape[1] - 2
    phase_controls = np.zeros(phase_basis.shape[1], dtype=np.float64)
    phase_controls[1:-1] = coordinates[:phase_count]
    magnitude_controls = coordinates[phase_count:]
    phase_delta = phase_basis @ phase_controls
    magnitude_delta_db = magnitude_basis @ magnitude_controls
    target = (
        anchor_spectrum
        * np.power(10.0, magnitude_delta_db / 20.0)
        * np.exp(1j * phase_delta)
    )
    target[0] = complex(float(target[0].real), 0.0)
    target[-1] = complex(float(target[-1].real), 0.0)
    periodic = np.fft.irfft(target, FFT_LENGTH)
    candidate = periodic[:support].copy()
    omitted = float(np.dot(periodic[support:], periodic[support:])) / max(
        float(np.dot(periodic, periodic)), 1.0e-300
    )
    candidate *= anchor_sum / float(math.fsum(float(value) for value in candidate))
    return candidate, {
        "omitted_periodic_energy_ratio": omitted,
        "maximum_phase_delta_rad": float(np.max(np.abs(phase_delta))),
        "minimum_magnitude_delta_db": float(np.min(magnitude_delta_db)),
        "maximum_magnitude_delta_db": float(np.max(magnitude_delta_db)),
    }


def _measure(
    character: np.ndarray,
    cleanup: np.ndarray,
    baseline_response: np.ndarray,
) -> dict[str, Any]:
    response = _cascade_character_and_cleanup(character, cleanup)
    return {
        "timing": asdict(_timing_metrics(response)),
        "packets": {
            str(int(frequency)): asdict(_measure_packet(response, frequency))
            for frequency in PACKET_FREQUENCIES_HZ
        },
        "frequency": _frequency_metrics(response, baseline_response),
    }


def _vector(measured: dict[str, Any]) -> tuple[np.ndarray, list[str], np.ndarray]:
    values: list[float] = []
    names: list[str] = []
    scales: list[float] = []
    timing_scales = {
        "pre_energy_db_total": 0.10,
        "maximum_pre_lobe_db_peak": 0.25,
        "post_energy_db_total": 0.10,
        "maximum_post_lobe_db_peak": 0.25,
        "main_lobe_width_us": 2.0,
        "step_overshoot_percent": 0.5,
        "step_undershoot_percent": 0.5,
        "decay_120_ms": 0.10,
    }
    for key, scale in timing_scales.items():
        value = measured["timing"][key]
        values.append(0.0 if value is None else float(value))
        names.append(f"timing/{key}")
        scales.append(scale)
    for frequency, packet in measured["packets"].items():
        for key in (
            "onset_pre_echo_energy_db_total",
            "maximum_onset_pre_echo_db_peak",
        ):
            values.append(float(packet[key]))
            names.append(f"packet/{frequency}/{key}")
            scales.append(PACKET_TOLERANCE_DB)
    return np.asarray(values), names, np.asarray(scales)


def _hard_gates(
    measured: dict[str, Any], baseline: dict[str, Any]
) -> tuple[bool, list[str]]:
    failures: list[str] = []
    timing = measured["timing"]
    base_timing = baseline["timing"]
    tolerances = {
        "pre_energy_db_total": 0.02,
        "maximum_pre_lobe_db_peak": 0.05,
        "post_energy_db_total": 0.02,
        "maximum_post_lobe_db_peak": 0.05,
        "main_lobe_width_us": 0.20,
        "step_overshoot_percent": 0.05,
        "step_undershoot_percent": 0.05,
        "decay_120_ms": 0.10,
    }
    for key, tolerance in tolerances.items():
        if timing[key] is None or timing[key] > base_timing[key] + tolerance:
            failures.append(f"timing/{key}")
    for frequency, packet in measured["packets"].items():
        for key in (
            "onset_pre_echo_energy_db_total",
            "maximum_onset_pre_echo_db_peak",
        ):
            if packet[key] > baseline["packets"][frequency][key] + PACKET_TOLERANCE_DB:
                failures.append(f"packet/{frequency}/{key}")
    frequency = measured["frequency"]
    if frequency["maximum_passband_delta_db_0_18khz"] > 1.0e-3:
        failures.append("frequency/passband")
    if frequency["maximum_stopband_db_22k05_nyquist"] > -150.0:
        failures.append("frequency/stopband")
    return not failures, failures


def _meaningful(measured: dict[str, Any], baseline: dict[str, Any]) -> dict[str, Any]:
    timing = measured["timing"]
    base = baseline["timing"]
    improvements = {
        "pre_lobe_2db": timing["maximum_pre_lobe_db_peak"]
        <= base["maximum_pre_lobe_db_peak"] - 2.0,
        "post_lobe_0p25db": timing["maximum_post_lobe_db_peak"]
        <= base["maximum_post_lobe_db_peak"] - 0.25,
        "side_energy_0p10db": (
            timing["pre_energy_db_total"] <= base["pre_energy_db_total"] - 0.10
            or timing["post_energy_db_total"] <= base["post_energy_db_total"] - 0.10
        ),
        "width_2us": timing["main_lobe_width_us"] <= base["main_lobe_width_us"] - 2.0,
        "overshoot_0p5pp": timing["step_overshoot_percent"]
        <= base["step_overshoot_percent"] - 0.5,
        "undershoot_0p5pp": timing["step_undershoot_percent"]
        <= base["step_undershoot_percent"] - 0.5,
    }
    secondary = sum(
        value for key, value in improvements.items() if key != "pre_lobe_2db"
    )
    return {
        "criteria": improvements,
        "secondary_count": secondary,
        "clear_replacement_timing": bool(
            improvements["pre_lobe_2db"] and secondary >= 3
        ),
    }


def _homotopy(
    e2: np.ndarray,
    p6: np.ndarray,
    cleanup: np.ndarray,
    baseline: dict[str, Any],
    baseline_response: np.ndarray,
) -> list[dict[str, Any]]:
    e2_spectrum = np.fft.rfft(e2, FFT_LENGTH)
    magnitude = np.abs(e2_spectrum)
    e2_phase = np.unwrap(np.angle(e2_spectrum))
    phase_delta = np.unwrap(np.angle(np.fft.rfft(p6, FFT_LENGTH))) - e2_phase
    alphas = np.unique(
        np.concatenate((np.asarray([0.0]), np.geomspace(1.0e-5, 1.0, 81)))
    )
    records = []
    e2_sum = float(math.fsum(float(value) for value in e2))
    for alpha in alphas:
        target = magnitude * np.exp(1j * (e2_phase + alpha * phase_delta))
        target[0] = complex(float(target[0].real), 0.0)
        target[-1] = complex(float(target[-1].real), 0.0)
        candidate = np.fft.irfft(target, FFT_LENGTH)[: e2.size]
        candidate *= e2_sum / float(math.fsum(float(value) for value in candidate))
        measured = _measure(candidate, cleanup, baseline_response)
        passes, failures = _hard_gates(measured, baseline)
        records.append(
            {
                "alpha": float(alpha),
                "passes_hard_gates": passes,
                "failures": failures,
                "timing": measured["timing"],
                "packets": measured["packets"],
            }
        )
    return records


def feasibility(
    root: Path, phase_knots_hz: np.ndarray = PHASE_KNOTS_HZ
) -> dict[str, Any]:
    e2_path = root / "assets/filters/split_phase_e2v3/character_full_rate.f64le"
    p6_path = root / "tools/split_phase_v6/baselines/e3-p6d-local-0145.f64le"
    cleanup_path = root / "assets/filters/split_phase_e2v3/cleanup_stage_1.f64le"
    e2 = _read_f64le(e2_path)
    p6 = _read_f64le(p6_path)
    cleanup = _read_f64le(cleanup_path)
    anchor_spectrum = np.fft.rfft(e2, FFT_LENGTH)
    frequency = np.fft.rfftfreq(FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ)
    phase_basis = _compact_basis(frequency, phase_knots_hz)
    magnitude_basis = _compact_basis(frequency, MAGNITUDE_KNOTS_HZ)
    phase_count = phase_knots_hz.size - 2
    coordinate_count = phase_count + MAGNITUDE_KNOTS_HZ.size
    baseline_response = _cascade_character_and_cleanup(e2, cleanup)
    baseline = _measure(e2, cleanup, baseline_response)
    baseline_vector, result_names, result_scales = _vector(baseline)
    steps = np.concatenate((np.full(phase_count, PHASE_STEP_RAD), MAGNITUDE_STEPS_DB))
    jacobian = np.empty((baseline_vector.size, coordinate_count), dtype=np.float64)
    coordinates: list[dict[str, Any]] = []
    e2_sum = float(math.fsum(float(value) for value in e2))
    for index, step in enumerate(steps):
        plus = np.zeros(coordinate_count, dtype=np.float64)
        minus = np.zeros(coordinate_count, dtype=np.float64)
        plus[index] = step
        minus[index] = -step
        plus_character, _ = _realize(
            anchor_spectrum, phase_basis, magnitude_basis, plus, e2.size, e2_sum
        )
        minus_character, _ = _realize(
            anchor_spectrum, phase_basis, magnitude_basis, minus, e2.size, e2_sum
        )
        plus_vector, _, _ = _vector(
            _measure(plus_character, cleanup, baseline_response)
        )
        minus_vector, _, _ = _vector(
            _measure(minus_character, cleanup, baseline_response)
        )
        jacobian[:, index] = (plus_vector - minus_vector) / (2.0 * step)
        coordinates.append(
            {
                "index": index,
                "kind": "phase_rad" if index < phase_count else "magnitude_db",
                "frequency_hz": float(
                    phase_knots_hz[index + 1]
                    if index < phase_count
                    else MAGNITUDE_KNOTS_HZ[index - phase_count]
                ),
                "finite_difference_step": float(step),
            }
        )

    normalized = jacobian / result_scales[:, None]
    singular_values = np.linalg.svd(normalized, compute_uv=False)
    packet_rows = np.asarray(
        [index for index, name in enumerate(result_names) if name.startswith("packet/")]
    )
    timing_rows = {
        name.split("/", 1)[1]: index
        for index, name in enumerate(result_names)
        if name.startswith("timing/")
    }
    lower = np.concatenate((np.full(phase_count, -PHASE_BOUND_RAD), MAGNITUDE_LOWER_DB))
    upper = np.concatenate((np.full(phase_count, PHASE_BOUND_RAD), MAGNITUDE_UPPER_DB))
    packet_budget = np.full(packet_rows.size, PACKET_TOLERANCE_DB * 0.80)
    constraints = optimize.LinearConstraint(
        jacobian[packet_rows], -np.inf, packet_budget
    )
    objectives = (
        "maximum_pre_lobe_db_peak",
        "pre_energy_db_total",
        "maximum_post_lobe_db_peak",
        "post_energy_db_total",
        "main_lobe_width_us",
        "step_overshoot_percent",
        "step_undershoot_percent",
    )
    trials = []
    for objective in objectives:
        result = optimize.milp(
            c=jacobian[timing_rows[objective]],
            bounds=optimize.Bounds(lower, upper),
            constraints=constraints,
            options={"time_limit": 30.0},
        )
        if result.x is None:
            trials.append({"objective": objective, "optimizer_success": False})
            continue
        for scale in (0.05, 0.10, 0.25, 0.50, 0.75, 1.0):
            trial_coordinates = np.asarray(result.x) * scale
            character, structural = _realize(
                anchor_spectrum,
                phase_basis,
                magnitude_basis,
                trial_coordinates,
                e2.size,
                e2_sum,
            )
            measured = _measure(character, cleanup, baseline_response)
            passes, failures = _hard_gates(measured, baseline)
            trials.append(
                {
                    "objective": objective,
                    "optimizer_success": bool(result.success),
                    "scale": scale,
                    "coordinates": trial_coordinates.tolist(),
                    "structural": structural,
                    "passes_hard_gates": passes,
                    "failures": failures,
                    "meaningful": _meaningful(measured, baseline),
                    "timing": measured["timing"],
                    "packets": measured["packets"],
                    "frequency": measured["frequency"],
                }
            )

    homotopy = _homotopy(e2, p6, cleanup, baseline, baseline_response)
    return {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "source": {
            "e2_character": str(e2_path.relative_to(root)).replace("\\", "/"),
            "e2_sha256": _sha256_bytes(e2_path.read_bytes()),
            "p6_character": str(p6_path.relative_to(root)).replace("\\", "/"),
            "p6_sha256": _sha256_bytes(p6_path.read_bytes()),
            "cleanup_sha256": _sha256_bytes(cleanup_path.read_bytes()),
        },
        "contract": {
            "packet_tolerance_db_vs_e2v3": PACKET_TOLERANCE_DB,
            "rejection_floor_db": -150.0,
            "phase_knots_hz": phase_knots_hz.tolist(),
            "magnitude_knots_hz": MAGNITUDE_KNOTS_HZ.tolist(),
            "magnitude_lower_db": MAGNITUDE_LOWER_DB.tolist(),
            "magnitude_upper_db": MAGNITUDE_UPPER_DB.tolist(),
        },
        "baseline": baseline,
        "homotopy": homotopy,
        "coordinates": coordinates,
        "result_names": result_names,
        "result_scales": result_scales.tolist(),
        "jacobian": jacobian.tolist(),
        "normalized_singular_values": singular_values.tolist(),
        "linearized_exact_trials": trials,
        "summary": {
            "homotopy_safe_count": sum(
                record["passes_hard_gates"] for record in homotopy
            ),
            "homotopy_maximum_safe_alpha": max(
                record["alpha"] for record in homotopy if record["passes_hard_gates"]
            ),
            "exact_trial_count": len(trials),
            "exact_safe_trial_count": sum(
                record.get("passes_hard_gates", False) for record in trials
            ),
            "clear_replacement_trial_count": sum(
                record.get("passes_hard_gates", False)
                and record.get("meaningful", {}).get("clear_replacement_timing", False)
                for record in trials
            ),
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Run E2-anchored P9 timing feasibility mapping"
    )
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[2]
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path(__file__).resolve().parent
        / "work-e3-p9-feasibility/e3_p9_feasibility.json",
    )
    parser.add_argument("--phase-grid", choices=("sparse", "dense"), default="sparse")
    arguments = parser.parse_args()
    phase_knots = (
        PHASE_KNOTS_HZ if arguments.phase_grid == "sparse" else DENSE_PHASE_KNOTS_HZ
    )
    report = feasibility(arguments.root.resolve(), phase_knots)
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"output": str(arguments.output), **report["summary"]}, indent=2))


if __name__ == "__main__":
    main()
