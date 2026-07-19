from __future__ import annotations

import json
import math
from pathlib import Path
from typing import Any

import mpmath
import numpy as np
from scipy import optimize


def _fsum(values: np.ndarray) -> float:
    return math.fsum(float(value) for value in values)


def _direct_response_longdouble(coefficients: np.ndarray, omega: float) -> complex:
    index = np.arange(coefficients.size, dtype=np.longdouble)
    angle = np.longdouble(omega) * index
    values = np.asarray(coefficients, dtype=np.longdouble)
    real = np.sum(values * np.cos(angle), dtype=np.longdouble)
    imag = -np.sum(values * np.sin(angle), dtype=np.longdouble)
    return complex(float(real), float(imag))


def _direct_response_mpmath(coefficients: np.ndarray, omega: float) -> complex:
    # Horner evaluation avoids constructing hundreds of thousands of arbitrary-
    # precision trigonometric values per frequency. Forty decimal digits exceed
    # the requested 80-bit-equivalent verification precision by a wide margin.
    with mpmath.workdps(40):
        step = mpmath.exp(-mpmath.j * mpmath.mpf(omega))
        accumulator = mpmath.mpc(0.0)
        for coefficient in coefficients[::-1]:
            accumulator = accumulator * step + mpmath.mpf(float(coefficient))
        return complex(float(accumulator.real), float(accumulator.imag))


def certify_character(
    coefficients: np.ndarray,
    target_magnitude: np.ndarray,
    target_residual_phase: np.ndarray,
    target_group_delay: np.ndarray,
    bulk_delay: int,
    fft_len: int,
    sample_rate_hz: float,
    pass_edge_hz: float,
    stop_edge_hz: float,
    split_lo_hz: float,
    split_hi_hz: float,
    edge_samples: int,
    cleanup_filters: list[np.ndarray],
    work_dir: Path,
) -> dict[str, Any]:
    response = np.fft.rfft(coefficients, n=fft_len)
    frequency = np.linspace(0.0, sample_rate_hz / 2.0, response.size)
    omega = np.linspace(0.0, np.pi, response.size)
    pass_mask = frequency <= pass_edge_hz
    stop_mask = frequency >= stop_edge_hz
    low_mask = (frequency >= 20.0) & (frequency <= split_lo_hz)
    high_mask = (frequency >= split_hi_hz) & (frequency <= pass_edge_hz)
    transition_mask = (frequency >= split_lo_hz) & (frequency <= split_hi_hz)
    magnitude_transition_mask = (frequency >= pass_edge_hz) & (
        frequency <= stop_edge_hz
    )
    magnitude = np.abs(response)
    pass_db = 20.0 * np.log10(np.maximum(magnitude[pass_mask], 1.0e-300))
    stop_peak = float(np.max(magnitude[stop_mask]))
    phase = np.unwrap(np.angle(response))
    delta_omega = np.pi / (response.size - 1)
    delay = -np.gradient(phase, delta_omega) - bulk_delay
    target_delay = np.asarray(target_group_delay)
    if target_delay.size != response.size:
        source_axis = np.linspace(0.0, np.pi, target_delay.size)
        target_delay = np.interp(omega, source_axis, target_delay)
    low_mean = float(np.mean(delay[low_mask]))
    low_error = float(np.max(np.abs(delay[low_mask] - low_mean)))
    high_error = float(np.max(np.abs(delay[high_mask] - target_delay[high_mask])))
    endpoint_low = low_mean
    endpoint_high = target_delay[transition_mask]
    envelope_low = np.minimum(endpoint_low, endpoint_high) - 0.25
    envelope_high = np.maximum(endpoint_low, endpoint_high) + 0.25
    transition_delay = delay[transition_mask]
    overshoot = float(
        max(
            np.max(np.maximum(envelope_low - transition_delay, 0.0)),
            np.max(np.maximum(transition_delay - envelope_high, 0.0)),
        )
    )
    total_energy = float(np.dot(coefficients, coefficients))
    edge_energy = float(
        np.dot(coefficients[:edge_samples], coefficients[:edge_samples])
        + np.dot(coefficients[-edge_samples:], coefficients[-edge_samples:])
    )
    # The support search freezes this logical origin. It is deliberately not
    # recomputed from the dominant tap after coefficient optimization.
    origin = int(bulk_delay)
    pre_peak_energy = float(np.dot(coefficients[:origin], coefficients[:origin]))
    step = np.cumsum(coefficients)
    transition_curvature = np.gradient(np.gradient(transition_delay))
    branch_length = (coefficients.size + 1) // 2
    phase0_origin = min((origin + 1) // 2, branch_length - 1)
    phase1_global = max(origin - 1, 0)
    phase1_origin = min((phase1_global + 1) // 2, branch_length - 1)
    target_magnitude_array = np.asarray(target_magnitude)
    target_phase_array = np.asarray(target_residual_phase)
    if target_magnitude_array.size != response.size:
        target_axis = np.linspace(0.0, np.pi, target_magnitude_array.size)
        target_magnitude_array = np.interp(omega, target_axis, target_magnitude_array)
        target_phase_array = np.interp(omega, target_axis, target_phase_array)
    target_response = target_magnitude_array * np.exp(
        1j * (target_phase_array - omega * bulk_delay)
    )
    join_index = int(np.ceil(2.0 * split_hi_hz / sample_rate_hz * (response.size - 1)))
    realized_join_error = float(
        np.angle(
            np.exp(
                1j
                * (
                    phase[join_index]
                    + omega[join_index] * bulk_delay
                    - target_phase_array[join_index]
                )
            )
        )
    )
    transition_db = 20.0 * np.log10(
        np.maximum(magnitude[magnitude_transition_mask], 1.0e-300)
    )
    transition_step_db = np.diff(transition_db)
    # Once both adjacent bins are below the -150 dB design floor, their dB
    # ratio describes floating-point/finite-FIR zero placement rather than a
    # transition spike. Stopband certification governs that region.
    transition_above_design_floor = (
        np.maximum(transition_db[:-1], transition_db[1:]) >= -150.0
    )
    transition_upward_excursion_db = float(
        np.max(
            np.maximum(
                transition_step_db[transition_above_design_floor], 0.0
            )
        )
    )
    transition_raw_upward_excursion_db = float(
        np.max(np.maximum(transition_step_db, 0.0))
    )
    cleanup_products = [np.ones_like(omega)]
    accumulated = np.ones_like(omega)
    axis = np.arange(omega.size, dtype=np.float64)
    cleanup_metrics = []
    cleanup_magnitudes = []
    for stage, cleanup in enumerate(cleanup_filters, start=1):
        center = cleanup.size // 2
        even_without_center = np.asarray(cleanup[::2]).copy()
        even_without_center[center // 2] = 0.0
        cleanup_metrics.append(
            {
                "stage": stage,
                "canonical_sum": _fsum(cleanup),
                "even_sum": _fsum(cleanup[::2]),
                "odd_sum": _fsum(cleanup[1::2]),
                "center_error": float(abs(cleanup[center] - 0.5)),
                "maximum_even_off_center": float(
                    np.max(np.abs(even_without_center))
                ),
                "maximum_symmetry_error": float(
                    np.max(np.abs(cleanup - cleanup[::-1]))
                ),
            }
        )
        cleanup_magnitude = np.abs(np.fft.rfft(cleanup, n=fft_len))
        cleanup_magnitudes.append(cleanup_magnitude)
        accumulated *= np.interp(axis / (2**stage), axis, cleanup_magnitude)
        cleanup_products.append(accumulated.copy())
    ratio_metrics = []
    worst_complex_error = 0.0
    worst_stop_peak = 0.0
    image_probe = np.linspace(
        0.0, 2.0 * np.pi * pass_edge_hz / sample_rate_hz, 8193
    )

    def magnitude_at(values: np.ndarray, angle: np.ndarray) -> np.ndarray:
        folded = np.abs((angle + np.pi) % (2.0 * np.pi) - np.pi)
        return np.interp(folded, omega, values)

    worst_image_peak = 0.0
    for exponent, cleanup_product in enumerate(cleanup_products):
        composite = response * cleanup_product
        complex_error = float(
            np.max(np.abs(composite[pass_mask] - target_response[pass_mask]))
        )
        ratio_stop_peak = float(np.max(np.abs(composite[stop_mask])))
        worst_complex_error = max(worst_complex_error, complex_error)
        worst_stop_peak = max(worst_stop_peak, ratio_stop_peak)
        ratio_db = 20.0 * np.log10(np.maximum(np.abs(composite[pass_mask]), 1.0e-300))
        stages = exponent + 1
        ratio = 1 << stages
        image_peak = 0.0
        for image_index in range(1, ratio):
            output_angle = (image_probe + 2.0 * np.pi * image_index) / ratio
            image_gain = magnitude_at(magnitude, output_angle * (1 << (stages - 1)))
            for cleanup_index in range(stages - 1):
                image_gain *= magnitude_at(
                    cleanup_magnitudes[cleanup_index],
                    output_angle * (1 << (stages - 2 - cleanup_index)),
                )
            image_peak = max(image_peak, float(np.max(image_gain)))
        worst_image_peak = max(worst_image_peak, image_peak)
        ratio_metrics.append(
            {
                "ratio": ratio,
                "passband_ripple_db_peak_to_peak": float(np.max(ratio_db) - np.min(ratio_db)),
                "maximum_passband_complex_error": complex_error,
                "stopband_peak_db": 20.0
                * math.log10(max(ratio_stop_peak, 1.0e-300)),
                "interpolation_image_peak_db": 20.0
                * math.log10(max(image_peak, 1.0e-300)),
                "decimation_alias_peak_db": 20.0
                * math.log10(max(image_peak, 1.0e-300)),
            }
        )
    pass_error_bins = np.abs(response[pass_mask] - target_response[pass_mask])
    pass_global_indices = np.flatnonzero(pass_mask)
    stop_global_indices = np.flatnonzero(stop_mask)
    worst_pass = pass_global_indices[
        np.argpartition(pass_error_bins, -min(500, pass_error_bins.size))[
            -min(500, pass_error_bins.size) :
        ]
    ]
    stop_values = magnitude[stop_mask]
    worst_stop = stop_global_indices[
        np.argpartition(stop_values, -min(500, stop_values.size))[
            -min(500, stop_values.size) :
        ]
    ]
    worst_indices = np.unique(np.concatenate((worst_pass, worst_stop)))
    direct_discrepancy = 0.0
    for index in worst_indices:
        direct = _direct_response_longdouble(coefficients, float(omega[index]))
        direct_discrepancy = max(direct_discrepancy, abs(direct - response[index]))

    selected_high_precision = np.unique(
        np.concatenate((worst_pass[-4:], worst_stop[-4:]))
    )
    arbitrary_precision_discrepancy = 0.0
    for index in selected_high_precision:
        direct = _direct_response_mpmath(coefficients, float(omega[index]))
        arbitrary_precision_discrepancy = max(
            arbitrary_precision_discrepancy, abs(direct - response[index])
        )

    refined_stop_peak = stop_peak
    bin_width = np.pi / (response.size - 1)
    top_stop = stop_global_indices[
        np.argpartition(stop_values, -min(32, stop_values.size))[
            -min(32, stop_values.size) :
        ]
    ]
    for index in top_stop:
        lower = max(float(omega[index] - bin_width), 0.0)
        upper = min(float(omega[index] + bin_width), np.pi)
        result = optimize.minimize_scalar(
            lambda value: -abs(_direct_response_longdouble(coefficients, value)),
            bounds=(lower, upper),
            method="bounded",
            options={"xatol": 1.0e-15, "maxiter": 80},
        )
        refined_stop_peak = max(refined_stop_peak, -float(result.fun))
    report = {
        "fft_len": fft_len,
        "passband_ripple_db_peak_to_peak": float(np.max(pass_db) - np.min(pass_db)),
        "passband_peak_absolute_db": float(np.max(np.abs(pass_db))),
        "character_stopband_peak_db": 20.0 * math.log10(max(stop_peak, 1.0e-300)),
        "character_refined_stopband_peak_db": 20.0
        * math.log10(max(refined_stop_peak, 1.0e-300)),
        "lowband_constant_delay_error_samples": low_error,
        "highband_minimum_delay_error_samples": high_error,
        "transition_delay_overshoot_samples": overshoot,
        "realized_join_phase_error_rad": realized_join_error,
        "transition_maximum_upward_excursion_db": transition_upward_excursion_db,
        "transition_raw_maximum_upward_excursion_db": (
            transition_raw_upward_excursion_db
        ),
        "transition_monotonic_floor_db": -150.0,
        "maximum_group_delay_curvature": float(
            np.max(np.abs(transition_curvature))
        ),
        "edge_energy_db": 10.0 * math.log10(max(edge_energy / total_energy, 1.0e-300)),
        "broadband_pre_peak_energy_db": 10.0
        * math.log10(max(pre_peak_energy / total_energy, 1.0e-300)),
        "step_response_overshoot": float(max(np.max(step) - 1.0, 0.0)),
        "canonical_sum": _fsum(coefficients),
        "canonical_even_sum": _fsum(coefficients[::2]),
        "canonical_odd_sum": _fsum(coefficients[1::2]),
        "full_rate_origin": origin,
        "phase0_prepad": branch_length - 1 - phase0_origin,
        "phase1_prepad": branch_length - 1 - phase1_origin,
        "decimation_prepad": coefficients.size - 1 - origin,
        "worst_composite_passband_complex_error": worst_complex_error,
        "worst_composite_stopband_peak_db": 20.0
        * math.log10(max(worst_stop_peak, 1.0e-300)),
        "worst_interpolation_image_peak_db": 20.0
        * math.log10(max(worst_image_peak, 1.0e-300)),
        "worst_decimation_alias_peak_db": 20.0
        * math.log10(max(worst_image_peak, 1.0e-300)),
        "ratios": ratio_metrics,
        "cleanups": cleanup_metrics,
        "direct_high_precision_points": int(worst_indices.size),
        "longdouble_mantissa_bits": int(np.finfo(np.longdouble).nmant),
        "maximum_fft_to_direct_discrepancy": direct_discrepancy,
        "arbitrary_precision_points": int(selected_high_precision.size),
        "arbitrary_precision_decimal_digits": 40,
        "maximum_fft_to_arbitrary_precision_discrepancy": (
            arbitrary_precision_discrepancy
        ),
    }
    report["accepted"] = bool(
        report["passband_ripple_db_peak_to_peak"] <= 0.0001
        and report["character_refined_stopband_peak_db"] <= -150.0
        and worst_complex_error <= 2.0e-5
        and report["worst_interpolation_image_peak_db"] <= -150.0
        and report["worst_decimation_alias_peak_db"] <= -150.0
        and report["lowband_constant_delay_error_samples"] <= 0.01
        and report["highband_minimum_delay_error_samples"] <= 0.02
        and report["transition_delay_overshoot_samples"] <= 0.25
        and abs(report["realized_join_phase_error_rad"]) <= 2.0e-5
        and report["transition_maximum_upward_excursion_db"] <= 0.0005
        and report["edge_energy_db"] <= -170.0
        and abs(report["canonical_even_sum"] - 0.5) <= 2.0e-15
        and abs(report["canonical_odd_sum"] - 0.5) <= 2.0e-15
        and all(
            abs(cleanup["canonical_sum"] - 1.0) <= 2.0e-15
            and abs(cleanup["even_sum"] - 0.5) <= 2.0e-15
            and abs(cleanup["odd_sum"] - 0.5) <= 2.0e-15
            and cleanup["center_error"] == 0.0
            and cleanup["maximum_even_off_center"] == 0.0
            and cleanup["maximum_symmetry_error"] <= 2.0e-15
            for cleanup in cleanup_metrics
        )
    )
    (work_dir / "certification.json").write_text(json.dumps(report, indent=2) + "\n")
    return report
