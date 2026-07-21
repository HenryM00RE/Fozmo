from __future__ import annotations

import argparse
import hashlib
import json
import math
from dataclasses import asdict
from pathlib import Path
from typing import Any, Callable

import numpy as np
from scipy import stats

from .e3_p7_cleanup_search import _frequency_metrics
from .e3_p7_counterfactual import (
    INTERVALS_MS,
    cleanup_counterfactual_residual,
    default_training_fixtures,
    interval_metrics,
)
from .e3_p7_magnitude_sensitivity import (
    CHARACTER_RATE_HZ,
    CONTROL_FREQUENCIES_HZ,
    FAMILY_BOUNDS_DB,
    FFT_LENGTH,
    _basis,
    _realize_character,
)
from .e3_phase_search import _cascade_character_and_cleanup, _read_f64le, _timing_metrics
from .evaluate_e3_packets import PACKET_FREQUENCIES_HZ, _measure_packet


IDENTITY = "SplitPhase128kE3-P7-bounded-magnitude-screen"
OUTPUT_RATE_HZ = 176_400
TOLERANCE_RMS = 2.0e-9
RADIAL_SCALES = (0.05, 0.10, 0.20, 0.35, 0.50, 0.75, 1.0)
EXACT_STATIC_COUNT = 128
COUNTERFACTUAL_COUNT = 48
FINALIST_COUNT = 12


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _screen_controls(count: int, lower: np.ndarray, upper: np.ndarray) -> np.ndarray:
    if count <= 0 or count & (count - 1):
        raise ValueError("screen count must be a positive power of two")
    samples = stats.qmc.Sobol(d=lower.size, scramble=False).random_base2(
        int(math.log2(count))
    )
    controls = lower + samples * (upper - lower)
    for index in range(count):
        controls[index] *= RADIAL_SCALES[index % len(RADIAL_SCALES)]
    controls[0] = 0.0
    return controls


def _relevant_transition_rebound(
    response: np.ndarray, stop_floor_db: float = -150.0
) -> float:
    spectrum = np.fft.rfft(response, FFT_LENGTH)
    frequency = np.fft.rfftfreq(FFT_LENGTH, 1.0 / OUTPUT_RATE_HZ)
    magnitude = np.abs(spectrum) / max(abs(spectrum[0]), 1.0e-300)
    active = (frequency >= 20_000.0) & (frequency <= 22_050.0)
    values = magnitude[active]
    floor = 10.0 ** (stop_floor_db / 20.0)
    differences = np.diff(values)
    relevant = np.maximum(values[:-1], values[1:]) > floor
    return float(np.max(np.where(relevant, np.maximum(differences, 0.0), 0.0), initial=0.0))


def _screen_monotone(
    baseline_magnitude: np.ndarray,
    basis: np.ndarray,
    controls: np.ndarray,
    baseline_rebound: float,
) -> bool:
    delta_db = basis @ controls
    candidate = baseline_magnitude * np.power(10.0, delta_db / 20.0)
    floor = 10.0 ** (-150.0 / 20.0)
    differences = np.diff(candidate)
    relevant = np.maximum(candidate[:-1], candidate[1:]) > floor
    rebound = float(
        np.max(np.where(relevant, np.maximum(differences, 0.0), 0.0), initial=0.0)
    )
    return rebound <= baseline_rebound + 1.0e-12


def _predicted_guards(
    predicted: np.ndarray, names: list[str], baseline: np.ndarray
) -> bool:
    values = baseline + predicted
    by_name = dict(zip(names, values, strict=True))
    if by_name["timing/maximum_pre_lobe_db_peak"] > -22.5:
        return False
    if by_name["timing/pre_energy_db_total"] > -4.85:
        return False
    if by_name["timing/main_lobe_width_us"] > 62.5:
        return False
    if by_name["timing/step_overshoot_percent"] > 9.22:
        return False
    if by_name["timing/decay_120_ms"] > 7.0:
        return False
    if by_name["frequency/maximum_stopband_db_22k05_nyquist"] > -150.0:
        return False
    for name, value, reference in zip(names, values, baseline, strict=True):
        if name.startswith("packet/") and value - reference > 0.10:
            return False
    counter = [predicted[index] for index, name in enumerate(names) if name.startswith("counterfactual/")]
    return max(counter) <= 0.10


def _ranking_functions(names: list[str], baseline: np.ndarray, jacobian: np.ndarray) -> list[tuple[str, Callable[[dict[str, Any]], tuple[float, ...]]]]:
    index = {name: offset for offset, name in enumerate(names)}
    counter = [offset for name, offset in index.items() if name.startswith("counterfactual/")]

    def movement(record: dict[str, Any], name: str) -> float:
        return float(record["predicted"][index[name]])

    return [
        ("transition_worst", lambda record: (record["predicted_counterfactual_worst_db"], record["predicted_counterfactual_mean_db"])),
        ("transition_mean", lambda record: (record["predicted_counterfactual_mean_db"], record["predicted_counterfactual_worst_db"])),
        ("post_lobe", lambda record: (movement(record, "timing/maximum_post_lobe_db_peak"), record["predicted_counterfactual_worst_db"])),
        ("post_energy", lambda record: (movement(record, "timing/post_energy_db_total"), record["predicted_counterfactual_worst_db"])),
        ("undershoot", lambda record: (movement(record, "timing/step_undershoot_percent"), record["predicted_counterfactual_worst_db"])),
        ("width", lambda record: (movement(record, "timing/main_lobe_width_us"), record["predicted_counterfactual_worst_db"])),
        (
            "balanced",
            lambda record: (
                record["predicted_counterfactual_worst_db"] / 0.03
                + movement(record, "timing/maximum_post_lobe_db_peak") / 0.05
                + movement(record, "timing/post_energy_db_total") / 0.02
                + movement(record, "timing/step_undershoot_percent") / 0.05,
            ),
        ),
    ]


def _select_diverse(
    records: list[dict[str, Any]],
    ranking: list[tuple[str, Callable[[dict[str, Any]], tuple[float, ...]]]],
    count: int,
) -> list[dict[str, Any]]:
    selected: dict[str, dict[str, Any]] = {}
    per_key = max(8, math.ceil(count / len(ranking)))
    for ranking_name, key in ranking:
        for record in sorted(records, key=key)[:per_key]:
            record.setdefault("selected_by", []).append(ranking_name)
            selected[record["identifier"]] = record
    if len(selected) < count:
        for record in sorted(records, key=ranking[0][1]):
            selected.setdefault(record["identifier"], record)
            if len(selected) >= count:
                break
    return list(selected.values())[:count]


def search(root: Path, work_dir: Path, per_family: int) -> dict[str, Any]:
    sensitivity_path = root / "tools/split_phase_v6/work-e3-p7-magnitude/e3_p7_magnitude_sensitivity.json"
    if not sensitivity_path.is_file():
        raise RuntimeError("run e3_p7_magnitude_sensitivity before the magnitude screen")
    sensitivity = json.loads(sensitivity_path.read_text(encoding="utf-8"))
    names = list(sensitivity["result_names"])
    baseline_vector = np.asarray(
        [
            # Reconstruct in the same serialized order used by the sensitivity report.
            next(
                interval["residual_rms_dbfs"]
                for fixture, intervals in sensitivity["baseline"]["counterfactual"].items()
                for interval in intervals[:2]
                if name == f"counterfactual/{fixture}/{interval['start_ms']:.0f}-{interval['end_ms']:.0f}ms"
            )
            if name.startswith("counterfactual/")
            else (
                sensitivity["baseline"]["timing"][name.split("/", 1)[1]]
                if name.startswith("timing/")
                else (
                    sensitivity["baseline"]["packets"][name.split("/")[1]][
                        "onset_pre_echo_energy_db_total"
                    ]
                    if name.startswith("packet/")
                    else sensitivity["baseline"]["frequency"][name.split("/", 1)[1]]
                )
            )
            for name in names
        ],
        dtype=np.float64,
    )
    jacobian = np.asarray(sensitivity["jacobian_per_control_db"], dtype=np.float64)
    character_path = root / "tools/split_phase_v6/baselines/e3-p6d-local-0145.f64le"
    e2_path = root / "assets/filters/split_phase_e2v3/character_full_rate.f64le"
    cleanup_path = root / "assets/filters/split_phase_e2v3/cleanup_stage_1.f64le"
    character = _read_f64le(character_path)
    e2_character = _read_f64le(e2_path)
    cleanup = _read_f64le(cleanup_path)
    anchor_spectrum = np.fft.rfft(character, FFT_LENGTH)
    frequency = np.fft.rfftfreq(FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ)
    magnitude_basis = _basis(frequency)
    anchor_sum = float(math.fsum(float(value) for value in character))
    baseline_response = _cascade_character_and_cleanup(character, cleanup)
    baseline_frequency = _frequency_metrics(baseline_response, baseline_response)
    baseline_rebound = _relevant_transition_rebound(baseline_response)

    coarse_frequency = np.linspace(20_000.0, 22_050.0, 4097)
    coarse_basis = _basis(coarse_frequency)
    anchor_full_spectrum = np.fft.rfft(baseline_response, FFT_LENGTH)
    anchor_full_frequency = np.fft.rfftfreq(FFT_LENGTH, 1.0 / OUTPUT_RATE_HZ)
    coarse_magnitude = np.interp(
        coarse_frequency,
        anchor_full_frequency,
        np.abs(anchor_full_spectrum) / abs(anchor_full_spectrum[0]),
    )
    coarse_rebound = _relevant_transition_rebound(baseline_response)
    ranking = _ranking_functions(names, baseline_vector, jacobian)

    screened: list[dict[str, Any]] = []
    for family, (lower, upper) in FAMILY_BOUNDS_DB.items():
        controls_set = _screen_controls(per_family, lower, upper)
        for index, controls in enumerate(controls_set):
            predicted = jacobian @ controls
            if not _predicted_guards(predicted, names, baseline_vector):
                continue
            if not _screen_monotone(
                coarse_magnitude, coarse_basis, controls, coarse_rebound
            ):
                continue
            counter = [
                predicted[offset]
                for offset, name in enumerate(names)
                if name.startswith("counterfactual/")
            ]
            screened.append(
                {
                    "identifier": f"mag-{family}-{index:04d}",
                    "family": family,
                    "sobol_index": index,
                    "controls_db": controls.tolist(),
                    "predicted": predicted.tolist(),
                    "predicted_counterfactual_worst_db": float(max(counter)),
                    "predicted_counterfactual_mean_db": float(np.mean(counter)),
                }
            )

    selected = _select_diverse(screened, ranking, EXACT_STATIC_COUNT)
    exact_static: list[dict[str, Any]] = []
    characters: dict[str, np.ndarray] = {}
    baseline_packets = sensitivity["baseline"]["packets"]
    for record in selected:
        controls = np.asarray(record["controls_db"], dtype=np.float64)
        candidate, realization = _realize_character(
            character, anchor_spectrum, magnitude_basis, controls, anchor_sum
        )
        response = _cascade_character_and_cleanup(candidate, cleanup)
        timing = asdict(_timing_metrics(response))
        frequency_metrics = _frequency_metrics(response, baseline_response)
        packets = {
            str(int(frequency_hz)): asdict(_measure_packet(response, frequency_hz))
            for frequency_hz in PACKET_FREQUENCIES_HZ
        }
        packet_delta = {
            key: packets[key]["onset_pre_echo_energy_db_total"]
            - baseline_packets[key]["onset_pre_echo_energy_db_total"]
            for key in packets
        }
        passband_limit = 2.1e-4 if record["family"] == "neutral" else 1.01e-3
        relevant_rebound = _relevant_transition_rebound(response)
        passes = bool(
            timing["maximum_pre_lobe_db_peak"] <= -22.5
            and timing["pre_energy_db_total"] <= -4.85
            and timing["main_lobe_width_us"] <= 62.5
            and timing["step_overshoot_percent"] <= 9.22
            and timing["decay_120_ms"] is not None
            and timing["decay_120_ms"] <= 7.0
            and max(packet_delta.values()) <= 0.10
            and frequency_metrics["maximum_passband_delta_db_0_18khz"] <= passband_limit
            and frequency_metrics["maximum_stopband_db_22k05_nyquist"] <= -150.0
            and relevant_rebound <= baseline_rebound + 1.0e-12
        )
        exact = {
            **record,
            "realization": realization,
            "timing": timing,
            "packets": packets,
            "packet_delta_db_vs_p6": packet_delta,
            "frequency": frequency_metrics,
            "relevant_transition_rebound_linear": relevant_rebound,
            "passes_static_packet_frequency_gates": passes,
        }
        exact_static.append(exact)
        if passes:
            characters[record["identifier"]] = candidate

    static_safe = [record for record in exact_static if record["passes_static_packet_frequency_gates"]]
    counter_selected = _select_diverse(static_safe, ranking, COUNTERFACTUAL_COUNT)
    fixtures = default_training_fixtures()
    reference_residuals = {
        fixture.name: cleanup_counterfactual_residual(e2_character, cleanup, fixture)
        for fixture in fixtures
    }
    incumbent_residuals = {
        fixture.name: cleanup_counterfactual_residual(character, cleanup, fixture)
        for fixture in fixtures
    }
    counterfactual_records = []
    for record in counter_selected:
        candidate = characters[record["identifier"]]
        fixture_reports = []
        first_two_rms_delta = []
        first_two_excess_ratio = []
        for fixture in fixtures:
            residual = cleanup_counterfactual_residual(candidate, cleanup, fixture)
            measured = interval_metrics(residual)
            incumbent_measured = interval_metrics(incumbent_residuals[fixture.name])
            reference = reference_residuals[fixture.name]
            incumbent = incumbent_residuals[fixture.name]
            tolerance_power = TOLERANCE_RMS**2
            for interval_index, ((start_ms, end_ms), interval, incumbent_interval) in enumerate(
                zip(INTERVALS_MS, measured, incumbent_measured, strict=True)
            ):
                start = round(start_ms * OUTPUT_RATE_HZ / 1000.0)
                end = round(end_ms * OUTPUT_RATE_HZ / 1000.0)
                candidate_excess = float(
                    np.sum(np.maximum(residual[start:end] ** 2 - reference[start:end] ** 2 - tolerance_power, 0.0))
                ) / OUTPUT_RATE_HZ
                incumbent_excess = float(
                    np.sum(np.maximum(incumbent[start:end] ** 2 - reference[start:end] ** 2 - tolerance_power, 0.0))
                ) / OUTPUT_RATE_HZ
                interval["delta_db_vs_p6"] = (
                    interval["residual_rms_dbfs"]
                    - incumbent_interval["residual_rms_dbfs"]
                )
                interval["positive_excess_ratio_vs_p6"] = (
                    candidate_excess / incumbent_excess if incumbent_excess > 0.0 else None
                )
                if interval_index < 2:
                    first_two_rms_delta.append(interval["delta_db_vs_p6"])
                    if interval["positive_excess_ratio_vs_p6"] is not None:
                        first_two_excess_ratio.append(interval["positive_excess_ratio_vs_p6"])
            fixture_reports.append({"fixture": fixture.name, "intervals": measured})
        counterfactual_records.append(
            {
                **record,
                "counterfactual_fixtures": fixture_reports,
                "worst_0_5ms_rms_delta_db_vs_p6": float(max(first_two_rms_delta)),
                "mean_0_5ms_rms_delta_db_vs_p6": float(np.mean(first_two_rms_delta)),
                "worst_0_5ms_positive_excess_ratio_vs_p6": float(max(first_two_excess_ratio)),
                "mean_0_5ms_positive_excess_ratio_vs_p6": float(np.mean(first_two_excess_ratio)),
            }
        )

    counterfactual_records.sort(
        key=lambda record: (
            record["worst_0_5ms_positive_excess_ratio_vs_p6"],
            record["mean_0_5ms_rms_delta_db_vs_p6"],
            record["timing"]["maximum_post_lobe_db_peak"],
            record["timing"]["post_energy_db_total"],
        )
    )
    finalist_dir = work_dir / "finalists"
    finalist_dir.mkdir(parents=True, exist_ok=True)
    finalists = counterfactual_records[:FINALIST_COUNT]
    for record in finalists:
        payload = np.asarray(characters[record["identifier"]], dtype="<f8").tobytes()
        path = finalist_dir / f"{record['identifier']}.f64le"
        path.write_bytes(payload)
        record["character_file"] = str(path.relative_to(work_dir)).replace("\\", "/")
        record["character_sha256"] = _sha256_bytes(payload)

    report = {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "source": sensitivity["source"],
        "screen": {
            "per_family": per_family,
            "candidate_count": per_family * len(FAMILY_BOUNDS_DB),
            "radial_scales": RADIAL_SCALES,
            "linear_and_monotone_safe_count": len(screened),
            "exact_static_count": len(exact_static),
            "exact_static_safe_count": len(static_safe),
            "counterfactual_count": len(counterfactual_records),
            "finalist_count": len(finalists),
        },
        "contracts": {
            "families": sensitivity["parameterization"]["families"],
            "control_frequencies_hz": CONTROL_FREQUENCIES_HZ.tolist(),
            "monotonicity": "no new positive transition rebound above the -150 dB absolute floor",
            "counterfactual_reference": "E2v3 pointwise power with 2e-9 RMS tolerance",
            "incumbent": "p6d-local-0145",
        },
        "baseline": {
            "frequency": baseline_frequency,
            "relevant_transition_rebound_linear": baseline_rebound,
        },
        "screened": screened,
        "exact_static": exact_static,
        "counterfactual": counterfactual_records,
        "finalists": finalists,
    }
    work_dir.mkdir(parents=True, exist_ok=True)
    (work_dir / "e3_p7_magnitude_search.json").write_text(
        json.dumps(report, indent=2) + "\n", encoding="utf-8"
    )
    return report


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--per-family", type=int, default=4096)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    work_dir = (
        arguments.work_dir or root / "tools/split_phase_v6/work-e3-p7-magnitude-screen"
    ).resolve()
    report = search(root, work_dir, arguments.per_family)
    print(json.dumps(report["screen"], indent=2))


if __name__ == "__main__":
    main()
