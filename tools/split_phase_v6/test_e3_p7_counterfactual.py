from __future__ import annotations

import math
import unittest
from pathlib import Path

import numpy as np

from .e3_p6_restarted_carrier_search import RestartedCarrierProbe
from .e3_p7_counterfactual import (
    CounterfactualFixture,
    CounterfactualResidual,
    cleanup_counterfactual_residual,
)
from .e3_phase_search import _cascade_character_and_cleanup, _read_f64le


class CounterfactualResidualTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.root = Path(__file__).resolve().parents[2]
        cls.character = _read_f64le(
            cls.root / "tools/split_phase_v6/baselines/e3-p6d-local-0145.f64le"
        )
        cls.cleanup = _read_f64le(
            cls.root / "assets/filters/split_phase_e2v3/cleanup_stage_1.f64le"
        )

    def test_large_mute_matches_p6_closed_form(self) -> None:
        old = RestartedCarrierProbe()
        response = _cascade_character_and_cleanup(self.character, self.cleanup)
        expected = old.residual(response)[: old.trace_samples]
        fixture = CounterfactualFixture(
            "p6-compatible",
            old.frequencies_hz,
            (0.31 + math.pi, 1.17 + math.pi),
            mute_source_frames=response.size,
        )
        actual = CounterfactualResidual(fixture, 176_400, 5_314).residual(response)
        np.testing.assert_allclose(actual, expected, atol=2.0e-13, rtol=2.0e-12)

    def test_staged_cleanup_matches_full_response(self) -> None:
        fixture = CounterfactualFixture(
            "stage-equivalence",
            (18_000.0, 19_000.0),
            (0.31 + math.pi, 1.17 + math.pi),
            mute_source_frames=8_192,
        )
        response = _cascade_character_and_cleanup(self.character, self.cleanup)
        direct = CounterfactualResidual(fixture, 176_400, 5_314).residual(response)
        staged = cleanup_counterfactual_residual(self.character, self.cleanup, fixture)
        np.testing.assert_allclose(staged, direct, atol=3.0e-13, rtol=3.0e-11)


if __name__ == "__main__":
    unittest.main()
