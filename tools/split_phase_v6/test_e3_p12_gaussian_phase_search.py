from __future__ import annotations

import unittest
from pathlib import Path

import numpy as np

from .e3_p12_gaussian_phase_search import (
    PACKET_CLEAN_CONTROLS,
    gaussian_phase_basis,
    evaluate_exact,
    realize_character,
)
from .e3_phase_search import FFT_LENGTH, _read_f64le


class P12GaussianPhaseSearchTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.root = Path(__file__).resolve().parents[2]
        cls.baseline = _read_f64le(
            cls.root
            / "assets/filters/split_phase_e2v3/character_full_rate.f64le"
        )

    def test_basis_is_closed_at_dc_and_nyquist(self) -> None:
        frequency = np.fft.rfftfreq(1 << 15, 1.0 / 88_200.0)
        basis = gaussian_phase_basis(frequency)
        self.assertEqual(basis.shape, (51, frequency.size))
        np.testing.assert_allclose(basis[:, 0], 0.0, atol=1.0e-15)
        np.testing.assert_allclose(basis[:, -1], 0.0, atol=1.0e-15)

    def test_zero_controls_reproduce_e2v3(self) -> None:
        character, realization = realize_character(
            self.baseline, np.zeros(51), fft_length=FFT_LENGTH
        )
        np.testing.assert_allclose(character, self.baseline, atol=2.0e-16)
        self.assertLess(realization["omitted_periodic_energy_ratio"], 1.0e-28)

    def test_frozen_anchor_passes_exact_timing_and_packet_gates(self) -> None:
        record, _ = evaluate_exact(self.root, PACKET_CLEAN_CONTROLS)
        self.assertTrue(record["passes_exact_static_packet_frequency_gates"])
        self.assertEqual(record["packet_failures"], [])
        self.assertEqual(record["timing_failures"], [])
        self.assertEqual(
            record["character_sha256"],
            "828a8c357415e2eff2547d778eca13aa480c80aeb5e5fe4873334018bc004e19",
        )
        self.assertAlmostEqual(
            record["timing"]["main_lobe_width_us"], 62.405365757480205
        )


if __name__ == "__main__":
    unittest.main()
