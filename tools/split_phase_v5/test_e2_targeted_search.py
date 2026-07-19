from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

import numpy as np

from .e2_targeted_search import (
    IDENTITY,
    _atomic_json,
    _initial_specs,
    _load_proxy_checkpoints,
    _sha256_array,
)


class E2TargetedSearchTests(unittest.TestCase):
    def test_initial_specs_cover_both_sides_of_every_coordinate(self) -> None:
        baseline = np.asarray([1.0, -2.0, 3.0])
        specs = _initial_specs(baseline, 0.02)
        self.assertEqual(len(specs), 7)
        np.testing.assert_array_equal(specs[0][1], baseline)
        for coordinate in range(baseline.size):
            plus = specs[1 + 2 * coordinate][1]
            minus = specs[2 + 2 * coordinate][1]
            self.assertAlmostEqual(plus[coordinate] - baseline[coordinate], 0.02)
            self.assertAlmostEqual(minus[coordinate] - baseline[coordinate], -0.02)

    def test_proxy_checkpoint_hash_is_verified(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            free = np.asarray([1.0, 2.0])
            report = {
                "identity": IDENTITY,
                "index": 0,
                "free": free.tolist(),
                "free_sha256": _sha256_array(free),
            }
            _atomic_json(directory / "candidate_00000.json", report)
            self.assertEqual(_load_proxy_checkpoints(directory)[0]["free"], [1.0, 2.0])
            report["free"] = [1.0, 3.0]
            _atomic_json(directory / "candidate_00000.json", report)
            with self.assertRaisesRegex(RuntimeError, "corrupt E2 checkpoint"):
                _load_proxy_checkpoints(directory)


if __name__ == "__main__":
    unittest.main()
