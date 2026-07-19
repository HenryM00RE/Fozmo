from __future__ import annotations

import hashlib
import json
import math
import platform
import subprocess
from pathlib import Path
from typing import Any

import numpy as np
from scipy import signal

from .finite_support import project_cleanup_constraints


def _fsum(values: np.ndarray) -> float:
    return math.fsum(float(value) for value in values)


def _repair_sum(values: np.ndarray, target: float, symmetric: bool = False) -> np.ndarray:
    result = np.asarray(values, dtype=np.float64).copy()
    if symmetric:
        left = int(np.argmax(np.abs(result[: result.size // 2])))
        right = result.size - 1 - left
        for _ in range(4):
            correction = target - _fsum(result)
            result[left] += correction * 0.5
            result[right] += correction * 0.5
    else:
        index = int(np.argmax(np.abs(result)))
        for _ in range(4):
            result[index] += target - _fsum(result)
    return result


def design_cleanup_assets(branch_taps: list[int]) -> tuple[list[np.ndarray], list[dict[str, Any]]]:
    assets: list[np.ndarray] = []
    reports: list[dict[str, Any]] = []
    # Each equal-support late stage still receives an independently solved
    # minimax response. The tiny edge progression is part of the system-level
    # objective and prevents accidental asset reuse.
    transition_half_widths = [0.0350, 0.0600, 0.0900, 0.1750, 0.1800, 0.1850, 0.1900]
    for stage, (runtime_taps, transition_half_width) in enumerate(
        zip(branch_taps, transition_half_widths), start=1
    ):
        full_length = 2 * runtime_taps - 1
        pass_edge = 0.5 - transition_half_width
        stop_edge = 0.5 + transition_half_width
        try:
            canonical = signal.remez(
                full_length,
                [0.0, pass_edge, stop_edge, 1.0],
                [1.0, 0.0],
                weight=[1.0, 1.0],
                fs=2.0,
                maxiter=500,
                grid_density=64,
            ).astype(np.float64)
            solver = "remez"
        except ValueError:
            canonical = signal.firls(
                full_length,
                [0.0, pass_edge, stop_edge, 1.0],
                [1.0, 1.0, 0.0, 0.0],
                weight=[1.0, 1.0],
                fs=2.0,
            ).astype(np.float64)
            solver = "constrained_firls"
        center = full_length // 2
        if center % 2 != 0:
            raise RuntimeError("cleanup support does not place its halfband centre on even parity")
        canonical = project_cleanup_constraints(canonical)
        frequency, response = signal.freqz(canonical, worN=1 << 18, fs=2.0)
        pass_peak = float(np.max(np.abs(np.abs(response[frequency <= pass_edge]) - 1.0)))
        stop_peak = float(np.max(np.abs(response[frequency >= stop_edge])))
        assets.append(canonical)
        reports.append(
            {
                "stage": stage,
                "solver": solver,
                "canonical_length": full_length,
                "runtime_odd_branch_taps": runtime_taps,
                "pass_edge_normalized": pass_edge,
                "stop_edge_normalized": stop_edge,
                "canonical_sum": _fsum(canonical),
                "odd_sum": _fsum(canonical[1::2]),
                "passband_peak_error": pass_peak,
                "stopband_peak_db": 20.0 * math.log10(max(stop_peak, 1.0e-300)),
            }
        )
    return assets, reports


def _row_sum_project(row: np.ndarray) -> np.ndarray:
    return _repair_sum(row, 1.0, symmetric=False)


def design_rational_table(
    phase_den: int,
    half_width: int,
    magnitude: np.ndarray,
    residual_phase: np.ndarray,
    design_fft_len: int,
    source_rate: int,
    target_rate: int,
) -> tuple[np.ndarray, dict[str, Any]]:
    row_taps = 2 * half_width + 1
    working_fft_len = 65536
    stride = design_fft_len // working_fft_len
    base_magnitude = np.asarray(magnitude)[::stride].copy()
    base_phase = np.asarray(residual_phase)[::stride]
    omega = np.linspace(0.0, np.pi, working_fft_len // 2 + 1)
    frequency = omega * source_rate / (2.0 * np.pi)
    family_scale = source_rate / 44100.0
    reference_axis = np.linspace(0.0, 44100.0, base_magnitude.size)
    mapped_reference_frequency = frequency / family_scale
    base_magnitude = np.interp(
        mapped_reference_frequency, reference_axis, base_magnitude
    )
    base_phase = np.interp(mapped_reference_frequency, reference_axis, base_phase)
    pass_edge = 20000.0 * family_scale
    stop_edge = 22050.0 * family_scale
    if target_rate < source_rate:
        anti_alias_scale = target_rate / source_rate
        pass_edge *= anti_alias_scale
        stop_edge *= anti_alias_scale
        mapped_frequency = np.minimum(frequency / anti_alias_scale, source_rate / 2.0)
        source_axis = frequency
        base_magnitude = np.interp(mapped_frequency, source_axis, base_magnitude)
        base_phase = np.interp(mapped_frequency, source_axis, base_phase)
    base_magnitude[frequency >= stop_edge] = 0.0
    transition = (frequency > pass_edge) & (frequency < stop_edge)
    if np.any(transition):
        t = (frequency[transition] - pass_edge) / (stop_edge - pass_edge)
        smooth = t**4 * (35.0 + t * (-84.0 + t * (70.0 - 20.0 * t)))
        base_magnitude[transition] *= 1.0 - smooth

    rows = np.empty((phase_den, row_taps), dtype=np.float64)
    center = half_width
    for phase_index in range(phase_den):
        tau = phase_index / phase_den
        spectrum = base_magnitude * np.exp(
            1j * (base_phase - omega * (center + tau))
        )
        periodic = np.fft.irfft(spectrum, n=working_fft_len)
        row = np.asarray(periodic[:row_taps], dtype=np.float64)
        rows[phase_index] = _row_sum_project(row)
    row_sums = np.asarray([_fsum(row) for row in rows])
    # Dense row certification uses the same runtime-centred convention as the
    # Rust table lookup rather than treating rows as standalone causal FIRs.
    probe = np.fft.rfft(rows, n=16384, axis=1)
    probe_frequency = np.linspace(0.0, source_rate / 2.0, probe.shape[1])
    pass_mask = probe_frequency <= pass_edge
    stop_mask = probe_frequency >= stop_edge
    pass_magnitudes = np.abs(probe[:, pass_mask])
    stop_peak = float(np.max(np.abs(probe[:, stop_mask])))
    report = {
        "phase_den": phase_den,
        "half_width": half_width,
        "row_taps": row_taps,
        "source_rate": source_rate,
        "target_rate": target_rate,
        "maximum_row_sum_error": float(np.max(np.abs(row_sums - 1.0))),
        "passband_ripple_db": float(
            20.0
            * np.log10(
                max(float(np.max(pass_magnitudes)), 1.0e-300)
                / max(float(np.min(pass_magnitudes)), 1.0e-300)
            )
        ),
        "stopband_peak_db": 20.0 * math.log10(max(stop_peak, 1.0e-300)),
    }
    return rows.reshape(-1), report


def _write_f64le(path: Path, values: np.ndarray) -> dict[str, Any]:
    data = np.asarray(values, dtype="<f8").tobytes(order="C")
    path.write_bytes(data)
    return {
        "file": path.name,
        "coefficient_count": int(np.asarray(values).size),
        "byte_length": len(data),
        "sha256": hashlib.sha256(data).hexdigest(),
    }


def export_assets(
    asset_dir: Path,
    character: np.ndarray,
    cleanups: list[np.ndarray],
    rational_147_160: np.ndarray,
    rational_160_147: np.ndarray,
    metadata: dict[str, Any],
) -> dict[str, Any]:
    asset_dir.mkdir(parents=True, exist_ok=True)
    files = {
        "character": _write_f64le(asset_dir / "character_full_rate.f64le", character),
        "rational_147_160": _write_f64le(
            asset_dir / "rational_147_160.f64le", rational_147_160
        ),
        "rational_160_147": _write_f64le(
            asset_dir / "rational_160_147.f64le", rational_160_147
        ),
    }
    files["cleanups"] = [
        _write_f64le(asset_dir / f"cleanup_stage_{index}.f64le", cleanup)
        for index, cleanup in enumerate(cleanups, start=1)
    ]
    try:
        source_commit = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], text=True
        ).strip()
        dirty = bool(subprocess.check_output(["git", "status", "--porcelain"], text=True))
    except (OSError, subprocess.CalledProcessError):
        source_commit = "unknown"
        dirty = True
    certification = metadata.get("certification", {})
    alignment = {
        key: int(certification[key])
        for key in (
            "full_rate_origin",
            "phase0_prepad",
            "phase1_prepad",
            "decimation_prepad",
        )
    }
    manifest = {
        "format_version": 1,
        "identity": "SplitPhase128kV3",
        "endianness": "little",
        "scalar": "IEEE-754 binary64",
        "source_git_commit": source_commit,
        "generator_git_commit": source_commit,
        "source_tree_dirty": dirty,
        "platform": platform.platform(),
        "python": platform.python_version(),
        "alignment": alignment,
        "files": files,
        **metadata,
    }
    (asset_dir / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return manifest
