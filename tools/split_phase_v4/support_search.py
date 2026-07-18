from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import numpy as np


def _bandlimited_pre_energy(response: np.ndarray, impulse_size: int, fft_len: int, frequency: np.ndarray, lo_hz: float, hi_hz: float, peak: int) -> float:
    band = np.fft.irfft(response * ((frequency >= lo_hz) & (frequency <= hi_hz)), n=fft_len)[:impulse_size]
    return float(np.dot(band[:peak], band[:peak]) / max(float(np.dot(band, band)), 1.0e-300))


def score_candidate(impulse: np.ndarray, target: np.ndarray, fft_len: int) -> tuple[float, ...]:
    response = np.fft.rfft(impulse, n=fft_len)
    frequency = np.linspace(0.0, 44_100.0, response.size)
    passband = frequency <= 20_000.0
    stopband = frequency >= 22_050.0
    peak = int(np.argmax(np.abs(impulse)))
    total = max(float(np.dot(impulse, impulse)), 1.0e-300)
    edge = float(np.dot(impulse[:2048], impulse[:2048]) + np.dot(impulse[-2048:], impulse[-2048:])) / total
    step = np.cumsum(impulse)
    return (
        max(float(np.max(np.abs(response[stopband]))) / 1.0e-8 - 1.0, 0.0),
        _bandlimited_pre_energy(response, impulse.size, fft_len, frequency, 3000.0, 14000.0, peak),
        _bandlimited_pre_energy(response, impulse.size, fft_len, frequency, 14000.0, 20000.0, peak),
        float(np.dot(impulse[:peak], impulse[:peak])) / total,
        float(np.max(np.abs(response[passband] - target[passband]))),
        edge,
        float(max(np.max(step) - 1.0, -np.min(step), 0.0)),
    )


def _proxy_score(
    impulse: np.ndarray,
    band_3_14: np.ndarray,
    band_14_20: np.ndarray,
    omitted_energy: float,
) -> tuple[float, ...]:
    """Cheap ordering proxy; every stage's finalists receive the exact score."""
    peak = int(np.argmax(np.abs(impulse)))
    total = max(float(np.dot(impulse, impulse)), 1.0e-300)
    band_3_14_total = max(float(np.dot(band_3_14, band_3_14)), 1.0e-300)
    band_14_20_total = max(float(np.dot(band_14_20, band_14_20)), 1.0e-300)
    edge = float(np.dot(impulse[:2048], impulse[:2048]) + np.dot(impulse[-2048:], impulse[-2048:])) / total
    step = np.cumsum(impulse)
    return (
        float(np.dot(band_3_14[:peak], band_3_14[:peak])) / band_3_14_total,
        float(np.dot(band_14_20[:peak], band_14_20[:peak])) / band_14_20_total,
        float(np.dot(impulse[:peak], impulse[:peak])) / total,
        edge,
        float(max(np.max(step) - 1.0, -np.min(step), 0.0)),
        omitted_energy,
    )


def search(periodic_impulse: np.ndarray, target_spectrum: np.ndarray, support: int, c_origin: int, report_path: Path) -> tuple[np.ndarray, int, dict[str, Any]]:
    fft_len = periodic_impulse.size
    frequency = np.linspace(0.0, 44_100.0, target_spectrum.size)
    band_3_14_periodic = np.fft.irfft(target_spectrum * ((frequency >= 3000.0) & (frequency <= 14_000.0)), n=fft_len)
    band_14_20_periodic = np.fft.irfft(target_spectrum * ((frequency >= 14_000.0) & (frequency <= 20_000.0)), n=fft_len)
    periodic_energy = float(np.dot(periodic_impulse, periodic_impulse))
    candidates = []
    best = None
    for step, radius in ((64, 4096), (2, 128)):
        center = c_origin if best is None else best[1]
        stage = []
        for origin in range(center - radius, center + radius + 1, step):
            # If the periodic sequence is sliced at s, its raw response gains
            # exp(+j*w*s) and its logical origin moves from C_origin to
            # C_origin-s. Therefore s = C_origin-new_origin.
            start = (c_origin - origin) % fft_len
            indices = (start + np.arange(support)) % fft_len
            impulse = periodic_impulse[indices].copy()
            proxy = _proxy_score(
                impulse,
                band_3_14_periodic[indices],
                band_14_20_periodic[indices],
                max(periodic_energy - float(np.dot(impulse, impulse)), 0.0) / max(periodic_energy, 1.0e-300),
            )
            record = {"origin": origin, "proxy_score": list(proxy), "step": step}
            candidates.append(record)
            stage.append((proxy, origin, impulse, record))

        # Exact million-point scoring is reserved for the best temporal/edge
        # proxies. This preserves the specified two-stage origin search while
        # avoiding three large FFTs for every one of its 258 candidates.
        stage.sort(key=lambda item: item[0])
        temporal_finalists = stage[:8]
        edge_frequency_guards = sorted(stage, key=lambda item: item[0][-1])[:4]
        finalists = {item[1]: item for item in temporal_finalists + edge_frequency_guards}.values()
        stage_best = None
        for _, origin, impulse, record in finalists:
            phase_shift = c_origin - origin
            omega = np.linspace(0.0, np.pi, target_spectrum.size)
            aligned_target = target_spectrum * np.exp(1j * omega * phase_shift)
            score = score_candidate(impulse, aligned_target, fft_len)
            record["exact_score"] = list(score)
            if stage_best is None or score < stage_best[0]:
                stage_best = (score, origin, impulse)
        assert stage_best is not None
        best = stage_best
    assert best is not None
    report = {
        "reference": "Split Phase C exported origin",
        "c_origin": c_origin,
        "coarse": {"radius": 4096, "step": 64},
        "fine": {"radius": 128, "step": 2},
        "best_origin": best[1],
        "selected_raw_target_phase_shift_samples": c_origin - best[1],
        "best_score": list(best[0]),
        "exact_finalists_per_stage": "8 temporal plus 4 edge/frequency guards (deduplicated)",
        "proxy_score_order": ["3_14khz_pre_energy", "14_20khz_pre_energy", "dominant_peak_pre_energy", "edge_energy", "step_overshoot", "omitted_energy_guard_only"],
        "score_order": ["hard_frequency_violation", "3_14khz_pre_energy", "14_20khz_pre_energy", "dominant_peak_pre_energy", "complex_error", "edge_energy", "step_overshoot"],
        "candidates": candidates,
    }
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    return best[2], best[1], report
