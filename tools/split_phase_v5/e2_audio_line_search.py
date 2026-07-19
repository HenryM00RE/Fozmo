from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

import numpy as np

from tools.split_phase_v5.e2_targeted_search import (
    IDENTITY,
    PROXY_FFT_LEN,
    _atomic_json,
    _evaluate_proxy_candidate,
    _gradient,
    _load_proxy_checkpoints,
    _load_state,
    _load_thresholds,
)


LINE_IDENTITY = IDENTITY + "-audio-line"
DEFAULT_STEPS = (1.6, 1.7, 1.8, 1.9, 2.0, 2.1, 2.2, 2.3, 2.4)


def build(
    root: Path,
    source_dir: Path,
    proxy_work_dir: Path,
    work_dir: Path,
    steps: tuple[float, ...] = DEFAULT_STEPS,
) -> dict[str, Any]:
    source_records = _load_proxy_checkpoints(proxy_work_dir / "proxy_checkpoints")
    state = _load_state(root, source_dir, PROXY_FFT_LEN)
    thresholds = _load_thresholds(root)
    gradient = _gradient(source_records, state["baseline_free"], 0.02, "audio_best")
    checkpoint_dir = work_dir / "proxy_checkpoints"
    existing = _load_proxy_checkpoints(checkpoint_dir)
    records = []
    for offset, step in enumerate(steps):
        index = 1000 + offset
        if index in existing:
            report = existing[index]
        else:
            free = state["baseline_free"] - step * gradient
            report = _evaluate_proxy_candidate(
                index,
                f"audio_gradient_refined_{step:g}",
                free,
                state,
                thresholds,
            )
            _atomic_json(checkpoint_dir / f"candidate_{index:05d}.json", report)
        records.append(report)
    best = min(records, key=lambda item: tuple(item["audio_best"]["audio_key"]))
    summary = {
        "identity": LINE_IDENTITY,
        "source_identity": IDENTITY,
        "steps": list(steps),
        "thresholds": thresholds,
        "completed": len(records),
        "best": best,
        "records": records,
    }
    _atomic_json(work_dir / "line_search_report.json", summary)
    return summary


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--source-dir", type=Path)
    parser.add_argument("--proxy-work-dir", type=Path)
    parser.add_argument("--work-dir", type=Path)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    source_dir = (arguments.source_dir or root / "tools/split_phase_v5/work-spe-direct-factor").resolve()
    proxy_work_dir = (
        arguments.proxy_work_dir or root / "tools/split_phase_v5/work-spe-e2-targeted-v2-20260719"
    ).resolve()
    work_dir = (
        arguments.work_dir or root / "tools/split_phase_v5/work-spe-e2-audio-line-20260719"
    ).resolve()
    print(json.dumps(build(root, source_dir, proxy_work_dir, work_dir), indent=2))


if __name__ == "__main__":
    main()
