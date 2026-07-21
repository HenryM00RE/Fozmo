from __future__ import annotations

import argparse
import hashlib
import json
import math
from dataclasses import asdict
from pathlib import Path
from typing import Any

import numpy as np
from scipy.stats import qmc

from .e3_p7_counterfactual import (
    cleanup_counterfactual_residual,
    default_training_fixtures,
    fixture_contract,
    interval_metrics,
)
from .e3_p9_feasibility import (
    MAGNITUDE_KNOTS_HZ,
    MAGNITUDE_LOWER_DB,
    MAGNITUDE_UPPER_DB,
    PACKET_TOLERANCE_DB,
    PHASE_BOUND_RAD,
    PHASE_KNOTS_HZ,
    _compact_basis,
    _hard_gates,
    _meaningful,
    _measure,
    _sha256_bytes,
)
from .e3_phase_search import (
    CHARACTER_RATE_HZ,
    _cascade_character_and_cleanup,
    _read_f64le,
    _timing_metrics,
)
from .evaluate_e3_packets import PACKET_FREQUENCIES_HZ, _measure_packet


IDENTITY = "SplitPhase128kE3-P9-packet-nullspace-timing-search"
SUPPORTS = (262_145, 524_289, 1_048_577)
RADII = np.asarray((0.01, 0.02, 0.04, 0.07, 0.10, 0.16, 0.25, 0.40), dtype=np.float64)


def _fft_length_for_support(support: int) -> int:
    return 1 << max(20, (2 * support - 1).bit_length())


def _realize_support(
    e2: np.ndarray,
    coordinates: np.ndarray,
    support: int,
    phase_knots_hz: np.ndarray = PHASE_KNOTS_HZ,
) -> tuple[np.ndarray, dict[str, float]]:
    fft_length = _fft_length_for_support(support)
    spectrum = np.fft.rfft(e2, fft_length)
    frequency = np.fft.rfftfreq(fft_length, 1.0 / CHARACTER_RATE_HZ)
    phase_basis = _compact_basis(frequency, phase_knots_hz)
    magnitude_basis = _compact_basis(frequency, MAGNITUDE_KNOTS_HZ)
    phase_count = phase_knots_hz.size - 2
    phase_controls = np.zeros(phase_knots_hz.size, dtype=np.float64)
    phase_controls[1:-1] = coordinates[:phase_count]
    magnitude_controls = coordinates[phase_count:]
    phase_delta = phase_basis @ phase_controls
    magnitude_delta_db = magnitude_basis @ magnitude_controls
    target = (
        spectrum * np.power(10.0, magnitude_delta_db / 20.0) * np.exp(1j * phase_delta)
    )
    target[0] = complex(float(target[0].real), 0.0)
    target[-1] = complex(float(target[-1].real), 0.0)
    periodic = np.fft.irfft(target, fft_length)
    candidate = periodic[:support].copy()
    total = max(float(np.dot(periodic, periodic)), 1.0e-300)
    omitted = float(np.dot(periodic[support:], periodic[support:])) / total
    candidate *= float(math.fsum(float(value) for value in e2)) / float(
        math.fsum(float(value) for value in candidate)
    )
    edge_count = min(2_048, candidate.size // 4)
    edge = float(
        np.dot(candidate[:edge_count], candidate[:edge_count])
        + np.dot(candidate[-edge_count:], candidate[-edge_count:])
    ) / max(float(np.dot(candidate, candidate)), 1.0e-300)
    return candidate, {
        "fft_length": fft_length,
        "omitted_periodic_energy_ratio": omitted,
        "edge_energy_ratio": edge,
        "maximum_phase_delta_rad": float(np.max(np.abs(phase_delta))),
        "minimum_magnitude_delta_db": float(np.min(magnitude_delta_db)),
        "maximum_magnitude_delta_db": float(np.max(magnitude_delta_db)),
    }


def _coordinate_scales(phase_count: int = PHASE_KNOTS_HZ.size - 2) -> np.ndarray:
    magnitude = np.maximum(np.abs(MAGNITUDE_LOWER_DB), np.abs(MAGNITUDE_UPPER_DB))
    return np.concatenate((np.full(phase_count, PHASE_BOUND_RAD), magnitude))


def _packet_nullspace(feasibility: dict[str, Any]) -> tuple[np.ndarray, dict[str, Any]]:
    jacobian = np.asarray(feasibility["jacobian"], dtype=np.float64)
    names = feasibility["result_names"]
    rows = np.asarray(
        [index for index, name in enumerate(names) if name.startswith("packet/")],
        dtype=np.int64,
    )
    phase_count = len(feasibility["contract"]["phase_knots_hz"]) - 2
    scales = _coordinate_scales(phase_count)
    normalized = jacobian[rows] * scales[None, :]
    _, singular_values, right_vectors = np.linalg.svd(normalized, full_matrices=True)
    threshold = max(float(singular_values[0]) * 1.0e-10, 1.0e-12)
    rank = int(np.sum(singular_values > threshold))
    nullspace = right_vectors[rank:].T
    if nullspace.shape[1] == 0:
        nullspace = right_vectors[-min(4, right_vectors.shape[0]) :].T
    return nullspace, {
        "packet_rows": rows.tolist(),
        "packet_singular_values": singular_values.tolist(),
        "packet_rank": rank,
        "nullspace_dimensions": int(nullspace.shape[1]),
        "coordinate_scales": scales.tolist(),
    }


def _sobol_coordinates(
    nullspace: np.ndarray,
    count: int,
    feasibility: dict[str, Any],
) -> list[dict[str, Any]]:
    if count <= 0 or count & (count - 1):
        raise ValueError("candidate count must be a positive power of two")
    unit = qmc.Sobol(d=nullspace.shape[1], scramble=False).random_base2(
        int(math.log2(count))
    )
    centered = 2.0 * unit - 1.0
    centered[0] = 0.0
    phase_count = len(feasibility["contract"]["phase_knots_hz"]) - 2
    scales = _coordinate_scales(phase_count)
    lower = np.concatenate((np.full(phase_count, -PHASE_BOUND_RAD), MAGNITUDE_LOWER_DB))
    upper = np.concatenate((np.full(phase_count, PHASE_BOUND_RAD), MAGNITUDE_UPPER_DB))
    jacobian = np.asarray(feasibility["jacobian"], dtype=np.float64)
    names = feasibility["result_names"]
    packet_rows = np.asarray(
        [index for index, name in enumerate(names) if name.startswith("packet/")]
    )
    timing_rows = {
        name.split("/", 1)[1]: index
        for index, name in enumerate(names)
        if name.startswith("timing/")
    }
    records = []
    for index, raw in enumerate(centered):
        direction = nullspace @ raw
        norm = float(np.max(np.abs(direction)))
        if norm > 0.0:
            direction /= norm
        radius = float(RADII[index % RADII.size])
        coordinates = direction * radius * scales
        within_bounds = bool(
            np.all(coordinates >= lower) and np.all(coordinates <= upper)
        )
        packet_prediction = jacobian[packet_rows] @ coordinates
        timing_prediction = {
            key: float(jacobian[row] @ coordinates) for key, row in timing_rows.items()
        }
        predicted_packet_safe = bool(np.max(packet_prediction, initial=-np.inf) <= 0.08)
        predicted_timing_safe = bool(
            timing_prediction["pre_energy_db_total"] <= 0.018
            and timing_prediction["maximum_pre_lobe_db_peak"] <= 0.045
            and timing_prediction["post_energy_db_total"] <= 0.018
            and timing_prediction["maximum_post_lobe_db_peak"] <= 0.045
            and timing_prediction["main_lobe_width_us"] <= 0.18
            and timing_prediction["step_overshoot_percent"] <= 0.045
            and timing_prediction["step_undershoot_percent"] <= 0.045
            and timing_prediction["decay_120_ms"] <= 0.09
        )
        records.append(
            {
                "index": index,
                "radius": radius,
                "coordinates": coordinates,
                "within_bounds": within_bounds,
                "predicted_packet_safe": predicted_packet_safe,
                "predicted_timing_safe": predicted_timing_safe,
                "predicted_packet_max_delta_db": float(
                    np.max(packet_prediction, initial=-np.inf)
                ),
                "predicted_timing_delta": timing_prediction,
            }
        )
    safe = [
        record
        for record in records
        if record["within_bounds"]
        and record["predicted_packet_safe"]
        and record["predicted_timing_safe"]
    ]
    safe.sort(
        key=lambda record: (
            record["predicted_timing_delta"]["maximum_pre_lobe_db_peak"],
            record["predicted_timing_delta"]["maximum_post_lobe_db_peak"],
            record["predicted_timing_delta"]["pre_energy_db_total"]
            + record["predicted_timing_delta"]["post_energy_db_total"],
            record["predicted_timing_delta"]["main_lobe_width_us"],
        )
    )
    return safe


def _static_safe(
    timing: dict[str, Any], baseline: dict[str, Any]
) -> tuple[bool, list[str]]:
    failures = []
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
        if timing[key] is None or timing[key] > baseline[key] + tolerance:
            failures.append(key)
    return not failures, failures


def _timing_delta(timing: dict[str, Any], baseline: dict[str, Any]) -> dict[str, Any]:
    return {
        key: (
            None
            if timing[key] is None or baseline[key] is None
            else timing[key] - baseline[key]
        )
        for key in timing
    }


def _packet_measurements(
    response: np.ndarray,
    baseline_packets: dict[str, Any],
) -> tuple[dict[str, Any], bool, list[str]]:
    packets = {
        str(int(frequency)): asdict(_measure_packet(response, frequency))
        for frequency in PACKET_FREQUENCIES_HZ
    }
    failures = []
    for frequency, packet in packets.items():
        for key in (
            "onset_pre_echo_energy_db_total",
            "maximum_onset_pre_echo_db_peak",
        ):
            packet[f"{key}_delta_db_vs_e2v3"] = (
                packet[key] - baseline_packets[frequency][key]
            )
            if packet[f"{key}_delta_db_vs_e2v3"] > PACKET_TOLERANCE_DB:
                failures.append(f"{frequency}/{key}")
    return packets, not failures, failures


def _candidate_rank(record: dict[str, Any]) -> tuple[float, ...]:
    delta = record["timing_delta_vs_e2v3"]
    return (
        delta["maximum_pre_lobe_db_peak"],
        delta["maximum_post_lobe_db_peak"],
        delta["pre_energy_db_total"] + delta["post_energy_db_total"],
        delta["main_lobe_width_us"],
        delta["step_overshoot_percent"] + delta["step_undershoot_percent"],
    )


def search(
    root: Path,
    feasibility_path: Path,
    work_dir: Path,
    candidate_count: int,
    static_count: int,
    packet_count: int,
) -> dict[str, Any]:
    feasibility = json.loads(feasibility_path.read_text(encoding="utf-8"))
    phase_knots_hz = np.asarray(
        feasibility["contract"]["phase_knots_hz"], dtype=np.float64
    )
    e2_path = root / "assets/filters/split_phase_e2v3/character_full_rate.f64le"
    cleanup_path = root / "assets/filters/split_phase_e2v3/cleanup_stage_1.f64le"
    e2 = _read_f64le(e2_path)
    cleanup = _read_f64le(cleanup_path)
    baseline_response = _cascade_character_and_cleanup(e2, cleanup)
    baseline_timing = asdict(_timing_metrics(baseline_response))
    baseline_packets = {
        str(int(frequency)): asdict(_measure_packet(baseline_response, frequency))
        for frequency in PACKET_FREQUENCIES_HZ
    }
    baseline_measurement = {
        "timing": baseline_timing,
        "packets": baseline_packets,
    }
    nullspace, nullspace_contract = _packet_nullspace(feasibility)
    screened = _sobol_coordinates(nullspace, candidate_count, feasibility)
    per_support = []
    finalist_characters: dict[str, np.ndarray] = {}
    for support in SUPPORTS:
        exact_static = []
        static_limit = min(static_count, len(screened))
        for screen in screened[:static_limit]:
            coordinates = np.asarray(screen["coordinates"], dtype=np.float64)
            character, structural = _realize_support(
                e2, coordinates, support, phase_knots_hz
            )
            response = _cascade_character_and_cleanup(character, cleanup)
            timing = asdict(_timing_metrics(response))
            safe, failures = _static_safe(timing, baseline_timing)
            exact_static.append(
                {
                    "identifier": f"p9-s{support}-{screen['index']:05d}",
                    "screen_index": screen["index"],
                    "radius": screen["radius"],
                    "coordinates": coordinates.tolist(),
                    "structural": structural,
                    "timing": timing,
                    "timing_delta_vs_e2v3": _timing_delta(timing, baseline_timing),
                    "passes_static_gates": safe,
                    "static_failures": failures,
                }
            )
        static_safe = sorted(
            (record for record in exact_static if record["passes_static_gates"]),
            key=_candidate_rank,
        )
        exact_packets = []
        for record in static_safe[:packet_count]:
            character, _ = _realize_support(
                e2,
                np.asarray(record["coordinates"], dtype=np.float64),
                support,
                phase_knots_hz,
            )
            response = _cascade_character_and_cleanup(character, cleanup)
            packets, packet_safe, packet_failures = _packet_measurements(
                response, baseline_packets
            )
            measured = {"timing": record["timing"], "packets": packets}
            meaningful = _meaningful(measured, baseline_measurement)
            completed = {
                **record,
                "packets": packets,
                "passes_packet_gates": packet_safe,
                "packet_failures": packet_failures,
                "meaningful": meaningful,
            }
            exact_packets.append(completed)
            if packet_safe:
                finalist_characters[record["identifier"]] = character
        packet_safe_records = sorted(
            (record for record in exact_packets if record["passes_packet_gates"]),
            key=_candidate_rank,
        )
        per_support.append(
            {
                "support": support,
                "fft_length": _fft_length_for_support(support),
                "screened_linear_safe_count": len(screened),
                "exact_static_count": len(exact_static),
                "exact_static_safe_count": len(static_safe),
                "exact_packet_count": len(exact_packets),
                "exact_packet_safe_count": len(packet_safe_records),
                "clear_replacement_count": sum(
                    record["meaningful"]["clear_replacement_timing"]
                    for record in packet_safe_records
                ),
                "exact_static": exact_static,
                "exact_packets": exact_packets,
                "packet_safe": packet_safe_records,
            }
        )

    combined = sorted(
        [record for support in per_support for record in support["packet_safe"]],
        key=_candidate_rank,
    )
    fixtures = default_training_fixtures()
    baseline_counterfactual = {
        fixture.name: interval_metrics(
            cleanup_counterfactual_residual(e2, cleanup, fixture)
        )
        for fixture in fixtures
    }
    finalists = []
    finalist_dir = work_dir / "finalists"
    finalist_dir.mkdir(parents=True, exist_ok=True)
    for record in combined[:12]:
        character = finalist_characters[record["identifier"]]
        fixture_results = {}
        primary_deltas = []
        for fixture in fixtures:
            intervals = interval_metrics(
                cleanup_counterfactual_residual(character, cleanup, fixture)
            )
            for measured, baseline in zip(
                intervals, baseline_counterfactual[fixture.name], strict=True
            ):
                measured["delta_db_vs_e2v3"] = (
                    measured["residual_rms_dbfs"] - baseline["residual_rms_dbfs"]
                )
            primary_deltas.extend(
                interval["delta_db_vs_e2v3"] for interval in intervals[:2]
            )
            fixture_results[fixture.name] = intervals
        payload = np.asarray(character, dtype="<f8").tobytes()
        path = finalist_dir / f"{record['identifier']}.f64le"
        path.write_bytes(payload)
        finalists.append(
            {
                **record,
                "counterfactual": fixture_results,
                "worst_primary_counterfactual_delta_db_vs_e2v3": max(primary_deltas),
                "best_primary_counterfactual_delta_db_vs_e2v3": min(primary_deltas),
                "character_file": str(path.relative_to(work_dir)).replace("\\", "/"),
                "character_sha256": _sha256_bytes(payload),
            }
        )
    return {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "source": {
            "feasibility": (
                str(feasibility_path.relative_to(root)).replace("\\", "/")
                if feasibility_path.is_relative_to(root)
                else str(feasibility_path)
            ),
            "e2_character_sha256": _sha256_bytes(e2_path.read_bytes()),
            "cleanup_sha256": _sha256_bytes(cleanup_path.read_bytes()),
        },
        "contract": {
            "candidate_count": candidate_count,
            "static_count_per_support": static_count,
            "packet_count_per_support": packet_count,
            "supports": SUPPORTS,
            "packet_tolerance_db_vs_e2v3": PACKET_TOLERANCE_DB,
            "rejection_floor_db": -150.0,
            "nullspace": nullspace_contract,
            "fixtures": [fixture_contract(fixture) for fixture in fixtures],
        },
        "baseline": {
            "timing": baseline_timing,
            "packets": baseline_packets,
            "counterfactual": baseline_counterfactual,
        },
        "per_support": per_support,
        "finalists": finalists,
        "summary": {
            "linear_screen_safe_count": len(screened),
            "packet_safe_count": len(combined),
            "clear_replacement_count": sum(
                record["meaningful"]["clear_replacement_timing"] for record in combined
            ),
            "finalist_count": len(finalists),
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Search P9 packet-nullspace timing candidates"
    )
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[2]
    )
    parser.add_argument(
        "--feasibility",
        type=Path,
        default=Path(__file__).resolve().parent
        / "work-e3-p9-feasibility/e3_p9_feasibility.json",
    )
    parser.add_argument(
        "--work-dir",
        type=Path,
        default=Path(__file__).resolve().parent / "work-e3-p9-search",
    )
    parser.add_argument("--candidate-count", type=int, default=8_192)
    parser.add_argument("--static-count", type=int, default=256)
    parser.add_argument("--packet-count", type=int, default=64)
    arguments = parser.parse_args()
    report = search(
        arguments.root.resolve(),
        arguments.feasibility.resolve(),
        arguments.work_dir.resolve(),
        arguments.candidate_count,
        arguments.static_count,
        arguments.packet_count,
    )
    output = arguments.work_dir / "e3_p9_timing_search.json"
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"output": str(output), **report["summary"]}, indent=2))


if __name__ == "__main__":
    main()
