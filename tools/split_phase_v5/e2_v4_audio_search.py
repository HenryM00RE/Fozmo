from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

import numpy as np
from scipy.stats import qmc

from tools.split_phase_v5.e2_targeted_search import (
    IDENTITY as E2_IDENTITY,
    PROXY_FFT_LEN,
    _atomic_json,
    _evaluate_proxy_candidate,
    _load_proxy_checkpoints,
    _load_state,
    _load_thresholds,
    _sha256_array,
)


IDENTITY = "SplitPhase128kV5-E2v4-bounded-audio-search-experimental"
MAX_CANDIDATES = 256
RADII = (0.15, 0.075, 0.0375, 0.01875, 0.009375)


def _key(report: dict[str, Any], thresholds: dict[str, float], mid_limit: float, step_limit: float) -> list[float]:
    selected = report["audio_best"]
    proxy = selected["proxy_score"]
    exact = selected["exact_score"]
    return [
        max(proxy[4] / step_limit - 1.0, 0.0),
        max(proxy[0] / mid_limit - 1.0, 0.0),
        proxy[2] / thresholds["dominant"],
        proxy[0],
        proxy[4],
        exact[4],
    ]


def _evaluate(
    index: int,
    label: str,
    free: np.ndarray,
    existing: dict[int, dict[str, Any]],
    checkpoint_dir: Path,
    state: dict[str, Any],
    thresholds: dict[str, float],
    mid_limit: float,
    step_limit: float,
) -> dict[str, Any]:
    if index in existing:
        report = existing[index]
        if report["free_sha256"] != _sha256_array(free):
            raise RuntimeError(f"E2v4 checkpoint {index} does not match deterministic replay")
    else:
        report = _evaluate_proxy_candidate(index, label, free, state, thresholds)
        report["e2v4_key"] = _key(report, thresholds, mid_limit, step_limit)
        _atomic_json(checkpoint_dir / f"candidate_{index:05d}.json", report)
        existing[index] = report
    if "e2v4_key" not in report:
        report["e2v4_key"] = _key(report, thresholds, mid_limit, step_limit)
    return report


def build(
    root: Path,
    base_e_dir: Path,
    center_work_dir: Path,
    e2v3_dir: Path,
    work_dir: Path,
) -> dict[str, Any]:
    center_records = _load_proxy_checkpoints(center_work_dir / "proxy_checkpoints")
    if 2064 not in center_records:
        raise RuntimeError("E2v4 requires local-search candidate 2064")
    center_record = center_records[2064]
    center = np.asarray(center_record["free"], dtype=np.float64)
    thresholds = _load_thresholds(root)
    e2v3 = json.loads((e2v3_dir / "e2v3_report.json").read_text())
    if not e2v3.get("accepted"):
        raise RuntimeError("E2v4 requires accepted E2v3 provenance")
    e2v3_metrics = e2v3["full_pipeline_result"]["comparison"]["d_metrics"]
    mid_limit = float(center_record["audio_best"]["proxy_score"][0]) * 1.01
    step_limit = float(e2v3_metrics["step_response_overshoot"]) * 1.01
    state = _load_state(root, base_e_dir, PROXY_FFT_LEN)
    checkpoint_dir = work_dir / "proxy_checkpoints"
    existing = _load_proxy_checkpoints(checkpoint_dir)
    records: list[dict[str, Any]] = []
    index = 3000
    incumbent = _evaluate(
        index,
        "center_2064",
        center,
        existing,
        checkpoint_dir,
        state,
        thresholds,
        mid_limit,
        step_limit,
    )
    records.append(incumbent)
    incumbent_free = center.copy()
    index += 1

    for sweep, radius in enumerate(RADII):
        for coordinate in range(center.size):
            base_free = incumbent_free.copy()
            direction = np.zeros_like(base_free)
            direction[coordinate] = radius
            pair = []
            for sign, candidate_free in (("plus", base_free + direction), ("minus", base_free - direction)):
                report = _evaluate(
                    index,
                    f"sweep_{sweep}_coordinate_{coordinate:02d}_{sign}_{radius:g}",
                    candidate_free,
                    existing,
                    checkpoint_dir,
                    state,
                    thresholds,
                    mid_limit,
                    step_limit,
                )
                records.append(report)
                pair.append((report, candidate_free))
                index += 1
            best_report, best_free = min(
                [(incumbent, incumbent_free), *pair],
                key=lambda item: tuple(item[0]["e2v4_key"]),
            )
            incumbent = best_report
            incumbent_free = best_free.copy()

    remaining = MAX_CANDIDATES - len(records)
    sampler = qmc.Sobol(d=center.size, scramble=False)
    probes = sampler.random_base2(m=7)[1 : remaining + 1]
    for probe_index, probe in enumerate(probes):
        perturbation = (2.0 * probe - 1.0) * 0.05
        report = _evaluate(
            index,
            f"sobol_local_{probe_index:03d}",
            incumbent_free + perturbation,
            existing,
            checkpoint_dir,
            state,
            thresholds,
            mid_limit,
            step_limit,
        )
        records.append(report)
        index += 1

    best = min(records, key=lambda item: tuple(item["e2v4_key"]))
    qualifying = [
        record
        for record in records
        if record["e2v4_key"][0] == 0.0
        and record["e2v4_key"][1] == 0.0
        and record["audio_best"]["proxy_score"][2] <= thresholds["dominant"]
    ]
    qualifying.sort(key=lambda item: tuple(item["e2v4_key"]))
    summary = {
        "identity": IDENTITY,
        "source_identity": E2_IDENTITY,
        "center_candidate": 2064,
        "budget": MAX_CANDIDATES,
        "completed": len(records),
        "radii": list(RADII),
        "thresholds": thresholds,
        "guards": {"mid_3_14_maximum": mid_limit, "step_overshoot_maximum": step_limit},
        "best": best,
        "qualifying_count": len(qualifying),
        "qualifying_finalists": qualifying[:2],
        "checkpoint_semantics": "immutable per-candidate JSON with deterministic free-coordinate hash replay",
    }
    _atomic_json(work_dir / "e2v4_proxy_report.json", summary)
    return summary


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--base-e-dir", type=Path)
    parser.add_argument("--center-work-dir", type=Path)
    parser.add_argument("--e2v3-dir", type=Path)
    parser.add_argument("--work-dir", type=Path)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    base_e_dir = (arguments.base_e_dir or root / "tools/split_phase_v5/work-spe-direct-factor").resolve()
    center_work_dir = (
        arguments.center_work_dir or root / "tools/split_phase_v5/work-spe-e2-audio-local-20260719"
    ).resolve()
    e2v3_dir = (
        arguments.e2v3_dir or root / "tools/split_phase_v5/work-spe-e2v3-audio-highres-20260719"
    ).resolve()
    work_dir = (
        arguments.work_dir or root / "tools/split_phase_v5/work-spe-e2v4-bounded-audio-20260719"
    ).resolve()
    print(json.dumps(build(root, base_e_dir, center_work_dir, e2v3_dir, work_dir), indent=2))


if __name__ == "__main__":
    main()
