from __future__ import annotations

import numpy as np
import unittest

from tools.split_phase_v5.e2_v5_structural_search import (
    _build_structural_spline,
    _model_sha256,
    _project_center,
)


def _minimum() -> tuple[np.ndarray, np.ndarray]:
    frequency = np.geomspace(1.0, 20_000.0, 8192)
    delay = 2.0 + 0.15 * np.log(frequency / 3000.0) ** 2
    return frequency, delay


class E2V5StructuralSearchTests(unittest.TestCase):
    def test_expanded_structures_have_expected_free_coordinates_and_constraints(self) -> None:
        frequency, delay = _minimum()
        for controls, join_hz in ((30, 14_000.0), (36, 15_000.0)):
            model = _build_structural_spline(frequency, delay, controls, join_hz)
            self.assertEqual(model.controls, controls)
            self.assertEqual(model.free_coordinates, controls - 6)
            self.assertLess(model.constraint_residual, 5.0e-10)

    def test_projection_is_deterministic_and_model_hash_tracks_join(self) -> None:
        frequency, delay = _minimum()
        model = _build_structural_spline(frequency, delay, 30, 14_000.0)
        first, first_error = _project_center(model, 14_000.0, frequency, delay)
        second, second_error = _project_center(model, 14_000.0, frequency, delay)
        np.testing.assert_array_equal(first, second)
        self.assertEqual(first_error, second_error)
        self.assertNotEqual(_model_sha256(model, 14_000.0), _model_sha256(model, 15_000.0))


if __name__ == "__main__":
    unittest.main()
