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
    FAMILIES,
    HOLDOUT_CYCLES,
    HOLDOUT_FREQUENCIES_HZ,
    IDENTITY as JOINT_IDENTITY,
    _build_context,
    _coordinate_bounds,
    _coordinate_slices,
    _frequency_contract,
    _holdout,
    _realize,
    _result_vector,
    _static_failures,
    _timing_delta,
)
from .e3_p10_packet_contract import (
    measure_packet_set,
    packet_contract,
    packet_gate_deltas,
    packet_gate_failures,
)
from .e3_phase_search import _cascade_character_and_cleanup, _timing_metrics


IDENTITY = "SplitPhase128kE3-P10-local-magnitude-refinement"
RADIAL_SCALES = np.asarray((0.01, 0.02, 0.05, 0.10, 0.18, 0.30, 0.45, 0.65, 0.85, 1.0))


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _screen_controls(
    count: int, lower: np.ndarray, upper: np.ndarray
) -> np.ndarray:
    if count <= 0 or count & (count - 1):
        raise ValueError("candidate count must be a positive power of two")
    unit = qmc.Sobol(d=lower.size, scramble=False).random_base2(
        int(math.log2(count))
    )
    controls = lower + unit * (upper - lower)
    for index in range(count):
        controls[index] *= RADIAL_SCALES[index % RADIAL_SCALES.size]
    controls[0] = 0.0
    return controls


def refine(
    root: Path,
    work_dir: Path,
    joint_report_path: Path,
    sensitivity_path: Path,
    base_identifier: str,
    family: str,
    candidate_count: int,
    static_count: int,
    packet_count: int,
    holdout_count: int,
) -> dict[str, Any]:
    context = _build_context(root, family)
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
        raise RuntimeError("magnitude refinement requires a holdout-qualified base")
    base_coordinates = np.asarray(base["coordinates"], dtype=np.float64)
    base_character, base_cleanup, _ = _realize(context, base_coordinates)
    base_response = _cascade_character_and_cleanup(base_character, base_cleanup)
    base_measurement = {
        "timing": asdict(_timing_metrics(base_response)),
        "packets": measure_packet_set(base_response),
    }
    e2_measurement = {
        "timing": context.baseline_timing,
        "packets": context.baseline_packets,
    }
    base_vector, names, _ = _result_vector(base_measurement)
    e2_vector, e2_names, _ = _result_vector(e2_measurement)
    if names != e2_names:
        raise RuntimeError("P10 result-vector contract changed")

    sensitivity = json.loads(sensitivity_path.read_text(encoding="utf-8"))
    if sensitivity["result_names"] != names:
        raise RuntimeError("sensitivity result-vector contract does not match base")
    jacobian = np.asarray(sensitivity["jacobian"], dtype=np.float64)
    _, magnitude_slice, _ = _coordinate_slices(context)
    magnitude_jacobian = jacobian[:, magnitude_slice]
    lower = context.magnitude_lower
    upper = context.magnitude_upper
    controls = _screen_controls(candidate_count, lower, upper)
    index = {name: offset for offset, name in enumerate(names)}
    screened = []
    for screen_index, magnitude_controls in enumerate(controls):
        predicted_values = base_vector + magnitude_jacobian @ magnitude_controls
        predicted_delta = predicted_values - e2_vector
        by_name = dict(zip(names, predicted_values, strict=True))
        timing_safe = bool(
            by_name["timing/pre_energy_db_total"] <= -4.85
            and by_name["timing/maximum_pre_lobe_db_peak"] <= -18.20
            and by_name["timing/post_energy_db_total"] <= -2.3738911226100226
            and by_name["timing/maximum_post_lobe_db_peak"] <= -7.702214322277805
            and by_name["timing/main_lobe_width_us"] <= 68.9430162564111
            and by_name["timing/step_overshoot_percent"] <= 10.954176789621346
            and by_name["timing/step_undershoot_percent"] <= 10.331889305015538
        )
        if not timing_safe:
            continue
        # Packet derivatives are useful for ordering, but exact nonlinear
        # packet gates decide qualification below.
        worst_predicted_peak_delta = max(
            predicted_delta[offset]
            for name, offset in index.items()
            if name.endswith("maximum_onset_pre_echo_db_peak")
        )
        if worst_predicted_peak_delta > 0.12:
            continue
        screened.append(
            {
                "screen_index": screen_index,
                "magnitude_controls_db": magnitude_controls.tolist(),
                "predicted_pre_lobe_delta_db_vs_e2v3": float(
                    predicted_delta[index["timing/maximum_pre_lobe_db_peak"]]
                ),
                "predicted_secondary_score": float(
                    predicted_delta[index["timing/maximum_post_lobe_db_peak"]] / 0.25
                    + predicted_delta[index["timing/post_energy_db_total"]] / 0.10
                    + predicted_delta[index["timing/main_lobe_width_us"]] / 2.0
                    + predicted_delta[index["timing/step_overshoot_percent"]] / 0.50
                    + predicted_delta[index["timing/step_undershoot_percent"]] / 0.50
                ),
                "predicted_worst_packet_peak_delta_db_vs_e2v3": float(
                    worst_predicted_peak_delta
                ),
            }
        )
    screened.sort(
        key=lambda record: (
            record["predicted_pre_lobe_delta_db_vs_e2v3"],
            record["predicted_secondary_score"],
        )
    )

    exact_static = []
    assets: dict[int, tuple[np.ndarray, np.ndarray, np.ndarray]] = {}
    for screen in screened[: min(static_count, len(screened))]:
        coordinates = base_coordinates.copy()
        coordinates[magnitude_slice] += np.asarray(
            screen["magnitude_controls_db"], dtype=np.float64
        )
        character, cleanup, structural = _realize(context, coordinates)
        response = _cascade_character_and_cleanup(character, cleanup)
        timing = asdict(_timing_metrics(response))
        failures = _static_failures(timing, structural)
        record = {
            **screen,
            "identifier": f"p10m-{family}-{screen['screen_index']:05d}",
            "coordinates": coordinates.tolist(),
            "structural": structural,
            "timing": timing,
            "timing_delta_vs_e2v3": _timing_delta(timing, context.baseline_timing),
            "timing_delta_vs_phase_base": _timing_delta(timing, base_measurement["timing"]),
            "passes_static_gates": not failures,
            "static_failures": failures,
        }
        exact_static.append(record)
        if not failures:
            assets[screen["screen_index"]] = (character, cleanup, response)
    static_safe = sorted(
        (record for record in exact_static if record["passes_static_gates"]),
        key=lambda record: (
            record["timing_delta_vs_e2v3"]["maximum_pre_lobe_db_peak"],
            record["timing_delta_vs_e2v3"]["maximum_post_lobe_db_peak"],
            record["timing_delta_vs_e2v3"]["post_energy_db_total"],
        ),
    )

    exact_packets = []
    for record in static_safe[: min(packet_count, len(static_safe))]:
        _, _, response = assets[record["screen_index"]]
        packets = measure_packet_set(response)
        packet_failures = packet_gate_failures(packets, context.baseline_packets)
        frequency, frequency_failures = _frequency_contract(
            response, context.baseline_response, family
        )
        meaningful = _meaningful(
            {"timing": record["timing"], "packets": packets}, e2_measurement
        )
        exact_packets.append(
            {
                **record,
                "packets": packets,
                "packet_gated_delta_db_vs_e2v3": packet_gate_deltas(
                    packets, context.baseline_packets
                ),
                "packet_failures": packet_failures,
                "frequency": frequency,
                "frequency_failures": frequency_failures,
                "passes_packet_frequency_gates": not packet_failures
                and not frequency_failures,
                "meaningful": meaningful,
            }
        )
    qualified = sorted(
        (record for record in exact_packets if record["passes_packet_frequency_gates"]),
        key=lambda record: (
            not record["meaningful"]["clear_replacement_timing"],
            record["timing_delta_vs_e2v3"]["maximum_pre_lobe_db_peak"],
            -record["meaningful"]["secondary_count"],
        ),
    )

    finalist_dir = work_dir / "finalists"
    finalist_dir.mkdir(parents=True, exist_ok=True)
    holdouts = []
    for record in qualified[: min(holdout_count, len(qualified))]:
        character, cleanup, response = assets[record["screen_index"]]
        cells, failures = _holdout(response, context.baseline_response)
        character_payload = np.asarray(character, dtype="<f8").tobytes()
        cleanup_payload = np.asarray(cleanup, dtype="<f8").tobytes()
        character_path = finalist_dir / f"{record['identifier']}.character.f64le"
        cleanup_path = finalist_dir / f"{record['identifier']}.cleanup1.f64le"
        character_path.write_bytes(character_payload)
        cleanup_path.write_bytes(cleanup_payload)
        holdouts.append(
            {
                **record,
                "holdout_cells": cells,
                "holdout_failures": failures,
                "passes_holdouts": not failures,
                "clear_replacement_after_holdouts": bool(
                    not failures
                    and record["meaningful"]["clear_replacement_timing"]
                    and FAMILIES[family]["production_eligible_family"]
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
            "base_cleanup_sha256": base["cleanup_sha256"],
        },
        "contract": {
            "family": family,
            "production_eligible_family": FAMILIES[family][
                "production_eligible_family"
            ],
            "radial_scales": RADIAL_SCALES.tolist(),
            "candidate_count": candidate_count,
            "static_count": static_count,
            "packet_count": packet_count,
            "holdout_count": holdout_count,
            "holdout_frequencies_hz": HOLDOUT_FREQUENCIES_HZ,
            "holdout_cycles": HOLDOUT_CYCLES,
            "packet": packet_contract(),
        },
        "baseline": {
            "e2v3": e2_measurement,
            "phase_base": base_measurement,
        },
        "screened_linear_safe_count": len(screened),
        "exact_static": exact_static,
        "exact_packets": exact_packets,
        "qualified": qualified,
        "holdout_finalists": holdouts,
        "summary": {
            "screened_linear_safe_count": len(screened),
            "exact_static_count": len(exact_static),
            "exact_static_safe_count": len(static_safe),
            "exact_packet_count": len(exact_packets),
            "packet_frequency_safe_count": len(qualified),
            "clear_replacement_before_holdouts": sum(
                record["meaningful"]["clear_replacement_timing"] for record in qualified
            ),
            "holdout_finalist_count": len(holdouts),
            "clear_replacement_after_holdouts": sum(
                record["clear_replacement_after_holdouts"] for record in holdouts
            ),
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Refine a holdout-qualified P10 phase candidate in magnitude"
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
    parser.add_argument("--family", choices=tuple(FAMILIES), default="moderate")
    parser.add_argument(
        "--work-dir",
        type=Path,
        default=Path(__file__).resolve().parent
        / "work-e3-p10/magnitude-moderate-v1",
    )
    parser.add_argument("--candidate-count", type=int, default=8_192)
    parser.add_argument("--static-count", type=int, default=256)
    parser.add_argument("--packet-count", type=int, default=96)
    parser.add_argument("--holdout-count", type=int, default=12)
    arguments = parser.parse_args()
    report = refine(
        arguments.root.resolve(),
        arguments.work_dir.resolve(),
        arguments.joint_report.resolve(),
        arguments.sensitivity.resolve(),
        arguments.base_identifier,
        arguments.family,
        arguments.candidate_count,
        arguments.static_count,
        arguments.packet_count,
        arguments.holdout_count,
    )
    arguments.work_dir.mkdir(parents=True, exist_ok=True)
    output = arguments.work_dir / "e3_p10_magnitude_refine.json"
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"output": str(output), **report["summary"]}, indent=2))


if __name__ == "__main__":
    main()
