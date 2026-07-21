from __future__ import annotations

import argparse
import hashlib
import json
from dataclasses import asdict
from pathlib import Path
from typing import Any

from .e3_phase_search import _cascade_character_and_cleanup, _read_f64le
from .evaluate_e3_packets import (
    PACKET_CYCLES,
    PACKET_FREQUENCIES_HZ,
    _measure_packet,
)


IDENTITY = "SplitPhase128kE3-P10-windowed-packet-contract-v1"
PACKET_CONTRACT_VERSION = "p10-windowed-v1"
PACKET_GATES_DB = {
    "maximum_onset_pre_echo_db_peak": 0.10,
    "onset_pre_echo_energy_db_0_0p5ms": 0.10,
    "onset_pre_echo_energy_db_0p5_2ms": 0.25,
    "onset_pre_echo_energy_db_2_8ms": 1.00,
}
PACKET_ABSOLUTE_CEILINGS_DB = {
    "onset_pre_echo_energy_db_0_0p5ms": -48.0,
    "onset_pre_echo_energy_db_0p5_2ms": -60.0,
    "onset_pre_echo_energy_db_2_8ms": -85.0,
}
REPORT_ONLY_METRICS = (
    "onset_pre_echo_energy_db_total",
    "onset_post_decay_energy_db_total",
    "maximum_onset_post_decay_db_peak",
)


def _sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def measure_packet_set(
    response,
    frequencies_hz: tuple[float, ...] = PACKET_FREQUENCIES_HZ,
    cycles: float = PACKET_CYCLES,
) -> dict[str, dict[str, float]]:
    return {
        str(int(frequency) if float(frequency).is_integer() else frequency): asdict(
            _measure_packet(response, frequency, cycles)
        )
        for frequency in frequencies_hz
    }


def packet_gate_deltas(
    packets: dict[str, dict[str, float]],
    reference: dict[str, dict[str, float]],
) -> dict[str, dict[str, float]]:
    return {
        frequency: {
            metric: float(packet[metric] - reference[frequency][metric])
            for metric in PACKET_GATES_DB
        }
        for frequency, packet in packets.items()
    }


def packet_gate_failures(
    packets: dict[str, dict[str, float]],
    reference: dict[str, dict[str, float]],
) -> list[str]:
    return [
        f"packet/{frequency}/{metric}"
        for frequency, packet in packets.items()
        for metric, relative_tolerance in PACKET_GATES_DB.items()
        if packet[metric]
        > max(
            reference[frequency][metric] + relative_tolerance,
            PACKET_ABSOLUTE_CEILINGS_DB.get(metric, -300.0),
        )
        + 1.0e-12
    ]


def packet_contract() -> dict[str, Any]:
    return {
        "version": PACKET_CONTRACT_VERSION,
        "alignment": "principal full-cascade impulse peak and nominal packet onset",
        "source_rate_hz": 44_100,
        "output_rate_hz": 176_400,
        "packet_cycles": PACKET_CYCLES,
        "packet_frequencies_hz": list(PACKET_FREQUENCIES_HZ),
        "pre_onset_windows_ms": {
            "onset_pre_echo_energy_db_0_0p5ms": [0.0, 0.5],
            "onset_pre_echo_energy_db_0p5_2ms": [0.5, 2.0],
            "onset_pre_echo_energy_db_2_8ms": [2.0, 8.0],
        },
        "maximum_allowed_delta_db_vs_e2v3": PACKET_GATES_DB,
        "absolute_energy_ceiling_db_total": PACKET_ABSOLUTE_CEILINGS_DB,
        "gate_rule": (
            "candidate <= max(E2v3 + relative tolerance, absolute ceiling); "
            "metrics without an absolute ceiling remain strictly E2v3-relative"
        ),
        "report_only_metrics": list(REPORT_ONLY_METRICS),
    }


def calibrate(root: Path) -> dict[str, Any]:
    cleanup_path = root / "assets/filters/split_phase_e2v3/cleanup_stage_1.f64le"
    characters = {
        "e2v3": root
        / "assets/filters/split_phase_e2v3/character_full_rate.f64le",
        "p6d-local-0145": root
        / "tools/split_phase_v6/baselines/e3-p6d-local-0145.f64le",
        "p9-best-valid-research": root
        / "tools/split_phase_v6/baselines/e3-p9-best-valid-research.f64le",
    }
    cleanup = _read_f64le(cleanup_path)
    measurements: dict[str, Any] = {}
    for identifier, path in characters.items():
        character = _read_f64le(path)
        response = _cascade_character_and_cleanup(character, cleanup)
        measurements[identifier] = {
            "character": str(path.relative_to(root)).replace("\\", "/"),
            "character_sha256": _sha256_file(path),
            "packets": measure_packet_set(response),
        }

    reference = measurements["e2v3"]["packets"]
    for identifier, measurement in measurements.items():
        deltas = packet_gate_deltas(measurement["packets"], reference)
        failures = packet_gate_failures(measurement["packets"], reference)
        measurement["gated_delta_db_vs_e2v3"] = deltas
        measurement["passes_p10_windowed_packet_gates"] = not failures
        measurement["failures"] = failures

    return {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "contract": packet_contract(),
        "cleanup_stage_1_sha256": _sha256_file(cleanup_path),
        "calibration": measurements,
        "summary": {
            identifier: {
                "passes": measurement["passes_p10_windowed_packet_gates"],
                "failure_count": len(measurement["failures"]),
            }
            for identifier, measurement in measurements.items()
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Freeze and calibrate the E3 P10 onset-window packet contract"
    )
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[2]
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path(__file__).resolve().parent
        / "work-e3-p10/e3_p10_packet_contract.json",
    )
    arguments = parser.parse_args()
    report = calibrate(arguments.root.resolve())
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"output": str(arguments.output), **report["summary"]}, indent=2))


if __name__ == "__main__":
    main()
