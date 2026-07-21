from __future__ import annotations

import unittest

import numpy as np

from .e3_p8_cleanup_support import _embed_centered


class CleanupSupportTests(unittest.TestCase):
    def test_embedding_preserves_halfband_structure_and_response(self) -> None:
        source = np.zeros(509, dtype=np.float64)
        source[254] = 0.5
        positions = np.arange(1, 254, 2)
        source[positions] = np.linspace(0.0, 0.25, positions.size)
        source[source.size - 1 - positions] = source[positions]
        expanded = _embed_centered(source, 765)
        self.assertEqual(expanded.size, 765)
        np.testing.assert_array_equal(expanded[128 : 128 + source.size], source)
        self.assertEqual(expanded[expanded.size // 2], 0.5)
        np.testing.assert_array_equal(expanded, expanded[::-1])

    def test_invalid_support_is_rejected(self) -> None:
        with self.assertRaises(ValueError):
            _embed_centered(np.zeros(509), 763)


if __name__ == "__main__":
    unittest.main()
