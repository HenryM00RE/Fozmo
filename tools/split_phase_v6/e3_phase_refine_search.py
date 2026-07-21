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

from .e3_packet_phase_search import _dominates, _passes_guards
from .e3_phase_search import (
    CHARACTER_RATE_HZ,
    FFT_LENGTH,
    _cascade_character_and_cleanup,
    _read_f64le,
    _timing_metrics,
)
from .evaluate_e3_packets import _measure_packet


IDENTITY = "SplitPhase128kE3-P3-local-phase-curvature-refinement"
CONTROL_FREQUENCIES_HZ = np.asarray(
    [14_000.0, 14_750.0, 15_500.0, 16_500.0, 18_000.0, 19_500.0, 20_500.0, 22_050.0],
    dtype=np.float64,
)
PHASE_DELTA_LIMIT_RAD = 0.12


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _samples(count: int) -> np.ndarray:
    if count <= 0 or count & (count - 1):
        raise ValueError("candidate count must be a positive power of two")
    unit = qmc.Sobol(d=CONTROL_FREQUENCIES_HZ.size - 2, scramble=False).random_base2(
        int(math.log2(count))
    )
    values = qmc.scale(unit, -PHASE_DELTA_LIMIT_RAD, PHASE_DELTA_LIMIT_RAD)
    values[0] = 0.0
    return values


def _candidate_character(
    base_spectrum: np.ndarray,
    base_phase: np.ndarray,
    magnitude: np.ndarray,
    frequency_hz: np.ndarray,
    support: int,
    base_sum: float,
    controls: np.ndarray,
) -> tuple[np.ndarray, float]:
    control_values = np.concatenate(([0.0], controls, [0.0]))
    spline = CubicSpline(
        CONTROL_FREQUENCIES_HZ,
        control_values,
        bc_type=((1, 0.0), (1, 0.0)),
    )
    phase_delta = np.zeros_like(frequency_hz)
    active = (frequency_hz >= CONTROL_FREQUENCIES_HZ[0]) & (
        frequency_hz <= CONTROL_FREQUENCIES_HZ[-1]
    )
    phase_delta[active] = spline(frequency_hz[active])
    target = magnitude * np.exp(1j * (base_phase + phase_delta))
    target[0] = complex(float(base_spectrum[0].real), 0.0)
    target[-1] = complex(float(base_spectrum[-1].real), 0.0)
    periodic = np.fft.irfft(target, FFT_LENGTH)
    kept = periodic[:support].copy()
    kept *= base_sum / float(math.fsum(float(value) for value in kept))
    omitted_energy = float(np.dot(periodic[support:], periodic[support:])) / max(
        float(np.dot(periodic, periodic)), 1.0e-300
    )
    return kept, omitted_energy


def search(root: Path, base_character_path: Path, work_dir: Path, count: int) -> dict[str, Any]:
    e2_assets = root / "assets/filters/split_phase_e2v3"
    e2_character = _read_f64le(e2_assets / "character_full_rate.f64le")
    cleanup = _read_f64le(e2_assets / "cleanup_stage_1.f64le")
    base_character = _read_f64le(base_character_path)
    base_spectrum = np.fft.rfft(base_character, FFT_LENGTH)
    base_phase = np.unwrap(np.angle(base_spectrum))
    magnitude = np.abs(base_spectrum)
    e2_magnitude = np.abs(np.fft.rfft(e2_character, FFT_LENGTH))
    frequency_hz = np.fft.rfftfreq(FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ)
    reliable = frequency_hz <= 20_000.0
    base_sum = float(math.fsum(float(value) for value in base_character))

    records: list[dict[str, Any]] = []
    characters: dict[str, np.ndarray] = {}
    for index, controls in enumerate(_samples(count)):
        candidate, omitted = _candidate_character(
            base_spectrum,
            base_phase,
            magnitude,
            frequency_hz,
            base_character.size,
            base_sum,
            controls,
        )
        candidate_spectrum = np.fft.rfft(candidate, FFT_LENGTH)
        magnitude_error_db = 20.0 * np.log10(
            np.maximum(np.abs(candidate_spectrum[reliable]), 1.0e-300)
            / np.maximum(e2_magnitude[reliable], 1.0e-300)
        )
        magnitude_delta = float(np.max(np.abs(magnitude_error_db)))
        response = _cascade_character_and_cleanup(candidate, cleanup)
        metrics = asdict(_timing_metrics(response))
        identifier = f"refine-{index:04d}"
        record: dict[str, Any] = {
            "identifier": identifier,
            "phase_delta_controls_rad": {
                str(int(frequency)): float(value)
                for frequency, value in zip(CONTROL_FREQUENCIES_HZ[1:-1], controls)
            },
            "omitted_periodic_energy_ratio": omitted,
            "maximum_magnitude_delta_db_0_20khz": magnitude_delta,
            "metrics": metrics,
            "passes_impulse_guards": _passes_guards(metrics, magnitude_delta),
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
    pareto_dir = work_dir / "pareto"
    pareto_dir.mkdir(parents=True, exist_ok=True)
    for record in ranked:
        payload = np.asarray(characters[record["identifier"]], dtype="<f8").tobytes(order="C")
        path = pareto_dir / f"{record['identifier']}.f64le"
        path.write_bytes(payload)
        record["character_file"] = str(path.relative_to(work_dir)).replace("\\", "/")
        record["character_sha256"] = _sha256_bytes(payload)

    report = {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "source": {
            "base_character": str(base_character_path.relative_to(root)).replace("\\", "/"),
            "base_character_sha256": _sha256_bytes(base_character_path.read_bytes()),
            "e2v3_character_sha256": _sha256_bytes(
                (e2_assets / "character_full_rate.f64le").read_bytes()
            ),
            "cleanup_stage_1_sha256": _sha256_bytes(
                (e2_assets / "cleanup_stage_1.f64le").read_bytes()
            ),
        },
        "search": {
            "method": "deterministic local unscrambled Sobol phase-delta refinement",
            "candidate_count": len(records),
            "impulse_guard_qualified_count": len(safe),
            "pareto_count": len(ranked),
            "control_frequencies_hz": CONTROL_FREQUENCIES_HZ.tolist(),
            "phase_delta_limit_rad": PHASE_DELTA_LIMIT_RAD,
        },
        "hard_guards": {
            "maximum_pre_lobe_db_peak_max": -22.5,
            "pre_energy_db_total_max": -4.85,
            "main_lobe_width_us_max": 62.5,
            "runtime_step_overshoot_percent_max": 13.4,
            "proxy_step_overshoot_percent_max": 9.70,
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
    (work_dir / "e3_phase_refine_search.json").write_text(
        json.dumps(report, indent=2) + "\n", encoding="utf-8"
    )
    return report


def main() -> None:
    parser = argparse.ArgumentParser(description="Refine an E3 finalist with local phase curvature")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--base-character", type=Path)
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--candidate-count", type=int, default=1024)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    base_character = (
        arguments.base_character
        or root / "tools/split_phase_v6/work-e3-p2/pareto/sobol-0711.f64le"
    ).resolve()
    work_dir = (
        arguments.work_dir or root / "tools/split_phase_v6/work-e3-p3"
    ).resolve()
    report = search(root, base_character, work_dir, arguments.candidate_count)
    print(json.dumps({"search": report["search"], "pareto": report["pareto"]}, indent=2))


if __name__ == "__main__":
    main()
