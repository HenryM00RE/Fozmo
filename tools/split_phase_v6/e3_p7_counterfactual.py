from __future__ import annotations

import hashlib
import math
from dataclasses import asdict, dataclass
from typing import Any, Iterable

import numpy as np
from scipy import signal


SOURCE_RATE_HZ = 44_100
INTERVALS_MS = ((0.0, 2.0), (2.0, 5.0), (5.0, 10.0), (10.0, 25.0), (25.0, 50.0))
TRACE_SECONDS = 0.050
WINDOW_SECONDS = 0.002
MATCHED_EFFECTIVE_PEAK = 0.630_326_387_135_713_1


@dataclass(frozen=True)
class CounterfactualFixture:
    name: str
    frequencies_hz: tuple[float, ...]
    phases_rad: tuple[float, ...]
    mute_source_frames: int
    effective_peak: float = MATCHED_EFFECTIVE_PEAK

    def __post_init__(self) -> None:
        if len(self.frequencies_hz) != len(self.phases_rad) or not self.frequencies_hz:
            raise ValueError("frequencies and phases must have the same non-zero length")
        if self.mute_source_frames <= 0:
            raise ValueError("mute_source_frames must be positive")

    @property
    def carrier_amplitude(self) -> float:
        frame = np.arange(16_384, dtype=np.float64)
        raw = sum(
            np.sin(2.0 * np.pi * frequency * frame / SOURCE_RATE_HZ + phase)
            for frequency, phase in zip(self.frequencies_hz, self.phases_rad, strict=True)
        )
        return self.effective_peak / float(np.max(np.abs(raw)))


class CounterfactualResidual:
    """Exact LTI mute-minus-continuous restart residual for an FIR response."""

    def __init__(
        self,
        fixture: CounterfactualFixture,
        output_rate_hz: int,
        response_origin: int,
        sample_start: int = 0,
        trace_seconds: float = TRACE_SECONDS,
    ) -> None:
        if output_rate_hz % SOURCE_RATE_HZ:
            raise ValueError("output rate must be an integer multiple of 44.1 kHz")
        self.fixture = fixture
        self.output_rate_hz = output_rate_hz
        self.ratio = output_rate_hz // SOURCE_RATE_HZ
        self.response_origin = response_origin
        self.sample_start = sample_start
        self.trace_samples = round(output_rate_hz * trace_seconds)
        self.sample_indices = np.arange(
            sample_start, sample_start + self.trace_samples, dtype=np.int64
        )
        self.aligned_indices = self.sample_indices + response_origin

    def residual(self, response: np.ndarray) -> np.ndarray:
        response = np.asarray(response, dtype=np.float64)
        residual = np.zeros(self.trace_samples, dtype=np.float64)
        amplitude = self.fixture.carrier_amplitude
        for frequency_hz, phase_rad in zip(
            self.fixture.frequencies_hz, self.fixture.phases_rad, strict=True
        ):
            omega = 2.0 * np.pi * frequency_hz / SOURCE_RATE_HZ
            for residue in range(self.ratio):
                selected = self.aligned_indices % self.ratio == residue
                source_offset = (self.aligned_indices[selected] - residue) // self.ratio
                polyphase = response[residue :: self.ratio]
                coefficient_index = np.arange(polyphase.size, dtype=np.float64)
                weighted = polyphase * np.exp(-1j * omega * coefficient_index)
                prefix = np.concatenate(([0.0j], np.cumsum(weighted)))
                lower = np.clip(source_offset + 1, 0, polyphase.size)
                upper = np.clip(
                    source_offset + self.fixture.mute_source_frames + 1,
                    0,
                    polyphase.size,
                )
                missing = prefix[upper] - prefix[lower]
                residual[selected] -= amplitude * np.imag(
                    np.exp(1j * (phase_rad + omega * source_offset)) * missing
                )
        return residual


def cleanup_counterfactual_residual(
    character: np.ndarray,
    cleanup: np.ndarray,
    fixture: CounterfactualFixture,
    character_origin: int = 2_530,
    output_rate_hz: int = 176_400,
    trace_seconds: float = TRACE_SECONDS,
) -> np.ndarray:
    cleanup = np.asarray(cleanup, dtype=np.float64)
    cleanup_center = cleanup.size // 2
    pre_samples = math.ceil(cleanup_center / 2) + 4
    post_samples = round(88_200 * trace_seconds) + pre_samples + 4
    character_residual = character_counterfactual_residual(
        character,
        fixture,
        character_origin=character_origin,
        sample_start=-pre_samples,
        sample_count=pre_samples + post_samples,
    )
    cascaded = signal.upfirdn(2.0 * cleanup, character_residual, up=2)
    zero_index = pre_samples * 2 + cleanup_center
    wanted = round(output_rate_hz * trace_seconds)
    return np.asarray(cascaded[zero_index : zero_index + wanted], dtype=np.float64)


def character_counterfactual_residual(
    character: np.ndarray,
    fixture: CounterfactualFixture,
    character_origin: int = 2_530,
    sample_start: int = 0,
    sample_count: int | None = None,
) -> np.ndarray:
    count = sample_count if sample_count is not None else round(88_200 * TRACE_SECONDS)
    probe = CounterfactualResidual(
        fixture,
        88_200,
        character_origin,
        sample_start=sample_start,
        trace_seconds=count / 88_200,
    )
    return probe.residual(2.0 * np.asarray(character, dtype=np.float64))


def interval_metrics(
    residual: np.ndarray,
    sample_rate_hz: int = 176_400,
    intervals_ms: Iterable[tuple[float, float]] = INTERVALS_MS,
) -> list[dict[str, float]]:
    reports = []
    for start_ms, end_ms in intervals_ms:
        start = round(start_ms * sample_rate_hz / 1000.0)
        end = round(end_ms * sample_rate_hz / 1000.0)
        mean_square = float(np.mean(np.asarray(residual[start:end]) ** 2))
        reports.append(
            {
                "start_ms": start_ms,
                "end_ms": end_ms,
                "residual_rms_dbfs": 10.0
                * math.log10(max(2.0 * mean_square, 1.0e-300)),
                "residual_energy_linear_seconds": mean_square / sample_rate_hz * (end - start),
            }
        )
    return reports


def default_training_fixtures() -> tuple[CounterfactualFixture, ...]:
    fixtures = []
    pairs = ((18_000.0, 19_000.0), (17_000.0, 19_500.0), (15_000.0, 20_000.0))
    phases = ((0.31 + math.pi, 1.17 + math.pi), (1.07, 2.29))
    mute_lengths = (2_048, 8_192)
    for pair_index, frequencies in enumerate(pairs):
        coherent = tuple(
            round(frequency / (SOURCE_RATE_HZ / 16_384)) * (SOURCE_RATE_HZ / 16_384)
            for frequency in frequencies
        )
        for phase_index, phase in enumerate(phases):
            for mute in mute_lengths:
                fixtures.append(
                    CounterfactualFixture(
                        name=f"pair{pair_index}-phase{phase_index}-mute{mute}",
                        frequencies_hz=coherent,
                        phases_rad=phase,
                        mute_source_frames=mute,
                    )
                )
    return tuple(fixtures)


def fixture_contract(fixture: CounterfactualFixture) -> dict[str, Any]:
    result = asdict(fixture)
    result["carrier_amplitude"] = fixture.carrier_amplitude
    payload = np.asarray(
        (*fixture.frequencies_hz, *fixture.phases_rad, fixture.mute_source_frames),
        dtype="<f8",
    ).tobytes()
    result["parameter_sha256"] = hashlib.sha256(payload).hexdigest()
    return result
