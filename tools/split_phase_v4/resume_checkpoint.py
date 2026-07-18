from __future__ import annotations

import hashlib
import json
import os
import tempfile
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping

import numpy as np


FORMAT_VERSION = 1


class CheckpointIntegrityError(RuntimeError):
    """Raised when a resume checkpoint is incomplete, corrupt, or incompatible."""


@dataclass(frozen=True)
class LoadedCheckpoint:
    metadata: dict[str, Any]
    arrays: dict[str, np.ndarray]
    manifest_path: Path
    state_path: Path


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def sha256_array(value: np.ndarray) -> str:
    array = np.ascontiguousarray(value)
    digest = hashlib.sha256()
    digest.update(array.dtype.str.encode("ascii"))
    digest.update(json.dumps(array.shape).encode("ascii"))
    digest.update(array.tobytes(order="C"))
    return digest.hexdigest()


def _atomic_json(path: Path, payload: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(prefix=".resume-manifest-", suffix=".json", dir=path.parent)
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as handle:
            json.dump(payload, handle, indent=2, sort_keys=True, allow_nan=False)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def write_checkpoint(
    work_dir: Path,
    order: int,
    identity: Mapping[str, Any],
    metadata: Mapping[str, Any],
    arrays: Mapping[str, np.ndarray],
) -> dict[str, Any]:
    """Write immutable state first and atomically publish its manifest last."""
    work_dir.mkdir(parents=True, exist_ok=True)
    normalized = {name: np.ascontiguousarray(value) for name, value in arrays.items()}
    for name, value in normalized.items():
        if value.dtype.hasobject:
            raise ValueError(f"checkpoint array {name} has object dtype")
        if not np.all(np.isfinite(value)):
            raise ValueError(f"checkpoint array {name} contains non-finite values")

    round_index = int(metadata.get("round_index", 0))
    stage_index = int(metadata.get("stage_index", 0))
    chunk_index = int(metadata.get("chunk_index", 0))
    token = uuid.uuid4().hex[:12]
    state_name = (
        f"magnitude_order_{order}_resume_r{round_index:02d}_s{stage_index:02d}_"
        f"c{chunk_index:05d}_{token}.npz"
    )
    state_path = work_dir / state_name
    descriptor, temporary_name = tempfile.mkstemp(prefix=".resume-state-", suffix=".npz", dir=work_dir)
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            np.savez(handle, **normalized)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, state_path)
    finally:
        temporary.unlink(missing_ok=True)

    array_records = {
        name: {
            "dtype": value.dtype.str,
            "shape": list(value.shape),
            "sha256": sha256_array(value),
        }
        for name, value in normalized.items()
    }
    manifest = {
        "format_version": FORMAT_VERSION,
        "published_unix_seconds": time.time(),
        "identity": dict(identity),
        "metadata": dict(metadata),
        "state_file": state_name,
        "state_sha256": sha256_file(state_path),
        "arrays": array_records,
    }
    _atomic_json(state_path.with_suffix(".json"), manifest)
    _atomic_json(work_dir / f"magnitude_order_{order}_resume.json", manifest)
    return manifest


def load_checkpoint(work_dir: Path, order: int, expected_identity: Mapping[str, Any]) -> LoadedCheckpoint:
    manifest_path = work_dir / f"magnitude_order_{order}_resume.json"
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise CheckpointIntegrityError(f"cannot read resume manifest: {error}") from error
    if manifest.get("format_version") != FORMAT_VERSION:
        raise CheckpointIntegrityError("unsupported resume checkpoint format")
    if manifest.get("identity") != dict(expected_identity):
        raise CheckpointIntegrityError("resume checkpoint identity does not match this solver invocation")

    state_name = manifest.get("state_file")
    if not isinstance(state_name, str) or Path(state_name).name != state_name:
        raise CheckpointIntegrityError("resume checkpoint state path is unsafe")
    state_path = work_dir / state_name
    if not state_path.is_file():
        raise CheckpointIntegrityError("resume checkpoint state file is missing")
    if sha256_file(state_path) != manifest.get("state_sha256"):
        raise CheckpointIntegrityError("resume checkpoint state SHA-256 mismatch")

    try:
        with np.load(state_path, allow_pickle=False) as archive:
            arrays = {name: np.asarray(archive[name]) for name in archive.files}
    except (OSError, ValueError, KeyError) as error:
        raise CheckpointIntegrityError(f"cannot load resume state: {error}") from error
    records = manifest.get("arrays")
    if not isinstance(records, dict) or set(records) != set(arrays):
        raise CheckpointIntegrityError("resume checkpoint array inventory mismatch")
    for name, array in arrays.items():
        record = records[name]
        if list(array.shape) != record.get("shape") or array.dtype.str != record.get("dtype"):
            raise CheckpointIntegrityError(f"resume checkpoint array layout mismatch: {name}")
        if sha256_array(array) != record.get("sha256"):
            raise CheckpointIntegrityError(f"resume checkpoint array SHA-256 mismatch: {name}")
        if not np.all(np.isfinite(array)):
            raise CheckpointIntegrityError(f"resume checkpoint array is non-finite: {name}")
    metadata = manifest.get("metadata")
    if not isinstance(metadata, dict):
        raise CheckpointIntegrityError("resume checkpoint metadata is missing")
    return LoadedCheckpoint(metadata, arrays, manifest_path, state_path)
