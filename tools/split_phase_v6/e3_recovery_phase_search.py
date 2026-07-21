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
    FFT_LENGTH,
    _cascade_character_and_cleanup,
    _read_f64le,
    _timing_metrics,
)
from .evaluate_e3_packets import _measure_packet


IDENTITY = "SplitPhase128kE3-P4-recovery-tail-phase-refinement"
CONTROL_FREQUENCIES_HZ = np.asarray(
    [14_000.0, 14_750.0, 15_500.0, 16_500.0, 18_000.0, 19_500.0, 20_500.0, 22_050.0],
    dtype=np.float64,
)
PHASE_DELTA_LIMIT_RAD = 0.020
PROXY_OVERSHOOT_GUARD_PERCENT = 9.22


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _samples(count: int) -> np.ndarray:
    if count < 2 or count & (count - 1):
        raise ValueError("candidate count must be a power of two of at least two")
    unit = qmc.Sobol(d=1 + CONTROL_FREQUENCIES_HZ.size - 2, scramble=False).random_base2(
        int(math.log2(count))
    )
    lower = np.asarray([0.0, *([-PHASE_DELTA_LIMIT_RAD] * 6)], dtype=np.float64)
    upper = np.asarray([1.0, *([PHASE_DELTA_LIMIT_RAD] * 6)], dtype=np.float64)
    values = qmc.scale(unit, lower, upper)
    # Make both immutable anchors explicit members of every search.
    values[0] = 0.0
    values[1] = 0.0
    values[1, 0] = 1.0
    return values


def _candidate_character(
    anchor_spectrum: np.ndarray,
    anchor_phase: np.ndarray,
    line_phase_delta: np.ndarray,
    magnitude: np.ndarray,
    frequency_hz: np.ndarray,
    support: int,
    anchor_sum: float,
    line_fraction: float,
    controls: np.ndarray,
) -> tuple[np.ndarray, float]:
    control_values = np.concatenate(([0.0], controls, [0.0]))
    spline = CubicSpline(
        CONTROL_FREQUENCIES_HZ,
        control_values,
        bc_type=((1, 0.0), (1, 0.0)),
    )
    local_phase_delta = np.zeros_like(frequency_hz)
    active = (frequency_hz >= CONTROL_FREQUENCIES_HZ[0]) & (
        frequency_hz <= CONTROL_FREQUENCIES_HZ[-1]
    )
    local_phase_delta[active] = spline(frequency_hz[active])
    phase = anchor_phase + line_fraction * line_phase_delta + local_phase_delta
    target = magnitude * np.exp(1j * phase)
    target[0] = complex(float(anchor_spectrum[0].real), 0.0)
    target[-1] = complex(float(anchor_spectrum[-1].real), 0.0)
    periodic = np.fft.irfft(target, FFT_LENGTH)
    kept = periodic[:support].copy()
    kept *= anchor_sum / float(math.fsum(float(value) for value in kept))
    omitted_energy = float(np.dot(periodic[support:], periodic[support:])) / max(
        float(np.dot(periodic, periodic)), 1.0e-300
    )
    return kept, omitted_energy


def _passes_impulse_guards(metrics: dict[str, Any], magnitude_delta_db: float) -> bool:
    decay_120 = metrics["decay_120_ms"]
    return bool(
        metrics["maximum_pre_lobe_db_peak"] <= -22.5
        and metrics["pre_energy_db_total"] <= -4.85
        and metrics["maximum_post_lobe_db_peak"] <= -8.6
        and metrics["main_lobe_width_us"] <= 62.5
        and metrics["step_overshoot_percent"] <= PROXY_OVERSHOOT_GUARD_PERCENT
        and (decay_120 is None or decay_120 <= 7.0)
        and magnitude_delta_db <= 5.0e-7
    )


def _passes_guards(metrics: dict[str, Any], packet_15khz: dict[str, Any]) -> bool:
    return bool(packet_15khz["onset_pre_echo_energy_db_total"] <= -30.5)


def _dominates(left: dict[str, Any], right: dict[str, Any]) -> bool:
    left_metrics = left["metrics"]
    right_metrics = right["metrics"]
    objectives = (
        (left_metrics["tail_energy_db_at_4_ms"], right_metrics["tail_energy_db_at_4_ms"]),
        (left_metrics["tail_energy_db_at_8_ms"], right_metrics["tail_energy_db_at_8_ms"]),
        (left_metrics["maximum_post_lobe_db_peak"], right_metrics["maximum_post_lobe_db_peak"]),
        (left_metrics["post_energy_db_total"], right_metrics["post_energy_db_total"]),
        (left_metrics["step_undershoot_percent"], right_metrics["step_undershoot_percent"]),
        (
            left["packet_15khz"]["onset_pre_echo_energy_db_total"],
            right["packet_15khz"]["onset_pre_echo_energy_db_total"],
        ),
    )
    return all(a <= b for a, b in objectives) and any(a < b for a, b in objectives)


def search(
    root: Path,
    anchor_path: Path,
    line_target_path: Path,
    work_dir: Path,
    count: int,
    line_only: bool = False,
) -> dict[str, Any]:
    e2_assets = root / "assets/filters/split_phase_e2v3"
    e2_character = _read_f64le(e2_assets / "character_full_rate.f64le")
    cleanup = _read_f64le(e2_assets / "cleanup_stage_1.f64le")
    anchor = _read_f64le(anchor_path)
    line_target = _read_f64le(line_target_path)
    if anchor.shape != line_target.shape:
        raise RuntimeError("phase anchors have different coefficient support")

    anchor_spectrum = np.fft.rfft(anchor, FFT_LENGTH)
    target_spectrum = np.fft.rfft(line_target, FFT_LENGTH)
    anchor_phase = np.unwrap(np.angle(anchor_spectrum))
    line_phase_delta = np.unwrap(np.angle(target_spectrum / anchor_spectrum))
    frequency_hz = np.fft.rfftfreq(FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ)
    active = (frequency_hz >= CONTROL_FREQUENCIES_HZ[0]) & (
        frequency_hz <= CONTROL_FREQUENCIES_HZ[-1]
    )
    line_phase_delta[~active] = 0.0

    # Reproject every candidate onto the accepted E2v3 magnitude before finite-support truncation.
    magnitude = np.abs(np.fft.rfft(e2_character, FFT_LENGTH))
    reliable = frequency_hz <= 20_000.0
    anchor_sum = float(math.fsum(float(value) for value in anchor))

    records: list[dict[str, Any]] = []
    characters: dict[str, np.ndarray] = {}
    if line_only:
        if count < 2:
            raise ValueError("line-only candidate count must be at least two")
        samples = np.zeros((count, 1 + CONTROL_FREQUENCIES_HZ.size - 2), dtype=np.float64)
        samples[:, 0] = np.linspace(0.0, 1.0, count)
    else:
        samples = _samples(count)

    for index, sample in enumerate(samples):
        line_fraction = float(sample[0])
        controls = sample[1:]
        if index == 0:
            candidate = anchor.copy()
            omitted = 0.0
        elif (not line_only and index == 1) or (line_only and index == len(samples) - 1):
            candidate = line_target.copy()
            omitted = 0.0
        else:
            candidate, omitted = _candidate_character(
                anchor_spectrum,
                anchor_phase,
                line_phase_delta,
                magnitude,
                frequency_hz,
                anchor.size,
                anchor_sum,
                line_fraction,
                controls,
            )
        candidate_spectrum = np.fft.rfft(candidate, FFT_LENGTH)
        magnitude_error_db = 20.0 * np.log10(
            np.maximum(np.abs(candidate_spectrum[reliable]), 1.0e-300)
            / np.maximum(magnitude[reliable], 1.0e-300)
        )
        magnitude_delta = float(np.max(np.abs(magnitude_error_db)))
        response = _cascade_character_and_cleanup(candidate, cleanup)
        metrics = asdict(_timing_metrics(response))
        identifier = f"recovery-{index:04d}"
        record: dict[str, Any] = {
            "identifier": identifier,
            "line_fraction_refine0900_to_sobol0370": line_fraction,
            "phase_delta_controls_rad": {
                str(int(frequency)): float(value)
                for frequency, value in zip(CONTROL_FREQUENCIES_HZ[1:-1], controls)
            },
            "omitted_periodic_energy_ratio": omitted,
            "maximum_magnitude_delta_db_0_20khz": magnitude_delta,
            "metrics": metrics,
        }
        record["passes_impulse_guards"] = _passes_impulse_guards(metrics, magnitude_delta)
        if record["passes_impulse_guards"]:
            packet_15khz = asdict(_measure_packet(response, 15_000.0))
            record["packet_15khz"] = packet_15khz
            record["passes_search_guards"] = _passes_guards(metrics, packet_15khz)
        else:
            record["passes_search_guards"] = False
        if record["passes_search_guards"]:
            characters[identifier] = candidate
        records.append(record)

    safe = [record for record in records if record["passes_search_guards"]]
    pareto = [
        record
        for record in safe
        if not any(_dominates(other, record) for other in safe if other is not record)
    ]
    ranked = sorted(
        pareto,
        key=lambda record: (
            record["metrics"]["tail_energy_db_at_4_ms"],
            record["metrics"]["tail_energy_db_at_8_ms"],
            record["metrics"]["maximum_post_lobe_db_peak"],
            record["metrics"]["step_undershoot_percent"],
        ),
    )
    work_dir.mkdir(parents=True, exist_ok=True)
    pareto_dir = work_dir / ("line" if line_only else "pareto")
    pareto_dir.mkdir(parents=True, exist_ok=True)
    records_to_save = safe if line_only else ranked
    for record in records_to_save:
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
            "anchor_refine0900": str(anchor_path.relative_to(root)).replace("\\", "/"),
            "anchor_refine0900_sha256": _sha256_bytes(anchor_path.read_bytes()),
            "line_target_sobol0370": str(line_target_path.relative_to(root)).replace("\\", "/"),
            "line_target_sobol0370_sha256": _sha256_bytes(line_target_path.read_bytes()),
            "e2v3_character_sha256": _sha256_bytes(
                (e2_assets / "character_full_rate.f64le").read_bytes()
            ),
            "cleanup_stage_1_sha256": _sha256_bytes(
                (e2_assets / "cleanup_stage_1.f64le").read_bytes()
            ),
        },
        "search": {
            "method": (
                "deterministic reprojected phase-domain anchor line"
                if line_only
                else "deterministic phase-domain anchor line plus local unscrambled Sobol curvature"
            ),
            "candidate_count": len(records),
            "search_guard_qualified_count": len(safe),
            "pareto_count": len(ranked),
            "control_frequencies_hz": CONTROL_FREQUENCIES_HZ.tolist(),
            "phase_delta_limit_rad": PHASE_DELTA_LIMIT_RAD,
        },
        "hard_guards": {
            "maximum_pre_lobe_db_peak_max": -22.5,
            "pre_energy_db_total_max": -4.85,
            "maximum_post_lobe_db_peak_max": -8.6,
            "main_lobe_width_us_max": 62.5,
            "runtime_step_overshoot_percent_max": 12.8,
            "conservative_proxy_step_overshoot_percent_max": PROXY_OVERSHOOT_GUARD_PERCENT,
            "decay_120_ms_max": 7.0,
            "packet_15khz_onset_pre_echo_energy_db_total_max": -30.5,
            "maximum_magnitude_delta_db_0_20khz": 5.0e-7,
            "frequency_and_dsd_non_regression": "required again for exact runtime finalists",
        },
        "objectives": [
            "tail_energy_db_at_4_ms (high-frequency DSD recovery proxy)",
            "tail_energy_db_at_8_ms",
            "maximum_post_lobe_db_peak",
            "post_energy_db_total",
            "step_undershoot_percent",
            "15 kHz onset_pre_echo_energy_db_total",
        ],
        "pareto": ranked,
        "candidates": records,
    }
    (work_dir / "e3_recovery_phase_search.json").write_text(
        json.dumps(report, indent=2) + "\n", encoding="utf-8"
    )
    return report


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Search the guarded E3 frontier with an explicit recovery-tail objective"
    )
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--anchor", type=Path)
    parser.add_argument("--line-target", type=Path)
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--candidate-count", type=int, default=2048)
    parser.add_argument(
        "--line-only",
        action="store_true",
        help="evaluate evenly spaced, reprojected phase points with no local curvature",
    )
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    anchor = (
        arguments.anchor
        or root / "tools/split_phase_v6/work-e3-p3/pareto/refine-0900.f64le"
    ).resolve()
    line_target = (
        arguments.line_target
        or root / "tools/split_phase_v6/work-e3-p2/pareto/sobol-0370.f64le"
    ).resolve()
    work_dir = (
        arguments.work_dir or root / "tools/split_phase_v6/work-e3-p4"
    ).resolve()
    report = search(
        root,
        anchor,
        line_target,
        work_dir,
        arguments.candidate_count,
        arguments.line_only,
    )
    summary = {
        "search": report["search"],
        "ranked_pareto": [
            {
                "identifier": item["identifier"],
                "line_fraction": item["line_fraction_refine0900_to_sobol0370"],
                "metrics": item["metrics"],
                "packet_15khz": item["packet_15khz"],
                "character_file": item["character_file"],
                "character_sha256": item["character_sha256"],
            }
            for item in report["pareto"]
        ],
    }
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
