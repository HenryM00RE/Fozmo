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

from .e3_p9_feasibility import _meaningful
from .e3_p10_joint_search import (
    HOLDOUT_CYCLES,
    HOLDOUT_FREQUENCIES_HZ,
    IDENTITY as JOINT_IDENTITY,
    _build_context,
    _coordinate_bounds,
    _frequency_contract,
    _holdout,
    _realize,
    _result_vector,
    _static_failures,
    _strict_packet_nullspace,
    _timing_delta,
)
from .e3_p10_packet_contract import (
    PACKET_GATES_DB,
    measure_packet_set,
    packet_contract,
    packet_gate_deltas,
    packet_gate_failures,
)
from .e3_phase_search import _cascade_character_and_cleanup, _timing_metrics
from .evaluate_e3_packets import PACKET_CYCLES, PACKET_FREQUENCIES_HZ, _measure_packet


IDENTITY = "SplitPhase128kE3-P10-holdout-aware-local-phase-refinement"
LOCAL_STEP = 0.01
LOCAL_RADII = np.asarray((0.01, 0.02, 0.05, 0.10, 0.18, 0.30, 0.45, 0.65, 0.85, 1.0))


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _packet_cells() -> tuple[tuple[str, float, float], ...]:
    production = tuple(
        (f"production-{frequency:g}hz-{PACKET_CYCLES:g}cycles", frequency, PACKET_CYCLES)
        for frequency in PACKET_FREQUENCIES_HZ
    )
    holdout = tuple(
        (f"holdout-{frequency:g}hz-{cycles:g}cycles", frequency, cycles)
        for cycles in HOLDOUT_CYCLES
        for frequency in HOLDOUT_FREQUENCIES_HZ
    )
    return production + holdout


def _peak_vector(response: np.ndarray) -> tuple[np.ndarray, list[str]]:
    values = []
    names = []
    for identifier, frequency, cycles in _packet_cells():
        packet = _measure_packet(response, frequency, cycles)
        values.append(packet.maximum_onset_pre_echo_db_peak)
        names.append(identifier)
    return np.asarray(values), names


def _timing_vector(response: np.ndarray) -> tuple[np.ndarray, list[str]]:
    timing = asdict(_timing_metrics(response))
    metrics = (
        "pre_energy_db_total",
        "maximum_pre_lobe_db_peak",
        "post_energy_db_total",
        "maximum_post_lobe_db_peak",
        "main_lobe_width_us",
        "step_overshoot_percent",
        "step_undershoot_percent",
    )
    return np.asarray([timing[metric] for metric in metrics]), list(metrics)


def refine(
    root: Path,
    work_dir: Path,
    joint_report_path: Path,
    sensitivity_path: Path,
    base_identifier: str,
    candidate_count: int,
    static_count: int,
    packet_count: int,
) -> dict[str, Any]:
    context = _build_context(root, "moderate")
    joint = json.loads(joint_report_path.read_text(encoding="utf-8"))
    if joint["identity"] != JOINT_IDENTITY:
        raise RuntimeError("base report is not a P10 joint-search report")
    base = next(
        (
            record
            for record in joint["holdout_finalists"]
            if record["identifier"] == base_identifier
        ),
        None,
    )
    if base is None or not base["passes_holdouts"]:
        raise RuntimeError("holdout refinement requires a holdout-qualified base")
    base_coordinates = np.asarray(base["coordinates"], dtype=np.float64)
    base_character, base_cleanup, _ = _realize(context, base_coordinates)
    base_response = _cascade_character_and_cleanup(base_character, base_cleanup)
    base_timing = asdict(_timing_metrics(base_response))
    base_peaks, peak_names = _peak_vector(base_response)
    e2_peaks, e2_peak_names = _peak_vector(context.baseline_response)
    if e2_peak_names != peak_names:
        raise RuntimeError("E2 and base packet-cell contracts differ")
    base_timing_vector, timing_names = _timing_vector(base_response)

    sensitivity_json = json.loads(sensitivity_path.read_text(encoding="utf-8"))
    e2_vector, result_names, _ = _result_vector(
        {"timing": context.baseline_timing, "packets": context.baseline_packets}
    )
    sensitivity = {
        "baseline_vector": e2_vector,
        "result_names": result_names,
        "jacobian": np.asarray(sensitivity_json["jacobian"], dtype=np.float64),
    }
    lower, upper, coordinate_scales = _coordinate_bounds(context)
    training_nullspace, training_null_report = _strict_packet_nullspace(
        context, sensitivity, coordinate_scales
    )

    peak_jacobian = np.empty(
        (base_peaks.size, training_nullspace.shape[1]), dtype=np.float64
    )
    timing_jacobian = np.empty(
        (base_timing_vector.size, training_nullspace.shape[1]), dtype=np.float64
    )
    for direction in range(training_nullspace.shape[1]):
        peak_vectors = []
        timing_vectors = []
        for sign in (-1.0, 1.0):
            coordinates = base_coordinates + (
                sign
                * LOCAL_STEP
                * coordinate_scales
                * training_nullspace[:, direction]
            )
            character, cleanup, _ = _realize(context, coordinates)
            response = _cascade_character_and_cleanup(character, cleanup)
            peaks, measured_peak_names = _peak_vector(response)
            timing, measured_timing_names = _timing_vector(response)
            if measured_peak_names != peak_names or measured_timing_names != timing_names:
                raise RuntimeError("holdout sensitivity vector changed shape")
            peak_vectors.append(peaks)
            timing_vectors.append(timing)
        peak_jacobian[:, direction] = (
            peak_vectors[1] - peak_vectors[0]
        ) / (2.0 * LOCAL_STEP)
        timing_jacobian[:, direction] = (
            timing_vectors[1] - timing_vectors[0]
        ) / (2.0 * LOCAL_STEP)

    _, peak_singular_values, peak_right = np.linalg.svd(
        peak_jacobian, full_matrices=True
    )
    threshold = max(float(peak_singular_values[0]) * 1.0e-10, 1.0e-12)
    peak_rank = int(np.sum(peak_singular_values > threshold))
    local_nullspace = peak_right[peak_rank:].T
    if local_nullspace.shape[1] == 0:
        raise RuntimeError("holdout packet-peak nullspace is empty")

    timing_index = {name: index for index, name in enumerate(timing_names)}
    objectives = (
        "maximum_pre_lobe_db_peak",
        "maximum_post_lobe_db_peak",
        "pre_energy_db_total",
        "post_energy_db_total",
        "main_lobe_width_us",
        "step_overshoot_percent",
        "step_undershoot_percent",
    )
    anchors = []
    for objective in objectives:
        gradient = timing_jacobian[timing_index[objective]]
        projected = -(local_nullspace @ (local_nullspace.T @ gradient))
        projected /= max(float(np.max(np.abs(projected))), 1.0e-300)
        anchors.append((objective, projected))

    if candidate_count <= 0 or candidate_count & (candidate_count - 1):
        raise ValueError("candidate count must be a positive power of two")
    unit = qmc.Sobol(d=local_nullspace.shape[1], scramble=False).random_base2(
        int(math.log2(candidate_count))
    )
    raw = 2.0 * unit - 1.0
    raw[0] = 0.0
    directions: list[tuple[int, str, float, np.ndarray]] = []
    manual_index = candidate_count
    for name, anchor in anchors:
        for radius in LOCAL_RADII:
            directions.append((manual_index, name, float(radius), anchor.copy()))
            manual_index += 1
    for index, sample in enumerate(raw):
        local = local_nullspace @ sample
        local /= max(float(np.max(np.abs(local))), 1.0e-300)
        name, anchor = anchors[index % len(anchors)]
        bias = (0.0, 0.25, 0.50, 1.0, 2.0)[index % 5]
        local += bias * anchor
        local /= max(float(np.max(np.abs(local))), 1.0e-300)
        directions.append((index, name, float(LOCAL_RADII[index % LOCAL_RADII.size]), local))

    screened = []
    for index, anchor_name, radius, local in directions:
        training_motion = training_nullspace @ local
        coordinates = base_coordinates + radius * coordinate_scales * training_motion
        if np.any(coordinates < lower) or np.any(coordinates > upper):
            continue
        predicted_timing = base_timing_vector + radius * timing_jacobian @ local
        by_name = dict(zip(timing_names, predicted_timing, strict=True))
        if (
            by_name["pre_energy_db_total"] > -4.85
            or by_name["maximum_pre_lobe_db_peak"] > -18.20
            or by_name["post_energy_db_total"] > -2.3738911226100226
            or by_name["maximum_post_lobe_db_peak"] > -7.702214322277805
            or by_name["main_lobe_width_us"] > 68.9430162564111
            or by_name["step_overshoot_percent"] > 10.954176789621346
            or by_name["step_undershoot_percent"] > 10.331889305015538
        ):
            continue
        predicted_peaks = base_peaks + radius * peak_jacobian @ local
        # Every peak is E2-relative in the frozen contract. Use the exact E2
        # references for both production and holdout cells.
        peak_safe = True
        for value, reference in zip(predicted_peaks, e2_peaks, strict=True):
            if value > reference + PACKET_GATES_DB[
                "maximum_onset_pre_echo_db_peak"
            ] - 0.005:
                peak_safe = False
                break
        if not peak_safe:
            continue
        screened.append(
            {
                "screen_index": index,
                "anchor": anchor_name,
                "radius": radius,
                "local_coordinates": local.tolist(),
                "coordinates": coordinates.tolist(),
                "predicted_pre_lobe_delta_db_vs_base": float(
                    radius
                    * timing_jacobian[
                        timing_index["maximum_pre_lobe_db_peak"]
                    ]
                    @ local
                ),
            }
        )
    screened.sort(key=lambda record: record["predicted_pre_lobe_delta_db_vs_base"])

    exact_static = []
    assets: dict[int, tuple[np.ndarray, np.ndarray, np.ndarray]] = {}
    for screen in screened[: min(static_count, len(screened))]:
        coordinates = np.asarray(screen["coordinates"], dtype=np.float64)
        character, cleanup, structural = _realize(context, coordinates)
        response = _cascade_character_and_cleanup(character, cleanup)
        timing = asdict(_timing_metrics(response))
        failures = _static_failures(timing, structural)
        record = {
            **screen,
            "identifier": f"p10h-{screen['screen_index']:05d}",
            "structural": structural,
            "timing": timing,
            "timing_delta_vs_e2v3": _timing_delta(timing, context.baseline_timing),
            "timing_delta_vs_base": _timing_delta(timing, base_timing),
            "passes_static_gates": not failures,
            "static_failures": failures,
        }
        exact_static.append(record)
        if not failures:
            assets[screen["screen_index"]] = (character, cleanup, response)
    static_safe = sorted(
        (record for record in exact_static if record["passes_static_gates"]),
        key=lambda record: record["timing_delta_vs_e2v3"][
            "maximum_pre_lobe_db_peak"
        ],
    )

    e2_measurement = {
        "timing": context.baseline_timing,
        "packets": context.baseline_packets,
    }
    qualified = []
    exact_packets = []
    for record in static_safe[: min(packet_count, len(static_safe))]:
        _, _, response = assets[record["screen_index"]]
        packets = measure_packet_set(response)
        packet_failures = packet_gate_failures(packets, context.baseline_packets)
        holdout_cells, holdout_failures = _holdout(
            response, context.baseline_response
        )
        frequency, frequency_failures = _frequency_contract(
            response, context.baseline_response, "moderate"
        )
        meaningful = _meaningful(
            {"timing": record["timing"], "packets": packets}, e2_measurement
        )
        completed = {
            **record,
            "packets": packets,
            "packet_gated_delta_db_vs_e2v3": packet_gate_deltas(
                packets, context.baseline_packets
            ),
            "packet_failures": packet_failures,
            "holdout_cells": holdout_cells,
            "holdout_failures": holdout_failures,
            "frequency": frequency,
            "frequency_failures": frequency_failures,
            "passes_all_packet_frequency_gates": not packet_failures
            and not holdout_failures
            and not frequency_failures,
            "meaningful": meaningful,
        }
        exact_packets.append(completed)
        if completed["passes_all_packet_frequency_gates"]:
            qualified.append(completed)
    qualified.sort(
        key=lambda record: (
            not record["meaningful"]["clear_replacement_timing"],
            record["timing_delta_vs_e2v3"]["maximum_pre_lobe_db_peak"],
            -record["meaningful"]["secondary_count"],
        )
    )

    finalist_dir = work_dir / "finalists"
    finalist_dir.mkdir(parents=True, exist_ok=True)
    finalists = []
    for record in qualified[:12]:
        character, cleanup, _ = assets[record["screen_index"]]
        character_payload = np.asarray(character, dtype="<f8").tobytes()
        cleanup_payload = np.asarray(cleanup, dtype="<f8").tobytes()
        character_path = finalist_dir / f"{record['identifier']}.character.f64le"
        cleanup_path = finalist_dir / f"{record['identifier']}.cleanup1.f64le"
        character_path.write_bytes(character_payload)
        cleanup_path.write_bytes(cleanup_payload)
        finalists.append(
            {
                **record,
                "clear_replacement_after_holdouts": bool(
                    record["meaningful"]["clear_replacement_timing"]
                ),
                "character_file": str(character_path.relative_to(work_dir)).replace("\\", "/"),
                "character_sha256": _sha256_bytes(character_payload),
                "cleanup_file": str(cleanup_path.relative_to(work_dir)).replace("\\", "/"),
                "cleanup_sha256": _sha256_bytes(cleanup_payload),
            }
        )

    return {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "source": {
            "joint_report": str(joint_report_path),
            "sensitivity": str(sensitivity_path),
            "base_identifier": base_identifier,
            "base_character_sha256": base["character_sha256"],
        },
        "contract": {
            "local_step": LOCAL_STEP,
            "local_radii": LOCAL_RADII.tolist(),
            "candidate_count": candidate_count,
            "static_count": static_count,
            "packet_count": packet_count,
            "packet_cells": [
                {"identifier": name, "frequency_hz": frequency, "cycles": cycles}
                for name, frequency, cycles in _packet_cells()
            ],
            "packet": packet_contract(),
        },
        "geometry": {
            "training_nullspace": training_null_report,
            "local_input_dimensions": int(training_nullspace.shape[1]),
            "holdout_peak_rank": peak_rank,
            "holdout_peak_nullspace_dimensions": int(local_nullspace.shape[1]),
            "holdout_peak_singular_values": peak_singular_values.tolist(),
        },
        "baseline": {
            "e2v3_timing": context.baseline_timing,
            "base_timing": base_timing,
            "base_packet_peaks": dict(zip(peak_names, base_peaks.tolist(), strict=True)),
        },
        "sensitivity": {
            "timing_names": timing_names,
            "timing_jacobian": timing_jacobian.tolist(),
            "packet_peak_names": peak_names,
            "packet_peak_jacobian": peak_jacobian.tolist(),
        },
        "screened_linear_safe_count": len(screened),
        "exact_static": exact_static,
        "exact_packets": exact_packets,
        "qualified": qualified,
        "finalists": finalists,
        "summary": {
            "screened_linear_safe_count": len(screened),
            "exact_static_count": len(exact_static),
            "exact_static_safe_count": len(static_safe),
            "exact_packet_count": len(exact_packets),
            "all_packet_frequency_safe_count": len(qualified),
            "clear_replacement_count": sum(
                record["meaningful"]["clear_replacement_timing"]
                for record in qualified
            ),
            "finalist_count": len(finalists),
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Run holdout-aware local phase refinement around a P10 finalist"
    )
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[2]
    )
    parser.add_argument(
        "--joint-report",
        type=Path,
        default=Path(__file__).resolve().parent
        / "work-e3-p10/phase-moderate-v4/e3_p10_joint_search.json",
    )
    parser.add_argument(
        "--sensitivity",
        type=Path,
        default=Path(__file__).resolve().parent
        / "work-e3-p10/phase-moderate-v4/e3_p10_sensitivity.json",
    )
    parser.add_argument("--base-identifier", default="p10-moderate-05711")
    parser.add_argument(
        "--work-dir",
        type=Path,
        default=Path(__file__).resolve().parent
        / "work-e3-p10/holdout-refine-v1",
    )
    parser.add_argument("--candidate-count", type=int, default=8_192)
    parser.add_argument("--static-count", type=int, default=384)
    parser.add_argument("--packet-count", type=int, default=96)
    arguments = parser.parse_args()
    report = refine(
        arguments.root.resolve(),
        arguments.work_dir.resolve(),
        arguments.joint_report.resolve(),
        arguments.sensitivity.resolve(),
        arguments.base_identifier,
        arguments.candidate_count,
        arguments.static_count,
        arguments.packet_count,
    )
    arguments.work_dir.mkdir(parents=True, exist_ok=True)
    output = arguments.work_dir / "e3_p10_holdout_refine.json"
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"output": str(output), **report["summary"]}, indent=2))


if __name__ == "__main__":
    main()
