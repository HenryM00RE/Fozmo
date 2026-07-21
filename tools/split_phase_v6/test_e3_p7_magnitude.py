from __future__ import annotations

import math
import unittest
from pathlib import Path

import numpy as np

from .e3_p7_magnitude_sensitivity import (
    CHARACTER_RATE_HZ,
    CONTROL_FREQUENCIES_HZ,
    FFT_LENGTH,
    _basis,
    _realize_character,
)
from .e3_phase_search import _read_f64le


class MagnitudeParameterizationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.root = Path(__file__).resolve().parents[2]
        cls.character = _read_f64le(
            cls.root / "tools/split_phase_v6/baselines/e3-p6d-local-0145.f64le"
        )

    def test_basis_is_compact_and_interpolates_controls(self) -> None:
        frequency = np.concatenate(
            (
                np.asarray((14_999.0,)),
                CONTROL_FREQUENCIES_HZ,
                np.asarray((22_051.0,)),
            )
        )
        basis = _basis(frequency)
        np.testing.assert_array_equal(basis[0], np.zeros(CONTROL_FREQUENCIES_HZ.size))
        np.testing.assert_allclose(
            basis[1 : 1 + CONTROL_FREQUENCIES_HZ.size],
            np.eye(CONTROL_FREQUENCIES_HZ.size),
            atol=0.0,
            rtol=0.0,
        )
        np.testing.assert_array_equal(
            basis[-1],
            np.asarray((0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0)),
        )

    def test_zero_controls_reproduce_the_incumbent(self) -> None:
        spectrum = np.fft.rfft(self.character, FFT_LENGTH)
        frequency = np.fft.rfftfreq(FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ)
        candidate, realization = _realize_character(
            self.character,
            spectrum,
            _basis(frequency),
            np.zeros(CONTROL_FREQUENCIES_HZ.size),
            float(math.fsum(float(value) for value in self.character)),
        )
        np.testing.assert_allclose(candidate, self.character, atol=2.0e-15, rtol=2.0e-13)
        self.assertLess(realization["maximum_realized_delta_db_0_15khz"], 2.0e-7)


if __name__ == "__main__":
    unittest.main()
