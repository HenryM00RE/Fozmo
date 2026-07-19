from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

import numpy as np

from .export_assets import _write_f64le, _write_generated_rust


def stage(work_dir: Path, asset_dir: Path) -> dict[str, Any]:
    """Stage a clearly marked non-production bundle for Rust runtime tests."""
    alignment = json.loads((work_dir / "alignment.json").read_text())
    asset_dir.mkdir(parents=True, exist_ok=True)
    files = {
        "character": _write_f64le(
            asset_dir / "character_full_rate.f64le",
            np.load(work_dir / "character_optimized.npy"),
        ),
        "rational_147_160": _write_f64le(
            asset_dir / "rational_147_160.f64le",
            np.load(work_dir / "rational_147_160.npy"),
        ),
        "rational_160_147": _write_f64le(
            asset_dir / "rational_160_147.f64le",
            np.load(work_dir / "rational_160_147.npy"),
        ),
    }
    cleanup = np.load(work_dir / "cleanup_optimized.npz")
    files["cleanups"] = [
        _write_f64le(
            asset_dir / ("cleanup_stage_" + str(index) + ".f64le"),
            cleanup["stage_" + str(index)],
        )
        for index in range(1, 8)
    ]
    development_manifest = {
        "identity": "SplitPhase128kV4",
        "development_runtime_checkpoint_only": True,
        "production_exported": False,
        "alignment": alignment,
        "files": files,
    }
    _write_generated_rust(asset_dir, development_manifest)
    (asset_dir / "DEVELOPMENT_CHECKPOINT.json").write_text(
        json.dumps(development_manifest, indent=2) + "\n"
    )
    return development_manifest


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--asset-dir", type=Path)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    print(
        json.dumps(
            stage(
                arguments.work_dir or root / "tools/split_phase_v4/work",
                arguments.asset_dir or root / "assets/filters/split_phase_v4",
            ),
            indent=2,
        )
    )
