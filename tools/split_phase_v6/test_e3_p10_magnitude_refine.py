from __future__ import annotations

import unittest

import numpy as np

from .e3_p10_magnitude_refine import _screen_controls


class P10MagnitudeRefineTests(unittest.TestCase):
    def test_screen_is_deterministic_and_contains_identity(self) -> None:
        lower = np.asarray((-1.0, -2.0))
        upper = np.asarray((0.0, 3.0))
        first = _screen_controls(64, lower, upper)
        second = _screen_controls(64, lower, upper)
        np.testing.assert_array_equal(first, second)
        np.testing.assert_array_equal(first[0], np.zeros(2))
        self.assertTrue(np.all(first >= lower))
        self.assertTrue(np.all(first <= upper))


if __name__ == "__main__":
    unittest.main()
