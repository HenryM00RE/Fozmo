from __future__ import annotations

import unittest
from pathlib import Path

import numpy as np

from tools.split_phase_v4.magnitude_sdp import MagnitudeSpec, _dense_metrics, autocorrelation_from_gram, evaluate_power
from tools.split_phase_v5.c_derived_magnitude import (
    SeedSearchSpec,
    candidate_coefficients,
    gram_with_positive_floor,
    load_split_c_character,
    minimum_phase_seed,
    search_c_guided_seed,
)


class CDerivedMagnitudeTests(unittest.TestCase):
    def test_positive_floor_gram_matches_direct_power(self) -> None:
        coefficients = np.asarray([0.2, 0.3, 0.5], dtype=np.float64)
        floor = 1.0e-8
        gram = gram_with_positive_floor(coefficients, floor)
        autocorrelation = autocorrelation_from_gram(gram)
        fft_len = 8192
        expected = (np.abs(np.fft.rfft(coefficients, n=fft_len)) ** 2 + floor) / (1.0 + floor)
        actual = evaluate_power(autocorrelation, fft_len)
        self.assertLess(float(np.max(np.abs(actual - expected))), 2.0e-15)
        self.assertLess(abs(float(np.sum(gram)) - 1.0), 2.0e-15)

    def test_known_structured_seed_passes_reduced_dense_audit(self) -> None:
        spec = MagnitudeSpec(verification_fft_len=262_144)
        linear = candidate_coefficients(spec, beta=19.0, cutoff_hz=21_050.0)
        coefficients, _ = minimum_phase_seed(linear, spec.verification_fft_len)
        gram = gram_with_positive_floor(coefficients, 1.0e-15)
        metrics = _dense_metrics(autocorrelation_from_gram(gram), spec)
        self.assertLessEqual(metrics["passband_amplitude_ripple"], spec.passband_amplitude_ripple)
        self.assertLessEqual(metrics["stopband_amplitude_db"], spec.stopband_amplitude_db)
        self.assertGreaterEqual(metrics["global_minimum_power"], -1.0e-12)
        self.assertLessEqual(metrics["transition_maximum_upward_power"], 1.0e-11)

    def test_c_guided_search_returns_a_candidate_with_margin(self) -> None:
        root = Path(__file__).resolve().parents[2]
        character, _ = load_split_c_character(root)
        spec = MagnitudeSpec(verification_fft_len=262_144)
        search = SeedSearchSpec(screening_fft_len=262_144)
        coefficients, report = search_c_guided_seed(
            character,
            spec,
            search,
            betas=(18.75, 19.0),
            cutoffs_hz=(21_025.0, 21_050.0),
        )
        self.assertEqual(coefficients.size, spec.order + 1)
        self.assertGreaterEqual(report["feasible_candidates"], 1)
        self.assertLessEqual(report["selected"]["gate_utilization"], search.maximum_gate_utilization)


if __name__ == "__main__":
    unittest.main()
