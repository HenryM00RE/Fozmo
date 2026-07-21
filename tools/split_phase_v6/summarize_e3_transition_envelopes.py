from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path
from typing import Any


IDENTITY = "SplitPhase128kE3-P5-fixed-reference-transition-envelope-audit"
CONTRACT = "transition-envelope-v1-fixed-2ms-rms-0-50ms"


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _load(path: Path) -> dict[str, Any]:
    report = json.loads(path.read_text(encoding="utf-8"))
    if report.get("transition_envelope_version") != CONTRACT:
        raise RuntimeError(f"{path} does not use {CONTRACT}")
    return report


def _stress_channels(report: dict[str, Any]) -> dict[tuple[str, str, str], dict[str, Any]]:
    channels: dict[tuple[str, str, str], dict[str, Any]] = {}
    for cell in report["cells"]:
        if not str(cell["scenario"]).startswith("high_frequency_"):
            continue
        measurements = cell["measurements"]
        if measurements.get("kind") != "high_frequency_stress":
            raise RuntimeError(f"unexpected stress measurement in {cell['scenario']}")
        for channel in measurements["channels"]:
            key = (cell["scenario"], cell["modulator"], channel["channel"])
            if key in channels:
                raise RuntimeError(f"duplicate stress channel {key}")
            channels[key] = channel
    return channels


def _power_ratio_db(candidate: float, reference: float) -> float:
    if candidate <= 0.0 and reference <= 0.0:
        return 0.0
    if reference <= 0.0:
        return 300.0
    if candidate <= 0.0:
        return -300.0
    return 10.0 * math.log10(candidate / reference)


def _candidate_audit(
    name: str,
    path: Path,
    report: dict[str, Any],
    reference_channels: dict[tuple[str, str, str], dict[str, Any]],
) -> dict[str, Any]:
    candidate_channels = _stress_channels(report)
    extra = sorted(candidate_channels.keys() - reference_channels.keys())
    if extra:
        raise RuntimeError(f"{name} has stress channels absent from the reference: {extra}")
    records = []
    for key in sorted(candidate_channels):
        reference = reference_channels[key]
        candidate = candidate_channels[key]
        candidate_envelope = candidate["transition_envelope"]
        reference_envelope = reference["transition_envelope"]
        excess = candidate.get("transition_envelope_excess_vs_reference")
        if excess is None:
            raise RuntimeError(f"{name} has no frozen-reference excess metrics for {key}")
        intervals = []
        for candidate_interval, reference_interval, excess_interval in zip(
            candidate_envelope["intervals"],
            reference_envelope["intervals"],
            excess["intervals"],
            strict=True,
        ):
            if (candidate_interval["start_ms"], candidate_interval["end_ms"]) != (
                reference_interval["start_ms"],
                reference_interval["end_ms"],
            ):
                raise RuntimeError(f"{name} interval mismatch for {key}")
            intervals.append(
                {
                    "start_ms": candidate_interval["start_ms"],
                    "end_ms": candidate_interval["end_ms"],
                    "candidate_residual_rms_dbfs": candidate_interval["residual_rms_dbfs"],
                    "reference_residual_rms_dbfs": reference_interval["residual_rms_dbfs"],
                    "residual_rms_delta_db": (
                        candidate_interval["residual_rms_dbfs"]
                        - reference_interval["residual_rms_dbfs"]
                    ),
                    "residual_energy_delta_db": _power_ratio_db(
                        candidate_interval["residual_energy_linear_seconds"],
                        reference_interval["residual_energy_linear_seconds"],
                    ),
                    "maximum_excess_power_dbfs": excess_interval[
                        "maximum_excess_power_dbfs"
                    ],
                    "integrated_positive_excess_power_linear_seconds": excess_interval[
                        "integrated_positive_excess_power_linear_seconds"
                    ],
                }
            )
        records.append(
            {
                "scenario": key[0],
                "modulator": key[1],
                "channel": key[2],
                "intervals": intervals,
                "total_positive_excess_power_linear_seconds": excess[
                    "total_positive_excess_power_linear_seconds"
                ],
                "restart_rms_delta_db": {
                    window: candidate[f"restart_residual_rms_{window}_dbfs"]
                    - reference[f"restart_residual_rms_{window}_dbfs"]
                    for window in ("1ms", "10ms", "50ms")
                },
                "first_crossing_recovery_delta_ms": (
                    candidate["end_to_end_recovery_time_ms"]
                    - reference["end_to_end_recovery_time_ms"]
                ),
            }
        )
    interval_summary = []
    for interval_index in range(5):
        interval_records = [record["intervals"][interval_index] for record in records]
        interval_summary.append(
            {
                "start_ms": interval_records[0]["start_ms"],
                "end_ms": interval_records[0]["end_ms"],
                "worst_residual_rms_delta_db": max(
                    interval["residual_rms_delta_db"] for interval in interval_records
                ),
                "best_residual_rms_delta_db": min(
                    interval["residual_rms_delta_db"] for interval in interval_records
                ),
                "worst_candidate_residual_rms_dbfs": max(
                    interval["candidate_residual_rms_dbfs"] for interval in interval_records
                ),
                "maximum_integrated_positive_excess_power_linear_seconds": max(
                    interval["integrated_positive_excess_power_linear_seconds"]
                    for interval in interval_records
                ),
            }
        )
    compact_cells = json.loads(json.dumps(report["cells"]))
    for cell in compact_cells:
        measurements = cell.get("measurements", {})
        if measurements.get("kind") != "high_frequency_stress":
            continue
        for channel in measurements["channels"]:
            channel.get("transition_envelope", {}).pop(
                "sliding_mean_square_trace", None
            )
    return {
        "name": name,
        "report_path": str(path),
        "report_sha256": _sha256(path),
        "research_character": report.get("research_character"),
        "hard_failure_count": report["hard_failure_count"],
        "diagnostic_hard_failure_count": report["diagnostic_hard_failure_count"],
        "interval_summary": interval_summary,
        "worst_restart_rms_delta_db": {
            window: max(record["restart_rms_delta_db"][window] for record in records)
            for window in ("1ms", "10ms", "50ms")
        },
        "worst_first_crossing_recovery_delta_ms": max(
            record["first_crossing_recovery_delta_ms"] for record in records
        ),
        "channels": records,
        "compact_exact_cells": compact_cells,
        "provenance": report.get("provenance"),
    }


def build(reference_path: Path, candidates: list[tuple[str, Path]]) -> dict[str, Any]:
    reference = _load(reference_path)
    reference_channels = _stress_channels(reference)
    audits = [
        _candidate_audit(name, path, _load(path), reference_channels)
        for name, path in candidates
    ]
    return {
        "schema_version": 1,
        "identity": IDENTITY,
        "contract": CONTRACT,
        "interpretation": {
            "first_crossing_recovery": "secondary threshold-sensitive diagnostic",
            "optimizer_primary": "linear-power fixed-reference interval envelope",
            "promotion_status": "research only; no E3 candidate promoted",
        },
        "reference": {
            "name": "SplitPhase128kE2v3",
            "path": str(reference_path),
            "sha256": _sha256(reference_path),
            "stress_channel_count": len(reference_channels),
            "transition_envelope_reference": reference.get(
                "transition_envelope_reference"
            ),
        },
        "candidates": audits,
    }


def _candidate(value: str) -> tuple[str, Path]:
    name, separator, raw_path = value.partition("=")
    if not separator or not name or not raw_path:
        raise argparse.ArgumentTypeError("candidate must be NAME=PATH")
    return name, Path(raw_path)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference", type=Path, required=True)
    parser.add_argument("--candidate", type=_candidate, action="append", required=True)
    parser.add_argument("--out", type=Path, required=True)
    arguments = parser.parse_args()
    report = build(arguments.reference, arguments.candidate)
    arguments.out.parent.mkdir(parents=True, exist_ok=True)
    arguments.out.write_bytes((json.dumps(report, indent=2) + "\n").encode("utf-8"))
    print(json.dumps({"out": str(arguments.out), "sha256": _sha256(arguments.out)}, indent=2))


if __name__ == "__main__":
    main()
