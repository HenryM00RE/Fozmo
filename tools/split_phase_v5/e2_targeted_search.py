from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import tempfile
from pathlib import Path
from typing import Any, Iterable

import numpy as np

from tools.split_phase_v4.baseline import DESIGN_FFT_LEN, _load_c, _metrics
from tools.split_phase_v4.certify import _ratio_metrics, _resample_target
from tools.split_phase_v4.character_minimax import matrix_free_lawson
from tools.split_phase_v4.cleanup_socp import optimize_all, project_cleanup_equalities
from tools.split_phase_v4.compare_abcd import compare
from tools.split_phase_v4.group_delay_spline import ConstrainedDelaySpline
from tools.split_phase_v4.magnitude_sdp import evaluate_power
from tools.split_phase_v4.rational_minimax import optimize_both
from tools.split_phase_v4.report import _spectrum_for_coordinates
from tools.split_phase_v4.support_search import _proxy_score, score_candidate


IDENTITY = "SplitPhase128kV5-E2-targeted-experimental"
SUPPORT = 262_145
PROXY_FFT_LEN = 1_048_576


def _jsonable(value: Any) -> Any:
    if isinstance(value, np.generic):
        return value.item()
    if isinstance(value, np.ndarray):
        return value.tolist()
    if isinstance(value, dict):
        return {str(key): _jsonable(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_jsonable(item) for item in value]
    return value


def _atomic_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix="." + path.name + "-", suffix=".tmp", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            json.dump(_jsonable(payload), handle, indent=2)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def _atomic_npy(path: Path, values: np.ndarray) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix="." + path.name + "-", suffix=".tmp", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            np.save(handle, np.asarray(values))
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def _sha256_array(values: np.ndarray) -> str:
    array = np.ascontiguousarray(values)
    digest = hashlib.sha256()
    digest.update(array.dtype.str.encode("ascii"))
    digest.update(json.dumps(list(array.shape), separators=(",", ":")).encode("ascii"))
    digest.update(memoryview(array).cast("B"))
    return digest.hexdigest()


def _linear_from_db(value: float) -> float:
    return 10.0 ** (value / 10.0)


def _load_thresholds(root: Path) -> dict[str, float]:
    metrics = json.loads((root / "tools/split_phase_v4/baselines/split_c.json").read_text())["metrics"]
    return {
        "dominant": 0.85 * _linear_from_db(metrics["pre_energy_before_dominant_peak_db"]),
        "band_3_14": 0.85 * _linear_from_db(metrics["bandlimited_3_14khz_pre_peak_energy_db"]),
        "band_14_20": 0.85 * _linear_from_db(metrics["bandlimited_14_20khz_pre_peak_energy_db"]),
        "complex": 0.85 * float(metrics["worst_complex_passband_error"]),
    }


def _load_state(root: Path, source_dir: Path, fft_len: int) -> dict[str, Any]:
    with np.load(source_dir / "group_delay_spline.npz") as saved:
        model = ConstrainedDelaySpline(
            degree=int(saved["degree"]),
            knots=np.asarray(saved["knots"], dtype=np.float64),
            particular=np.asarray(saved["particular"], dtype=np.float64),
            nullspace=np.asarray(saved["nullspace"], dtype=np.float64),
            constraint_residual=float(
                json.loads((source_dir / "group_delay_spline.json").read_text())["constraint_residual"]
            ),
        )
        baseline_free = np.asarray(saved["free"], dtype=np.float64)
    factor = np.load(source_dir / "spectral_factor_coefficients.npy")
    minimum_response = np.fft.rfft(factor, n=fft_len)
    weighted = np.fft.rfft(np.arange(factor.size, dtype=np.float64) * factor, n=fft_len)
    minimum_delay = np.real(weighted / minimum_response)
    minimum_phase = np.unwrap(np.angle(minimum_response))
    frequency = np.linspace(0.0, 44_100.0, minimum_response.size)
    with np.load(source_dir / "magnitude_order_512.npz") as magnitude_data:
        autocorrelation = np.asarray(magnitude_data["autocorrelation"], dtype=np.float64)
    magnitude = np.sqrt(np.maximum(evaluate_power(autocorrelation, fft_len), 0.0))
    support_report = json.loads((source_dir / "support_search.json").read_text())
    spline_report = json.loads((source_dir / "group_delay_spline.json").read_text())
    source_alignment = json.loads((source_dir / "alignment.json").read_text())
    return {
        "model": model,
        "baseline_free": baseline_free,
        "frequency": frequency,
        "minimum_delay": minimum_delay,
        "minimum_phase": minimum_phase,
        "magnitude": magnitude,
        "origin_c": int(support_report["c_origin"]),
        "source_origin": int(source_alignment["full_rate_origin"]),
        "curvature_cap": 1.01
        * float(spline_report["target_group_delay_curvature_max_abs_samples_per_ln_hz_squared"]),
        "source_character": np.load(source_dir / "character_optimized.npy"),
        "source_target": np.load(source_dir / "target_spectrum.npy"),
    }


def _candidate_target(state: dict[str, Any], free: np.ndarray, origin: int) -> tuple[np.ndarray, dict[str, float]]:
    raw_target, _, coordinate_metrics = _spectrum_for_coordinates(
        state["model"],
        free,
        state["frequency"],
        state["minimum_delay"],
        state["minimum_phase"],
        state["magnitude"],
        state["origin_c"],
    )
    omega = np.linspace(0.0, np.pi, raw_target.size)
    target = raw_target * np.exp(1j * omega * (state["origin_c"] - origin))
    target[0] = target[0].real
    target[-1] = target[-1].real
    return target, coordinate_metrics


def _support_slice(periodic: np.ndarray, origin_c: int, origin: int) -> tuple[np.ndarray, np.ndarray]:
    start = (origin_c - origin) % periodic.size
    indices = (start + np.arange(SUPPORT)) % periodic.size
    return periodic[indices].copy(), indices


def _keys(
    proxy: tuple[float, ...],
    exact: tuple[float, ...],
    thresholds: dict[str, float],
    curvature: float,
    curvature_cap: float,
) -> tuple[list[float], list[float]]:
    audible_ratios = (
        proxy[0] / thresholds["band_3_14"],
        proxy[1] / thresholds["band_14_20"],
        proxy[2] / thresholds["dominant"],
    )
    # The constrained delay spline joins minimum phase at 14 kHz, so the
    # 14-20 kHz energy is intentionally reported but is not searchable here.
    controllable_audio_ratio = min(audible_ratios[0], audible_ratios[2])
    curvature_hard = max(curvature / curvature_cap - 1.0, 0.0)
    audio = [
        curvature_hard,
        controllable_audio_ratio,
        exact[0],
        audible_ratios[1],
        audible_ratios[2],
        audible_ratios[0],
        exact[4],
        proxy[3],
        proxy[4],
    ]
    formal = [curvature_hard, exact[4] / thresholds["complex"], exact[0], controllable_audio_ratio]
    return audio, formal


def _evaluate_proxy_candidate(
    index: int,
    label: str,
    free: np.ndarray,
    state: dict[str, Any],
    thresholds: dict[str, float],
) -> dict[str, Any]:
    raw_target, _, coordinate_metrics = _spectrum_for_coordinates(
        state["model"],
        free,
        state["frequency"],
        state["minimum_delay"],
        state["minimum_phase"],
        state["magnitude"],
        state["origin_c"],
    )
    periodic = np.fft.irfft(raw_target, n=PROXY_FFT_LEN)
    frequency = np.linspace(0.0, 44_100.0, raw_target.size)
    band_3_14 = np.fft.irfft(raw_target * ((frequency >= 3000.0) & (frequency <= 14_000.0)), n=PROXY_FFT_LEN)
    band_14_20 = np.fft.irfft(raw_target * ((frequency >= 14_000.0) & (frequency <= 20_000.0)), n=PROXY_FFT_LEN)
    periodic_energy = float(np.dot(periodic, periodic))
    origin_trials = []
    for origin in range(state["source_origin"] - 12, state["source_origin"] + 13, 2):
        impulse, indices = _support_slice(periodic, state["origin_c"], origin)
        omitted = max(periodic_energy - float(np.dot(impulse, impulse)), 0.0) / max(periodic_energy, 1.0e-300)
        proxy = _proxy_score(impulse, band_3_14[indices], band_14_20[indices], omitted)
        ratios = (
            proxy[0] / thresholds["band_3_14"],
            proxy[1] / thresholds["band_14_20"],
            proxy[2] / thresholds["dominant"],
        )
        origin_trials.append((min(ratios[0], ratios[2]), proxy[3], origin, impulse, proxy))
    origin_trials.sort(key=lambda item: (item[0], item[1]))
    exact_origins = {state["source_origin"], origin_trials[0][2], origin_trials[1][2]}
    evaluated = []
    omega = np.linspace(0.0, np.pi, raw_target.size)
    for origin in sorted(exact_origins):
        impulse, indices = _support_slice(periodic, state["origin_c"], origin)
        proxy = _proxy_score(
            impulse,
            band_3_14[indices],
            band_14_20[indices],
            max(periodic_energy - float(np.dot(impulse, impulse)), 0.0) / max(periodic_energy, 1.0e-300),
        )
        aligned_target = raw_target * np.exp(1j * omega * (state["origin_c"] - origin))
        aligned_target[0] = aligned_target[0].real
        aligned_target[-1] = aligned_target[-1].real
        exact = score_candidate(impulse, aligned_target, PROXY_FFT_LEN)
        audio_key, formal_key = _keys(
            proxy,
            exact,
            thresholds,
            coordinate_metrics["target_group_delay_curvature_max_abs_samples_per_ln_hz_squared"],
            state["curvature_cap"],
        )
        evaluated.append(
            {
                "origin": origin,
                "proxy_score": list(proxy),
                "exact_score": list(exact),
                "audio_key": audio_key,
                "formal_key": formal_key,
            }
        )
    return {
        "identity": IDENTITY,
        "index": index,
        "label": label,
        "free": free.tolist(),
        "free_sha256": _sha256_array(free),
        "coordinate_metrics": coordinate_metrics,
        "audio_best": min(evaluated, key=lambda item: tuple(item["audio_key"])),
        "formal_best": min(evaluated, key=lambda item: tuple(item["formal_key"])),
        "evaluated_origins": evaluated,
    }


def _load_proxy_checkpoints(directory: Path) -> dict[int, dict[str, Any]]:
    loaded = {}
    for path in sorted(directory.glob("candidate_*.json")):
        report = json.loads(path.read_text())
        if report.get("identity") != IDENTITY:
            raise RuntimeError(f"foreign E2 checkpoint: {path}")
        free = np.asarray(report["free"], dtype=np.float64)
        if report.get("free_sha256") != _sha256_array(free):
            raise RuntimeError(f"corrupt E2 checkpoint: {path}")
        loaded[int(report["index"])] = report
    return loaded


def _initial_specs(baseline: np.ndarray, epsilon: float) -> list[tuple[str, np.ndarray]]:
    specs = [("baseline", baseline.copy())]
    for coordinate in range(baseline.size):
        direction = np.zeros_like(baseline)
        direction[coordinate] = epsilon
        specs.append((f"coordinate_{coordinate:02d}_plus", baseline + direction))
        specs.append((f"coordinate_{coordinate:02d}_minus", baseline - direction))
    return specs


def _gradient(records: dict[int, dict[str, Any]], baseline: np.ndarray, epsilon: float, key: str) -> np.ndarray:
    gradient = np.zeros_like(baseline)
    for coordinate in range(baseline.size):
        plus = records[1 + 2 * coordinate][key]["audio_key" if key == "audio_best" else "formal_key"]
        minus = records[2 + 2 * coordinate][key]["audio_key" if key == "audio_best" else "formal_key"]
        component = 1
        gradient[coordinate] = (float(plus[component]) - float(minus[component])) / (2.0 * epsilon)
    norm = float(np.linalg.norm(gradient))
    return gradient / norm if norm > 0.0 else gradient


def _extended_specs(
    baseline: np.ndarray,
    records: dict[int, dict[str, Any]],
    epsilon: float,
) -> list[tuple[str, np.ndarray]]:
    audio_gradient = _gradient(records, baseline, epsilon, "audio_best")
    formal_gradient = _gradient(records, baseline, epsilon, "formal_best")
    specs = []
    for step in (0.01, 0.02, 0.04, 0.08, 0.12, 0.18, 0.26, 0.36, 0.5, 0.75, 1.0, 1.5, 2.5, 4.0):
        specs.append((f"audio_gradient_{step:g}", baseline - step * audio_gradient))
    for step in (0.01, 0.02, 0.04, 0.08, 0.16, 0.30, 0.6, 1.2, 2.4, 4.0):
        specs.append((f"formal_gradient_{step:g}", baseline - step * formal_gradient))
    mixed = audio_gradient + formal_gradient
    mixed /= max(float(np.linalg.norm(mixed)), 1.0e-300)
    for step in (0.02, 0.05, 0.10, 0.20, 0.5, 1.0, 2.0, 4.0):
        specs.append((f"mixed_gradient_{step:g}", baseline - step * mixed))
    # Revisit the strongest one-coordinate sensitivities at a wider radius.
    sensitivities = []
    for coordinate in range(baseline.size):
        plus = records[1 + 2 * coordinate]["audio_best"]["audio_key"][1]
        minus = records[2 + 2 * coordinate]["audio_best"]["audio_key"][1]
        sensitivities.append((abs(float(plus) - float(minus)), coordinate, -1.0 if plus < minus else 1.0))
    strongest = sorted(sensitivities, reverse=True)[:9]
    for radius, prefix in ((0.1, "wide"), (0.5, "aggressive"), (1.5, "far")):
        for _, coordinate, sign in strongest:
            direction = np.zeros_like(baseline)
            direction[coordinate] = sign * radius
            specs.append((f"{prefix}_coordinate_{coordinate:02d}", baseline + direction))
    return specs


def run_proxy_search(
    root: Path,
    source_dir: Path,
    work_dir: Path,
    budget: int,
    epsilon: float,
) -> tuple[dict[str, Any], dict[str, Any], dict[int, dict[str, Any]]]:
    state = _load_state(root, source_dir, PROXY_FFT_LEN)
    thresholds = _load_thresholds(root)
    checkpoint_dir = work_dir / "proxy_checkpoints"
    records = _load_proxy_checkpoints(checkpoint_dir)
    specs = _initial_specs(state["baseline_free"], epsilon)
    initial_count = len(specs)
    for index, (label, free) in enumerate(specs[:budget]):
        if index not in records:
            report = _evaluate_proxy_candidate(index, label, free, state, thresholds)
            _atomic_json(checkpoint_dir / f"candidate_{index:05d}.json", report)
            records[index] = report
            _atomic_json(work_dir / "resume.json", {"identity": IDENTITY, "phase": "proxy", "latest_candidate": index})
    if budget > initial_count:
        if any(index not in records for index in range(initial_count)):
            raise RuntimeError("cannot derive E2 gradients until the coordinate checkpoints are complete")
        specs.extend(_extended_specs(state["baseline_free"], records, epsilon))
        for index, (label, free) in enumerate(specs[initial_count:budget], start=initial_count):
            if index not in records:
                report = _evaluate_proxy_candidate(index, label, free, state, thresholds)
                _atomic_json(checkpoint_dir / f"candidate_{index:05d}.json", report)
                records[index] = report
                _atomic_json(work_dir / "resume.json", {"identity": IDENTITY, "phase": "proxy", "latest_candidate": index})
    completed = {index: records[index] for index in range(min(budget, len(specs))) if index in records}
    summary = {
        "identity": IDENTITY,
        "phase": "proxy_complete",
        "budget": budget,
        "completed": len(completed),
        "checkpoint_validation": "identity and dtype/shape/value SHA-256 verified on resume",
        "audio_best": min(completed.values(), key=lambda item: tuple(item["audio_best"]["audio_key"])),
        "formal_best": min(completed.values(), key=lambda item: tuple(item["formal_best"]["formal_key"])),
        "thresholds": thresholds,
    }
    _atomic_json(work_dir / "proxy_summary.json", summary)
    return state, thresholds, completed


def _load_cleanups(source_dir: Path, root: Path) -> tuple[list[Any], list[np.ndarray]]:
    _, templates, _, _ = _load_c(root)
    with np.load(source_dir / "cleanup_optimized.npz") as saved:
        values = [np.asarray(saved[f"stage_{index}"], dtype=np.float64) for index in range(1, 8)]
    return templates, values


def _full_audit(
    root: Path,
    source_dir: Path,
    character: np.ndarray,
    target: np.ndarray,
    origin: int,
    rational_report: dict[str, Any] | None = None,
    cleanup_values: list[np.ndarray] | None = None,
) -> dict[str, Any]:
    templates, source_cleanups = _load_cleanups(source_dir, root)
    values = source_cleanups if cleanup_values is None else cleanup_values
    metric_omega = np.linspace(0.0, np.pi, DESIGN_FFT_LEN // 2 + 1)
    metric_target = _resample_target(target, metric_omega, origin)
    metrics = _metrics(character, [type(templates[0])(value) for value in values], origin, metric_target)
    if rational_report is None:
        rational_report = json.loads((source_dir / "rational_minimax.json").read_text())
    comparison = compare(root, metrics, character, rational_report)
    comparison["d_metrics"] = metrics
    return comparison


def _resume_lawson(
    directory: Path,
    initial: np.ndarray,
    target: np.ndarray,
    fft_len: int,
    iterations: int,
    trust_radius: float,
) -> tuple[np.ndarray, list[dict[str, Any]]]:
    directory.mkdir(parents=True, exist_ok=True)
    character_path = directory / "character.npy"
    state_path = directory / "lawson_resume.json"
    history: list[dict[str, Any]] = []
    character = np.asarray(initial, dtype=np.float64)
    completed = 0
    if state_path.exists() or character_path.exists():
        if not state_path.exists() or not character_path.exists():
            raise RuntimeError(f"incomplete Lawson checkpoint in {directory}")
        state = json.loads(state_path.read_text())
        character = np.load(character_path)
        if state.get("identity") != IDENTITY or state.get("character_sha256") != _sha256_array(character):
            raise RuntimeError(f"corrupt Lawson checkpoint in {directory}")
        if int(state.get("fft_len", -1)) != fft_len:
            raise RuntimeError(f"Lawson checkpoint FFT mismatch in {directory}")
        completed = int(state["completed_iterations"])
        history = list(state.get("history", []))
    elif iterations == 0:
        _atomic_npy(character_path, character)
        _atomic_json(
            state_path,
            {
                "identity": IDENTITY,
                "fft_len": fft_len,
                "completed_iterations": 0,
                "character_sha256": _sha256_array(character),
                "history": history,
            },
        )
    for iteration in range(completed, iterations):
        character, report = matrix_free_lawson(character, target, fft_len, iterations=1, trust_radius=trust_radius)
        history.append(report["iterations"][0])
        _atomic_npy(character_path, character)
        _atomic_json(
            state_path,
            {
                "identity": IDENTITY,
                "fft_len": fft_len,
                "completed_iterations": iteration + 1,
                "character_sha256": _sha256_array(character),
                "history": history,
            },
        )
    return character, history


def _select_finalists(records: Iterable[dict[str, Any]], count: int) -> list[tuple[str, dict[str, Any], dict[str, Any]]]:
    values = list(records)
    selected: list[tuple[str, dict[str, Any], dict[str, Any]]] = []
    seen = set()
    for mode, field, key in (
        ("audio", "audio_best", "audio_key"),
        ("formal_proxy", "formal_best", "formal_key"),
    ):
        for record in sorted(values, key=lambda item: tuple(item[field][key]))[:count]:
            identifier = (mode, int(record["index"]))
            if identifier not in seen:
                selected.append((mode, record, record[field]))
                seen.add(identifier)
    return selected


def run_finalists(
    root: Path,
    source_dir: Path,
    work_dir: Path,
    state: dict[str, Any],
    records: dict[int, dict[str, Any]],
    count: int,
    lawson_iterations: int,
) -> list[dict[str, Any]]:
    results = []
    for mode, record, selection in _select_finalists(records.values(), count):
        candidate_dir = work_dir / "finalists" / f"{mode}_{int(record['index']):05d}"
        free = np.asarray(record["free"], dtype=np.float64)
        origin = int(selection["origin"])
        target, coordinate_metrics = _candidate_target(state, free, origin)
        _atomic_npy(candidate_dir / "target_spectrum.npy", target)
        _atomic_json(candidate_dir / "alignment.json", {"full_rate_origin": origin})
        character, history = _resume_lawson(
            candidate_dir,
            state["source_character"],
            target,
            PROXY_FFT_LEN,
            lawson_iterations,
            2.0e-4,
        )
        audit_path = candidate_dir / "full_audit.json"
        if audit_path.exists():
            audit = json.loads(audit_path.read_text())
        else:
            audit = _full_audit(root, source_dir, character, target, origin)
            _atomic_json(audit_path, audit)
        result = {
            "mode": mode,
            "proxy_candidate_index": int(record["index"]),
            "proxy_label": record["label"],
            "origin": origin,
            "coordinate_metrics": coordinate_metrics,
            "lawson_iterations": len(history),
            "screening_reuses_e_cleanup_and_rational_assets": True,
            "comparison": audit,
            "directory": str(candidate_dir),
        }
        _atomic_json(candidate_dir / "result.json", result)
        results.append(result)
    return results


def run_formal_high_resolution(
    root: Path,
    source_dir: Path,
    work_dir: Path,
    state: dict[str, Any],
    fft_len: int,
    iterations: int,
) -> dict[str, Any]:
    directory = work_dir / "formal_high_resolution"
    omega = np.linspace(0.0, np.pi, fft_len // 2 + 1)
    target = _resample_target(state["source_target"], omega, state["source_origin"])
    character, history = _resume_lawson(
        directory,
        state["source_character"],
        target,
        fft_len,
        iterations,
        5.0e-5,
    )
    audit_path = directory / "full_audit.json"
    if audit_path.exists():
        audit = json.loads(audit_path.read_text())
    else:
        audit = _full_audit(
            root,
            source_dir,
            character,
            state["source_target"],
            state["source_origin"],
        )
        _atomic_json(audit_path, audit)
    result = {
        "mode": "formal_high_resolution",
        "fft_len": fft_len,
        "lawson_iterations": len(history),
        "screening_reuses_e_cleanup_and_rational_assets": True,
        "comparison": audit,
        "directory": str(directory),
    }
    _atomic_json(directory / "result.json", result)
    return result


def _result_key(result: dict[str, Any]) -> tuple[Any, ...]:
    comparison = result["comparison"]
    improvements = comparison["improvements"]
    audible = min(
        improvements[name]["d"] / max(improvements[name]["c"] * 0.85, 1.0e-300)
        for name in (
            "dominant_peak_pre_energy",
            "bandlimited_3_14khz_pre_peak_energy",
        )
    )
    formal = improvements["worst_complex_passband_error"]["d"] / max(
        improvements["worst_complex_passband_error"]["c"] * 0.85, 1.0e-300
    )
    return (
        not bool(comparison["accepted"]),
        -int(comparison["pareto_improvement_count"]),
        audible,
        formal,
    )


def finalize_screening_winner(
    root: Path,
    source_dir: Path,
    work_dir: Path,
    winner: dict[str, Any],
) -> dict[str, Any] | None:
    if not winner["comparison"]["accepted"]:
        return None
    source = Path(winner["directory"])
    character = np.load(source / "character.npy")
    if winner["mode"] == "formal_high_resolution":
        target = np.load(source_dir / "target_spectrum.npy")
        origin = int(json.loads((source_dir / "alignment.json").read_text())["full_rate_origin"])
    else:
        target = np.load(source / "target_spectrum.npy")
        origin = int(json.loads((source / "alignment.json").read_text())["full_rate_origin"])
    directory = work_dir / "winner_full_pipeline"
    directory.mkdir(parents=True, exist_ok=True)
    _, cleanup_templates, _, _ = _load_c(root)
    cleanup_values = [stage.canonical for stage in cleanup_templates]
    proposed, cleanup_report = optimize_all(cleanup_values, directory)
    response = np.fft.rfft(character, n=PROXY_FFT_LEN)
    frequency = np.linspace(0.0, 44_100.0, response.size)
    omega = np.linspace(0.0, np.pi, response.size)
    target_response = _resample_target(target, omega, origin)

    def cleanup_key(values: list[np.ndarray]) -> tuple[tuple[float, ...], dict[str, Any]]:
        metrics = _ratio_metrics(response, target_response, frequency, omega, origin, values, PROXY_FFT_LEN)
        ripple = metrics["worst_2x_256x_passband_ripple_db"]
        complex_error = metrics["worst_2x_256x_composite_complex_error"]
        hard = max(ripple / 1.0e-7 - 1.0, complex_error / 8.0e-9 - 1.0, 0.0)
        return (hard, complex_error, ripple), metrics

    cleanups = [value.copy() for value in cleanup_values]
    incumbent_key, incumbent_metrics = cleanup_key(cleanups)
    for stage_index, proposed_value in enumerate(proposed):
        for alpha in (1.0, 0.5, 0.25, 0.125, 0.0625):
            blended = project_cleanup_equalities(cleanups[stage_index] + alpha * (proposed_value - cleanups[stage_index]))
            trial = list(cleanups)
            trial[stage_index] = blended
            trial_key, trial_metrics = cleanup_key(trial)
            if trial_key < incumbent_key:
                cleanups = trial
                incumbent_key = trial_key
                incumbent_metrics = trial_metrics
                break
    np.savez(directory / "cleanup_optimized.npz", **{f"stage_{i}": value for i, value in enumerate(cleanups, 1)})
    rational_147, rational_160, rational_report = optimize_both(
        root / "assets/filters/split_phase_v3", target, origin, directory
    )
    del rational_147, rational_160
    audit = _full_audit(root, source_dir, character, target, origin, rational_report, cleanups)
    result = {
        "identity": IDENTITY,
        "source_screening_result": winner,
        "cleanup_complete_cascade_score": list(incumbent_key),
        "cleanup_complete_cascade_metrics": incumbent_metrics,
        "rational": rational_report,
        "comparison": audit,
        "accepted": bool(audit["accepted"]),
        "production_promoted": False,
    }
    _atomic_npy(directory / "character.npy", character)
    _atomic_npy(directory / "target_spectrum.npy", target)
    _atomic_json(directory / "alignment.json", {"full_rate_origin": origin})
    _atomic_json(directory / "result.json", result)
    return result


def build(
    root: Path,
    source_dir: Path,
    work_dir: Path,
    proxy_budget: int,
    finalists: int,
    lawson_iterations: int,
    formal_fft_len: int,
    formal_iterations: int,
    proxy_only: bool,
) -> dict[str, Any]:
    work_dir.mkdir(parents=True, exist_ok=True)
    provenance = {
        name: _sha256_file(source_dir / name)
        for name in (
            "magnitude_order_512.npz",
            "spectral_factor_coefficients.npy",
            "spectral_factor.json",
            "group_delay_spline.npz",
            "character_optimized.npy",
            "cleanup_optimized.npz",
            "rational_minimax.json",
            "target_spectrum.npy",
        )
    }
    manifest_path = work_dir / "manifest.json"
    manifest = {
        "identity": IDENTITY,
        "source_dir": str(source_dir),
        "source_sha256": provenance,
        "proxy_budget": proxy_budget,
        "finalists_per_objective": finalists,
        "lawson_iterations": lawson_iterations,
        "formal_fft_len": formal_fft_len,
        "formal_iterations": formal_iterations,
    }
    if manifest_path.exists() and json.loads(manifest_path.read_text()) != manifest:
        raise RuntimeError("E2 work directory belongs to a different invocation")
    _atomic_json(manifest_path, manifest)
    state, thresholds, records = run_proxy_search(root, source_dir, work_dir, proxy_budget, 0.02)
    if proxy_only:
        summary = {"identity": IDENTITY, "phase": "proxy_only", "proxy_candidates": len(records)}
        _atomic_json(work_dir / "e2_report.json", summary)
        return summary
    results = run_finalists(root, source_dir, work_dir, state, records, finalists, lawson_iterations)
    results.append(
        run_formal_high_resolution(
            root, source_dir, work_dir, state, formal_fft_len, formal_iterations
        )
    )
    winner = min(results, key=_result_key)
    full_result = finalize_screening_winner(root, source_dir, work_dir, winner)
    summary = {
        "identity": IDENTITY,
        "thresholds": thresholds,
        "source_provenance": provenance,
        "proxy_candidates": len(records),
        "screening_results": results,
        "screening_winner": winner,
        "full_pipeline_result": full_result,
        "accepted": bool(full_result is not None and full_result["accepted"]),
        "production_promoted": False,
        "resume_semantics": "immutable proxy JSON plus hash-verified Lawson NPY/JSON checkpoints",
    }
    _atomic_json(work_dir / "e2_report.json", summary)
    _atomic_json(work_dir / "resume.json", {"identity": IDENTITY, "phase": "complete", "accepted": summary["accepted"]})
    return summary


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--source-dir", type=Path)
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--proxy-budget", type=int, default=96)
    parser.add_argument("--finalists", type=int, default=3)
    parser.add_argument("--lawson-iterations", type=int, default=8)
    parser.add_argument("--formal-fft-len", type=int, default=1 << 22)
    parser.add_argument("--formal-iterations", type=int, default=4)
    parser.add_argument("--proxy-only", action="store_true")
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    source_dir = (arguments.source_dir or root / "tools/split_phase_v5/work-spe-direct-factor").resolve()
    work_dir = (arguments.work_dir or root / "tools/split_phase_v5/work-spe-e2-targeted").resolve()
    if arguments.proxy_budget < 1 or arguments.proxy_budget > 96:
        parser.error("--proxy-budget must be between 1 and 96")
    if arguments.formal_fft_len < PROXY_FFT_LEN or arguments.formal_fft_len & (arguments.formal_fft_len - 1):
        parser.error("--formal-fft-len must be a power of two at least 1048576")
    report = build(
        root,
        source_dir,
        work_dir,
        arguments.proxy_budget,
        arguments.finalists,
        arguments.lawson_iterations,
        arguments.formal_fft_len,
        arguments.formal_iterations,
        arguments.proxy_only,
    )
    print(json.dumps(_jsonable(report), indent=2))


if __name__ == "__main__":
    main()
