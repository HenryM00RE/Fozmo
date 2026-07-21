from __future__ import annotations

import unittest
from pathlib import Path

import numpy as np

from .e3_p10_joint_search import (
    CLEANUP_DIRECTIONS,
    PHASE_COORDINATES,
    _build_context,
    _coordinate_bounds,
    _coordinate_slices,
    _magnitude_basis,
    _realize,
    MAGNITUDE_KNOTS_HZ,
)


class P10JointSearchTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.root = Path(__file__).resolve().parents[2]
        cls.context = _build_context(cls.root, "moderate")

    def test_geometry_has_requested_dimensions(self) -> None:
        phase, magnitude, cleanup = _coordinate_slices(self.context)
        self.assertEqual(phase.stop - phase.start, PHASE_COORDINATES)
        self.assertEqual(cleanup.stop - cleanup.start, CLEANUP_DIRECTIONS)
        self.assertEqual(magnitude.stop - magnitude.start, 11)

    def test_zero_coordinates_reproduce_production_assets(self) -> None:
        lower, _, _ = _coordinate_bounds(self.context)
        character, cleanup, structural = _realize(
            self.context, np.zeros(lower.size, dtype=np.float64)
        )
        np.testing.assert_allclose(character, self.context.character, rtol=0.0, atol=1.0e-16)
        np.testing.assert_array_equal(cleanup, self.context.cleanup)
        self.assertLess(structural["omitted_periodic_energy_ratio"], 1.0e-24)

    def test_cleanup_modes_preserve_exact_halfband_equalities(self) -> None:
        modes = self.context.cleanup_modes
        center = modes.shape[0] // 2
        self.assertTrue(np.all(modes[0::2] == 0.0))
        self.assertTrue(np.all(modes[center] == 0.0))
        np.testing.assert_allclose(modes, modes[::-1], rtol=0.0, atol=0.0)
        np.testing.assert_allclose(np.sum(modes, axis=0), 0.0, rtol=0.0, atol=2.0e-14)

    def test_magnitude_basis_interpolates_and_closes(self) -> None:
        frequency = np.concatenate(([0.0], MAGNITUDE_KNOTS_HZ, [30_000.0]))
        basis = _magnitude_basis(frequency)
        np.testing.assert_array_equal(basis[0], np.zeros(MAGNITUDE_KNOTS_HZ.size))
        np.testing.assert_allclose(basis[1:-1], np.eye(MAGNITUDE_KNOTS_HZ.size), atol=1.0e-12)
        np.testing.assert_array_equal(basis[-1], np.zeros(MAGNITUDE_KNOTS_HZ.size))


if __name__ == "__main__":
    unittest.main()
