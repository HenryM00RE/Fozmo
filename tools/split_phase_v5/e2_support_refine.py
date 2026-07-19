from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

import numpy as np

from tools.split_phase_v4.baseline import _log_delay_metrics
from tools.split_phase_v4.character_minimax import _dense_score
from tools.split_phase_v4.report import _temporal_objectives
from tools.split_phase_v5.e2_targeted_search import (
    IDENTITY,
    PROXY_FFT_LEN,
    SUPPORT,
    _atomic_json,
    _atomic_npy,
    _candidate_target,
    _full_audit,
    _load_proxy_checkpoints,
    _load_state,
    _resume_lawson,
    _sha256_array,
    finalize_screening_winner,
)


SUPPORT_IDENTITY = IDENTITY + "-support-start"


def build(
    root: Path,
    source_dir: Path,
    proxy_work_dir: Path,
    work_dir: Path,
    candidate_index: int,
    iterations: int,
) -> dict[str, Any]:
    records = _load_proxy_checkpoints(proxy_work_dir / "proxy_checkpoints")
    if candidate_index not in records:
        raise RuntimeError(f"proxy candidate {candidate_index} is unavailable")
    record = records[candidate_index]
    selection = record["audio_best"]
    free = np.asarray(record["free"], dtype=np.float64)
    if record["free_sha256"] != _sha256_array(free):
        raise RuntimeError("selected proxy free-coordinate hash mismatch")

    state = _load_state(root, source_dir, PROXY_FFT_LEN)
    origin = int(selection["origin"])
    target, coordinate_metrics = _candidate_target(state, free, origin)
    periodic = np.fft.irfft(target, n=PROXY_FFT_LEN)
    initial = np.asarray(periodic[:SUPPORT], dtype=np.float64).copy()

    work_dir.mkdir(parents=True, exist_ok=True)
    manifest = {
        "identity": SUPPORT_IDENTITY,
        "source_identity": IDENTITY,
        "source_dir": str(source_dir),
        "proxy_work_dir": str(proxy_work_dir),
        "proxy_candidate_index": candidate_index,
        "proxy_candidate_free_sha256": record["free_sha256"],
        "origin": origin,
        "fft_len": PROXY_FFT_LEN,
        "initial_character_sha256": _sha256_array(initial),
    }
    manifest_path = work_dir / "manifest.json"
    if manifest_path.exists() and json.loads(manifest_path.read_text()) != manifest:
        raise RuntimeError("support-refinement work directory belongs to a different candidate")
    _atomic_json(manifest_path, manifest)
    _atomic_npy(work_dir / "target_spectrum.npy", target)
    _atomic_npy(work_dir / "raw_support_initial.npy", initial)
    _atomic_json(work_dir / "alignment.json", {"full_rate_origin": origin})

    initial_report_path = work_dir / "raw_support_metrics.json"
    if initial_report_path.exists():
        initial_report = json.loads(initial_report_path.read_text())
    else:
        initial_report = {
            "dense_score": list(_dense_score(initial, target, PROXY_FFT_LEN)),
            "temporal_objectives": list(_temporal_objectives(initial, PROXY_FFT_LEN)),
            "delay_metrics": _log_delay_metrics(initial),
            "coordinate_metrics": coordinate_metrics,
        }
        _atomic_json(initial_report_path, initial_report)

    character, history = _resume_lawson(
        work_dir,
        initial,
        target,
        PROXY_FFT_LEN,
        iterations,
        5.0e-4,
    )
    completed = len(history)
    audit_path = work_dir / f"full_audit_{completed:05d}.json"
    if audit_path.exists():
        audit = json.loads(audit_path.read_text())
    else:
        audit = _full_audit(root, source_dir, character, target, origin)
        _atomic_json(audit_path, audit)
    result = {
        "identity": SUPPORT_IDENTITY,
        "proxy_candidate_index": candidate_index,
        "origin": origin,
        "completed_lawson_iterations": completed,
        "initial": initial_report,
        "comparison": audit,
        "screening_reuses_e_cleanup_and_rational_assets": True,
        "accepted": bool(audit["accepted"]),
        "directory": str(work_dir),
        "production_promoted": False,
    }
    result_path = work_dir / f"result_{completed:05d}.json"
    _atomic_json(result_path, result)
    _atomic_json(work_dir / "latest_result.json", result)
    full_result = finalize_screening_winner(root, source_dir, work_dir, result)
    if full_result is not None:
        result["full_pipeline_result"] = full_result
        result["accepted"] = bool(full_result["accepted"])
        _atomic_json(result_path, result)
        _atomic_json(work_dir / "latest_result.json", result)
    return result


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--source-dir", type=Path)
    parser.add_argument("--proxy-work-dir", type=Path)
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--candidate-index", type=int, default=48)
    parser.add_argument("--iterations", type=int, default=16)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    source_dir = (arguments.source_dir or root / "tools/split_phase_v5/work-spe-direct-factor").resolve()
    proxy_work_dir = (
        arguments.proxy_work_dir or root / "tools/split_phase_v5/work-spe-e2-targeted-v2-20260719"
    ).resolve()
    work_dir = (
        arguments.work_dir or root / "tools/split_phase_v5/work-spe-e2-support-48-20260719"
    ).resolve()
    report = build(
        root,
        source_dir,
        proxy_work_dir,
        work_dir,
        arguments.candidate_index,
        arguments.iterations,
    )
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
