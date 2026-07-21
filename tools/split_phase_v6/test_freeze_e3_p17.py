from __future__ import annotations

import hashlib
import json
import unittest
from pathlib import Path

from .freeze_e3_p17 import CHARACTER_SHA256, SELECTED_IDENTIFIER


class FreezeE3P17Tests(unittest.TestCase):
    def test_frozen_replacement_is_hash_addressed_and_dominates_e2v3(self) -> None:
        root = Path(__file__).resolve().parents[2]
        baseline_dir = root / "tools/split_phase_v6/baselines"
        report = json.loads(
            (baseline_dir / "e3-p17-definitive-freeze.json").read_text(
                encoding="utf-8"
            )
        )
        payload = (
            baseline_dir / "e3-p17-replacement-candidate.f64le"
        ).read_bytes()

        self.assertEqual(SELECTED_IDENTIFIER, "p17-balanced")
        self.assertEqual(hashlib.sha256(payload).hexdigest(), CHARACTER_SHA256)
        self.assertTrue(report["decision"]["clear_replacement_found"])
        self.assertFalse(report["production_promoted"])
        self.assertTrue(
            report["timing_comparison"][
                "candidate_dominates_e2v3_on_all_frozen_metrics"
            ]
        )
        self.assertTrue(
            all(
                delta < 0.0
                for delta in report["timing_comparison"][
                    "candidate_delta_vs_e2v3"
                ].values()
            )
        )
        self.assertEqual(
            report["selected_replacement_candidate"][
                "production_packet_failures"
            ],
            [],
        )
        self.assertEqual(
            report["selected_replacement_candidate"]["holdout_packet_failures"],
            [],
        )
        self.assertEqual(report["dsd128_validation"]["hard_failure_count"], 0)
        self.assertTrue(
            all(
                delta < 0.0
                for delta in report["dsd128_validation"][
                    "delta_vs_p6"
                ].values()
            )
        )


if __name__ == "__main__":
    unittest.main()
