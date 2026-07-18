from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

import numpy as np
import mpmath as mp
from scipy import signal

from .magnitude_sdp import evaluate_power


def _homomorphic_factor(autocorrelation: np.ndarray, fft_len: int) -> np.ndarray:
    symmetric = np.concatenate((autocorrelation[:0:-1], autocorrelation))
    return signal.minimum_phase(symmetric, method="homomorphic", n_fft=fft_len).astype(np.float64)


def _wilson_factor(autocorrelation: np.ndarray, fft_len: int, iterations: int = 30) -> tuple[np.ndarray, list[dict[str, float]]]:
    order = autocorrelation.size - 1
    target = np.maximum(evaluate_power(autocorrelation, fft_len), 1.0e-30)
    factor = _homomorphic_factor(autocorrelation, fft_len)
    history = []
    for iteration in range(iterations):
        spectrum = np.fft.rfft(factor, n=fft_len)
        ratio = target / np.maximum(np.abs(spectrum) ** 2, 1.0e-30)
        correction_spectrum = 1.0 + ratio
        correction_time = np.fft.irfft(correction_spectrum, n=fft_len)
        causal = np.zeros(fft_len, dtype=np.float64)
        causal[0] = 0.5 * correction_time[0]
        causal[1 : fft_len // 2] = correction_time[1 : fft_len // 2]
        causal[fft_len // 2] = 0.5 * correction_time[fft_len // 2]
        corrected = spectrum * np.fft.rfft(causal, n=fft_len)
        candidate = np.fft.irfft(corrected, n=fft_len)[: order + 1]
        candidate_spectrum = np.fft.rfft(candidate, n=fft_len)
        error = np.abs(np.abs(candidate_spectrum) ** 2 - target)
        history.append({"iteration": iteration + 1, "maximum_power_residual": float(np.max(error)), "passband_power_residual": float(np.max(error[: int(20_000.0 / 88_200.0 * fft_len) + 1]))})
        factor = candidate
        if history[-1]["maximum_power_residual"] <= 5.0e-12 and history[-1]["passband_power_residual"] <= 1.0e-13:
            break
    if factor[0] < 0.0:
        factor *= -1.0
    return factor, history


def _factor_autocorrelation(coefficients: np.ndarray) -> np.ndarray:
    values = np.asarray(coefficients, dtype=np.float64)
    return np.asarray(
        [np.dot(values[: values.size - lag], values[lag:]) for lag in range(values.size)],
        dtype=np.float64,
    )


def _newton_autocorrelation_refine(
    initial: np.ndarray,
    target: np.ndarray,
    iterations: int = 8,
) -> tuple[np.ndarray, list[dict[str, float]]]:
    """Refine the finite polynomial against its exact lag equations."""
    factor = np.asarray(initial, dtype=np.float64).copy()
    dimension = factor.size
    history = []
    for iteration in range(iterations):
        residual = _factor_autocorrelation(factor) - target
        before = float(np.max(np.abs(residual)))
        jacobian = np.zeros((dimension, dimension), dtype=np.float64)
        for lag in range(dimension):
            count = dimension - lag
            index = np.arange(count)
            jacobian[lag, index] += factor[index + lag]
            jacobian[lag, index + lag] += factor[index]
        correction, *_ = np.linalg.lstsq(jacobian, -residual, rcond=1.0e-14)
        accepted = False
        step = 1.0
        after = before
        for _ in range(16):
            candidate = factor + step * correction
            candidate_error = float(
                np.max(np.abs(_factor_autocorrelation(candidate) - target))
            )
            if candidate_error < before:
                factor = candidate
                after = candidate_error
                accepted = True
                break
            step *= 0.5
        history.append(
            {
                "iteration": iteration + 1,
                "accepted": accepted,
                "step": step,
                "maximum_lag_residual_before": before,
                "maximum_lag_residual_after": after,
            }
        )
        if not accepted or after <= 2.0e-16:
            break
    if factor[0] < 0.0:
        factor *= -1.0
    return factor, history


def _high_precision_reconstruction(
    factor_coefficients: np.ndarray,
    autocorrelation: np.ndarray,
    candidate_indices: np.ndarray,
    fft_len: int,
) -> dict[str, Any]:
    prepared_factor = [mp.mpf(float(value)) for value in factor_coefficients]
    prepared_r = [mp.mpf(float(value)) for value in autocorrelation]
    maximum = mp.mpf("0")
    with mp.workprec(192):
        for index in candidate_indices:
            omega = mp.pi * int(index) / (fft_len // 2)
            step = mp.exp(-mp.j * omega)
            response = mp.mpc(0)
            for coefficient in reversed(prepared_factor):
                response = response * step + coefficient
            power = prepared_r[0]
            for lag, value in enumerate(prepared_r[1:], start=1):
                power += 2 * value * mp.cos(lag * omega)
            maximum = max(maximum, abs(abs(response) ** 2 - power))
    return {
        "points": int(candidate_indices.size),
        "effective_precision_bits": 192,
        "maximum_power_reconstruction_error": float(maximum),
        "backend": "mpmath arbitrary precision",
    }


def factor(autocorrelation: np.ndarray, fft_len: int, work_dir: Path) -> dict[str, Any]:
    factor_fft_len = max(fft_len, 1 << 22)
    target = evaluate_power(autocorrelation, factor_fft_len)
    frequency = np.linspace(0.0, 44_100.0, target.size)
    passband = frequency <= 20_000.0
    homomorphic = _homomorphic_factor(autocorrelation, factor_fft_len)
    homomorphic_response = np.fft.rfft(homomorphic, n=factor_fft_len)
    homotopy_history = []
    selected = None
    for power_floor in (0.0, 1.0e-14, 1.0e-13, 2.0e-13, 5.0e-13, 1.0e-12):
        regularized = np.asarray(autocorrelation, dtype=np.float64).copy()
        regularized[0] += power_floor
        candidate, history = _wilson_factor(regularized, factor_fft_len)
        candidate_response = np.fft.rfft(candidate, n=factor_fft_len)
        error = np.abs(np.abs(candidate_response) ** 2 - target)
        key = (float(np.max(error[passband])), float(np.max(error)))
        homotopy_history.append(
            {
                "power_floor": power_floor,
                "maximum_passband_power_reconstruction_error": key[0],
                "maximum_fullband_power_reconstruction_error": key[1],
                "wilson_history": history,
            }
        )
        if selected is None or key < selected[0]:
            selected = (key, power_floor, candidate, candidate_response, history)
    assert selected is not None
    _, selected_floor, wilson, wilson_response, history = selected
    frequency = np.linspace(0.0, 44_100.0, wilson_response.size)
    passband = frequency <= 20_000.0
    reliable = target >= 1.0e-18
    power_error = np.abs(np.abs(wilson_response) ** 2 - target)
    crosscheck = np.abs(np.abs(homomorphic_response) ** 2 - np.abs(wilson_response) ** 2)
    # For H(z)=sum q[n]z^-n, these are the roots of q[0]z^M+...+q[M].
    roots = np.roots(wilson)
    maximum_zero_radius = float(np.max(np.abs(roots))) if roots.size else 0.0
    pass_indices = np.flatnonzero(passband)
    reliable_indices = np.flatnonzero(reliable)
    pass_worst = pass_indices[np.argpartition(power_error[pass_indices], -64)[-64:]]
    full_worst = reliable_indices[np.argpartition(power_error[reliable_indices], -64)[-64:]]
    high_precision = _high_precision_reconstruction(
        wilson,
        autocorrelation,
        np.unique(np.concatenate((pass_worst, full_worst))),
        factor_fft_len,
    )
    report = {
        "primary_method": "positive-spectrum homotopy Wilson Fejer-Riesz factorization with 192-bit reconstruction certification",
        "crosscheck_method": "independent homomorphic finite-polynomial factorization",
        "requested_fft_len": fft_len,
        "factor_fft_len": factor_fft_len,
        "selected_regularization_power_floor": selected_floor,
        "regularization_changes_exported_magnitude": False,
        "factor_coefficients": int(wilson.size),
        "wilson_history": history,
        "positive_spectrum_homotopy_history": homotopy_history,
        "maximum_passband_power_reconstruction_error": float(np.max(power_error[passband])),
        "maximum_fullband_power_reconstruction_error": float(np.max(power_error)),
        "maximum_reliable_band_power_reconstruction_error": float(np.max(power_error[reliable])),
        "homomorphic_crosscheck_maximum_power_difference": float(np.max(crosscheck[reliable])),
        "maximum_zero_radius": maximum_zero_radius,
        "all_zeros_inside_unit_circle_with_tolerance": bool(maximum_zero_radius <= 1.0 + 1.0e-7),
        "high_precision_reconstruction": high_precision,
    }
    report["accepted"] = bool(report["maximum_passband_power_reconstruction_error"] <= 1.0e-12 and report["maximum_fullband_power_reconstruction_error"] <= 1.0e-9 and high_precision["maximum_power_reconstruction_error"] <= 1.0e-9 and report["all_zeros_inside_unit_circle_with_tolerance"])
    work_dir.mkdir(parents=True, exist_ok=True)
    np.save(work_dir / "spectral_factor_coefficients.npy", wilson)
    np.save(work_dir / "sdp_magnitude.npy", np.sqrt(np.maximum(target, 0.0)))
    (work_dir / "spectral_factor.json").write_text(json.dumps(report, indent=2) + "\n")
    if not report["accepted"]:
        raise RuntimeError("dual spectral factorization failed acceptance")
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--work-dir", type=Path, default=Path(__file__).resolve().parent / "work")
    parser.add_argument("--order", type=int, default=512)
    parser.add_argument("--fft-len", type=int, default=8_388_608)
    arguments = parser.parse_args()
    data = np.load(arguments.work_dir / ("magnitude_order_" + str(arguments.order) + ".npz"))
    print(json.dumps(factor(data["autocorrelation"], arguments.fft_len, arguments.work_dir), indent=2))
