from __future__ import annotations

import json
import math
from pathlib import Path
from typing import Any

import numpy as np
from scipy import signal

from .magnitude_sdp import evaluate_power_spectrum


def spectral_factor_from_autocorrelation(
    autocorrelation: np.ndarray,
    fft_len: int,
    work_dir: Path,
    resume: bool = True,
) -> tuple[np.ndarray, np.ndarray, dict[str, Any]]:
    spectrum_path = work_dir / "minimum_spectrum.npy"
    magnitude_path = work_dir / "sdp_magnitude.npy"
    report_path = work_dir / "spectral_factor.json"
    if resume and spectrum_path.exists() and magnitude_path.exists() and report_path.exists():
        return (
            np.load(magnitude_path, mmap_mode="r"),
            np.load(spectrum_path, mmap_mode="r"),
            json.loads(report_path.read_text()),
        )

    power = evaluate_power_spectrum(autocorrelation, fft_len)
    clipped_power = np.maximum(power, 1.0e-30)
    magnitude = np.sqrt(clipped_power)
    # Factor the finite autocorrelation polynomial itself. Keeping the raw
    # length-N/2 cepstral coefficient on a padded FFT grid creates a spurious
    # half-period impulse when the deep stopband reaches the floating-point
    # floor; truncating that periodic impulse would not be an exact finite
    # spectral factor. scipy's homomorphic polynomial factorization returns
    # the order+1 minimum-phase FIR directly.
    symmetric_autocorrelation = np.concatenate(
        (autocorrelation[:0:-1], autocorrelation)
    )
    minimum_impulse = signal.minimum_phase(
        symmetric_autocorrelation, method="homomorphic", n_fft=fft_len
    ).astype(np.float64)
    repair_index = int(np.argmax(np.abs(minimum_impulse)))
    for _ in range(4):
        minimum_impulse[repair_index] += 1.0 - math.fsum(
            float(value) for value in minimum_impulse
        )
    minimum_spectrum = np.fft.rfft(minimum_impulse, n=fft_len)
    factor_error = np.max(np.abs(np.abs(minimum_spectrum) - magnitude))
    frequency = np.linspace(0.0, 44_100.0, minimum_spectrum.size)
    report = {
        "fft_len": fft_len,
        "method": "finite autocorrelation polynomial homomorphic factorization",
        "factor_coefficients": int(minimum_impulse.size),
        "magnitude_floor_power": 1.0e-30,
        "maximum_factor_magnitude_error": float(factor_error),
        "maximum_passband_factor_magnitude_error": float(
            np.max(
                np.abs(
                    np.abs(minimum_spectrum[frequency <= 20_000.0])
                    - magnitude[frequency <= 20_000.0]
                )
            )
        ),
        "factor_stopband_peak_db": float(
            20.0
            * np.log10(
                max(
                    float(np.max(np.abs(minimum_spectrum[frequency >= 22_050.0]))),
                    1.0e-300,
                )
            )
        ),
        "minimum_unclipped_power": float(np.min(power)),
        "maximum_power": float(np.max(power)),
    }
    np.save(magnitude_path, np.asarray(magnitude, dtype=np.float64))
    np.save(spectrum_path, np.asarray(minimum_spectrum, dtype=np.complex128))
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    return magnitude, minimum_spectrum, report
