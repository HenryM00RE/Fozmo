from __future__ import annotations

import unittest
from pathlib import Path

import numpy as np

from .e3_p8_capacity_audit import MODEL_ORDERS, _magnitude_order_audit, _p6_target, _realize


class CapacityAuditTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.root = Path(__file__).resolve().parents[2]

    def test_p6_target_reconstructs_frozen_incumbent(self) -> None:
        _, periodic, contract = _p6_target(self.root)
        candidate, omitted = _realize(periodic, 262_145, contract["normalization_sum"])
        frozen = np.fromfile(
            self.root / "tools/split_phase_v6/baselines/e3-p6d-local-0145.f64le",
            dtype="<f8",
        )
        self.assertLessEqual(float(np.max(np.abs(candidate - frozen))), 1.0e-16)
        self.assertAlmostEqual(
            omitted,
            contract["reported_omitted_periodic_energy_ratio"],
            delta=1.0e-24,
        )

    def test_magnitude_order_residual_is_monotone(self) -> None:
        target, _, _ = _p6_target(self.root)
        records = _magnitude_order_audit(np.abs(target))
        self.assertEqual([record["order"] for record in records], list(MODEL_ORDERS))
        omitted = [record["omitted_autocorrelation_energy_ratio"] for record in records]
        self.assertTrue(all(right <= left for left, right in zip(omitted, omitted[1:])))


if __name__ == "__main__":
    unittest.main()
