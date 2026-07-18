from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import numpy as np

from . import magnitude_sdp


class SimulatedCrash(RuntimeError):
    pass


class MagnitudeResumeTests(unittest.TestCase):
    def test_scs_primal_dual_slack_state_resumes_after_crash(self) -> None:
        spec = magnitude_sdp.MagnitudeSpec(
            order=16,
            pass_edge_hz=1_000.0,
            stop_edge_hz=15_000.0,
            passband_amplitude_ripple=0.1,
            stopband_amplitude_db=-10.0,
            verification_fft_len=4_096,
            maximum_exchange_rounds=1,
        )
        real_write = magnitude_sdp.write_checkpoint
        real_options = magnitude_sdp._solver_options
        write_count = 0

        def short_options(*args, **kwargs):
            options = real_options(*args, **kwargs)
            options["max_iters"] = 300
            return options

        def crash_after_first_checkpoint(*args, **kwargs):
            nonlocal write_count
            manifest = real_write(*args, **kwargs)
            write_count += 1
            if write_count == 1:
                raise SimulatedCrash("intentional crash after a durable checkpoint")
            return manifest

        with tempfile.TemporaryDirectory() as directory:
            work_dir = Path(directory)
            with patch.object(magnitude_sdp, "_solver_options", side_effect=short_options):
                with self.assertRaises(SimulatedCrash):
                    with patch.object(
                        magnitude_sdp,
                        "write_checkpoint",
                        side_effect=crash_after_first_checkpoint,
                    ):
                        magnitude_sdp.solve(
                            spec,
                            work_dir,
                            "SCS",
                            "indirect",
                            "initial",
                            checkpoint_iterations=100,
                        )
                first = json.loads((work_dir / "magnitude_order_16_resume.json").read_text())
                self.assertEqual(first["metadata"]["stage_iterations"], 100)
                self.assertFalse(first["metadata"]["stage_complete"])
                try:
                    magnitude_sdp.solve(
                        spec,
                        work_dir,
                        "SCS",
                        "indirect",
                        "initial",
                        checkpoint_iterations=100,
                        resume=True,
                    )
                except RuntimeError as error:
                    self.assertIn("failed independent verification", str(error))

                uninterrupted_dir = work_dir / "uninterrupted"
                try:
                    magnitude_sdp.solve(
                        spec,
                        uninterrupted_dir,
                        "SCS",
                        "indirect",
                        "initial",
                        checkpoint_iterations=100,
                    )
                except RuntimeError as error:
                    self.assertIn("failed independent verification", str(error))

            latest = json.loads((work_dir / "magnitude_order_16_resume.json").read_text())
            self.assertEqual(latest["metadata"]["stage_index"], 2)
            self.assertEqual(latest["metadata"]["total_iterations"], 900)
            self.assertGreaterEqual(len(list(work_dir.glob("magnitude_order_16_resume_*.json"))), 9)
            with np.load(work_dir / "magnitude_order_16.npz") as resumed, np.load(
                uninterrupted_dir / "magnitude_order_16.npz"
            ) as uninterrupted:
                np.testing.assert_allclose(resumed["autocorrelation"], uninterrupted["autocorrelation"], rtol=0.0, atol=1.0e-3)
                np.testing.assert_allclose(resumed["gram"], uninterrupted["gram"], rtol=0.0, atol=1.0e-3)
            resumed_report = json.loads((work_dir / "magnitude_order_16.json").read_text())
            uninterrupted_report = json.loads((uninterrupted_dir / "magnitude_order_16.json").read_text())
            self.assertTrue(resumed_report["accepted"])
            self.assertTrue(uninterrupted_report["accepted"])


if __name__ == "__main__":
    unittest.main()
