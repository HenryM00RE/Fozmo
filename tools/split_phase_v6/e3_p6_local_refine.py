from __future__ import annotations

import argparse
import hashlib
import json
import math
from dataclasses import asdict
from pathlib import Path
from typing import Any

import numpy as np
from scipy.stats import qmc

from .e3_p5_group_delay_search import (
    PACKET_NON_REGRESSION_DB,
    PACKET_QUALIFICATION_COUNT,
    STRUCTURES,
    _build_model,
    _candidate_character,
    _impulse_guards,
    _model_hash,
)
from .e3_p6_restarted_carrier_search import RestartedCarrierProbe, _reference_probe
from .e3_phase_search import (
    CHARACTER_RATE_HZ,
    FFT_LENGTH,
    _cascade_character_and_cleanup,
    _read_f64le,
    _timing_metrics,
)
from .evaluate_e3_packets import PACKET_FREQUENCIES_HZ, _measure_packet


IDENTITY = "SplitPhase128kE3-P6-D0145-local-refine-experimental"
FAMILY = "D"
LOCAL_RADIUS_SAMPLES = 0.125
FINALIST_COUNT = 32


def _sha256(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _local_samples(center: np.ndarray, count: int, radius: float) -> np.ndarray:
    if count < 2 or count & (count - 1):
        raise ValueError("candidate count must be a power of two")
    unit = qmc.Sobol(d=center.size, scramble=False).random_base2(int(math.log2(count)))
    perturbation = (2.0 * unit - 1.0) * radius
    perturbation[0] = 0.0
    return center[None, :] + perturbation


def _ranking_key(record: dict[str, Any]) -> tuple[float, ...]:
    metrics = record["metrics"]
    probe = record["restarted_carrier_probe"]
    return (
        metrics["maximum_post_lobe_db_peak"],
        metrics["post_energy_db_total"],
        metrics["step_undershoot_percent"],
        metrics["main_lobe_width_us"],
        probe["positive_excess_ratio_vs_refine0900"][1],
        probe["positive_excess_ratio_vs_refine0900"][0],
    )


def search(root: Path, work_dir: Path, count: int, radius: float) -> dict[str, Any]:
    p6_path = root / "tools/split_phase_v6/work-e3-p6/e3_p6_restarted_carrier_search.json"
    p6 = json.loads(p6_path.read_text(encoding="utf-8"))
    incumbent = next(
        record for record in p6["finalists"] if record["identifier"] == "p6-d-0145"
    )
    incumbent_free = np.asarray(incumbent["free"], dtype=np.float64)
    incumbent_probe = incumbent["restarted_carrier_probe"]

    structure = next(structure for structure in STRUCTURES if structure[0] == FAMILY)
    _, low_join_hz, _, controls = structure
    model = _build_model(*structure[1:])
    model_hash = _model_hash(model, structure)
    if model_hash != incumbent["model_sha256"]:
        raise RuntimeError("local model does not match the P6 incumbent")

    assets = root / "assets/filters/split_phase_e2v3"
    refine_path = root / "tools/split_phase_v6/work-e3-p3/pareto/refine-0900.f64le"
    incumbent_path = root / "tools/split_phase_v6/work-e3-p6/finalists/p6-d-0145.f64le"
    refine = _read_f64le(refine_path)
    e2 = _read_f64le(assets / "character_full_rate.f64le")
    cleanup = _read_f64le(assets / "cleanup_stage_1.f64le")
    incumbent_character = _read_f64le(incumbent_path)
    refine_spectrum = np.fft.rfft(refine, FFT_LENGTH)
    refine_phase = np.unwrap(np.angle(refine_spectrum))
    magnitude = np.abs(np.fft.rfft(e2, FFT_LENGTH))
    frequency_hz = np.fft.rfftfreq(FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ)
    reliable = frequency_hz <= 20_000.0
    refine_sum = float(math.fsum(float(value) for value in refine))
    refine_response = _cascade_character_and_cleanup(refine, cleanup)
    e2_response = _cascade_character_and_cleanup(e2, cleanup)
    incumbent_response = _cascade_character_and_cleanup(incumbent_character, cleanup)
    packet_refine = {
        str(int(frequency)): asdict(_measure_packet(refine_response, frequency))
        for frequency in PACKET_FREQUENCIES_HZ
    }
    probe = RestartedCarrierProbe()
    e2_envelope, _, _ = _reference_probe(probe, e2_response)
    refine_envelope, refine_rms, refine_excess = _reference_probe(
        probe, refine_response, e2_envelope
    )

    records: list[dict[str, Any]] = []
    characters: dict[str, np.ndarray] = {}
    for index, free in enumerate(_local_samples(incumbent_free, count, radius)):
        identifier = f"p6d-local-{index:04d}"
        if index == 0:
            candidate = incumbent_character.copy()
            structural = incumbent["structural"]
        else:
            candidate, structural = _candidate_character(
                refine_phase,
                magnitude,
                frequency_hz,
                model,
                free,
                low_join_hz,
                refine.size,
                refine_sum,
            )
        candidate_spectrum = np.fft.rfft(candidate, FFT_LENGTH)
        magnitude_error = 20.0 * np.log10(
            np.maximum(np.abs(candidate_spectrum[reliable]), 1.0e-300)
            / np.maximum(magnitude[reliable], 1.0e-300)
        )
        magnitude_delta = float(np.max(np.abs(magnitude_error)))
        response = _cascade_character_and_cleanup(candidate, cleanup)
        metrics = asdict(_timing_metrics(response))
        record: dict[str, Any] = {
            "index": index,
            "identifier": identifier,
            "family": FAMILY,
            "model_sha256": model_hash,
            "free": free.tolist(),
            "structural": structural,
            "maximum_magnitude_delta_db_0_20khz": magnitude_delta,
            "metrics": metrics,
        }
        record["passes_impulse_guards"] = _impulse_guards(
            metrics, structural, magnitude_delta
        )
        if record["passes_impulse_guards"]:
            measured = probe.measure(
                response,
                e2_envelope,
                refine_envelope,
                refine_rms,
                refine_excess,
            )
            record["restarted_carrier_probe"] = measured
            record["passes_incumbent_transition_guards"] = bool(
                measured["interval_rms_delta_db_vs_refine0900"][0]
                <= incumbent_probe["interval_rms_delta_db_vs_refine0900"][0]
                and measured["interval_rms_delta_db_vs_refine0900"][1]
                <= incumbent_probe["interval_rms_delta_db_vs_refine0900"][1]
                and measured["positive_excess_ratio_vs_refine0900"][0]
                <= incumbent_probe["positive_excess_ratio_vs_refine0900"][0]
                and measured["positive_excess_ratio_vs_refine0900"][1]
                <= incumbent_probe["positive_excess_ratio_vs_refine0900"][1]
            )
            if record["passes_incumbent_transition_guards"]:
                characters[identifier] = candidate
        records.append(record)

    impulse_safe = [record for record in records if record["passes_impulse_guards"]]
    transition_safe = sorted(
        (
            record
            for record in impulse_safe
            if record.get("passes_incumbent_transition_guards")
        ),
        key=_ranking_key,
    )
    for record in transition_safe[:PACKET_QUALIFICATION_COUNT]:
        response = _cascade_character_and_cleanup(characters[record["identifier"]], cleanup)
        packets = {
            str(int(frequency)): asdict(_measure_packet(response, frequency))
            for frequency in PACKET_FREQUENCIES_HZ
        }
        record["packets"] = packets
        record["packet_delta_db_vs_refine0900"] = {
            frequency: packets[frequency]["onset_pre_echo_energy_db_total"]
            - packet_refine[frequency]["onset_pre_echo_energy_db_total"]
            for frequency in packets
        }
        record["passes_packet_guards"] = all(
            delta <= PACKET_NON_REGRESSION_DB
            for delta in record["packet_delta_db_vs_refine0900"].values()
        )
    packet_safe = [record for record in transition_safe if record.get("passes_packet_guards")]
    finalists = sorted(packet_safe, key=_ranking_key)[:FINALIST_COUNT]
    finalist_dir = work_dir / "finalists"
    finalist_dir.mkdir(parents=True, exist_ok=True)
    for record in finalists:
        payload = np.asarray(characters[record["identifier"]], dtype="<f8").tobytes()
        path = finalist_dir / f"{record['identifier']}.f64le"
        path.write_bytes(payload)
        record["character_file"] = str(path.relative_to(work_dir)).replace("\\", "/")
        record["character_sha256"] = _sha256(payload)

    report = {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "source": {
            "p6_search": str(p6_path.relative_to(root)).replace("\\", "/"),
            "incumbent": "p6-d-0145",
            "incumbent_character": str(incumbent_path.relative_to(root)).replace("\\", "/"),
            "incumbent_sha256": _sha256(incumbent_path.read_bytes()),
        },
        "search": {
            "method": "unscrambled local Sobol perturbation around p6-d-0145",
            "family": FAMILY,
            "candidate_count": len(records),
            "radius_samples": radius,
            "impulse_safe_count": len(impulse_safe),
            "incumbent_transition_safe_count": len(transition_safe),
            "packet_qualified_count": len(packet_safe),
            "finalist_count": len(finalists),
        },
        "hard_guards": {
            "p6_impulse_guards": True,
            "zero_to_two_ms_rms_no_worse_than_p6_d_0145": True,
            "two_to_five_ms_rms_no_worse_than_p6_d_0145": True,
            "zero_to_two_ms_positive_excess_no_worse_than_p6_d_0145": True,
            "two_to_five_ms_positive_excess_no_worse_than_p6_d_0145": True,
            "all_packet_onset_delta_db_vs_refine0900_max": PACKET_NON_REGRESSION_DB,
        },
        "objective_order": [
            "maximum post-lobe",
            "post energy",
            "step undershoot",
            "main-lobe width",
            "2-5 ms restarted-carrier positive excess",
            "0-2 ms restarted-carrier positive excess",
        ],
        "incumbent": incumbent,
        "finalists": finalists,
        "records": records,
    }
    work_dir.mkdir(parents=True, exist_ok=True)
    (work_dir / "e3_p6_local_refine.json").write_text(
        json.dumps(report, indent=2) + "\n", encoding="utf-8"
    )
    return report


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--count", type=int, default=512)
    parser.add_argument("--radius", type=float, default=LOCAL_RADIUS_SAMPLES)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    work_dir = (
        arguments.work_dir or root / "tools/split_phase_v6/work-e3-p6-local"
    ).resolve()
    report = search(root, work_dir, arguments.count, arguments.radius)
    print(json.dumps(report["search"], indent=2))


if __name__ == "__main__":
    main()
