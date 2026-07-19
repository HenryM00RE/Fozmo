from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

import numpy as np

from tools.split_phase_v4.certify import _resample_target
from tools.split_phase_v5.e2_targeted_search import (
    _atomic_json,
    _atomic_npy,
    _full_audit,
    _resume_lawson,
    _sha256_file,
    finalize_screening_winner,
)


IDENTITY = "SplitPhase128kV5-E2v3-audio-high-resolution-experimental"


def build(
    root: Path,
    base_e_dir: Path,
    audio_dir: Path,
    work_dir: Path,
    fft_len: int,
    iterations: int,
) -> dict[str, Any]:
    audio_result = json.loads((audio_dir / "latest_result.json").read_text())
    if int(audio_result.get("proxy_candidate_index", -1)) != 2064:
        raise RuntimeError("E2v3 requires the audited audio-2064 source")
    if int(audio_result.get("completed_lawson_iterations", -1)) != 16:
        raise RuntimeError("E2v3 requires the complete 16-step audio source")
    character = np.load(audio_dir / "character.npy")
    target = np.load(audio_dir / "target_spectrum.npy")
    origin = int(json.loads((audio_dir / "alignment.json").read_text())["full_rate_origin"])
    provenance = {
        "audio_character": _sha256_file(audio_dir / "character.npy"),
        "audio_target": _sha256_file(audio_dir / "target_spectrum.npy"),
        "audio_result": _sha256_file(audio_dir / "latest_result.json"),
        "e_cleanup": _sha256_file(base_e_dir / "cleanup_optimized.npz"),
        "e_rational": _sha256_file(base_e_dir / "rational_minimax.json"),
    }
    manifest = {
        "identity": IDENTITY,
        "base_e_dir": str(base_e_dir),
        "audio_dir": str(audio_dir),
        "origin": origin,
        "fft_len": fft_len,
        "source_sha256": provenance,
        "resume_semantics": "one hash-verified Lawson character checkpoint after every high-resolution iteration",
    }
    work_dir.mkdir(parents=True, exist_ok=True)
    manifest_path = work_dir / "manifest.json"
    if manifest_path.exists() and json.loads(manifest_path.read_text()) != manifest:
        raise RuntimeError("E2v3 work directory belongs to a different invocation")
    _atomic_json(manifest_path, manifest)
    _atomic_npy(work_dir / "target_spectrum.npy", target)
    _atomic_json(work_dir / "alignment.json", {"full_rate_origin": origin})

    omega = np.linspace(0.0, np.pi, fft_len // 2 + 1)
    high_resolution_target = _resample_target(target, omega, origin)
    refined, history = _resume_lawson(
        work_dir,
        character,
        high_resolution_target,
        fft_len,
        iterations,
        5.0e-5,
    )
    completed = len(history)
    audit_path = work_dir / f"full_audit_{completed:05d}.json"
    if audit_path.exists():
        audit = json.loads(audit_path.read_text())
    else:
        audit = _full_audit(root, base_e_dir, refined, target, origin)
        _atomic_json(audit_path, audit)
    screening = {
        "identity": IDENTITY,
        "mode": "audio_high_resolution",
        "origin": origin,
        "fft_len": fft_len,
        "completed_lawson_iterations": completed,
        "source_audio_comparison": audio_result["comparison"],
        "comparison": audit,
        "screening_reuses_e_cleanup_and_rational_assets": True,
        "directory": str(work_dir),
        "accepted": bool(audit["accepted"]),
        "production_promoted": False,
    }
    _atomic_json(work_dir / "screening_result.json", screening)
    full_result = finalize_screening_winner(root, base_e_dir, work_dir, screening)
    report = {
        "identity": IDENTITY,
        "provenance": provenance,
        "screening": screening,
        "full_pipeline_result": full_result,
        "accepted": bool(full_result is not None and full_result["accepted"]),
        "production_promoted": False,
        "resume_semantics": manifest["resume_semantics"],
    }
    _atomic_json(work_dir / "e2v3_report.json", report)
    return report


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--base-e-dir", type=Path)
    parser.add_argument("--audio-dir", type=Path)
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--fft-len", type=int, default=1 << 22)
    parser.add_argument("--iterations", type=int, default=4)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    base_e_dir = (arguments.base_e_dir or root / "tools/split_phase_v5/work-spe-direct-factor").resolve()
    audio_dir = (
        arguments.audio_dir or root / "tools/split_phase_v5/work-spe-e2-support-2064-20260719"
    ).resolve()
    work_dir = (
        arguments.work_dir or root / "tools/split_phase_v5/work-spe-e2v3-audio-highres-20260719"
    ).resolve()
    if arguments.fft_len < 1_048_576 or arguments.fft_len & (arguments.fft_len - 1):
        parser.error("--fft-len must be a power of two at least 1048576")
    print(
        json.dumps(
            build(
                root,
                base_e_dir,
                audio_dir,
                work_dir,
                arguments.fft_len,
                arguments.iterations,
            ),
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
