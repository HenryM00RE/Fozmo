from __future__ import annotations

import argparse
import hashlib
import json
import math
from dataclasses import asdict
from pathlib import Path
from typing import Any

import numpy as np

from .e3_p5_group_delay_search import CLOSURE_HZ, STRUCTURES, _build_model
from .e3_p7_cleanup_search import _frequency_metrics
from .e3_p7_counterfactual import (
    cleanup_counterfactual_residual,
    default_training_fixtures,
    fixture_contract,
    interval_metrics,
)
from .e3_phase_search import (
    CHARACTER_RATE_HZ,
    FFT_LENGTH,
    _cascade_character_and_cleanup,
    _read_f64le,
    _timing_metrics,
)
from .evaluate_e3_packets import PACKET_FREQUENCIES_HZ, _measure_packet


IDENTITY = "SplitPhase128kE3-P8-structural-capacity-audit"
MODEL_ORDERS = (512, 768, 1_024)
CHARACTER_SUPPORTS = (262_145, 524_289)
P6_IDENTIFIER = "p6d-local-0145"


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _p6_record(root: Path) -> dict[str, Any]:
    report_path = root / "tools/split_phase_v6/baselines/e3-p6-local-refine.json"
    report = json.loads(report_path.read_text(encoding="utf-8"))
    return next(record for record in report["records"] if record["identifier"] == P6_IDENTIFIER)


def _p6_target(root: Path) -> tuple[np.ndarray, np.ndarray, dict[str, Any]]:
    refine = _read_f64le(root / "assets/filters/split_phase_e3/character_full_rate.f64le")
    e2 = _read_f64le(root / "assets/filters/split_phase_e2v3/character_full_rate.f64le")
    record = _p6_record(root)
    structure = next(structure for structure in STRUCTURES if structure[0] == record["family"])
    model = _build_model(*structure[1:])
    frequency = np.fft.rfftfreq(FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ)
    anchor_phase = np.unwrap(np.angle(np.fft.rfft(refine, FFT_LENGTH)))
    magnitude = np.abs(np.fft.rfft(e2, FFT_LENGTH))
    active = (frequency >= structure[1]) & (frequency <= CLOSURE_HZ)
    active_frequency = frequency[active]
    delay_delta = model.evaluate(active_frequency, np.asarray(record["free"], dtype=np.float64))
    omega = 2.0 * np.pi * active_frequency / CHARACTER_RATE_HZ
    phase_delta_active = np.zeros_like(active_frequency)
    phase_delta_active[1:] = -np.cumsum(
        0.5 * (delay_delta[1:] + delay_delta[:-1]) * np.diff(omega)
    )
    phase_delta = np.zeros_like(frequency)
    phase_delta[active] = phase_delta_active
    target = magnitude * np.exp(1j * (anchor_phase + phase_delta))
    target[0] = complex(float(target[0].real), 0.0)
    target[-1] = complex(float(target[-1].real), 0.0)
    periodic = np.fft.irfft(target, FFT_LENGTH)
    contract = {
        "identifier": P6_IDENTIFIER,
        "family": record["family"],
        "free_coordinates": record["free"],
        "reported_character_sha256": record["character_sha256"],
        "reported_omitted_periodic_energy_ratio": record["structural"][
            "omitted_periodic_energy_ratio"
        ],
        "phase_closure_error_rad": float(phase_delta_active[-1]),
        "periodic_target_energy": float(np.dot(periodic, periodic)),
        "normalization_sum": float(math.fsum(float(value) for value in refine)),
    }
    return target, periodic, contract


def _realize(periodic: np.ndarray, support: int, normalization_sum: float) -> tuple[np.ndarray, float]:
    candidate = periodic[:support].copy()
    omitted = float(np.dot(periodic[support:], periodic[support:])) / max(
        float(np.dot(periodic, periodic)), 1.0e-300
    )
    candidate *= normalization_sum / float(math.fsum(float(value) for value in candidate))
    return candidate, omitted


def _magnitude_model(magnitude: np.ndarray, order: int) -> tuple[np.ndarray, np.ndarray]:
    power = magnitude * magnitude
    autocorrelation = np.fft.irfft(power, FFT_LENGTH)
    embedded = np.zeros(FFT_LENGTH, dtype=np.float64)
    embedded[: order + 1] = autocorrelation[: order + 1]
    embedded[-order:] = autocorrelation[-order:]
    model_power = np.fft.rfft(embedded).real
    return np.sqrt(np.maximum(model_power, 0.0)), autocorrelation


def _magnitude_order_audit(magnitude: np.ndarray) -> list[dict[str, float | int]]:
    power = magnitude * magnitude
    autocorrelation = np.fft.irfft(power, FFT_LENGTH)
    frequency = np.fft.rfftfreq(FFT_LENGTH, 1.0 / CHARACTER_RATE_HZ)
    passband = frequency <= 20_000.0
    transition = (frequency >= 20_000.0) & (frequency <= 22_050.0)
    total_correlation_energy = float(np.dot(autocorrelation, autocorrelation))
    records: list[dict[str, float | int]] = []
    for order in MODEL_ORDERS:
        model_magnitude, _ = _magnitude_model(magnitude, order)
        model_power = model_magnitude * model_magnitude
        pass_delta_db = 20.0 * np.log10(
            np.maximum(model_magnitude[passband], 1.0e-300)
            / np.maximum(magnitude[passband], 1.0e-300)
        )
        omitted_energy = float(
            np.dot(
                autocorrelation[order + 1 : -order],
                autocorrelation[order + 1 : -order],
            )
        )
        records.append(
            {
                "order": order,
                "omitted_autocorrelation_energy_ratio": omitted_energy
                / max(total_correlation_energy, 1.0e-300),
                "maximum_passband_magnitude_error_db": float(np.max(np.abs(pass_delta_db))),
                "maximum_transition_amplitude_error_linear": float(
                    np.max(np.abs(model_magnitude[transition] - magnitude[transition]))
                ),
                "maximum_absolute_power_error": float(np.max(np.abs(model_power - power))),
            }
        )
    return records


def _candidate_measurements(
    character: np.ndarray,
    cleanup: np.ndarray,
    reference_response: np.ndarray,
    fixtures: tuple,
) -> dict[str, Any]:
    response = _cascade_character_and_cleanup(character, cleanup)
    return {
        "character_sha256": _sha256_bytes(np.asarray(character, dtype="<f8").tobytes()),
        "character_sum": float(math.fsum(float(value) for value in character)),
        "timing": asdict(_timing_metrics(response)),
        "packets": {
            str(int(frequency)): asdict(_measure_packet(response, frequency))
            for frequency in PACKET_FREQUENCIES_HZ
        },
        "counterfactual": {
            fixture.name: interval_metrics(
                cleanup_counterfactual_residual(character, cleanup, fixture)
            )
            for fixture in fixtures
        },
        "frequency": _frequency_metrics(response, reference_response),
    }


def audit(root: Path) -> dict[str, Any]:
    target, periodic, contract = _p6_target(root)
    cleanup = _read_f64le(root / "assets/filters/split_phase_e2v3/cleanup_stage_1.f64le")
    tracked = _read_f64le(root / "tools/split_phase_v6/baselines/e3-p6d-local-0145.f64le")
    fixtures = default_training_fixtures()
    reference_response = _cascade_character_and_cleanup(tracked, cleanup)
    realizations: list[dict[str, Any]] = []
    tracked_measurements: dict[str, Any] | None = None
    for support in CHARACTER_SUPPORTS:
        candidate, omitted = _realize(periodic, support, contract["normalization_sum"])
        measurements = _candidate_measurements(candidate, cleanup, reference_response, fixtures)
        if support == tracked.size:
            maximum_reconstruction_error = float(np.max(np.abs(candidate - tracked)))
            if maximum_reconstruction_error > 1.0e-16:
                raise RuntimeError("P6 target reconstruction did not reproduce the frozen incumbent")
            contract["reconstruction_bit_exact"] = bool(
                measurements["character_sha256"] == contract["reported_character_sha256"]
            )
            contract["maximum_reconstruction_error"] = maximum_reconstruction_error
            tracked_measurements = measurements
        realizations.append(
            {
                "support": support,
                "omitted_periodic_energy_ratio": omitted,
                "measurements": measurements,
            }
        )
    if tracked_measurements is None:
        raise RuntimeError("current P6 support was not audited")
    long_measurements = realizations[-1]["measurements"]
    timing_delta = {
        key: (
            None
            if tracked_measurements["timing"][key] is None
            or long_measurements["timing"][key] is None
            else long_measurements["timing"][key] - tracked_measurements["timing"][key]
        )
        for key in tracked_measurements["timing"]
    }
    worst_packet_delta = max(
        long_measurements["packets"][frequency]["onset_pre_echo_energy_db_total"]
        - tracked_measurements["packets"][frequency]["onset_pre_echo_energy_db_total"]
        for frequency in tracked_measurements["packets"]
    )
    counterfactual_deltas = []
    for fixture in tracked_measurements["counterfactual"]:
        for current, longer in zip(
            tracked_measurements["counterfactual"][fixture][:2],
            long_measurements["counterfactual"][fixture][:2],
        ):
            counterfactual_deltas.append(
                longer["residual_rms_dbfs"] - current["residual_rms_dbfs"]
            )
    magnitude = np.abs(target)
    target_phase = np.exp(1j * np.angle(target))
    order_realizations = []
    for order in MODEL_ORDERS:
        model_magnitude, _ = _magnitude_model(magnitude, order)
        model_target = model_magnitude * target_phase
        model_target[0] = complex(float(model_target[0].real), 0.0)
        model_target[-1] = complex(float(model_target[-1].real), 0.0)
        model_periodic = np.fft.irfft(model_target, FFT_LENGTH)
        candidate, omitted = _realize(
            model_periodic, tracked.size, contract["normalization_sum"]
        )
        measurements = _candidate_measurements(
            candidate, cleanup, reference_response, fixtures
        )
        order_primary_deltas = []
        for fixture in tracked_measurements["counterfactual"]:
            for current, modeled in zip(
                tracked_measurements["counterfactual"][fixture][:2],
                measurements["counterfactual"][fixture][:2],
            ):
                order_primary_deltas.append(
                    modeled["residual_rms_dbfs"] - current["residual_rms_dbfs"]
                )
        order_timing_delta = {
            key: (
                None
                if tracked_measurements["timing"][key] is None
                or measurements["timing"][key] is None
                else measurements["timing"][key] - tracked_measurements["timing"][key]
            )
            for key in tracked_measurements["timing"]
        }
        order_realizations.append(
            {
                "order": order,
                "omitted_periodic_energy_ratio": omitted,
                "measurements": measurements,
                "timing_delta_vs_p6": order_timing_delta,
                "worst_primary_counterfactual_rms_delta_db_vs_p6": max(
                    order_primary_deltas
                ),
                "best_primary_counterfactual_rms_delta_db_vs_p6": min(
                    order_primary_deltas
                ),
                "meaningful_static_effect": bool(
                    order_timing_delta["maximum_post_lobe_db_peak"] <= -0.05
                    or order_timing_delta["post_energy_db_total"] <= -0.02
                    or order_timing_delta["main_lobe_width_us"] <= -0.20
                ),
                "meaningful_counterfactual_effect": bool(
                    max(order_primary_deltas) <= -0.03
                ),
            }
        )
    return {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "contract": contract,
        "magnitude_model_orders": _magnitude_order_audit(magnitude),
        "magnitude_order_realizations": order_realizations,
        "character_realizations": realizations,
        "support_comparison": {
            "long_support": CHARACTER_SUPPORTS[-1],
            "timing_delta_vs_262145": timing_delta,
            "worst_packet_onset_delta_db_vs_262145": worst_packet_delta,
            "worst_primary_counterfactual_rms_delta_db_vs_262145": max(
                counterfactual_deltas
            ),
            "best_primary_counterfactual_rms_delta_db_vs_262145": min(
                counterfactual_deltas
            ),
            "meaningful_static_effect": bool(
                timing_delta["maximum_post_lobe_db_peak"] <= -0.05
                or timing_delta["post_energy_db_total"] <= -0.02
                or timing_delta["main_lobe_width_us"] <= -0.20
            ),
            "meaningful_counterfactual_effect": bool(
                max(counterfactual_deltas) <= -0.03
            ),
        },
        "training_fixtures": [fixture_contract(fixture) for fixture in fixtures],
    }


def main() -> None:
    parser = argparse.ArgumentParser(description="Audit whether P8 magnitude or character support is binding")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument(
        "--output",
        type=Path,
        default=Path(__file__).resolve().parent / "work-e3-p8-capacity/e3_p8_capacity_audit.json",
    )
    arguments = parser.parse_args()
    report = audit(arguments.root.resolve())
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({
        "output": str(arguments.output),
        "model_orders": report["magnitude_model_orders"],
        "support_comparison": report["support_comparison"],
    }, indent=2))


if __name__ == "__main__":
    main()
