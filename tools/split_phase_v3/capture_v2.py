from __future__ import annotations

import json
import math
from pathlib import Path
from typing import Any

import numpy as np


def _smootherstep7(value: np.ndarray) -> np.ndarray:
    t = np.clip(value, 0.0, 1.0)
    return t**4 * (35.0 + t * (-84.0 + t * (70.0 - 20.0 * t)))


def _minimum_spectrum(magnitude: np.ndarray, fft_len: int) -> np.ndarray:
    cepstrum = np.fft.irfft(np.log(np.maximum(magnitude, 1.0e-30)), n=fft_len)
    cepstrum[1 : fft_len // 2] *= 2.0
    cepstrum[fft_len // 2 + 1 :] = 0.0
    return np.exp(np.fft.rfft(cepstrum, n=fft_len))


def _v2_target_phase(minimum_phase: np.ndarray, fft_len: int) -> np.ndarray:
    lo = 3000.0 / 88200.0
    hi = 14000.0 / 88200.0
    floor = 0.038155
    lo_bin = int(round(lo * fft_len))
    join_bin = int(math.ceil(hi * fft_len + 0.5))
    reference_increment = minimum_phase[lo_bin] / lo_bin
    target = np.zeros_like(minimum_phase)
    index = np.arange(lo_bin + 1)
    target[: lo_bin + 1] = (
        (1.0 - floor) * reference_increment * index + floor * minimum_phase[: lo_bin + 1]
    )
    bins = np.arange(lo_bin + 1, join_bin + 1)
    frequency_mid = (bins - 0.5) / fft_len
    log_t = (np.log(frequency_mid) - math.log(lo)) / (math.log(hi) - math.log(lo))
    weight = floor + (1.0 - floor) * _smootherstep7(log_t)
    minimum_increment = minimum_phase[bins] - minimum_phase[bins - 1]
    base = (1.0 - weight) * reference_increment + weight * minimum_increment
    bump = np.maximum(log_t, 0.0) ** 4 * np.maximum(1.0 - log_t, 0.0) ** 4
    raw_join = target[lo_bin] + np.sum(base)
    amplitude = (minimum_phase[join_bin] - raw_join) / np.sum(bump)
    target[lo_bin + 1 : join_bin + 1] = target[lo_bin] + np.cumsum(
        base + amplitude * bump
    )
    target[join_bin:] = minimum_phase[join_bin:]
    return target


def _metrics(
    coefficients: np.ndarray, fft_len: int, target_spectrum: np.ndarray
) -> dict[str, float]:
    response = np.fft.rfft(coefficients, n=fft_len)
    frequency = np.linspace(0.0, 44100.0, response.size)
    magnitude = np.abs(response)
    pass_mask = frequency <= 20000.0
    stop_mask = frequency >= 22050.0
    transition = (frequency >= 3000.0) & (frequency <= 14000.0)
    pass_db = 20.0 * np.log10(np.maximum(magnitude[pass_mask], 1.0e-300))
    phase = np.unwrap(np.angle(response))
    delay = -np.gradient(phase, np.pi / (response.size - 1))
    curvature = np.gradient(np.gradient(delay[transition]))
    peak = int(np.argmax(np.abs(coefficients)))
    total_energy = float(np.dot(coefficients, coefficients))
    edge = float(
        np.dot(coefficients[:2048], coefficients[:2048])
        + np.dot(coefficients[-2048:], coefficients[-2048:])
    )
    pre_peak = float(np.dot(coefficients[:peak], coefficients[:peak]))
    step = np.cumsum(coefficients)
    return {
        "passband_ripple_db_peak_to_peak": float(np.max(pass_db) - np.min(pass_db)),
        "stopband_peak_db": 20.0
        * math.log10(max(float(np.max(magnitude[stop_mask])), 1.0e-300)),
        "maximum_group_delay_curvature": float(np.max(np.abs(curvature))),
        "edge_energy_db": 10.0 * math.log10(max(edge / total_energy, 1.0e-300)),
        "broadband_pre_peak_energy_db": 10.0
        * math.log10(max(pre_peak / total_energy, 1.0e-300)),
        "step_response_overshoot": float(max(np.max(step) - 1.0, 0.0)),
        "worst_complex_passband_approximation_error": float(
            np.max(np.abs(response[pass_mask] - target_spectrum[pass_mask]))
        ),
        "peak_index": peak,
    }


def capture_v2_baseline(
    fft_len: int, work_dir: Path, resume: bool = True
) -> dict[str, Any]:
    report_path = work_dir / "v2_baseline.json"
    if resume and report_path.exists():
        return json.loads(report_path.read_text())
    half_width = 65536
    support = 4 * half_width + 1
    position = np.arange(support, dtype=np.float64) * 0.5 - half_width
    radius = np.clip(position / half_width, -1.0, 1.0)
    window = np.i0(23.12088 * np.sqrt(np.maximum(1.0 - radius**2, 0.0))) / np.i0(
        23.12088
    )
    prototype = 2.0 * 0.465333 * np.sinc(2.0 * 0.465333 * position) * window
    prototype /= math.fsum(float(value) for value in prototype)
    linear_spectrum = np.fft.rfft(prototype, n=fft_len)
    peak_magnitude = float(np.max(np.abs(linear_spectrum)))
    cepstral_magnitude = np.maximum(np.abs(linear_spectrum), peak_magnitude * 1.0e-12)
    minimum_spectrum = _minimum_spectrum(cepstral_magnitude, fft_len)
    minimum_phase = np.unwrap(np.angle(minimum_spectrum))
    # Match the Rust unwrap-floor policy by carrying the previous reliable bin.
    reliable = np.abs(minimum_spectrum) > peak_magnitude * 1.0e-6
    last_reliable = np.maximum.accumulate(
        np.where(reliable, np.arange(minimum_phase.size), 0)
    )
    minimum_phase = minimum_phase[last_reliable]
    target_phase = _v2_target_phase(minimum_phase, fft_len)
    frequency = np.arange(target_phase.size, dtype=np.float64) / fft_len
    shift = (support // 64) * 1.040606
    spectrum = np.abs(linear_spectrum) * np.exp(
        1j * (target_phase - 2.0 * np.pi * frequency * shift)
    )
    spectrum[0] = spectrum[0].real
    spectrum[-1] = spectrum[-1].real
    impulse = np.fft.irfft(spectrum, n=fft_len)[:support]
    fade_length = min(max(int(round(support * 0.005621)), 8), 2048, support // 4)
    fade = 0.5 * (1.0 + np.cos(np.pi * np.arange(fade_length) / (fade_length - 1)))
    impulse[-fade_length:] *= fade
    impulse /= math.fsum(float(value) for value in impulse)
    report = {
        "identity": "Split128kV2",
        "fft_len": fft_len,
        "metrics": _metrics(impulse, fft_len, spectrum),
    }
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    return report
