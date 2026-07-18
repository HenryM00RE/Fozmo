from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

import numpy as np

from .resume_checkpoint import CheckpointIntegrityError, load_checkpoint, write_checkpoint


class ResumeCheckpointTests(unittest.TestCase):
    def test_round_trip_verifies_identity_and_arrays(self) -> None:
        identity = {"solver": "SCS", "backend": "gpu", "order": 4}
        metadata = {"round_index": 0, "stage_index": 1, "chunk_index": 3}
        arrays = {
            "solver_x": np.linspace(0.0, 1.0, 7),
            "solver_y": np.linspace(1.0, 2.0, 5),
            "solver_s": np.linspace(2.0, 3.0, 5),
            "gram": np.eye(5),
        }
        with tempfile.TemporaryDirectory() as directory:
            work_dir = Path(directory)
            manifest = write_checkpoint(work_dir, 4, identity, metadata, arrays)
            loaded = load_checkpoint(work_dir, 4, identity)
            self.assertEqual(loaded.metadata, metadata)
            self.assertEqual(loaded.state_path.name, manifest["state_file"])
            for name, value in arrays.items():
                np.testing.assert_array_equal(loaded.arrays[name], value)
            self.assertTrue(loaded.state_path.with_suffix(".json").is_file())

    def test_corrupt_state_is_rejected(self) -> None:
        identity = {"solver": "SCS", "backend": "gpu", "order": 4}
        with tempfile.TemporaryDirectory() as directory:
            work_dir = Path(directory)
            manifest = write_checkpoint(
                work_dir,
                4,
                identity,
                {"round_index": 0, "stage_index": 0, "chunk_index": 1},
                {"solver_x": np.ones(3)},
            )
            state_path = work_dir / manifest["state_file"]
            with state_path.open("ab") as handle:
                handle.write(b"corruption")
            with self.assertRaisesRegex(CheckpointIntegrityError, "SHA-256 mismatch"):
                load_checkpoint(work_dir, 4, identity)

    def test_mismatched_identity_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            work_dir = Path(directory)
            write_checkpoint(
                work_dir,
                4,
                {"solver": "SCS", "backend": "gpu", "order": 4},
                {"round_index": 0, "stage_index": 0, "chunk_index": 1},
                {"solver_x": np.ones(3)},
            )
            with self.assertRaisesRegex(CheckpointIntegrityError, "identity"):
                load_checkpoint(work_dir, 4, {"solver": "SCS", "backend": "mkl", "order": 4})


if __name__ == "__main__":
    unittest.main()
