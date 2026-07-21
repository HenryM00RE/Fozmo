from __future__ import annotations

import argparse
import hashlib
import json
import math
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Iterable

import numpy as np
from scipy.special import betainc

from .e3_p10_packet_contract import (
    measure_packet_set,
    packet_gate_deltas,
    packet_gate_failures,
)
from .e3_phase_search import (
    CHARACTER_RATE_HZ,
    FFT_LENGTH,
    OUTPUT_RATE_HZ,
    _cascade_character_and_cleanup,
    _read_f64le,
    _timing_metrics,
)


IDENTITY = "SplitPhase128kE3-P11-wide-guard-band-structural-search"
EXTERNAL_BASELINE = (
    "tools/filter_timing/baselines/external-product-static-filters-pcm24.json"
)
GLOBAL_DELAY_SAMPLES = 4_096.0
STOPBAND_START_HZ = 24_100.0


@dataclass(frozen=True)
class StructuralParameters:
    passband_edge_hz: float
    stopband_edge_hz: float
    beta_left: float
    beta_right: float
    stopband_floor_db: float
    minimum_phase_fraction: float
    fractional_delay_samples: float = 0.0

    @property
    def identifier(self) -> str:
        return (
            f"p11-fp{int(self.passband_edge_hz):05d}"
            f"-fs{int(self.stopband_edge_hz):05d}"
            f"-b{int(self.beta_left)}x{int(self.beta_right)}"
            f"-a{int(round(self.minimum_phase_fraction * 1000)):03d}"
        )


BALANCED_TARGETS = {
    # E2v3 remains the production safety reference. The unbranded external
    # hybrid supplies the sharper post-response and width targets.
    "pre_energy_db_total": -5.243295015715933,
    "maximum_pre_lobe_db_peak": -18.25087131088886,
    "post_energy_db_total": -3.79,
    "maximum_post_lobe_db_peak": -11.17,
    "main_lobe_width_us": 45.90,
    "decay_120_ms": 4.251700680272109,
    "step_overshoot_percent": 13.661983516034383,
    "step_undershoot_percent": 8.952,
}

TARGET_SCALES = {
    "pre_energy_db_total": 0.25,
    "maximum_pre_lobe_db_peak": 0.50,
    "post_energy_db_total": 0.25,
    "maximum_post_lobe_db_peak": 0.50,
    "main_lobe_width_us": 1.00,
    "decay_120_ms": 0.50,
    "step_overshoot_percent": 0.25,
    "step_undershoot_percent": 0.25,
}


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _write_json_lf(path: Path, value: Any) -> None:
    path.write_bytes((json.dumps(value, indent=2) + "\n").encode("utf-8"))


def _frequency_grid() -> tuple[np.ndarray, np.ndarray]:
    frequency_hz = np.fft.rfftfreq(FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ)
    omega = 2.0 * np.pi * frequency_hz / CHARACTER_RATE_HZ
    return frequency_hz, omega


def _magnitude_target(
    frequency_hz: np.ndarray, parameters: StructuralParameters
) -> np.ndarray:
    if parameters.stopband_edge_hz <= parameters.passband_edge_hz:
        raise ValueError("stopband edge must exceed passband edge")
    if parameters.beta_left < 2.0 or parameters.beta_right < 2.0:
        raise ValueError("beta transition exponents must be at least two")
    x = np.clip(
        (frequency_hz - parameters.passband_edge_hz)
        / (parameters.stopband_edge_hz - parameters.passband_edge_hz),
        0.0,
        1.0,
    )
    floor = 10.0 ** (parameters.stopband_floor_db / 20.0)
    transition = betainc(parameters.beta_left, parameters.beta_right, x)
    return floor + (1.0 - floor) * (1.0 - transition)


def _minimum_phase_from_magnitude(magnitude: np.ndarray) -> np.ndarray:
    if magnitude.shape != (FFT_LENGTH // 2 + 1,):
        raise ValueError("magnitude grid does not match the P11 design FFT")
    cepstrum = np.fft.irfft(np.log(np.maximum(magnitude, 1.0e-300)), n=FFT_LENGTH)
    cepstrum[1 : FFT_LENGTH // 2] *= 2.0
    cepstrum[FFT_LENGTH // 2 + 1 :] = 0.0
    return np.unwrap(np.angle(np.exp(np.fft.rfft(cepstrum, n=FFT_LENGTH))))


def _realize_character(
    support: int,
    frequency_hz: np.ndarray,
    omega: np.ndarray,
    parameters: StructuralParameters,
    phase_delta: np.ndarray | None = None,
) -> tuple[np.ndarray, dict[str, float]]:
    magnitude = _magnitude_target(frequency_hz, parameters)
    minimum_phase = _minimum_phase_from_magnitude(magnitude)
    phase = parameters.minimum_phase_fraction * minimum_phase
    if phase_delta is not None:
        if phase_delta.shape != phase.shape:
            raise ValueError("phase delta does not match design grid")
        phase = phase + phase_delta
    phase = phase - omega * (
        GLOBAL_DELAY_SAMPLES + parameters.fractional_delay_samples
    )
    target = magnitude * np.exp(1j * phase)
    target[0] = complex(float(target[0].real), 0.0)
    target[-1] = complex(float(target[-1].real), 0.0)
    periodic = np.fft.irfft(target, n=FFT_LENGTH)
    character = periodic[:support].copy()
    total_periodic_energy = max(float(np.dot(periodic, periodic)), 1.0e-300)
    omitted = (
        float(np.dot(periodic[support:], periodic[support:]))
        / total_periodic_energy
    )
    character /= float(math.fsum(float(value) for value in character))
    edge_count = min(2_048, support // 4)
    edge = float(
        np.dot(character[:edge_count], character[:edge_count])
        + np.dot(character[-edge_count:], character[-edge_count:])
    ) / max(float(np.dot(character, character)), 1.0e-300)
    return character, {
        "omitted_periodic_energy_ratio": omitted,
        "character_edge_energy_ratio": edge,
        "character_sum": float(math.fsum(float(value) for value in character)),
        "minimum_magnitude_db": float(
            20.0 * np.log10(max(float(np.min(magnitude)), 1.0e-300))
        ),
        "maximum_phase_delta_rad": float(
            np.max(np.abs(phase_delta), initial=0.0)
            if phase_delta is not None
            else 0.0
        ),
    }


def _frequency_metrics(response: np.ndarray) -> dict[str, float]:
    fft_length = 1 << max(20, (response.size - 1).bit_length())
    spectrum = np.fft.rfft(response, n=fft_length)
    magnitude = np.abs(spectrum)
    magnitude /= max(float(magnitude[0]), 1.0e-300)
    frequency = np.fft.rfftfreq(fft_length, 1.0 / OUTPUT_RATE_HZ)
    magnitude_db = 20.0 * np.log10(np.maximum(magnitude, 1.0e-300))

    def gain(hz: float) -> float:
        return float(np.interp(hz, frequency, magnitude_db))

    passband = magnitude_db[(frequency >= 20.0) & (frequency <= 20_000.0)]
    stopband = magnitude_db[frequency >= STOPBAND_START_HZ]
    first_image = magnitude_db[
        (frequency >= STOPBAND_START_HZ) & (frequency <= 64_100.0)
    ]
    return {
        **{
            f"gain_{int(hz)}hz_db_dc": gain(hz)
            for hz in (5_000, 10_000, 15_000, 18_000, 20_000, 22_050)
        },
        "passband_ripple_20hz_20khz_db": float(np.max(passband) - np.min(passband)),
        "maximum_stopband_db_24p1khz_nyquist": float(np.max(stopband)),
        "maximum_first_image_db_24p1_64p1khz": float(np.max(first_image)),
    }


def _runtime_step_metrics(
    response: np.ndarray, integer_ratio: int = 4
) -> tuple[float, float]:
    """Measure the source-rate step used by the native timing bench.

    A source-rate step becomes an impulse train after integer interpolation;
    it is not the cumulative sum of every output-rate impulse sample.  Each
    output polyphase branch therefore has its own cumulative response.  The
    native bench normalizes their common settled plateau and reports the
    largest excursion over all branches.
    """
    if integer_ratio <= 0:
        raise ValueError("integer ratio must be positive")
    values = np.asarray(response, dtype=np.float64)
    if values.ndim != 1 or values.size == 0:
        raise ValueError("response must be a non-empty one-dimensional array")
    maximum = -math.inf
    minimum = math.inf
    settled_branches: list[float] = []
    for phase in range(integer_ratio):
        branch = values[phase::integer_ratio]
        if branch.size == 0:
            continue
        step = np.cumsum(branch)
        maximum = max(maximum, float(np.max(step)))
        minimum = min(minimum, float(np.min(step)))
        settled_branches.append(float(step[-1]))
    if len(settled_branches) != integer_ratio:
        raise ValueError("response is shorter than the interpolation ratio")
    settled = float(math.fsum(settled_branches) / integer_ratio)
    if abs(settled) <= np.finfo(np.float64).eps:
        return 0.0, 0.0
    return (
        max(maximum / settled - 1.0, 0.0) * 100.0,
        max(-minimum / settled, 0.0) * 100.0,
    )


def _timing_with_runtime_step(response: np.ndarray) -> dict[str, Any]:
    timing = asdict(_timing_metrics(response))
    overshoot, undershoot = _runtime_step_metrics(response)
    timing["step_overshoot_percent"] = overshoot
    timing["step_undershoot_percent"] = undershoot
    return timing


def _timing_violation(timing: dict[str, Any]) -> tuple[float, list[str]]:
    failures: list[str] = []
    score = 0.0
    for metric, scale in TARGET_SCALES.items():
        value = timing[metric]
        if value is None:
            failures.append(metric)
            score += 100.0
            continue
        excess = float(value) - BALANCED_TARGETS[metric]
        if excess > 0.0:
            failures.append(metric)
            score += excess / scale
    return score, failures


def _parameter_grid() -> Iterable[StructuralParameters]:
    for passband_edge_hz in (20_500.0, 21_000.0, 21_500.0, 22_000.0):
        for stopband_edge_hz in (23_800.0, 24_100.0):
            for beta_left in (2.0, 4.0, 8.0):
                for beta_right in (2.0, 4.0, 8.0):
                    for minimum_phase_fraction in (0.14, 0.16, 0.18, 0.20, 0.22, 0.24):
                        yield StructuralParameters(
                            passband_edge_hz=passband_edge_hz,
                            stopband_edge_hz=stopband_edge_hz,
                            beta_left=beta_left,
                            beta_right=beta_right,
                            stopband_floor_db=-80.0,
                            minimum_phase_fraction=minimum_phase_fraction,
                        )


def _external_summary(root: Path) -> dict[str, Any]:
    report = json.loads((root / EXTERNAL_BASELINE).read_text(encoding="utf-8"))
    return {
        row["id"]: {
            "display_name": row["display_name"],
            "impulse": row["impulse"],
            "magnitude": row["magnitude"],
            "packets": row["packets"],
        }
        for row in report["results"]
        if row["system"] == "ExternalProduct"
    }


def search(root: Path, output_dir: Path, limit: int | None = None) -> dict[str, Any]:
    character_path = root / "assets/filters/split_phase_e2v3/character_full_rate.f64le"
    cleanup_path = root / "assets/filters/split_phase_e2v3/cleanup_stage_1.f64le"
    e2 = _read_f64le(character_path)
    cleanup = _read_f64le(cleanup_path)
    e2_response = _cascade_character_and_cleanup(e2, cleanup)
    e2_timing = _timing_with_runtime_step(e2_response)
    e2_packets = measure_packet_set(e2_response)
    frequency_hz, omega = _frequency_grid()

    records: list[dict[str, Any]] = []
    grid = list(_parameter_grid())
    if limit is not None:
        grid = grid[:limit]
    for index, parameters in enumerate(grid):
        character, structural = _realize_character(
            e2.size, frequency_hz, omega, parameters
        )
        response = _cascade_character_and_cleanup(character, cleanup)
        timing = _timing_with_runtime_step(response)
        score, failures = _timing_violation(timing)
        records.append(
            {
                "identifier": parameters.identifier,
                "parameters": asdict(parameters),
                "structural": structural,
                "timing": timing,
                "timing_delta_vs_e2v3": {
                    key: (
                        None
                        if timing[key] is None
                        else float(timing[key] - e2_timing[key])
                    )
                    for key in BALANCED_TARGETS
                },
                "balanced_target_violation": score,
                "balanced_target_failures": failures,
            }
        )
        if (index + 1) % 72 == 0:
            print(
                f"evaluated {index + 1}/{len(grid)} structural candidates",
                flush=True,
            )

    records.sort(
        key=lambda record: (
            record["balanced_target_violation"],
            len(record["balanced_target_failures"]),
            record["timing"]["maximum_pre_lobe_db_peak"],
        )
    )
    finalists = records[:12]
    output_dir.mkdir(parents=True, exist_ok=True)
    for record in finalists:
        parameters = StructuralParameters(**record["parameters"])
        character, _ = _realize_character(e2.size, frequency_hz, omega, parameters)
        response = _cascade_character_and_cleanup(character, cleanup)
        packets = measure_packet_set(response)
        packet_failures = packet_gate_failures(packets, e2_packets)
        frequency = _frequency_metrics(response)
        frequency_failures = []
        if frequency["passband_ripple_20hz_20khz_db"] > 0.005:
            frequency_failures.append("frequency/passband_ripple")
        if frequency["maximum_stopband_db_24p1khz_nyquist"] > -45.0:
            frequency_failures.append("frequency/stopband")
        record["packets"] = packets
        record["packet_delta_db_vs_e2v3"] = packet_gate_deltas(packets, e2_packets)
        record["packet_failures"] = packet_failures
        record["frequency"] = frequency
        record["frequency_failures"] = frequency_failures
        record["passes_packets_and_frequency"] = not (
            packet_failures or frequency_failures
        )
        payload = np.asarray(character, dtype="<f8").tobytes()
        record["character_sha256"] = _sha256_bytes(payload)
        (output_dir / f"{record['identifier']}.character.f64le").write_bytes(payload)

    report = {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "hypothesis": (
            "Use the otherwise-unexercised 22.05-24.1 kHz guard band and a smooth "
            "-80 dB floor to trade surplus E2v3 rejection for a narrower, shorter "
            "mixed-phase response before performing local phase refinement."
        ),
        "design": {
            "fft_length": FFT_LENGTH,
            "character_support": int(e2.size),
            "global_delay_samples": GLOBAL_DELAY_SAMPLES,
            "candidate_count": len(records),
            "balanced_targets": BALANCED_TARGETS,
            "target_scales": TARGET_SCALES,
            "stopband_gate_start_hz": STOPBAND_START_HZ,
            "stopband_gate_db": -45.0,
            "step_contract": (
                "source-rate step through the exact four polyphase branches; "
                "matches filter_timing_bench rather than the full-rate cumsum proxy"
            ),
        },
        "e2v3": {
            "character_sha256": _sha256_bytes(np.asarray(e2, dtype="<f8").tobytes()),
            "cleanup_stage_1_sha256": _sha256_bytes(
                np.asarray(cleanup, dtype="<f8").tobytes()
            ),
            "timing": e2_timing,
            "packets": e2_packets,
        },
        "external_static_references": _external_summary(root),
        "finalists": finalists,
        "best_identifier": finalists[0]["identifier"] if finalists else None,
        "clear_replacement_found": any(
            not record["balanced_target_failures"]
            and record["passes_packets_and_frequency"]
            for record in finalists
        ),
    }
    _write_json_lf(output_dir / "e3_p11_structural_search.json", report)
    return report


def main() -> None:
    parser = argparse.ArgumentParser(description="Run the E3 P11 structural search")
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[2]
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(__file__).resolve().parent / "work-e3-p11/structural",
    )
    parser.add_argument("--limit", type=int)
    arguments = parser.parse_args()
    report = search(
        arguments.root.resolve(), arguments.output_dir.resolve(), arguments.limit
    )
    print(
        json.dumps(
            {
                "output": str(arguments.output_dir / "e3_p11_structural_search.json"),
                "best_identifier": report["best_identifier"],
                "clear_replacement_found": report["clear_replacement_found"],
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
