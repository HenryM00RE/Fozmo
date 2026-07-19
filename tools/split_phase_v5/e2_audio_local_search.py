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
    _load_proxy_checkpoints,
    _load_state,
    _load_thresholds,
)


LOCAL_IDENTITY = IDENTITY + "-audio-local"


def build(
    root: Path,
    source_dir: Path,
    line_work_dir: Path,
    work_dir: Path,
    center_index: int = 1005,
    radii: tuple[float, ...] = (0.05, 0.15),
    coordinates: tuple[int, ...] | None = None,
) -> dict[str, Any]:
    line_records = _load_proxy_checkpoints(line_work_dir / "proxy_checkpoints")
    if center_index not in line_records:
        raise RuntimeError(f"line-search center {center_index} is unavailable")
    center = np.asarray(line_records[center_index]["free"], dtype=np.float64)
    state = _load_state(root, source_dir, PROXY_FFT_LEN)
    thresholds = _load_thresholds(root)
    checkpoint_dir = work_dir / "proxy_checkpoints"
    existing = _load_proxy_checkpoints(checkpoint_dir)
    specs: list[tuple[str, np.ndarray]] = [("center", center.copy())]
    selected_coordinates = tuple(range(center.size)) if coordinates is None else coordinates
    if any(coordinate < 0 or coordinate >= center.size for coordinate in selected_coordinates):
        raise ValueError("local-search coordinate is out of range")
    for radius in radii:
        for coordinate in selected_coordinates:
            direction = np.zeros_like(center)
            direction[coordinate] = radius
            specs.append((f"coordinate_{coordinate:02d}_plus_{radius:g}", center + direction))
            specs.append((f"coordinate_{coordinate:02d}_minus_{radius:g}", center - direction))
    records = []
    for offset, (label, free) in enumerate(specs):
        index = 2000 + offset
        if index in existing:
            report = existing[index]
        else:
            report = _evaluate_proxy_candidate(index, label, free, state, thresholds)
            _atomic_json(checkpoint_dir / f"candidate_{index:05d}.json", report)
        records.append(report)
    best = min(records, key=lambda item: tuple(item["audio_best"]["audio_key"]))
    summary = {
        "identity": LOCAL_IDENTITY,
        "source_identity": IDENTITY,
        "center_index": center_index,
        "radii": list(radii),
        "coordinates": list(selected_coordinates),
        "thresholds": thresholds,
        "completed": len(records),
        "best": best,
    }
    _atomic_json(work_dir / "local_search_report.json", summary)
    return summary


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--source-dir", type=Path)
    parser.add_argument("--line-work-dir", type=Path)
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--center-index", type=int, default=1005)
    parser.add_argument("--radii", type=float, nargs="+", default=(0.05, 0.15))
    parser.add_argument("--coordinates", type=int, nargs="+")
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    source_dir = (arguments.source_dir or root / "tools/split_phase_v5/work-spe-direct-factor").resolve()
    line_work_dir = (
        arguments.line_work_dir or root / "tools/split_phase_v5/work-spe-e2-audio-line-20260719"
    ).resolve()
    work_dir = (
        arguments.work_dir or root / "tools/split_phase_v5/work-spe-e2-audio-local-20260719"
    ).resolve()
    print(
        json.dumps(
            build(
                root,
                source_dir,
                line_work_dir,
                work_dir,
                arguments.center_index,
                tuple(arguments.radii),
                None if arguments.coordinates is None else tuple(arguments.coordinates),
            ),
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
