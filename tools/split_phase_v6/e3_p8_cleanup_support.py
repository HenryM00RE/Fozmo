from __future__ import annotations

import argparse
import hashlib
import json
import math
from dataclasses import asdict
from pathlib import Path
from typing import Any

import numpy as np

from tools.split_phase_v4.cleanup_socp import optimize_cleanup

from .e3_p7_cleanup_search import _frequency_metrics
from .e3_p7_counterfactual import (
    cleanup_counterfactual_residual,
    default_training_fixtures,
    fixture_contract,
    interval_metrics,
)
from .e3_phase_search import _cascade_character_and_cleanup, _read_f64le, _timing_metrics
from .evaluate_e3_packets import PACKET_FREQUENCIES_HZ, _measure_packet


IDENTITY = "SplitPhase128kE3-P8-cleanup-stage-1-support-search"
SUPPORTS = (765, 1_021)
TRUST_RADII = (2.0e-5, 5.0e-5, 1.0e-4, 2.0e-4)


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _embed_centered(source: np.ndarray, support: int) -> np.ndarray:
    if support < source.size or support % 4 != 1 or (support - source.size) % 2:
        raise ValueError("cleanup support must be a centred 4m+1 expansion")
    result = np.zeros(support, dtype=np.float64)
    offset = (support - source.size) // 2
    result[offset : offset + source.size] = source
    return result


def _measure(
    character: np.ndarray,
    cleanup: np.ndarray,
    baseline_response: np.ndarray,
    baseline_timing: dict[str, Any],
    baseline_packets: dict[str, Any],
    baseline_counterfactual: dict[str, Any],
    fixtures: tuple,
) -> dict[str, Any]:
    response = _cascade_character_and_cleanup(character, cleanup)
    timing = asdict(_timing_metrics(response))
    timing_delta = {
        key: timing[key] - baseline_timing[key]
        for key in timing
        if timing[key] is not None and baseline_timing[key] is not None
    }
    packets = {
        str(int(frequency)): asdict(_measure_packet(response, frequency))
        for frequency in PACKET_FREQUENCIES_HZ
    }
    packet_delta = {
        frequency: packets[frequency]["onset_pre_echo_energy_db_total"]
        - baseline_packets[frequency]["onset_pre_echo_energy_db_total"]
        for frequency in packets
    }
    counterfactual = {}
    primary_deltas = []
    for fixture in fixtures:
        intervals = interval_metrics(
            cleanup_counterfactual_residual(character, cleanup, fixture)
        )
        for measured, baseline in zip(
            intervals, baseline_counterfactual[fixture.name], strict=True
        ):
            measured["delta_db_vs_p6"] = (
                measured["residual_rms_dbfs"] - baseline["residual_rms_dbfs"]
            )
        primary_deltas.extend(interval["delta_db_vs_p6"] for interval in intervals[:2])
        counterfactual[fixture.name] = intervals
    frequency = _frequency_metrics(response, baseline_response)
    return {
        "cleanup_sha256": _sha256_bytes(np.asarray(cleanup, dtype="<f8").tobytes()),
        "timing": timing,
        "timing_delta_vs_p6": timing_delta,
        "packets": packets,
        "packet_delta_db_vs_p6": packet_delta,
        "counterfactual": counterfactual,
        "worst_primary_counterfactual_rms_delta_db_vs_p6": float(max(primary_deltas)),
        "best_primary_counterfactual_rms_delta_db_vs_p6": float(min(primary_deltas)),
        "mean_primary_counterfactual_rms_delta_db_vs_p6": float(np.mean(primary_deltas)),
        "frequency": frequency,
    }


def search(root: Path, work_dir: Path) -> dict[str, Any]:
    character_path = root / "tools/split_phase_v6/baselines/e3-p6d-local-0145.f64le"
    cleanup_path = root / "assets/filters/split_phase_e2v3/cleanup_stage_1.f64le"
    character = _read_f64le(character_path)
    cleanup = _read_f64le(cleanup_path)
    baseline_response = _cascade_character_and_cleanup(character, cleanup)
    baseline_timing = asdict(_timing_metrics(baseline_response))
    baseline_packets = {
        str(int(frequency)): asdict(_measure_packet(baseline_response, frequency))
        for frequency in PACKET_FREQUENCIES_HZ
    }
    fixtures = default_training_fixtures()
    baseline_counterfactual = {
        fixture.name: interval_metrics(
            cleanup_counterfactual_residual(character, cleanup, fixture)
        )
        for fixture in fixtures
    }
    baseline_frequency = _frequency_metrics(baseline_response, baseline_response)
    records: list[dict[str, Any]] = []
    candidates: dict[str, np.ndarray] = {}
    for support in SUPPORTS:
        initial = _embed_centered(cleanup, support)
        for radius in TRUST_RADII:
            candidate, solver = optimize_cleanup(
                initial,
                stage=1,
                pass_edge=0.465,
                stop_edge=0.535,
                trust_radius=radius,
            )
            measured = _measure(
                character,
                candidate,
                baseline_response,
                baseline_timing,
                baseline_packets,
                baseline_counterfactual,
                fixtures,
            )
            timing = measured["timing"]
            frequency = measured["frequency"]
            passes = bool(
                timing["maximum_pre_lobe_db_peak"] <= -22.5
                and timing["pre_energy_db_total"] <= -4.85
                and timing["maximum_post_lobe_db_peak"] <= -8.6
                and timing["main_lobe_width_us"] <= 62.5
                and timing["step_overshoot_percent"] <= 9.22
                and timing["decay_120_ms"] is not None
                and timing["decay_120_ms"] <= 7.0
                and max(measured["packet_delta_db_vs_p6"].values()) <= 0.10
                and frequency["maximum_passband_delta_db_0_18khz"] <= 1.0e-4
                and frequency["maximum_stopband_db_22k05_nyquist"] <= -150.0
                and frequency["maximum_transition_rebound_linear"]
                <= baseline_frequency["maximum_transition_rebound_linear"] + 1.0e-15
            )
            delta = measured["timing_delta_vs_p6"]
            meaningful = bool(
                measured["worst_primary_counterfactual_rms_delta_db_vs_p6"] <= -0.03
                or delta["maximum_post_lobe_db_peak"] <= -0.05
                or delta["post_energy_db_total"] <= -0.02
                or delta["main_lobe_width_us"] <= -0.20
                or delta["step_undershoot_percent"] <= -0.05
            )
            identifier = f"cleanup{support}-r{radius:.0e}".replace("+", "")
            record = {
                "identifier": identifier,
                "support": support,
                "trust_radius": radius,
                "solver": solver,
                **measured,
                "passes_hard_gates": passes,
                "passes_minimum_effect_size": meaningful,
            }
            records.append(record)
            candidates[identifier] = candidate
    qualified = [record for record in records if record["passes_hard_gates"]]
    qualified.sort(
        key=lambda record: (
            record["worst_primary_counterfactual_rms_delta_db_vs_p6"],
            record["mean_primary_counterfactual_rms_delta_db_vs_p6"],
            record["timing"]["maximum_post_lobe_db_peak"],
            record["timing"]["post_energy_db_total"],
        )
    )
    finalist_dir = work_dir / "finalists"
    finalist_dir.mkdir(parents=True, exist_ok=True)
    finalists = qualified[:4]
    for record in finalists:
        payload = np.asarray(candidates[record["identifier"]], dtype="<f8").tobytes()
        path = finalist_dir / f"{record['identifier']}.f64le"
        path.write_bytes(payload)
        record["cleanup_file"] = str(path.relative_to(work_dir)).replace("\\", "/")
    return {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "source": {
            "character": str(character_path.relative_to(root)).replace("\\", "/"),
            "character_sha256": _sha256_bytes(character_path.read_bytes()),
            "cleanup": str(cleanup_path.relative_to(root)).replace("\\", "/"),
            "cleanup_sha256": _sha256_bytes(cleanup_path.read_bytes()),
        },
        "contracts": {
            "supports": SUPPORTS,
            "trust_radii": TRUST_RADII,
            "pass_edge_normalized_to_nyquist": 0.465,
            "stop_edge_normalized_to_nyquist": 0.535,
            "fixtures": [fixture_contract(fixture) for fixture in fixtures],
        },
        "baseline": {
            "timing": baseline_timing,
            "packets": baseline_packets,
            "counterfactual": baseline_counterfactual,
            "frequency": baseline_frequency,
        },
        "records": records,
        "qualified_count": len(qualified),
        "meaningful_qualified_count": sum(
            record["passes_minimum_effect_size"] for record in qualified
        ),
        "finalists": finalists,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description="Search longer cleanup-stage-1 supports for E3 P8")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument(
        "--work-dir",
        type=Path,
        default=Path(__file__).resolve().parent / "work-e3-p8-cleanup-support",
    )
    arguments = parser.parse_args()
    report = search(arguments.root.resolve(), arguments.work_dir.resolve())
    output = arguments.work_dir / "e3_p8_cleanup_support.json"
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({
        "output": str(output),
        "candidate_count": len(report["records"]),
        "qualified_count": report["qualified_count"],
        "meaningful_qualified_count": report["meaningful_qualified_count"],
        "finalists": [
            {
                "identifier": record["identifier"],
                "worst_restart_delta_db": record["worst_primary_counterfactual_rms_delta_db_vs_p6"],
                "post_lobe_delta_db": record["timing_delta_vs_p6"]["maximum_post_lobe_db_peak"],
                "post_energy_delta_db": record["timing_delta_vs_p6"]["post_energy_db_total"],
            }
            for record in report["finalists"]
        ],
    }, indent=2))


if __name__ == "__main__":
    main()
