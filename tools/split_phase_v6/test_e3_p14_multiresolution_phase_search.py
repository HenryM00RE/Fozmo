from __future__ import annotations

import unittest
from pathlib import Path

import numpy as np

from .e3_p12_gaussian_phase_search import evaluate_exact
from .e3_p14_multiresolution_phase_search import initial_controls


class P14MultiresolutionPhaseSearchTests(unittest.TestCase):
    def test_initial_controls_embed_p12_post_lobe_exactly(self) -> None:
        root = Path(__file__).resolve().parents[2]
        specifications, controls = initial_controls()
        self.assertEqual(len(specifications), 145)
        self.assertEqual(controls.shape, (145,))
        record, _ = evaluate_exact(
            root,
            controls,
            basis_specifications=specifications,
        )
        self.assertEqual(
            record["character_sha256"],
            "12f45d169899edf53f14ecc32ee715a4ba24ce4c28eab7e12e3efcb8d6f8b30f",
        )
        self.assertTrue(record["passes_exact_static_packet_frequency_gates"])


if __name__ == "__main__":
    unittest.main()
