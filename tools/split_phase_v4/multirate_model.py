from __future__ import annotations

import json
import math
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable, Union

import numpy as np


def fir_response(coefficients: np.ndarray, omega: float | np.ndarray) -> np.ndarray:
    values = np.asarray(coefficients, dtype=np.float64)
    frequencies = np.atleast_1d(np.asarray(omega, dtype=np.float64))
    index = np.arange(values.size, dtype=np.float64)
    result = np.exp(-1j * frequencies[:, None] * index[None, :]) @ values
    return result if np.ndim(omega) else result[0]


@dataclass(frozen=True)
class CharacterStage:
    canonical: np.ndarray
    origin: int
    phase0_prepad: int
    phase1_prepad: int

    def branch_gains(self, omega_input: float) -> tuple[complex, complex]:
        even = 2.0 * np.asarray(self.canonical[::2], dtype=np.float64)
        odd = 2.0 * np.asarray(self.canonical[1::2], dtype=np.float64)
        odd = np.concatenate((odd, np.zeros(1, dtype=np.float64)))
        length = even.size
        even_runtime = even[::-1]
        odd_runtime = odd[::-1]
        index = np.arange(length, dtype=np.float64)
        tone = np.exp(1j * omega_input * index)
        gain0 = np.dot(even_runtime, tone) * np.exp(
            -1j * omega_input * self.phase0_prepad
        )
        gain1 = np.dot(odd_runtime, tone) * np.exp(
            -1j * omega_input * self.phase1_prepad
        )
        return complex(gain0), complex(gain1)

    def decimation_gain(self, omega_input: float) -> complex:
        response = fir_response(self.canonical, omega_input)
        return complex(response * np.exp(1j * omega_input * self.origin))


@dataclass(frozen=True)
class CleanupStage:
    canonical: np.ndarray

    @property
    def runtime_branch_taps(self) -> int:
        return self.canonical.size // 2 + 1

    def branch_gains(self, omega_input: float) -> tuple[complex, complex]:
        odd = 2.0 * np.asarray(self.canonical[1::2], dtype=np.float64)
        odd = np.concatenate((odd, np.zeros(1, dtype=np.float64)))
        runtime = odd[::-1]
        prepad = runtime.size // 2
        index = np.arange(runtime.size, dtype=np.float64)
        gain1 = np.dot(runtime, np.exp(1j * omega_input * index)) * np.exp(
            -1j * omega_input * prepad
        )
        return 1.0 + 0.0j, complex(gain1)

    def decimation_gain(self, omega_input: float) -> complex:
        center = self.canonical.size // 2
        response = fir_response(self.canonical, omega_input)
        return complex(response * np.exp(1j * omega_input * center))


InterpolationStage = Union[CharacterStage, CleanupStage]


@dataclass(frozen=True)
class RationalTable:
    rows: np.ndarray
    step_num: int
    phase_den: int

    @property
    def half_width(self) -> int:
        return (self.rows.shape[1] - 1) // 2

    def phase_gain(self, omega_input: float, phase: int) -> complex:
        row = np.asarray(self.rows[phase], dtype=np.float64)
        tap = np.arange(row.size, dtype=np.float64)
        direct = np.dot(row, np.exp(1j * omega_input * tap))
        return complex(direct * np.exp(-1j * omega_input * (self.half_width + phase / self.phase_den)))

    def exact_accumulator_gains(self, count: int, omega_input: float) -> list[tuple[int, int, complex]]:
        current_time_num = self.half_width * self.phase_den
        result = []
        for _ in range(count):
            index = current_time_num // self.phase_den
            phase = current_time_num % self.phase_den
            result.append((index, phase, self.phase_gain(omega_input, phase)))
            current_time_num += self.step_num
        return result


def split_interpolation_tone(
    omega_input: float, amplitude: complex, stage: InterpolationStage
) -> tuple[tuple[float, complex], tuple[float, complex]]:
    gain0, gain1 = stage.branch_gains(omega_input)
    base = (omega_input % (2.0 * np.pi)) / 2.0
    first = 0.5 * amplitude * (gain0 + gain1 * np.exp(-1j * base))
    second = 0.5 * amplitude * (gain0 - gain1 * np.exp(-1j * base))
    return ((base, first), ((base + np.pi) % (2.0 * np.pi), second))


def interpolation_paths(
    omega_input: float, stages: Iterable[InterpolationStage]
) -> list[tuple[float, complex]]:
    paths: list[tuple[float, complex]] = [(omega_input % (2.0 * np.pi), 1.0 + 0.0j)]
    for stage in stages:
        expanded: list[tuple[float, complex]] = []
        for frequency, amplitude in paths:
            expanded.extend(split_interpolation_tone(frequency, amplitude, stage))
        paths = expanded
    return sorted(paths, key=lambda value: value[0])


def decimation_path_gain(
    omega_input: float, stages_in_runtime_order: Iterable[InterpolationStage]
) -> tuple[float, complex]:
    frequency = omega_input % (2.0 * np.pi)
    gain = 1.0 + 0.0j
    for stage in stages_in_runtime_order:
        gain *= stage.decimation_gain(frequency)
        frequency = (2.0 * frequency) % (2.0 * np.pi)
    return frequency, gain


def decimation_alias_paths(
    omega_output: float, stages_in_runtime_order: list[InterpolationStage]
) -> list[tuple[float, complex]]:
    ratio = 1 << len(stages_in_runtime_order)
    paths = []
    for alias_index in range(ratio):
        input_frequency = (omega_output + 2.0 * np.pi * alias_index) / ratio
        realized, gain = decimation_path_gain(input_frequency, stages_in_runtime_order)
        phase_error = abs(np.angle(np.exp(1j * (realized - omega_output))))
        if phase_error > 2.0e-12:
            raise RuntimeError("decimation frequency propagation mismatch")
        paths.append((input_frequency, gain))
    return paths


def _direct_branch_gain(
    runtime_coefficients: np.ndarray, prepad: int, omega_input: float
) -> complex:
    coefficients = np.asarray(runtime_coefficients, dtype=np.float64)
    # Directly execute the steady-state sliding dot product used by FirEngine.
    indices = np.arange(coefficients.size, dtype=np.float64) - prepad
    samples = np.exp(1j * omega_input * indices)
    return complex(np.dot(coefficients, samples))


def _direct_interpolation_stage(
    input_paths: list[tuple[float, complex]], stage: InterpolationStage, count: int = 1024
) -> np.ndarray:
    output = np.zeros(count * 2, dtype=np.complex128)
    for omega_input, amplitude in input_paths:
        if isinstance(stage, CharacterStage):
            even = 2.0 * stage.canonical[::2]
            odd = np.concatenate((2.0 * stage.canonical[1::2], [0.0]))
            gain0 = _direct_branch_gain(even[::-1], stage.phase0_prepad, omega_input)
            gain1 = _direct_branch_gain(odd[::-1], stage.phase1_prepad, omega_input)
        else:
            odd = np.concatenate((2.0 * stage.canonical[1::2], [0.0]))
            gain0 = 1.0 + 0.0j
            gain1 = _direct_branch_gain(odd[::-1], odd.size // 2, omega_input)
        index = np.arange(count, dtype=np.float64)
        tone = amplitude * np.exp(1j * omega_input * index)
        output[::2] += gain0 * tone
        output[1::2] += gain1 * tone
    return output


def _random_character(rng: np.random.Generator, length: int = 17) -> CharacterStage:
    values = rng.normal(size=length)
    values[::2] *= 0.5 / math.fsum(float(value) for value in values[::2])
    values[1::2] *= 0.5 / math.fsum(float(value) for value in values[1::2])
    origin = 4
    branch_length = (length + 1) // 2
    prepad = branch_length - 1 - origin // 2
    return CharacterStage(values, origin, prepad, prepad)


def _random_cleanup(rng: np.random.Generator, branch_taps: int = 7) -> CleanupStage:
    length = 2 * branch_taps - 1
    values = np.zeros(length, dtype=np.float64)
    center = length // 2
    values[center] = 0.5
    positive_odd = np.arange(1, center, 2)
    random = rng.normal(size=positive_odd.size)
    if random.size:
        random *= 0.25 / math.fsum(float(value) for value in random)
        values[positive_odd] = random
        values[-positive_odd - 1] = random
    return CleanupStage(values)


def _synthesize_paths(paths: list[tuple[float, complex]], count: int) -> np.ndarray:
    index = np.arange(count, dtype=np.float64)
    result = np.zeros(count, dtype=np.complex128)
    for frequency, amplitude in paths:
        result += amplitude * np.exp(1j * frequency * index)
    return result


def _direct_decimation_stage_gain(stage: InterpolationStage, omega_input: float) -> complex:
    if isinstance(stage, CharacterStage):
        runtime = np.asarray(stage.canonical[::-1], dtype=np.float64)
        prepad = runtime.size - 1 - stage.origin
    else:
        runtime = np.asarray(stage.canonical[::-1], dtype=np.float64)
        prepad = runtime.size // 2
    return _direct_branch_gain(runtime, prepad, omega_input)


def _direct_decimation_path_gain(
    omega_input: float, stages_in_runtime_order: Iterable[InterpolationStage]
) -> tuple[float, complex]:
    frequency = omega_input % (2.0 * np.pi)
    gain = 1.0 + 0.0j
    for stage in stages_in_runtime_order:
        gain *= _direct_decimation_stage_gain(stage, frequency)
        frequency = (2.0 * frequency) % (2.0 * np.pi)
    return frequency, gain


def verify_complete_model(seed: int, report_path: Path) -> dict[str, Any]:
    rng = np.random.default_rng(seed)
    cases: list[dict[str, Any]] = []
    worst_interpolation = 0.0
    worst_decimation = 0.0
    worst_alias = 0.0
    worst_rational = 0.0

    for exponent in range(1, 9):
        stages: list[InterpolationStage] = [_random_character(rng)]
        stages.extend(_random_cleanup(rng) for _ in range(exponent - 1))
        omega_input = float(rng.uniform(0.071, 2.71))
        analytical = interpolation_paths(omega_input, stages)
        direct_paths = [(omega_input, 1.0 + 0.0j)]
        direct = np.empty(0, dtype=np.complex128)
        stage_interpolation_error = 0.0
        for stage_index, stage in enumerate(stages):
            direct = _direct_interpolation_stage(direct_paths, stage, count=1024)
            direct_paths = interpolation_paths(omega_input, stages[: stage_index + 1])
            expected_samples = _synthesize_paths(direct_paths, direct.size)
            sample_error = float(
                np.max(np.abs(direct - expected_samples))
                / max(float(np.max(np.abs(expected_samples))), 1.0e-15)
            )
            stage_interpolation_error = max(stage_interpolation_error, sample_error)
        interpolation_error = stage_interpolation_error
        worst_interpolation = max(worst_interpolation, interpolation_error)

        reverse = list(reversed(stages))
        omega_output = float(rng.uniform(0.031, 0.91 * np.pi))
        aliases = decimation_alias_paths(omega_output, reverse)
        direct_aliases: list[complex] = []
        expected_aliases: list[complex] = []
        case_alias_error = 0.0
        for alias_frequency, expected_gain in aliases:
            frequency, direct_gain = _direct_decimation_path_gain(alias_frequency, reverse)
            direct_aliases.append(direct_gain)
            expected_aliases.append(expected_gain)
            frequency_error = abs(np.angle(np.exp(1j * (frequency - omega_output))))
            case_alias_error = max(case_alias_error, frequency_error)
            worst_alias = max(worst_alias, frequency_error)
        direct_alias_array = np.asarray(direct_aliases)
        expected_alias_array = np.asarray(expected_aliases)
        case_decimation_error = float(
            np.max(np.abs(direct_alias_array - expected_alias_array))
            / max(float(np.max(np.abs(expected_alias_array))), 1.0e-15)
        )
        worst_decimation = max(worst_decimation, case_decimation_error)
        cases.append(
            {
                "ratio": 1 << exponent,
                "interpolation_relative_error": interpolation_error,
                "decimation_relative_error": case_decimation_error,
                "alias_frequency_propagation_error_rad": case_alias_error,
            }
        )

    rational_cases = []
    for step_num, phase_den in ((147, 160), (160, 147)):
        rows = rng.normal(size=(phase_den, 9))
        rows /= np.sum(rows, axis=1)[:, None]
        table = RationalTable(rows, step_num, phase_den)
        omega = float(rng.uniform(0.041, 2.81))
        case_error = 0.0
        for index, phase, analytical_gain in table.exact_accumulator_gains(2 * phase_den, omega):
            start = index - table.half_width
            sample_index = start + np.arange(rows.shape[1], dtype=np.float64)
            direct_output = np.dot(rows[phase], np.exp(1j * omega * sample_index))
            ideal_output = np.exp(1j * omega * (index + phase / phase_den))
            direct_gain = direct_output / ideal_output
            case_error = max(case_error, abs(direct_gain - analytical_gain) / max(abs(analytical_gain), 1.0e-15))
        worst_rational = max(worst_rational, case_error)
        rational_cases.append({"step_num": step_num, "phase_den": phase_den, "relative_error": case_error})

    report = {
        "seed": seed,
        "cases": cases,
        "worst_complex_interpolation_relative_error": worst_interpolation,
        "worst_complex_decimation_relative_error": worst_decimation,
        "worst_alias_path_relative_error": worst_alias,
        "rational_cases": rational_cases,
        "worst_exact_rational_relative_error": worst_rational,
        "accepted": bool(
            worst_interpolation <= 1.0e-11
            and worst_decimation <= 1.0e-11
            and worst_alias <= 1.0e-11
            and worst_rational <= 1.0e-11
        ),
    }
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    if not report["accepted"]:
        raise RuntimeError(
            "complete multirate model verification failed: "
            f"interpolation={worst_interpolation}, decimation={worst_decimation}, "
            f"alias={worst_alias}, rational={worst_rational}"
        )
    return report


if __name__ == "__main__":
    root = Path(__file__).resolve().parents[2]
    result = verify_complete_model(128004, root / "tools/split_phase_v4/work/model.json")
    print(json.dumps(result, indent=2))
