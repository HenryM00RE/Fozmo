from __future__ import annotations

import argparse
import hashlib
import json
import math
from dataclasses import asdict
from pathlib import Path
from typing import Any

import numpy as np
from scipy.interpolate import CubicSpline
from scipy.stats import qmc

from .e3_phase_search import (
    CHARACTER_RATE_HZ,
    CLOSURE_END_HZ,
    CLOSURE_START_HZ,
    FFT_LENGTH,
    LOW_JOIN_HZ,
    _cascade_character_and_cleanup,
    _read_f64le,
    _smoothstep5,
    _timing_metrics,
)
from .evaluate_e3_packets import _measure_packet


IDENTITY = "SplitPhase128kE3-P2-packet-constrained-treble-phase-search"
CONTROL_FREQUENCIES_HZ = np.asarray(
    [LOW_JOIN_HZ, 15_000.0, 16_500.0, 18_000.0, 19_500.0, CLOSURE_START_HZ],
    dtype=np.float64,
)
DELAY_RANGE_SAMPLES = (-42.0, -20.0)
BLEND_RANGES = (
    (0.015, 0.050),
    (0.075, 0.140),
    (0.075, 0.160),
    (0.060, 0.150),
    (0.050, 0.150),
)
PROXY_OVERSHOOT_GUARD_PERCENT = 9.70


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _candidate_character(
    baseline_spectrum: np.ndarray,
    baseline_phase: np.ndarray,
    magnitude: np.ndarray,
    omega: np.ndarray,
    frequency_hz: np.ndarray,
    baseline_sum: float,
    support: int,
    peak_delay: float,
    delay_offset: float,
    blends: np.ndarray,
) -> tuple[np.ndarray, float]:
    low_index = int(np.argmin(np.abs(frequency_hz - LOW_JOIN_HZ)))
    target_delay = peak_delay + delay_offset
    linear_phase = baseline_phase[low_index] - target_delay * (omega - omega[low_index])

    control_values = np.concatenate(([0.0], np.asarray(blends, dtype=np.float64)))
    blend_spline = CubicSpline(
        CONTROL_FREQUENCIES_HZ,
        control_values,
        bc_type=((1, 0.0), (1, 0.0)),
    )
    blend = np.zeros_like(frequency_hz)
    searchable = (frequency_hz >= LOW_JOIN_HZ) & (frequency_hz <= CLOSURE_START_HZ)
    blend[searchable] = blend_spline(frequency_hz[searchable])
    closure = (frequency_hz > CLOSURE_START_HZ) & (frequency_hz <= CLOSURE_END_HZ)
    blend[closure] = blends[-1] * (
        1.0
        - _smoothstep5(
            (frequency_hz[closure] - CLOSURE_START_HZ)
            / (CLOSURE_END_HZ - CLOSURE_START_HZ)
        )
    )
    phase = baseline_phase + blend * (linear_phase - baseline_phase)
    target = magnitude * np.exp(1j * phase)
    target[0] = complex(float(baseline_spectrum[0].real), 0.0)
    target[-1] = complex(float(baseline_spectrum[-1].real), 0.0)
    periodic = np.fft.irfft(target, FFT_LENGTH)
    kept = periodic[:support].copy()
    kept *= baseline_sum / float(math.fsum(float(value) for value in kept))
    omitted_energy = float(np.dot(periodic[support:], periodic[support:])) / max(
        float(np.dot(periodic, periodic)), 1.0e-300
    )
    return kept, omitted_energy


def _passes_guards(metrics: dict[str, Any], magnitude_delta_db: float) -> bool:
    decay_120 = metrics["decay_120_ms"]
    return bool(
        metrics["maximum_pre_lobe_db_peak"] <= -22.5
        and metrics["pre_energy_db_total"] <= -4.85
        and metrics["main_lobe_width_us"] <= 62.5
        and metrics["step_overshoot_percent"] <= PROXY_OVERSHOOT_GUARD_PERCENT
        and (decay_120 is None or decay_120 <= 7.0)
        and magnitude_delta_db <= 5.0e-7
    )


def _dominates(left: dict[str, Any], right: dict[str, Any]) -> bool:
    left_metrics = left["metrics"]
    right_metrics = right["metrics"]
    left_objectives = (
        left_metrics["maximum_post_lobe_db_peak"],
        left_metrics["post_energy_db_total"],
        left_metrics["step_undershoot_percent"],
        left["packet_15khz"]["onset_pre_echo_energy_db_total"],
    )
    right_objectives = (
        right_metrics["maximum_post_lobe_db_peak"],
        right_metrics["post_energy_db_total"],
        right_metrics["step_undershoot_percent"],
        right["packet_15khz"]["onset_pre_echo_energy_db_total"],
    )
    return all(a <= b for a, b in zip(left_objectives, right_objectives)) and any(
        a < b for a, b in zip(left_objectives, right_objectives)
    )


def _samples(count: int) -> np.ndarray:
    if count <= 0 or count & (count - 1):
        raise ValueError("candidate count must be a positive power of two")
    unit = qmc.Sobol(d=6, scramble=False).random_base2(int(math.log2(count)))
    lower = np.asarray([DELAY_RANGE_SAMPLES[0], *(item[0] for item in BLEND_RANGES)])
    upper = np.asarray([DELAY_RANGE_SAMPLES[1], *(item[1] for item in BLEND_RANGES)])
    scaled = qmc.scale(unit, lower, upper)
    p3_seed = np.asarray([-37.0, 0.031744, 0.10, 0.10, 0.10, 0.10])
    scaled[0] = p3_seed
    return scaled


def search(root: Path, work_dir: Path, candidate_count: int) -> dict[str, Any]:
    asset_dir = root / "assets/filters/split_phase_e2v3"
    character_path = asset_dir / "character_full_rate.f64le"
    cleanup_path = asset_dir / "cleanup_stage_1.f64le"
    character = _read_f64le(character_path)
    cleanup = _read_f64le(cleanup_path)
    spectrum = np.fft.rfft(character, FFT_LENGTH)
    magnitude = np.abs(spectrum)
    phase = np.unwrap(np.angle(spectrum))
    frequency_hz = np.fft.rfftfreq(FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ)
    omega = 2.0 * np.pi * np.arange(spectrum.size, dtype=np.float64) / FFT_LENGTH
    peak_delay = float(np.argmax(np.abs(character)))
    baseline_sum = float(math.fsum(float(value) for value in character))
    reliable = frequency_hz <= 20_000.0

    records: list[dict[str, Any]] = []
    characters: dict[str, np.ndarray] = {}
    for index, sample in enumerate(_samples(candidate_count)):
        delay_offset = float(sample[0])
        blends = sample[1:]
        candidate, omitted_energy = _candidate_character(
            spectrum,
            phase,
            magnitude,
            omega,
            frequency_hz,
            baseline_sum,
            character.size,
            peak_delay,
            delay_offset,
            blends,
        )
        candidate_spectrum = np.fft.rfft(candidate, FFT_LENGTH)
        magnitude_error_db = 20.0 * np.log10(
            np.maximum(np.abs(candidate_spectrum[reliable]), 1.0e-300)
            / np.maximum(magnitude[reliable], 1.0e-300)
        )
        magnitude_delta_db = float(np.max(np.abs(magnitude_error_db)))
        response = _cascade_character_and_cleanup(candidate, cleanup)
        metrics = asdict(_timing_metrics(response))
        identifier = f"sobol-{index:04d}"
        record: dict[str, Any] = {
            "identifier": identifier,
            "delay_offset_samples": delay_offset,
            "blend_controls": {
                str(int(frequency)): float(value)
                for frequency, value in zip(CONTROL_FREQUENCIES_HZ[1:], blends)
            },
            "omitted_periodic_energy_ratio": omitted_energy,
            "maximum_magnitude_delta_db_0_20khz": magnitude_delta_db,
            "metrics": metrics,
            "passes_impulse_guards": _passes_guards(metrics, magnitude_delta_db),
        }
        if record["passes_impulse_guards"]:
            record["packet_15khz"] = asdict(_measure_packet(response, 15_000.0))
            characters[identifier] = candidate
        records.append(record)

    safe = [record for record in records if record["passes_impulse_guards"]]
    pareto = [
        record
        for record in safe
        if not any(_dominates(other, record) for other in safe if other is not record)
    ]
    ranked = sorted(
        pareto,
        key=lambda record: (
            record["metrics"]["maximum_post_lobe_db_peak"],
            record["metrics"]["post_energy_db_total"],
            record["metrics"]["step_undershoot_percent"],
            record["packet_15khz"]["onset_pre_echo_energy_db_total"],
        ),
    )
    work_dir.mkdir(parents=True, exist_ok=True)
    finalist_dir = work_dir / "pareto"
    finalist_dir.mkdir(parents=True, exist_ok=True)
    for record in ranked:
        payload = np.asarray(characters[record["identifier"]], dtype="<f8").tobytes(order="C")
        candidate_path = finalist_dir / f"{record['identifier']}.f64le"
        candidate_path.write_bytes(payload)
        record["character_file"] = str(candidate_path.relative_to(work_dir)).replace("\\", "/")
        record["character_sha256"] = _sha256_bytes(payload)

    report = {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "source": {
            "character": str(character_path.relative_to(root)).replace("\\", "/"),
            "character_sha256": _sha256_bytes(character_path.read_bytes()),
            "cleanup_stage_1": str(cleanup_path.relative_to(root)).replace("\\", "/"),
            "cleanup_stage_1_sha256": _sha256_bytes(cleanup_path.read_bytes()),
        },
        "search": {
            "method": "deterministic unscrambled Sobol with P3 seed",
            "candidate_count": len(records),
            "impulse_guard_qualified_count": len(safe),
            "pareto_count": len(ranked),
            "control_frequencies_hz": CONTROL_FREQUENCIES_HZ.tolist(),
            "delay_range_samples": DELAY_RANGE_SAMPLES,
            "blend_ranges": BLEND_RANGES,
        },
        "hard_guards": {
            "maximum_pre_lobe_db_peak_max": -22.5,
            "pre_energy_db_total_max": -4.85,
            "main_lobe_width_us_max": 62.5,
            "runtime_step_overshoot_percent_max": 13.4,
            "proxy_step_overshoot_percent_max": PROXY_OVERSHOOT_GUARD_PERCENT,
            "decay_120_ms_max": 7.0,
            "maximum_magnitude_delta_db_0_20khz": 5.0e-7,
            "frequency_and_dsd_non_regression": "required again for exact runtime finalists",
        },
        "objectives": [
            "maximum_post_lobe_db_peak",
            "post_energy_db_total",
            "step_undershoot_percent",
            "15 kHz onset_pre_echo_energy_db_total",
        ],
        "pareto": ranked,
        "candidates": records,
    }
    report_path = work_dir / "e3_packet_phase_search.json"
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    return report


def main() -> None:
    parser = argparse.ArgumentParser(description="Run the packet-aware Split Phase E3-P2 search")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--candidate-count", type=int, default=1024)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    work_dir = (
        arguments.work_dir or root / "tools/split_phase_v6/work-e3-p2"
    ).resolve()
    result = search(root, work_dir, arguments.candidate_count)
    print(json.dumps({"search": result["search"], "pareto": result["pareto"]}, indent=2))


if __name__ == "__main__":
    main()
