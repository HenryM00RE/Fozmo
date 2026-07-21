from __future__ import annotations

import unittest

import numpy as np

from .e3_p7_cleanup_search import _frequency_fft_length
from .e3_p9_timing_search import SUPPORTS, _coordinate_scales, _fft_length_for_support


class P9TimingSearchTests(unittest.TestCase):
    def test_fft_lengths_cover_linear_realization(self) -> None:
        for support in SUPPORTS:
            self.assertGreaterEqual(_fft_length_for_support(support), 2 * support - 1)

    def test_coordinate_scales_are_positive(self) -> None:
        scales = _coordinate_scales()
        self.assertEqual(scales.size, 17)
        self.assertTrue(np.all(scales > 0.0))
        dense_scales = _coordinate_scales(20)
        self.assertEqual(dense_scales.size, 28)

    def test_frequency_fft_covers_million_tap_cascade(self) -> None:
        response_size = 1_048_577 + 509 - 1
        fft_length = _frequency_fft_length(response_size, 262_145 + 509 - 1)
        self.assertEqual(fft_length, 1 << 21)
        self.assertGreaterEqual(fft_length, response_size)


if __name__ == "__main__":
    unittest.main()
