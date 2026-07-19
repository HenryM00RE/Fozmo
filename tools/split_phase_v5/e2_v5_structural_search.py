from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import numpy as np
from scipy import interpolate, linalg
from scipy.stats import qmc

from tools.split_phase_v4.analytic_group_delay import physical_log_derivatives
from tools.split_phase_v4.certify import _resample_target
from tools.split_phase_v4.group_delay_spline import ConstrainedDelaySpline, _basis
from tools.split_phase_v4.report import _spectrum_for_coordinates
from tools.split_phase_v4.support_search import _proxy_score, score_candidate
from tools.split_phase_v5.e2_targeted_search import (
    PROXY_FFT_LEN,
    SUPPORT,
    _atomic_json,
    _atomic_npy,
    _full_audit,
    _keys,
    _load_state,
    _load_thresholds,
    _resume_lawson,
    _result_key,
    _sha256_array,
    _sha256_file,
    finalize_screening_winner,
)


IDENTITY = "SplitPhase128kV5-E2v5-structural-delay-search-experimental"
STRUCTURES = ((30, 14_000.0), (30, 15_000.0), (36, 14_000.0), (36, 15_000.0))
CANDIDATES_PER_STRUCTURE = 64
COORDINATE_RADIUS = 0.10
SOBOL_RADIUS = 0.05


@dataclass(frozen=True)
class StructuralState:
    identifier: str
    controls: int
    join_hz: float
    model_sha256: str
    state: dict[str, Any]
    center: np.ndarray
    projection_max_abs_error: float


def _build_structural_spline(
    minimum_frequency_hz: np.ndarray,
    minimum_delay: np.ndarray,
    controls: int,
    join_hz: float,
    degree: int = 5,
) -> ConstrainedDelaySpline:
    if controls < 2 * (degree + 1):
        raise ValueError("too few controls for a clamped spline")
    lo_hz = 3000.0
    lo = math.log(lo_hz)
    hi = math.log(join_hz)
    interior_count = controls - degree - 1
    interior = np.linspace(lo, hi, interior_count + 2)[1:-1]
    knots = np.concatenate((np.full(degree + 1, lo), interior, np.full(degree + 1, hi)))
    log_frequency = np.log(np.asarray(minimum_frequency_hz, dtype=np.float64))
    minimum = np.asarray(minimum_delay, dtype=np.float64)
    minimum_slope = np.gradient(minimum, log_frequency, edge_order=2)
    minimum_curvature = np.gradient(minimum_slope, log_frequency, edge_order=2)
    rows: list[np.ndarray] = []
    right: list[float] = []
    for derivative, target in ((0, 0.0), (1, 0.0), (2, 0.0)):
        row = np.zeros(controls + 1, dtype=np.float64)
        row[:controls] = _basis(knots, degree, np.asarray([lo]), derivative)[0]
        if derivative == 0:
            row[-1] = -1.0
        rows.append(row)
        right.append(target)
    for derivative, values in ((0, minimum), (1, minimum_slope), (2, minimum_curvature)):
        row = np.zeros(controls + 1, dtype=np.float64)
        row[:controls] = _basis(knots, degree, np.asarray([hi]), derivative)[0]
        rows.append(row)
        right.append(float(np.interp(hi, log_frequency, values)))
    integration_frequency = np.linspace(max(float(minimum_frequency_hz[0]), 1.0e-6), join_hz, 8193)
    omega = 2.0 * np.pi * integration_frequency / 88_200.0
    closure = np.zeros(controls + 1, dtype=np.float64)
    transition = integration_frequency >= lo_hz
    transition_basis = _basis(knots, degree, np.log(integration_frequency[transition]), 0)
    for index in range(controls):
        values = np.zeros(integration_frequency.size, dtype=np.float64)
        values[transition] = transition_basis[:, index]
        closure[index] = np.trapezoid(values, omega)
    closure[-1] = np.trapezoid((~transition).astype(np.float64), omega)
    rows.append(closure)
    right.append(float(np.trapezoid(np.interp(integration_frequency, minimum_frequency_hz, minimum), omega)))
    constraint = np.asarray(rows)
    target = np.asarray(right)
    particular, *_ = np.linalg.lstsq(constraint, target, rcond=None)
    nullspace = linalg.null_space(constraint)
    residual = float(np.max(np.abs(constraint @ particular - target)))
    if residual > 5.0e-10:
        raise RuntimeError("E2v5 structural equality construction failed")
    return ConstrainedDelaySpline(degree, knots, particular, nullspace, residual)


def _model_sha256(model: ConstrainedDelaySpline, join_hz: float) -> str:
    digest = hashlib.sha256()
    digest.update(IDENTITY.encode("utf-8"))
    digest.update(np.float64(join_hz).tobytes())
    for values in (np.asarray(model.degree), model.knots, model.particular, model.nullspace):
        digest.update(_sha256_array(np.asarray(values)).encode("ascii"))
    return digest.hexdigest()


def _structural_spectrum(
    state: dict[str, Any], free: np.ndarray
) -> tuple[np.ndarray, np.ndarray, dict[str, float]]:
    model: ConstrainedDelaySpline = state["model"]
    frequency = state["frequency"]
    join_hz = float(state["join_hz"])
    delay = state["minimum_delay"].copy()
    low = frequency < 3000.0
    transition = (frequency >= 3000.0) & (frequency <= join_hz)
    _, low_delay = model.coefficients_and_low_delay(free)
    delay[low] = low_delay
    delay[transition] = model.evaluate(frequency[transition], free)
    omega = np.linspace(0.0, np.pi, delay.size)
    phase = np.zeros(delay.size, dtype=np.float64)
    phase[1:] = -np.cumsum(0.5 * (delay[1:] + delay[:-1]) * np.diff(omega))
    join = int(np.searchsorted(frequency, join_hz))
    closure_error = float(phase[join] - state["minimum_phase"][join])
    closure_t = (np.log(frequency[transition]) - math.log(3000.0)) / (
        math.log(join_hz) - math.log(3000.0)
    )
    closure_shape = closure_t**4 * (35.0 + closure_t * (-84.0 + closure_t * (70.0 - 20.0 * closure_t)))
    phase[transition] -= closure_error * closure_shape
    phase[join:] = state["minimum_phase"][join:]
    delay[1:-1] = -np.gradient(phase, omega, edge_order=2)[1:-1]
    target = state["magnitude"] * np.exp(1j * (phase - omega * state["origin_c"]))
    target[0] = target[0].real
    target[-1] = target[-1].real
    reliable = (frequency >= 1.0) & (frequency <= 20_000.0)
    slope, curvature = physical_log_derivatives(frequency[reliable], delay[reliable])
    metrics = {
        "numerical_phase_closure_error_before_exact_correction_rad": closure_error,
        "target_group_delay_slope_max_abs_samples_per_ln_hz": float(np.max(np.abs(slope))),
        "target_group_delay_curvature_max_abs_samples_per_ln_hz_squared": float(np.max(np.abs(curvature))),
    }
    return target, delay, metrics


def _project_center(
    model: ConstrainedDelaySpline,
    join_hz: float,
    frequency: np.ndarray,
    reference_delay: np.ndarray,
) -> tuple[np.ndarray, float]:
    sample_frequency = np.geomspace(3000.0, join_hz, 4096)
    basis = _basis(model.knots, model.degree, np.log(sample_frequency), 0)
    target = np.interp(sample_frequency, frequency, reference_delay)
    operator = basis @ model.nullspace[:-1]
    residual_target = target - basis @ model.particular[:-1]
    low_weight = math.sqrt(128.0)
    operator = np.vstack((operator, low_weight * model.nullspace[-1]))
    residual_target = np.concatenate(
        (residual_target, [low_weight * (reference_delay[0] - model.particular[-1])])
    )
    free, *_ = np.linalg.lstsq(operator, residual_target, rcond=None)
    realized = model.evaluate(sample_frequency, free)
    return free, float(np.max(np.abs(realized - target)))


def _prepare_structures(
    root: Path, base_e_dir: Path, center_work_dir: Path
) -> list[StructuralState]:
    source = _load_state(root, base_e_dir, PROXY_FFT_LEN)
    center_path = center_work_dir / "proxy_checkpoints/candidate_02064.json"
    center_record = json.loads(center_path.read_text())
    center_free = np.asarray(center_record["free"], dtype=np.float64)
    if center_record.get("free_sha256") != _sha256_array(center_free):
        raise RuntimeError("E2v5 center candidate hash mismatch")
    _, reference_delay, _ = _spectrum_for_coordinates(
        source["model"],
        center_free,
        source["frequency"],
        source["minimum_delay"],
        source["minimum_phase"],
        source["magnitude"],
        source["origin_c"],
    )
    reliable = (source["frequency"] >= 1.0) & (source["frequency"] <= 20_000.0)
    structures = []
    for controls, join_hz in STRUCTURES:
        model = _build_structural_spline(
            source["frequency"][reliable],
            source["minimum_delay"][reliable],
            controls,
            join_hz,
        )
        center, projection_error = _project_center(
            model, join_hz, source["frequency"], reference_delay
        )
        state = dict(source)
        state.update({"model": model, "baseline_free": center, "join_hz": join_hz})
        identifier = f"controls_{controls}_join_{int(join_hz)}"
        structures.append(
            StructuralState(
                identifier,
                controls,
                join_hz,
                _model_sha256(model, join_hz),
                state,
                center,
                projection_error,
            )
        )
    return structures


def _support_slice(periodic: np.ndarray, origin_c: int, origin: int) -> tuple[np.ndarray, np.ndarray]:
    start = (origin_c - origin) % periodic.size
    indices = (start + np.arange(SUPPORT)) % periodic.size
    return periodic[indices].copy(), indices


def _evaluate_candidate(
    index: int,
    label: str,
    free: np.ndarray,
    structure: StructuralState,
    thresholds: dict[str, float],
    mid_limit: float,
    step_limit: float,
) -> dict[str, Any]:
    state = structure.state
    raw_target, _, coordinate_metrics = _structural_spectrum(state, free)
    periodic = np.fft.irfft(raw_target, n=PROXY_FFT_LEN)
    frequency = state["frequency"]
    band_3_14 = np.fft.irfft(raw_target * ((frequency >= 3000.0) & (frequency <= 14_000.0)), n=PROXY_FFT_LEN)
    band_14_20 = np.fft.irfft(raw_target * ((frequency >= 14_000.0) & (frequency <= 20_000.0)), n=PROXY_FFT_LEN)
    periodic_energy = float(np.dot(periodic, periodic))
    origin_trials = []
    for origin in range(state["source_origin"] - 12, state["source_origin"] + 13, 2):
        impulse, indices = _support_slice(periodic, state["origin_c"], origin)
        omitted = max(periodic_energy - float(np.dot(impulse, impulse)), 0.0) / max(periodic_energy, 1.0e-300)
        proxy = _proxy_score(impulse, band_3_14[indices], band_14_20[indices], omitted)
        origin_trials.append((min(proxy[0] / thresholds["band_3_14"], proxy[2] / thresholds["dominant"]), proxy[3], origin))
    origin_trials.sort()
    exact_origins = {state["source_origin"], origin_trials[0][2], origin_trials[1][2]}
    evaluated = []
    omega = np.linspace(0.0, np.pi, raw_target.size)
    for origin in sorted(exact_origins):
        impulse, indices = _support_slice(periodic, state["origin_c"], origin)
        omitted = max(periodic_energy - float(np.dot(impulse, impulse)), 0.0) / max(periodic_energy, 1.0e-300)
        proxy = _proxy_score(impulse, band_3_14[indices], band_14_20[indices], omitted)
        aligned = raw_target * np.exp(1j * omega * (state["origin_c"] - origin))
        aligned[0] = aligned[0].real
        aligned[-1] = aligned[-1].real
        exact = score_candidate(impulse, aligned, PROXY_FFT_LEN)
        audio_key, formal_key = _keys(
            proxy,
            exact,
            thresholds,
            coordinate_metrics["target_group_delay_curvature_max_abs_samples_per_ln_hz_squared"],
            state["curvature_cap"],
        )
        e2v5_key = [
            max(proxy[4] / step_limit - 1.0, 0.0),
            max(proxy[0] / mid_limit - 1.0, 0.0),
            proxy[2] / thresholds["dominant"],
            proxy[0],
            proxy[4],
            exact[4],
        ]
        evaluated.append(
            {
                "origin": origin,
                "proxy_score": list(proxy),
                "exact_score": list(exact),
                "audio_key": audio_key,
                "formal_key": formal_key,
                "e2v5_key": e2v5_key,
            }
        )
    return {
        "identity": IDENTITY,
        "index": index,
        "label": label,
        "structure_id": structure.identifier,
        "controls": structure.controls,
        "join_hz": structure.join_hz,
        "model_sha256": structure.model_sha256,
        "free": free.tolist(),
        "free_sha256": _sha256_array(free),
        "coordinate_metrics": coordinate_metrics,
        "best": min(evaluated, key=lambda item: tuple(item["e2v5_key"])),
        "evaluated_origins": evaluated,
    }


def _load_checkpoints(directory: Path) -> dict[int, dict[str, Any]]:
    loaded: dict[int, dict[str, Any]] = {}
    for path in sorted(directory.glob("candidate_*.json")):
        report = json.loads(path.read_text())
        free = np.asarray(report.get("free", []), dtype=np.float64)
        if report.get("identity") != IDENTITY or report.get("free_sha256") != _sha256_array(free):
            raise RuntimeError(f"corrupt or foreign E2v5 checkpoint: {path}")
        index = int(report["index"])
        if index in loaded:
            raise RuntimeError(f"duplicate E2v5 checkpoint index {index}")
        loaded[index] = report
    return loaded


def _evaluate_or_resume(
    index: int,
    label: str,
    free: np.ndarray,
    structure: StructuralState,
    existing: dict[int, dict[str, Any]],
    checkpoint_dir: Path,
    thresholds: dict[str, float],
    mid_limit: float,
    step_limit: float,
) -> dict[str, Any]:
    if index in existing:
        report = existing[index]
        if (
            report["free_sha256"] != _sha256_array(free)
            or report.get("model_sha256") != structure.model_sha256
            or report.get("label") != label
        ):
            raise RuntimeError(f"E2v5 checkpoint {index} does not match deterministic replay")
        return report
    report = _evaluate_candidate(index, label, free, structure, thresholds, mid_limit, step_limit)
    _atomic_json(checkpoint_dir / f"candidate_{index:05d}.json", report)
    existing[index] = report
    return report


def _save_structure(directory: Path, structure: StructuralState, free: np.ndarray) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / "structural_spline.npz"
    descriptor, temporary = tempfile.mkstemp(prefix="." + path.name + "-", suffix=".tmp", dir=directory)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            np.savez(
                handle,
                degree=structure.state["model"].degree,
                knots=structure.state["model"].knots,
                particular=structure.state["model"].particular,
                nullspace=structure.state["model"].nullspace,
                free=free,
                join_hz=structure.join_hz,
            )
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def _candidate_target(
    structure: StructuralState, free: np.ndarray, origin: int
) -> tuple[np.ndarray, dict[str, float]]:
    raw, _, metrics = _structural_spectrum(structure.state, free)
    omega = np.linspace(0.0, np.pi, raw.size)
    target = raw * np.exp(1j * omega * (structure.state["origin_c"] - origin))
    target[0] = target[0].real
    target[-1] = target[-1].real
    return target, metrics


def _refine_finalists(
    root: Path,
    base_e_dir: Path,
    e2v3_dir: Path,
    work_dir: Path,
    finalists: list[dict[str, Any]],
    structures: dict[str, StructuralState],
) -> list[dict[str, Any]]:
    initial = np.load(e2v3_dir / "character.npy")
    results = []
    for record in finalists:
        structure = structures[record["structure_id"]]
        selection = record["best"]
        free = np.asarray(record["free"], dtype=np.float64)
        origin = int(selection["origin"])
        target, coordinate_metrics = _candidate_target(structure, free, origin)
        candidate_dir = work_dir / "finalists" / f"candidate_{int(record['index']):05d}"
        low_dir = candidate_dir / "lawson_1m"
        _atomic_npy(low_dir / "target_spectrum.npy", target)
        _atomic_json(low_dir / "alignment.json", {"full_rate_origin": origin})
        low_character, low_history = _resume_lawson(low_dir, initial, target, PROXY_FFT_LEN, 4, 5.0e-5)
        low_audit = _full_audit(root, base_e_dir, low_character, target, origin)
        _atomic_json(low_dir / "full_audit.json", low_audit)

        high_dir = candidate_dir / "lawson_4m"
        high_fft_len = 1 << 22
        high_omega = np.linspace(0.0, np.pi, high_fft_len // 2 + 1)
        high_target = _resample_target(target, high_omega, origin)
        _atomic_npy(high_dir / "target_spectrum.npy", target)
        _atomic_json(high_dir / "alignment.json", {"full_rate_origin": origin})
        high_character, high_history = _resume_lawson(
            high_dir, low_character, high_target, high_fft_len, 4, 5.0e-5
        )
        high_audit = _full_audit(root, base_e_dir, high_character, target, origin)
        _atomic_json(high_dir / "full_audit.json", high_audit)
        _save_structure(high_dir, structure, free)
        result = {
            "identity": IDENTITY,
            "mode": "structural_high_resolution",
            "proxy_candidate_index": int(record["index"]),
            "proxy_label": record["label"],
            "structure_id": structure.identifier,
            "origin": origin,
            "coordinate_metrics": coordinate_metrics,
            "lawson_1m_iterations": len(low_history),
            "lawson_4m_iterations": len(high_history),
            "comparison": high_audit,
            "directory": str(high_dir),
            "screening_reuses_e_cleanup_and_rational_assets": True,
            "accepted": bool(high_audit["accepted"]),
            "production_promoted": False,
        }
        _atomic_json(candidate_dir / "result.json", result)
        results.append(result)
    return results


def build(
    root: Path,
    base_e_dir: Path,
    center_work_dir: Path,
    e2v3_dir: Path,
    work_dir: Path,
) -> dict[str, Any]:
    e2v3 = json.loads((e2v3_dir / "e2v3_report.json").read_text())
    if not e2v3.get("accepted"):
        raise RuntimeError("E2v5 requires accepted E2v3 provenance")
    thresholds = _load_thresholds(root)
    center_record = json.loads(
        (center_work_dir / "proxy_checkpoints/candidate_02064.json").read_text()
    )
    mid_limit = float(center_record["audio_best"]["proxy_score"][0]) * 1.01
    e2v3_metrics = e2v3["full_pipeline_result"]["comparison"]["d_metrics"]
    step_limit = float(e2v3_metrics["step_response_overshoot"]) * 1.01
    structures = _prepare_structures(root, base_e_dir, center_work_dir)
    structure_by_id = {structure.identifier: structure for structure in structures}
    provenance = {
        "base_magnitude": _sha256_file(base_e_dir / "magnitude_order_512.npz"),
        "base_factor": _sha256_file(base_e_dir / "spectral_factor_coefficients.npy"),
        "center_candidate": _sha256_file(center_work_dir / "proxy_checkpoints/candidate_02064.json"),
        "e2v3_report": _sha256_file(e2v3_dir / "e2v3_report.json"),
        "e2v3_character": _sha256_file(e2v3_dir / "character.npy"),
    }
    manifest = {
        "identity": IDENTITY,
        "structures": [
            {
                "identifier": item.identifier,
                "controls": item.controls,
                "free_coordinates": item.center.size,
                "join_hz": item.join_hz,
                "model_sha256": item.model_sha256,
                "center_projection_max_abs_samples": item.projection_max_abs_error,
            }
            for item in structures
        ],
        "candidates_per_structure": CANDIDATES_PER_STRUCTURE,
        "coordinate_radius": COORDINATE_RADIUS,
        "sobol_radius": SOBOL_RADIUS,
        "provenance": provenance,
        "refinement_gate": "dominant threshold plus midband and overshoot hard guards",
        "production_promoted": False,
    }
    work_dir.mkdir(parents=True, exist_ok=True)
    manifest_path = work_dir / "manifest.json"
    if manifest_path.exists() and json.loads(manifest_path.read_text()) != manifest:
        raise RuntimeError("E2v5 work directory belongs to a different invocation")
    _atomic_json(manifest_path, manifest)
    checkpoint_dir = work_dir / "proxy_checkpoints"
    existing = _load_checkpoints(checkpoint_dir)
    records: list[dict[str, Any]] = []
    index = 5000
    for structure in structures:
        incumbent_free = structure.center.copy()
        incumbent = _evaluate_or_resume(
            index,
            f"{structure.identifier}_projected_center",
            incumbent_free,
            structure,
            existing,
            checkpoint_dir,
            thresholds,
            mid_limit,
            step_limit,
        )
        records.append(incumbent)
        index += 1
        for coordinate in range(incumbent_free.size):
            base = incumbent_free.copy()
            pair = []
            for sign, scale in (("plus", 1.0), ("minus", -1.0)):
                candidate = base.copy()
                candidate[coordinate] += scale * COORDINATE_RADIUS
                report = _evaluate_or_resume(
                    index,
                    f"{structure.identifier}_coordinate_{coordinate:02d}_{sign}",
                    candidate,
                    structure,
                    existing,
                    checkpoint_dir,
                    thresholds,
                    mid_limit,
                    step_limit,
                )
                records.append(report)
                pair.append((report, candidate))
                index += 1
            incumbent, incumbent_free = min(
                [(incumbent, incumbent_free), *pair],
                key=lambda item: tuple(item[0]["best"]["e2v5_key"]),
            )
            incumbent_free = incumbent_free.copy()
        remaining = CANDIDATES_PER_STRUCTURE - (1 + 2 * structure.center.size)
        if remaining < 0:
            raise RuntimeError("E2v5 per-structure budget is smaller than coordinate sweep")
        if remaining:
            size = 1 << math.ceil(math.log2(remaining + 1))
            probes = qmc.Sobol(d=structure.center.size, scramble=False).random_base2(
                int(math.log2(size))
            )[1 : remaining + 1]
            for probe_index, probe in enumerate(probes):
                candidate = incumbent_free + (2.0 * probe - 1.0) * SOBOL_RADIUS
                report = _evaluate_or_resume(
                    index,
                    f"{structure.identifier}_sobol_{probe_index:02d}",
                    candidate,
                    structure,
                    existing,
                    checkpoint_dir,
                    thresholds,
                    mid_limit,
                    step_limit,
                )
                records.append(report)
                index += 1

    qualifying = [
        record
        for record in records
        if record["best"]["e2v5_key"][0] == 0.0
        and record["best"]["e2v5_key"][1] == 0.0
        and record["best"]["proxy_score"][2] <= thresholds["dominant"]
    ]
    qualifying.sort(key=lambda item: tuple(item["best"]["e2v5_key"]))
    finalists = qualifying[:2]
    proxy_summary = {
        "identity": IDENTITY,
        "phase": "proxy_complete",
        "budget": CANDIDATES_PER_STRUCTURE * len(structures),
        "completed": len(records),
        "thresholds": thresholds,
        "guards": {"mid_3_14_maximum": mid_limit, "step_overshoot_maximum": step_limit},
        "best": min(records, key=lambda item: tuple(item["best"]["e2v5_key"])),
        "per_structure_best": {
            structure.identifier: min(
                (record for record in records if record["structure_id"] == structure.identifier),
                key=lambda item: tuple(item["best"]["e2v5_key"]),
            )
            for structure in structures
        },
        "qualifying_count": len(qualifying),
        "qualifying_finalists": finalists,
        "checkpoint_semantics": "immutable JSON; free-coordinate and structural-model SHA-256 verified on deterministic replay",
    }
    _atomic_json(work_dir / "e2v5_proxy_report.json", proxy_summary)
    screening_results = _refine_finalists(
        root, base_e_dir, e2v3_dir, work_dir, finalists, structure_by_id
    )
    accepted = [result for result in screening_results if result["comparison"]["accepted"]]
    screening_winner = min(accepted, key=_result_key) if accepted else None
    full_result = (
        finalize_screening_winner(root, base_e_dir, work_dir, screening_winner)
        if screening_winner is not None
        else None
    )
    report = {
        "identity": IDENTITY,
        "proxy": proxy_summary,
        "screening_results": screening_results,
        "screening_winner": screening_winner,
        "full_pipeline_result": full_result,
        "accepted": bool(full_result is not None and full_result["accepted"]),
        "production_promoted": False,
        "resume_semantics": "immutable structural proxy checkpoints and hash-verified Lawson checkpoints",
    }
    _atomic_json(work_dir / "e2v5_report.json", report)
    _atomic_json(
        work_dir / "resume.json",
        {"identity": IDENTITY, "phase": "complete", "accepted": report["accepted"]},
    )
    return report


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--base-e-dir", type=Path)
    parser.add_argument("--center-work-dir", type=Path)
    parser.add_argument("--e2v3-dir", type=Path)
    parser.add_argument("--work-dir", type=Path)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    base_e_dir = (arguments.base_e_dir or root / "tools/split_phase_v5/work-spe-direct-factor").resolve()
    center_work_dir = (
        arguments.center_work_dir or root / "tools/split_phase_v5/work-spe-e2-audio-local-20260719"
    ).resolve()
    e2v3_dir = (
        arguments.e2v3_dir or root / "tools/split_phase_v5/work-spe-e2v3-audio-highres-20260719"
    ).resolve()
    work_dir = (
        arguments.work_dir or root / "tools/split_phase_v5/work-spe-e2v5-structural-20260719"
    ).resolve()
    print(json.dumps(build(root, base_e_dir, center_work_dir, e2v3_dir, work_dir), indent=2))


if __name__ == "__main__":
    main()
