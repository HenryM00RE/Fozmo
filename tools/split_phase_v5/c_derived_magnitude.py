from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import tempfile
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Iterable

import numpy as np
from scipy import signal

from tools.split_phase_v4.magnitude_sdp import (
    MagnitudeSpec,
    _dense_metrics,
    _exchange,
    _high_precision_check,
    _initial_grids,
    autocorrelation_from_gram,
    evaluate_power,
)
from tools.split_phase_v4.spectral_factor import (
    _high_precision_reconstruction,
    _homomorphic_factor,
)


EXPERIMENT_IDENTITY = "SplitPhase128kV5-experimental"
C_IDENTITY = "SplitPhase128kV3"


@dataclass(frozen=True)
class SeedSearchSpec:
    screening_fft_len: int = 1_048_576
    beta_start: float = 15.0
    beta_stop: float = 21.0
    beta_step: float = 0.25
    cutoff_start_hz: float = 20_750.0
    cutoff_stop_hz: float = 21_300.0
    cutoff_step_hz: float = 25.0
    positivity_floor_power: float = 1.0e-15
    maximum_gate_utilization: float = 0.1


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def _sha256_array(values: np.ndarray) -> str:
    return hashlib.sha256(np.asarray(values, dtype="<f8").tobytes(order="C")).hexdigest()


def _grid(start: float, stop: float, step: float) -> np.ndarray:
    if step <= 0.0 or stop < start:
        raise ValueError("invalid deterministic search grid")
    count = int(round((stop - start) / step))
    values = start + step * np.arange(count + 1, dtype=np.float64)
    return values[values <= stop + 0.5 * step]


def load_split_c_character(root: Path) -> tuple[np.ndarray, dict[str, Any]]:
    asset_dir = root / "assets" / "filters" / "split_phase_v3"
    manifest_path = asset_dir / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    if manifest.get("identity") != C_IDENTITY:
        raise RuntimeError("Split C manifest identity does not match")
    entry = manifest["files"]["character"]
    character_path = asset_dir / entry["file"]
    actual_hash = _sha256_file(character_path)
    if actual_hash != entry["sha256"]:
        raise RuntimeError("Split C character asset hash does not match its manifest")
    character = np.fromfile(character_path, dtype="<f8")
    if character.size != int(entry["coefficient_count"]):
        raise RuntimeError("Split C character coefficient count does not match")
    return character, {
        "identity": C_IDENTITY,
        "manifest_sha256": _sha256_file(manifest_path),
        "character_file": entry["file"],
        "character_sha256": actual_hash,
        "character_coefficients": int(character.size),
    }


def candidate_coefficients(spec: MagnitudeSpec, beta: float, cutoff_hz: float) -> np.ndarray:
    coefficients = signal.firwin(
        spec.order + 1,
        cutoff_hz,
        window=("kaiser", beta),
        fs=spec.sample_rate_hz,
        scale=True,
    ).astype(np.float64)
    coefficients /= math.fsum(float(value) for value in coefficients)
    return coefficients


def gram_with_positive_floor(
    coefficients: np.ndarray,
    positivity_floor_power: float,
) -> np.ndarray:
    values = np.asarray(coefficients, dtype=np.float64)
    if values.ndim != 1 or values.size < 2 or not np.all(np.isfinite(values)):
        raise ValueError("seed coefficients must be a finite vector")
    if positivity_floor_power <= 0.0:
        raise ValueError("positivity floor must be positive")
    dimension = values.size
    gram = np.outer(values, values)
    gram += (positivity_floor_power / dimension) * np.eye(dimension, dtype=np.float64)
    gram /= 1.0 + positivity_floor_power
    return gram


def minimum_phase_seed(
    linear_coefficients: np.ndarray,
    fft_len: int,
) -> tuple[np.ndarray, dict[str, Any]]:
    linear = np.asarray(linear_coefficients, dtype=np.float64)
    linear_gram = np.outer(linear, linear)
    linear_autocorrelation = autocorrelation_from_gram(linear_gram)
    minimum = _homomorphic_factor(linear_autocorrelation, fft_len)
    minimum /= math.fsum(float(value) for value in minimum)
    linear_power = np.abs(np.fft.rfft(linear, n=fft_len)) ** 2
    minimum_power = np.abs(np.fft.rfft(minimum, n=fft_len)) ** 2
    return minimum, {
        "method": "homomorphic finite-polynomial minimum-phase conversion",
        "fft_len": fft_len,
        "maximum_power_change": float(np.max(np.abs(minimum_power - linear_power))),
        "linear_coefficient_sha256": _sha256_array(linear),
        "minimum_phase_coefficient_sha256": _sha256_array(minimum),
    }


def _screen_metrics(
    coefficients: np.ndarray,
    c_response: np.ndarray,
    frequency: np.ndarray,
    spec: MagnitudeSpec,
    positivity_floor_power: float,
) -> dict[str, float]:
    response = np.fft.rfft(coefficients, n=(frequency.size - 1) * 2)
    power = (np.abs(response) ** 2 + positivity_floor_power) / (1.0 + positivity_floor_power)
    pass_mask = frequency <= spec.pass_edge_hz
    transition_mask = (frequency >= spec.pass_edge_hz) & (frequency <= spec.stop_edge_hz)
    stop_mask = frequency >= spec.stop_edge_hz
    comparison_mask = (frequency >= 20.0) & pass_mask
    pass_ripple = float(np.max(np.abs(np.sqrt(power[pass_mask]) - 1.0)))
    stop_peak = float(np.max(power[stop_mask]))
    transition = power[transition_mask]
    upward = float(np.max(np.maximum(np.diff(transition), 0.0))) if transition.size > 1 else 0.0
    c_db = 20.0 * np.log10(np.maximum(np.abs(c_response[comparison_mask]), 1.0e-300))
    candidate_db = 20.0 * np.log10(np.maximum(np.abs(response[comparison_mask]), 1.0e-300))
    return {
        "passband_amplitude_ripple": pass_ripple,
        "stopband_power_peak": stop_peak,
        "stopband_amplitude_db": float(10.0 * np.log10(max(stop_peak, 1.0e-300))),
        "transition_maximum_upward_power": upward,
        "maximum_c_e_magnitude_difference_db_20_20k": float(np.max(np.abs(c_db - candidate_db))),
    }


def _gate_utilization(metrics: dict[str, float], spec: MagnitudeSpec) -> float:
    stop_gate = 10.0 ** (spec.stopband_amplitude_db / 10.0)
    return max(
        metrics["passband_amplitude_ripple"] / spec.passband_amplitude_ripple,
        metrics["stopband_power_peak"] / stop_gate,
        metrics["transition_maximum_upward_power"] / 1.0e-11,
    )


def search_c_guided_seed(
    c_character: np.ndarray,
    spec: MagnitudeSpec,
    search: SeedSearchSpec,
    betas: Iterable[float] | None = None,
    cutoffs_hz: Iterable[float] | None = None,
) -> tuple[np.ndarray, dict[str, Any]]:
    if search.screening_fft_len < 2 * (spec.order + 1):
        raise ValueError("screening FFT is too short")
    frequency = np.linspace(0.0, spec.sample_rate_hz / 2.0, search.screening_fft_len // 2 + 1)
    c_response = np.fft.rfft(c_character, n=search.screening_fft_len)
    beta_values = np.asarray(
        list(betas) if betas is not None else _grid(search.beta_start, search.beta_stop, search.beta_step),
        dtype=np.float64,
    )
    cutoff_values = np.asarray(
        list(cutoffs_hz)
        if cutoffs_hz is not None
        else _grid(search.cutoff_start_hz, search.cutoff_stop_hz, search.cutoff_step_hz),
        dtype=np.float64,
    )
    feasible: list[tuple[tuple[float, float, float], np.ndarray, dict[str, Any]]] = []
    evaluated = 0
    for beta in beta_values:
        for cutoff_hz in cutoff_values:
            evaluated += 1
            coefficients = candidate_coefficients(spec, float(beta), float(cutoff_hz))
            metrics = _screen_metrics(
                coefficients,
                c_response,
                frequency,
                spec,
                search.positivity_floor_power,
            )
            utilization = _gate_utilization(metrics, spec)
            if utilization > search.maximum_gate_utilization:
                continue
            record = {
                "beta": float(beta),
                "cutoff_hz": float(cutoff_hz),
                "gate_utilization": utilization,
                **metrics,
            }
            key = (
                metrics["maximum_c_e_magnitude_difference_db_20_20k"],
                utilization,
                float(beta),
            )
            feasible.append((key, coefficients, record))
    if not feasible:
        raise RuntimeError("C-guided structured search found no candidate with a 10x gate margin")
    feasible.sort(key=lambda item: item[0])
    _, selected, selected_record = feasible[0]
    return selected, {
        "method": "deterministic Kaiser grid ranked by frozen Split C magnitude agreement",
        "evaluated_candidates": evaluated,
        "feasible_candidates": len(feasible),
        "selection_rule": "minimum 20 Hz-20 kHz C/E magnitude difference among candidates using at most 10% of each magnitude gate",
        "selected": selected_record,
        "top_candidates": [record for _, _, record in feasible[:20]],
    }


def _full_c_difference_db(
    c_character: np.ndarray,
    coefficients: np.ndarray,
    spec: MagnitudeSpec,
) -> float:
    c_response = np.fft.rfft(c_character, n=spec.verification_fft_len)
    e_response = np.fft.rfft(coefficients, n=spec.verification_fft_len)
    frequency = np.linspace(0.0, spec.sample_rate_hz / 2.0, e_response.size)
    mask = (frequency >= 20.0) & (frequency <= spec.pass_edge_hz)
    c_db = 20.0 * np.log10(np.maximum(np.abs(c_response[mask]), 1.0e-300))
    e_db = 20.0 * np.log10(np.maximum(np.abs(e_response[mask]), 1.0e-300))
    return float(np.max(np.abs(c_db - e_db)))


def _audit_arrays(
    root: Path,
    spec: MagnitudeSpec,
    coefficients: np.ndarray,
    gram: np.ndarray,
    autocorrelation: np.ndarray,
    positivity_floor_power: float,
) -> dict[str, Any]:
    c_character, c_provenance = load_split_c_character(root)
    expected_gram = gram_with_positive_floor(coefficients, positivity_floor_power)
    gram_residual = float(np.max(np.abs(gram - expected_gram)))
    autocorrelation_residual = float(
        np.max(np.abs(autocorrelation - autocorrelation_from_gram(gram)))
    )
    dense = _dense_metrics(autocorrelation, spec)
    high_precision = _high_precision_check(autocorrelation, spec)
    eigen_minimum = float(np.linalg.eigvalsh(0.5 * (gram + gram.T))[0])
    dc_residual = abs(float(np.sum(gram)) - 1.0)
    grids, added = _exchange(_initial_grids(spec), autocorrelation, spec)
    c_difference = _full_c_difference_db(c_character, coefficients, spec)
    accepted = bool(
        dense["passband_amplitude_ripple"] <= 1.01 * spec.passband_amplitude_ripple
        and dense["stopband_amplitude_db"] <= spec.stopband_amplitude_db + 0.05
        and dense["global_minimum_power"] >= -1.0e-12
        and dense["transition_maximum_upward_power"] <= 1.0e-11
        and high_precision["minimum_power"] >= -1.0e-18
        and eigen_minimum >= -1.0e-8
        and dc_residual <= 1.0e-9
        and gram_residual <= 1.0e-15
        and autocorrelation_residual <= 1.0e-15
        and added == 0
        and c_difference <= 1.0e-4
    )
    return {
        "dense_verification": dense,
        "high_precision_verification": high_precision,
        "psd_minimum_eigenvalue": eigen_minimum,
        "dc_equality_residual": dc_residual,
        "gram_construction_residual": gram_residual,
        "autocorrelation_diagonal_sum_residual": autocorrelation_residual,
        "post_projection_exchange_points_added": added,
        "active_frequency_set_sizes": {name: int(values.size) for name, values in grids.items()},
        "maximum_c_e_magnitude_difference_db_20_20k": c_difference,
        "c_source": c_provenance,
        "accepted": accepted,
    }


def certify_known_factor(
    autocorrelation: np.ndarray,
    coefficients: np.ndarray,
    fft_len: int,
    work_dir: Path,
) -> dict[str, Any]:
    saved_autocorrelation = np.asarray(autocorrelation, dtype=np.float64)
    target = evaluate_power(saved_autocorrelation, fft_len)
    factor_coefficients = np.asarray(coefficients, dtype=np.float64)
    response = np.fft.rfft(factor_coefficients, n=fft_len)
    frequency = np.linspace(0.0, 44_100.0, response.size)
    passband = frequency <= 20_000.0
    power_error = np.abs(np.abs(response) ** 2 - target)
    roots = np.roots(factor_coefficients)
    maximum_zero_radius = float(np.max(np.abs(roots))) if roots.size else 0.0
    worst_count = min(128, power_error.size)
    worst_indices = np.argpartition(power_error, -worst_count)[-worst_count:]
    high_precision = _high_precision_reconstruction(
        factor_coefficients,
        saved_autocorrelation,
        worst_indices,
        fft_len,
    )
    crosscheck = _homomorphic_factor(saved_autocorrelation, fft_len)
    crosscheck /= math.fsum(float(value) for value in crosscheck)
    crosscheck_response = np.fft.rfft(crosscheck, n=fft_len)
    crosscheck_power_difference = float(
        np.max(np.abs(np.abs(crosscheck_response) ** 2 - np.abs(response) ** 2))
    )
    report = {
        "primary_method": "explicit structured minimum-phase seed with exact autocorrelation reconstruction",
        "crosscheck_method": "independent homomorphic factorization from the saved autocorrelation",
        "fft_len": fft_len,
        "factor_coefficients": int(factor_coefficients.size),
        "factor_coefficient_sha256": _sha256_array(factor_coefficients),
        "autocorrelation_sha256": _sha256_array(saved_autocorrelation),
        "maximum_passband_power_reconstruction_error": float(np.max(power_error[passband])),
        "maximum_fullband_power_reconstruction_error": float(np.max(power_error)),
        "homomorphic_crosscheck_maximum_power_difference": crosscheck_power_difference,
        "maximum_zero_radius": maximum_zero_radius,
        "all_zeros_inside_unit_circle_with_tolerance": bool(maximum_zero_radius <= 1.0 + 1.0e-7),
        "high_precision_reconstruction": high_precision,
    }
    report["accepted"] = bool(
        report["maximum_passband_power_reconstruction_error"] <= 1.0e-12
        and report["maximum_fullband_power_reconstruction_error"] <= 1.0e-9
        and high_precision["maximum_power_reconstruction_error"] <= 1.0e-9
        and report["all_zeros_inside_unit_circle_with_tolerance"]
    )
    work_dir.mkdir(parents=True, exist_ok=True)
    np.save(work_dir / "spectral_factor_coefficients.npy", factor_coefficients)
    np.save(work_dir / "sdp_magnitude.npy", np.sqrt(np.maximum(target, 0.0)))
    _atomic_json(work_dir / "spectral_factor.json", report)
    if not report["accepted"]:
        raise RuntimeError("Split Phase E known spectral factor failed independent acceptance")
    return report


def certify_saved_factor(
    autocorrelation: np.ndarray,
    fft_len: int,
    work_dir: Path,
) -> dict[str, Any]:
    with np.load(work_dir / "magnitude_order_512.npz") as artifact:
        coefficients = np.asarray(artifact["seed_coefficients"], dtype=np.float64)
    return certify_known_factor(
        autocorrelation,
        coefficients,
        max(fft_len, 8_388_608),
        work_dir,
    )


def _atomic_npz(path: Path, **arrays: np.ndarray) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, suffix=".npz", delete=False) as temporary:
        temporary_path = Path(temporary.name)
        np.savez(temporary, **arrays)
        temporary.flush()
        os.fsync(temporary.fileno())
    os.replace(temporary_path, path)


def _atomic_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = (json.dumps(value, indent=2) + "\n").encode("utf-8")
    with tempfile.NamedTemporaryFile(dir=path.parent, suffix=".json", delete=False) as temporary:
        temporary_path = Path(temporary.name)
        temporary.write(payload)
        temporary.flush()
        os.fsync(temporary.fileno())
    os.replace(temporary_path, path)


def build(
    root: Path,
    work_dir: Path,
    spec: MagnitudeSpec | None = None,
    search: SeedSearchSpec | None = None,
) -> dict[str, Any]:
    spec = MagnitudeSpec() if spec is None else spec
    search = SeedSearchSpec() if search is None else search
    started = time.time()
    c_character, c_provenance = load_split_c_character(root)
    linear_coefficients, search_report = search_c_guided_seed(c_character, spec, search)
    coefficients, minimum_phase_report = minimum_phase_seed(
        linear_coefficients,
        spec.verification_fft_len,
    )
    gram = gram_with_positive_floor(coefficients, search.positivity_floor_power)
    autocorrelation = autocorrelation_from_gram(gram)
    audit = _audit_arrays(
        root,
        spec,
        coefficients,
        gram,
        autocorrelation,
        search.positivity_floor_power,
    )
    report = {
        "identity": EXPERIMENT_IDENTITY,
        "status": "experimental magnitude artifact; no production ID or UI wiring",
        "formulation": "C-guided structured FIR projection with an exact positive-floor Fejer-Riesz Gram construction",
        "cold_sdp_iterations": 0,
        "solver": "none",
        "order": spec.order,
        "specification": asdict(spec),
        "search_specification": asdict(search),
        "c_source": c_provenance,
        "search": search_report,
        "minimum_phase_conversion": minimum_phase_report,
        "coefficient_sha256": _sha256_array(coefficients),
        "autocorrelation_sha256": _sha256_array(autocorrelation),
        "gram_sha256": _sha256_array(gram),
        "positivity_floor_power": search.positivity_floor_power,
        "elapsed_seconds": time.time() - started,
        **audit,
    }
    work_dir.mkdir(parents=True, exist_ok=True)
    _atomic_npz(
        work_dir / f"magnitude_order_{spec.order}.npz",
        autocorrelation=autocorrelation,
        gram=gram,
        seed_coefficients=coefficients,
        linear_seed_coefficients=linear_coefficients,
    )
    _atomic_json(work_dir / f"magnitude_order_{spec.order}.json", report)
    if not report["accepted"]:
        raise RuntimeError("Split Phase E C-guided magnitude artifact failed independent verification")
    return report


def audit_existing(root: Path, work_dir: Path, spec: MagnitudeSpec | None = None) -> dict[str, Any]:
    spec = MagnitudeSpec() if spec is None else spec
    artifact_path = work_dir / f"magnitude_order_{spec.order}.npz"
    report_path = work_dir / f"magnitude_order_{spec.order}.json"
    saved_report = json.loads(report_path.read_text())
    with np.load(artifact_path) as artifact:
        coefficients = np.asarray(artifact["seed_coefficients"], dtype=np.float64)
        gram = np.asarray(artifact["gram"], dtype=np.float64)
        autocorrelation = np.asarray(artifact["autocorrelation"], dtype=np.float64)
    if _sha256_array(coefficients) != saved_report.get("coefficient_sha256"):
        raise RuntimeError("saved Split Phase E seed coefficient hash does not match")
    positivity_floor = float(saved_report["positivity_floor_power"])
    audit = _audit_arrays(root, spec, coefficients, gram, autocorrelation, positivity_floor)
    report = {**saved_report, **audit, "re_audited": True}
    _atomic_json(report_path, report)
    if not report["accepted"]:
        raise RuntimeError("saved Split Phase E magnitude artifact failed independent re-audit")
    return report


def main() -> None:
    default_root = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=default_root)
    parser.add_argument("--work-dir", type=Path, default=Path(__file__).resolve().parent / "work-spe")
    parser.add_argument("--verification-fft-len", type=int, default=8_388_608)
    parser.add_argument("--screening-fft-len", type=int, default=1_048_576)
    parser.add_argument("--audit-existing", action="store_true")
    parser.add_argument("--factor", action="store_true")
    arguments = parser.parse_args()
    specification = MagnitudeSpec(verification_fft_len=arguments.verification_fft_len)
    if arguments.audit_existing:
        report = audit_existing(arguments.root, arguments.work_dir, specification)
    else:
        report = build(
            arguments.root,
            arguments.work_dir,
            specification,
            SeedSearchSpec(screening_fft_len=arguments.screening_fft_len),
        )
    if arguments.factor:
        with np.load(arguments.work_dir / f"magnitude_order_{specification.order}.npz") as artifact:
            autocorrelation = np.asarray(artifact["autocorrelation"], dtype=np.float64)
            coefficients = np.asarray(artifact["seed_coefficients"], dtype=np.float64)
        report = {
            "magnitude": report,
            "spectral_factor": certify_known_factor(
                autocorrelation,
                coefficients,
                specification.verification_fft_len,
                arguments.work_dir,
            ),
        }
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
