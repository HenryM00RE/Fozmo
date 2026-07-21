from __future__ import annotations

import argparse
import hashlib
import json
import math
from dataclasses import asdict
from pathlib import Path
from typing import Any

import numpy as np
from scipy import optimize

from .e3_p7_counterfactual import (
    INTERVALS_MS,
    character_counterfactual_residual,
    default_training_fixtures,
    fixture_contract,
    interval_metrics,
)
from .e3_phase_search import _cascade_character_and_cleanup, _read_f64le, _timing_metrics
from .evaluate_e3_packets import PACKET_FREQUENCIES_HZ, _measure_packet


IDENTITY = "SplitPhase128kE3-P7-cleanup1-counterfactual-pilot"
OUTPUT_RATE_HZ = 176_400
CHARACTER_RATE_HZ = 88_200
CHARACTER_ORIGIN = 2_530
TRACE_SAMPLES = round(OUTPUT_RATE_HZ * 0.050)
TRUST_RADII = (1.0e-5, 2.0e-5, 5.0e-5, 1.0e-4, 2.0e-4)
OBJECTIVES = ("transition", "post_lobe", "post_energy", "balanced")
REDUCED_DIRECTIONS = 24
FINALIST_COUNT = 12


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _halfband_geometry(cleanup: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    if cleanup.size % 2 != 1 or not np.allclose(cleanup, cleanup[::-1], atol=1e-15):
        raise RuntimeError("cleanup stage 1 must be an odd-length symmetric FIR")
    center = cleanup.size // 2
    if abs(cleanup[center] - 0.5) > 1.0e-15:
        raise RuntimeError("cleanup centre coefficient is not exactly 0.5")
    fixed_parity = np.arange(0, cleanup.size, 2)
    fixed_parity = fixed_parity[fixed_parity != center]
    if np.max(np.abs(cleanup[fixed_parity])) > 1.0e-15:
        raise RuntimeError("cleanup even halfband coefficients are not zero")
    left = np.arange(1, center, 2, dtype=np.int64)
    if left.size != 127:
        raise RuntimeError("unexpected cleanup halfband nullspace dimension")
    # Orthonormal Helmert basis for the exact sum-zero subspace. Each unique
    # odd coefficient occurs twice, so sum(delta)==0 preserves both branch sum
    # and interpolation DC exactly.
    basis = np.zeros((left.size, left.size - 1), dtype=np.float64)
    for column in range(left.size - 1):
        scale = math.sqrt((column + 1) * (column + 2))
        basis[: column + 1, column] = 1.0 / scale
        basis[column + 1, column] = -(column + 1) / scale
    return left, basis


def _pair_shift_matrix(
    source: np.ndarray,
    raw_indices: np.ndarray,
    left_indices: np.ndarray,
) -> np.ndarray:
    matrix = np.zeros((raw_indices.size, left_indices.size), dtype=np.float64)
    last = source.size
    filter_last = 508
    for column, left in enumerate(left_indices):
        for shift in (int(left), filter_last - int(left)):
            delta = raw_indices - shift
            valid = (delta >= 0) & ((delta & 1) == 0)
            source_index = delta[valid] // 2
            bounded = source_index < last
            rows = np.flatnonzero(valid)[bounded]
            matrix[rows, column] += 2.0 * source[source_index[bounded]]
    return matrix


def _cleanup_from_unique(
    baseline: np.ndarray, left_indices: np.ndarray, delta: np.ndarray
) -> np.ndarray:
    candidate = baseline.copy()
    for index, value in zip(left_indices, delta, strict=True):
        candidate[index] += value
        candidate[-1 - index] += value
    return candidate


def _main_lobe_bounds(response: np.ndarray) -> tuple[int, int, int]:
    peak = int(np.argmax(np.abs(response)))
    sign = math.copysign(1.0, float(response[peak]))
    left = peak
    while left > 0 and response[left - 1] != 0.0 and math.copysign(1.0, float(response[left - 1])) == sign:
        left -= 1
    right = peak
    while right + 1 < response.size and response[right + 1] != 0.0 and math.copysign(1.0, float(response[right + 1])) == sign:
        right += 1
    return left, peak, right


def _frequency_model(
    character: np.ndarray,
    cleanup: np.ndarray,
    left_indices: np.ndarray,
    transform: np.ndarray,
) -> dict[str, np.ndarray | float]:
    fft_length = 1 << 20
    upsampled = np.zeros(fft_length, dtype=np.float64)
    upsampled[: (character.size - 1) * 2 + 1 : 2] = 2.0 * character
    source_spectrum = np.fft.rfft(upsampled)
    bins = np.arange(source_spectrum.size, dtype=np.float64)
    frequencies = bins * OUTPUT_RATE_HZ / fft_length
    cleanup_spectrum = np.fft.rfft(cleanup, fft_length)
    baseline = source_spectrum * (2.0 * cleanup_spectrum)
    selected = (
        (frequencies <= 18_000.0)
        | ((frequencies >= 20_000.0) & (frequencies <= 22_050.0))
        | (frequencies >= 22_050.0)
    )
    # Keep the constraint model compact while retaining exact FFT-bin endpoints.
    selected_indices = np.flatnonzero(selected)[::128]
    endpoints = np.searchsorted(frequencies, [18_000.0, 20_000.0, 22_050.0, 88_200.0])
    selected_indices = np.unique(np.concatenate((selected_indices, endpoints.clip(0, bins.size - 1))))
    selected_omega = 2.0 * np.pi * selected_indices / fft_length
    selected_source = source_spectrum[selected_indices]
    center = cleanup.size // 2
    unique = np.empty((selected_indices.size, left_indices.size), dtype=np.complex128)
    phase = np.exp(-1j * selected_omega * center)
    for column, left in enumerate(left_indices):
        unique[:, column] = (
            selected_source
            * 4.0
            * np.cos(selected_omega * (center - int(left)))
            * phase
        )
    return {
        "frequency_hz": frequencies[selected_indices],
        "baseline": baseline[selected_indices],
        "directions": unique @ transform,
        "dc": float(abs(baseline[0])),
    }


def _frequency_fft_length(response_size: int, baseline_size: int) -> int:
    required = max(response_size, baseline_size)
    return 1 << max(20, (required - 1).bit_length())


def _frequency_metrics(response: np.ndarray, baseline: np.ndarray) -> dict[str, float]:
    fft_length = _frequency_fft_length(response.size, baseline.size)
    candidate_spectrum = np.fft.rfft(response, fft_length)
    baseline_spectrum = np.fft.rfft(baseline, fft_length)
    frequency = np.fft.rfftfreq(fft_length, 1.0 / OUTPUT_RATE_HZ)
    reliable = frequency <= 18_000.0
    delta_db = 20.0 * np.log10(
        np.maximum(np.abs(candidate_spectrum[reliable]), 1.0e-300)
        / np.maximum(np.abs(baseline_spectrum[reliable]), 1.0e-300)
    )
    stop = frequency >= 22_050.0
    stop_db = 20.0 * np.log10(
        max(float(np.max(np.abs(candidate_spectrum[stop]))) / abs(candidate_spectrum[0]), 1.0e-300)
    )
    transition = (frequency >= 20_000.0) & (frequency <= 22_050.0)
    transition_magnitude = np.abs(candidate_spectrum[transition])
    rebound = np.maximum(np.diff(transition_magnitude), 0.0)
    return {
        "maximum_passband_delta_db_0_18khz": float(np.max(np.abs(delta_db))),
        "maximum_stopband_db_22k05_nyquist": stop_db,
        "maximum_transition_rebound_linear": float(np.max(rebound, initial=0.0)),
    }


def search(root: Path, work_dir: Path, reduced_directions: int = REDUCED_DIRECTIONS) -> dict[str, Any]:
    assets = root / "assets/filters/split_phase_e2v3"
    character_path = root / "tools/split_phase_v6/baselines/e3-p6d-local-0145.f64le"
    cleanup_path = assets / "cleanup_stage_1.f64le"
    character = _read_f64le(character_path)
    cleanup = _read_f64le(cleanup_path)
    left_indices, nullspace = _halfband_geometry(cleanup)
    baseline_response = _cascade_character_and_cleanup(character, cleanup)
    baseline_metrics = asdict(_timing_metrics(baseline_response))
    baseline_frequency_metrics = _frequency_metrics(baseline_response, baseline_response)
    baseline_packets = {
        str(int(frequency)): asdict(_measure_packet(baseline_response, frequency))
        for frequency in PACKET_FREQUENCIES_HZ
    }
    left_main, peak, right_main = _main_lobe_bounds(baseline_response)

    # Static response Jacobian in the exact halfband nullspace.
    post_indices = np.arange(right_main + 1, min(peak + round(0.007 * OUTPUT_RATE_HZ), baseline_response.size))
    post_unique = _pair_shift_matrix(2.0 * character, post_indices, left_indices)
    post_null = post_unique @ nullspace
    baseline_post = baseline_response[post_indices]

    fixtures = default_training_fixtures()
    transition_blocks: list[np.ndarray] = []
    transition_baselines: list[np.ndarray] = []
    fixture_baselines: dict[str, dict[str, Any]] = {}
    cleanup_center = cleanup.size // 2
    pre_character = math.ceil(cleanup_center / 2) + 4
    character_count = pre_character + round(CHARACTER_RATE_HZ * 0.050) + pre_character + 8
    output_raw_indices = pre_character * 2 + cleanup_center + np.arange(TRACE_SAMPLES)
    for fixture in fixtures:
        character_residual = character_counterfactual_residual(
            character,
            fixture,
            character_origin=CHARACTER_ORIGIN,
            sample_start=-pre_character,
            sample_count=character_count,
        )
        unique = _pair_shift_matrix(character_residual, output_raw_indices, left_indices)
        baseline = unique @ cleanup[left_indices] + 2.0 * cleanup[cleanup_center] * np.where(
            ((output_raw_indices - cleanup_center) >= 0)
            & (((output_raw_indices - cleanup_center) & 1) == 0)
            & (((output_raw_indices - cleanup_center) // 2) < character_residual.size),
            character_residual[np.clip((output_raw_indices - cleanup_center) // 2, 0, character_residual.size - 1)],
            0.0,
        )
        direct = np.asarray(baseline, dtype=np.float64)
        null = unique @ nullspace
        fixture_baselines[fixture.name] = {
            "contract": fixture_contract(fixture),
            "intervals": interval_metrics(direct),
        }
        for interval_index in (0, 1):
            start_ms, end_ms = INTERVALS_MS[interval_index]
            start = round(start_ms * OUTPUT_RATE_HZ / 1000.0)
            end = round(end_ms * OUTPUT_RATE_HZ / 1000.0)
            scale = max(float(np.linalg.norm(direct[start:end])), 1.0e-300)
            transition_blocks.append(null[start:end] / scale)
            transition_baselines.append(direct[start:end] / scale)

    transition_matrix = np.vstack(transition_blocks)
    transition_base = np.concatenate(transition_baselines)
    post_scale = max(float(np.linalg.norm(baseline_post)), 1.0e-300)
    geometry = np.vstack((transition_matrix, post_null / post_scale))
    # Preserve the cleanup's zero-phase stop response at 64 uniformly spaced
    # frequencies before ranking objective sensitivity. This prevents the raw
    # transition SVD from spending all retained directions on vectors that the
    # frozen rejection floor immediately forbids.
    stop_frequency = np.linspace(22_050.0, 88_200.0, 64, endpoint=True)
    stop_omega = 2.0 * np.pi * stop_frequency / OUTPUT_RATE_HZ
    cleanup_center = cleanup.size // 2
    stop_unique = np.column_stack(
        [
            2.0 * np.cos(stop_omega * (cleanup_center - int(left)))
            for left in left_indices
        ]
    )
    stop_jacobian = stop_unique @ nullspace
    _, stop_singular_values, stop_right_vectors = np.linalg.svd(
        stop_jacobian, full_matrices=True
    )
    stop_rank = int(np.sum(stop_singular_values > stop_singular_values[0] * 1.0e-12))
    stop_nullspace = stop_right_vectors[stop_rank:].T
    if stop_nullspace.shape[1] == 0:
        raise RuntimeError("stopband-preserving cleanup nullspace is empty")
    reduced_geometry = geometry @ stop_nullspace
    _, singular_values, right_vectors = np.linalg.svd(reduced_geometry, full_matrices=False)
    count = min(reduced_directions, right_vectors.shape[0])
    reduced_coordinates = stop_nullspace @ right_vectors[:count].T
    reduced_transform = nullspace @ reduced_coordinates
    transition_reduced = transition_matrix @ reduced_coordinates
    post_reduced = post_null @ reduced_coordinates

    frequency_model = _frequency_model(character, cleanup, left_indices, reduced_transform)
    frequency = np.asarray(frequency_model["frequency_hz"])
    frequency_baseline = np.asarray(frequency_model["baseline"])
    frequency_directions = np.asarray(frequency_model["directions"])
    dc = float(frequency_model["dc"])
    passband = frequency <= 18_000.0
    transition = (frequency >= 20_000.0) & (frequency <= 22_050.0)
    stopband = frequency >= 22_050.0
    # The compact optimization grid carries 5 dB of margin; exact dense FFT
    # certification below still applies the frozen -150 dB production floor.
    stop_limit = dc * 10.0 ** (-155.0 / 20.0)
    baseline_phase = frequency_baseline / np.maximum(np.abs(frequency_baseline), 1.0e-300)
    collinear_directions = np.real(frequency_directions * np.conj(baseline_phase[:, None]))

    def linear_constraints(radius: float) -> optimize.LinearConstraint:
        pass_scale = np.maximum(np.abs(frequency_baseline[passband]) * (10.0 ** (1.0e-4 / 20.0) - 1.0), 1.0e-300)
        pass_matrix = collinear_directions[passband] / pass_scale[:, None]
        stop_matrix = collinear_directions[stopband] / stop_limit
        stop_baseline = np.abs(frequency_baseline[stopband]) / stop_limit
        matrix = np.vstack((reduced_transform / radius, pass_matrix, stop_matrix))
        lower = np.concatenate(
            (
                -np.ones(reduced_transform.shape[0]),
                -np.ones(pass_matrix.shape[0]),
                -np.ones(stop_matrix.shape[0]) - stop_baseline,
            )
        )
        upper = np.concatenate(
            (
                np.ones(reduced_transform.shape[0]),
                np.ones(pass_matrix.shape[0]),
                np.ones(stop_matrix.shape[0]) - stop_baseline,
            )
        )
        return optimize.LinearConstraint(matrix, lower, upper)

    baseline_post_lobe = max(float(np.max(np.abs(baseline_post))), 1.0e-300)

    def objective(q: np.ndarray, name: str) -> float:
        transition_value = float(np.mean((transition_base + transition_reduced @ q) ** 2))
        post = baseline_post + post_reduced @ q
        post_energy = float(np.dot(post, post) / max(np.dot(baseline_post, baseline_post), 1.0e-300))
        post_lobe = float(np.max(np.abs(post)) / baseline_post_lobe)
        if name == "transition":
            return transition_value + 0.01 * post_energy
        if name == "post_lobe":
            return post_lobe + 0.02 * transition_value
        if name == "post_energy":
            return post_energy + 0.02 * transition_value
        return transition_value + 0.25 * post_energy + 0.25 * post_lobe

    candidates: list[dict[str, Any]] = []
    cleanups: dict[str, np.ndarray] = {}
    for radius in TRUST_RADII:
        for objective_name in OBJECTIVES:
            linear_constraint = linear_constraints(radius)
            result = optimize.minimize(
                lambda q, name=objective_name: objective(q, name),
                np.zeros(count, dtype=np.float64),
                method="SLSQP",
                constraints=[linear_constraint],
                options={"maxiter": 300, "ftol": 1.0e-12, "disp": False},
            )
            linear_value = linear_constraint.A @ result.x
            linear_violation = float(
                max(
                    np.max(linear_constraint.lb - linear_value, initial=0.0),
                    np.max(linear_value - linear_constraint.ub, initial=0.0),
                )
            )
            optimizer_feasible = linear_violation <= 1.0e-6
            unique_delta = reduced_transform @ result.x
            candidate_cleanup = _cleanup_from_unique(cleanup, left_indices, unique_delta)
            response = _cascade_character_and_cleanup(character, candidate_cleanup)
            metrics = asdict(_timing_metrics(response))
            packets = {
                key: asdict(_measure_packet(response, float(key))) for key in baseline_packets
            }
            packet_delta = {
                key: packets[key]["onset_pre_echo_energy_db_total"]
                - baseline_packets[key]["onset_pre_echo_energy_db_total"]
                for key in packets
            }
            transition_reports = []
            for fixture in fixtures:
                from .e3_p7_counterfactual import cleanup_counterfactual_residual

                residual = cleanup_counterfactual_residual(character, candidate_cleanup, fixture)
                measured = interval_metrics(residual)
                baseline_intervals = fixture_baselines[fixture.name]["intervals"]
                for measured_interval, baseline_interval in zip(measured, baseline_intervals, strict=True):
                    measured_interval["delta_db_vs_p6"] = (
                        measured_interval["residual_rms_dbfs"]
                        - baseline_interval["residual_rms_dbfs"]
                    )
                transition_reports.append({"fixture": fixture.name, "intervals": measured})
            frequency_metrics = _frequency_metrics(response, baseline_response)
            identifier = f"cleanup-{objective_name}-r{radius:.0e}".replace("+", "")
            passes = bool(
                optimizer_feasible
                and metrics["maximum_pre_lobe_db_peak"] <= -22.5
                and metrics["pre_energy_db_total"] <= -4.85
                and metrics["main_lobe_width_us"] <= 62.5
                and metrics["step_overshoot_percent"] <= 9.22
                and metrics["decay_120_ms"] is not None
                and metrics["decay_120_ms"] <= 7.0
                and max(packet_delta.values()) <= 0.10
                and frequency_metrics["maximum_passband_delta_db_0_18khz"] <= 1.0e-4
                and frequency_metrics["maximum_stopband_db_22k05_nyquist"] <= -150.0
                and frequency_metrics["maximum_transition_rebound_linear"]
                <= baseline_frequency_metrics["maximum_transition_rebound_linear"]
                + abs(baseline_response.sum()) * 1.0e-15
            )
            first_two_deltas = [
                interval["delta_db_vs_p6"]
                for report in transition_reports
                for interval in report["intervals"][:2]
            ]
            timing_deltas = {key: metrics[key] - baseline_metrics[key] for key in metrics if metrics[key] is not None and baseline_metrics[key] is not None}
            meaningful = bool(
                min(first_two_deltas) <= -0.03
                or timing_deltas["maximum_post_lobe_db_peak"] <= -0.05
                or timing_deltas["post_energy_db_total"] <= -0.02
                or timing_deltas["main_lobe_width_us"] <= -0.20
                or timing_deltas["step_undershoot_percent"] <= -0.05
            )
            record = {
                "identifier": identifier,
                "objective": objective_name,
                "trust_radius": radius,
                "optimizer_success": bool(result.success),
                "optimizer_feasible": optimizer_feasible,
                "maximum_linear_constraint_violation": linear_violation,
                "optimizer_message": str(result.message),
                "optimizer_iterations": int(result.nit),
                "maximum_unique_coefficient_delta": float(np.max(np.abs(unique_delta))),
                "sum_unique_coefficient_delta": float(np.sum(unique_delta)),
                "metrics": metrics,
                "timing_delta_vs_p6": timing_deltas,
                "packets": packets,
                "packet_delta_db_vs_p6": packet_delta,
                "counterfactual_fixtures": transition_reports,
                "worst_0_5ms_delta_db_vs_p6": float(max(first_two_deltas)),
                "best_0_5ms_delta_db_vs_p6": float(min(first_two_deltas)),
                "mean_0_5ms_delta_db_vs_p6": float(np.mean(first_two_deltas)),
                "frequency": frequency_metrics,
                "passes_hard_gates": passes,
                "passes_minimum_effect_size": meaningful,
            }
            candidates.append(record)
            cleanups[identifier] = candidate_cleanup

    qualified = [record for record in candidates if record["passes_hard_gates"]]
    qualified.sort(
        key=lambda record: (
            record["worst_0_5ms_delta_db_vs_p6"],
            record["mean_0_5ms_delta_db_vs_p6"],
            record["metrics"]["maximum_post_lobe_db_peak"],
            record["metrics"]["post_energy_db_total"],
            record["metrics"]["step_undershoot_percent"],
        )
    )
    finalist_dir = work_dir / "finalists"
    finalist_dir.mkdir(parents=True, exist_ok=True)
    finalists = qualified[:FINALIST_COUNT]
    for record in finalists:
        payload = np.asarray(cleanups[record["identifier"]], dtype="<f8").tobytes()
        path = finalist_dir / f"{record['identifier']}.f64le"
        path.write_bytes(payload)
        record["cleanup_file"] = str(path.relative_to(work_dir)).replace("\\", "/")
        record["cleanup_sha256"] = _sha256_bytes(payload)

    report = {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "source": {
            "character": str(character_path.relative_to(root)).replace("\\", "/"),
            "character_sha256": _sha256_bytes(character_path.read_bytes()),
            "cleanup_stage_1": str(cleanup_path.relative_to(root)).replace("\\", "/"),
            "cleanup_stage_1_sha256": _sha256_bytes(cleanup_path.read_bytes()),
        },
        "geometry": {
            "unique_odd_pairs": int(left_indices.size),
            "exact_nullspace_dimensions": int(nullspace.shape[1]),
            "stopband_preserving_constraints": int(stop_rank),
            "stopband_preserving_dimensions": int(stop_nullspace.shape[1]),
            "stopband_constraint_singular_values": stop_singular_values.tolist(),
            "retained_svd_directions": count,
            "singular_values": singular_values.tolist(),
            "condition_ratio_retained": float(singular_values[count - 1] / singular_values[0]),
        },
        "contract": {
            "counterfactual": "actual mute/restart minus continuously running recovered carrier",
            "intervals_ms": INTERVALS_MS,
            "training_fixtures": [fixture_contract(fixture) for fixture in fixtures],
            "trust_radii": TRUST_RADII,
            "objectives": OBJECTIVES,
            "minimum_effect_sizes": {
                "transition_interval_db": 0.03,
                "maximum_lobe_db": 0.05,
                "integrated_side_energy_db": 0.02,
                "main_lobe_width_us": 0.20,
                "step_response_percentage_points": 0.05,
            },
        },
        "baseline": {
            "metrics": baseline_metrics,
            "frequency": baseline_frequency_metrics,
            "packets": baseline_packets,
            "counterfactual_fixtures": fixture_baselines,
        },
        "candidate_count": len(candidates),
        "hard_gate_qualified_count": len(qualified),
        "meaningful_qualified_count": sum(
            record["passes_hard_gates"] and record["passes_minimum_effect_size"]
            for record in candidates
        ),
        "finalists": finalists,
        "candidates": candidates,
    }
    work_dir.mkdir(parents=True, exist_ok=True)
    (work_dir / "e3_p7_cleanup_search.json").write_text(
        json.dumps(report, indent=2) + "\n", encoding="utf-8"
    )
    return report


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--reduced-directions", type=int, default=REDUCED_DIRECTIONS)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    work_dir = (arguments.work_dir or root / "tools/split_phase_v6/work-e3-p7-cleanup").resolve()
    report = search(root, work_dir, arguments.reduced_directions)
    print(
        json.dumps(
            {
                "candidate_count": report["candidate_count"],
                "hard_gate_qualified_count": report["hard_gate_qualified_count"],
                "meaningful_qualified_count": report["meaningful_qualified_count"],
                "best": report["finalists"][0]["identifier"] if report["finalists"] else None,
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
