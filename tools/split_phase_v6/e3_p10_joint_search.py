from __future__ import annotations

import argparse
import hashlib
import json
import math
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

import numpy as np
from scipy.stats import qmc

from tools.split_phase_v4.group_delay_spline import _basis as spline_basis

from .e3_p5_group_delay_search import _build_model
from .e3_p7_cleanup_search import _frequency_metrics, _halfband_geometry
from .e3_p9_feasibility import _meaningful
from .e3_phase_search import (
    CHARACTER_RATE_HZ,
    FFT_LENGTH,
    OUTPUT_RATE_HZ,
    _cascade_character_and_cleanup,
    _read_f64le,
    _timing_metrics,
)
from .e3_p10_packet_contract import (
    PACKET_ABSOLUTE_CEILINGS_DB,
    PACKET_GATES_DB,
    measure_packet_set,
    packet_contract,
    packet_gate_deltas,
    packet_gate_failures,
)


IDENTITY = "SplitPhase128kE3-P10-windowed-packet-nullspace-joint-search"
PHASE_LOW_JOIN_HZ = 2_000.0
PHASE_TREBLE_JOIN_HZ = 19_500.0
PHASE_SPLINE_CONTROLS = 103
PHASE_COORDINATES = 96
PHASE_FREE_BOUND = 0.75
PHASE_DELTA_CAP_RAD = 0.10
GROUP_DELAY_CAP_SAMPLES = 1.50
CURVATURE_CAP = 20_000.0
SMOOTH_PHASE_DIRECTIONS = 40
CLEANUP_DIRECTIONS = 12
CLEANUP_BOUND = 1.0e-7
RADII = np.asarray((0.02, 0.05, 0.10, 0.18, 0.30, 0.45, 0.65, 0.85, 1.0))
HOLDOUT_FREQUENCIES_HZ = (7_000.0, 12_000.0, 13_500.0, 16_000.0, 17_000.0, 19_000.0, 19_500.0)
HOLDOUT_CYCLES = (4.0, 8.0, 16.0)

MAGNITUDE_KNOTS_HZ = np.asarray(
    (
        15_000.0,
        16_000.0,
        17_000.0,
        18_000.0,
        19_000.0,
        20_000.0,
        20_500.0,
        21_000.0,
        21_500.0,
        22_050.0,
        23_000.0,
        24_000.0,
        26_000.0,
    ),
    dtype=np.float64,
)

FAMILIES = {
    "moderate": {
        "lower_db": np.asarray(
            (-0.001, -0.005, -0.010, -0.080, -0.250, -2.0, -8.0, -24.0, 0.0, 0.0, 0.0)
        ),
        "upper_db": np.asarray(
            (0.001, 0.001, 0.0, 0.0, 0.0, 1.0, 4.0, 12.0, 63.0, 80.0, 80.0)
        ),
        "passband_0_16khz_db": 0.001,
        "passband_16_18khz_db": 0.010,
        "passband_18_20khz_db": 0.250,
        "stopband_floor_db": -120.0,
        "production_eligible_family": True,
    },
    "aggressive": {
        "lower_db": np.asarray(
            (-0.001, -0.020, -0.050, -0.200, -0.500, -4.0, -15.0, -50.0, 0.0, 0.0, 0.0)
        ),
        "upper_db": np.asarray(
            (0.001, 0.001, 0.0, 0.0, 0.0, 2.0, 8.0, 30.0, 83.0, 120.0, 120.0)
        ),
        "passband_0_16khz_db": 0.001,
        "passband_16_18khz_db": 0.050,
        "passband_18_20khz_db": 0.500,
        "stopband_floor_db": -100.0,
        "production_eligible_family": False,
    },
}


def _smoothstep11(value: np.ndarray) -> np.ndarray:
    """C5-continuous zero-to-one transition (degree eleven)."""
    x = np.clip(value, 0.0, 1.0)
    return x**6 * (
        462.0
        + x
        * (
            -1_980.0
            + x * (3_465.0 + x * (-3_080.0 + x * (1_386.0 + x * (-252.0))))
        )
    )


def _magnitude_basis(frequency_hz: np.ndarray) -> np.ndarray:
    basis = np.zeros(
        (frequency_hz.size, MAGNITUDE_KNOTS_HZ.size), dtype=np.float64
    )
    for interval in range(MAGNITUDE_KNOTS_HZ.size - 1):
        low = MAGNITUDE_KNOTS_HZ[interval]
        high = MAGNITUDE_KNOTS_HZ[interval + 1]
        active = (frequency_hz >= low) & (frequency_hz <= high)
        smooth = _smoothstep11((frequency_hz[active] - low) / (high - low))
        basis[active, interval] = 1.0 - smooth
        basis[active, interval + 1] = smooth
    return basis


@dataclass
class SearchContext:
    root: Path
    family: str
    character: np.ndarray
    cleanup: np.ndarray
    anchor_spectrum: np.ndarray
    anchor_sum: float
    active_phase_bins: np.ndarray
    phase_map: np.ndarray
    delay_sample_map: np.ndarray
    curvature_sample_map: np.ndarray
    magnitude_basis: np.ndarray
    magnitude_lower: np.ndarray
    magnitude_upper: np.ndarray
    cleanup_left: np.ndarray
    cleanup_modes: np.ndarray
    baseline_response: np.ndarray
    baseline_timing: dict[str, Any]
    baseline_packets: dict[str, dict[str, float]]
    baseline_frequency: dict[str, float]


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _cleanup_modes(cleanup: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    left, equality_nullspace = _halfband_geometry(cleanup)
    center = cleanup.size // 2
    # Preserve the cleanup response exactly on a deterministic passband and
    # stopband grid before exposing any directions to the joint optimizer.
    # The halfband equality nullspace already preserves symmetry, centre tap,
    # branch sum and DC; this second nullspace prevents a timing direction from
    # buying its score with an image-rejection collapse.
    constraint_frequency = np.concatenate(
        (
            np.linspace(0.0, 20_000.0, 24, endpoint=True),
            np.linspace(22_050.0, 88_200.0, 88, endpoint=True),
        )
    )
    omega = 2.0 * np.pi * constraint_frequency / OUTPUT_RATE_HZ
    unique_response = np.column_stack(
        [2.0 * np.cos(omega * (center - int(index))) for index in left]
    )
    constrained = unique_response @ equality_nullspace
    _, singular_values, right_vectors = np.linalg.svd(constrained, full_matrices=True)
    threshold = max(float(singular_values[0]) * 1.0e-12, 1.0e-13)
    rank = int(np.sum(singular_values > threshold))
    frequency_nullspace = right_vectors[rank:].T
    if frequency_nullspace.shape[1] < CLEANUP_DIRECTIONS:
        raise RuntimeError("cleanup frequency-preserving nullspace is too small")
    unique = equality_nullspace @ frequency_nullspace[:, :CLEANUP_DIRECTIONS]
    unique /= np.max(np.abs(unique), axis=0, keepdims=True)
    modes = np.zeros((cleanup.size, CLEANUP_DIRECTIONS), dtype=np.float64)
    modes[left] = unique
    modes[cleanup.size - 1 - left] = unique
    return left, modes


def _phase_geometry() -> tuple[np.ndarray, np.ndarray, np.ndarray, np.ndarray, Any]:
    model = _build_model(
        PHASE_LOW_JOIN_HZ, PHASE_TREBLE_JOIN_HZ, PHASE_SPLINE_CONTROLS
    )
    if model.free_coordinates != PHASE_COORDINATES:
        raise RuntimeError("P10 phase model no longer has 96 free coordinates")
    frequency = np.fft.rfftfreq(FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ)
    active_bins = np.flatnonzero(
        (frequency >= PHASE_LOW_JOIN_HZ) & (frequency <= 22_050.0)
    )
    active_frequency = frequency[active_bins]
    state = model.nullspace[:-1]
    delay_map = spline_basis(
        model.knots, model.degree, np.log(active_frequency), 0
    ) @ state
    curvature_map = spline_basis(
        model.knots, model.degree, np.log(active_frequency), 2
    ) @ state
    omega = 2.0 * np.pi * active_frequency / CHARACTER_RATE_HZ
    phase_map = np.zeros_like(delay_map)
    phase_map[1:] = -np.cumsum(
        0.5 * (delay_map[1:] + delay_map[:-1]) * np.diff(omega)[:, None],
        axis=0,
    )
    return active_bins, phase_map, delay_map, curvature_map, model


def _build_context(root: Path, family: str) -> SearchContext:
    if family not in FAMILIES:
        raise ValueError(f"unknown magnitude family: {family}")
    character_path = root / "assets/filters/split_phase_e2v3/character_full_rate.f64le"
    cleanup_path = root / "assets/filters/split_phase_e2v3/cleanup_stage_1.f64le"
    character = _read_f64le(character_path)
    cleanup = _read_f64le(cleanup_path)
    anchor_spectrum = np.fft.rfft(character, FFT_LENGTH)
    frequency = np.fft.rfftfreq(FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ)
    magnitude_full = _magnitude_basis(frequency)
    # Endpoint controls are frozen at zero so the magnitude perturbation and
    # its first two derivatives close at 15 and 26 kHz.
    magnitude_basis = magnitude_full[:, 1:-1]
    active_bins, phase_map, delay_map, curvature_map, _ = _phase_geometry()
    cleanup_left, cleanup_modes = _cleanup_modes(cleanup)
    response = _cascade_character_and_cleanup(character, cleanup)
    return SearchContext(
        root=root,
        family=family,
        character=character,
        cleanup=cleanup,
        anchor_spectrum=anchor_spectrum,
        anchor_sum=float(math.fsum(float(value) for value in character)),
        active_phase_bins=active_bins,
        phase_map=phase_map,
        delay_sample_map=delay_map,
        curvature_sample_map=curvature_map,
        magnitude_basis=magnitude_basis,
        magnitude_lower=np.asarray(FAMILIES[family]["lower_db"], dtype=np.float64),
        magnitude_upper=np.asarray(FAMILIES[family]["upper_db"], dtype=np.float64),
        cleanup_left=cleanup_left,
        cleanup_modes=cleanup_modes,
        baseline_response=response,
        baseline_timing=asdict(_timing_metrics(response)),
        baseline_packets=measure_packet_set(response),
        baseline_frequency=_frequency_metrics(response, response),
    )


def _coordinate_slices(context: SearchContext) -> tuple[slice, slice, slice]:
    phase = slice(0, PHASE_COORDINATES)
    magnitude = slice(phase.stop, phase.stop + context.magnitude_basis.shape[1])
    cleanup = slice(magnitude.stop, magnitude.stop + CLEANUP_DIRECTIONS)
    return phase, magnitude, cleanup


def _coordinate_bounds(context: SearchContext) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    _, magnitude, _ = _coordinate_slices(context)
    count = magnitude.stop + CLEANUP_DIRECTIONS
    lower = np.empty(count, dtype=np.float64)
    upper = np.empty(count, dtype=np.float64)
    lower[:PHASE_COORDINATES] = -PHASE_FREE_BOUND
    upper[:PHASE_COORDINATES] = PHASE_FREE_BOUND
    lower[magnitude] = context.magnitude_lower
    upper[magnitude] = context.magnitude_upper
    lower[magnitude.stop :] = -CLEANUP_BOUND
    upper[magnitude.stop :] = CLEANUP_BOUND
    scales = np.maximum(np.abs(lower), np.abs(upper))
    return lower, upper, scales


def _realize(
    context: SearchContext, coordinates: np.ndarray
) -> tuple[np.ndarray, np.ndarray, dict[str, float]]:
    phase_slice, magnitude_slice, cleanup_slice = _coordinate_slices(context)
    phase_coordinates = coordinates[phase_slice]
    magnitude_coordinates = coordinates[magnitude_slice]
    cleanup_coordinates = coordinates[cleanup_slice]
    phase_delta = context.phase_map @ phase_coordinates
    magnitude_delta = context.magnitude_basis @ magnitude_coordinates
    target = context.anchor_spectrum * np.power(10.0, magnitude_delta / 20.0)
    target = target.astype(np.complex128, copy=True)
    target[context.active_phase_bins] *= np.exp(1j * phase_delta)
    target[0] = complex(float(target[0].real), 0.0)
    target[-1] = complex(float(target[-1].real), 0.0)
    periodic = np.fft.irfft(target, FFT_LENGTH)
    character = periodic[: context.character.size].copy()
    character *= context.anchor_sum / float(math.fsum(float(value) for value in character))
    cleanup = context.cleanup + context.cleanup_modes @ cleanup_coordinates
    delay = context.delay_sample_map @ phase_coordinates
    curvature = context.curvature_sample_map @ phase_coordinates
    total = max(float(np.dot(periodic, periodic)), 1.0e-300)
    omitted = float(np.dot(periodic[context.character.size :], periodic[context.character.size :])) / total
    edge_count = min(2_048, character.size // 4)
    edge = float(
        np.dot(character[:edge_count], character[:edge_count])
        + np.dot(character[-edge_count:], character[-edge_count:])
    ) / max(float(np.dot(character, character)), 1.0e-300)
    structural = {
        "maximum_phase_delta_rad": float(np.max(np.abs(phase_delta), initial=0.0)),
        "phase_closure_error_rad": float(phase_delta[-1] if phase_delta.size else 0.0),
        "maximum_group_delay_delta_samples": float(np.max(np.abs(delay), initial=0.0)),
        "maximum_group_delay_curvature_samples_per_ln_hz_squared": float(
            np.max(np.abs(curvature), initial=0.0)
        ),
        "minimum_magnitude_delta_db": float(np.min(magnitude_delta, initial=0.0)),
        "maximum_magnitude_delta_db": float(np.max(magnitude_delta, initial=0.0)),
        "maximum_cleanup_coefficient_delta": float(
            np.max(np.abs(context.cleanup_modes @ cleanup_coordinates), initial=0.0)
        ),
        "cleanup_sum_delta": float(math.fsum(float(value) for value in cleanup - context.cleanup)),
        "omitted_periodic_energy_ratio": omitted,
        "character_edge_energy_ratio": edge,
    }
    return character, cleanup, structural


def _measure_core(character: np.ndarray, cleanup: np.ndarray) -> dict[str, Any]:
    response = _cascade_character_and_cleanup(character, cleanup)
    return {
        "response": response,
        "timing": asdict(_timing_metrics(response)),
        "packets": measure_packet_set(response),
    }


def _result_vector(measured: dict[str, Any]) -> tuple[np.ndarray, list[str], np.ndarray]:
    values: list[float] = []
    names: list[str] = []
    scales: list[float] = []
    timing_scales = {
        "pre_energy_db_total": 0.10,
        "maximum_pre_lobe_db_peak": 0.25,
        "post_energy_db_total": 0.10,
        "maximum_post_lobe_db_peak": 0.25,
        "main_lobe_width_us": 2.0,
        "step_overshoot_percent": 0.50,
        "step_undershoot_percent": 0.50,
    }
    for metric, scale in timing_scales.items():
        values.append(float(measured["timing"][metric]))
        names.append(f"timing/{metric}")
        scales.append(scale)
    for frequency, packet in measured["packets"].items():
        for metric in PACKET_GATES_DB:
            values.append(float(packet[metric]))
            names.append(f"packet/{frequency}/{metric}")
            scales.append(max(PACKET_GATES_DB[metric], 0.10))
    return np.asarray(values), names, np.asarray(scales)


def _finite_difference_steps(context: SearchContext) -> np.ndarray:
    phase, magnitude, cleanup = _coordinate_slices(context)
    steps = np.empty(cleanup.stop, dtype=np.float64)
    steps[phase] = 0.02
    magnitude_scale = np.maximum(
        np.abs(context.magnitude_lower), np.abs(context.magnitude_upper)
    )
    steps[magnitude] = np.maximum(np.minimum(magnitude_scale * 0.02, 0.05), 1.0e-4)
    steps[cleanup] = 1.0e-6
    return steps


def _sensitivity(context: SearchContext) -> dict[str, Any]:
    baseline_measurement = {
        "timing": context.baseline_timing,
        "packets": context.baseline_packets,
    }
    baseline_vector, names, scales = _result_vector(baseline_measurement)
    steps = _finite_difference_steps(context)
    jacobian = np.empty((baseline_vector.size, steps.size), dtype=np.float64)
    coordinate_reports = []
    phase, magnitude, _ = _coordinate_slices(context)
    for coordinate, step in enumerate(steps):
        vectors = []
        structures = []
        for sign in (-1.0, 1.0):
            values = np.zeros(steps.size, dtype=np.float64)
            values[coordinate] = sign * step
            character, cleanup, structural = _realize(context, values)
            measured = _measure_core(character, cleanup)
            vector, measured_names, _ = _result_vector(measured)
            if measured_names != names:
                raise RuntimeError("P10 sensitivity result vector changed shape")
            vectors.append(vector)
            structures.append(structural)
        jacobian[:, coordinate] = (vectors[1] - vectors[0]) / (2.0 * step)
        coordinate_reports.append(
            {
                "index": coordinate,
                "kind": (
                    "group_delay"
                    if coordinate < phase.stop
                    else "magnitude_db"
                    if coordinate < magnitude.stop
                    else "cleanup_halfband"
                ),
                "finite_difference_step": float(step),
                "negative_structural": structures[0],
                "positive_structural": structures[1],
            }
        )
    normalized = jacobian / scales[:, None]
    singular_values = np.linalg.svd(normalized, compute_uv=False)
    return {
        "baseline_vector": baseline_vector,
        "result_names": names,
        "result_scales": scales,
        "steps": steps,
        "jacobian": jacobian,
        "normalized_singular_values": singular_values,
        "coordinates": coordinate_reports,
    }


def _packet_allowed(reference: float, metric: str) -> float:
    return max(
        reference + PACKET_GATES_DB[metric],
        PACKET_ABSOLUTE_CEILINGS_DB.get(metric, -300.0),
    )


def _linear_safe(
    predicted: np.ndarray,
    baseline: np.ndarray,
    names: list[str],
) -> bool:
    values = baseline + predicted
    by_name = dict(zip(names, values, strict=True))
    timing_limits = {
        "pre_energy_db_total": -4.85,
        "maximum_pre_lobe_db_peak": -18.20,
        "post_energy_db_total": -2.3738911226100226,
        "maximum_post_lobe_db_peak": -7.702214322277805,
        "main_lobe_width_us": 68.9430162564111,
        "step_overshoot_percent": 10.954176789621346,
        "step_undershoot_percent": 10.331889305015538,
    }
    if any(by_name[f"timing/{metric}"] > limit for metric, limit in timing_limits.items()):
        return False
    for name, value in by_name.items():
        if not name.startswith("packet/"):
            continue
        _, frequency, metric = name.split("/", 2)
        reference = baseline[names.index(name)]
        if value > _packet_allowed(reference, metric) - 0.01:
            return False
    return True


def _strict_packet_nullspace(
    context: SearchContext,
    sensitivity: dict[str, Any],
    scales: np.ndarray,
) -> tuple[np.ndarray, dict[str, Any]]:
    names = sensitivity["result_names"]
    rows = np.asarray(
        [
            index
            for index, name in enumerate(names)
            if name.startswith("packet/")
            and name.endswith("maximum_onset_pre_echo_db_peak")
        ],
        dtype=np.int64,
    )
    jacobian = sensitivity["jacobian"]
    # Random motion in the raw 96-coordinate spline basis excites local
    # curvature and produces a quiet but very long tail. Retain the 40 least-
    # curvature phase combinations before taking the strict packet-peak
    # nullspace. Magnitude and cleanup remain measured by the joint Jacobian,
    # but are frozen in this first feasibility stage; their one-sided bounds
    # otherwise reject nearly every unbiased high-dimensional sample.
    curvature = context.curvature_sample_map[::256] * PHASE_FREE_BOUND
    _, curvature_singular_values, curvature_right = np.linalg.svd(
        curvature, full_matrices=False
    )
    smooth_phase = curvature_right[-SMOOTH_PHASE_DIRECTIONS:].T
    design = np.zeros((scales.size, SMOOTH_PHASE_DIRECTIONS), dtype=np.float64)
    design[:PHASE_COORDINATES, :SMOOTH_PHASE_DIRECTIONS] = smooth_phase
    normalized = (jacobian[rows] * scales[None, :]) @ design
    _, singular_values, right_vectors = np.linalg.svd(normalized, full_matrices=True)
    threshold = max(float(singular_values[0]) * 1.0e-10, 1.0e-12)
    rank = int(np.sum(singular_values > threshold))
    reduced_nullspace = right_vectors[rank:].T
    nullspace = design @ reduced_nullspace
    return nullspace, {
        "strict_packet_rows": rows.tolist(),
        "singular_values": singular_values.tolist(),
        "rank": rank,
        "dimensions": int(nullspace.shape[1]),
        "smooth_phase_directions": SMOOTH_PHASE_DIRECTIONS,
        "smooth_phase_curvature_singular_values": curvature_singular_values.tolist(),
        "screened_parameter_blocks": ["group_delay"],
        "frozen_parameter_blocks": ["magnitude", "cleanup_stage_1"],
    }


def _screen(
    context: SearchContext,
    sensitivity: dict[str, Any],
    candidate_count: int,
) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    if candidate_count <= 0 or candidate_count & (candidate_count - 1):
        raise ValueError("candidate count must be a positive power of two")
    lower, upper, scales = _coordinate_bounds(context)
    nullspace, null_report = _strict_packet_nullspace(context, sensitivity, scales)
    jacobian = sensitivity["jacobian"]
    names = sensitivity["result_names"]
    baseline = sensitivity["baseline_vector"]
    index = {name: offset for offset, name in enumerate(names)}
    objectives = [
        "timing/maximum_pre_lobe_db_peak",
        "timing/maximum_post_lobe_db_peak",
        "timing/pre_energy_db_total",
        "timing/post_energy_db_total",
        "timing/main_lobe_width_us",
        "timing/step_overshoot_percent",
        "timing/step_undershoot_percent",
    ]
    anchors = []
    for objective in objectives:
        gradient = jacobian[index[objective]] * scales
        projected = -(nullspace @ (nullspace.T @ gradient))
        norm = float(np.max(np.abs(projected), initial=0.0))
        if norm > 0.0:
            anchors.append(projected / norm)
    balanced_gradient = sum(
        jacobian[index[name]] * scales / scale
        for name, scale in (
            ("timing/maximum_pre_lobe_db_peak", 0.25),
            ("timing/maximum_post_lobe_db_peak", 0.25),
            ("timing/post_energy_db_total", 0.10),
            ("timing/main_lobe_width_us", 2.0),
            ("timing/step_undershoot_percent", 0.50),
        )
    )
    balanced = -(nullspace @ (nullspace.T @ balanced_gradient))
    balanced /= max(float(np.max(np.abs(balanced))), 1.0e-300)
    anchors.append(balanced)

    unit = qmc.Sobol(d=nullspace.shape[1], scramble=False).random_base2(
        int(math.log2(candidate_count))
    )
    raw = 2.0 * unit - 1.0
    raw[0] = 0.0
    direction_specs: list[tuple[int, np.ndarray, float, str]] = []
    manual_index = candidate_count
    for anchor_index, anchor in enumerate(anchors):
        anchor_name = (
            objectives[anchor_index]
            if anchor_index < len(objectives)
            else "balanced"
        )
        for radius in RADII:
            direction_specs.append(
                (manual_index, anchor.copy(), float(radius), anchor_name)
            )
            manual_index += 1
    for candidate_index, sample in enumerate(raw):
        random_direction = nullspace @ sample
        random_direction /= max(
            float(np.max(np.abs(random_direction), initial=0.0)), 1.0e-300
        )
        anchor = anchors[candidate_index % len(anchors)]
        bias = (0.0, 0.25, 0.50, 1.0, 2.0)[candidate_index % 5]
        direction = random_direction + bias * anchor
        direction /= max(float(np.max(np.abs(direction))), 1.0e-300)
        radius = float(RADII[candidate_index % RADII.size])
        anchor_name = (
            objectives[candidate_index % len(objectives)]
            if candidate_index % len(anchors) < len(objectives)
            else "balanced"
        )
        direction_specs.append((candidate_index, direction, radius, anchor_name))

    records = []
    for candidate_index, direction, radius, anchor_name in direction_specs:
        coordinates = direction * scales * radius
        if np.any(coordinates < lower) or np.any(coordinates > upper):
            continue
        phase_coordinates = coordinates[:PHASE_COORDINATES]
        phase_delta = context.phase_map[::512] @ phase_coordinates
        delay = context.delay_sample_map[::512] @ phase_coordinates
        curvature = context.curvature_sample_map[::512] @ phase_coordinates
        if (
            np.max(np.abs(phase_delta), initial=0.0) > PHASE_DELTA_CAP_RAD * 0.95
            or np.max(np.abs(delay), initial=0.0) > GROUP_DELAY_CAP_SAMPLES * 0.95
            or np.max(np.abs(curvature), initial=0.0) > CURVATURE_CAP * 0.95
        ):
            continue
        predicted = jacobian @ coordinates
        if not _linear_safe(predicted, baseline, names):
            continue
        records.append(
            {
                "screen_index": candidate_index,
                "radius": radius,
                "anchor": anchor_name,
                "coordinates": coordinates.tolist(),
                "predicted": predicted.tolist(),
                "predicted_pre_lobe_delta_db": float(
                    predicted[index["timing/maximum_pre_lobe_db_peak"]]
                ),
                "predicted_secondary_score": float(
                    predicted[index["timing/maximum_post_lobe_db_peak"]] / 0.25
                    + predicted[index["timing/post_energy_db_total"]] / 0.10
                    + predicted[index["timing/main_lobe_width_us"]] / 2.0
                    + predicted[index["timing/step_overshoot_percent"]] / 0.50
                    + predicted[index["timing/step_undershoot_percent"]] / 0.50
                ),
            }
        )
    records.sort(
        key=lambda record: (
            record["predicted_pre_lobe_delta_db"],
            record["predicted_secondary_score"],
        )
    )
    return records, null_report


def _static_failures(
    timing: dict[str, Any], structural: dict[str, float]
) -> list[str]:
    limits = {
        "pre_energy_db_total": -4.85,
        "maximum_pre_lobe_db_peak": -18.20,
        "post_energy_db_total": -2.3738911226100226,
        "maximum_post_lobe_db_peak": -7.702214322277805,
        "main_lobe_width_us": 68.9430162564111,
        "step_overshoot_percent": 10.954176789621346,
        "step_undershoot_percent": 10.331889305015538,
        "decay_120_ms": 7.0,
    }
    failures = [
        f"timing/{metric}"
        for metric, limit in limits.items()
        if timing[metric] is None or timing[metric] > limit
    ]
    structural_limits = {
        "maximum_phase_delta_rad": PHASE_DELTA_CAP_RAD,
        "maximum_group_delay_delta_samples": GROUP_DELAY_CAP_SAMPLES,
        "maximum_group_delay_curvature_samples_per_ln_hz_squared": CURVATURE_CAP,
        "maximum_cleanup_coefficient_delta": CLEANUP_BOUND + 1.0e-12,
        "omitted_periodic_energy_ratio": 1.0e-11,
    }
    failures.extend(
        f"structural/{metric}"
        for metric, limit in structural_limits.items()
        if structural[metric] > limit
    )
    if abs(structural["cleanup_sum_delta"]) > 1.0e-12:
        failures.append("structural/cleanup_sum_delta")
    return failures


def _timing_delta(timing: dict[str, Any], baseline: dict[str, Any]) -> dict[str, Any]:
    return {
        metric: (
            None
            if value is None or baseline[metric] is None
            else float(value - baseline[metric])
        )
        for metric, value in timing.items()
    }


def _frequency_contract(
    response: np.ndarray,
    baseline_response: np.ndarray,
    family: str,
) -> tuple[dict[str, float], list[str]]:
    fft_length = 1 << max(20, (max(response.size, baseline_response.size) - 1).bit_length())
    spectrum = np.fft.rfft(response, fft_length)
    baseline = np.fft.rfft(baseline_response, fft_length)
    frequency = np.fft.rfftfreq(fft_length, 1.0 / OUTPUT_RATE_HZ)
    delta_db = 20.0 * np.log10(
        np.maximum(np.abs(spectrum), 1.0e-300)
        / np.maximum(np.abs(baseline), 1.0e-300)
    )
    normalized = np.abs(spectrum) / max(abs(spectrum[0]), 1.0e-300)
    normalized_baseline = np.abs(baseline) / max(abs(baseline[0]), 1.0e-300)
    config = FAMILIES[family]
    bands = {
        "maximum_delta_db_0_16khz": float(np.max(np.abs(delta_db[frequency <= 16_000.0]))),
        "maximum_delta_db_16_18khz": float(
            np.max(np.abs(delta_db[(frequency >= 16_000.0) & (frequency <= 18_000.0)]))
        ),
        "maximum_delta_db_18_20khz": float(
            np.max(np.abs(delta_db[(frequency >= 18_000.0) & (frequency <= 20_000.0)]))
        ),
        "maximum_boost_db_0_20khz": float(np.max(delta_db[frequency <= 20_000.0])),
        "maximum_stopband_db_22k05_nyquist": float(
            20.0
            * np.log10(
                max(float(np.max(normalized[frequency >= 22_050.0])), 1.0e-300)
            )
        ),
    }
    transition = (frequency >= 20_000.0) & (frequency <= 22_050.0)
    values = normalized[transition]
    base_values = normalized_baseline[transition]
    floor = 10.0 ** (float(config["stopband_floor_db"]) / 20.0)
    relevant = np.maximum(values[:-1], values[1:]) > floor
    base_relevant = np.maximum(base_values[:-1], base_values[1:]) > floor
    bands["maximum_transition_rebound_linear"] = float(
        np.max(np.where(relevant, np.maximum(np.diff(values), 0.0), 0.0), initial=0.0)
    )
    bands["baseline_transition_rebound_linear"] = float(
        np.max(
            np.where(base_relevant, np.maximum(np.diff(base_values), 0.0), 0.0),
            initial=0.0,
        )
    )
    failures = []
    for metric, limit in (
        ("maximum_delta_db_0_16khz", config["passband_0_16khz_db"]),
        ("maximum_delta_db_16_18khz", config["passband_16_18khz_db"]),
        ("maximum_delta_db_18_20khz", config["passband_18_20khz_db"]),
    ):
        if bands[metric] > float(limit) + 1.0e-9:
            failures.append(f"frequency/{metric}")
    if bands["maximum_boost_db_0_20khz"] > 0.001 + 1.0e-9:
        failures.append("frequency/passband_boost")
    if bands["maximum_stopband_db_22k05_nyquist"] > float(config["stopband_floor_db"]):
        failures.append("frequency/stopband")
    if bands["maximum_transition_rebound_linear"] > max(
        bands["baseline_transition_rebound_linear"] + 1.0e-8, 1.0e-8
    ):
        failures.append("frequency/transition_rebound")
    return bands, failures


def _holdout(
    response: np.ndarray, baseline_response: np.ndarray
) -> tuple[list[dict[str, Any]], list[str]]:
    cells = []
    failures = []
    for cycles in HOLDOUT_CYCLES:
        for frequency in HOLDOUT_FREQUENCIES_HZ:
            reference = measure_packet_set(baseline_response, (frequency,), cycles)
            candidate = measure_packet_set(response, (frequency,), cycles)
            cell_failures = packet_gate_failures(candidate, reference)
            identifier = f"{frequency:g}hz-{cycles:g}cycles"
            failures.extend(f"holdout/{identifier}/{failure}" for failure in cell_failures)
            cells.append(
                {
                    "identifier": identifier,
                    "frequency_hz": frequency,
                    "cycles": cycles,
                    "packets": candidate,
                    "gated_delta_db_vs_e2v3": packet_gate_deltas(candidate, reference),
                    "failures": cell_failures,
                }
            )
    return cells, failures


def campaign(
    root: Path,
    work_dir: Path,
    family: str,
    candidate_count: int,
    static_count: int,
    packet_count: int,
    holdout_count: int,
) -> dict[str, Any]:
    context = _build_context(root, family)
    sensitivity = _sensitivity(context)
    work_dir.mkdir(parents=True, exist_ok=True)
    sensitivity_report = {
        "identity": IDENTITY,
        "family": family,
        "result_names": sensitivity["result_names"],
        "result_scales": sensitivity["result_scales"].tolist(),
        "finite_difference_steps": sensitivity["steps"].tolist(),
        "jacobian": sensitivity["jacobian"].tolist(),
        "normalized_singular_values": sensitivity["normalized_singular_values"].tolist(),
        "coordinates": sensitivity["coordinates"],
    }
    (work_dir / "e3_p10_sensitivity.json").write_text(
        json.dumps(sensitivity_report, indent=2) + "\n", encoding="utf-8"
    )
    screened, nullspace = _screen(context, sensitivity, candidate_count)
    baseline_measurement = {
        "timing": context.baseline_timing,
        "packets": context.baseline_packets,
    }
    exact_static = []
    characters: dict[int, np.ndarray] = {}
    cleanups: dict[int, np.ndarray] = {}
    for screen in screened[: min(static_count, len(screened))]:
        coordinates = np.asarray(screen["coordinates"], dtype=np.float64)
        character, cleanup, structural = _realize(context, coordinates)
        response = _cascade_character_and_cleanup(character, cleanup)
        timing = asdict(_timing_metrics(response))
        failures = _static_failures(timing, structural)
        record = {
            **screen,
            "identifier": f"p10-{family}-{screen['screen_index']:05d}",
            "structural": structural,
            "timing": timing,
            "timing_delta_vs_e2v3": _timing_delta(timing, context.baseline_timing),
            "passes_static_gates": not failures,
            "static_failures": failures,
        }
        exact_static.append(record)
        if not failures:
            characters[screen["screen_index"]] = character
            cleanups[screen["screen_index"]] = cleanup

    static_safe = sorted(
        (record for record in exact_static if record["passes_static_gates"]),
        key=lambda record: (
            record["timing_delta_vs_e2v3"]["maximum_pre_lobe_db_peak"],
            record["timing_delta_vs_e2v3"]["maximum_post_lobe_db_peak"],
            record["timing_delta_vs_e2v3"]["post_energy_db_total"],
            record["timing_delta_vs_e2v3"]["main_lobe_width_us"],
        ),
    )
    exact_packets = []
    for record in static_safe[: min(packet_count, len(static_safe))]:
        character = characters[record["screen_index"]]
        cleanup = cleanups[record["screen_index"]]
        response = _cascade_character_and_cleanup(character, cleanup)
        packets = measure_packet_set(response)
        packet_failures = packet_gate_failures(packets, context.baseline_packets)
        frequency, frequency_failures = _frequency_contract(
            response, context.baseline_response, family
        )
        measured = {"timing": record["timing"], "packets": packets}
        meaningful = _meaningful(measured, baseline_measurement)
        exact_packets.append(
            {
                **record,
                "packets": packets,
                "packet_gated_delta_db_vs_e2v3": packet_gate_deltas(
                    packets, context.baseline_packets
                ),
                "packet_failures": packet_failures,
                "frequency": frequency,
                "frequency_failures": frequency_failures,
                "passes_packet_frequency_gates": not packet_failures
                and not frequency_failures,
                "meaningful": meaningful,
            }
        )
    qualified = sorted(
        (record for record in exact_packets if record["passes_packet_frequency_gates"]),
        key=lambda record: (
            not record["meaningful"]["clear_replacement_timing"],
            record["timing_delta_vs_e2v3"]["maximum_pre_lobe_db_peak"],
            -record["meaningful"]["secondary_count"],
        ),
    )

    holdouts = []
    finalist_dir = work_dir / "finalists"
    finalist_dir.mkdir(parents=True, exist_ok=True)
    for record in qualified[: min(holdout_count, len(qualified))]:
        character = characters[record["screen_index"]]
        cleanup = cleanups[record["screen_index"]]
        response = _cascade_character_and_cleanup(character, cleanup)
        cells, failures = _holdout(response, context.baseline_response)
        character_payload = np.asarray(character, dtype="<f8").tobytes()
        cleanup_payload = np.asarray(cleanup, dtype="<f8").tobytes()
        character_path = finalist_dir / f"{record['identifier']}.character.f64le"
        cleanup_path = finalist_dir / f"{record['identifier']}.cleanup1.f64le"
        character_path.write_bytes(character_payload)
        cleanup_path.write_bytes(cleanup_payload)
        holdouts.append(
            {
                **record,
                "holdout_cells": cells,
                "holdout_failures": failures,
                "passes_holdouts": not failures,
                "clear_replacement_after_holdouts": bool(
                    not failures
                    and record["meaningful"]["clear_replacement_timing"]
                    and FAMILIES[family]["production_eligible_family"]
                ),
                "character_file": str(character_path.relative_to(work_dir)).replace("\\", "/"),
                "character_sha256": _sha256_bytes(character_payload),
                "cleanup_file": str(cleanup_path.relative_to(work_dir)).replace("\\", "/"),
                "cleanup_sha256": _sha256_bytes(cleanup_payload),
            }
        )

    character_path = root / "assets/filters/split_phase_e2v3/character_full_rate.f64le"
    cleanup_path = root / "assets/filters/split_phase_e2v3/cleanup_stage_1.f64le"
    return {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "source": {
            "character": str(character_path.relative_to(root)).replace("\\", "/"),
            "character_sha256": _sha256_bytes(character_path.read_bytes()),
            "cleanup": str(cleanup_path.relative_to(root)).replace("\\", "/"),
            "cleanup_sha256": _sha256_bytes(cleanup_path.read_bytes()),
        },
        "contract": {
            "family": family,
            "family_limits": {
                key: value.tolist() if isinstance(value, np.ndarray) else value
                for key, value in FAMILIES[family].items()
            },
            "phase_coordinates": PHASE_COORDINATES,
            "phase_free_bound": PHASE_FREE_BOUND,
            "phase_delta_cap_rad": PHASE_DELTA_CAP_RAD,
            "group_delay_cap_samples": GROUP_DELAY_CAP_SAMPLES,
            "curvature_cap": CURVATURE_CAP,
            "magnitude_knots_hz": MAGNITUDE_KNOTS_HZ.tolist(),
            "cleanup_directions": CLEANUP_DIRECTIONS,
            "cleanup_bound": CLEANUP_BOUND,
            "packet": packet_contract(),
            "candidate_count": candidate_count,
            "static_count": static_count,
            "packet_count": packet_count,
            "holdout_count": holdout_count,
            "holdout_frequencies_hz": HOLDOUT_FREQUENCIES_HZ,
            "holdout_cycles": HOLDOUT_CYCLES,
            "clear_replacement": (
                "at least 2 dB maximum-pre-lobe improvement plus at least three "
                "secondary timing thresholds, all exact gates and all holdouts"
            ),
        },
        "baseline": {
            "timing": context.baseline_timing,
            "packets": context.baseline_packets,
            "frequency": context.baseline_frequency,
        },
        "sensitivity": {
            "file": "e3_p10_sensitivity.json",
            "coordinate_count": int(sensitivity["jacobian"].shape[1]),
            "result_count": int(sensitivity["jacobian"].shape[0]),
            "normalized_singular_values": sensitivity["normalized_singular_values"].tolist(),
        },
        "packet_nullspace": nullspace,
        "screened_linear_safe_count": len(screened),
        "exact_static": exact_static,
        "exact_packets": exact_packets,
        "qualified": qualified,
        "holdout_finalists": holdouts,
        "summary": {
            "screened_linear_safe_count": len(screened),
            "exact_static_count": len(exact_static),
            "exact_static_safe_count": len(static_safe),
            "exact_packet_count": len(exact_packets),
            "packet_frequency_safe_count": len(qualified),
            "clear_replacement_before_holdouts": sum(
                record["meaningful"]["clear_replacement_timing"] for record in qualified
            ),
            "holdout_finalist_count": len(holdouts),
            "clear_replacement_after_holdouts": sum(
                record["clear_replacement_after_holdouts"] for record in holdouts
            ),
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Run the P10 high-dimensional windowed-packet joint timing search"
    )
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[2]
    )
    parser.add_argument(
        "--work-dir",
        type=Path,
        default=Path(__file__).resolve().parent / "work-e3-p10/joint-moderate",
    )
    parser.add_argument("--family", choices=tuple(FAMILIES), default="moderate")
    parser.add_argument("--candidate-count", type=int, default=8_192)
    parser.add_argument("--static-count", type=int, default=384)
    parser.add_argument("--packet-count", type=int, default=96)
    parser.add_argument("--holdout-count", type=int, default=12)
    arguments = parser.parse_args()
    report = campaign(
        arguments.root.resolve(),
        arguments.work_dir.resolve(),
        arguments.family,
        arguments.candidate_count,
        arguments.static_count,
        arguments.packet_count,
        arguments.holdout_count,
    )
    output = arguments.work_dir / "e3_p10_joint_search.json"
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"output": str(output), **report["summary"]}, indent=2))


if __name__ == "__main__":
    main()
