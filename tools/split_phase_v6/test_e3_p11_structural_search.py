from __future__ import annotations

import unittest
from pathlib import Path

import numpy as np

from .e3_p11_structural_search import (
    _runtime_step_metrics,
    _timing_with_runtime_step,
)
from .e3_phase_search import (
    _cascade_character_and_cleanup,
    _read_f64le,
)


class P11StructuralSearchTests(unittest.TestCase):
    def test_runtime_step_is_polyphase_not_full_rate_cumsum(self) -> None:
        response = np.asarray([1.2, 1.0, 1.0, 1.0, -0.2, 0.0, 0.0, 0.0])
        overshoot, undershoot = _runtime_step_metrics(response)
        self.assertAlmostEqual(overshoot, 20.0)
        self.assertAlmostEqual(undershoot, 0.0)

        full_rate_step = np.cumsum(response)
        self.assertGreater(float(np.max(full_rate_step)) - 4.0, 0.0)
        self.assertNotAlmostEqual(
            overshoot,
            max(float(np.max(full_rate_step)) / 4.0 - 1.0, 0.0) * 100.0,
        )

    def test_exact_p11_seed_matches_native_step_contract(self) -> None:
        root = Path(__file__).resolve().parents[2]
        candidate = (
            root
            / "tools/split_phase_v6/work-e3-p11/structural"
            / "p11-fp22000-fs24100-b8x8-a140.character.f64le"
        )
        if not candidate.exists():
            self.skipTest("P11 structural work product has not been generated")
        cleanup = _read_f64le(
            root / "assets/filters/split_phase_e2v3/cleanup_stage_1.f64le"
        )
        response = _cascade_character_and_cleanup(_read_f64le(candidate), cleanup)
        timing = _timing_with_runtime_step(response)
        self.assertAlmostEqual(
            timing["step_overshoot_percent"], 18.0106585221, places=8
        )
        self.assertAlmostEqual(
            timing["step_undershoot_percent"], 8.8657180267, places=8
        )


if __name__ == "__main__":
    unittest.main()
