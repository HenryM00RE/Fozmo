from __future__ import annotations

import argparse
import json
import math
from dataclasses import dataclass, asdict
from pathlib import Path

import numpy as np

from .e3_phase_search import (
    CHARACTER_RATE_HZ,
    FFT_LENGTH,
    OUTPUT_RATE_HZ,
    _candidate_character,
    _cascade_character_and_cleanup,
    _read_f64le,
)


SOURCE_RATE_HZ = 44_100.0
PACKET_CYCLES = 8.0
PACKET_FREQUENCIES_HZ = (5_000.0, 10_000.0, 15_000.0, 18_000.0, 20_000.0)
INTEGER_RATIO = int(OUTPUT_RATE_HZ / SOURCE_RATE_HZ)


@dataclass(frozen=True)
class PacketMetrics:
    frequency_hz: float
    onset_pre_echo_energy_db_total: float
    onset_pre_echo_energy_db_0_0p5ms: float
    onset_pre_echo_energy_db_0p5_2ms: float
    onset_pre_echo_energy_db_2_8ms: float
    onset_post_decay_energy_db_total: float
    maximum_onset_pre_echo_db_peak: float
    maximum_onset_post_decay_db_peak: float


def _power_db(value: float, reference: float) -> float:
    if value <= 0.0 or reference <= 0.0:
        return -300.0
    return max(10.0 * math.log10(value / reference), -300.0)


def _amplitude_db(value: float, reference: float) -> float:
    if value <= 0.0 or reference <= 0.0:
        return -300.0
    return max(20.0 * math.log10(value / reference), -300.0)


def _packet(
    frequency_hz: float, cycles: float = PACKET_CYCLES
) -> tuple[np.ndarray, np.ndarray]:
    if cycles <= 0.0:
        raise ValueError("packet cycles must be positive")
    samples = max(round(cycles / frequency_hz * SOURCE_RATE_HZ), 3)
    index = np.arange(samples, dtype=np.float64)
    window = 0.5 - 0.5 * np.cos(2.0 * np.pi * index / (samples - 1))
    phase = 2.0 * np.pi * frequency_hz * index / SOURCE_RATE_HZ
    return window * np.cos(phase), window * np.sin(phase)


def _convolve_upsampled(source: np.ndarray, response: np.ndarray) -> np.ndarray:
    upsampled_length = (source.size - 1) * INTEGER_RATIO + 1
    output_length = upsampled_length + response.size - 1
    fft_length = 1 << (output_length - 1).bit_length()
    upsampled = np.zeros(fft_length, dtype=np.float64)
    upsampled[:upsampled_length:INTEGER_RATIO] = source
    spectrum = np.fft.rfft(upsampled)
    spectrum *= np.fft.rfft(response, fft_length)
    return np.fft.irfft(spectrum, fft_length)[:output_length]


def _measure_packet(
    response: np.ndarray,
    frequency_hz: float,
    cycles: float = PACKET_CYCLES,
) -> PacketMetrics:
    packet_i, packet_q = _packet(frequency_hz, cycles)
    output_i = _convolve_upsampled(packet_i, response)
    output_q = _convolve_upsampled(packet_q, response)
    envelope = np.hypot(output_i, output_q)
    energy = float(np.dot(envelope, envelope))
    peak = float(np.max(envelope))
    onset_start = int(np.argmax(np.abs(response)))
    nominal_output_samples = packet_i.size / SOURCE_RATE_HZ * OUTPUT_RATE_HZ
    onset_end = min(int(math.floor(onset_start + nominal_output_samples)), envelope.size - 1)
    pre = envelope[:onset_start]
    post = envelope[onset_end + 1 :]
    return PacketMetrics(
        frequency_hz=frequency_hz,
        onset_pre_echo_energy_db_total=_power_db(float(np.dot(pre, pre)), energy),
        onset_pre_echo_energy_db_0_0p5ms=_power_db(
            _pre_onset_window_energy(envelope, onset_start, 0.0, 0.5), energy
        ),
        onset_pre_echo_energy_db_0p5_2ms=_power_db(
            _pre_onset_window_energy(envelope, onset_start, 0.5, 2.0), energy
        ),
        onset_pre_echo_energy_db_2_8ms=_power_db(
            _pre_onset_window_energy(envelope, onset_start, 2.0, 8.0), energy
        ),
        onset_post_decay_energy_db_total=_power_db(float(np.dot(post, post)), energy),
        maximum_onset_pre_echo_db_peak=_amplitude_db(
            float(np.max(pre)) if pre.size else 0.0, peak
        ),
        maximum_onset_post_decay_db_peak=_amplitude_db(
            float(np.max(post)) if post.size else 0.0, peak
        ),
    )


def _pre_onset_window_energy(
    envelope: np.ndarray, onset: int, near_ms: float, far_ms: float
) -> float:
    near_samples = round(near_ms / 1000.0 * OUTPUT_RATE_HZ)
    far_samples = round(far_ms / 1000.0 * OUTPUT_RATE_HZ)
    start = max(onset - far_samples, 0)
    end = max(onset - near_samples, 0)
    window = envelope[start:end]
    return float(np.dot(window, window))


def _packet_set(response: np.ndarray) -> list[dict[str, float]]:
    return [asdict(_measure_packet(response, frequency)) for frequency in PACKET_FREQUENCIES_HZ]


def evaluate(root: Path, search_report: Path, output: Path) -> dict[str, object]:
    asset_dir = root / "assets/filters/split_phase_e2v3"
    character = _read_f64le(asset_dir / "character_full_rate.f64le")
    cleanup = _read_f64le(asset_dir / "cleanup_stage_1.f64le")
    spectrum = np.fft.rfft(character, FFT_LENGTH)
    magnitude = np.abs(spectrum)
    phase = np.unwrap(np.angle(spectrum))
    frequency_hz = np.fft.rfftfreq(FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ)
    omega = 2.0 * np.pi * np.arange(spectrum.size, dtype=np.float64) / FFT_LENGTH
    peak_delay = float(np.argmax(np.abs(character)))
    baseline_response = _cascade_character_and_cleanup(character, cleanup)
    baseline_packets = _packet_set(baseline_response)
    report = json.loads(search_report.read_text(encoding="utf-8"))

    candidates: list[dict[str, object]] = []
    for record in report["candidates"]:
        if not record["passes_safety"]:
            continue
        candidate, _ = _candidate_character(
            spectrum,
            phase,
            magnitude,
            omega,
            frequency_hz,
            float(math.fsum(float(value) for value in character)),
            character.size,
            peak_delay,
            float(record["join_hz"]),
            float(record["delay_offset_samples"]),
            float(record["strength"]),
        )
        packets = _packet_set(_cascade_character_and_cleanup(candidate, cleanup))
        deltas = [
            candidate_packet["onset_pre_echo_energy_db_total"]
            - baseline_packet["onset_pre_echo_energy_db_total"]
            for candidate_packet, baseline_packet in zip(packets, baseline_packets)
        ]
        candidates.append(
            {
                "identifier": record["identifier"],
                "impulse_metrics": record["metrics"],
                "packets": packets,
                "onset_pre_echo_delta_db_vs_e2v3": deltas,
                "passes_packet_non_regression": all(delta <= 0.10 for delta in deltas),
            }
        )

    result: dict[str, object] = {
        "schema_version": 1,
        "identity": "SplitPhase128kE3 packet-onset qualification",
        "production_promoted": False,
        "configuration": {
            "source_rate_hz": SOURCE_RATE_HZ,
            "output_rate_hz": OUTPUT_RATE_HZ,
            "packet_cycles": PACKET_CYCLES,
            "packet_frequencies_hz": PACKET_FREQUENCIES_HZ,
            "alignment": "principal impulse peak plus nominal source-packet bounds",
            "non_regression_tolerance_db": 0.10,
        },
        "baseline_packets": baseline_packets,
        "evaluated_safety_qualified_candidates": len(candidates),
        "packet_qualified_candidates": sum(
            bool(candidate["passes_packet_non_regression"]) for candidate in candidates
        ),
        "candidates": candidates,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description="Qualify E3 phase candidates with tone-packet onset metrics")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--search-report", type=Path)
    parser.add_argument("--output", type=Path)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    search_report = (
        arguments.search_report
        or root / "tools/split_phase_v6/work-e3-p/e3_phase_search.json"
    ).resolve()
    output = (
        arguments.output
        or root / "tools/split_phase_v6/work-e3-p/e3_packet_qualification.json"
    ).resolve()
    result = evaluate(root, search_report, output)
    print(
        json.dumps(
            {
                "evaluated": result["evaluated_safety_qualified_candidates"],
                "qualified": result["packet_qualified_candidates"],
                "output": str(output),
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
