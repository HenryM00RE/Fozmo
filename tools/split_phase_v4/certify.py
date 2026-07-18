from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
from typing import Any

import mpmath as mp
import numpy as np
from scipy import optimize

from .analytic_group_delay import physical_log_derivatives


def _direct_response(coefficients: np.ndarray, omega: float) -> complex:
    index = np.arange(coefficients.size, dtype=np.float64)
    return complex(np.dot(coefficients, np.exp(-1j * omega * index)))


def _refine_magnitude(
    coefficients: np.ndarray,
    center_omega: float,
    half_bin: float,
    maximize: bool,
) -> tuple[float, float]:
    index = np.arange(coefficients.size, dtype=np.float64)
    sign = -1.0 if maximize else 1.0
    result = optimize.minimize_scalar(
        lambda value: sign * abs(np.dot(coefficients, np.exp(-1j * value * index))),
        bounds=(max(center_omega - half_bin, 0.0), min(center_omega + half_bin, np.pi)),
        method="bounded",
        options={"xatol": 1.0e-15},
    )
    return float(result.x), float(sign * result.fun)


def _mp_horner(prepared: list[mp.mpf], omega: float, precision_bits: int) -> complex:
    with mp.workprec(precision_bits):
        step = mp.exp(-mp.j * mp.mpf(str(float(omega))))
        accumulator = mp.mpc(0.0)
        for coefficient in reversed(prepared):
            accumulator = accumulator * step + coefficient
        return complex(float(accumulator.real), float(accumulator.imag))


def _high_precision(coefficients: np.ndarray, response: np.ndarray, point_indices: np.ndarray, fft_len: int, precision_bits: int) -> dict[str, Any]:
    prepared = [mp.mpf(float(value)) for value in coefficients]
    maximum = 0.0
    for index in point_indices:
        direct = _mp_horner(prepared, np.pi * int(index) / (fft_len // 2), precision_bits)
        maximum = max(maximum, abs(direct - response[int(index)]))
    return {"points": int(point_indices.size), "effective_precision_bits": precision_bits, "maximum_fft_discrepancy": maximum, "backend": "mpmath arbitrary precision (MPFR-equivalent precision; platform longdouble not counted)"}


def _worst_indices(response: np.ndarray, frequency: np.ndarray, count: int) -> np.ndarray:
    passband = np.flatnonzero((frequency >= 20.0) & (frequency <= 20_000.0))
    stopband = np.flatnonzero(frequency >= 22_050.0)
    pass_error = np.abs(np.abs(response[passband]) - 1.0)
    stop_error = np.abs(response[stopband])
    per_category = count // 2
    selected_pass = passband[np.argpartition(pass_error, -per_category)[-per_category:]]
    selected_stop = stopband[np.argpartition(stop_error, -per_category)[-per_category:]]
    return np.unique(np.concatenate((selected_pass, selected_stop)))[:count]


def _resample_target(target: np.ndarray, omega: np.ndarray, origin: int) -> np.ndarray:
    """Interpolate the smooth residual phase, never the bulk-shifted phasor."""
    target_axis = np.linspace(0.0, np.pi, target.size)
    magnitude = np.interp(omega, target_axis, np.abs(target))
    residual_phase = np.unwrap(np.angle(target)) + target_axis * origin
    phase = np.interp(omega, target_axis, residual_phase) - omega * origin
    result = magnitude * np.exp(1j * phase)
    result[0] = result[0].real
    result[-1] = result[-1].real
    return result


def _ratio_metrics(
    character_response: np.ndarray,
    target_response: np.ndarray,
    frequency: np.ndarray,
    omega: np.ndarray,
    origin: int,
    cleanups: list[np.ndarray],
    fft_len: int,
) -> dict[str, Any]:
    passband = (frequency >= 20.0) & (frequency <= 20_000.0)
    compensated = character_response[passband] * np.exp(1j * omega[passband] * origin)
    target_compensated = target_response[passband] * np.exp(1j * omega[passband] * origin)
    pass_frequency = frequency[passband]
    rows = []
    worst_ripple = 0.0
    worst_complex = 0.0
    for exponent in range(1, 9):
        if exponent > 1:
            cleanup = cleanups[exponent - 2]
            cleanup_response = np.fft.rfft(cleanup, n=fft_len)
            center = cleanup.size // 2
            cleanup_grid_omega = 2.0 * np.pi * np.arange(cleanup_response.size) / fft_len
            cleanup_response *= np.exp(1j * cleanup_grid_omega * center)
            stage_rate = 88_200.0 * (1 << (exponent - 1))
            bins = pass_frequency * fft_len / stage_rate
            left = np.floor(bins).astype(np.int64)
            fraction = bins - left
            sampled = cleanup_response[left] * (1.0 - fraction) + cleanup_response[left + 1] * fraction
            compensated *= sampled
        magnitude_db = 20.0 * np.log10(np.maximum(np.abs(compensated), 1.0e-300))
        ripple = float(np.max(magnitude_db) - np.min(magnitude_db))
        complex_error = float(np.max(np.abs(compensated - target_compensated)))
        rows.append({"ratio": 1 << exponent, "passband_ripple_db": ripple, "composite_complex_error": complex_error})
        worst_ripple = max(worst_ripple, ripple)
        worst_complex = max(worst_complex, complex_error)
    return {
        "ratios": rows,
        "worst_2x_256x_passband_ripple_db": worst_ripple,
        "worst_2x_256x_composite_complex_error": worst_complex,
    }


def certify(
    character: np.ndarray,
    target: np.ndarray,
    origin: int,
    fft_len: int,
    work_dir: Path,
    run_high_precision: bool = True,
    cleanups: list[np.ndarray] | None = None,
) -> dict[str, Any]:
    response = np.fft.rfft(character, n=fft_len)
    frequency = np.linspace(0.0, 44_100.0, response.size)
    omega = np.linspace(0.0, np.pi, response.size)
    passband = (frequency >= 20.0) & (frequency <= 20_000.0)
    stopband = frequency >= 22_050.0
    transition = (frequency >= 20_000.0) & (frequency <= 22_050.0)
    magnitude = np.abs(response)
    pass_db = 20.0 * np.log10(np.maximum(magnitude[passband], 1.0e-300))
    stop_indices = np.flatnonzero(stopband)
    local = np.flatnonzero((magnitude[stop_indices][1:-1] >= magnitude[stop_indices][:-2]) & (magnitude[stop_indices][1:-1] >= magnitude[stop_indices][2:])) + 1
    candidate_indices = stop_indices[local]
    if candidate_indices.size:
        candidate_indices = candidate_indices[np.argsort(magnitude[candidate_indices])[-64:]]
    refined = [
        _refine_magnitude(character, omega[index], np.pi / (fft_len // 2), True)
        for index in candidate_indices
    ]
    refined_stop = max([value for _, value in refined] + [float(np.max(magnitude[stopband]))])
    pass_indices = np.flatnonzero(passband)
    pass_error = np.abs(magnitude[pass_indices] - 1.0)
    pass_candidates = pass_indices[np.argpartition(pass_error, -64)[-64:]]
    refined_pass = [
        _refine_magnitude(
            character,
            omega[index],
            np.pi / (fft_len // 2),
            bool(magnitude[index] >= 1.0),
        )[1]
        for index in pass_candidates
    ]
    refined_pass_min = min(refined_pass + [float(np.min(magnitude[passband]))])
    refined_pass_max = max(refined_pass + [float(np.max(magnitude[passband]))])
    refined_pass_ripple_db = float(20.0 * np.log10(refined_pass_max / refined_pass_min))
    target_response = _resample_target(target, omega, origin)
    complex_error = float(np.max(np.abs(response[passband] - target_response[passband])))
    weighted = np.fft.rfft(np.arange(character.size, dtype=np.float64) * character, n=fft_len)
    with np.errstate(divide="ignore", invalid="ignore"):
        delay = np.real(weighted / response) - origin
    log_frequency = np.geomspace(20.0, 20_000.0, 8192)
    log_bins = log_frequency * fft_len / 88_200.0
    left = np.floor(log_bins).astype(np.int64)
    fraction = log_bins - left
    log_delay = delay[left] * (1.0 - fraction) + delay[left + 1] * fraction
    slope, curvature = physical_log_derivatives(log_frequency, log_delay)
    low = (frequency >= 20.0) & (frequency <= 3000.0)
    high = (frequency >= 14_000.0) & (frequency <= 20_000.0)
    low_error = float(np.max(np.abs(delay[low] - np.mean(delay[low]))))
    target_phase = np.unwrap(np.angle(target_response))
    target_delay = -np.gradient(target_phase, omega, edge_order=2) - origin
    high_error = float(np.max(np.abs(delay[high] - target_delay[high])))
    transition_delay_mask = (frequency >= 3000.0) & (frequency <= 14_000.0)
    delay_overshoot = float(np.max(np.abs(delay[transition_delay_mask] - target_delay[transition_delay_mask])))
    total = float(np.dot(character, character))
    edge = float(np.dot(character[:2048], character[:2048]) + np.dot(character[-2048:], character[-2048:]))
    step = np.cumsum(character)
    join = int(np.searchsorted(frequency, 14_000.0))
    join_error = float(abs(np.angle(np.exp(1j * (np.angle(response[join]) - np.angle(target_response[join]))))))
    transition_db = 20.0 * np.log10(np.maximum(magnitude[transition], 1.0e-300))
    report: dict[str, Any] = {
        "fft_len": fft_len,
        "scalar_refined_stopband_extrema": len(refined),
        "scalar_refined_passband_extrema": len(refined_pass),
        "passband_ripple_db_peak_to_peak": refined_pass_ripple_db,
        "character_refined_stopband_db": float(20.0 * np.log10(max(refined_stop, 1.0e-300))),
        "worst_composite_complex_error": complex_error,
        "transition_maximum_upward_excursion_db": float(np.max(np.maximum(np.diff(transition_db), 0.0))),
        "lowband_constant_delay_error_samples": low_error,
        "highband_minimum_delay_error_samples": high_error,
        "realized_join_phase_error_rad": join_error,
        "transition_delay_overshoot_samples": delay_overshoot,
        "group_delay_slope_max_abs_samples_per_ln_hz": float(np.max(np.abs(slope))),
        "group_delay_curvature_max_abs_samples_per_ln_hz_squared": float(np.max(np.abs(curvature))),
        "edge_energy_db": float(10.0 * np.log10(max(edge / total, 1.0e-300))),
        "step_response_overshoot": float(max(np.max(step) - 1.0, -np.min(step), 0.0)),
        "canonical_even_sum_error": abs(math.fsum(float(value) for value in character[::2]) - 0.5),
        "canonical_odd_sum_error": abs(math.fsum(float(value) for value in character[1::2]) - 0.5),
    }
    if cleanups is not None:
        report["integer_ratio_certification"] = _ratio_metrics(
            response, target_response, frequency, omega, origin, cleanups, fft_len
        )
    if run_high_precision:
        points80 = _worst_indices(response, frequency, 1000)
        error = np.abs(response - target_response)
        points160 = np.argpartition(error[passband], -64)[-64:]
        points160 = np.flatnonzero(passband)[points160]
        report["high_precision_80bit"] = _high_precision(character, response, points80, fft_len, 96)
        report["high_precision_160bit"] = _high_precision(character, response, points160, fft_len, 192)
    else:
        report["high_precision_skipped_for_development"] = True
    ratio_report = report.get("integer_ratio_certification")
    ratio_gates = bool(
        ratio_report is not None
        and ratio_report["worst_2x_256x_passband_ripple_db"] <= 1.0e-7
        and ratio_report["worst_2x_256x_composite_complex_error"] <= 8.0e-9
    )
    report["accepted_character_gates"] = bool(report["passband_ripple_db_peak_to_peak"] <= 1.0e-7 and report["character_refined_stopband_db"] <= -160.0 and complex_error <= 8.0e-9 and ratio_gates and low_error <= 1.0e-5 and high_error <= 1.0e-4 and join_error <= 2.0e-9 and delay_overshoot <= 0.05 and report["edge_energy_db"] <= -215.0 and report["canonical_even_sum_error"] <= 2.0e-15 and report["canonical_odd_sum_error"] <= 2.0e-15 and run_high_precision)
    auxiliary: dict[str, Any] = {}
    build_path = work_dir / "build_report.json"
    if build_path.exists():
        build_report = json.loads(build_path.read_text())
        comparison = build_report.get("comparison", {})
        rational = build_report.get("rational", {})
        d_metrics = comparison.get("d_metrics", {})
        auxiliary["comparison_abcd_accepted"] = bool(comparison.get("accepted"))
        auxiliary["interpolation_image_db"] = d_metrics.get("worst_interpolation_image_db")
        auxiliary["independent_decimation_alias_db"] = d_metrics.get("worst_independent_decimation_alias_db")
        auxiliary["multirate_image_alias_accepted"] = bool(
            auxiliary["interpolation_image_db"] is not None
            and auxiliary["independent_decimation_alias_db"] is not None
            and auxiliary["interpolation_image_db"] <= -160.0
            and auxiliary["independent_decimation_alias_db"] <= -160.0
        )
        rational_rows = [rational.get("rational_147_160", {}), rational.get("rational_160_147", {})]
        auxiliary["rational_accepted"] = bool(all(row.get("accepted") for row in rational_rows))
    runtime_path = work_dir / "runtime_capture.json"
    if runtime_path.exists():
        runtime = json.loads(runtime_path.read_text())
        auxiliary["runtime_accepted"] = bool(runtime.get("accepted"))
        auxiliary["runtime_metrics"] = runtime.get("measured_runtime_metrics")
    else:
        auxiliary["runtime_accepted"] = False
    report["complete_system_gates"] = auxiliary
    report["accepted"] = bool(
        report["accepted_character_gates"]
        and auxiliary.get("comparison_abcd_accepted")
        and auxiliary.get("multirate_image_alias_accepted")
        and auxiliary.get("rational_accepted")
        and auxiliary.get("runtime_accepted")
    )
    work_dir.mkdir(parents=True, exist_ok=True)
    (work_dir / "certification.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--work-dir", type=Path, default=Path(__file__).resolve().parent / "work")
    parser.add_argument("--fft-len", type=int, default=33_554_432)
    parser.add_argument("--development-skip-high-precision", action="store_true")
    arguments = parser.parse_args()
    coefficients = np.load(arguments.work_dir / "character_optimized.npy")
    target = np.load(arguments.work_dir / "target_spectrum.npy")
    alignment = json.loads((arguments.work_dir / "alignment.json").read_text())
    cleanup_data = np.load(arguments.work_dir / "cleanup_optimized.npz")
    cleanups = [cleanup_data["stage_" + str(index)] for index in range(1, 8)]
    print(json.dumps(certify(coefficients, target, int(alignment["full_rate_origin"]), arguments.fft_len, arguments.work_dir, not arguments.development_skip_high_precision, cleanups), indent=2))
