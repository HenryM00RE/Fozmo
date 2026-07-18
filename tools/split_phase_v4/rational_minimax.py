from __future__ import annotations

import json
import math
from pathlib import Path
from typing import Any

import numpy as np


def _project_row_equalities(rows: np.ndarray) -> np.ndarray:
    result = np.asarray(rows, dtype=np.float64).copy()
    pivot = np.argmax(np.abs(result), axis=1)
    for _ in range(4):
        sums = np.asarray([math.fsum(float(value) for value in row) for row in result])
        for row_index, coefficient_index in enumerate(pivot):
            result[row_index, coefficient_index] += 1.0 - sums[row_index]
    return result


def _metrics(
    rows: np.ndarray,
    step_num: int,
    source_rate: int,
    pass_edge: float,
    stop_edge: float,
    desired: np.ndarray | None = None,
    fft_len: int = 16384,
) -> dict[str, float]:
    response = np.fft.rfft(rows, n=fft_len, axis=1)
    frequency = np.linspace(0.0, source_rate / 2.0, response.shape[1])
    passband = frequency <= pass_edge
    stopband = frequency >= stop_edge
    magnitude = np.abs(response[:, passband])
    phases = np.arange(rows.shape[0], dtype=np.float64)[:, None] / rows.shape[0]
    omega = 2.0 * np.pi * frequency[None, :] / source_rate
    compensated = response * np.exp(1j * omega * phases)
    discontinuity = np.max(np.abs(np.diff(compensated, axis=0)))
    runtime_order = (np.arange(rows.shape[0], dtype=np.int64) * step_num) % rows.shape[0]
    causal_compensation = np.exp(
        1j
        * omega
        * (rows.shape[1] // 2 + np.arange(rows.shape[0], dtype=np.float64)[:, None] / rows.shape[0])
    )
    runtime_gain = (response * causal_compensation)[runtime_order]
    modulation = np.fft.fft(runtime_gain, axis=0) / rows.shape[0]
    desired_modulation = np.maximum(np.abs(modulation[0, passband]), 1.0e-300)
    interpolation_image = float(np.max(np.abs(modulation[1:, passband]) / desired_modulation[None, :]))
    decimation_alias = float(
        np.max(np.abs(modulation[:, stopband]))
        / max(float(np.min(np.abs(modulation[0, passband]))), 1.0e-300)
    )
    result = {
        "passband_ripple_db": float(20.0 * np.log10(max(float(np.max(magnitude)), 1.0e-300) / max(float(np.min(magnitude)), 1.0e-300))),
        "stopband_peak_db": float(20.0 * np.log10(max(float(np.max(np.abs(response[:, stopband]))), 1.0e-300))),
        "maximum_row_sum_error": float(np.max(np.abs(np.asarray([math.fsum(float(value) for value in row) for row in rows]) - 1.0))),
        "phase_to_phase_compensated_discontinuity": float(discontinuity),
        "interpolation_image_db": float(20.0 * np.log10(max(interpolation_image, 1.0e-300))),
        "decimation_alias_db": float(20.0 * np.log10(max(decimation_alias, 1.0e-300))),
    }
    if desired is not None:
        result["maximum_row_complex_error"] = float(np.max(np.abs(response[:, passband] - desired[:, passband])))
    return result


def _desired_rows(
    target_spectrum: np.ndarray,
    origin: int,
    phase_den: int,
    row_taps: int,
    source_rate: int,
    target_rate: int,
    fft_len: int,
) -> tuple[np.ndarray, float, float]:
    frequency = np.linspace(0.0, source_rate / 2.0, fft_len // 2 + 1)
    omega = 2.0 * np.pi * frequency / source_rate
    reference_frequency = np.linspace(0.0, 44_100.0, target_spectrum.size)
    reference_omega = 2.0 * np.pi * reference_frequency / 88_200.0
    magnitude_reference = np.abs(target_spectrum)
    residual_phase_reference = np.unwrap(np.angle(target_spectrum)) + reference_omega * origin
    family_scale = source_rate / 44_100.0
    mapped_reference_frequency = frequency / family_scale
    magnitude = np.interp(mapped_reference_frequency, reference_frequency, magnitude_reference)
    residual_phase = np.interp(mapped_reference_frequency, reference_frequency, residual_phase_reference)
    pass_edge = 20_000.0 * family_scale
    stop_edge = 22_050.0 * family_scale
    if target_rate < source_rate:
        anti_alias_scale = target_rate / source_rate
        pass_edge *= anti_alias_scale
        stop_edge *= anti_alias_scale
        mapped_frequency = np.minimum(frequency / anti_alias_scale, source_rate / 2.0)
        magnitude = np.interp(mapped_frequency, frequency, magnitude)
        residual_phase = np.interp(mapped_frequency, frequency, residual_phase)
    transition = (frequency > pass_edge) & (frequency < stop_edge)
    if np.any(transition):
        t = (frequency[transition] - pass_edge) / (stop_edge - pass_edge)
        smooth = t**4 * (35.0 + t * (-84.0 + t * (70.0 - 20.0 * t)))
        magnitude[transition] *= 1.0 - smooth
    magnitude[frequency >= stop_edge] = 0.0
    phase = np.arange(phase_den, dtype=np.float64)[:, None] / phase_den
    desired = magnitude[None, :] * np.exp(1j * (residual_phase[None, :] - omega[None, :] * (row_taps // 2 + phase)))
    return desired, pass_edge, stop_edge


def optimize_table(
    initial_flat: np.ndarray,
    step_num: int,
    phase_den: int,
    row_taps: int,
    source_rate: int,
    target_rate: int,
    target_spectrum: np.ndarray,
    origin: int,
    iterations: int = 12,
) -> tuple[np.ndarray, dict[str, Any]]:
    c_warm_rows = _project_row_equalities(np.asarray(initial_flat).reshape(phase_den, row_taps))
    fft_len = 16384
    desired, pass_edge, stop_edge = _desired_rows(
        target_spectrum, origin, phase_den, row_taps, source_rate, target_rate, fft_len
    )
    history = []
    def objective_key(metrics: dict[str, float]) -> tuple[float, ...]:
        hard = max(metrics["passband_ripple_db"] / 2.0e-6 - 1.0, metrics["interpolation_image_db"] + 160.0, metrics["decimation_alias_db"] + 160.0, metrics["maximum_row_sum_error"] / 2.0e-15 - 1.0, metrics["maximum_row_complex_error"] / 2.0e-5 - 1.0, 0.0)
        return (hard, metrics["maximum_row_complex_error"], metrics["phase_to_phase_compensated_discontinuity"], metrics["passband_ripple_db"], metrics["stopband_peak_db"])

    target_warm_rows = _project_row_equalities(
        np.fft.irfft(desired, n=fft_len, axis=1)[:, :row_taps]
    )
    warm_candidates = []
    for label, candidate in (
        ("split_c_frozen_rows", c_warm_rows),
        ("joint_target_adjoint_warm_start", target_warm_rows),
    ):
        metrics = _metrics(candidate, step_num, source_rate, pass_edge, stop_edge, desired, fft_len)
        warm_candidates.append((objective_key(metrics), label, candidate, metrics))
    incumbent_key, selected_warm_start, rows, before = min(warm_candidates, key=lambda item: item[0])
    rows = rows.copy()
    incumbent_key = objective_key(before)
    for iteration in range(iterations):
        response = np.fft.rfft(rows, n=fft_len, axis=1)
        residual = desired - response
        correction = np.fft.irfft(residual, n=fft_len, axis=1)[:, :row_taps]
        norm = np.linalg.norm(correction)
        if norm > 2.0e-4:
            correction *= 2.0e-4 / norm
        accepted = False
        step = 1.0
        candidate_metrics = before
        for _ in range(10):
            candidate = _project_row_equalities(rows + step * correction)
            metrics = _metrics(candidate, step_num, source_rate, pass_edge, stop_edge, desired, fft_len)
            key = objective_key(metrics)
            if key < incumbent_key:
                rows = candidate
                before = metrics
                incumbent_key = key
                candidate_metrics = metrics
                accepted = True
                break
            step *= 0.5
        history.append({"iteration": iteration + 1, "accepted": accepted, "step": step, "metrics": candidate_metrics})
        if not accepted:
            break
    final = _metrics(rows, step_num, source_rate, pass_edge, stop_edge, desired, fft_len)
    report = {
        "method": "joint matrix-free fine-grid polyphase minimax with exact affine row constraints",
        "phase_den": phase_den,
        "step_num": step_num,
        "row_taps": row_taps,
        "source_rate": source_rate,
        "target_rate": target_rate,
        "selected_warm_start": selected_warm_start,
        "warm_start_metrics": {label: metrics for _, label, _, metrics in warm_candidates},
        "target_ifft_used_for_warm_start_only": True,
        "production_constructed_by_ifft_row_slicing": False,
        "no_post_normalization": True,
        "history": history,
        **final,
    }
    report["accepted"] = bool(
        final["passband_ripple_db"] <= 2.0e-6
        and final["maximum_row_sum_error"] <= 2.0e-15
        and final["interpolation_image_db"] <= -160.0
        and final["decimation_alias_db"] <= -160.0
        and final["maximum_row_complex_error"] <= 2.0e-5
    )
    return rows.reshape(-1), report


def optimize_both(asset_dir_c: Path, target_spectrum: np.ndarray, origin: int, work_dir: Path) -> tuple[np.ndarray, np.ndarray, dict[str, Any]]:
    first_initial = np.fromfile(asset_dir_c / "rational_147_160.f64le", dtype="<f8")
    second_initial = np.fromfile(asset_dir_c / "rational_160_147.f64le", dtype="<f8")
    first, first_report = optimize_table(first_initial, 147, 160, 1025, 44_100, 48_000, target_spectrum, origin)
    second, second_report = optimize_table(second_initial, 160, 147, 2049, 48_000, 44_100, target_spectrum, origin)
    report = {"rational_147_160": first_report, "rational_160_147": second_report}
    work_dir.mkdir(parents=True, exist_ok=True)
    np.save(work_dir / "rational_147_160.npy", first)
    np.save(work_dir / "rational_160_147.npy", second)
    (work_dir / "rational_minimax.json").write_text(json.dumps(report, indent=2) + "\n")
    if not first_report["accepted"] or not second_report["accepted"]:
        raise RuntimeError("joint rational minimax candidate failed a production gate")
    return first, second, report
