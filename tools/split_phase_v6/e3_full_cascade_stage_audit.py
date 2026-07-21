from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path
from typing import Any

import numpy as np
from scipy import signal

from .e3_phase_search import _read_f64le


IDENTITY = "SplitPhase128kE3-P5-full-cascade-stage-impulse-envelope-audit"
SOURCE_RATE_HZ = 44_100
TARGET_RATE_HZ = 5_644_800
INTERVALS_MS = ((0.0, 2.0), (2.0, 5.0), (5.0, 10.0), (10.0, 25.0), (25.0, 50.0))
WINDOW_MS = 2.0
PRE_TRIM_MS = 25.0
POST_TRIM_MS = 100.0


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _trim(response: np.ndarray, sample_rate: int) -> np.ndarray:
    peak = int(np.argmax(np.abs(response)))
    start = max(0, peak - round(PRE_TRIM_MS * sample_rate / 1000.0))
    end = min(response.size, peak + round(POST_TRIM_MS * sample_rate / 1000.0) + 1)
    return np.asarray(response[start:end], dtype=np.float64)


def _stages(character: np.ndarray, cleanups: list[np.ndarray]) -> list[tuple[str, int, np.ndarray]]:
    sample_rate = SOURCE_RATE_HZ * 2
    response = _trim(2.0 * character, sample_rate)
    stages = [("character", sample_rate, response)]
    for index, cleanup in enumerate(cleanups, start=1):
        response = signal.upfirdn(2.0 * cleanup, response, up=2)
        sample_rate *= 2
        response = _trim(response, sample_rate)
        stages.append((f"cleanup_{index}", sample_rate, response))
    if stages[-1][1] != TARGET_RATE_HZ:
        raise RuntimeError(f"cascade terminated at {stages[-1][1]} Hz")
    return stages


def _trace(response: np.ndarray, sample_rate: int) -> tuple[np.ndarray, list[dict[str, float]]]:
    peak_index = int(np.argmax(np.abs(response)))
    peak = float(abs(response[peak_index]))
    if peak <= 0.0:
        raise RuntimeError("stage response has no principal peak")
    normalized = response / peak
    trace_samples = round(50.0 * sample_rate / 1000.0)
    window = max(round(WINDOW_MS * sample_rate / 1000.0), 16)
    required = peak_index + trace_samples + window - 1
    if required > normalized.size:
        normalized = np.pad(normalized, (0, required - normalized.size))
    squared = normalized * normalized
    prefix = np.concatenate(([0.0], np.cumsum(squared, dtype=np.float64)))
    starts = peak_index + np.arange(trace_samples)
    trace = (prefix[starts + window] - prefix[starts]) / window
    intervals = []
    for start_ms, end_ms in INTERVALS_MS:
        start = round(start_ms * sample_rate / 1000.0)
        end = round(end_ms * sample_rate / 1000.0)
        power = trace[start:end]
        raw = normalized[peak_index + start : peak_index + end]
        rms = math.sqrt(float(np.mean(raw * raw)))
        intervals.append(
            {
                "start_ms": start_ms,
                "end_ms": end_ms,
                "residual_rms_db_peak_relative": 20.0 * math.log10(max(rms, 1.0e-300)),
                "residual_energy_peak_normalized_seconds": float(np.dot(raw, raw)) / sample_rate,
                "maximum_sliding_rms_db_peak_relative": 10.0
                * math.log10(max(float(np.max(power)), 1.0e-300)),
                "integrated_envelope_power_peak_normalized_seconds": float(np.sum(power))
                / sample_rate,
            }
        )
    return trace, intervals


def _audit_candidate(
    name: str,
    path: Path,
    cleanups: list[np.ndarray],
    reference_stages: list[tuple[str, int, np.ndarray]],
) -> dict[str, Any]:
    candidate_stages = _stages(_read_f64le(path), cleanups)
    reports = []
    for (stage_name, sample_rate, candidate), (reference_name, reference_rate, reference) in zip(
        candidate_stages, reference_stages, strict=True
    ):
        if (stage_name, sample_rate) != (reference_name, reference_rate):
            raise RuntimeError("candidate/reference stage mismatch")
        candidate_trace, candidate_intervals = _trace(candidate, sample_rate)
        reference_trace, reference_intervals = _trace(reference, sample_rate)
        interval_reports = []
        for index, ((start_ms, end_ms), candidate_interval, reference_interval) in enumerate(
            zip(INTERVALS_MS, candidate_intervals, reference_intervals, strict=True)
        ):
            start = round(start_ms * sample_rate / 1000.0)
            end = round(end_ms * sample_rate / 1000.0)
            excess = np.maximum(candidate_trace[start:end] - reference_trace[start:end], 0.0)
            interval_reports.append(
                {
                    **candidate_interval,
                    "reference_residual_rms_db_peak_relative": reference_interval[
                        "residual_rms_db_peak_relative"
                    ],
                    "residual_rms_delta_db": candidate_interval[
                        "residual_rms_db_peak_relative"
                    ]
                    - reference_interval["residual_rms_db_peak_relative"],
                    "maximum_positive_excess_power_peak_normalized": float(np.max(excess)),
                    "integrated_positive_excess_power_peak_normalized_seconds": float(
                        np.sum(excess)
                    )
                    / sample_rate,
                }
            )
        reports.append(
            {
                "stage": stage_name,
                "sample_rate_hz": sample_rate,
                "integer_ratio_from_source": sample_rate // SOURCE_RATE_HZ,
                "candidate_principal_peak_index": int(np.argmax(np.abs(candidate))),
                "reference_principal_peak_index": int(np.argmax(np.abs(reference))),
                "alignment": "independent principal absolute peak",
                "intervals": interval_reports,
            }
        )
    return {
        "name": name,
        "character_path": str(path),
        "character_sha256": _sha256(path),
        "stages": reports,
    }


def build(root: Path, candidates: list[tuple[str, Path]]) -> dict[str, Any]:
    assets = root / "assets/filters/split_phase_e2v3"
    reference_path = assets / "character_full_rate.f64le"
    cleanups = [_read_f64le(assets / f"cleanup_stage_{index}.f64le") for index in range(1, 7)]
    reference_stages = _stages(_read_f64le(reference_path), cleanups)
    return {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "measurement": {
            "kind": "exact finite-support effective LTI impulse cascade",
            "normalization": "independent principal absolute peak per stage",
            "sliding_window_ms": WINDOW_MS,
            "intervals_ms": [list(interval) for interval in INTERVALS_MS],
            "trim_before_peak_ms": PRE_TRIM_MS,
            "trim_after_peak_ms": POST_TRIM_MS,
            "limitation": "isolates interpolation stages; Standard and EcBeam2 reconstructed restart envelopes are measured by dsd_public_quality v5",
        },
        "reference": {
            "name": "SplitPhase128kE2v3",
            "character_path": str(reference_path),
            "character_sha256": _sha256(reference_path),
            "cleanup_sha256": {
                str(index): _sha256(assets / f"cleanup_stage_{index}.f64le")
                for index in range(1, 7)
            },
        },
        "candidates": [
            _audit_candidate(name, path, cleanups, reference_stages) for name, path in candidates
        ],
    }


def _candidate(value: str) -> tuple[str, Path]:
    name, separator, raw_path = value.partition("=")
    if not separator or not name or not raw_path:
        raise argparse.ArgumentTypeError("candidate must be NAME=PATH")
    return name, Path(raw_path).resolve()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--candidate", type=_candidate, action="append", required=True)
    parser.add_argument("--out", type=Path, required=True)
    arguments = parser.parse_args()
    report = build(arguments.root.resolve(), arguments.candidate)
    arguments.out.parent.mkdir(parents=True, exist_ok=True)
    arguments.out.write_bytes((json.dumps(report, indent=2) + "\n").encode("utf-8"))
    print(json.dumps({"out": str(arguments.out), "sha256": _sha256(arguments.out)}, indent=2))


if __name__ == "__main__":
    main()
