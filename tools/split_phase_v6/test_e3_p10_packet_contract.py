from __future__ import annotations

import unittest

import numpy as np

from .e3_p10_packet_contract import (
    PACKET_ABSOLUTE_CEILINGS_DB,
    PACKET_GATES_DB,
    packet_gate_deltas,
    packet_gate_failures,
)


class P10PacketContractTests(unittest.TestCase):
    def test_each_window_has_an_independent_relative_gate(self) -> None:
        reference = {
            "18000": {metric: -40.0 for metric in PACKET_GATES_DB},
        }
        candidate = {
            "18000": {
                metric: -40.0 + tolerance
                for metric, tolerance in PACKET_GATES_DB.items()
            },
        }
        self.assertEqual(packet_gate_failures(candidate, reference), [])
        deltas = packet_gate_deltas(candidate, reference)
        np.testing.assert_allclose(
            list(deltas["18000"].values()),
            list(PACKET_GATES_DB.values()),
            rtol=0.0,
            atol=1.0e-12,
        )

    def test_peak_regression_is_not_hidden_by_window_energy(self) -> None:
        reference = {
            "18000": {metric: -40.0 for metric in PACKET_GATES_DB},
        }
        candidate = {
            "18000": {metric: -50.0 for metric in PACKET_GATES_DB},
        }
        candidate["18000"]["maximum_onset_pre_echo_db_peak"] = -39.89
        self.assertEqual(
            packet_gate_failures(candidate, reference),
            ["packet/18000/maximum_onset_pre_echo_db_peak"],
        )

    def test_floor_level_diffuse_energy_uses_absolute_ceiling(self) -> None:
        reference = {
            "18000": {metric: -150.0 for metric in PACKET_GATES_DB},
        }
        candidate = {
            "18000": {metric: -150.0 for metric in PACKET_GATES_DB},
        }
        for metric, ceiling in PACKET_ABSOLUTE_CEILINGS_DB.items():
            candidate["18000"][metric] = ceiling
        self.assertEqual(packet_gate_failures(candidate, reference), [])
        candidate["18000"]["onset_pre_echo_energy_db_0p5_2ms"] = (
            PACKET_ABSOLUTE_CEILINGS_DB[
                "onset_pre_echo_energy_db_0p5_2ms"
            ]
            + 0.01
        )
        self.assertEqual(
            packet_gate_failures(candidate, reference),
            ["packet/18000/onset_pre_echo_energy_db_0p5_2ms"],
        )


if __name__ == "__main__":
    unittest.main()
