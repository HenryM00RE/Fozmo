from __future__ import annotations

import argparse
import hashlib
import json
import math
import shutil
from pathlib import Path
from typing import Any

import numpy as np

from .e3_p10_joint_search import _holdout
from .e3_p10_packet_contract import packet_gate_failures
from .e3_p12_gaussian_phase_search import evaluate_exact
from .e3_phase_search import _cascade_character_and_cleanup, _read_f64le


IDENTITY = "SplitPhase128kE3-P17-definitive-replacement-freeze"
SELECTED_IDENTIFIER = "p17-balanced"
CHARACTER_SHA256 = "c8dd84a905e188df39f0aa714cb8cd89cf91e99e9c87881bbe8aac8f6c11f8c4"
UNEXPECTED_SPUR_FLOOR_DBFS = -190.0
STEADY_RESIDUAL_REGRESSION_TOLERANCE_DB = 2.0

NATIVE_TO_EXACT_TIMING = {
    "pre_peak_energy_db_total": "pre_energy_db_total",
    "maximum_pre_ringing_lobe_db_peak": "maximum_pre_lobe_db_peak",
    "post_peak_energy_db_total": "post_energy_db_total",
    "maximum_post_ringing_lobe_db_peak": "maximum_post_lobe_db_peak",
    "main_lobe_width_us": "main_lobe_width_us",
    "step_response_overshoot_percent": "step_overshoot_percent",
    "step_response_undershoot_percent": "step_undershoot_percent",
    "decay_time_to_minus_120_db_ms": "decay_120_ms",
}


def _load(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _write_json_lf(path: Path, value: Any) -> None:
    path.write_bytes((json.dumps(value, indent=2) + "\n").encode("utf-8"))


def _strip_volatile(value: Any) -> Any:
    if isinstance(value, dict):
        return {
            key: _strip_volatile(item)
            for key, item in value.items()
            if key not in {"render_seconds"}
        }
    if isinstance(value, list):
        return [_strip_volatile(item) for item in value]
    return value


def _canonical_sha256(value: Any) -> str:
    payload = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(payload).hexdigest()


def _native_filters(report: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {record["filter"]: record for record in report["filters"]}


def _native_timing(record: dict[str, Any]) -> dict[str, float]:
    impulse = record["impulse"]
    return {
        exact_name: float(impulse[native_name])
        for native_name, exact_name in NATIVE_TO_EXACT_TIMING.items()
    }


def _timing_delta(
    candidate: dict[str, float], reference: dict[str, float]
) -> dict[str, float]:
    return {
        metric: float(candidate[metric] - reference[metric]) for metric in candidate
    }


def _assert_timing_dominance(
    candidate: dict[str, float], reference: dict[str, float], label: str
) -> None:
    failures = [
        metric
        for metric, value in candidate.items()
        if value >= reference[metric] - 1.0e-12
    ]
    if failures:
        raise RuntimeError(f"{label} no longer dominates E2v3: {failures}")


def _packet_mapping(record: dict[str, Any]) -> dict[str, dict[str, float]]:
    return {
        str(int(packet["frequency_hz"])): packet
        for packet in record["tone_packets"]
    }


def _stress_spectral(report: dict[str, Any]) -> dict[str, dict[str, float]]:
    result: dict[str, dict[str, float]] = {}
    for cell in report["cells"]:
        if cell["measurements"]["kind"] != "high_frequency_stress":
            continue
        for channel in cell["measurements"]["channels"]:
            steady = channel["steady"]
            key = f"{cell['scenario']}/{cell['modulator']}/{channel['channel']}"
            result[key] = {
                "residual_excluding_declared_products_dbfs": float(
                    steady["residual_excluding_declared_products_dbfs"]
                ),
                "worst_unexpected_spur_dbfs": float(
                    steady["worst_unexpected_spur"]["level_dbfs"]
                ),
            }
    return result


def _matched_transition_excess(report: dict[str, Any]) -> dict[str, float]:
    result: dict[str, float] = {}
    for cell in report["cells"]:
        if cell["scenario"] != "high_frequency_matched_stress":
            continue
        for channel in cell["measurements"]["channels"]:
            excess = channel.get("transition_envelope_excess_vs_reference")
            if excess is None:
                continue
            key = f"{cell['modulator']}/{channel['channel']}"
            result[key] = float(
                excess["total_positive_excess_power_linear_seconds"]
            )
    return result


def _cell_hashes(report: dict[str, Any]) -> dict[str, list[str]]:
    return {
        f"{cell['scenario']}/{cell['dsd_rate']}/{cell['modulator']}": list(
            cell["native_dsd_sha256"]
        )
        for cell in report["cells"]
    }


def _quality_scalars(cell: dict[str, Any]) -> list[dict[str, Any]]:
    measurements = cell["measurements"]
    kind = measurements["kind"]
    rows: list[dict[str, Any]] = []

    def add(name: str, value: float, direction: str) -> None:
        rows.append({"metric": name, "value": float(value), "direction": direction})

    if kind == "level_sweep":
        for segment in measurements["segments"]:
            for channel in segment["channels"]:
                metrics = channel["metrics"]
                prefix = f"{segment['name']}/{channel['channel']}"
                add(f"{prefix}/sinad_db", metrics["sinad_db"], "higher")
                add(
                    f"{prefix}/residual_noise_dbfs",
                    metrics["residual_noise_dbfs"],
                    "lower",
                )
                add(
                    f"{prefix}/worst_nonharmonic_spur_dbfs",
                    metrics["worst_nonharmonic_spur"]["level_dbfs"],
                    "lower",
                )
                add(
                    f"{prefix}/absolute_gain_error_db",
                    abs(metrics["carrier"]["gain_error_db"]),
                    "lower",
                )
    elif kind == "idle_tiny_signal":
        for section in measurements["sections"]:
            for channel in section["channels"]:
                prefix = f"{section['name']}/{channel['channel']}"
                add(
                    f"{prefix}/integrated_noise_dbfs",
                    channel["noise"]["integrated_noise_dbfs"],
                    "lower",
                )
                add(
                    f"{prefix}/worst_spur_dbfs",
                    channel["noise"]["worst_spur"]["level_dbfs"],
                    "lower",
                )
    elif kind == "hires_reconstruction":
        for channel in measurements["channels"]:
            for carrier in channel["metrics"]["carriers"]:
                add(
                    f"{channel['channel']}/{carrier['name']}/absolute_gain_error_db",
                    abs(carrier["gain_error_db"]),
                    "lower",
                )
            for band in channel["metrics"]["bands"]:
                prefix = (
                    f"{channel['channel']}/{band['low_hz']:g}-"
                    f"{band['high_hz']:g}hz"
                )
                add(f"{prefix}/residual_dbfs", band["residual_dbfs"], "lower")
                add(
                    f"{prefix}/worst_unexpected_spur_dbfs",
                    band["worst_unexpected_spur"]["level_dbfs"],
                    "lower",
                )
    elif kind == "high_frequency_stress":
        for channel in measurements["channels"]:
            steady = channel["steady"]
            prefix = channel["channel"]
            add(f"{prefix}/sinad_db", steady["sinad_db"], "higher")
            add(
                f"{prefix}/residual_excluding_products_dbfs",
                steady["residual_excluding_declared_products_dbfs"],
                "lower",
            )
            add(
                f"{prefix}/worst_unexpected_spur_dbfs",
                steady["worst_unexpected_spur"]["level_dbfs"],
                "lower",
            )
    return rows


def _cross_rate_summary(
    candidate: dict[str, Any], reference: dict[str, Any]
) -> dict[str, Any]:
    for label, report in (("candidate", candidate), ("E2v3", reference)):
        if report["hard_failure_count"] or report["diagnostic_hard_failure_count"]:
            raise RuntimeError(f"{label} cross-rate DSD run has structural failures")
        if report["execution_failures"]:
            raise RuntimeError(f"{label} cross-rate DSD run has execution failures")

    reference_cells = {
        (cell["scenario"], cell["dsd_rate"], cell["modulator"]): cell
        for cell in reference["cells"]
    }
    comparisons = []
    for cell in candidate["cells"]:
        key = (cell["scenario"], cell["dsd_rate"], cell["modulator"])
        reference_cell = reference_cells.get(key)
        if reference_cell is None:
            raise RuntimeError(f"missing E2v3 cross-rate cell {key}")
        candidate_scalars = {row["metric"]: row for row in _quality_scalars(cell)}
        reference_scalars = {
            row["metric"]: row for row in _quality_scalars(reference_cell)
        }
        scalar_rows = []
        for metric, row in candidate_scalars.items():
            baseline = reference_scalars[metric]["value"]
            scalar_rows.append(
                {
                    **row,
                    "e2v3": baseline,
                    "delta_vs_e2v3": float(row["value"] - baseline),
                }
            )
        comparisons.append(
            {
                "scenario": key[0],
                "dsd_rate": key[1],
                "modulator": key[2],
                "candidate_native_dsd_sha256": cell["native_dsd_sha256"],
                "e2v3_native_dsd_sha256": reference_cell["native_dsd_sha256"],
                "quality_scalars": scalar_rows,
            }
        )

    return {
        "selected_rates": sorted(
            {
                int(cell["dsd_rate"].removeprefix("DSD"))
                for cell in candidate["cells"]
            }
        ),
        "selected_modulators": candidate["selected_modulators"],
        "candidate_cell_count": len(candidate["cells"]),
        "e2v3_cell_count": len(reference["cells"]),
        "candidate_hard_failure_count": candidate["hard_failure_count"],
        "candidate_diagnostic_hard_failure_count": candidate[
            "diagnostic_hard_failure_count"
        ],
        "e2v3_hard_failure_count": reference["hard_failure_count"],
        "e2v3_diagnostic_hard_failure_count": reference[
            "diagnostic_hard_failure_count"
        ],
        "comparisons": comparisons,
    }


def freeze(root: Path, output_dir: Path) -> dict[str, Any]:
    work = root / "tools/split_phase_v6/work-e3-p17"
    p16_work = root / "tools/split_phase_v6/work-e3-p16"
    paths = {
        "seed_search": p16_work / "dsd-calibrated-final/report.json",
        "search": work / "cross-rate-repair/e3_p17_cross_rate_packet_repair.json",
        "character": work / "cross-rate-repair/p17-balanced.character.f64le",
        "native_a": work / "native-a/report.json",
        "native_b": work / "native-b/report.json",
        "group_delay_a": work / "native-a/group-delay.csv",
        "group_delay_b": work / "native-b/group-delay.csv",
        "native_48k": work / "native-48k/report.json",
        "dsd_a": work / "dsd128-a/dsd-public-quality.json",
        "dsd_b": work / "dsd128-b/dsd-public-quality.json",
        "dsd_p6": root
        / "tools/split_phase_v6/baselines/e3-p6-freeze-dsd128.json",
        "dsd_e2v3": root
        / "tools/split_phase_v6/baselines/e3-p5-transition-envelope-e2v3-dsd128.json",
        "dsd_full_candidate": work
        / "dsd-full-rates-candidate/dsd-public-quality.json",
        "dsd_full_e2v3": p16_work
        / "dsd-full-rates-e2v3/dsd-public-quality.json",
        "external": root
        / "tools/filter_timing/baselines/external-product-static-filters-pcm24.json",
    }
    missing = [str(path) for path in paths.values() if not path.is_file()]
    if missing:
        raise RuntimeError(f"missing P17 freeze inputs: {missing}")
    if _sha256(paths["character"]) != CHARACTER_SHA256:
        raise RuntimeError("selected P17 character hash changed")

    search = _load(paths["search"])
    profiles = search["profiles"]
    selected = next(
        record
        for record in (profiles if isinstance(profiles, list) else [profiles])
        if record["identifier"] == SELECTED_IDENTIFIER
    )
    if selected["identifier"] != SELECTED_IDENTIFIER:
        raise RuntimeError("P17 selected identifier changed")
    if selected["character_sha256"] != CHARACTER_SHA256:
        raise RuntimeError("P17 report character hash changed")
    if not selected.get("passes_exact_cross_rate_packet_gates", False):
        raise RuntimeError("P17 report no longer passes the cross-rate packet gates")
    specifications = tuple(
        tuple(float(value) for value in specification)
        for specification in search["contract"]["basis_specifications"]
    )
    replay, replay_character = evaluate_exact(
        root,
        np.asarray(selected["controls"], dtype=np.float64),
        basis_specifications=specifications,
    )
    if replay["character_sha256"] != CHARACTER_SHA256:
        raise RuntimeError("P17 exact replay did not reproduce the frozen character")
    if not np.array_equal(
        replay_character,
        np.fromfile(paths["character"], dtype="<f8"),
    ):
        raise RuntimeError(
            "P17 exact replay coefficients differ from the selected asset"
        )
    if not replay["passes_exact_static_packet_frequency_gates"]:
        raise RuntimeError(
            "P17 exact replay no longer passes static/packet/frequency gates"
        )

    asset_dir = root / "assets/filters/split_phase_e2v3"
    baseline_character = _read_f64le(asset_dir / "character_full_rate.f64le")
    cleanup = _read_f64le(asset_dir / "cleanup_stage_1.f64le")
    response = _cascade_character_and_cleanup(replay_character, cleanup)
    baseline_response = _cascade_character_and_cleanup(baseline_character, cleanup)
    holdout_cells, holdout_failures = _holdout(response, baseline_response)
    if holdout_failures:
        raise RuntimeError(f"P17 holdout packet failures: {holdout_failures}")

    native_a = _load(paths["native_a"])
    native_b = _load(paths["native_b"])
    if paths["native_a"].read_bytes() != paths["native_b"].read_bytes():
        raise RuntimeError("P17 native timing reruns differ")
    if paths["group_delay_a"].read_bytes() != paths["group_delay_b"].read_bytes():
        raise RuntimeError("P17 native group-delay reruns differ")
    native_by_filter = _native_filters(native_a)
    native_e2 = _native_timing(native_by_filter["SplitPhase128kE2v3"])
    native_candidate = _native_timing(native_by_filter["SplitPhase128kE3"])
    _assert_timing_dominance(native_candidate, native_e2, "44.1 kHz P17")
    for metric, exact_value in replay["timing"].items():
        if metric in native_candidate and not math.isclose(
            native_candidate[metric], float(exact_value), abs_tol=1.0e-9
        ):
            raise RuntimeError(f"native/exact P17 timing mismatch for {metric}")

    native_48k = _load(paths["native_48k"])
    native_48_by_filter = _native_filters(native_48k)
    native_48_e2_record = native_48_by_filter["SplitPhase128kE2v3"]
    native_48_candidate_record = native_48_by_filter["SplitPhase128kE3"]
    native_48_e2 = _native_timing(native_48_e2_record)
    native_48_candidate = _native_timing(native_48_candidate_record)
    _assert_timing_dominance(native_48_candidate, native_48_e2, "48 kHz P17")
    packet_48_failures = packet_gate_failures(
        _packet_mapping(native_48_candidate_record),
        _packet_mapping(native_48_e2_record),
    )
    if packet_48_failures:
        raise RuntimeError(f"48 kHz packet failures: {packet_48_failures}")
    if native_48_candidate_record["runtime"] != native_48_e2_record["runtime"]:
        raise RuntimeError("P17 changes the 48 kHz runtime path or memory contract")

    dsd_a = _load(paths["dsd_a"])
    dsd_b = _load(paths["dsd_b"])
    if _strip_volatile(dsd_a) != _strip_volatile(dsd_b):
        raise RuntimeError("P17 DSD128 reruns differ outside render timing")
    if _cell_hashes(dsd_a) != _cell_hashes(dsd_b):
        raise RuntimeError("P17 DSD128 native hashes are not deterministic")
    if dsd_a["hard_failure_count"] or dsd_a["execution_failures"]:
        raise RuntimeError("P17 DSD128 has structural or execution failures")

    p6_dsd = _load(paths["dsd_p6"])
    final_excess = _matched_transition_excess(dsd_a)
    p6_excess = _matched_transition_excess(p6_dsd)
    if set(final_excess) != set(p6_excess):
        raise RuntimeError("P17/P6 matched transition cell sets differ")
    transition_delta = {
        key: float(final_excess[key] - p6_excess[key]) for key in final_excess
    }
    if any(delta >= 0.0 for delta in transition_delta.values()):
        raise RuntimeError("P17 no longer improves every matched P6 DSD restart cell")

    e2_dsd = _load(paths["dsd_e2v3"])
    final_spectral = _stress_spectral(dsd_a)
    e2_spectral = _stress_spectral(e2_dsd)
    spectral_delta = {}
    for key, values in final_spectral.items():
        reference = e2_spectral[key]
        residual_delta = float(
            values["residual_excluding_declared_products_dbfs"]
            - reference["residual_excluding_declared_products_dbfs"]
        )
        spectral_delta[key] = {
            "residual_delta_db_vs_e2v3": residual_delta,
            "worst_unexpected_spur_delta_db_vs_e2v3": float(
                values["worst_unexpected_spur_dbfs"]
                - reference["worst_unexpected_spur_dbfs"]
            ),
        }
        if residual_delta > STEADY_RESIDUAL_REGRESSION_TOLERANCE_DB:
            raise RuntimeError(f"material steady DSD residual regression in {key}")
        if values["worst_unexpected_spur_dbfs"] > UNEXPECTED_SPUR_FLOOR_DBFS:
            raise RuntimeError(f"unexpected DSD spur above absolute floor in {key}")

    full_candidate = _load(paths["dsd_full_candidate"])
    full_e2 = _load(paths["dsd_full_e2v3"])
    cross_rate = _cross_rate_summary(full_candidate, full_e2)

    external = _load(paths["external"])
    external_hybrid = next(
        record for record in external["results"] if record["id"] == "megaextreme"
    )
    coefficients = np.fromfile(paths["character"], dtype="<f8")
    source_files = (
        "audio_tests/dsd_public_quality.rs",
        "src/audio/dsp/timing.rs",
        "src/bin/filter_timing_bench.rs",
        "tools/split_phase_v6/README.md",
        "tools/split_phase_v6/e3_p7_cleanup_search.py",
        "tools/split_phase_v6/e3_p11_structural_search.py",
        "tools/split_phase_v6/e3_p12_gaussian_phase_search.py",
        "tools/split_phase_v6/e3_p14_multiresolution_phase_search.py",
        "tools/split_phase_v6/e3_p17_cross_rate_packet_repair.py",
        "tools/split_phase_v6/freeze_e3_p17.py",
    )
    report = {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "selected_replacement_candidate": {
            "identifier": SELECTED_IDENTIFIER,
            "character_sha256": CHARACTER_SHA256,
            "coefficient_count": int(coefficients.size),
            "exact_fsum": float(math.fsum(float(value) for value in coefficients)),
            "realization": replay["realization"],
            "timing": replay["timing"],
            "frequency": replay["frequency"],
            "production_packets": replay["packets"],
            "production_packet_failures": replay["packet_failures"],
            "restarted_carrier": replay["restarted_carrier"],
            "holdout_packet_cell_count": len(holdout_cells),
            "holdout_packet_failures": holdout_failures,
        },
        "timing_comparison": {
            "e2v3": native_e2,
            "candidate": native_candidate,
            "candidate_delta_vs_e2v3": _timing_delta(native_candidate, native_e2),
            "candidate_dominates_e2v3_on_all_frozen_metrics": True,
            "external_hybrid_reference": {
                "display_name": external_hybrid["display_name"],
                "impulse": external_hybrid["impulse"],
                "measurement_caveat": (
                    "signed PCM24 external render with a reported mandatory one-LSB "
                    "TPDF path; long -120 dB decay is floor-sensitive"
                ),
            },
        },
        "native_determinism": {
            "report_sha256": _sha256(paths["native_a"]),
            "group_delay_sha256": _sha256(paths["group_delay_a"]),
            "reports_byte_identical": True,
            "group_delay_csv_byte_identical": True,
            "runtime": native_by_filter["SplitPhase128kE3"]["runtime"],
        },
        "native_48khz_holdout": {
            "configuration": native_48k["configuration"],
            "e2v3": native_48_e2,
            "candidate": native_48_candidate,
            "candidate_delta_vs_e2v3": _timing_delta(
                native_48_candidate, native_48_e2
            ),
            "packet_failures": packet_48_failures,
            "runtime_contract_identical": True,
        },
        "dsd128_validation": {
            "cell_count": len(dsd_a["cells"]),
            "hard_failure_count": dsd_a["hard_failure_count"],
            "execution_failures": dsd_a["execution_failures"],
            "native_dsd_sha256": _cell_hashes(dsd_a),
            "rerun_native_hashes_identical": True,
            "canonical_report_sha256": _canonical_sha256(_strip_volatile(dsd_a)),
            "matched_transition_positive_excess_power_seconds": final_excess,
            "p6_matched_transition_positive_excess_power_seconds": p6_excess,
            "delta_vs_p6": transition_delta,
            "steady_spectral": final_spectral,
            "steady_spectral_delta_vs_e2v3": spectral_delta,
            "unexpected_spur_absolute_floor_dbfs": UNEXPECTED_SPUR_FLOOR_DBFS,
        },
        "cross_rate_dsd_validation": cross_rate,
        "capacity_conclusion": {
            "million_tap_repeat_run": False,
            "reason": (
                "P8/P9 already exact-tested 262,145, 524,289, and 1,048,577 "
                "taps for the same target. Timing was invariant at about 1e-12, "
                "packet movement stayed below 2.13e-10 dB, and rejection changed "
                "by less than 2e-6 dB; support is not the active constraint."
            ),
        },
        "decision": {
            "clear_replacement_found": True,
            "reason": (
                "P17 Pareto-dominates E2v3 on every frozen impulse/step metric, "
                "passes all production and holdout packets, preserves the phase-only "
                "frequency response and integer runtime contract, and improves every "
                "matched exact DSD128 restart-excess cell over P6."
            ),
            "production_default": "SplitPhase128kE2v3",
            "promotion_pending": [
                "final asset/manifest integration",
                "target-Mac real-time throughput",
                "controlled listening against E2v3",
            ],
        },
        "source_inputs": {
            name: {
                "path": str(path.relative_to(root)).replace("\\", "/"),
                "sha256": _sha256(path),
            }
            for name, path in paths.items()
        },
        "source_files": {path: _sha256(root / path) for path in source_files},
    }

    output_dir.mkdir(parents=True, exist_ok=True)
    outputs = {
        "summary": output_dir / "e3-p17-definitive-freeze.json",
        "character": output_dir / "e3-p17-replacement-candidate.f64le",
        "seed_search": output_dir / "e3-p16-search.json",
        "search": output_dir / "e3-p17-search.json",
        "native": output_dir / "e3-p17-native-timing.json",
        "group_delay": output_dir / "e3-p17-group-delay.csv",
        "holdouts": output_dir / "e3-p17-holdouts.json",
        "dsd128": output_dir / "e3-p17-dsd128.json",
        "native_48k": output_dir / "e3-p17-native-48k.json",
        "cross_rate": output_dir / "e3-p17-dsd-cross-rate-summary.json",
    }
    shutil.copyfile(paths["character"], outputs["character"])
    shutil.copyfile(paths["seed_search"], outputs["seed_search"])
    shutil.copyfile(paths["search"], outputs["search"])
    shutil.copyfile(paths["native_a"], outputs["native"])
    shutil.copyfile(paths["group_delay_a"], outputs["group_delay"])
    shutil.copyfile(paths["dsd_a"], outputs["dsd128"])
    shutil.copyfile(paths["native_48k"], outputs["native_48k"])
    _write_json_lf(
        outputs["holdouts"],
        {
            "schema_version": 1,
            "identity": f"{IDENTITY}-packet-holdouts",
            "character_sha256": CHARACTER_SHA256,
            "cell_count": len(holdout_cells),
            "failure_count": len(holdout_failures),
            "failures": holdout_failures,
            "cells": holdout_cells,
        },
    )
    _write_json_lf(outputs["cross_rate"], cross_rate)
    _write_json_lf(outputs["summary"], report)
    return report


def main() -> None:
    parser = argparse.ArgumentParser(description="Freeze the definitive E3 P17 package")
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[2]
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(__file__).resolve().parent / "baselines",
    )
    arguments = parser.parse_args()
    report = freeze(arguments.root.resolve(), arguments.output_dir.resolve())
    print(
        json.dumps(
            {
                "output": str(
                    arguments.output_dir.resolve() / "e3-p17-definitive-freeze.json"
                ),
                "identifier": SELECTED_IDENTIFIER,
                "character_sha256": CHARACTER_SHA256,
                "clear_replacement_found": report["decision"][
                    "clear_replacement_found"
                ],
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
