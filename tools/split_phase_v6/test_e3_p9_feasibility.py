from __future__ import annotations

import math
import unittest
from pathlib import Path

import numpy as np

from .e3_p9_feasibility import (
    MAGNITUDE_KNOTS_HZ,
    PHASE_KNOTS_HZ,
    _compact_basis,
    _realize,
)
from .e3_phase_search import CHARACTER_RATE_HZ, FFT_LENGTH, _read_f64le


class P9FeasibilityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.root = Path(__file__).resolve().parents[2]
        cls.e2 = _read_f64le(
            cls.root / "assets/filters/split_phase_e2v3/character_full_rate.f64le"
        )
        cls.spectrum = np.fft.rfft(cls.e2, FFT_LENGTH)
        cls.frequency = np.fft.rfftfreq(FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ)

    def test_compact_basis_interpolates_and_closes(self) -> None:
        frequency = np.concatenate(([0.0], PHASE_KNOTS_HZ, [30_000.0]))
        basis = _compact_basis(frequency, PHASE_KNOTS_HZ)
        np.testing.assert_array_equal(basis[0], np.zeros(PHASE_KNOTS_HZ.size))
        np.testing.assert_allclose(basis[1:-1], np.eye(PHASE_KNOTS_HZ.size), atol=0.0)
        np.testing.assert_array_equal(basis[-1], np.zeros(PHASE_KNOTS_HZ.size))

    def test_zero_coordinates_reproduce_e2(self) -> None:
        phase_basis = _compact_basis(self.frequency, PHASE_KNOTS_HZ)
        magnitude_basis = _compact_basis(self.frequency, MAGNITUDE_KNOTS_HZ)
        count = PHASE_KNOTS_HZ.size - 2 + MAGNITUDE_KNOTS_HZ.size
        candidate, _ = _realize(
            self.spectrum,
            phase_basis,
            magnitude_basis,
            np.zeros(count),
            self.e2.size,
            float(math.fsum(float(value) for value in self.e2)),
        )
        np.testing.assert_allclose(candidate, self.e2, rtol=0.0, atol=1.0e-16)


if __name__ == "__main__":
    unittest.main()
