from __future__ import annotations

import argparse
import hashlib
import json
import math
from dataclasses import asdict
from pathlib import Path
from typing import Any

import numpy as np

from .e3_p5_group_delay_search import (
    IDENTITY as P5_IDENTITY,
    PACKET_NON_REGRESSION_DB,
    PACKET_QUALIFICATION_COUNT,
    STRUCTURES,
    _build_model,
    _candidate_character,
    _impulse_guards,
    _model_hash,
    _samples,
)
from .e3_phase_search import (
    CHARACTER_RATE_HZ,
    FFT_LENGTH,
    _cascade_character_and_cleanup,
    _read_f64le,
    _timing_metrics,
)
from .evaluate_e3_packets import PACKET_FREQUENCIES_HZ, _measure_packet


IDENTITY = "SplitPhase128kE3-P6-restarted-carrier-search-experimental"
OUTPUT_RATE_HZ = 176_400
SOURCE_RATE_HZ = 44_100
FULL_RATE_ORIGIN = 5_314
TRACE_SECONDS = 0.050
WINDOW_SECONDS = 0.002
INTERVALS_MS = ((0.0, 2.0), (2.0, 5.0), (5.0, 10.0), (10.0, 25.0), (25.0, 50.0))
TARGET_FREQUENCIES_HZ = (18_000.0, 19_000.0)
SOURCE_PHASES_RAD = (0.31 + math.pi, 1.17 + math.pi)
MATCHED_EFFECTIVE_PEAK = 0.630_326_387_135_713_1
TOLERANCE_RMS = 2.0e-9
MAX_ZERO_TO_TWO_RMS_DELTA_DB = 0.0
MAX_ZERO_TO_TWO_EXCESS_RATIO = 1.0
FINALIST_COUNT = 32


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _sha256_file(path: Path) -> str:
    return _sha256_bytes(path.read_bytes())


def _coherent_frequencies() -> tuple[float, ...]:
    bin_hz = SOURCE_RATE_HZ / 16_384
    return tuple(round(frequency / bin_hz) * bin_hz for frequency in TARGET_FREQUENCIES_HZ)


def _effective_carrier_amplitude(frequencies_hz: tuple[float, ...]) -> float:
    frame = np.arange(16_384, dtype=np.float64)
    raw = sum(
        np.sin(2.0 * np.pi * frequency * frame / SOURCE_RATE_HZ + phase)
        for frequency, phase in zip(frequencies_hz, SOURCE_PHASES_RAD)
    )
    return MATCHED_EFFECTIVE_PEAK / float(np.max(np.abs(raw)))


class RestartedCarrierProbe:
    def __init__(self) -> None:
        self.frequencies_hz = _coherent_frequencies()
        self.carrier_amplitude = _effective_carrier_amplitude(self.frequencies_hz)
        self.trace_samples = round(OUTPUT_RATE_HZ * TRACE_SECONDS)
        self.window_samples = round(OUTPUT_RATE_HZ * WINDOW_SECONDS)
        self.residual_samples = self.trace_samples + self.window_samples - 1
        self.sample_indices = np.arange(self.residual_samples, dtype=np.int64)
        self.aligned_indices = self.sample_indices + FULL_RATE_ORIGIN
        self.interval_ranges = tuple(
            (
                round(start_ms * OUTPUT_RATE_HZ / 1000.0),
                round(end_ms * OUTPUT_RATE_HZ / 1000.0),
            )
            for start_ms, end_ms in INTERVALS_MS
        )

    def residual(self, response: np.ndarray) -> np.ndarray:
        residual = np.zeros(self.residual_samples, dtype=np.float64)
        for frequency_hz, phase_rad in zip(self.frequencies_hz, SOURCE_PHASES_RAD):
            omega = 2.0 * np.pi * frequency_hz / SOURCE_RATE_HZ
            for residue in range(4):
                selected = self.aligned_indices % 4 == residue
                source_offset = (self.aligned_indices[selected] - residue) // 4
                polyphase = response[residue::4]
                coefficient_index = np.arange(polyphase.size, dtype=np.float64)
                weighted = polyphase * np.exp(-1j * omega * coefficient_index)
                suffix = np.cumsum(weighted[::-1])[::-1]
                values = np.zeros(source_offset.size, dtype=np.complex128)
                valid = source_offset + 1 < suffix.size
                values[valid] = suffix[source_offset[valid] + 1]
                residual[selected] -= self.carrier_amplitude * np.imag(
                    np.exp(1j * (phase_rad + omega * source_offset)) * values
                )
        return residual

    def envelope(self, residual: np.ndarray) -> np.ndarray:
        prefix = np.concatenate(([0.0], np.cumsum(residual * residual)))
        return (
            prefix[self.window_samples : self.window_samples + self.trace_samples]
            - prefix[: self.trace_samples]
        ) / self.window_samples

    def measure(
        self,
        response: np.ndarray,
        e2_envelope: np.ndarray,
        anchor_envelope: np.ndarray,
        anchor_interval_rms_dbfs: list[float],
        anchor_excess: list[float],
    ) -> dict[str, Any]:
        residual = self.residual(response)
        envelope = self.envelope(residual)
        interval_rms_dbfs = []
        positive_excess = []
        tolerance_power = TOLERANCE_RMS * TOLERANCE_RMS
        pointwise_excess = np.maximum(envelope - e2_envelope - tolerance_power, 0.0)
        anchor_pointwise_excess = np.maximum(
            anchor_envelope - e2_envelope - tolerance_power, 0.0
        )
        for start, end in self.interval_ranges:
            mean_square = float(np.mean(residual[start:end] ** 2))
            interval_rms_dbfs.append(
                10.0 * math.log10(max(2.0 * mean_square, 1.0e-300))
            )
            positive_excess.append(float(np.sum(pointwise_excess[start:end])) / OUTPUT_RATE_HZ)
        return {
            "interval_rms_dbfs": interval_rms_dbfs,
            "interval_rms_delta_db_vs_refine0900": [
                value - anchor
                for value, anchor in zip(interval_rms_dbfs, anchor_interval_rms_dbfs)
            ],
            "positive_excess_power_linear_seconds_vs_e2v3": positive_excess,
            "positive_excess_ratio_vs_refine0900": [
                value / anchor if anchor > 0.0 else None
                for value, anchor in zip(positive_excess, anchor_excess)
            ],
            "total_positive_excess_power_linear_seconds_vs_e2v3": float(
                np.sum(pointwise_excess)
            )
            / OUTPUT_RATE_HZ,
            "anchor_total_positive_excess_power_linear_seconds_vs_e2v3": float(
                np.sum(anchor_pointwise_excess)
            )
            / OUTPUT_RATE_HZ,
        }


def _reference_probe(
    probe: RestartedCarrierProbe,
    response: np.ndarray,
    e2_envelope: np.ndarray | None = None,
) -> tuple[np.ndarray, list[float], list[float]]:
    residual = probe.residual(response)
    envelope = probe.envelope(residual)
    rms = []
    excess = []
    reference = envelope if e2_envelope is None else e2_envelope
    for start, end in probe.interval_ranges:
        mean_square = float(np.mean(residual[start:end] ** 2))
        rms.append(10.0 * math.log10(max(2.0 * mean_square, 1.0e-300)))
        pointwise = np.maximum(
            envelope[start:end] - reference[start:end] - TOLERANCE_RMS**2,
            0.0,
        )
        excess.append(float(np.sum(pointwise)) / OUTPUT_RATE_HZ)
    return envelope, rms, excess


def _transition_guards(probe: dict[str, Any]) -> bool:
    ratio = probe["positive_excess_ratio_vs_refine0900"][0]
    return bool(
        probe["interval_rms_delta_db_vs_refine0900"][0]
        <= MAX_ZERO_TO_TWO_RMS_DELTA_DB
        and ratio is not None
        and ratio <= MAX_ZERO_TO_TWO_EXCESS_RATIO
    )


def _ranking_key(record: dict[str, Any]) -> tuple[float, ...]:
    probe = record["restarted_carrier_probe"]
    metrics = record["metrics"]
    return (
        probe["positive_excess_ratio_vs_refine0900"][1],
        probe["interval_rms_delta_db_vs_refine0900"][1],
        metrics["maximum_post_lobe_db_peak"],
        metrics["post_energy_db_total"],
        metrics["step_undershoot_percent"],
        metrics["main_lobe_width_us"],
    )


def search(root: Path, work_dir: Path, per_family: int) -> dict[str, Any]:
    assets = root / "assets/filters/split_phase_e2v3"
    anchor_path = root / "tools/split_phase_v6/work-e3-p3/pareto/refine-0900.f64le"
    anchor = _read_f64le(anchor_path)
    e2_character = _read_f64le(assets / "character_full_rate.f64le")
    cleanup = _read_f64le(assets / "cleanup_stage_1.f64le")
    anchor_spectrum = np.fft.rfft(anchor, FFT_LENGTH)
    anchor_phase = np.unwrap(np.angle(anchor_spectrum))
    magnitude = np.abs(np.fft.rfft(e2_character, FFT_LENGTH))
    frequency_hz = np.fft.rfftfreq(FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ)
    reliable = frequency_hz <= 20_000.0
    anchor_sum = float(math.fsum(float(value) for value in anchor))
    anchor_response = _cascade_character_and_cleanup(anchor, cleanup)
    e2_response = _cascade_character_and_cleanup(e2_character, cleanup)
    packet_anchor = {
        str(int(frequency)): asdict(_measure_packet(anchor_response, frequency))
        for frequency in PACKET_FREQUENCIES_HZ
    }
    packet_e2 = {
        str(int(frequency)): asdict(_measure_packet(e2_response, frequency))
        for frequency in PACKET_FREQUENCIES_HZ
    }
    probe = RestartedCarrierProbe()
    e2_envelope, e2_rms, _ = _reference_probe(probe, e2_response)
    anchor_envelope, anchor_rms, anchor_excess = _reference_probe(
        probe, anchor_response, e2_envelope
    )

    records: list[dict[str, Any]] = []
    characters: dict[str, np.ndarray] = {}
    models = []
    global_index = 0
    for structure in STRUCTURES:
        family, low_join_hz, treble_join_hz, controls = structure
        model = _build_model(low_join_hz, treble_join_hz, controls)
        model_hash = _model_hash(model, structure)
        models.append(
            {
                "family": family,
                "low_join_hz": low_join_hz,
                "treble_join_hz": treble_join_hz,
                "controls": controls,
                "free_coordinates": model.free_coordinates,
                "model_sha256": model_hash,
            }
        )
        for family_index, free in enumerate(_samples(model.free_coordinates, per_family)):
            identifier = f"p6-{family.lower()}-{family_index:04d}"
            if family_index == 0:
                candidate = anchor.copy()
                structural = {
                    "phase_closure_error_rad": 0.0,
                    "maximum_phase_delta_rad": 0.0,
                    "maximum_group_delay_delta_samples": 0.0,
                    "maximum_group_delay_curvature_samples_per_ln_hz_squared": 0.0,
                    "omitted_periodic_energy_ratio": 0.0,
                }
            else:
                candidate, structural = _candidate_character(
                    anchor_phase,
                    magnitude,
                    frequency_hz,
                    model,
                    free,
                    low_join_hz,
                    anchor.size,
                    anchor_sum,
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
                "index": global_index,
                "identifier": identifier,
                "family": family,
                "family_index": family_index,
                "model_sha256": model_hash,
                "free": free.tolist(),
                "free_sha256": _sha256_bytes(np.asarray(free, dtype="<f8").tobytes()),
                "structural": structural,
                "maximum_magnitude_delta_db_0_20khz": magnitude_delta,
                "metrics": metrics,
            }
            record["passes_impulse_guards"] = _impulse_guards(
                metrics, structural, magnitude_delta
            )
            if record["passes_impulse_guards"]:
                record["restarted_carrier_probe"] = probe.measure(
                    response,
                    e2_envelope,
                    anchor_envelope,
                    anchor_rms,
                    anchor_excess,
                )
                record["passes_transition_guards"] = _transition_guards(
                    record["restarted_carrier_probe"]
                )
                if record["passes_transition_guards"]:
                    characters[identifier] = candidate
            records.append(record)
            global_index += 1

    impulse_safe = [record for record in records if record["passes_impulse_guards"]]
    transition_safe = sorted(
        (record for record in impulse_safe if record.get("passes_transition_guards")),
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
            - packet_anchor[frequency]["onset_pre_echo_energy_db_total"]
            for frequency in packets
        }
        record["packet_delta_db_vs_e2v3"] = {
            frequency: packets[frequency]["onset_pre_echo_energy_db_total"]
            - packet_e2[frequency]["onset_pre_echo_energy_db_total"]
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
        record["character_sha256"] = _sha256_bytes(payload)

    report = {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "source": {
            "p5_identity": P5_IDENTITY,
            "anchor_refine0900": str(anchor_path.relative_to(root)).replace("\\", "/"),
            "anchor_refine0900_sha256": _sha256_file(anchor_path),
            "e2v3_character_sha256": _sha256_file(assets / "character_full_rate.f64le"),
            "cleanup_stage_1_sha256": _sha256_file(assets / "cleanup_stage_1.f64le"),
        },
        "search": {
            "method": "P5 constrained-delay Sobol search ranked by restarted-carrier residual",
            "per_family": per_family,
            "candidate_count": len(records),
            "impulse_safe_count": len(impulse_safe),
            "transition_safe_count": len(transition_safe),
            "packet_qualified_count": len(packet_safe),
            "finalist_count": len(finalists),
            "models": models,
        },
        "restarted_carrier_contract": {
            "source_rate_hz": SOURCE_RATE_HZ,
            "analysis_rate_hz": OUTPUT_RATE_HZ,
            "full_rate_origin": FULL_RATE_ORIGIN,
            "frequencies_hz": probe.frequencies_hz,
            "effective_carrier_amplitude": probe.carrier_amplitude,
            "sliding_window_ms": WINDOW_SECONDS * 1000.0,
            "trace_duration_ms": TRACE_SECONDS * 1000.0,
            "intervals_ms": INTERVALS_MS,
            "tolerance_rms": TOLERANCE_RMS,
            "e2v3_interval_rms_dbfs": e2_rms,
            "refine0900_interval_rms_dbfs": anchor_rms,
            "refine0900_positive_excess_power_linear_seconds_vs_e2v3": anchor_excess,
        },
        "hard_guards": {
            "maximum_pre_lobe_db_peak_max": -22.5,
            "pre_energy_db_total_max": -4.85,
            "maximum_post_lobe_db_peak_max": -8.6,
            "main_lobe_width_us_max": 62.5,
            "proxy_step_overshoot_percent_max": 9.22,
            "decay_120_ms_max": 7.0,
            "maximum_magnitude_delta_db_0_20khz": 5.0e-7,
            "zero_to_two_ms_rms_delta_db_vs_refine0900_max": MAX_ZERO_TO_TWO_RMS_DELTA_DB,
            "zero_to_two_ms_positive_excess_ratio_vs_refine0900_max": MAX_ZERO_TO_TWO_EXCESS_RATIO,
            "all_packet_onset_delta_db_vs_refine0900_max": PACKET_NON_REGRESSION_DB,
        },
        "objective_order": [
            "2-5 ms restarted-carrier positive excess versus E2v3",
            "2-5 ms restarted-carrier RMS delta versus refine-0900",
            "maximum post-lobe",
            "post energy",
            "step undershoot",
            "main-lobe width",
        ],
        "packet_reference_refine0900": packet_anchor,
        "packet_reference_e2v3": packet_e2,
        "finalists": finalists,
        "records": records,
    }
    work_dir.mkdir(parents=True, exist_ok=True)
    (work_dir / "e3_p6_restarted_carrier_search.json").write_bytes(
        (json.dumps(report, indent=2) + "\n").encode("utf-8")
    )
    return report


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--per-family", type=int, default=1024)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    work_dir = (
        arguments.work_dir or root / "tools/split_phase_v6/work-e3-p6"
    ).resolve()
    report = search(root, work_dir, arguments.per_family)
    print(json.dumps(report["search"], indent=2))


if __name__ == "__main__":
    main()
