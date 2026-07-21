from __future__ import annotations

import argparse
import hashlib
import json
import math
from dataclasses import asdict
from pathlib import Path
from typing import Any

import numpy as np
from scipy import linalg
from scipy.stats import qmc

from tools.split_phase_v4.group_delay_spline import ConstrainedDelaySpline, _basis

from .e3_phase_search import (
    CHARACTER_RATE_HZ,
    FFT_LENGTH,
    _cascade_character_and_cleanup,
    _read_f64le,
    _timing_metrics,
)
from .evaluate_e3_packets import PACKET_FREQUENCIES_HZ, _measure_packet


IDENTITY = "SplitPhase128kE3-P5-constrained-group-delay-search-experimental"
STRUCTURES = (
    ("A", 12_000.0, 20_500.0, 24),
    ("B", 14_000.0, 20_500.0, 24),
    ("C", 14_000.0, 21_000.0, 30),
    ("D", 15_500.0, 21_000.0, 36),
)
CLOSURE_HZ = 22_050.0
DEGREE = 5
COORDINATE_RADIUS_SAMPLES = 0.50
CURVATURE_CAP_SAMPLES_PER_LN_HZ_SQUARED = 20_000.0
PHASE_DELTA_CAP_RAD = 0.10
PACKET_NON_REGRESSION_DB = 0.10
PROXY_OVERSHOOT_GUARD_PERCENT = 9.22
FINALIST_COUNT = 32
PACKET_QUALIFICATION_COUNT = 128


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _sha256_file(path: Path) -> str:
    return _sha256_bytes(path.read_bytes())


def _build_model(low_join_hz: float, treble_join_hz: float, controls: int) -> ConstrainedDelaySpline:
    lo = math.log(low_join_hz)
    join = math.log(treble_join_hz)
    hi = math.log(CLOSURE_HZ)
    interior_count = controls - DEGREE - 1
    closure_interior = max(2, round(interior_count * 0.20))
    main_interior = interior_count - closure_interior
    interior = np.concatenate(
        (
            np.linspace(lo, join, main_interior + 2)[1:-1],
            np.linspace(join, hi, closure_interior + 2)[1:-1],
        )
    )
    knots = np.concatenate((np.full(DEGREE + 1, lo), interior, np.full(DEGREE + 1, hi)))
    rows: list[np.ndarray] = []
    for coordinate in (lo, hi):
        for derivative in (0, 1, 2):
            row = np.zeros(controls + 1, dtype=np.float64)
            row[:controls] = _basis(
                knots, DEGREE, np.asarray([coordinate], dtype=np.float64), derivative
            )[0]
            rows.append(row)
    integration_frequency = np.linspace(low_join_hz, CLOSURE_HZ, 16_385)
    omega = 2.0 * np.pi * integration_frequency / CHARACTER_RATE_HZ
    integration_basis = _basis(knots, DEGREE, np.log(integration_frequency), 0)
    closure = np.zeros(controls + 1, dtype=np.float64)
    closure[:controls] = np.trapz(integration_basis, omega, axis=0)
    rows.append(closure)
    unused_low_delay = np.zeros(controls + 1, dtype=np.float64)
    unused_low_delay[-1] = 1.0
    rows.append(unused_low_delay)
    constraint = np.asarray(rows)
    nullspace = linalg.null_space(constraint)
    particular = np.zeros(controls + 1, dtype=np.float64)
    residual = float(np.max(np.abs(constraint @ particular)))
    expected_free = controls + 1 - np.linalg.matrix_rank(constraint)
    if residual > 1.0e-12 or nullspace.shape[1] != expected_free:
        raise RuntimeError("P5 constrained-delay nullspace construction failed")
    return ConstrainedDelaySpline(DEGREE, knots, particular, nullspace, residual)


def _model_hash(model: ConstrainedDelaySpline, structure: tuple[str, float, float, int]) -> str:
    digest = hashlib.sha256()
    digest.update(IDENTITY.encode("utf-8"))
    digest.update(repr(structure).encode("ascii"))
    for values in (model.knots, model.particular, model.nullspace):
        digest.update(np.asarray(values, dtype="<f8").tobytes())
    return digest.hexdigest()


def _samples(free_coordinates: int, count: int) -> np.ndarray:
    if count < 2 or count & (count - 1):
        raise ValueError("per-family candidate count must be a power of two")
    unit = qmc.Sobol(d=free_coordinates, scramble=False).random_base2(int(math.log2(count)))
    samples = (2.0 * unit - 1.0) * COORDINATE_RADIUS_SAMPLES
    samples[0] = 0.0
    if count > 1 and not np.any(samples[1]):
        samples[1, 0] = COORDINATE_RADIUS_SAMPLES
    return samples


def _candidate_character(
    anchor_phase: np.ndarray,
    magnitude: np.ndarray,
    frequency_hz: np.ndarray,
    model: ConstrainedDelaySpline,
    free: np.ndarray,
    low_join_hz: float,
    support: int,
    anchor_sum: float,
) -> tuple[np.ndarray, dict[str, float]]:
    active = (frequency_hz >= low_join_hz) & (frequency_hz <= CLOSURE_HZ)
    active_frequency = frequency_hz[active]
    delay_delta = model.evaluate(active_frequency, free)
    omega = 2.0 * np.pi * active_frequency / CHARACTER_RATE_HZ
    phase_delta = np.zeros_like(frequency_hz)
    phase_delta_active = np.zeros_like(active_frequency)
    phase_delta_active[1:] = -np.cumsum(
        0.5 * (delay_delta[1:] + delay_delta[:-1]) * np.diff(omega)
    )
    closure_error = float(phase_delta_active[-1])
    phase_delta[active] = phase_delta_active
    target = magnitude * np.exp(1j * (anchor_phase + phase_delta))
    target[0] = complex(float(target[0].real), 0.0)
    target[-1] = complex(float(target[-1].real), 0.0)
    periodic = np.fft.irfft(target, FFT_LENGTH)
    kept = periodic[:support].copy()
    omitted = float(np.dot(periodic[support:], periodic[support:])) / max(
        float(np.dot(periodic, periodic)), 1.0e-300
    )
    kept *= anchor_sum / float(math.fsum(float(value) for value in kept))
    curvature = model.evaluate(active_frequency, free, derivative=2)
    return kept, {
        "phase_closure_error_rad": closure_error,
        "maximum_phase_delta_rad": float(np.max(np.abs(phase_delta_active))),
        "maximum_group_delay_delta_samples": float(np.max(np.abs(delay_delta))),
        "maximum_group_delay_curvature_samples_per_ln_hz_squared": float(
            np.max(np.abs(curvature))
        ),
        "omitted_periodic_energy_ratio": omitted,
    }


def _impulse_guards(metrics: dict[str, Any], structural: dict[str, float], magnitude_delta: float) -> bool:
    decay = metrics["decay_120_ms"]
    return bool(
        metrics["maximum_pre_lobe_db_peak"] <= -22.5
        and metrics["pre_energy_db_total"] <= -4.85
        and metrics["maximum_post_lobe_db_peak"] <= -8.6
        and metrics["main_lobe_width_us"] <= 62.5
        and metrics["step_overshoot_percent"] <= PROXY_OVERSHOOT_GUARD_PERCENT
        and (decay is None or decay <= 7.0)
        and magnitude_delta <= 5.0e-7
        and abs(structural["phase_closure_error_rad"]) <= 5.0e-10
        and structural["maximum_phase_delta_rad"] <= PHASE_DELTA_CAP_RAD
        and structural[
            "maximum_group_delay_curvature_samples_per_ln_hz_squared"
        ]
        <= CURVATURE_CAP_SAMPLES_PER_LN_HZ_SQUARED
    )


def _tail_rms_db(response: np.ndarray, start_ms: float, end_ms: float) -> float:
    peak_index = int(np.argmax(np.abs(response)))
    peak = float(abs(response[peak_index]))
    start = peak_index + round(start_ms * 176_400.0 / 1000.0)
    end = peak_index + round(end_ms * 176_400.0 / 1000.0)
    values = response[start:end] / max(peak, 1.0e-300)
    return 20.0 * math.log10(max(float(np.sqrt(np.mean(values * values))), 1.0e-300))


def _ranking_key(record: dict[str, Any]) -> tuple[float, ...]:
    metrics = record["metrics"]
    return (
        record["tail_rms_db_peak_relative_2_5ms"],
        metrics["maximum_post_lobe_db_peak"],
        metrics["post_energy_db_total"],
        metrics["step_undershoot_percent"],
        metrics["main_lobe_width_us"],
    )


def search(root: Path, work_dir: Path, per_family: int) -> dict[str, Any]:
    assets = root / "assets/filters/split_phase_e2v3"
    anchor_path = root / "tools/split_phase_v6/work-e3-p3/pareto/refine-0900.f64le"
    anchor = _read_f64le(anchor_path)
    e2_character = _read_f64le(assets / "character_full_rate.f64le")
    cleanup = _read_f64le(assets / "cleanup_stage_1.f64le")
    anchor_spectrum = np.fft.rfft(anchor, FFT_LENGTH)
    anchor_phase = np.unwrap(np.angle(anchor_spectrum))
    magnitude = np.abs(np.fft.rfft(e2_character, FFT_LENGTH))
    frequency_hz = np.fft.rfftfreq(FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ)
    reliable = frequency_hz <= 20_000.0
    anchor_sum = float(math.fsum(float(value) for value in anchor))
    anchor_response = _cascade_character_and_cleanup(anchor, cleanup)
    e2_response = _cascade_character_and_cleanup(e2_character, cleanup)
    packet_anchor = {
        str(int(frequency)): asdict(_measure_packet(anchor_response, frequency))
        for frequency in PACKET_FREQUENCIES_HZ
    }
    packet_e2 = {
        str(int(frequency)): asdict(_measure_packet(e2_response, frequency))
        for frequency in PACKET_FREQUENCIES_HZ
    }

    records: list[dict[str, Any]] = []
    characters: dict[str, np.ndarray] = {}
    models = []
    global_index = 0
    for structure in STRUCTURES:
        family, low_join_hz, treble_join_hz, controls = structure
        model = _build_model(low_join_hz, treble_join_hz, controls)
        model_hash = _model_hash(model, structure)
        models.append(
            {
                "family": family,
                "low_join_hz": low_join_hz,
                "treble_join_hz": treble_join_hz,
                "closure_hz": CLOSURE_HZ,
                "controls": controls,
                "free_coordinates": model.free_coordinates,
                "constraint_residual": model.constraint_residual,
                "model_sha256": model_hash,
            }
        )
        for family_index, free in enumerate(_samples(model.free_coordinates, per_family)):
            identifier = f"p5-{family.lower()}-{family_index:04d}"
            if family_index == 0:
                candidate = anchor.copy()
                structural = {
                    "phase_closure_error_rad": 0.0,
                    "maximum_phase_delta_rad": 0.0,
                    "maximum_group_delay_delta_samples": 0.0,
                    "maximum_group_delay_curvature_samples_per_ln_hz_squared": 0.0,
                    "omitted_periodic_energy_ratio": 0.0,
                }
            else:
                candidate, structural = _candidate_character(
                    anchor_phase,
                    magnitude,
                    frequency_hz,
                    model,
                    free,
                    low_join_hz,
                    anchor.size,
                    anchor_sum,
                )
            candidate_spectrum = np.fft.rfft(candidate, FFT_LENGTH)
            magnitude_error = 20.0 * np.log10(
                np.maximum(np.abs(candidate_spectrum[reliable]), 1.0e-300)
                / np.maximum(magnitude[reliable], 1.0e-300)
            )
            magnitude_delta = float(np.max(np.abs(magnitude_error)))
            response = _cascade_character_and_cleanup(candidate, cleanup)
            metrics = asdict(_timing_metrics(response))
            record = {
                "index": global_index,
                "identifier": identifier,
                "family": family,
                "family_index": family_index,
                "model_sha256": model_hash,
                "free": free.tolist(),
                "free_sha256": _sha256_bytes(np.asarray(free, dtype="<f8").tobytes()),
                "structural": structural,
                "maximum_magnitude_delta_db_0_20khz": magnitude_delta,
                "tail_rms_db_peak_relative_2_5ms": _tail_rms_db(response, 2.0, 5.0),
                "tail_rms_db_peak_relative_5_10ms": _tail_rms_db(response, 5.0, 10.0),
                "metrics": metrics,
            }
            record["passes_impulse_guards"] = _impulse_guards(
                metrics, structural, magnitude_delta
            )
            if record["passes_impulse_guards"]:
                characters[identifier] = candidate
            records.append(record)
            global_index += 1

    impulse_safe = sorted(
        (record for record in records if record["passes_impulse_guards"]), key=_ranking_key
    )
    for record in impulse_safe[:PACKET_QUALIFICATION_COUNT]:
        response = _cascade_character_and_cleanup(characters[record["identifier"]], cleanup)
        packets = {
            str(int(frequency)): asdict(_measure_packet(response, frequency))
            for frequency in PACKET_FREQUENCIES_HZ
        }
        record["packets"] = packets
        record["packet_delta_db_vs_refine0900"] = {
            frequency: packets[frequency]["onset_pre_echo_energy_db_total"]
            - packet_anchor[frequency]["onset_pre_echo_energy_db_total"]
            for frequency in packets
        }
        record["packet_delta_db_vs_e2v3"] = {
            frequency: packets[frequency]["onset_pre_echo_energy_db_total"]
            - packet_e2[frequency]["onset_pre_echo_energy_db_total"]
            for frequency in packets
        }
        record["passes_packet_guards"] = all(
            delta <= PACKET_NON_REGRESSION_DB
            for delta in record["packet_delta_db_vs_refine0900"].values()
        )
    packet_safe = [record for record in impulse_safe if record.get("passes_packet_guards")]
    finalists = sorted(packet_safe, key=_ranking_key)[:FINALIST_COUNT]
    finalist_dir = work_dir / "finalists"
    finalist_dir.mkdir(parents=True, exist_ok=True)
    for record in finalists:
        payload = np.asarray(characters[record["identifier"]], dtype="<f8").tobytes()
        path = finalist_dir / f"{record['identifier']}.f64le"
        path.write_bytes(payload)
        record["character_file"] = str(path.relative_to(work_dir)).replace("\\", "/")
        record["character_sha256"] = _sha256_bytes(payload)

    report = {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "source": {
            "anchor_refine0900": str(anchor_path.relative_to(root)).replace("\\", "/"),
            "anchor_refine0900_sha256": _sha256_file(anchor_path),
            "e2v3_character_sha256": _sha256_file(assets / "character_full_rate.f64le"),
            "cleanup_stage_1_sha256": _sha256_file(assets / "cleanup_stage_1.f64le"),
        },
        "search": {
            "method": "unscrambled Sobol in an exact constrained-delay nullspace",
            "per_family": per_family,
            "candidate_count": len(records),
            "impulse_safe_count": len(impulse_safe),
            "packet_qualified_count": len(packet_safe),
            "finalist_count": len(finalists),
            "coordinate_radius_samples": COORDINATE_RADIUS_SAMPLES,
            "models": models,
        },
        "hard_guards": {
            "maximum_pre_lobe_db_peak_max": -22.5,
            "pre_energy_db_total_max": -4.85,
            "maximum_post_lobe_db_peak_max": -8.6,
            "main_lobe_width_us_max": 62.5,
            "proxy_step_overshoot_percent_max": PROXY_OVERSHOOT_GUARD_PERCENT,
            "decay_120_ms_max": 7.0,
            "maximum_magnitude_delta_db_0_20khz": 5.0e-7,
            "maximum_phase_delta_rad": PHASE_DELTA_CAP_RAD,
            "maximum_group_delay_curvature_samples_per_ln_hz_squared": CURVATURE_CAP_SAMPLES_PER_LN_HZ_SQUARED,
            "all_packet_onset_delta_db_vs_refine0900_max": PACKET_NON_REGRESSION_DB,
        },
        "objective_order": [
            "2-5 ms peak-normalized impulse-envelope RMS",
            "maximum post-lobe",
            "post energy",
            "step undershoot",
            "main-lobe width",
        ],
        "packet_reference_refine0900": packet_anchor,
        "packet_reference_e2v3": packet_e2,
        "finalists": finalists,
        "records": records,
    }
    work_dir.mkdir(parents=True, exist_ok=True)
    (work_dir / "e3_p5_group_delay_search.json").write_bytes(
        (json.dumps(report, indent=2) + "\n").encode("utf-8")
    )
    return report


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--per-family", type=int, default=1024)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    work_dir = (
        arguments.work_dir or root / "tools/split_phase_v6/work-e3-p5"
    ).resolve()
    report = search(root, work_dir, arguments.per_family)
    print(
        json.dumps(
            {
                "candidate_count": report["search"]["candidate_count"],
                "impulse_safe_count": report["search"]["impulse_safe_count"],
                "packet_qualified_count": report["search"]["packet_qualified_count"],
                "finalist_count": report["search"]["finalist_count"],
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
