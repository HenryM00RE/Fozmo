from __future__ import annotations

import json
import unittest
from pathlib import Path

import numpy as np

from .e3_p9_nonlinear_refine import (
    _objective_value,
    _recover_feasible_boundary,
    _timing_subspace,
)
from .e3_p9_timing_search import _coordinate_scales


class E3P9NonlinearRefineTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        path = (
            Path(__file__).resolve().parent / "baselines/e3-p9-feasibility-dense.json"
        )
        if not path.exists():
            raise unittest.SkipTest("dense P9 feasibility report is not present")
        cls.feasibility = json.loads(path.read_text(encoding="utf-8"))

    def test_timing_subspace_is_orthonormal_and_packet_null(self) -> None:
        basis, contract = _timing_subspace(self.feasibility, 7)
        self.assertLessEqual(basis.shape[1], 7)
        np.testing.assert_allclose(
            basis.T @ basis, np.eye(basis.shape[1]), atol=1.0e-12
        )
        jacobian = np.asarray(self.feasibility["jacobian"], dtype=np.float64)
        names = self.feasibility["result_names"]
        rows = [index for index, name in enumerate(names) if name.startswith("packet/")]
        phase_count = len(self.feasibility["contract"]["phase_knots_hz"]) - 2
        scales = _coordinate_scales(phase_count)
        np.testing.assert_allclose(
            (jacobian[rows] * scales[None, :]) @ basis,
            0.0,
            atol=1.0e-9,
        )
        self.assertEqual(contract["selected_dimensions"], basis.shape[1])

    def test_objectives_use_native_db_metrics(self) -> None:
        timing = {
            "maximum_pre_lobe_db_peak": -20.0,
            "maximum_post_lobe_db_peak": -10.0,
            "pre_energy_db_total": -5.0,
            "post_energy_db_total": -3.0,
        }
        self.assertEqual(_objective_value(timing, "pre_lobe"), -20.0)
        self.assertEqual(_objective_value(timing, "post_lobe"), -10.0)
        self.assertEqual(_objective_value(timing, "side_energy"), -8.0)

    def test_boundary_recovery_retains_last_feasible_segment_point(self) -> None:
        class Evaluator:
            @staticmethod
            def evaluate(values: np.ndarray) -> dict[str, bool]:
                safe = bool(values[0] <= 0.4)
                return {
                    "passes_static_gates": safe,
                    "passes_packet_gates": safe,
                }

        values, fraction = _recover_feasible_boundary(
            Evaluator(), np.asarray([0.0]), np.asarray([1.0]), iterations=20
        )
        self.assertLessEqual(values[0], 0.4)
        self.assertAlmostEqual(values[0], 0.4, places=5)
        self.assertAlmostEqual(fraction, values[0], places=12)


if __name__ == "__main__":
    unittest.main()
