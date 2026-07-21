from __future__ import annotations

import unittest

from .e3_p17_cross_rate_packet_repair import (
    equivalent_reference_frequency,
    training_packet_cells,
)


class P17CrossRatePacketRepairTests(unittest.TestCase):
    def test_48khz_packets_map_to_identical_four_times_geometry(self) -> None:
        self.assertEqual(equivalent_reference_frequency(15_000.0), 13_781.25)
        self.assertEqual(equivalent_reference_frequency(20_000.0), 18_375.0)

    def test_training_contract_contains_all_three_packet_sets(self) -> None:
        cells = training_packet_cells()
        self.assertEqual(len(cells), 31)
        self.assertIn((13_781.25, 8.0), cells)
        self.assertIn((19_500.0, 16.0), cells)


if __name__ == "__main__":
    unittest.main()
