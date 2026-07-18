from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path
from typing import Any, Optional, Tuple

import numpy as np

from .multirate_model import CharacterStage, CleanupStage, decimation_alias_paths, interpolation_paths


RATE = 88_200.0
SOURCE_RATE = 44_100.0
SUPPORT = 262_145
DESIGN_FFT_LEN = 1 << 24
METRIC_FFT_LEN = 1 << 23


def _fsum(values: np.ndarray) -> float:
    return math.fsum(float(value) for value in values)


def _prototype() -> np.ndarray:
    half_width = 65_536
    position = np.arange(SUPPORT, dtype=np.float64) * 0.5 - half_width
    radius = np.clip(position / half_width, -1.0, 1.0)
    window = np.i0(23.12088 * np.sqrt(np.maximum(1.0 - radius**2, 0.0))) / np.i0(23.12088)
    result = 2.0 * 0.465333 * np.sinc(2.0 * 0.465333 * position) * window
    result /= _fsum(result)
    return result


def _minimum_spectrum(magnitude: np.ndarray, fft_len: int) -> np.ndarray:
    cepstrum = np.fft.irfft(np.log(np.maximum(magnitude, 1.0e-30)), n=fft_len)
    cepstrum[1 : fft_len // 2] *= 2.0
    cepstrum[fft_len // 2 + 1 :] = 0.0
    return np.exp(np.fft.rfft(cepstrum, n=fft_len))


def _smootherstep7(value: np.ndarray) -> np.ndarray:
    t = np.clip(value, 0.0, 1.0)
    return t**4 * (35.0 + t * (-84.0 + t * (70.0 - 20.0 * t)))


def _v2_phase(minimum_phase: np.ndarray, fft_len: int) -> np.ndarray:
    lo = 3000.0 / RATE
    hi = 14000.0 / RATE
    floor = 0.038155
    lo_bin = int(round(lo * fft_len))
    join_bin = int(math.ceil(hi * fft_len + 0.5))
    reference_increment = minimum_phase[lo_bin] / lo_bin
    result = np.zeros_like(minimum_phase)
    index = np.arange(lo_bin + 1)
    result[: lo_bin + 1] = (1.0 - floor) * reference_increment * index + floor * minimum_phase[: lo_bin + 1]
    bins = np.arange(lo_bin + 1, join_bin + 1)
    frequency_mid = (bins - 0.5) / fft_len
    log_t = (np.log(frequency_mid) - math.log(lo)) / (math.log(hi) - math.log(lo))
    weight = floor + (1.0 - floor) * _smootherstep7(log_t)
    base = (1.0 - weight) * reference_increment + weight * np.diff(minimum_phase[lo_bin : join_bin + 1])
    bump = np.maximum(log_t, 0.0) ** 4 * np.maximum(1.0 - log_t, 0.0) ** 4
    amplitude = (minimum_phase[join_bin] - result[lo_bin] - np.sum(base)) / np.sum(bump)
    result[lo_bin + 1 : join_bin + 1] = result[lo_bin] + np.cumsum(base + amplitude * bump)
    result[join_bin:] = minimum_phase[join_bin:]
    return result


def _v1_phase(minimum_phase: np.ndarray, magnitude: np.ndarray, fft_len: int) -> np.ndarray:
    frequency = np.arange(minimum_phase.size, dtype=np.float64) / fft_len
    lo = 3000.0 / RATE
    hi = 14000.0 / RATE
    lo_bin = int(round(lo * fft_len))
    reference_delay = -minimum_phase[lo_bin] * fft_len / (2.0 * np.pi * lo_bin)
    t = np.clip((np.log(np.maximum(frequency, lo)) - math.log(lo)) / (math.log(hi) - math.log(lo)), 0.0, 1.0)
    smooth = t**3 * (t * (t * 6.0 - 15.0) + 10.0)
    blend = 0.038155 + (1.0 - 0.038155) * smooth
    blend[frequency <= lo] = 0.038155
    blend[frequency >= hi] = 1.0
    blend[magnitude <= np.max(magnitude) * 1.0e-6] = 1.0
    linear = -2.0 * np.pi * frequency * reference_delay
    return (1.0 - blend) * linear + blend * minimum_phase


def _procedural_character(version: str) -> Tuple[np.ndarray, np.ndarray]:
    prototype = _prototype()
    linear = np.fft.rfft(prototype, n=DESIGN_FFT_LEN)
    magnitude = np.abs(linear)
    minimum = _minimum_spectrum(np.maximum(magnitude, np.max(magnitude) * 1.0e-12), DESIGN_FFT_LEN)
    minimum_phase = np.unwrap(np.angle(minimum))
    reliable = np.abs(minimum) > np.max(magnitude) * 1.0e-6
    last = np.maximum.accumulate(np.where(reliable, np.arange(minimum_phase.size), 0))
    minimum_phase = minimum_phase[last]
    phase = _v1_phase(minimum_phase, magnitude, DESIGN_FFT_LEN) if version == "A" else _v2_phase(minimum_phase, DESIGN_FFT_LEN)
    shift = (SUPPORT // 64) * 1.040606
    frequency = np.arange(phase.size, dtype=np.float64) / DESIGN_FFT_LEN
    target = magnitude * np.exp(1j * (phase - 2.0 * np.pi * frequency * shift))
    target[0] = target[0].real
    target[-1] = target[-1].real
    impulse = np.fft.irfft(target, n=DESIGN_FFT_LEN)[:SUPPORT]
    fade_length = min(max(int(round(SUPPORT * 0.005621)), 8), 2048, SUPPORT // 4)
    fade = 0.5 * (1.0 + np.cos(np.pi * np.arange(fade_length) / (fade_length - 1)))
    impulse[-fade_length:] *= fade
    impulse /= _fsum(impulse)
    return impulse, target


def _procedural_cleanups() -> list[CleanupStage]:
    result = []
    for taps in (255, 127, 63, 31, 31, 31, 31):
        half_width = taps // 2
        full_length = 4 * half_width + 1
        position = np.arange(full_length, dtype=np.float64) * 0.5 - half_width
        radius = position / half_width
        window = np.i0(23.12088 * np.sqrt(np.maximum(1.0 - radius**2, 0.0))) / np.i0(23.12088)
        canonical = np.sinc(2.0 * 0.5 * position) * window
        canonical /= _fsum(canonical)
        result.append(CleanupStage(canonical))
    return result


def _load_c(root: Path) -> Tuple[np.ndarray, list[CleanupStage], int, Optional[np.ndarray]]:
    asset = root / "assets/filters/split_phase_v3"
    manifest = json.loads((asset / "manifest.json").read_text())
    character = np.fromfile(asset / "character_full_rate.f64le", dtype="<f8")
    cleanups = [CleanupStage(np.fromfile(asset / entry["file"], dtype="<f8")) for entry in manifest["files"]["cleanups"]]
    origin = int(manifest["alignment"]["full_rate_origin"])
    magnitude_path = root / "tools/split_phase_v3/work/sdp_magnitude.npy"
    phase_path = root / "tools/split_phase_v3/work/target_residual_phase.npy"
    target = None
    if magnitude_path.exists() and phase_path.exists():
        magnitude = np.load(magnitude_path, mmap_mode="r")
        phase = np.load(phase_path, mmap_mode="r")
        omega = np.linspace(0.0, np.pi, magnitude.size)
        target = np.asarray(magnitude) * np.exp(1j * (np.asarray(phase) - omega * origin))
    return character, cleanups, origin, target


def _db_ratio(numerator: float, denominator: float) -> float:
    return 10.0 * math.log10(max(numerator / max(denominator, 1.0e-300), 1.0e-300))


def _log_delay_metrics(coefficients: np.ndarray) -> dict[str, Any]:
    fft_len = METRIC_FFT_LEN
    response = np.fft.rfft(coefficients, n=fft_len)
    weighted = np.fft.rfft(np.arange(coefficients.size, dtype=np.float64) * coefficients, n=fft_len)
    frequencies = np.geomspace(20.0, 20_000.0, 4096)
    bins = frequencies * fft_len / RATE
    lo = np.floor(bins).astype(np.int64)
    fraction = bins - lo
    h = response[lo] * (1.0 - fraction) + response[lo + 1] * fraction
    nh = weighted[lo] * (1.0 - fraction) + weighted[lo + 1] * fraction
    delay = np.real(nh / h)
    coordinate = np.log(frequencies)
    slope = np.gradient(delay, coordinate, edge_order=2)
    curvature = np.gradient(slope, coordinate, edge_order=2)
    return {
        "grid": {"coordinate": "ln(f/Hz)", "minimum_hz": 20.0, "maximum_hz": 20_000.0, "points": 4096},
        "group_delay_slope_max_abs_samples_per_ln_hz": float(np.max(np.abs(slope))),
        "group_delay_curvature_max_abs_samples_per_ln_hz_squared": float(np.max(np.abs(curvature))),
    }


def _multirate_metrics(character: np.ndarray, cleanups: list[CleanupStage], origin: int) -> dict[str, float]:
    branch_length = character.size // 2 + 1
    branch_origin0 = (origin + 1) // 2
    branch_origin1 = origin // 2
    stage = CharacterStage(character, origin, branch_length - 1 - branch_origin0, branch_length - 1 - branch_origin1)
    worst_image = 0.0
    worst_alias = 0.0
    for exponent in range(1, 9):
        stages = [stage] + cleanups[: exponent - 1]
        reverse = list(reversed(stages))
        for frequency_hz in np.linspace(20.0, 20_000.0, 96):
            paths = interpolation_paths(2.0 * np.pi * frequency_hz / SOURCE_RATE, stages)
            desired = max(abs(paths[0][1]), 1.0e-300)
            worst_image = max(worst_image, max(abs(value[1]) for value in paths[1:]) / desired)
            output_omega = 2.0 * np.pi * frequency_hz / SOURCE_RATE
            aliases = decimation_alias_paths(output_omega, reverse)
            desired_alias = max(abs(aliases[0][1]), 1.0e-300)
            worst_alias = max(worst_alias, max(abs(value[1]) for value in aliases[1:]) / desired_alias)
    return {
        "worst_interpolation_image_db": 20.0 * math.log10(max(worst_image, 1.0e-300)),
        "worst_independent_decimation_alias_db": 20.0 * math.log10(max(worst_alias, 1.0e-300)),
    }


def _metrics(character: np.ndarray, cleanups: list[CleanupStage], origin: int, target: Optional[np.ndarray]) -> dict[str, Any]:
    total = float(np.dot(character, character))
    peak = int(np.argmax(np.abs(character)))
    before_origin = float(np.dot(character[:origin], character[:origin]))
    before_peak = float(np.dot(character[:peak], character[:peak]))
    response = np.fft.rfft(character, n=METRIC_FFT_LEN)
    frequencies = np.linspace(0.0, RATE / 2.0, response.size)
    band_3_14 = np.fft.irfft(response * ((frequencies >= 3000.0) & (frequencies <= 14000.0)), n=METRIC_FFT_LEN)[: character.size]
    band_14_20 = np.fft.irfft(response * ((frequencies >= 14000.0) & (frequencies <= 20000.0)), n=METRIC_FFT_LEN)[: character.size]
    samples_100us = int(round(RATE * 100.0e-6))
    samples_500us = int(round(RATE * 500.0e-6))
    step = np.cumsum(character)
    result: dict[str, Any] = {
        "metric_definition_version": 1,
        "logical_origin": origin,
        "dominant_peak_index": peak,
        "pre_energy_before_exported_logical_origin_db": _db_ratio(before_origin, total),
        "pre_energy_before_dominant_peak_db": _db_ratio(before_peak, total),
        "bandlimited_3_14khz_pre_peak_energy_db": _db_ratio(float(np.dot(band_3_14[:peak], band_3_14[:peak])), float(np.dot(band_3_14, band_3_14))),
        "bandlimited_14_20khz_pre_peak_energy_db": _db_ratio(float(np.dot(band_14_20[:peak], band_14_20[:peak])), float(np.dot(band_14_20, band_14_20))),
        "post_peak_0_100us_energy_db": _db_ratio(float(np.dot(character[peak + 1 : peak + 1 + samples_100us], character[peak + 1 : peak + 1 + samples_100us])), total),
        "post_peak_100_500us_energy_db": _db_ratio(float(np.dot(character[peak + 1 + samples_100us : peak + 1 + samples_500us], character[peak + 1 + samples_100us : peak + 1 + samples_500us])), total),
        "step_response_overshoot": float(max(np.max(step) - 1.0, -np.min(step), 0.0)),
    }
    result.update(_log_delay_metrics(character))
    result.update(_multirate_metrics(character, cleanups, origin))
    if target is not None:
        stride = DESIGN_FFT_LEN // METRIC_FFT_LEN
        reference = target[::stride]
        pass_mask = frequencies <= 20_000.0
        result["worst_complex_passband_error"] = float(np.max(np.abs(response[pass_mask] - reference[pass_mask])))
    else:
        result["worst_complex_passband_error"] = None
    return result


def _freeze_c_hashes(root: Path) -> dict[str, Any]:
    asset = root / "assets/filters/split_phase_v3"
    manifest = json.loads((asset / "manifest.json").read_text())
    entries = [manifest["files"]["character"], manifest["files"]["rational_147_160"], manifest["files"]["rational_160_147"]] + manifest["files"]["cleanups"]
    files = {}
    for entry in entries:
        payload = (asset / entry["file"]).read_bytes()
        digest = hashlib.sha256(payload).hexdigest()
        if digest != entry["sha256"]:
            raise RuntimeError("Split Phase C asset hash changed: " + entry["file"])
        files[entry["file"]] = digest
    return {"identity": "SplitPhase128kV3", "immutable_asset_hashes": files, "all_match_manifest": True}


def generate(root: Path) -> None:
    output = root / "tools/split_phase_v4/baselines"
    output.mkdir(parents=True, exist_ok=True)
    frozen = _freeze_c_hashes(root)
    records = {}
    procedural_cleanups = _procedural_cleanups()
    for version, identity in (("A", "Split128k"), ("B", "Split128kV2")):
        character, target = _procedural_character(version)
        origin = int(np.argmax(np.abs(character)))
        record = {"identity": identity, "coefficient_sha256": hashlib.sha256(np.asarray(character, dtype="<f8").tobytes()).hexdigest(), "metrics": _metrics(character, procedural_cleanups, origin, target)}
        records[version] = record
        (output / ("split_" + version.lower() + ".json")).write_text(json.dumps(record, indent=2) + "\n")
    character_c, cleanups_c, origin_c, target_c = _load_c(root)
    record_c = {"identity": "SplitPhase128kV3", "coefficient_sha256": hashlib.sha256(np.asarray(character_c, dtype="<f8").tobytes()).hexdigest(), "frozen": frozen, "metrics": _metrics(character_c, cleanups_c, origin_c, target_c)}
    records["C"] = record_c
    (output / "split_c.json").write_text(json.dumps(record_c, indent=2) + "\n")
    comparison = {"metric_definition_version": 1, "same_metric_definitions": True, "filters": {key: value["identity"] for key, value in records.items()}, "metrics": {key: value["metrics"] for key, value in records.items()}}
    (output / "comparison.json").write_text(json.dumps(comparison, indent=2) + "\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    arguments = parser.parse_args()
    generate(arguments.root.resolve())
