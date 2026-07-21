from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

import numpy as np

from .e3_p10_joint_search import HOLDOUT_CYCLES, HOLDOUT_FREQUENCIES_HZ
from .e3_p10_packet_contract import (
    measure_packet_set,
    packet_gate_deltas,
    packet_gate_failures,
)
from .e3_p12_gaussian_phase_search import (
    SEARCH_STATIC_GATES,
    evaluate_exact,
    optimize,
)
from .e3_phase_search import _cascade_character_and_cleanup, _read_f64le
from .evaluate_e3_packets import PACKET_CYCLES, PACKET_FREQUENCIES_HZ


IDENTITY = "SplitPhase128kE3-P17-cross-rate-packet-repair"
SOURCE_FAMILY_REFERENCE_HZ = 44_100.0
SOURCE_FAMILY_HOLDOUT_HZ = 48_000.0


def _load(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def _write_json_lf(path: Path, value: Any) -> None:
    path.write_bytes((json.dumps(value, indent=2) + "\n").encode("utf-8"))


def equivalent_reference_frequency(frequency_hz: float) -> float:
    """Map a 48 kHz-family packet to its identical four-times sample geometry."""
    return float(frequency_hz * SOURCE_FAMILY_REFERENCE_HZ / SOURCE_FAMILY_HOLDOUT_HZ)


def training_packet_cells() -> tuple[tuple[float, float], ...]:
    production = tuple(
        (float(frequency), float(PACKET_CYCLES))
        for frequency in PACKET_FREQUENCIES_HZ
    )
    holdouts = tuple(
        (float(frequency), float(cycles))
        for cycles in HOLDOUT_CYCLES
        for frequency in HOLDOUT_FREQUENCIES_HZ
    )
    cross_rate = tuple(
        (equivalent_reference_frequency(float(frequency)), float(PACKET_CYCLES))
        for frequency in PACKET_FREQUENCIES_HZ
    )
    return production + holdouts + cross_rate


def _selected_seed(report: dict[str, Any]) -> dict[str, Any]:
    records = report["profiles"]
    if not isinstance(records, list):
        records = [records]
    identifier = report["best_qualified_by_profile"]["balanced"]
    return next(record for record in records if record["identifier"] == identifier)


def _validate_cells(
    response: np.ndarray,
    baseline_response: np.ndarray,
    cells: tuple[tuple[float, float], ...],
) -> dict[str, Any]:
    records = []
    failures = []
    for frequency, cycles in cells:
        reference = measure_packet_set(baseline_response, (frequency,), cycles)
        candidate = measure_packet_set(response, (frequency,), cycles)
        cell_failures = packet_gate_failures(candidate, reference)
        identifier = f"{frequency:g}hz-{cycles:g}cycles"
        failures.extend(f"{identifier}/{failure}" for failure in cell_failures)
        records.append(
            {
                "identifier": identifier,
                "frequency_hz_at_44p1_geometry": frequency,
                "physical_frequency_hz_at_48k": float(
                    frequency
                    * SOURCE_FAMILY_HOLDOUT_HZ
                    / SOURCE_FAMILY_REFERENCE_HZ
                ),
                "cycles": cycles,
                "packets": candidate,
                "gated_delta_db_vs_e2v3": packet_gate_deltas(
                    candidate, reference
                ),
                "failures": cell_failures,
            }
        )
    return {"cells": records, "failures": failures, "passes": not failures}


def search(
    root: Path,
    output_dir: Path,
    seed_report_path: Path,
    iterations: int = 3_200,
) -> dict[str, Any]:
    seed_report = _load(seed_report_path)
    seed = _selected_seed(seed_report)
    specifications = tuple(
        tuple(float(value) for value in item)
        for item in seed_report["contract"]["basis_specifications"]
    )
    static_gates = dict(SEARCH_STATIC_GATES)
    static_gates.update(
        {
            "pre_energy_db_total": -5.35,
            "maximum_pre_lobe_db_peak": -25.50,
            "post_energy_db_total": -2.55,
            "maximum_post_lobe_db_peak": -10.30,
            "main_lobe_width_us": 60.80,
            "step_overshoot_percent": 12.40,
            "step_undershoot_percent": 7.70,
            "tail_energy_db_at_4_ms": -120.0,
        }
    )
    report = optimize(
        root,
        output_dir,
        iterations=iterations,
        profile_names=("balanced",),
        initial_controls=np.asarray(seed["controls"], dtype=np.float64),
        basis_specifications=specifications,
        search_static_gates=static_gates,
        candidate_prefix="p17",
        report_filename="e3_p17_cross_rate_packet_repair.json",
        identity=IDENTITY,
        learning_rate=8.0e-5,
        regularization=1.0e-6,
        learning_rate_milestones=(0.5, 0.75, 0.9),
        training_packet_cells=training_packet_cells(),
        restart_excess_target_power_seconds=1.7e-8,
    )

    asset_dir = root / "assets/filters/split_phase_e2v3"
    baseline = _read_f64le(asset_dir / "character_full_rate.f64le")
    cleanup = _read_f64le(asset_dir / "cleanup_stage_1.f64le")
    baseline_response = _cascade_character_and_cleanup(baseline, cleanup)
    cross_rate_cells = training_packet_cells()[-len(PACKET_FREQUENCIES_HZ) :]
    qualified = []
    for record in report["profiles"]:
        exact, character = evaluate_exact(
            root,
            np.asarray(record["controls"], dtype=np.float64),
            basis_specifications=specifications,
        )
        if exact["character_sha256"] != record["character_sha256"]:
            raise RuntimeError("P17 exact validation did not reproduce the candidate")
        response = _cascade_character_and_cleanup(character, cleanup)
        validation = _validate_cells(response, baseline_response, cross_rate_cells)
        record["cross_rate_packet_validation"] = validation
        record["passes_exact_cross_rate_packet_gates"] = validation["passes"]
        if (
            record["passes_exact_static_packet_frequency_gates"]
            and record["passes_exact_restart_excess_gate"]
            and validation["passes"]
        ):
            qualified.append(record)

    report["exact_cross_rate_qualified_count"] = len(qualified)
    report["best_cross_rate_qualified_by_profile"] = {
        "balanced": min(
            qualified,
            key=lambda record: record["exact_objective"],
            default=None,
        )["identifier"]
        if qualified
        else None
    }
    report["cross_rate_contract"] = {
        "source_family_hz": SOURCE_FAMILY_HOLDOUT_HZ,
        "reference_geometry_source_family_hz": SOURCE_FAMILY_REFERENCE_HZ,
        "physical_packet_frequencies_hz": list(PACKET_FREQUENCIES_HZ),
        "mapped_reference_geometry_frequencies_hz": [
            frequency for frequency, _ in cross_rate_cells
        ],
        "exact_validation_required": True,
    }
    _write_json_lf(output_dir / "e3_p17_cross_rate_packet_repair.json", report)
    return report


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Repair the P16 48 kHz-family packet holdout"
    )
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[2]
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(__file__).resolve().parent / "work-e3-p17/cross-rate-repair",
    )
    parser.add_argument(
        "--seed-report",
        type=Path,
        default=Path(__file__).resolve().parent / "baselines/e3-p16-search.json",
    )
    parser.add_argument("--iterations", type=int, default=3_200)
    arguments = parser.parse_args()
    report = search(
        arguments.root.resolve(),
        arguments.output_dir.resolve(),
        arguments.seed_report.resolve(),
        arguments.iterations,
    )
    print(
        json.dumps(
            {
                "output": str(
                    arguments.output_dir.resolve()
                    / "e3_p17_cross_rate_packet_repair.json"
                ),
                "exact_cross_rate_qualified_count": report[
                    "exact_cross_rate_qualified_count"
                ],
                "best_cross_rate_qualified_by_profile": report[
                    "best_cross_rate_qualified_by_profile"
                ],
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
