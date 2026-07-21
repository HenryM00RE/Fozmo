from __future__ import annotations

import unittest

from .e3_p10_holdout_refine import _packet_cells


class P10HoldoutRefineTests(unittest.TestCase):
    def test_packet_cells_are_unique_and_cover_production_plus_holdouts(self) -> None:
        cells = _packet_cells()
        self.assertEqual(len(cells), 26)
        self.assertEqual(len({cell[0] for cell in cells}), len(cells))
        self.assertEqual(sum(name.startswith("production-") for name, _, _ in cells), 5)
        self.assertEqual(sum(name.startswith("holdout-") for name, _, _ in cells), 21)


if __name__ == "__main__":
    unittest.main()
