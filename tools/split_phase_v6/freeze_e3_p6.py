from __future__ import annotations

import argparse
import hashlib
import json
import math
import shutil
from pathlib import Path
from typing import Any

import numpy as np


IDENTITY = "SplitPhase128kE3-P6-definitive-freeze"
CHARACTER_SHA256 = "da418ad185fdd0317c3046598eb40ec205bd33976b563870011d3f058acd51d5"


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _load(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def _strip_volatile(value: Any) -> Any:
    if isinstance(value, dict):
        return {
            key: _strip_volatile(item)
            for key, item in value.items()
            if key not in {"render_seconds", "executable_sha256"}
        }
    if isinstance(value, list):
        return [_strip_volatile(item) for item in value]
    return value


def _canonical_sha256(value: Any) -> str:
    payload = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def _counterfactual_summary(path: Path) -> dict[str, Any]:
    report = _load(path)
    fitted = report["transition_envelope"]["intervals"]
    counterfactual = report["counterfactual_transition_envelope"]["intervals"]
    return {
        "report_sha256": _sha256(path),
        "schema_version": report["schema_version"],
        "modulator": report["modulator"],
        "source": report["source"],
        "fitted_intervals": fitted,
        "counterfactual_intervals": counterfactual,
        "counterfactual_minus_fitted_rms_db": [
            counter["residual_rms_dbfs"] - fit["residual_rms_dbfs"]
            for counter, fit in zip(counterfactual, fitted, strict=True)
        ],
        "fitted_residual_peak_dbfs": report["residual_peak_dbfs"],
        "counterfactual_residual_peak_dbfs": report[
            "counterfactual_residual_peak_dbfs"
        ],
    }


def _cell_summary(cell: dict[str, Any]) -> dict[str, Any]:
    result = {
        "scenario": cell["scenario"],
        "modulator": cell["modulator"],
        "native_dsd_sha256": cell["native_dsd_sha256"],
        "hard_failures": cell["hard_failures"],
        "health": cell["health"],
    }
    measurements = cell.get("measurements")
    if isinstance(measurements, dict):
        for key in (
            "transition_envelope",
            "transition_envelope_excess_vs_reference",
            "restart_residual_peak_dbfs",
            "restart_rms_1ms_dbfs",
            "restart_rms_10ms_dbfs",
            "restart_rms_50ms_dbfs",
            "first_crossing_recovery_ms",
        ):
            if key in measurements:
                result[key] = measurements[key]
    return result


def freeze(root: Path, work_dir: Path, output: Path) -> dict[str, Any]:
    character_path = root / "tools/split_phase_v6/baselines/e3-p6d-local-0145.f64le"
    timing_source = work_dir / "native-timing/report.json"
    group_delay_source = work_dir / "native-timing/group-delay.csv"
    dsd_a_path = work_dir / "dsd-run-a/dsd-public-quality.json"
    dsd_b_path = work_dir / "dsd-run-b/dsd-public-quality.json"
    standard_probe = work_dir / "counterfactual-standard.json"
    ecbeam2_probe = work_dir / "counterfactual-ecbeam2.json"
    required = (
        character_path,
        timing_source,
        group_delay_source,
        dsd_a_path,
        dsd_b_path,
        standard_probe,
        ecbeam2_probe,
    )
    missing = [str(path) for path in required if not path.is_file()]
    if missing:
        raise RuntimeError(f"missing P6 freeze inputs: {missing}")
    if _sha256(character_path) != CHARACTER_SHA256:
        raise RuntimeError("P6 incumbent character hash changed")

    timing = _load(timing_source)
    dsd_a = _load(dsd_a_path)
    dsd_b = _load(dsd_b_path)
    canonical_a = _strip_volatile(dsd_a)
    canonical_b = _strip_volatile(dsd_b)
    if canonical_a != canonical_b:
        raise RuntimeError("clean DSD reports differ outside volatile provenance/timing")
    native_a = [cell["native_dsd_sha256"] for cell in dsd_a["cells"]]
    native_b = [cell["native_dsd_sha256"] for cell in dsd_b["cells"]]
    if native_a != native_b:
        raise RuntimeError("clean DSD reruns produced different native hashes")

    output.parent.mkdir(parents=True, exist_ok=True)
    frozen_timing = output.parent / "e3-p6-freeze-native-timing.json"
    frozen_dsd = output.parent / "e3-p6-freeze-dsd128.json"
    shutil.copyfile(timing_source, frozen_timing)
    shutil.copyfile(dsd_a_path, frozen_dsd)

    coefficients = np.fromfile(character_path, dtype="<f8")
    manifest = _load(root / "assets/filters/split_phase_e3/manifest.json")
    timing_filter = timing["filters"][0]
    report = {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "references": {
            "production_safety": "SplitPhase128kE2v3",
            "research_frontier": "p6d-local-0145",
        },
        "character": {
            "path": str(character_path.relative_to(root)).replace("\\", "/"),
            "sha256": CHARACTER_SHA256,
            "coefficient_count": int(coefficients.size),
            "exact_fsum": float(math.fsum(float(value) for value in coefficients)),
            "alignment": manifest["alignment"],
        },
        "native_timing": {
            "path": str(frozen_timing.relative_to(root)).replace("\\", "/"),
            "sha256": _sha256(frozen_timing),
            "group_delay_csv_sha256": _sha256(group_delay_source),
            "configuration": timing["configuration"],
            "runtime": timing_filter["runtime"],
            "impulse": timing_filter["impulse"],
            "tone_packets": timing_filter["tone_packets"],
        },
        "counterfactual_validation": {
            "contract": "actual mute/restart minus continuously running recovered carrier",
            "standard": _counterfactual_summary(standard_probe),
            "ecbeam2": _counterfactual_summary(ecbeam2_probe),
        },
        "clean_release_reproducibility": {
            "run_a_report_sha256": _sha256(dsd_a_path),
            "run_b_report_sha256": _sha256(dsd_b_path),
            "canonical_report_sha256": _canonical_sha256(canonical_a),
            "canonical_reports_identical": True,
            "native_dsd_hashes_identical": True,
            "run_a_executable_sha256": dsd_a["provenance"]["executable_sha256"],
            "run_b_executable_sha256": dsd_b["provenance"]["executable_sha256"],
            "source_snapshot_sha256": dsd_a["provenance"]["source_snapshot_sha256"],
            "rustc_version": dsd_a["provenance"]["rustc_version"],
            "target_os": dsd_a["provenance"]["target_os"],
            "target_arch": dsd_a["provenance"]["target_arch"],
            "cpu_class": dsd_a["provenance"]["cpu_class"],
            "rustflags": dsd_a["provenance"]["launch_rustflags"],
            "cell_count": len(dsd_a["cells"]),
            "hard_failure_count": dsd_a["hard_failure_count"],
            "diagnostic_hard_failure_count": dsd_a["diagnostic_hard_failure_count"],
            "cells": [_cell_summary(cell) for cell in dsd_a["cells"]],
            "frozen_run_a_path": str(frozen_dsd.relative_to(root)).replace("\\", "/"),
            "frozen_run_a_sha256": _sha256(frozen_dsd),
            "allowed_cross_build_differences": [
                "provenance.executable_sha256",
                "cells[*].render_seconds",
            ],
        },
        "minimum_effect_sizes": {
            "transition_interval_db": 0.03,
            "positive_transition_excess_percent": 1.0,
            "maximum_lobe_db": 0.05,
            "integrated_side_energy_db": 0.02,
            "main_lobe_width_us": 0.20,
            "step_response_percentage_points": 0.05,
        },
    }
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    return report


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--output", type=Path)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    work_dir = (arguments.work_dir or root / "tools/split_phase_v6/work-e3-p7-freeze").resolve()
    output = (
        arguments.output
        or root / "tools/split_phase_v6/baselines/e3-p6-definitive-freeze.json"
    ).resolve()
    report = freeze(root, work_dir, output)
    print(
        json.dumps(
            {
                "output": str(output),
                "character_sha256": report["character"]["sha256"],
                "canonical_reports_identical": report["clean_release_reproducibility"][
                    "canonical_reports_identical"
                ],
                "native_dsd_hashes_identical": report["clean_release_reproducibility"][
                    "native_dsd_hashes_identical"
                ],
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
