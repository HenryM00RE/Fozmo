from __future__ import annotations

import json
import math
from pathlib import Path
from typing import Any

import numpy as np

from .rational_minimax import _metrics as _rational_metrics


def _linear_from_db(value: float) -> float:
    return 10.0 ** (value / 10.0)


def compare(
    root: Path,
    d_metrics: dict[str, Any],
    d_character: np.ndarray | None = None,
    rational_report: dict[str, Any] | None = None,
) -> dict[str, Any]:
    baseline_dir = root / "tools/split_phase_v4/baselines"
    baselines = {name: json.loads((baseline_dir / ("split_" + name.lower() + ".json")).read_text())["metrics"] for name in ("A", "B", "C")}
    c = baselines["C"]
    candidates = {
        "physical_group_delay_curvature": (d_metrics["group_delay_curvature_max_abs_samples_per_ln_hz_squared"], c["group_delay_curvature_max_abs_samples_per_ln_hz_squared"]),
        "dominant_peak_pre_energy": (_linear_from_db(d_metrics["pre_energy_before_dominant_peak_db"]), _linear_from_db(c["pre_energy_before_dominant_peak_db"])),
        "bandlimited_3_14khz_pre_peak_energy": (_linear_from_db(d_metrics["bandlimited_3_14khz_pre_peak_energy_db"]), _linear_from_db(c["bandlimited_3_14khz_pre_peak_energy_db"])),
        "bandlimited_14_20khz_pre_peak_energy": (_linear_from_db(d_metrics["bandlimited_14_20khz_pre_peak_energy_db"]), _linear_from_db(c["bandlimited_14_20khz_pre_peak_energy_db"])),
        "worst_complex_passband_error": (d_metrics["worst_complex_passband_error"], c["worst_complex_passband_error"]),
        "worst_independent_alias": (_linear_from_db(d_metrics["worst_independent_decimation_alias_db"]), _linear_from_db(c["worst_independent_decimation_alias_db"])),
    }
    c_rational_consistency = None
    d_rational_consistency = None
    if rational_report is not None:
        c_asset = root / "assets/filters/split_phase_v3"
        c_rows_147_160 = np.fromfile(c_asset / "rational_147_160.f64le", dtype="<f8").reshape(160, 1025)
        c_rows_160_147 = np.fromfile(c_asset / "rational_160_147.f64le", dtype="<f8").reshape(147, 2049)
        c_rational_consistency = max(
            _rational_metrics(c_rows_147_160, 147, 44_100, 20_000.0, 22_050.0)["phase_to_phase_compensated_discontinuity"],
            _rational_metrics(c_rows_160_147, 160, 48_000, 20_000.0, 22_050.0)["phase_to_phase_compensated_discontinuity"],
        )
        d_rational_consistency = max(
            rational_report["rational_147_160"]["phase_to_phase_compensated_discontinuity"],
            rational_report["rational_160_147"]["phase_to_phase_compensated_discontinuity"],
        )
        candidates["rational_phase_to_phase_consistency"] = (
            d_rational_consistency,
            c_rational_consistency,
        )
    improvements = {}
    for name, (d_value, c_value) in candidates.items():
        fraction = 1.0 - d_value / c_value if c_value not in (None, 0.0) and d_value is not None else None
        improvements[name] = {"d": d_value, "c": c_value, "improvement_fraction": fraction, "at_least_15_percent": bool(fraction is not None and fraction >= 0.15)}
    curvature_d = d_metrics["group_delay_curvature_max_abs_samples_per_ln_hz_squared"]
    magnitude_match_db = None
    if d_character is not None:
        c_character = np.fromfile(root / "assets/filters/split_phase_v3/character_full_rate.f64le", dtype="<f8")
        fft_len = 1 << 23
        d_response = np.fft.rfft(d_character, n=fft_len)
        c_response = np.fft.rfft(c_character, n=fft_len)
        frequency = np.linspace(0.0, 44_100.0, d_response.size)
        passband = (frequency >= 20.0) & (frequency <= 20_000.0)
        magnitude_match_db = float(
            np.max(
                np.abs(
                    20.0
                    * np.log10(
                        np.maximum(np.abs(d_response[passband]), 1.0e-300)
                        / np.maximum(np.abs(c_response[passband]), 1.0e-300)
                    )
                )
            )
        )
    step_ratio = d_metrics["step_response_overshoot"] / max(c["step_response_overshoot"], 1.0e-300)
    report = {
        "same_metric_definition_version": 1,
        "improvements": improvements,
        "pareto_improvement_count": sum(value["at_least_15_percent"] for value in improvements.values()),
        "beats_b_and_c_physical_curvature": bool(curvature_d < baselines["B"]["group_delay_curvature_max_abs_samples_per_ln_hz_squared"] and curvature_d < baselines["C"]["group_delay_curvature_max_abs_samples_per_ln_hz_squared"]),
        "c_d_passband_magnitude_maximum_difference_db": magnitude_match_db,
        "c_d_passband_magnitude_gate_db": 0.0001,
        "c_d_passband_magnitude_accepted": bool(magnitude_match_db is not None and magnitude_match_db <= 0.0001),
        "step_overshoot_ratio_to_c": step_ratio,
        "step_overshoot_accepted": bool(step_ratio <= 1.005),
        "c_rational_phase_to_phase_consistency": c_rational_consistency,
        "d_rational_phase_to_phase_consistency": d_rational_consistency,
    }
    report["accepted"] = bool(report["pareto_improvement_count"] >= 3 and report["beats_b_and_c_physical_curvature"] and report["c_d_passband_magnitude_accepted"] and report["step_overshoot_accepted"])
    return report
