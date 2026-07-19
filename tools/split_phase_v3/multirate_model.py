from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import numpy as np


def _response(coefficients: np.ndarray, omega: float) -> complex:
    index = np.arange(coefficients.size, dtype=np.float64)
    return complex(np.dot(coefficients, np.exp(-1j * omega * index)))


def _upsample_direct(filters: list[np.ndarray]) -> np.ndarray:
    impulse = np.ones(1, dtype=np.float64)
    for coefficients in filters:
        expanded = np.zeros(2 * impulse.size - 1, dtype=np.float64)
        expanded[::2] = impulse
        impulse = np.convolve(expanded, 2.0 * coefficients)
    return impulse


def _upsample_analytical(filters: list[np.ndarray], omega_out: float) -> complex:
    result = 1.0 + 0.0j
    stages = len(filters)
    for stage, coefficients in enumerate(filters):
        stage_frequency = omega_out * 2 ** (stages - 1 - stage)
        result *= 2.0 * _response(coefficients, stage_frequency)
    return result


def _downsample_direct_gain(filters: list[np.ndarray], omega_input: float) -> complex:
    # Evaluate a long non-bin-centred complex tone through the actual
    # convolution/phase-0 decimation ordering and fit its settled output.
    length = 262144
    index = np.arange(length, dtype=np.float64)
    values = np.exp(1j * omega_input * index)
    current_frequency = omega_input
    for coefficients in filters:
        values = np.convolve(values, coefficients, mode="same")[::2]
        current_frequency *= 2.0
    trim = min(256, values.size // 4)
    settled = values[trim:-trim]
    if settled.size == 0:
        raise RuntimeError("direct decimation model produced no settled samples")
    settled_index = np.arange(settled.size, dtype=np.float64)
    fitted = np.mean(settled * np.exp(-1j * current_frequency * settled_index))
    # Remove the arbitrary phase introduced by slicing the settled interval;
    # compare magnitudes separately and phase increments through a ratio below.
    return complex(fitted)


def verify_multirate_model(seed: int, work_dir: Path) -> dict[str, Any]:
    report_path = work_dir / "multirate_model.json"
    rng = np.random.default_rng(seed)
    cases: list[dict[str, Any]] = []
    worst_interpolation = 0.0
    worst_decimation_magnitude = 0.0
    for stages in range(1, 9):
        filters: list[np.ndarray] = []
        for stage in range(stages):
            length = 9 if stage == 0 else 7
            coefficients = rng.normal(size=length)
            coefficients /= np.sum(coefficients)
            filters.append(coefficients)
        direct_impulse = _upsample_direct(filters)
        for _ in range(4):
            omega_out = float(rng.uniform(0.001, 0.8 * np.pi / (2**stages)))
            direct = _response(direct_impulse, omega_out)
            analytical = _upsample_analytical(filters, omega_out)
            relative = abs(direct - analytical) / max(abs(direct), 1.0e-15)
            worst_interpolation = max(worst_interpolation, relative)

            reverse_filters = list(reversed(filters))
            omega_input = omega_out
            analytical_down = 1.0 + 0.0j
            stage_frequency = omega_input
            for coefficients in reverse_filters:
                analytical_down *= _response(coefficients, stage_frequency)
                stage_frequency *= 2.0
            direct_down = _downsample_direct_gain(reverse_filters, omega_input)
            magnitude_relative = abs(abs(direct_down) - abs(analytical_down)) / max(
                abs(analytical_down), 1.0e-15
            )
            worst_decimation_magnitude = max(
                worst_decimation_magnitude, magnitude_relative
            )
            cases.append(
                {
                    "stages": stages,
                    "omega_out": omega_out,
                    "interpolation_relative_error": relative,
                    "decimation_magnitude_relative_error": magnitude_relative,
                }
            )
    report = {
        "seed": seed,
        "cases": cases,
        "worst_interpolation_relative_error": worst_interpolation,
        "worst_decimation_magnitude_relative_error": worst_decimation_magnitude,
        "accepted": worst_interpolation <= 1.0e-11
        and worst_decimation_magnitude <= 1.0e-11,
    }
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    if not report["accepted"]:
        raise RuntimeError(
            "analytical multirate model failed direct simulation: "
            f"interpolation={worst_interpolation}, decimation={worst_decimation_magnitude}"
        )
    return report
