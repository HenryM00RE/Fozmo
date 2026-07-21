from __future__ import annotations

import argparse
import hashlib
import json
import math
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

import numpy as np


IDENTITY = "SplitPhase128kE3-P-treble-phase-search-experimental"
FFT_LENGTH = 1 << 20
CHARACTER_RATE_HZ = 88_200.0
OUTPUT_RATE_HZ = 176_400.0
LOW_JOIN_HZ = 14_000.0
CLOSURE_START_HZ = 20_500.0
CLOSURE_END_HZ = 22_050.0
JOIN_HZ = (16_500.0, 18_000.0, 19_000.0, 20_000.0, 20_500.0)
DELAY_OFFSETS = (-50.0, -37.0, -25.0, -17.0, 0.0, 13.0, 25.0)
STRENGTHS = (0.05, 0.10, 0.20, 0.35, 0.50, 0.65, 0.75, 0.90, 1.0)


@dataclass(frozen=True)
class TimingMetrics:
    pre_energy_db_total: float
    maximum_pre_lobe_db_peak: float
    post_energy_db_total: float
    maximum_post_lobe_db_peak: float
    main_lobe_width_us: float
    step_overshoot_percent: float
    step_undershoot_percent: float
    decay_80_ms: float | None
    decay_120_ms: float | None
    centroid_relative_to_peak_ms: float
    tail_energy_db_at_1_ms: float
    tail_energy_db_at_2_ms: float
    tail_energy_db_at_4_ms: float
    tail_energy_db_at_8_ms: float
    tail_energy_db_at_16_ms: float


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _read_f64le(path: Path) -> np.ndarray:
    values = np.fromfile(path, dtype="<f8")
    if values.size == 0 or not np.all(np.isfinite(values)):
        raise RuntimeError(f"invalid coefficient asset: {path}")
    return np.asarray(values, dtype=np.float64)


def _smoothstep5(value: np.ndarray) -> np.ndarray:
    x = np.clip(value, 0.0, 1.0)
    return x**3 * (10.0 + x * (-15.0 + 6.0 * x))


def _treble_weight(frequency_hz: np.ndarray, join_hz: float) -> np.ndarray:
    rise = _smoothstep5((frequency_hz - LOW_JOIN_HZ) / (join_hz - LOW_JOIN_HZ))
    fall = 1.0 - _smoothstep5(
        (frequency_hz - CLOSURE_START_HZ) / (CLOSURE_END_HZ - CLOSURE_START_HZ)
    )
    weight = rise * fall
    weight[(frequency_hz < LOW_JOIN_HZ) | (frequency_hz > CLOSURE_END_HZ)] = 0.0
    return weight


def _cascade_character_and_cleanup(character: np.ndarray, cleanup: np.ndarray) -> np.ndarray:
    upsampled = np.zeros((character.size - 1) * 2 + 1, dtype=np.float64)
    upsampled[::2] = 2.0 * character
    output_length = upsampled.size + cleanup.size - 1
    fft_length = 1 << (output_length - 1).bit_length()
    spectrum = np.fft.rfft(upsampled, fft_length)
    spectrum *= np.fft.rfft(2.0 * cleanup, fft_length)
    return np.fft.irfft(spectrum, fft_length)[:output_length]


def _interpolated_zero(i0: int, y0: float, i1: int, y1: float) -> float:
    denominator = abs(y0) + abs(y1)
    if denominator <= np.finfo(np.float64).eps:
        return 0.5 * (i0 + i1)
    return i0 + abs(y0) / denominator


def _decay_ms(response: np.ndarray, peak_index: int, peak: float, threshold_db: float) -> float | None:
    threshold = peak * 10.0 ** (threshold_db / 20.0)
    indices = np.flatnonzero(np.abs(response[peak_index + 1 :]) > threshold)
    if indices.size == 0:
        return None
    return float((indices[-1] + 1) / OUTPUT_RATE_HZ * 1000.0)


def _power_db(value: float, reference: float) -> float:
    if value <= 0.0 or reference <= 0.0:
        return -300.0
    return max(10.0 * math.log10(value / reference), -300.0)


def _amplitude_db(value: float, reference: float) -> float:
    if value <= 0.0 or reference <= 0.0:
        return -300.0
    return max(20.0 * math.log10(value / reference), -300.0)


def _timing_metrics(response: np.ndarray) -> TimingMetrics:
    peak_index = int(np.argmax(np.abs(response)))
    peak = float(abs(response[peak_index]))
    energy_samples = response * response
    total_energy = float(math.fsum(float(value) for value in energy_samples))
    pre_energy = float(math.fsum(float(value) for value in energy_samples[:peak_index]))
    post_energy = float(math.fsum(float(value) for value in energy_samples[peak_index + 1 :]))
    sign = math.copysign(1.0, float(response[peak_index]))
    left_index = peak_index
    while (
        left_index > 0
        and response[left_index - 1] != 0.0
        and math.copysign(1.0, float(response[left_index - 1])) == sign
    ):
        left_index -= 1
    right_index = peak_index
    while (
        right_index + 1 < response.size
        and response[right_index + 1] != 0.0
        and math.copysign(1.0, float(response[right_index + 1])) == sign
    ):
        right_index += 1
    left = (
        _interpolated_zero(
            left_index - 1,
            float(response[left_index - 1]),
            left_index,
            float(response[left_index]),
        )
        if left_index > 0
        else 0.0
    )
    right = (
        _interpolated_zero(
            right_index,
            float(response[right_index]),
            right_index + 1,
            float(response[right_index + 1]),
        )
        if right_index + 1 < response.size
        else float(response.size - 1)
    )
    pre_lobe = float(np.max(np.abs(response[:left_index]))) if left_index else 0.0
    post_lobe = (
        float(np.max(np.abs(response[right_index + 1 :])))
        if right_index + 1 < response.size
        else 0.0
    )
    centroid = float(np.dot(np.arange(response.size, dtype=np.float64), energy_samples) / total_energy)
    step = np.cumsum(response)
    settled = float(math.fsum(float(value) for value in response))
    overshoot = max(float(np.max(step) / settled - 1.0), 0.0) * 100.0
    undershoot = max(float(-np.min(step) / settled), 0.0) * 100.0

    def tail_energy(milliseconds: float) -> float:
        start = peak_index + int(math.ceil(milliseconds / 1000.0 * OUTPUT_RATE_HZ))
        return _power_db(float(np.dot(response[start:], response[start:])), total_energy)

    return TimingMetrics(
        pre_energy_db_total=_power_db(pre_energy, total_energy),
        maximum_pre_lobe_db_peak=_amplitude_db(pre_lobe, peak),
        post_energy_db_total=_power_db(post_energy, total_energy),
        maximum_post_lobe_db_peak=_amplitude_db(post_lobe, peak),
        main_lobe_width_us=(right - left) / OUTPUT_RATE_HZ * 1_000_000.0,
        step_overshoot_percent=overshoot,
        step_undershoot_percent=undershoot,
        decay_80_ms=_decay_ms(response, peak_index, peak, -80.0),
        decay_120_ms=_decay_ms(response, peak_index, peak, -120.0),
        centroid_relative_to_peak_ms=(centroid - peak_index) / OUTPUT_RATE_HZ * 1000.0,
        tail_energy_db_at_1_ms=tail_energy(1.0),
        tail_energy_db_at_2_ms=tail_energy(2.0),
        tail_energy_db_at_4_ms=tail_energy(4.0),
        tail_energy_db_at_8_ms=tail_energy(8.0),
        tail_energy_db_at_16_ms=tail_energy(16.0),
    )


def _passes_safety(metrics: TimingMetrics, baseline: TimingMetrics) -> bool:
    return bool(
        metrics.pre_energy_db_total <= -4.90
        and metrics.maximum_pre_lobe_db_peak <= -17.0
        and metrics.main_lobe_width_us <= 68.75
        and (metrics.decay_120_ms is None or metrics.decay_120_ms <= 10.0)
        and metrics.step_undershoot_percent <= baseline.step_undershoot_percent
    )


def _dominates(left: dict[str, Any], right: dict[str, Any]) -> bool:
    left_metrics = left["metrics"]
    right_metrics = right["metrics"]
    objectives = (
        "post_energy_db_total",
        "maximum_post_lobe_db_peak",
        "main_lobe_width_us",
        "step_overshoot_percent",
        "step_undershoot_percent",
    )
    no_worse = all(left_metrics[key] <= right_metrics[key] for key in objectives)
    strictly_better = any(left_metrics[key] < right_metrics[key] for key in objectives)
    return no_worse and strictly_better


def _candidate_character(
    baseline_spectrum: np.ndarray,
    baseline_phase: np.ndarray,
    magnitude: np.ndarray,
    omega: np.ndarray,
    frequency_hz: np.ndarray,
    baseline_sum: float,
    support: int,
    peak_delay: float,
    join_hz: float,
    delay_offset: float,
    strength: float,
) -> tuple[np.ndarray, float]:
    low_index = int(np.argmin(np.abs(frequency_hz - LOW_JOIN_HZ)))
    target_delay = peak_delay + delay_offset
    linear_phase = baseline_phase[low_index] - target_delay * (omega - omega[low_index])
    weight = _treble_weight(frequency_hz, join_hz)
    phase = baseline_phase + strength * weight * (linear_phase - baseline_phase)
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


def search(root: Path, work_dir: Path) -> dict[str, Any]:
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
    baseline_response = _cascade_character_and_cleanup(character, cleanup)
    baseline_metrics = _timing_metrics(baseline_response)
    baseline_magnitude = np.abs(spectrum)
    reliable = frequency_hz <= 20_000.0

    records: list[dict[str, Any]] = []
    characters: dict[str, np.ndarray] = {}
    for join_hz in JOIN_HZ:
        for delay_offset in DELAY_OFFSETS:
            for strength in STRENGTHS:
                candidate, omitted_energy = _candidate_character(
                    spectrum,
                    phase,
                    magnitude,
                    omega,
                    frequency_hz,
                    float(math.fsum(float(value) for value in character)),
                    character.size,
                    peak_delay,
                    join_hz,
                    delay_offset,
                    strength,
                )
                candidate_spectrum = np.fft.rfft(candidate, FFT_LENGTH)
                magnitude_error_db = 20.0 * np.log10(
                    np.maximum(np.abs(candidate_spectrum[reliable]), 1.0e-300)
                    / np.maximum(baseline_magnitude[reliable], 1.0e-300)
                )
                metrics = _timing_metrics(_cascade_character_and_cleanup(candidate, cleanup))
                identifier = (
                    f"join-{int(join_hz)}_delay-{delay_offset:+.0f}_strength-{strength:.2f}"
                )
                payload = np.asarray(candidate, dtype="<f8").tobytes(order="C")
                record = {
                    "identifier": identifier,
                    "join_hz": join_hz,
                    "delay_offset_samples": delay_offset,
                    "strength": strength,
                    "character_sha256": _sha256_bytes(payload),
                    "omitted_periodic_energy_ratio": omitted_energy,
                    "maximum_magnitude_delta_db_0_20khz": float(
                        np.max(np.abs(magnitude_error_db))
                    ),
                    "metrics": asdict(metrics),
                    "passes_safety": _passes_safety(metrics, baseline_metrics),
                }
                records.append(record)
                characters[identifier] = candidate

    safe = [record for record in records if record["passes_safety"]]
    pareto = [
        record
        for record in safe
        if not any(_dominates(other, record) for other in safe if other is not record)
    ]
    if not pareto:
        raise RuntimeError("E3 phase search produced no safety-qualified Pareto candidate")
    winner = min(
        pareto,
        key=lambda record: (
            record["metrics"]["maximum_post_lobe_db_peak"],
            record["metrics"]["post_energy_db_total"],
            record["metrics"]["main_lobe_width_us"],
            record["identifier"],
        ),
    )
    work_dir.mkdir(parents=True, exist_ok=True)
    pareto_dir = work_dir / "pareto"
    pareto_dir.mkdir(parents=True, exist_ok=True)
    for record in pareto:
        candidate_path = pareto_dir / f"{record['identifier']}.f64le"
        candidate_path.write_bytes(
            np.asarray(characters[record["identifier"]], dtype="<f8").tobytes(order="C")
        )
        record["character_file"] = str(candidate_path.relative_to(work_dir)).replace("\\", "/")
    winner_character = characters[winner["identifier"]]
    winner_path = work_dir / "winner_character.f64le"
    winner_path.write_bytes(np.asarray(winner_character, dtype="<f8").tobytes(order="C"))
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
            "fft_length": FFT_LENGTH,
            "low_join_hz": LOW_JOIN_HZ,
            "join_hz": JOIN_HZ,
            "closure_start_hz": CLOSURE_START_HZ,
            "closure_end_hz": CLOSURE_END_HZ,
            "delay_offsets_samples": DELAY_OFFSETS,
            "strengths": STRENGTHS,
            "candidate_count": len(records),
            "safety_qualified_count": len(safe),
            "pareto_count": len(pareto),
        },
        "safety_gates": {
            "pre_energy_db_total_max": -4.90,
            "maximum_pre_lobe_db_peak_max": -17.0,
            "main_lobe_width_us_max": 68.75,
            "decay_120_ms_max": 10.0,
            "step_undershoot_percent_max": baseline_metrics.step_undershoot_percent,
        },
        "baseline_metrics": asdict(baseline_metrics),
        "winner": winner,
        "winner_file": winner_path.name,
        "pareto": pareto,
        "candidates": records,
    }
    report_path = work_dir / "e3_phase_search.json"
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    return report


def main() -> None:
    parser = argparse.ArgumentParser(description="Run the bounded Split Phase E3-P search")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--work-dir", type=Path)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    work_dir = (arguments.work_dir or root / "tools/split_phase_v6/work-e3-p").resolve()
    result = search(root, work_dir)
    print(json.dumps({"winner": result["winner"], "search": result["search"]}, indent=2))


if __name__ == "__main__":
    main()
