from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any

import numpy as np

from .e3_p10_joint_search import CLEANUP_BOUND


IDENTITY = "SplitPhase128kE3-P10-definitive-research-freeze"
SELECTED_IDENTIFIER = "p10-moderate-05711"


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _sha256_file(path: Path) -> str:
    return _sha256_bytes(path.read_bytes())


def _write_text_lf(path: Path, text: str) -> None:
    """Write immutable reports with platform-independent LF line endings."""
    path.write_bytes(text.encode("utf-8"))


def _load(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def _best_holdout(report: dict[str, Any]) -> dict[str, Any] | None:
    qualified = [
        record for record in report["holdout_finalists"] if record["passes_holdouts"]
    ]
    return min(
        qualified,
        key=lambda record: record["timing_delta_vs_e2v3"][
            "maximum_pre_lobe_db_peak"
        ],
        default=None,
    )


def freeze(root: Path, output_dir: Path) -> dict[str, Any]:
    work = root / "tools/split_phase_v6/work-e3-p10"
    paths = {
        "packet_contract": work / "e3_p10_packet_contract.json",
        "phase_search": work
        / "phase-moderate-v4/e3_p10_joint_search.json",
        "sensitivity": work / "phase-moderate-v4/e3_p10_sensitivity.json",
        "magnitude_moderate": work
        / "magnitude-moderate-v1/e3_p10_magnitude_refine.json",
        "magnitude_aggressive": work
        / "magnitude-aggressive-diagnostic/e3_p10_magnitude_refine.json",
        "holdout_refine": work
        / "holdout-refine-v1/e3_p10_holdout_refine.json",
        "native_timing": work / "native-05711/report.json",
        "native_group_delay": work / "native-05711/group-delay.csv",
        "selected_character": work
        / "phase-moderate-v4/finalists"
        / f"{SELECTED_IDENTIFIER}.character.f64le",
    }
    missing = [str(path) for path in paths.values() if not path.is_file()]
    if missing:
        raise RuntimeError(f"missing P10 freeze inputs: {missing}")

    packet = _load(paths["packet_contract"])
    phase = _load(paths["phase_search"])
    sensitivity = _load(paths["sensitivity"])
    moderate = _load(paths["magnitude_moderate"])
    aggressive = _load(paths["magnitude_aggressive"])
    holdout = _load(paths["holdout_refine"])
    native = _load(paths["native_timing"])
    selected = next(
        record
        for record in phase["holdout_finalists"]
        if record["identifier"] == SELECTED_IDENTIFIER
    )
    if not selected["passes_holdouts"]:
        raise RuntimeError("selected P10 candidate no longer passes holdouts")
    selected_hash = _sha256_file(paths["selected_character"])
    if selected_hash != selected["character_sha256"]:
        raise RuntimeError("selected P10 character hash no longer matches report")

    jacobian = np.asarray(sensitivity["jacobian"], dtype=np.float64)
    cleanup_columns = np.asarray(
        [
            coordinate["index"]
            for coordinate in sensitivity["coordinates"]
            if coordinate["kind"] == "cleanup_halfband"
        ],
        dtype=np.int64,
    )
    result_names = sensitivity["result_names"]
    timing_rows = {
        name.split("/", 1)[1]: index
        for index, name in enumerate(result_names)
        if name.startswith("timing/")
    }
    cleanup_upper_bounds = {
        metric: float(np.sum(np.abs(jacobian[row, cleanup_columns])) * CLEANUP_BOUND)
        for metric, row in timing_rows.items()
    }

    holdout_timing_jacobian = np.asarray(
        holdout["sensitivity"]["timing_jacobian"], dtype=np.float64
    )
    holdout_peak_jacobian = np.asarray(
        holdout["sensitivity"]["packet_peak_jacobian"], dtype=np.float64
    )
    _, _, peak_right = np.linalg.svd(holdout_peak_jacobian, full_matrices=True)
    peak_rank = int(holdout["geometry"]["holdout_peak_rank"])
    residual_nullspace = peak_right[peak_rank:].T
    projected_timing_gradient_norms = {
        name: float(np.linalg.norm(residual_nullspace.T @ holdout_timing_jacobian[index]))
        for index, name in enumerate(holdout["sensitivity"]["timing_names"])
    }
    exact_local_pre_movements = [
        record["timing_delta_vs_base"]["maximum_pre_lobe_db_peak"]
        for record in holdout["exact_static"]
    ]

    moderate_best = _best_holdout(moderate)
    aggressive_best = _best_holdout(aggressive)
    native_by_filter = {record["filter"]: record for record in native["filters"]}
    source_files = (
        "src/audio/dsp/timing.rs",
        "src/bin/filter_timing_bench.rs",
        "tools/split_phase_v6/evaluate_e3_packets.py",
        "tools/split_phase_v6/e3_p10_packet_contract.py",
        "tools/split_phase_v6/e3_p10_joint_search.py",
        "tools/split_phase_v6/e3_p10_magnitude_refine.py",
        "tools/split_phase_v6/e3_p10_holdout_refine.py",
        "tools/split_phase_v6/freeze_e3_p10.py",
    )
    report = {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "selected_research_candidate": {
            "identifier": SELECTED_IDENTIFIER,
            "character_sha256": selected_hash,
            "cleanup_stage_1_sha256": selected["cleanup_sha256"],
            "timing": selected["timing"],
            "timing_delta_vs_e2v3": selected["timing_delta_vs_e2v3"],
            "structural": selected["structural"],
            "packets": selected["packets"],
            "packet_gated_delta_db_vs_e2v3": selected[
                "packet_gated_delta_db_vs_e2v3"
            ],
            "frequency": selected["frequency"],
            "passes_production_packets": selected[
                "passes_packet_frequency_gates"
            ],
            "passes_holdouts": selected["passes_holdouts"],
            "meaningful": selected["meaningful"],
        },
        "packet_contract": {
            "contract": packet["contract"],
            "calibration_summary": packet["summary"],
            "p6_failure_count": len(
                packet["calibration"]["p6d-local-0145"]["failures"]
            ),
        },
        "campaign": {
            "phase_search": phase["summary"],
            "moderate_magnitude": {
                "summary": moderate["summary"],
                "best_holdout_safe_timing_delta_vs_e2v3": (
                    moderate_best["timing_delta_vs_e2v3"]
                    if moderate_best is not None
                    else None
                ),
            },
            "aggressive_magnitude_diagnostic": {
                "summary": aggressive["summary"],
                "production_eligible": False,
                "best_holdout_safe_timing_delta_vs_e2v3": (
                    aggressive_best["timing_delta_vs_e2v3"]
                    if aggressive_best is not None
                    else None
                ),
            },
            "holdout_aware_refinement": {
                "summary": holdout["summary"],
                "input_dimensions": holdout["geometry"]["local_input_dimensions"],
                "packet_peak_rank": peak_rank,
                "residual_dimensions": holdout["geometry"][
                    "holdout_peak_nullspace_dimensions"
                ],
                "projected_timing_gradient_norms": projected_timing_gradient_norms,
                "exact_pre_lobe_delta_vs_base_min_db": float(
                    min(exact_local_pre_movements)
                ),
                "exact_pre_lobe_delta_vs_base_max_db": float(
                    max(exact_local_pre_movements)
                ),
            },
            "cleanup_legal_box_linear_upper_bounds": {
                "coefficient_bound": CLEANUP_BOUND,
                "timing": cleanup_upper_bounds,
            },
        },
        "native_validation": {
            "configuration": native["configuration"],
            "e2v3": native_by_filter["SplitPhase128kE2v3"],
            "selected": native_by_filter["SplitPhase128kE3"],
            "report_sha256": _sha256_file(paths["native_timing"]),
            "group_delay_sha256": _sha256_file(paths["native_group_delay"]),
        },
        "decision": {
            "clear_replacement_found": False,
            "reason": (
                "The best production- and holdout-packet-safe candidate improves "
                "maximum pre-lobe by 0.3197 dB, below the frozen 2 dB effect size. "
                "Once all 26 packet peaks are held fixed, the residual phase "
                "subspace is timing-null at practical precision."
            ),
            "dsd_and_realtime_promotion_gates_run": False,
            "dsd_and_realtime_skip_reason": (
                "Conditional gates are reserved for a clear static timing replacement."
            ),
            "million_tap_repeat_run": False,
            "million_tap_skip_reason": (
                "P8 and P9 already showed support-invariant timing and rejection; "
                "P10 found no new support-binding edge or omitted-energy evidence."
            ),
            "production_default": "SplitPhase128kE2v3",
            "existing_e3_research_frontier": "p6d-local-0145",
        },
        "source_reports": {
            name: {
                "path": str(path.relative_to(root)).replace("\\", "/"),
                "sha256": _sha256_file(path),
            }
            for name, path in paths.items()
        },
        "source_files": {
            path: _sha256_file(root / path) for path in source_files
        },
    }

    output_dir.mkdir(parents=True, exist_ok=True)
    summary_path = output_dir / "e3-p10-campaign-summary.json"
    contract_path = output_dir / "e3-p10-packet-contract.json"
    sensitivity_path = output_dir / "e3-p10-sensitivity.json"
    native_path = output_dir / "e3-p10-best-safe-native-timing.json"
    group_delay_path = output_dir / "e3-p10-best-safe-group-delay.csv"
    character_path = output_dir / "e3-p10-best-safe-research.f64le"
    _write_text_lf(summary_path, json.dumps(report, indent=2) + "\n")
    _write_text_lf(contract_path, json.dumps(packet, indent=2) + "\n")
    _write_text_lf(sensitivity_path, json.dumps(sensitivity, indent=2) + "\n")
    _write_text_lf(native_path, json.dumps(native, indent=2) + "\n")
    group_delay_path.write_bytes(paths["native_group_delay"].read_bytes())
    character_path.write_bytes(paths["selected_character"].read_bytes())
    print(
        json.dumps(
            {
                "summary": str(summary_path),
                "selected_identifier": SELECTED_IDENTIFIER,
                "selected_character_sha256": selected_hash,
                "clear_replacement_found": False,
            },
            indent=2,
        )
    )
    return report


def main() -> None:
    parser = argparse.ArgumentParser(description="Freeze the definitive E3 P10 package")
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[2]
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(__file__).resolve().parent / "baselines",
    )
    arguments = parser.parse_args()
    freeze(arguments.root.resolve(), arguments.output_dir.resolve())


if __name__ == "__main__":
    main()
