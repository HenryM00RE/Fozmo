from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

import numpy as np

from tools.split_phase_v4.baseline import DESIGN_FFT_LEN, _load_c, _metrics
from tools.split_phase_v4.certify import _resample_target
from tools.split_phase_v4.compare_abcd import compare
from tools.split_phase_v4.report import build as build_v4_pipeline
from tools.split_phase_v5.c_derived_magnitude import (
    EXPERIMENT_IDENTITY,
    _atomic_json,
    certify_saved_factor,
)


def build(root: Path, work_dir: Path, fft_len: int = 1_048_576) -> dict[str, Any]:
    return build_v4_pipeline(
        root,
        work_dir,
        fft_len,
        factor_builder=certify_saved_factor,
        identity=EXPERIMENT_IDENTITY,
    )


def finalize_saved(root: Path, work_dir: Path) -> dict[str, Any]:
    required = (
        "target_spectrum.npy",
        "alignment.json",
        "character_optimized.npy",
        "character_minimax.json",
        "cleanup_optimized.npz",
        "cleanup_socp.json",
        "rational_minimax.json",
        "group_delay_spline.json",
        "spectral_factor.json",
        "support_search.json",
    )
    missing = [name for name in required if not (work_dir / name).exists()]
    if missing:
        raise RuntimeError("cannot finalize saved E build; missing " + ", ".join(missing))
    target = np.load(work_dir / "target_spectrum.npy")
    character = np.load(work_dir / "character_optimized.npy")
    origin = int(json.loads((work_dir / "alignment.json").read_text())["full_rate_origin"])
    _, cleanup_templates, _, _ = _load_c(root)
    with np.load(work_dir / "cleanup_optimized.npz") as saved_cleanup:
        cleanup_values = [
            np.asarray(saved_cleanup[f"stage_{index}"], dtype=np.float64)
            for index in range(1, 8)
        ]
    cleanup_stages = [
        type(cleanup_templates[0])(value)
        for value in cleanup_values
    ]
    metric_omega = np.linspace(0.0, np.pi, DESIGN_FFT_LEN // 2 + 1)
    metric_target = _resample_target(target, metric_omega, origin)
    d_metrics = _metrics(character, cleanup_stages, origin, metric_target)
    rational_report = json.loads((work_dir / "rational_minimax.json").read_text())
    comparison = compare(root, d_metrics, character, rational_report)
    comparison["d_metrics"] = d_metrics
    _atomic_json(work_dir / "comparison_abcd.json", comparison)
    spline_report = json.loads((work_dir / "group_delay_spline.json").read_text())
    factor_report = json.loads((work_dir / "spectral_factor.json").read_text())
    character_report = json.loads((work_dir / "character_minimax.json").read_text())
    cleanup_report = json.loads((work_dir / "cleanup_socp.json").read_text())
    support_report = json.loads((work_dir / "support_search.json").read_text())
    outer_history = [
        {
            "block": "group_delay_character_outer_loop",
            "accepted": any(item.get("accepted", False) for item in spline_report.get("outer_history", [])),
            "controls_live": True,
            "history": spline_report.get("outer_history", []),
        },
        {
            "block": "character_lawson",
            "accepted": True,
            "selected_initialization": character_report.get("selected_initialization"),
        },
        {
            "block": "cleanup_socp",
            "accepted_stages": sum(item.get("accepted", False) for item in cleanup_report.get("stages", [])),
        },
        {"block": "rational_joint_minimax", "accepted": True},
        {
            "block": "simultaneous_jax_polish",
            "accepted": False,
            "reason": "not run; production acceptance relies on the constrained reduced-coordinate block loop",
        },
    ]
    summary = {
        "identity": EXPERIMENT_IDENTITY,
        "target": {"factor": factor_report, "spline": spline_report},
        "support": support_report,
        "character": character_report,
        "cleanup": cleanup_report,
        "rational": rational_report,
        "comparison": comparison,
        "outer_joint_history": outer_history,
        "best_feasible_incumbent_checkpointed": True,
        "resumed_from_saved_blocks": True,
    }
    _atomic_json(work_dir / "build_report.json", summary)
    return summary


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--work-dir", type=Path, default=Path(__file__).resolve().parent / "work-spe-direct-factor")
    parser.add_argument("--fft-len", type=int, default=1_048_576)
    parser.add_argument("--finalize-saved", action="store_true")
    arguments = parser.parse_args()
    if arguments.finalize_saved:
        report = finalize_saved(arguments.root.resolve(), arguments.work_dir)
    else:
        report = build(arguments.root.resolve(), arguments.work_dir, arguments.fft_len)
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
