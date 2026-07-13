#!/usr/bin/env python3
"""Bounded, reproducible DSD64 EcBeam2 experiment campaign.

It runs a small, predeclared EcBeam2 matrix over both DSD64 wire families, preserves the A1 and
EcDepth2 controls, and refuses to choose a winner unless all hard-health and
cross-cell quality rules pass.

The script is useful before the native engine is available: ``--dry-run``
writes every candidate config, the resolved corpus manifest, and exact-oracle
request scaffolding without executing ``ecbeam2_quality``.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import os
import shlex
import statistics
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence

from dsd_candidate_config import canonical_candidate_params, write_candidate_config


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_BINARY = ROOT / "target" / "release" / (
    "ecbeam2_quality.exe" if sys.platform == "win32" else "ecbeam2_quality"
)
DEFAULT_OUT = ROOT / "audio_tests" / "out" / "dsd64-ecbeam2-experiment"
MANIFEST_DIR = ROOT / "audio_tests" / "ecbeam2" / "manifests"

CAMPAIGN_SCHEMA_VERSION = "ecbeam2-campaign-v1"
CORPUS_SCHEMA_VERSION = "ecbeam2-corpus-v1"
ORACLE_SCHEMA_VERSION = "ecbeam2-exact-oracle-v2"
SCALE_PROBE_SCHEMA_VERSION = "ecbeam2-objective-scale-probe-v1"
FROZEN_BUDGET_SCHEMA_VERSION = "ecbeam2-frozen-budgets-v1"

FILTERS = ("MinimumPhase", "SplitPhase")
CHANNELS = ("left", "right")
SOURCE_RATES = (44_100, 48_000)
WIRE_RATES = {44_100: 2_822_400, 48_000: 3_072_000}
EXPECTED_CELLS = tuple((name, source_rate) for source_rate in SOURCE_RATES for name in FILTERS)
PROFILE = "harness24to32-v1"
ECBEAM2_MODULATOR = "EcBeam2"
A1_MODULATOR = "EcBeam"
ECDEPTH2_MODULATOR = "EcDepth2"

# This is deliberately a literal, independently frozen binding rather than a
# digest derived from the current Rust source. If the runtime coefficient table
# changes, the native oracle must reject this request until the v1 experiment is
# deliberately versioned again.
ECBEAM2_V1_PLANT = {
    "plant_id": "ecbeam2-crfb-osr64-obg165-v1",
    "coefficient_table": "CRFB_OSR64_OBG165",
    "coefficient_encoding": (
        "a-row-major,b-row-major,c,d1,state-limit,input-peak,osr-u32,obg;little-endian"
    ),
    "coefficients_sha256": "e5ddedd2c3885c0c92050c4f25243e803467d169d916565961e8687cfc83d554",
    "state_limit_sha256": "247105152940185696a9745a57454825ff78c79ddb996e432c7d54933b2338e5",
    "osr": 64,
    "obg": 1.65,
    "input_peak": 0.23256,
    "headroom_db": -2.0,
    "isi_penalty": 0.0,
    "dither_scale": 0.0,
}
ORACLE_REQUIRED_RESULT_FIELDS = (
    "case_id",
    "source_case_id",
    "fixture_id",
    "filter",
    "channel",
    "source_rate",
    "wire_rate",
    "seed",
    "ultrasonic_budget",
    "signed_error_budget",
    "horizon",
    "first_bit",
    "m4n8_first_bit",
    "sequence_bits",
    "objective",
    "reconstruction_objective",
    "starting_state_potential",
    "terminal_state_potential",
    "state_terminal_delta",
    "state_terminal_cost",
    "state_barrier_raw",
    "state_barrier_cost",
    "quantizer_error_energy",
    "quantizer_regularizer_cost",
    "total_objective",
    "starting_tail_energy",
    "causal_reconstruction_energy",
    "remaining_tail_energy",
    "tail_adjusted_energy",
    "causal_ultrasonic_energy",
    "maximum_state_overflow",
    "maximum_budget_violation",
    "constraint_escapes",
    "state_repairs",
    "complete_sequences",
    "state_feasible",
    "budgets_feasible",
    "reconstructed_outputs",
    "source_window_start_sample",
    "prefix_sample_count",
    "prefix_constraint_escapes",
    "prefix_state_repairs",
    "prefix_all_nonfinite_resets",
    "prefix_invalid_input_substitutions",
    "prefix_output_length_events",
    "prefix_sha256",
    "window_sha256",
)

MATERIAL_THRESHOLDS = {
    "worst_sinad_db": 0.5,
    "spur_margin_db": 1.5,
    "hf_residual_db": 1.0,
    "multitone_residual_db": 0.5,
}
REQUIRED_PROTECTED_METRICS = (*MATERIAL_THRESHOLDS, "overload_recovery_db")
OPTIONAL_PROTECTED_METRICS = (
    "inband_noise_worst_rms_dbfs",
    "stereo_snr_worst_mismatch_db",
    "idle_worst_tone_dbfs",
    "low_level_worst_residual_db",
    "low_level_worst_spur_dbfs",
    "high_freq_worst_spur_dbfs",
    "multitone_spur_dbfs",
    "ultrasonic_24_50k_max_dbfs",
    "ultrasonic_50_100k_max_dbfs",
    "ultrasonic_100_200k_max_dbfs",
)
PROTECTED_METRICS = (*REQUIRED_PROTECTED_METRICS, *OPTIONAL_PROTECTED_METRICS)
PROTECTED_REGRESSION_DB = 0.25
HARD_COUNTERS = (
    "stability_resets",
    "state_clamps",
    "stress_stability_resets",
    "stress_state_clamps",
    "ecbeam2_constraint_escape",
    "ecbeam2_state_repair_fallback",
    "ecbeam2_all_nonfinite_resets",
    "ecbeam2_output_length_error",
    "ecbeam2_observer_desynchronizations",
    "ecbeam2_invalid_input_substitutions",
    "ecbeam2_renderer_truncation_events",
    "ecbeam2_renderer_discarded_left_bits",
    "ecbeam2_renderer_discarded_right_bits",
)
PRODUCTION_HARD_COUNTERS = HARD_COUNTERS[:4]
QUALIFICATION_HEALTH_COUNTERS = (
    "stability_resets",
    "state_clamps",
    "ecbeam2_state_repair_fallback",
    "ecbeam2_all_nonfinite_resets",
    "ecbeam2_output_length_error",
    "ecbeam2_observer_desynchronizations",
    "ecbeam2_invalid_input_substitutions",
    "ecbeam2_renderer_truncation_events",
    "ecbeam2_renderer_discarded_left_bits",
    "ecbeam2_renderer_discarded_right_bits",
)


def _clean_float(value: float) -> float:
    return float(f"{value:.12g}")


def _stable_hash(value: Mapping[str, Any], length: int = 12) -> str:
    payload = json.dumps(value, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()[:length]


def _parse_float(value: str | None) -> float | None:
    if value in (None, ""):
        return None
    try:
        parsed = float(value)
    except (TypeError, ValueError):
        return None
    return parsed if math.isfinite(parsed) else None


def _parse_int(value: str | None) -> int | None:
    parsed = _parse_float(value)
    return None if parsed is None else int(parsed)


def _notes(row: Mapping[str, str]) -> dict[str, str]:
    values: dict[str, str] = {}
    for item in (row.get("candidate_notes") or "").split(";"):
        if "=" not in item:
            continue
        key, value = item.strip().split("=", 1)
        values[key.strip()] = value.strip()
    return values


@dataclass(frozen=True)
class FrozenWireBudget:
    ultrasonic_ema_max: float
    signed_error_ema_abs_max: float
    ultrasonic_ema_p99_9: float = 0.0
    ultrasonic_ema_p99_99: float = 0.0
    signed_error_ema_abs_p99_9: float = 0.0
    signed_error_ema_abs_p99_99: float = 0.0
    ultrasonic_worst_cell: str = ""
    signed_error_worst_cell: str = ""
    ultrasonic_p99_9_worst_window: str = ""
    ultrasonic_p99_99_worst_window: str = ""
    signed_error_p99_9_worst_window: str = ""
    signed_error_p99_99_worst_window: str = ""

    def __post_init__(self) -> None:
        if not math.isfinite(self.ultrasonic_ema_max) or self.ultrasonic_ema_max < 0.0:
            raise ValueError("ultrasonic budget must be finite and non-negative")
        if not math.isfinite(self.signed_error_ema_abs_max) or self.signed_error_ema_abs_max < 0.0:
            raise ValueError("signed-error budget must be finite and non-negative")
        for name, value in (
            ("ultrasonic p99.9", self.ultrasonic_ema_p99_9),
            ("ultrasonic p99.99", self.ultrasonic_ema_p99_99),
            ("signed-error p99.9", self.signed_error_ema_abs_p99_9),
            ("signed-error p99.99", self.signed_error_ema_abs_p99_99),
        ):
            if not math.isfinite(value) or value < 0.0:
                raise ValueError(f"{name} must be finite and non-negative")


@dataclass(frozen=True)
class FrozenBudgets:
    by_wire_rate: Mapping[int, FrozenWireBudget]
    calibration_digest: str
    a1_bitstream_digests: Mapping[str, str] = field(default_factory=dict)

    def __post_init__(self) -> None:
        missing = set(WIRE_RATES.values()) - set(self.by_wire_rate)
        if missing:
            raise ValueError(f"frozen budgets missing wire rates: {sorted(missing)}")
        if not self.calibration_digest:
            raise ValueError("frozen budgets require a calibration digest")

    def document(self) -> dict[str, Any]:
        return {
            "schema_version": FROZEN_BUDGET_SCHEMA_VERSION,
            "calibration_digest": self.calibration_digest,
            "ema_window_ms": 10.0,
            "a1_bitstream_digests": dict(sorted(self.a1_bitstream_digests.items())),
            "by_wire_rate": {
                str(rate): {
                    "ultrasonic_ema_max": budget.ultrasonic_ema_max,
                    "signed_error_ema_abs_max": budget.signed_error_ema_abs_max,
                    "ultrasonic_ema_p99_9": budget.ultrasonic_ema_p99_9,
                    "ultrasonic_ema_p99_99": budget.ultrasonic_ema_p99_99,
                    "signed_error_ema_abs_p99_9": budget.signed_error_ema_abs_p99_9,
                    "signed_error_ema_abs_p99_99": budget.signed_error_ema_abs_p99_99,
                    "ultrasonic_worst_cell": budget.ultrasonic_worst_cell,
                    "signed_error_worst_cell": budget.signed_error_worst_cell,
                    "ultrasonic_p99_9_worst_window": budget.ultrasonic_p99_9_worst_window,
                    "ultrasonic_p99_99_worst_window": budget.ultrasonic_p99_99_worst_window,
                    "signed_error_p99_9_worst_window": budget.signed_error_p99_9_worst_window,
                    "signed_error_p99_99_worst_window": budget.signed_error_p99_99_worst_window,
                }
                for rate, budget in sorted(self.by_wire_rate.items())
            },
        }


def load_frozen_budgets(path: Path) -> FrozenBudgets:
    data = json.loads(path.read_text(encoding="utf-8"))
    if data.get("schema_version") != FROZEN_BUDGET_SCHEMA_VERSION:
        raise ValueError(f"{path} must use {FROZEN_BUDGET_SCHEMA_VERSION}")
    required_budget_fields = {
        "ultrasonic_ema_max",
        "signed_error_ema_abs_max",
        "ultrasonic_ema_p99_9",
        "ultrasonic_ema_p99_99",
        "signed_error_ema_abs_p99_9",
        "signed_error_ema_abs_p99_99",
        "ultrasonic_worst_cell",
        "signed_error_worst_cell",
        "ultrasonic_p99_9_worst_window",
        "ultrasonic_p99_99_worst_window",
        "signed_error_p99_9_worst_window",
        "signed_error_p99_99_worst_window",
    }
    budgets = {}
    for rate, values in data.get("by_wire_rate", {}).items():
        missing = required_budget_fields - set(values)
        if missing:
            raise ValueError(f"{path} wire rate {rate} lacks frozen fields {sorted(missing)}")
        provenance_fields = (
            "ultrasonic_worst_cell",
            "signed_error_worst_cell",
            "ultrasonic_p99_9_worst_window",
            "ultrasonic_p99_99_worst_window",
            "signed_error_p99_9_worst_window",
            "signed_error_p99_99_worst_window",
        )
        if any(not str(values[field]) for field in provenance_fields):
            raise ValueError(f"{path} wire rate {rate} lacks worst-cell provenance")
        budgets[int(rate)] = FrozenWireBudget(
            ultrasonic_ema_max=float(values["ultrasonic_ema_max"]),
            signed_error_ema_abs_max=float(values["signed_error_ema_abs_max"]),
            ultrasonic_ema_p99_9=float(values["ultrasonic_ema_p99_9"]),
            ultrasonic_ema_p99_99=float(values["ultrasonic_ema_p99_99"]),
            signed_error_ema_abs_p99_9=float(values["signed_error_ema_abs_p99_9"]),
            signed_error_ema_abs_p99_99=float(values["signed_error_ema_abs_p99_99"]),
            ultrasonic_worst_cell=str(values["ultrasonic_worst_cell"]),
            signed_error_worst_cell=str(values["signed_error_worst_cell"]),
            ultrasonic_p99_9_worst_window=str(values["ultrasonic_p99_9_worst_window"]),
            ultrasonic_p99_99_worst_window=str(values["ultrasonic_p99_99_worst_window"]),
            signed_error_p99_9_worst_window=str(values["signed_error_p99_9_worst_window"]),
            signed_error_p99_99_worst_window=str(values["signed_error_p99_99_worst_window"]),
        )
    return FrozenBudgets(
        budgets,
        str(data.get("calibration_digest", "")),
        {
            str(key): str(value)
            for key, value in data.get("a1_bitstream_digests", {}).items()
        },
    )


@dataclass(frozen=True)
class Candidate:
    label: str
    modulator: str
    params: Mapping[str, Any]
    role: str = "selection"
    baseline: bool = False
    wire_budgets: Mapping[int, FrozenWireBudget] | None = None
    wire_params: Mapping[int, Mapping[str, Any]] | None = None
    contribution_ratios: Mapping[str, float] = field(default_factory=dict)
    scale_probe_digest: str | None = None
    budget_mode: str = "both"
    ultrasonic_allowance_db: float | None = None
    signed_error_multiplier: float | None = None

    def __post_init__(self) -> None:
        if self.budget_mode not in ("disabled", "ultrasonic", "signed-error", "both"):
            raise ValueError(f"invalid EcBeam2 budget mode {self.budget_mode}")

    def canonical_params(self) -> dict[str, Any]:
        return canonical_candidate_params(self.params)

    def stable_id(self) -> str:
        return _stable_hash(
            {
                "campaign_schema_version": CAMPAIGN_SCHEMA_VERSION,
                "modulator": self.modulator,
                "params": self.canonical_params(),
                "role": self.role,
                "wire_budgets": (
                    {
                        str(rate): {
                            "ultrasonic_ema_max": budget.ultrasonic_ema_max,
                            "signed_error_ema_abs_max": budget.signed_error_ema_abs_max,
                        }
                        for rate, budget in sorted(self.wire_budgets.items())
                    }
                    if self.wire_budgets is not None
                    else None
                ),
                "wire_params": (
                    {
                        str(rate): canonical_candidate_params(params)
                        for rate, params in sorted(self.wire_params.items())
                    }
                    if self.wire_params is not None
                    else None
                ),
                "contribution_ratios": dict(sorted(self.contribution_ratios.items())),
                "scale_probe_digest": self.scale_probe_digest,
                "budget_mode": self.budget_mode,
                "ultrasonic_allowance_db": self.ultrasonic_allowance_db,
                "signed_error_multiplier": self.signed_error_multiplier,
            }
        )


def _base_params() -> dict[str, Any]:
    return {
        "headroom_db": -2.0,
        "expected_gain_db": -2.0,
        # Keep the production EcBeam A1 coefficient frontier identical for the
        # control, ShadowA1 calibration, and every active EcBeam2 row.
        "ec_obg": 1.65,
        "dither_scale": 0.0,
    }


def _ecdepth2_reference_params() -> dict[str, Any]:
    # This row is the current production EcDepth2 control, not an EcBeam2
    # direct-comparison configuration. Leave coefficient and dither selection
    # to EcDepth2's production defaults and use its production -4 dB contract.
    return {
        "headroom_db": -4.0,
        "expected_gain_db": -4.0,
    }


def calibration_candidate() -> Candidate:
    return Candidate(
        label="ecbeam-a1-shadow-calibration",
        modulator=A1_MODULATOR,
        role="calibration",
        baseline=True,
        params={
            **_base_params(),
            "ecbeam2_run_mode": "shadow-a1",
            "ecbeam2_profile": PROFILE,
        },
    )


def calibration_candidates() -> list[Candidate]:
    return [
        Candidate(
            label="ecbeam-a1-observer-off",
            modulator=A1_MODULATOR,
            role="calibration",
            baseline=True,
            params=_base_params(),
        ),
        calibration_candidate(),
    ]


def a1_reference_candidate(*, role: str = "selection") -> Candidate:
    """Production A1 plus the read-only observer used for same-run validation."""
    return Candidate(
        label="ecbeam-a1-production",
        modulator=A1_MODULATOR,
        role=role,
        baseline=True,
        params={
            **_base_params(),
            "ecbeam2_run_mode": "shadow-a1",
            "ecbeam2_profile": PROFILE,
        },
    )


def _active_params(
    *,
    state_terminal_weight: float = 0.0,
    state_deadzone: float = 0.0,
    state_deadzone_weight: float = 0.0,
    quantizer_regularizer: float = 0.0,
) -> dict[str, Any]:
    params: dict[str, Any] = {
        **_base_params(),
        "ecbeam2_run_mode": "active",
        "ecbeam2_profile": PROFILE,
        "ecbeam2_state_terminal_weight": _clean_float(state_terminal_weight),
        "ecbeam2_state_deadzone": _clean_float(state_deadzone),
        "ecbeam2_state_deadzone_weight": _clean_float(state_deadzone_weight),
        "ecbeam2_quantizer_regularizer": _clean_float(quantizer_regularizer),
    }
    return params


def legacy_v1_selection_candidates(budgets: FrozenBudgets) -> list[Candidate]:
    """Archived pre-stability 28-row matrix. Never used by campaign execution.

    This retains reproducibility for old artifacts only. Its raw barrier and
    tiny quantizer weights predate measured objective scaling and must not be
    used to select a current EcBeam2 candidate.
    """
    candidates = [
        a1_reference_candidate(),
        Candidate(
            "ecdepth2-reference",
            ECDEPTH2_MODULATOR,
            _ecdepth2_reference_params(),
            baseline=True,
        ),
        Candidate(
            "legacy-four-pole-proxy-control",
            A1_MODULATOR,
            {
                **_base_params(),
                "ec_beam_m": 4,
                "ec_beam_n": 8,
                "beam_reconstruction_error_weight": 1.0,
            },
        ),
        Candidate("tail-aware-unconstrained", ECBEAM2_MODULATOR, _active_params()),
    ]

    for weight in (0.01, 0.025, 0.05):
        candidates.append(
            Candidate(
                f"state-deadzone-{weight:g}",
                ECBEAM2_MODULATOR,
                _active_params(state_deadzone=0.45, state_deadzone_weight=weight),
            )
        )

    budgeted_barriers = (
        (0.0, 0.0),
        (0.35, 0.01),
        (0.35, 0.025),
        (0.35, 0.05),
        (0.45, 0.01),
        (0.45, 0.025),
        (0.45, 0.05),
    )
    frozen_wire_budgets = dict(budgets.by_wire_rate)
    for deadzone, barrier_weight in budgeted_barriers:
        for regularizer in (0.0, 1.0e-8, 1.0e-7):
            candidates.append(
                Candidate(
                    (
                        f"budget-frozen-dz{deadzone:g}-dzw{barrier_weight:g}-"
                        f"q{regularizer:g}"
                    ),
                    ECBEAM2_MODULATOR,
                    _active_params(
                        state_deadzone=deadzone,
                        state_deadzone_weight=barrier_weight,
                        quantizer_regularizer=regularizer,
                    ),
                    wire_budgets=frozen_wire_budgets,
                )
            )

    if not 20 <= len(candidates) <= 40:
        raise AssertionError(f"EcBeam2 campaign unexpectedly has {len(candidates)} rows")
    ids = [candidate.stable_id() for candidate in candidates]
    if len(ids) != len(set(ids)):
        raise AssertionError("EcBeam2 campaign contains duplicate candidate identities")
    return candidates


def qualified_selection_candidates(candidate: Candidate) -> list[Candidate]:
    """External-quality rows after stability, budgets, and oracle qualification."""
    if (
        candidate.modulator != ECBEAM2_MODULATOR
        or candidate.wire_budgets is None
        or candidate.budget_mode != "both"
    ):
        raise ValueError("selection candidate must have qualified two-budget EcBeam2 config")
    return [
        a1_reference_candidate(),
        Candidate(
            "ecdepth2-reference",
            ECDEPTH2_MODULATOR,
            _ecdepth2_reference_params(),
            baseline=True,
        ),
        Candidate(
            "legacy-four-pole-proxy-control",
            A1_MODULATOR,
            {
                **_base_params(),
                "ec_beam_m": 4,
                "ec_beam_n": 8,
                "beam_reconstruction_error_weight": 1.0,
            },
        ),
        Candidate(
            candidate.label,
            candidate.modulator,
            candidate.params,
            role="selection",
            wire_budgets=candidate.wire_budgets,
            wire_params=candidate.wire_params,
            contribution_ratios=candidate.contribution_ratios,
            scale_probe_digest=candidate.scale_probe_digest,
            budget_mode=candidate.budget_mode,
            ultrasonic_allowance_db=candidate.ultrasonic_allowance_db,
            signed_error_multiplier=candidate.signed_error_multiplier,
        ),
    ]


SCALE_TERMS = (
    "reconstruction_increment_abs",
    "state_terminal_delta_abs",
    "state_barrier_raw",
    "quantizer_error_squared",
)
STABILITY_BARRIER_KNEES = (0.70, 0.80, 0.88)


def scale_probe_candidates() -> list[Candidate]:
    """Inert probes: changing the knee cannot affect bits at zero weight."""
    return [
        Candidate(
            f"objective-scale-probe-rho{rho:g}",
            ECBEAM2_MODULATOR,
            _active_params(state_deadzone=rho),
            role="stability",
            contribution_ratios={"barrier_probe_knee": rho},
        )
        for rho in (0.0, *STABILITY_BARRIER_KNEES)
    ]


def freeze_objective_scale_probe(results: Sequence[RunResult]) -> dict[str, Any]:
    by_wire: dict[str, Any] = {}
    reference_digests: dict[tuple[int, str], str] = {}
    reference_health: dict[tuple[int, str], dict[str, int]] = {}
    bitstream_digests: dict[str, str] = {}
    health_by_cell: dict[str, dict[str, int]] = {}
    for source_rate, wire_rate in WIRE_RATES.items():
        wire_rows: dict[str, Any] = {}
        for result in results:
            if not result.spec.candidate.label.startswith("objective-scale-probe-rho"):
                continue
            rho = float(result.spec.candidate.params["ecbeam2_state_deadzone"])
            distributions: dict[str, dict[str, float]] = {}
            matching = [
                row
                for (filter_name, row_source_rate), row in result.rows.items()
                if row_source_rate == source_rate and filter_name in FILTERS
            ]
            if len(matching) != len(FILTERS):
                raise ValueError(f"scale probe rho={rho:g} lacks wire {wire_rate} cells")
            for row in matching:
                notes = _notes(row)
                filter_name = next(
                    filter_name
                    for (filter_name, row_source_rate), candidate_row in result.rows.items()
                    if row_source_rate == source_rate and candidate_row is row
                )
                digest = notes.get("ecbeam2_qualification_bitstream_digest")
                if not digest or len(digest) != 64:
                    raise ValueError(
                        f"scale probe lacks bitstream digest for {wire_rate}/{filter_name}"
                    )
                key = (wire_rate, filter_name)
                expected = reference_digests.setdefault(key, digest)
                if digest != expected:
                    raise ValueError(
                        f"zero-weight barrier knee changed bits for {wire_rate}/{filter_name}"
                    )
                bitstream_digests[f"{wire_rate}|{filter_name}"] = digest
                health = {}
                for counter in QUALIFICATION_HEALTH_COUNTERS:
                    value = _parse_int(row.get(counter))
                    if value is None:
                        value = _note_int(notes, counter)
                    if value is None:
                        raise ValueError(
                            f"scale probe lacks health counter {wire_rate}/{filter_name}: {counter}"
                        )
                    health[counter] = value
                expected_health = reference_health.setdefault(key, health)
                if health != expected_health:
                    raise ValueError(
                        f"zero-weight barrier knee changed health for {wire_rate}/{filter_name}"
                    )
                health_by_cell[f"{wire_rate}|{filter_name}"] = health
            for term in SCALE_TERMS:
                distributions[term] = {}
                for quantile in ("median", "p95", "p99", "max"):
                    note = f"ecbeam2_scale_{term}_{quantile}"
                    values = [_parse_float(_notes(row).get(note)) for row in matching]
                    if any(value is None or value < 0.0 for value in values):
                        raise ValueError(f"scale probe lacks finite {note} for wire {wire_rate}")
                    distributions[term][quantile] = max(float(value) for value in values)
            wire_rows[f"{rho:.2f}"] = distributions
        if set(wire_rows) != {f"{rho:.2f}" for rho in (0.0, *STABILITY_BARRIER_KNEES)}:
            raise ValueError(f"scale probe does not cover every knee for wire {wire_rate}")
        by_wire[str(wire_rate)] = wire_rows
    body = {
        "schema_version": SCALE_PROBE_SCHEMA_VERSION,
        "aggregation": "worst-channel-distribution-by-p95-then-max-across-primary-filters",
        "by_wire_rate": by_wire,
        "bitstream_digests": dict(sorted(bitstream_digests.items())),
        "health_by_cell": dict(sorted(health_by_cell.items())),
    }
    body["scale_probe_digest"] = _stable_hash(body, length=64)
    return body


def load_objective_scale_probe(path: Path) -> dict[str, Any]:
    document = json.loads(path.read_text(encoding="utf-8"))
    digest = document.get("scale_probe_digest")
    if document.get("schema_version") != SCALE_PROBE_SCHEMA_VERSION:
        raise ValueError(f"{path} must use {SCALE_PROBE_SCHEMA_VERSION}")
    if document.get("aggregation") != (
        "worst-channel-distribution-by-p95-then-max-across-primary-filters"
    ):
        raise ValueError(f"{path} has an unknown scale-distribution aggregation")
    digest_body = {
        "schema_version": document.get("schema_version"),
        "aggregation": document.get("aggregation"),
        "by_wire_rate": document.get("by_wire_rate"),
        "bitstream_digests": document.get("bitstream_digests"),
        "health_by_cell": document.get("health_by_cell"),
    }
    if digest != _stable_hash(digest_body, length=64):
        raise ValueError(f"{path} scale-probe digest mismatch")
    for wire_rate in WIRE_RATES.values():
        rows = document.get("by_wire_rate", {}).get(str(wire_rate), {})
        for rho in (0.0, *STABILITY_BARRIER_KNEES):
            terms = rows.get(f"{rho:.2f}", {})
            for term in SCALE_TERMS:
                values = terms.get(term, {})
                if any(
                    not math.isfinite(float(values.get(quantile, float("nan"))))
                    or float(values.get(quantile, -1.0)) < 0.0
                    for quantile in ("median", "p95", "p99", "max")
                ):
                    raise ValueError(
                        f"{path} lacks {wire_rate}/{rho:.2f}/{term} distribution"
                    )
    return document


def _scaled_weight(
    probe: Mapping[str, Any], wire_rate: int, rho: float, term: str, ratio: float
) -> float:
    rows = probe["by_wire_rate"][str(wire_rate)]
    reconstruction = float(rows["0.00"]["reconstruction_increment_abs"]["p95"])
    denominator = float(rows[f"{rho:.2f}"][term]["p95"])
    return _clean_float(ratio * reconstruction / max(denominator, 1.0e-18))


def stability_candidates(probe: Mapping[str, Any]) -> list[Candidate]:
    digest = str(probe["scale_probe_digest"])
    candidates = [
        a1_reference_candidate(role="stability"),
        Candidate(
            "tail-aware-unconstrained",
            ECBEAM2_MODULATOR,
            _active_params(),
            role="stability",
            scale_probe_digest=digest,
        ),
    ]

    def add(
        label: str,
        *,
        terminal: float,
        rho: float = 0.0,
        barrier: float = 0.0,
        quantizer: float = 0.0,
    ) -> None:
        wire_params = {}
        for wire_rate in WIRE_RATES.values():
            effective = {
                "ecbeam2_state_terminal_weight": _scaled_weight(
                    probe, wire_rate, 0.0, "state_terminal_delta_abs", terminal
                ),
                "ecbeam2_state_deadzone": rho,
                "ecbeam2_state_deadzone_weight": (
                    _scaled_weight(probe, wire_rate, rho, "state_barrier_raw", barrier)
                    if barrier > 0.0
                    else 0.0
                ),
                "ecbeam2_quantizer_regularizer": (
                    _scaled_weight(
                        probe, wire_rate, 0.0, "quantizer_error_squared", quantizer
                    )
                    if quantizer > 0.0
                    else 0.0
                ),
            }
            wire_params[wire_rate] = effective
        candidates.append(
            Candidate(
                label,
                ECBEAM2_MODULATOR,
                _active_params(state_deadzone=rho),
                role="stability",
                wire_params=wire_params,
                contribution_ratios={
                    "state_terminal": terminal,
                    "state_barrier": barrier,
                    "quantizer": quantizer,
                    "barrier_knee": rho,
                },
                scale_probe_digest=digest,
            )
        )

    for terminal in (0.03, 0.10, 0.30):
        add(f"terminal-a{terminal:g}", terminal=terminal)
    for terminal in (0.03, 0.10, 0.30):
        for rho in STABILITY_BARRIER_KNEES:
            for barrier in (0.03, 0.10):
                add(
                    f"terminal-barrier-a{terminal:g}-rho{rho:g}-b{barrier:g}",
                    terminal=terminal,
                    rho=rho,
                    barrier=barrier,
                )
    for terminal in (0.03, 0.10, 0.30):
        for quantizer in (0.01, 0.03):
            add(
                f"terminal-quantizer-a{terminal:g}-q{quantizer:g}",
                terminal=terminal,
                quantizer=quantizer,
            )
    # A compact both-controls cross-check uses the central terminal ratio while
    # still exercising every requested knee and both secondary ratios.
    for rho in STABILITY_BARRIER_KNEES:
        for barrier, quantizer in ((0.03, 0.01), (0.10, 0.03)):
            add(
                f"terminal-both-a0.1-rho{rho:g}-b{barrier:g}-q{quantizer:g}",
                terminal=0.10,
                rho=rho,
                barrier=barrier,
                quantizer=quantizer,
            )
    ids = [candidate.stable_id() for candidate in candidates]
    if len(candidates) != 35 or len(ids) != len(set(ids)):
        raise AssertionError(
            "stability grid is not the frozen deterministic 35-row matrix: "
            f"rows={len(candidates)} unique={len(set(ids))}"
        )
    return candidates


def ultrasonic_power_allowance(base_budget: float, allowance_db: float) -> float:
    if base_budget <= 0.0 or not math.isfinite(base_budget):
        raise ValueError("base ultrasonic power budget must be finite and positive")
    if not math.isfinite(allowance_db):
        raise ValueError("ultrasonic allowance must be finite")
    return _clean_float(base_budget * 10.0 ** (allowance_db / 10.0))


def budget_qualification_candidates(
    retained: Sequence[Candidate], budgets: FrozenBudgets
) -> list[Candidate]:
    if not 1 <= len(retained) <= 2:
        raise ValueError("budget qualification requires one or two retained candidates")
    candidates = [a1_reference_candidate(role="budget")]
    for retained_candidate in retained:
        if retained_candidate.modulator != ECBEAM2_MODULATOR:
            raise ValueError("budget qualification accepts only EcBeam2 candidates")

        def clone(
            label: str,
            wire_budgets: Mapping[int, FrozenWireBudget],
            mode: str,
            ultrasonic_db: float | None = None,
            signed_multiplier: float | None = None,
        ) -> None:
            candidates.append(
                Candidate(
                    label,
                    retained_candidate.modulator,
                    retained_candidate.params,
                    role="budget",
                    wire_budgets=wire_budgets,
                    wire_params=retained_candidate.wire_params,
                    contribution_ratios=retained_candidate.contribution_ratios,
                    scale_probe_digest=retained_candidate.scale_probe_digest,
                    budget_mode=mode,
                    ultrasonic_allowance_db=ultrasonic_db,
                    signed_error_multiplier=signed_multiplier,
                )
            )

        short = retained_candidate.stable_id()
        clone(f"{short}-ultrasonic-only", budgets.by_wire_rate, "ultrasonic")
        clone(f"{short}-signed-error-only", budgets.by_wire_rate, "signed-error")
        clone(f"{short}-both-base", budgets.by_wire_rate, "both", 0.0, 1.0)
        for ultrasonic_db in (0.0, 0.25, 0.50):
            for signed_multiplier in (1.0, 1.25, 1.50):
                if ultrasonic_db == 0.0 and signed_multiplier == 1.0:
                    continue  # represented by the required both-base diagnostic
                effective = {
                    wire_rate: FrozenWireBudget(
                        ultrasonic_ema_max=ultrasonic_power_allowance(
                            base.ultrasonic_ema_max, ultrasonic_db
                        ),
                        signed_error_ema_abs_max=_clean_float(
                            base.signed_error_ema_abs_max * signed_multiplier
                        ),
                    )
                    for wire_rate, base in budgets.by_wire_rate.items()
                }
                clone(
                    f"{short}-grid-u{ultrasonic_db:g}db-s{signed_multiplier:g}x",
                    effective,
                    "both",
                    ultrasonic_db,
                    signed_multiplier,
                )
    return candidates


def choose_strictest_budget_allowance(
    evaluations: Sequence[Mapping[str, Any]],
) -> Mapping[str, Any] | None:
    eligible = [
        row
        for row in evaluations
        if int(row.get("constraint_escapes", 0)) == 0
        and not row.get("health_failures")
        and bool(row.get("all_required_corpora", False))
    ]
    if not eligible:
        return None
    return min(
        eligible,
        key=lambda row: (
            float(row["ultrasonic_allowance_db"]),
            float(row["signed_error_multiplier"]),
            str(row.get("candidate_id", "")),
        ),
    )


def freeze_budget_qualification(
    evidence_paths: Sequence[Path], base_budgets: FrozenBudgets
) -> tuple[Candidate, dict[str, Any]]:
    grouped: dict[str, list[Mapping[str, Any]]] = {}
    for path in evidence_paths:
        document = json.loads(path.read_text(encoding="utf-8"))
        if document.get("schema_version") != "ecbeam2-budget-qualification-v1":
            raise ValueError(f"{path} is not EcBeam2 budget qualification evidence")
        if document.get("calibration_digest") != base_budgets.calibration_digest:
            raise ValueError(f"{path} uses a different calibration digest")
        for row in document.get("evaluations", []):
            if (
                row.get("budget_mode") == "both"
                and row.get("ultrasonic_allowance_db") is not None
                and row.get("signed_error_multiplier") is not None
            ):
                grouped.setdefault(str(row["candidate_id"]), []).append(row)
    combined = []
    for candidate_id, rows in grouped.items():
        roles = {str(row["corpus_role"]) for row in rows}
        representative = rows[0]
        combined.append(
            {
                "candidate_id": candidate_id,
                "candidate": representative["candidate"],
                "ultrasonic_allowance_db": representative["ultrasonic_allowance_db"],
                "signed_error_multiplier": representative["signed_error_multiplier"],
                "constraint_escapes": sum(int(row["constraint_escapes"]) for row in rows),
                "health_failures": [
                    failure for row in rows for failure in row.get("health_failures", [])
                ],
                "all_required_corpora": {"calibration", "held_out"}.issubset(roles),
                "corpus_roles": sorted(roles),
            }
        )
    selected = choose_strictest_budget_allowance(combined)
    if selected is None:
        raise ValueError("no allowance has zero escapes across calibration and held-out corpora")
    candidate = candidate_from_document(selected["candidate"])
    if candidate.wire_budgets is None or candidate.scale_probe_digest is None:
        raise ValueError("selected allowance lacks effective budgets or scale provenance")
    effective = FrozenBudgets(
        candidate.wire_budgets,
        base_budgets.calibration_digest,
        base_budgets.a1_bitstream_digests,
    ).document()
    effective["qualification"] = {
        "base_budgets": {
            str(rate): {
                "ultrasonic_ema_max": budget.ultrasonic_ema_max,
                "signed_error_ema_abs_max": budget.signed_error_ema_abs_max,
            }
            for rate, budget in sorted(base_budgets.by_wire_rate.items())
        },
        "ultrasonic_allowance_db": candidate.ultrasonic_allowance_db,
        "signed_error_multiplier": candidate.signed_error_multiplier,
        "candidate": candidate_document(candidate),
        "candidate_objective_config": {
            str(rate): canonical_candidate_params(params)
            for rate, params in sorted((candidate.wire_params or {}).items())
        },
        "scale_probe_digest": candidate.scale_probe_digest,
        "wire_rates": sorted(candidate.wire_budgets),
        "evidence": [str(path) for path in evidence_paths],
    }
    return candidate, effective


def candidate_document(candidate: Candidate) -> dict[str, Any]:
    return {
        "label": candidate.label,
        "modulator": candidate.modulator,
        "role": candidate.role,
        "baseline": candidate.baseline,
        "params": candidate.canonical_params(),
        "wire_budgets": (
            {
                str(rate): {
                    "ultrasonic_ema_max": budget.ultrasonic_ema_max,
                    "signed_error_ema_abs_max": budget.signed_error_ema_abs_max,
                }
                for rate, budget in sorted(candidate.wire_budgets.items())
            }
            if candidate.wire_budgets is not None
            else None
        ),
        "wire_params": (
            {
                str(rate): canonical_candidate_params(params)
                for rate, params in sorted(candidate.wire_params.items())
            }
            if candidate.wire_params is not None
            else None
        ),
        "contribution_ratios": dict(sorted(candidate.contribution_ratios.items())),
        "scale_probe_digest": candidate.scale_probe_digest,
        "budget_mode": candidate.budget_mode,
        "ultrasonic_allowance_db": candidate.ultrasonic_allowance_db,
        "signed_error_multiplier": candidate.signed_error_multiplier,
    }


def candidate_from_document(data: Mapping[str, Any]) -> Candidate:
    wire_data = data.get("wire_budgets")
    wire_budgets = None
    if isinstance(wire_data, Mapping):
        wire_budgets = {
            int(rate): FrozenWireBudget(
                float(values["ultrasonic_ema_max"]),
                float(values["signed_error_ema_abs_max"]),
            )
            for rate, values in wire_data.items()
        }
    wire_params_data = data.get("wire_params")
    wire_params = None
    if isinstance(wire_params_data, Mapping):
        wire_params = {
            int(rate): dict(values) for rate, values in wire_params_data.items()
        }
    candidate = Candidate(
        label=str(data["label"]),
        modulator=str(data["modulator"]),
        params=dict(data["params"]),
        role=str(data.get("role", "selection")),
        baseline=bool(data.get("baseline", False)),
        wire_budgets=wire_budgets,
        wire_params=wire_params,
        contribution_ratios={
            str(name): float(value)
            for name, value in dict(data.get("contribution_ratios", {})).items()
        },
        scale_probe_digest=(
            str(data["scale_probe_digest"])
            if data.get("scale_probe_digest") is not None
            else None
        ),
        budget_mode=str(data.get("budget_mode", "both")),
        ultrasonic_allowance_db=(
            float(data["ultrasonic_allowance_db"])
            if data.get("ultrasonic_allowance_db") is not None
            else None
        ),
        signed_error_multiplier=(
            float(data["signed_error_multiplier"])
            if data.get("signed_error_multiplier") is not None
            else None
        ),
    )
    if candidate.modulator != ECBEAM2_MODULATOR:
        raise ValueError("held-out candidate must use EcBeam2")
    return candidate


def _validate_expected_baseline_digests(
    path: Path, value: Any, *, required: bool
) -> dict[str, str]:
    if not isinstance(value, Mapping):
        raise ValueError(f"{path} expected_baseline_digests must be an object")
    digests = {str(key): str(digest) for key, digest in value.items()}
    if required and not digests:
        raise ValueError(f"{path} must freeze non-empty expected baseline digests")
    if not digests:
        return digests
    for key, digest in digests.items():
        parts = key.split("|")
        if len(parts) != 7:
            raise ValueError(f"{path} baseline digest key {key!r} is not a corpus bitstream key")
        case_id, fixture_id, filter_name, modulator, source, wire, channel = parts
        if (
            not case_id
            or not fixture_id
            or filter_name not in FILTERS
            or modulator != A1_MODULATOR
            or channel not in {"left", "right"}
        ):
            raise ValueError(f"{path} baseline digest key {key!r} has invalid axes")
        try:
            source_rate = int(source)
            wire_rate = int(wire)
        except ValueError as error:
            raise ValueError(f"{path} baseline digest key {key!r} has invalid rates") from error
        if source_rate not in SOURCE_RATES or WIRE_RATES[source_rate] != wire_rate:
            raise ValueError(f"{path} baseline digest key {key!r} has mismatched rates")
        if len(digest) != 64 or any(character not in "0123456789abcdef" for character in digest):
            raise ValueError(f"{path} baseline digest {key} is not lowercase SHA-256")
    return digests


def _parse_ecbeam2_generator_spec(spec: str) -> int:
    """Validate the native v1 generator grammar and return its u64 seed."""

    parts = spec.split("|")
    if len(parts) == 3 and parts[0] in {
        "program_multitone",
        "pink_noise",
        "fades_overload",
        "spur_windows",
    } and parts[2] == "v1":
        seed_part = parts[1]
    elif (
        len(parts) == 4
        and parts[0] == "low_level_tones"
        and parts[1] == "-120,-100,-80"
        and parts[3] == "v1"
    ):
        seed_part = parts[2]
    elif (
        len(parts) == 4
        and parts[0] == "tiny_dc"
        and parts[1] == "levels=1e-6,1e-5"
        and parts[3] == "v1"
    ):
        seed_part = parts[2]
    elif (
        len(parts) == 4
        and parts[0] == "high_frequency"
        and parts[1] == "18000,19000"
        and parts[3] == "v1"
    ):
        seed_part = parts[2]
    else:
        raise ValueError(f"unsupported EcBeam2 generator spec {spec!r}")

    if not seed_part.startswith("seed="):
        raise ValueError(f"EcBeam2 generator lacks seed: {spec!r}")
    token = seed_part.removeprefix("seed=")
    if token.startswith("0x"):
        digits = token[2:]
        if not digits or any(character not in "0123456789abcdefABCDEF" for character in digits):
            raise ValueError(f"invalid EcBeam2 generator seed {token!r}")
        seed = int(digits, 16)
    else:
        if not token or any(character not in "0123456789" for character in token):
            raise ValueError(f"invalid EcBeam2 generator seed {token!r}")
        seed = int(token, 10)
    if seed > (1 << 64) - 1:
        raise ValueError(f"invalid EcBeam2 generator seed {token!r}")
    return seed


def load_corpus_manifest(
    path: Path,
    expected_role: str | None = None,
    *,
    require_expected_baseline_digests: bool = False,
) -> dict[str, Any]:
    data = json.loads(path.read_text(encoding="utf-8"))
    if data.get("schema_version") != CORPUS_SCHEMA_VERSION:
        raise ValueError(f"{path} must use {CORPUS_SCHEMA_VERSION}")
    if not isinstance(data.get("corpus_id"), str) or not data["corpus_id"]:
        raise ValueError(f"{path} must freeze a non-empty corpus_id")
    if expected_role is not None and data.get("role") != expected_role:
        raise ValueError(f"{path} role must be {expected_role}")
    if data.get("measurement_version") != "dsd-ultrasonic-bands-v3-20260704":
        raise ValueError(f"{path} has an unexpected measurement version")
    if data.get("scoring_version") != "dsd-sectioned-score-v9":
        raise ValueError(f"{path} has an unexpected scoring version")
    if data.get("fixture_set_version") != "dsd-fixtures-v3":
        raise ValueError(f"{path} has an unexpected fixture-set version")
    if tuple(data.get("source_rates", [])) != SOURCE_RATES:
        raise ValueError(f"{path} must freeze source rates {SOURCE_RATES}")
    if tuple(data.get("wire_rates", [])) != tuple(WIRE_RATES[rate] for rate in SOURCE_RATES):
        raise ValueError(f"{path} must freeze DSD64 wire rates")
    if tuple(data.get("filters", [])) != FILTERS:
        raise ValueError(f"{path} must freeze filters {FILTERS}")
    seeds = data.get("seeds")
    if (
        not isinstance(seeds, list)
        or not seeds
        or any(not isinstance(seed, int) or isinstance(seed, bool) or seed < 0 for seed in seeds)
        or len(seeds) != len(set(seeds))
    ):
        raise ValueError(f"{path} must freeze unique non-negative integer seeds")
    data["expected_baseline_digests"] = _validate_expected_baseline_digests(
        path,
        data.get("expected_baseline_digests"),
        required=require_expected_baseline_digests,
    )
    fixtures = data.get("fixtures")
    if not isinstance(fixtures, list) or not fixtures:
        raise ValueError(f"{path} must contain fixtures")
    fixture_ids = [fixture.get("id") for fixture in fixtures]
    if any(not isinstance(value, str) or not value or "|" in value for value in fixture_ids):
        raise ValueError(f"{path} fixture ids must be non-empty strings")
    if len(fixture_ids) != len(set(fixture_ids)):
        raise ValueError(f"{path} fixture ids must be unique")
    generated_seeds: set[int] = set()
    generated_seed_fixture: dict[int, str] = {}
    for fixture in fixtures:
        digest = fixture.get("sha256") or fixture.get("generator_spec_sha256")
        if (
            not isinstance(digest, str)
            or len(digest) != 64
            or any(character not in "0123456789abcdef" for character in digest)
        ):
            raise ValueError(f"{path} fixture {fixture.get('id')} needs a SHA-256 digest")
        if fixture.get("kind") == "generated":
            generator = fixture.get("generator")
            if not isinstance(generator, str) or not generator:
                raise ValueError(f"{path} generated fixture {fixture.get('id')} lacks a spec")
            actual = hashlib.sha256(generator.encode("utf-8")).hexdigest()
            if actual != fixture.get("generator_spec_sha256"):
                raise ValueError(f"{path} fixture {fixture.get('id')} generator hash changed")
            try:
                generator_seed = _parse_ecbeam2_generator_spec(generator)
            except ValueError as error:
                raise ValueError(f"{path} fixture {fixture.get('id')}: {error}") from error
            previous_fixture = generated_seed_fixture.setdefault(
                generator_seed, str(fixture.get("id"))
            )
            if previous_fixture != fixture.get("id"):
                raise ValueError(
                    f"{path} generated fixtures {previous_fixture} and {fixture.get('id')} "
                    f"reuse seed {generator_seed}"
                )
            generated_seeds.add(generator_seed)
        else:
            raise ValueError(
                f"{path} fixture {fixture.get('id')} must be generated; external audio is unsupported"
            )
    if set(seeds) != generated_seeds:
        raise ValueError(
            f"{path} seeds {seeds} do not exactly match generated fixture seeds "
            f"{sorted(generated_seeds)}"
        )
    difficult_windows = data.get("difficult_windows")
    if not isinstance(difficult_windows, list) or not difficult_windows:
        raise ValueError(f"{path} must freeze difficult windows")
    case_ids: set[str] = set()
    for window in difficult_windows:
        case_id = window.get("case_id")
        if (
            not isinstance(case_id, str)
            or not case_id
            or "|" in case_id
            or case_id in case_ids
        ):
            raise ValueError(f"{path} difficult-window case ids must be unique strings")
        case_ids.add(case_id)
        if window.get("fixture_id") not in fixture_ids:
            raise ValueError(f"{path} window {case_id} references an unknown fixture")
        if not isinstance(window.get("category"), str) or not window["category"].strip():
            raise ValueError(f"{path} window {case_id} has an invalid category")
        if window.get("source_rate") not in SOURCE_RATES:
            raise ValueError(f"{path} window {case_id} has an unsupported source rate")
        if (
            not isinstance(window.get("start_sample"), int)
            or isinstance(window["start_sample"], bool)
            or window["start_sample"] < 0
        ):
            raise ValueError(f"{path} window {case_id} has an invalid start")
        if (
            not isinstance(window.get("length_samples"), int)
            or isinstance(window["length_samples"], bool)
            or window["length_samples"] <= 0
        ):
            raise ValueError(f"{path} window {case_id} has an invalid length")
    return data


def validate_disjoint_corpora(corpora: Sequence[Mapping[str, Any]]) -> None:
    seen: dict[str, str] = {}
    for corpus in corpora:
        role = str(corpus["role"])
        for fixture in corpus["fixtures"]:
            fixture_id = str(fixture["id"])
            if fixture_id in seen:
                raise ValueError(
                    f"fixture {fixture_id} occurs in both {seen[fixture_id]} and {role} corpora"
                )
            seen[fixture_id] = role


def oracle_request_document(
    corpus: Mapping[str, Any],
    *,
    corpus_manifest_sha256: str,
    frozen_budgets: FrozenBudgets | None = None,
    frozen_budget_file_sha256: str | None = None,
    candidate: Candidate | None = None,
) -> dict[str, Any]:
    windows = corpus.get("difficult_windows", [])
    if not isinstance(windows, list) or not windows:
        raise ValueError("corpus must freeze at least one difficult window")
    if len(corpus_manifest_sha256) != 64:
        raise ValueError("oracle request requires the raw corpus manifest SHA-256")
    fixtures_by_id = {
        str(fixture["id"]): fixture for fixture in corpus.get("fixtures", [])
    }
    cases = []
    for window in windows:
        fixture_id = str(window["fixture_id"])
        fixture = fixtures_by_id.get(fixture_id)
        if fixture is None:
            raise ValueError(f"oracle window references unknown fixture {fixture_id}")
        if fixture.get("kind") != "generated":
            raise ValueError(
                f"exact-oracle window {window['case_id']} must use a generated seeded fixture"
            )
        generator_spec = fixture.get("generator")
        generator_spec_sha256 = fixture.get("generator_spec_sha256")
        if not isinstance(generator_spec, str) or not isinstance(
            generator_spec_sha256, str
        ):
            raise ValueError(f"oracle fixture {fixture_id} lacks frozen generator metadata")
        if hashlib.sha256(generator_spec.encode("utf-8")).hexdigest() != generator_spec_sha256:
            raise ValueError(f"oracle fixture {fixture_id} generator hash changed")
        seed = _parse_ecbeam2_generator_spec(generator_spec)
        if seed not in corpus["seeds"]:
            raise ValueError(f"oracle fixture {fixture_id} seed is not declared by the corpus")
        for filter_name in FILTERS:
            for channel in CHANNELS:
                source_rate = int(window["source_rate"])
                cases.append(
                    {
                        **window,
                        "source_case_id": window["case_id"],
                        "case_id": f"{window['case_id']}--{filter_name}--{channel}",
                        "filter": filter_name,
                        "channel": channel,
                        "wire_rate": WIRE_RATES[source_rate],
                        "generator_spec": generator_spec,
                        "generator_spec_sha256": generator_spec_sha256,
                        "seed": seed,
                    }
                )
    budget_binding = None
    if frozen_budgets is not None:
        if frozen_budget_file_sha256 is None or len(frozen_budget_file_sha256) != 64:
            raise ValueError("oracle request requires the frozen budget document SHA-256")
        budget_binding = {
            "schema_version": FROZEN_BUDGET_SCHEMA_VERSION,
            "document_sha256": frozen_budget_file_sha256,
            "calibration_digest": frozen_budgets.calibration_digest,
            "by_wire_rate": {
                str(wire_rate): {
                    "ultrasonic_ema_max": budget.ultrasonic_ema_max,
                    "signed_error_ema_abs_max": budget.signed_error_ema_abs_max,
                }
                for wire_rate, budget in sorted(frozen_budgets.by_wire_rate.items())
            },
        }
    if candidate is None:
        candidate = Candidate(
            "tail-aware-unconstrained-oracle-scaffold",
            ECBEAM2_MODULATOR,
            _active_params(),
            role="oracle",
            wire_budgets=(
                dict(frozen_budgets.by_wire_rate) if frozen_budgets is not None else None
            ),
            scale_probe_digest="0" * 64,
        )
    if candidate.modulator != ECBEAM2_MODULATOR:
        raise ValueError("exact oracle candidate must use EcBeam2")
    scale_probe_digest = candidate.scale_probe_digest
    if (
        not isinstance(scale_probe_digest, str)
        or len(scale_probe_digest) != 64
        or any(character not in "0123456789abcdef" for character in scale_probe_digest)
    ):
        raise ValueError("exact oracle candidate requires a scale-probe SHA-256")
    objective_configs: dict[str, Any] = {}
    objective_scale_bindings: dict[str, Any] = {}
    for wire_rate in sorted(WIRE_RATES.values()):
        params = candidate.canonical_params()
        if candidate.wire_params is not None:
            params.update(canonical_candidate_params(candidate.wire_params[wire_rate]))
        budget = (
            candidate.wire_budgets.get(wire_rate)
            if candidate.wire_budgets is not None
            else None
        )
        objective_configs[str(wire_rate)] = {
            "state_terminal_weight": float(
                params.get("ecbeam2_state_terminal_weight", 0.0)
            ),
            "state_deadzone": float(params.get("ecbeam2_state_deadzone", 0.0)),
            "state_deadzone_weight": float(
                params.get("ecbeam2_state_deadzone_weight", 0.0)
            ),
            "quantizer_regularizer": float(
                params.get("ecbeam2_quantizer_regularizer", 0.0)
            ),
            "ultrasonic_budget": (
                budget.ultrasonic_ema_max
                if budget is not None and candidate.budget_mode in ("ultrasonic", "both")
                else None
            ),
            "signed_error_budget": (
                budget.signed_error_ema_abs_max
                if budget is not None and candidate.budget_mode in ("signed-error", "both")
                else None
            ),
        }
        objective_scale_bindings[str(wire_rate)] = {
            "scale_probe_digest": scale_probe_digest,
            "wire_rate": wire_rate,
        }
    document = {
        "schema_version": ORACLE_SCHEMA_VERSION,
        "corpus_id": corpus["corpus_id"],
        "corpus_manifest_sha256": corpus_manifest_sha256,
        "profile": PROFILE,
        "profile_bindings": {
            str(wire_rate): PROFILE for wire_rate in sorted(WIRE_RATES.values())
        },
        "input_hash_encoding": "f64-le",
        "plant": dict(ECBEAM2_V1_PLANT),
        "constraint_budgets": budget_binding,
        "beam": {"m": 4, "n": 8},
        "exact_horizons": [8, 12, 16],
        "objective": "tail_adjusted_energy_increment",
        "feasibility": "ecbeam2-v1",
        "candidate_id": candidate.stable_id(),
        "objective_configs": objective_configs,
        "objective_scale_bindings": objective_scale_bindings,
        "start_mode": "active-prefix",
        "cases": cases,
        "required_result_fields": list(ORACLE_REQUIRED_RESULT_FIELDS),
    }
    digest = _stable_hash(document, length=64)
    document["request_digest"] = digest
    document["request_sha256"] = digest
    return document


def validate_oracle_results(request: Mapping[str, Any], results: Mapping[str, Any]) -> None:
    digest_document = dict(request)
    request_digest = digest_document.pop("request_digest", None)
    request_sha256 = digest_document.pop("request_sha256", None)
    expected_digest = _stable_hash(digest_document, length=64)
    if request_digest != expected_digest or request_sha256 != expected_digest:
        raise ValueError("oracle request digest mismatch")
    if request.get("plant") != ECBEAM2_V1_PLANT:
        raise ValueError("oracle request does not use the frozen EcBeam2 v1 plant")
    if request.get("beam") != {"m": 4, "n": 8}:
        raise ValueError("oracle request does not use the frozen M4/N8 beam")
    if request.get("exact_horizons") != [8, 12, 16]:
        raise ValueError("oracle request does not use the frozen exact horizons")
    if request.get("required_result_fields") != list(ORACLE_REQUIRED_RESULT_FIELDS):
        raise ValueError("oracle request changed its required result fields")
    if request.get("start_mode") != "active-prefix" or not request.get("candidate_id"):
        raise ValueError("oracle request is not candidate-bound to an active prefix")
    request_cases = request.get("cases")
    if not isinstance(request_cases, list) or not request_cases:
        raise ValueError("oracle request must contain cases")
    expanded_cells: set[tuple[str, str, str]] = set()
    axes_by_source_case: dict[str, tuple[Any, ...]] = {}
    for case in request_cases:
        if not isinstance(case, Mapping):
            raise ValueError("oracle request contains a non-object case")
        source_case_id = str(case.get("source_case_id", ""))
        filter_name = str(case.get("filter", ""))
        channel = str(case.get("channel", ""))
        if (
            not source_case_id
            or filter_name not in FILTERS
            or channel not in CHANNELS
            or case.get("case_id") != f"{source_case_id}--{filter_name}--{channel}"
        ):
            raise ValueError("oracle request contains an invalid expanded case identity")
        generator_spec = case.get("generator_spec")
        generator_digest = case.get("generator_spec_sha256")
        if (
            not isinstance(generator_spec, str)
            or not isinstance(generator_digest, str)
            or hashlib.sha256(generator_spec.encode("utf-8")).hexdigest()
            != generator_digest
            or _parse_ecbeam2_generator_spec(generator_spec) != case.get("seed")
        ):
            raise ValueError(
                f"oracle request case {case.get('case_id')} has an invalid generator/seed binding"
            )
        source_rate = case.get("source_rate")
        if source_rate not in WIRE_RATES or case.get("wire_rate") != WIRE_RATES[source_rate]:
            raise ValueError(
                f"oracle request case {case.get('case_id')} has an invalid wire-rate binding"
            )
        cell = (source_case_id, filter_name, channel)
        if cell in expanded_cells:
            raise ValueError(f"oracle request duplicates expanded cell {cell}")
        expanded_cells.add(cell)
        axes = (
            case.get("fixture_id"),
            case.get("category"),
            source_rate,
            case.get("wire_rate"),
            case.get("seed"),
            generator_spec,
            generator_digest,
            case.get("start_sample"),
            case.get("length_samples"),
        )
        previous_axes = axes_by_source_case.setdefault(source_case_id, axes)
        if previous_axes != axes:
            raise ValueError(f"oracle request changes source axes across {source_case_id}")
    source_case_ids = set(axes_by_source_case)
    expected_cells = {
        (source_case_id, filter_name, channel)
        for source_case_id in source_case_ids
        for filter_name in FILTERS
        for channel in CHANNELS
    }
    if expanded_cells != expected_cells:
        raise ValueError("oracle request does not cover every source-case/filter/channel cell")
    if results.get("schema_version") != ORACLE_SCHEMA_VERSION:
        raise ValueError("oracle results use an unexpected schema")
    for field in (
        "request_digest",
        "request_sha256",
        "corpus_id",
        "corpus_manifest_sha256",
        "profile",
        "profile_bindings",
        "input_hash_encoding",
        "plant",
        "constraint_budgets",
        "objective",
        "candidate_id",
        "objective_configs",
        "objective_scale_bindings",
        "start_mode",
    ):
        if results.get(field) != request.get(field):
            raise ValueError(f"oracle results do not match request {field}")
    expected_request_file_sha256 = hashlib.sha256(_canonical_json_bytes(request)).hexdigest()
    if results.get("request_file_sha256") != expected_request_file_sha256:
        raise ValueError("oracle results do not match the raw request file SHA-256")
    required = set(request["required_result_fields"])
    expected = {
        (str(case["case_id"]), horizon)
        for case in request_cases
        for horizon in request["exact_horizons"]
    }
    cases_by_id = {str(case["case_id"]): case for case in request_cases}
    actual: set[tuple[str, int]] = set()
    for row in results.get("results", []):
        missing = required - set(row)
        if missing:
            raise ValueError(f"oracle row is missing fields: {sorted(missing)}")
        key = (str(row["case_id"]), int(row["horizon"]))
        if key in actual:
            raise ValueError(f"duplicate oracle result {key}")
        actual.add(key)
        case = cases_by_id.get(key[0])
        if case is None:
            raise ValueError(f"oracle result {key} references an unknown case")
        identity_fields = (
            "source_case_id",
            "fixture_id",
            "filter",
            "channel",
            "source_rate",
            "wire_rate",
            "seed",
        )
        if any(row[field] != case[field] for field in identity_fields):
            raise ValueError(f"oracle result {key} does not match frozen case axes")
        if row["source_window_start_sample"] != case["start_sample"]:
            raise ValueError(f"oracle result {key} changed its frozen window start")
        prefix_sample_count = row["prefix_sample_count"]
        prefix_numerator = int(case["start_sample"]) * int(case["wire_rate"])
        expected_prefix_samples, prefix_remainder = divmod(
            prefix_numerator, int(case["source_rate"])
        )
        if (
            prefix_remainder != 0
            or not isinstance(prefix_sample_count, int)
            or isinstance(prefix_sample_count, bool)
            or prefix_sample_count != expected_prefix_samples
        ):
            raise ValueError(f"oracle result {key} has inconsistent exact prefix coverage")
        prefix_health_fields = (
            "prefix_constraint_escapes",
            "prefix_state_repairs",
            "prefix_all_nonfinite_resets",
            "prefix_invalid_input_substitutions",
            "prefix_output_length_events",
        )
        if any(
            not isinstance(row[field], int)
            or isinstance(row[field], bool)
            or row[field] < 0
            for field in prefix_health_fields
        ):
            raise ValueError(f"oracle result {key} has invalid prefix health counters")
        failed_prefix_health = {
            field: row[field] for field in prefix_health_fields if row[field] != 0
        }
        if failed_prefix_health:
            raise ValueError(
                f"oracle result {key} has an ineligible prefix: {failed_prefix_health}"
            )
        budget_binding = request.get("constraint_budgets")
        expected_wire_budget = (
            budget_binding.get("by_wire_rate", {}).get(str(case["wire_rate"]))
            if isinstance(budget_binding, Mapping)
            else None
        )
        expected_ultrasonic_budget = (
            expected_wire_budget.get("ultrasonic_ema_max")
            if isinstance(expected_wire_budget, Mapping)
            else None
        )
        expected_signed_budget = (
            expected_wire_budget.get("signed_error_ema_abs_max")
            if isinstance(expected_wire_budget, Mapping)
            else None
        )
        if (
            row["ultrasonic_budget"] != expected_ultrasonic_budget
            or row["signed_error_budget"] != expected_signed_budget
        ):
            raise ValueError(f"oracle result {key} does not use its frozen wire budget")
        objective_config = request["objective_configs"][str(case["wire_rate"])]
        if (
            row["ultrasonic_budget"] != objective_config["ultrasonic_budget"]
            or row["signed_error_budget"] != objective_config["signed_error_budget"]
        ):
            raise ValueError(f"oracle result {key} does not use candidate objective budgets")
        component_sum = sum(
            float(row[field])
            for field in (
                "reconstruction_objective",
                "state_terminal_cost",
                "state_barrier_cost",
                "quantizer_regularizer_cost",
            )
        )
        total = float(row["total_objective"])
        tolerance = 256.0 * sys.float_info.epsilon * max(abs(total), abs(component_sum), 1.0)
        if abs(total - component_sum) > tolerance or abs(float(row["objective"]) - total) > tolerance:
            raise ValueError(f"oracle result {key} objective components do not sum")
        state_delta = float(row["terminal_state_potential"]) - float(
            row["starting_state_potential"]
        )
        state_tolerance = 256.0 * sys.float_info.epsilon * max(
            abs(state_delta), abs(float(row["state_terminal_delta"])), 1.0
        )
        if abs(float(row["state_terminal_delta"]) - state_delta) > state_tolerance:
            raise ValueError(f"oracle result {key} state potential does not telescope")
        for field in ("prefix_sha256", "window_sha256"):
            digest = row[field]
            if (
                not isinstance(digest, str)
                or len(digest) != 64
                or any(character not in "0123456789abcdef" for character in digest)
            ):
                raise ValueError(f"oracle result {key} has invalid {field}")
        sequence = row["sequence_bits"]
        if not isinstance(sequence, list) or len(sequence) != key[1]:
            raise ValueError(f"oracle result {key} has the wrong sequence length")
        if any(
            not isinstance(bit, int) or isinstance(bit, bool) or bit not in {-1, 1}
            for bit in sequence
        ):
            raise ValueError(f"oracle result {key} has invalid sequence bits")
        if not isinstance(row["first_bit"], int) or row["first_bit"] not in {-1, 1}:
            raise ValueError(f"oracle result {key} has an invalid first bit")
        if row["first_bit"] != sequence[0]:
            raise ValueError(f"oracle result {key} first bit disagrees with its sequence")
        if (
            not isinstance(row["m4n8_first_bit"], int)
            or row["m4n8_first_bit"] not in {-1, 1}
        ):
            raise ValueError(f"oracle result {key} has an invalid M4/N8 first bit")
        reconstructed = row["reconstructed_outputs"]
        if not isinstance(reconstructed, list) or len(reconstructed) != key[1]:
            raise ValueError(f"oracle result {key} has the wrong reconstructed-output length")
        numeric_fields = (
            "objective",
            "starting_tail_energy",
            "causal_reconstruction_energy",
            "remaining_tail_energy",
            "tail_adjusted_energy",
            "causal_ultrasonic_energy",
            "maximum_state_overflow",
            "maximum_budget_violation",
        )
        if any(_parse_float(str(row[field])) is None for field in numeric_fields):
            raise ValueError(f"oracle result {key} contains a non-finite metric")
        if any(_parse_float(str(value)) is None for value in reconstructed):
            raise ValueError(f"oracle result {key} contains a non-finite reconstruction")
        if any(
            not isinstance(row[field], int) or isinstance(row[field], bool) or row[field] < 0
            for field in ("constraint_escapes", "state_repairs")
        ):
            raise ValueError(f"oracle result {key} has invalid fallback counters")
        if (
            not isinstance(row["complete_sequences"], int)
            or isinstance(row["complete_sequences"], bool)
            or row["complete_sequences"] <= 0
        ):
            raise ValueError(f"oracle result {key} has invalid sequence coverage")
        if not isinstance(row["state_feasible"], bool):
            raise ValueError(f"oracle result {key} has a non-boolean feasibility flag")
        if not isinstance(row["budgets_feasible"], bool):
            raise ValueError(f"oracle result {key} has a non-boolean budget flag")
        nonnegative_fields = (
            "starting_tail_energy",
            "causal_reconstruction_energy",
            "remaining_tail_energy",
            "causal_ultrasonic_energy",
            "maximum_state_overflow",
            "maximum_budget_violation",
        )
        if any(float(row[field]) < 0.0 for field in nonnegative_fields):
            raise ValueError(f"oracle result {key} contains a negative causal metric")
        reconstructed_energy = sum(float(value) ** 2 for value in reconstructed)
        reported_energy = float(row["causal_reconstruction_energy"])
        tolerance = 2.0e-9 * (1.0 + reconstructed_energy + reported_energy)
        if abs(reconstructed_energy - reported_energy) > tolerance:
            raise ValueError(f"oracle result {key} reconstruction energy identity failed")
        expected_tail_adjusted = (
            reported_energy
            + float(row["remaining_tail_energy"])
            - float(row["starting_tail_energy"])
        )
        reported_tail_adjusted = float(row["tail_adjusted_energy"])
        tolerance = 2.0e-9 * (
            1.0 + abs(expected_tail_adjusted) + abs(reported_tail_adjusted)
        )
        if abs(expected_tail_adjusted - reported_tail_adjusted) > tolerance:
            raise ValueError(f"oracle result {key} tail-adjusted identity failed")
        if abs(float(row["objective"]) - reported_tail_adjusted) > tolerance:
            raise ValueError(f"oracle result {key} formal objective identity failed")
        expected_state_feasible = (
            float(row["maximum_state_overflow"]) == 0.0 and row["state_repairs"] == 0
        )
        if row["state_feasible"] != expected_state_feasible:
            raise ValueError(f"oracle result {key} state feasibility is inconsistent")
        expected_budgets_feasible = (
            float(row["maximum_budget_violation"]) == 0.0
            and row["constraint_escapes"] == 0
        )
        if row["budgets_feasible"] != expected_budgets_feasible:
            raise ValueError(f"oracle result {key} budget feasibility is inconsistent")
    if actual != expected:
        raise ValueError(f"oracle result coverage mismatch: missing={sorted(expected - actual)}")

    by_case: dict[str, list[Mapping[str, Any]]] = {}
    for row in results["results"]:
        by_case.setdefault(str(row["case_id"]), []).append(row)
    for case_id, rows in by_case.items():
        m4_bits = {int(row["m4n8_first_bit"]) for row in rows}
        if len(m4_bits) != 1:
            raise ValueError(f"oracle case {case_id} changed its M4/N8 first bit by horizon")
        prefix_identities = {
            (
                int(row["source_window_start_sample"]),
                int(row["prefix_sample_count"]),
                str(row["prefix_sha256"]),
                float(row["starting_tail_energy"]),
            )
            for row in rows
        }
        if len(prefix_identities) != 1:
            raise ValueError(f"oracle case {case_id} changed its frozen starting state")


def oracle_comparison_summary(
    request: Mapping[str, Any],
    results: Mapping[str, Any],
    *,
    results_sha256: str,
) -> dict[str, Any]:
    validate_oracle_results(request, results)
    indexed = {
        (str(row["case_id"]), int(row["horizon"])): row
        for row in results["results"]
    }
    cases = []
    for case in request["cases"]:
        case_id = str(case["case_id"])
        n8 = indexed[(case_id, 8)]
        n12 = indexed[(case_id, 12)]
        n16 = indexed[(case_id, 16)]
        n16_output = [float(value) for value in n16["reconstructed_outputs"]]
        cases.append(
            {
                "case_id": case_id,
                "source_case_id": case["source_case_id"],
                "fixture_id": case["fixture_id"],
                "filter": case["filter"],
                "channel": case["channel"],
                "source_rate": case["source_rate"],
                "wire_rate": case["wire_rate"],
                "seed": case["seed"],
                "m4n8_vs_exact_n8": {
                    "m4n8_first_bit": n8["m4n8_first_bit"],
                    "exact_first_bit": n8["first_bit"],
                    "first_bit_disagrees": n8["m4n8_first_bit"] != n8["first_bit"],
                },
                "exact_horizon": {
                    "n8_first_bit": n8["first_bit"],
                    "n12_first_bit": n12["first_bit"],
                    "n16_first_bit": n16["first_bit"],
                    "n8_vs_n12_first_bit_disagrees": n8["first_bit"] != n12["first_bit"],
                    "n8_vs_n16_first_bit_disagrees": n8["first_bit"] != n16["first_bit"],
                    "objective_n8": n8["objective"],
                    "objective_n12": n12["objective"],
                    "objective_n16": n16["objective"],
                    "objective_per_sample_n8": float(n8["objective"]) / 8.0,
                    "objective_per_sample_n12": float(n12["objective"]) / 12.0,
                    "objective_per_sample_n16": float(n16["objective"]) / 16.0,
                    "n12_minus_n8_objective_per_sample": (
                        float(n12["objective"]) / 12.0 - float(n8["objective"]) / 8.0
                    ),
                    "n16_minus_n8_objective_per_sample": (
                        float(n16["objective"]) / 16.0 - float(n8["objective"]) / 8.0
                    ),
                },
                "n16_external": {
                    "prefix_eligible": True,
                    "prefix_constraint_escapes": n16["prefix_constraint_escapes"],
                    "prefix_state_repairs": n16["prefix_state_repairs"],
                    "prefix_all_nonfinite_resets": n16["prefix_all_nonfinite_resets"],
                    "prefix_invalid_input_substitutions": n16[
                        "prefix_invalid_input_substitutions"
                    ],
                    "prefix_output_length_events": n16["prefix_output_length_events"],
                    "state_feasible": n16["state_feasible"],
                    "budgets_feasible": n16["budgets_feasible"],
                    "constraint_escapes": n16["constraint_escapes"],
                    "state_repairs": n16["state_repairs"],
                    "causal_reconstruction_energy": n16["causal_reconstruction_energy"],
                    "causal_ultrasonic_energy": n16["causal_ultrasonic_energy"],
                    "remaining_tail_energy": n16["remaining_tail_energy"],
                    "reconstructed_output_peak": max(map(abs, n16_output), default=0.0),
                    "reconstructed_output_rms": math.sqrt(
                        sum(value * value for value in n16_output) / len(n16_output)
                    ),
                    "prefix_sha256": n16["prefix_sha256"],
                    "window_sha256": n16["window_sha256"],
                },
            }
        )
    return {
        "schema_version": "ecbeam2-exact-oracle-summary-v1",
        "request_digest": request["request_digest"],
        "results_sha256": results_sha256,
        "corpus_id": request["corpus_id"],
        "corpus_manifest_sha256": request["corpus_manifest_sha256"],
        "profile_bindings": request["profile_bindings"],
        "plant": request["plant"],
        "constraint_budgets": request["constraint_budgets"],
        "case_count": len(cases),
        "m4n8_exact_n8_first_bit_disagreements": sum(
            row["m4n8_vs_exact_n8"]["first_bit_disagrees"] for row in cases
        ),
        "n8_n12_first_bit_disagreements": sum(
            row["exact_horizon"]["n8_vs_n12_first_bit_disagrees"] for row in cases
        ),
        "n8_n16_first_bit_disagreements": sum(
            row["exact_horizon"]["n8_vs_n16_first_bit_disagrees"] for row in cases
        ),
        "n16_infeasible_cases": sum(
            not (
                row["n16_external"]["state_feasible"]
                and row["n16_external"]["budgets_feasible"]
            )
            for row in cases
        ),
        "cases": cases,
    }


@dataclass(frozen=True)
class RunSpec:
    candidate: Candidate
    index: int
    out_dir: Path
    binary: Path
    source_rate: int
    corpus_manifest_path: Path | None = None

    @property
    def candidate_id(self) -> str:
        return self.candidate.stable_id()

    @property
    def candidate_dir(self) -> Path:
        return (
            self.out_dir
            / f"{self.index:02d}-{self.candidate_id}"
            / f"source-{self.source_rate}"
        )


@dataclass
class RunResult:
    spec: RunSpec
    commands: list[list[str]]
    status: str
    exit_code: int | None
    elapsed_s: float
    rows: dict[tuple[str, int], dict[str, str]] = field(default_factory=dict)
    errors: list[str] = field(default_factory=list)


def write_run_config(spec: RunSpec) -> Path:
    path = spec.candidate_dir / "candidate_config.json"
    params = spec.candidate.canonical_params()
    wire_rate = WIRE_RATES[spec.source_rate]
    if spec.candidate.wire_params is not None:
        params.update(
            canonical_candidate_params(spec.candidate.wire_params[wire_rate])
        )
    if spec.candidate.wire_budgets is not None:
        budget = spec.candidate.wire_budgets[wire_rate]
        if spec.candidate.budget_mode in ("ultrasonic", "both"):
            params["ecbeam2_ultrasonic_budget"] = budget.ultrasonic_ema_max
        if spec.candidate.budget_mode in ("signed-error", "both"):
            params["ecbeam2_signed_error_budget"] = budget.signed_error_ema_abs_max
    write_candidate_config(
        path,
        spec.candidate.label,
        params,
        baseline=spec.candidate.baseline,
    )
    return path


def build_command(spec: RunSpec) -> list[str]:
    config = write_run_config(spec)
    if spec.candidate.role in {"stability", "budget"}:
        mode = (
            "scale-probe"
            if spec.candidate.label.startswith("objective-scale-probe-rho")
            else spec.candidate.role
        )
        return [
            str(spec.binary),
            "--ecbeam2-qualification",
            "--mode",
            mode,
            "--filters",
            ",".join(FILTERS),
            "--modulator",
            spec.candidate.modulator,
            "--source-rates",
            str(spec.source_rate),
            "--candidate-config",
            str(config),
            "--corpus-manifest",
            str(spec.corpus_manifest_path.resolve()),
            "--allow-exploratory",
            "--out",
            str(spec.candidate_dir),
        ]
    command = [
        str(spec.binary),
        "--selectable-dsd-matrix",
        "--selectable-filter",
        ",".join(FILTERS),
        "--selectable-modulator",
        spec.candidate.modulator,
        "--source-rates",
        str(spec.source_rate),
        "--rates",
        "64",
        "--candidate-config",
        str(config),
    ]
    if spec.corpus_manifest_path is not None:
        command.extend(
            ["--ecbeam2-corpus-manifest", str(spec.corpus_manifest_path.resolve())]
        )
    command.extend(
        [
            "--allow-exploratory",
            "--budget-cell-cap",
            str(len(FILTERS)),
            "--out",
            str(spec.candidate_dir),
        ]
    )
    return command


def read_qualification_rows(spec: RunSpec) -> dict[tuple[str, int], dict[str, str]]:
    path = spec.candidate_dir / "ecbeam2_qualification_report.json"
    if not path.is_file():
        return {}
    report = json.loads(path.read_text(encoding="utf-8"))
    if report.get("schema_version") != "ecbeam2-qualification-report-v1":
        raise ValueError(f"{path} uses an unexpected qualification schema")
    measurements = report.get("measurements")
    if not isinstance(measurements, list):
        raise ValueError(f"{path} lacks qualification measurements")
    rows: dict[tuple[str, int], dict[str, str]] = {}
    sum_keys = {
        "ecbeam2_committed_samples",
        "ecbeam2_total_committed_samples",
        "ecbeam2_constraint_escape",
        "ecbeam2_state_repair_fallback",
        "ecbeam2_all_nonfinite_resets",
        "ecbeam2_observer_desynchronizations",
        "ecbeam2_invalid_input_substitutions",
        "ecbeam2_output_length_error",
        "ecbeam2_renderer_truncation_events",
        "ecbeam2_renderer_discarded_left_bits",
        "ecbeam2_renderer_discarded_right_bits",
        "ecbeam2_ultrasonic_budget_escape_count",
        "ecbeam2_signed_error_budget_escape_count",
        "ecbeam2_both_budget_escape_count",
        "stability_resets",
        "state_clamps",
    }
    max_keys = {
        "ecbeam2_maximum_state_overflow",
        "ecbeam2_maximum_budget_violation",
        "ecbeam2_maximum_consecutive_constraint_escapes",
        "ecbeam2_maximum_consecutive_state_repairs",
    }
    for filter_name in FILTERS:
        selected = [
            measurement
            for measurement in measurements
            if measurement.get("filter") == filter_name
            and measurement.get("source_rate") == spec.source_rate
        ]
        if not selected:
            continue
        parsed = [
            _notes({"candidate_notes": ";".join(measurement.get("notes", []))})
            for measurement in selected
        ]
        aggregated: dict[str, str] = {}
        for key in sum_keys:
            values = [_parse_int(notes.get(key)) for notes in parsed]
            if all(value is not None for value in values):
                aggregated[key] = str(sum(int(value) for value in values))
        for key in max_keys:
            values = [_parse_float(notes.get(key)) for notes in parsed]
            if all(value is not None for value in values):
                aggregated[key] = f"{max(float(value) for value in values):.12g}"
        survivors = [_parse_int(notes.get("ecbeam2_min_survivors")) for notes in parsed]
        if all(value is not None for value in survivors):
            aggregated["ecbeam2_min_survivors"] = str(min(int(value) for value in survivors))
        for key in (
            "ecbeam2_first_constraint_escape_sequence",
            "ecbeam2_first_state_repair_sequence",
        ):
            values = [_parse_int(notes.get(key)) for notes in parsed]
            present = [int(value) for value in values if value is not None]
            if present:
                aggregated[key] = str(min(present))
        for key in (
            "ecbeam2_last_constraint_escape_sequence",
            "ecbeam2_last_state_repair_sequence",
        ):
            values = [_parse_int(notes.get(key)) for notes in parsed]
            present = [int(value) for value in values if value is not None]
            if present:
                aggregated[key] = str(max(present))
        stage_counts = [0] * 7
        stage_maxima = [0.0] * 7
        for notes in parsed:
            counts = notes.get("ecbeam2_state_repair_stage_counts", "").split("-")
            maxima = notes.get("ecbeam2_maximum_normalized_state_by_stage", "").split("-")
            if len(counts) == 7:
                stage_counts = [left + int(right) for left, right in zip(stage_counts, counts)]
            if len(maxima) == 7:
                stage_maxima = [
                    max(left, float(right)) for left, right in zip(stage_maxima, maxima)
                ]
        aggregated["ecbeam2_state_repair_stage_counts"] = "-".join(map(str, stage_counts))
        aggregated["ecbeam2_maximum_normalized_state_by_stage"] = "-".join(
            f"{value:.12g}" for value in stage_maxima
        )
        committed_energy = sum(
            float(value)
            for notes in parsed
            if (value := _parse_float(notes.get("ecbeam2_committed_output_energy")))
            is not None
        )
        committed_samples = int(aggregated.get("ecbeam2_committed_samples", "0"))
        aggregated["ecbeam2_committed_output_energy"] = f"{committed_energy:.12g}"
        if committed_samples > 0:
            aggregated["ecbeam2_committed_output_energy_mean"] = (
                f"{committed_energy / committed_samples:.12g}"
            )
        for term in SCALE_TERMS:
            prefix = f"ecbeam2_scale_{term}"
            best = max(
                parsed,
                key=lambda notes: _parse_float(notes.get(f"{prefix}_p95")) or 0.0,
            )
            for quantile in ("median", "p95", "p99", "max"):
                value = best.get(f"{prefix}_{quantile}")
                if value is not None:
                    aggregated[f"{prefix}_{quantile}"] = value
        digest_payload = {
            str(measurement["case_id"]): measurement["native_stereo_sha256"]
            for measurement in selected
        }
        aggregated["ecbeam2_qualification_bitstream_digest"] = _stable_hash(
            digest_payload, length=64
        )
        rows[(filter_name, spec.source_rate)] = {
            "candidate_notes": ";".join(
                f"{key}={value}" for key, value in sorted(aggregated.items())
            ),
            "render_ms": f"{sum(float(row['render_ms']) for row in selected):.12g}",
        }
    return rows


QUALIFICATION_PARITY_NOTE_KEYS = (
    "ecbeam2_committed_samples",
    "ecbeam2_min_survivors",
    "ecbeam2_constraint_escape",
    "ecbeam2_state_repair_fallback",
    "ecbeam2_all_nonfinite_resets",
    "ecbeam2_invalid_input_substitutions",
    "ecbeam2_output_length_error",
    "ecbeam2_committed_output_energy",
    "ecbeam2_committed_output_energy_mean",
    "ecbeam2_maximum_state_overflow",
    "ecbeam2_state_repair_stage_counts",
    "ecbeam2_scale_reconstruction_increment_abs_p95",
    "ecbeam2_scale_state_terminal_delta_abs_p95",
    "ecbeam2_scale_state_barrier_raw_p95",
    "ecbeam2_scale_quantizer_error_squared_p95",
)


def validate_lightweight_full_parity(
    lightweight_path: Path, full_corpus_path: Path
) -> dict[str, Any]:
    lightweight = json.loads(lightweight_path.read_text(encoding="utf-8"))
    full = json.loads(full_corpus_path.read_text(encoding="utf-8"))
    light_rows = {
        (
            row["case_id"],
            row["filter"],
            row["source_rate"],
            row["wire_rate"],
        ): row
        for row in lightweight.get("measurements", [])
    }
    full_rows = {
        (
            row["case_id"],
            row["filter"],
            row["source_rate"],
            row["wire_rate"],
        ): row
        for row in full.get("measurements", [])
    }
    if set(light_rows) != set(full_rows):
        raise ValueError("lightweight/full qualification coverage differs")
    compared = []
    for identity in sorted(light_rows):
        light = light_rows[identity]
        full_row = full_rows[identity]
        metric = full_row.get("metric", {})
        if (
            light.get("native_left_sha256") != metric.get("native_left_sha256")
            or light.get("native_right_sha256") != metric.get("native_right_sha256")
        ):
            raise ValueError(f"lightweight/full bitstream digest mismatch for {identity}")
        light_notes = _notes({"candidate_notes": ";".join(light.get("notes", []))})
        full_notes = _notes({"candidate_notes": ";".join(metric.get("notes", []))})
        for key in QUALIFICATION_PARITY_NOTE_KEYS:
            if light_notes.get(key) != full_notes.get(key):
                raise ValueError(
                    f"lightweight/full diagnostic mismatch for {identity}: "
                    f"{key}={light_notes.get(key)!r}/{full_notes.get(key)!r}"
                )
        compared.append("|".join(map(str, identity)))
    return {
        "schema_version": "ecbeam2-qualification-parity-v1",
        "lightweight_report_sha256": _sha256_file(lightweight_path),
        "full_corpus_report_sha256": _sha256_file(full_corpus_path),
        "compared_measurements": compared,
    }


def _ecbeam2_fixture_category(fixture: Mapping[str, Any]) -> str:
    generator = str(fixture.get("generator", "")).split("|", 1)[0]
    return {
        "program_multitone": "program",
        "pink_noise": "broadband",
        "low_level_tones": "low-level-tone",
        "tiny_dc": "tiny-dc",
        "high_frequency": "high-frequency",
        "fades_overload": "overload-recovery",
        "spur_windows": "known-spur",
    }.get(generator, "fixture")


def _ecbeam2_generated_fixture_frames(
    manifest: Mapping[str, Any], fixture_id: str, source_rate: int
) -> int:
    minimum = source_rate // 2
    required = minimum
    for window in manifest["difficult_windows"]:
        if window["fixture_id"] != fixture_id:
            continue
        end = int(window["start_sample"]) + int(window["length_samples"])
        window_rate = int(window["source_rate"])
        converted = (end * source_rate + window_rate - 1) // window_rate
        required = max(required, converted)
    return required


def _ecbeam2_committed_fixture_frames(
    fixture: Mapping[str, Any], source_rate: int
) -> int:
    # The native loader rounds the 44.1 kHz excerpt bounds, then its fixed
    # 44.1 -> 48 kHz exact-rational bridge emits only FIR-backed samples.
    start = math.floor(float(fixture["start_sec"]) * 44_100 + 0.5)
    end = math.floor(float(fixture["end_sec"]) * 44_100 + 0.5)
    source_frames = end - start
    if source_rate == 44_100:
        return source_frames
    if source_rate == 48_000:
        fir_half_width = 512
        available = max(source_frames - fir_half_width, 0)
        return (available * 160 + 147 - 1) // 147
    raise ValueError(f"unsupported EcBeam2 corpus source rate {source_rate}")


def _expected_native_corpus_cases(
    manifest: Mapping[str, Any], source_rate: int
) -> dict[str, dict[str, Any]]:
    """Mirror native difficult-window/full-fixture materialization metadata."""

    cases: dict[str, dict[str, Any]] = {}
    for fixture in manifest["fixtures"]:
        fixture_id = str(fixture["id"])
        generator_seed = (
            _parse_ecbeam2_generator_spec(str(fixture["generator"]))
            if fixture["kind"] == "generated"
            else None
        )
        windows = [
            window
            for window in manifest["difficult_windows"]
            if window["fixture_id"] == fixture_id
            and int(window["source_rate"]) == source_rate
        ]
        if windows:
            materialized = windows
        else:
            if fixture["kind"] == "generated":
                length_samples = _ecbeam2_generated_fixture_frames(
                    manifest, fixture_id, source_rate
                )
            else:
                length_samples = _ecbeam2_committed_fixture_frames(fixture, source_rate)
            materialized = [
                {
                    "case_id": f"{fixture_id}-full-{source_rate}",
                    "fixture_id": fixture_id,
                    "category": _ecbeam2_fixture_category(fixture),
                    "source_rate": source_rate,
                    "start_sample": 0,
                    "length_samples": length_samples,
                }
            ]
        for case in materialized:
            case_id = str(case["case_id"])
            if case_id in cases:
                raise ValueError(f"duplicate materialized EcBeam2 corpus case {case_id}")
            cases[case_id] = {
                "case_id": case_id,
                "fixture_id": fixture_id,
                "category": str(case["category"]),
                "source_rate": source_rate,
                "wire_rate": WIRE_RATES[source_rate],
                "generator_seed": generator_seed,
                "start_sample": int(case["start_sample"]),
                "length_samples": int(case["length_samples"]),
            }
    return cases


def _canonical_report_modulator(actual: Any, requested: str) -> str:
    row = {"modulator": actual} if isinstance(actual, str) else {"modulator": ""}
    return requested if _row_matches_modulator(row, requested) else str(actual)


def _format_corpus_identity(identity: tuple[Any, ...]) -> str:
    return "/".join(str(value) for value in identity)


def validate_native_corpus_report(spec: RunSpec) -> list[str]:
    if spec.corpus_manifest_path is None:
        return []
    report_path = spec.candidate_dir / "ecbeam2_corpus_report.json"
    if not report_path.is_file():
        return [f"missing native corpus report {report_path}"]
    try:
        report = json.loads(report_path.read_text(encoding="utf-8"))
        manifest = load_corpus_manifest(spec.corpus_manifest_path)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        return [f"invalid native corpus evidence: {error}"]
    errors = []
    expected_manifest_hash = hashlib.sha256(spec.corpus_manifest_path.read_bytes()).hexdigest()
    expected_wire_rate = WIRE_RATES[spec.source_rate]
    scalar_expectations = {
        "schema_version": "ecbeam2-corpus-report-v1",
        "corpus_schema_version": CORPUS_SCHEMA_VERSION,
        "manifest_sha256": expected_manifest_hash,
        "corpus_id": manifest["corpus_id"],
        "role": manifest["role"],
        "measurement_version": manifest["measurement_version"],
        "scoring_version": manifest["scoring_version"],
        "fixture_set_version": manifest["fixture_set_version"],
    }
    for field, expected_value in scalar_expectations.items():
        if report.get(field) != expected_value:
            errors.append(
                f"native corpus report {field}={report.get(field)!r}, expected {expected_value!r}"
            )
    vector_expectations = {
        "declared_source_rates": list(SOURCE_RATES),
        "declared_wire_rates": [WIRE_RATES[rate] for rate in SOURCE_RATES],
        "declared_filters": list(FILTERS),
        "declared_seeds": list(manifest["seeds"]),
        "selected_source_rates": [spec.source_rate],
        "selected_wire_rates": [expected_wire_rate],
        "selected_filters": list(FILTERS),
        "selected_modulators": [spec.candidate.modulator],
    }
    for field, expected_value in vector_expectations.items():
        if report.get(field) != expected_value:
            errors.append(
                f"native corpus report {field}={report.get(field)!r}, expected {expected_value!r}"
            )
    try:
        expected_cases = _expected_native_corpus_cases(manifest, spec.source_rate)
    except ValueError as error:
        return [f"invalid native corpus expectations: {error}"]
    expected_fixture_cells = len(manifest["fixtures"]) * len(FILTERS)
    reported_expected_cells = report.get("expected_fixture_cells")
    rendered_cells = report.get("rendered_fixture_cells")
    if (
        type(reported_expected_cells) is not int
        or reported_expected_cells != expected_fixture_cells
        or type(rendered_cells) is not int
        or rendered_cells != expected_fixture_cells
    ):
        errors.append(
            "native corpus fixture coverage mismatch: "
            f"rendered={rendered_cells}, reported_expected={reported_expected_cells}, "
            f"manifest_expected={expected_fixture_cells}"
        )
    summaries = report.get("cell_summaries")
    if not isinstance(summaries, list):
        errors.append("native corpus report lacks cell summaries")
    else:
        expected_summary_identities = {
            (
                filter_name,
                spec.source_rate,
                expected_wire_rate,
                spec.candidate.modulator,
            )
            for filter_name in FILTERS
        }
        actual_summary_identities: list[tuple[Any, ...]] = []
        for row in summaries:
            if not isinstance(row, Mapping):
                errors.append("native corpus report contains a non-object summary")
                continue
            identity = (
                row.get("filter"),
                row.get("source_rate"),
                row.get("wire_rate"),
                _canonical_report_modulator(
                    row.get("modulator"), spec.candidate.modulator
                ),
            )
            actual_summary_identities.append(identity)
            if row.get("rendered_cases") != len(expected_cases):
                errors.append(
                    f"native corpus summary {_format_corpus_identity(identity)} "
                    f"rendered_cases={row.get('rendered_cases')!r}, "
                    f"expected {len(expected_cases)}"
                )
            if row.get("rendered_fixtures") != len(manifest["fixtures"]):
                errors.append(
                    f"native corpus summary {_format_corpus_identity(identity)} "
                    f"rendered_fixtures={row.get('rendered_fixtures')!r}, "
                    f"expected {len(manifest['fixtures'])}"
                )
            hard_failure_count = row.get("hard_failure_count")
            if (
                not isinstance(hard_failure_count, int)
                or isinstance(hard_failure_count, bool)
                or hard_failure_count < 0
            ):
                errors.append("native corpus summary has invalid hard_failure_count")
            elif hard_failure_count > 0:
                errors.append(
                    f"native corpus {row.get('filter')} hard_failure_count={hard_failure_count}"
                )
        summary_counts = {
            identity: actual_summary_identities.count(identity)
            for identity in set(actual_summary_identities)
        }
        missing_summaries = expected_summary_identities - set(actual_summary_identities)
        extra_summaries = set(actual_summary_identities) - expected_summary_identities
        duplicate_summaries = {
            identity for identity, count in summary_counts.items() if count != 1
        }
        if missing_summaries or extra_summaries or duplicate_summaries:
            errors.append(
                "native corpus summary coverage mismatch: "
                f"missing={sorted(map(_format_corpus_identity, missing_summaries))}, "
                f"extra={sorted(map(_format_corpus_identity, extra_summaries))}, "
                f"duplicates={sorted(map(_format_corpus_identity, duplicate_summaries))}"
            )
    hard_failures = report.get("hard_failures")
    if not isinstance(hard_failures, list):
        errors.append("native corpus report hard_failures is not a list")
    else:
        errors.extend(f"native corpus: {failure}" for failure in hard_failures)
    measurements = report.get("measurements")
    if not isinstance(measurements, list) or not measurements:
        errors.append("native corpus report lacks measurements")
    else:
        expected_measurement_identities = {
            (
                case_id,
                filter_name,
                spec.source_rate,
                expected_wire_rate,
                spec.candidate.modulator,
            )
            for case_id in expected_cases
            for filter_name in FILTERS
        }
        actual_measurement_identities: list[tuple[Any, ...]] = []
        for row in measurements:
            if not isinstance(row, Mapping):
                errors.append("native corpus report contains a non-object measurement")
                continue
            identity = (
                row.get("case_id"),
                row.get("filter"),
                row.get("source_rate"),
                row.get("wire_rate"),
                _canonical_report_modulator(
                    row.get("modulator"), spec.candidate.modulator
                ),
            )
            actual_measurement_identities.append(identity)
            if (
                row.get("manifest_sha256") != expected_manifest_hash
                or row.get("corpus_id") != manifest["corpus_id"]
                or row.get("role") != manifest["role"]
            ):
                errors.append(
                    "native corpus measurement provenance mismatch for "
                    f"{_format_corpus_identity(identity)}"
                )
            expected_case = expected_cases.get(row.get("case_id"))
            if expected_case is not None:
                for field in (
                    "fixture_id",
                    "category",
                    "source_rate",
                    "wire_rate",
                    "generator_seed",
                    "start_sample",
                    "length_samples",
                ):
                    actual_value = row.get(field)
                    expected_value = expected_case[field]
                    wrong_integer_type = (
                        isinstance(expected_value, int)
                        and not isinstance(expected_value, bool)
                        and type(actual_value) is not int
                    )
                    if actual_value != expected_value or wrong_integer_type:
                        errors.append(
                            f"native corpus measurement {row.get('case_id')} {field}="
                            f"{actual_value!r}, expected {expected_value!r}"
                        )
            metric = row.get("metric")
            if not isinstance(metric, Mapping):
                errors.append("native corpus measurement lacks metrics")
                continue
            for field in ("native_left_sha256", "native_right_sha256"):
                digest = metric.get(field)
                if (
                    not isinstance(digest, str)
                    or len(digest) != 64
                    or any(character not in "0123456789abcdef" for character in digest)
                ):
                    errors.append(f"native corpus measurement has invalid {field}")
        measurement_counts = {
            identity: actual_measurement_identities.count(identity)
            for identity in set(actual_measurement_identities)
        }
        missing_measurements = expected_measurement_identities - set(
            actual_measurement_identities
        )
        extra_measurements = set(actual_measurement_identities) - (
            expected_measurement_identities
        )
        duplicate_measurements = {
            identity for identity, count in measurement_counts.items() if count != 1
        }
        if missing_measurements or extra_measurements or duplicate_measurements:
            errors.append(
                "native corpus measurement coverage mismatch: "
                f"missing={sorted(map(_format_corpus_identity, missing_measurements))}, "
                f"extra={sorted(map(_format_corpus_identity, extra_measurements))}, "
                f"duplicates={sorted(map(_format_corpus_identity, duplicate_measurements))}"
            )
    return errors


def _row_matches_modulator(row: Mapping[str, str], requested: str) -> bool:
    actual = row.get("modulator", "")
    if requested == A1_MODULATOR:
        return actual == A1_MODULATOR or actual.startswith("EcBeamM")
    if requested == ECBEAM2_MODULATOR:
        return actual == ECBEAM2_MODULATOR or actual.startswith("EcBeam2M")
    return actual == requested


def read_matrix_rows(path: Path, modulator: str) -> dict[tuple[str, int], dict[str, str]]:
    rows: dict[tuple[str, int], dict[str, str]] = {}
    if not path.exists():
        return rows
    with path.open(newline="", encoding="utf-8") as handle:
        for row in csv.DictReader(handle):
            source_rate = _parse_int(row.get("source_rate"))
            key = (row.get("filter", ""), source_rate or 0)
            if (
                key in EXPECTED_CELLS
                and row.get("dsd_rate") == "DSD64"
                and row.get("path_variant") == "direct"
                and _row_matches_modulator(row, modulator)
            ):
                rows[key] = row
    return rows


def run_spec(spec: RunSpec, dry_run: bool) -> RunResult:
    spec.candidate_dir.mkdir(parents=True, exist_ok=True)
    command = build_command(spec)
    if dry_run:
        return RunResult(spec, [command], "dry-run", None, 0.0)
    env = os.environ.copy()
    env.setdefault("RUSTFLAGS", "-C target-cpu=native")
    env.setdefault("FOZMO_DSD_PREMOD_WINDOWS", "1")
    env["FOZMO_DSD_DUMP_BITS"] = str(spec.candidate_dir)
    start = time.monotonic()
    with (spec.candidate_dir / "stdout.log").open("w", encoding="utf-8") as stdout:
        with (spec.candidate_dir / "stderr.log").open("w", encoding="utf-8") as stderr:
            proc = subprocess.run(
                command,
                cwd=ROOT,
                env=env,
                stdout=stdout,
                stderr=stderr,
                check=False,
            )
    qualification = "--ecbeam2-qualification" in command
    rows = (
        read_qualification_rows(spec)
        if qualification
        else read_matrix_rows(spec.candidate_dir / "dsd_rankings.csv", spec.candidate.modulator)
    )
    errors = []
    if qualification:
        report_path = spec.candidate_dir / "ecbeam2_qualification_report.json"
        if not report_path.is_file():
            errors.append(f"missing qualification report {report_path}")
        else:
            report = json.loads(report_path.read_text(encoding="utf-8"))
            expected_config = Path(command[command.index("--candidate-config") + 1])
            expected_mode = command[command.index("--mode") + 1]
            if report.get("mode") != expected_mode:
                errors.append("qualification report mode mismatch")
            if report.get("candidate_config_sha256") != _sha256_file(expected_config):
                errors.append("qualification candidate-config digest mismatch")
            if report.get("selected_modulator") != spec.candidate.modulator:
                errors.append("qualification modulator mismatch")
            if report.get("selected_source_rates") != [spec.source_rate]:
                errors.append("qualification source-rate mismatch")
            if report.get("selected_filters") != list(FILTERS):
                errors.append("qualification filter mismatch")
    else:
        errors.extend(validate_native_corpus_report(spec))
    expected_cells = {(filter_name, spec.source_rate) for filter_name in FILTERS}
    if set(rows) != expected_cells:
        errors.append(f"matrix coverage mismatch: {sorted(rows)}")
    if proc.returncode not in (0, 2):
        errors.append(f"ecbeam2_quality exited {proc.returncode}")
    return RunResult(
        spec,
        [command],
        "complete" if not errors else "failed",
        proc.returncode,
        time.monotonic() - start,
        rows,
        errors,
    )


def merge_wire_results(results: Sequence[RunResult]) -> list[RunResult]:
    grouped: dict[str, list[RunResult]] = {}
    for result in results:
        grouped.setdefault(result.spec.candidate_id, []).append(result)
    merged: list[RunResult] = []
    for group in grouped.values():
        group = sorted(group, key=lambda result: result.spec.source_rate)
        source_rates = {result.spec.source_rate for result in group}
        if source_rates != set(SOURCE_RATES):
            raise ValueError(
                f"candidate {group[0].spec.candidate_id} source coverage is {sorted(source_rates)}"
            )
        rows: dict[tuple[str, int], dict[str, str]] = {}
        for result in group:
            overlap = set(rows) & set(result.rows)
            if overlap:
                raise ValueError(f"duplicate matrix cells while merging: {sorted(overlap)}")
            rows.update(result.rows)
        errors = [error for result in group for error in result.errors]
        exit_codes = [result.exit_code for result in group if result.exit_code is not None]
        merged.append(
            RunResult(
                spec=group[0].spec,
                commands=[command for result in group for command in result.commands],
                status="failed" if errors else group[0].status,
                exit_code=max(exit_codes) if exit_codes else None,
                elapsed_s=sum(result.elapsed_s for result in group),
                rows=rows,
                errors=errors,
            )
        )
    return sorted(merged, key=lambda result: result.spec.index)


def _metric(row: Mapping[str, str], name: str) -> float | None:
    field = {
        "worst_sinad_db": "inband_snr_worst_db",
        "spur_margin_db": "inband_noise_spur_margin_db",
        "hf_residual_db": "high_freq_worst_residual_db",
        "multitone_residual_db": "multitone_residual_db",
        "overload_recovery_db": "overload_recovery_dbfs",
        "inband_noise_worst_rms_dbfs": "inband_noise_worst_rms_dbfs",
        "stereo_snr_worst_mismatch_db": "stereo_snr_worst_mismatch_db",
        "idle_worst_tone_dbfs": "idle_worst_tone_dbfs",
        "low_level_worst_residual_db": "low_level_worst_residual_db",
        "low_level_worst_spur_dbfs": "low_level_worst_spur_dbfs",
        "high_freq_worst_spur_dbfs": "high_freq_worst_spur_dbfs",
        "multitone_spur_dbfs": "multitone_spur_dbfs",
        "ultrasonic_24_50k_max_dbfs": "ultrasonic_24_50k_max_dbfs",
        "ultrasonic_50_100k_max_dbfs": "ultrasonic_50_100k_max_dbfs",
        "ultrasonic_100_200k_max_dbfs": "ultrasonic_100_200k_max_dbfs",
        "quality_score": "constrained_quality_score",
        "render_ms": "render_ms",
    }[name]
    return _parse_float(row.get(field))


def _better_delta(name: str, candidate: float, baseline: float) -> float:
    if name in {"worst_sinad_db", "spur_margin_db", "quality_score"}:
        return candidate - baseline
    return baseline - candidate


def _worst_aggregate(name: str, values: Sequence[float]) -> float:
    if not values:
        return float("nan")
    if name in {"worst_sinad_db", "spur_margin_db", "quality_score"}:
        return min(values)
    return max(values)


def _note_int(notes: Mapping[str, str], key: str) -> int | None:
    return _parse_int(notes.get(key))


def hard_failure_reasons(
    result: RunResult,
    *,
    require_ecbeam2_diagnostics: bool = True,
    require_active_survivor_diagnostics: bool = True,
) -> list[str]:
    reasons = list(result.errors)
    for cell in EXPECTED_CELLS:
        row = result.rows.get(cell)
        if row is None:
            reasons.append(f"{cell}: missing")
            continue
        # The general DSD harness has absolute quality gates designed for its
        # broad matrix. EcBeam2's frozen winner contract is relative to the
        # same-run A1 baseline, so those status/hard-failure strings are not
        # health failures here. Explicit counters, overload, density, corpus
        # validity, directionality, and protected-regression checks below are
        # the authoritative campaign gates.
        notes = _notes(row)
        counters = HARD_COUNTERS if require_ecbeam2_diagnostics else PRODUCTION_HARD_COUNTERS
        for counter in counters:
            value = _parse_int(row.get(counter))
            if value is None:
                value = _note_int(notes, counter)
            if value is None:
                reasons.append(f"{cell}: missing {counter} diagnostic")
                continue
            if (value or 0) > 0:
                reasons.append(f"{cell}: {counter}={value}")
        if require_ecbeam2_diagnostics:
            committed_samples = _note_int(notes, "ecbeam2_committed_samples")
            if committed_samples is None or committed_samples <= 0:
                reasons.append(f"{cell}: invalid ecbeam2_committed_samples={committed_samples}")
            if require_active_survivor_diagnostics:
                min_survivors = _note_int(notes, "ecbeam2_min_survivors")
                if min_survivors is None or min_survivors <= 0:
                    reasons.append(f"{cell}: invalid ecbeam2_min_survivors={min_survivors}")
        for limiter_counter in ("limiter_limited_events", "limiter_limited_samples"):
            value = _parse_int(row.get(limiter_counter))
            if value is None:
                reasons.append(f"{cell}: missing {limiter_counter}")
            elif value > 0:
                reasons.append(f"{cell}: {limiter_counter}={value}")
        density = _parse_float(row.get("bit_density_max_deviation"))
        if density is None:
            reasons.append(f"{cell}: missing bit_density_max_deviation")
        elif density > 0.005:
            reasons.append(f"{cell}: bit_density_max_deviation={density:.9g}")
    return reasons


def evaluate_candidate(result: RunResult, baseline: RunResult) -> dict[str, Any]:
    failures = hard_failure_reasons(result)
    failures.extend(
        f"A1 baseline health: {reason}"
        for reason in hard_failure_reasons(
            baseline, require_active_survivor_diagnostics=False
        )
    )
    deltas: dict[str, list[float]] = {name: [] for name in PROTECTED_METRICS}
    candidate_values: dict[str, list[float]] = {name: [] for name in MATERIAL_THRESHOLDS}
    baseline_values: dict[str, list[float]] = {name: [] for name in MATERIAL_THRESHOLDS}
    protected_regressions: list[str] = []
    scores: list[float] = []
    runtime_ms = 0.0
    reconstruction_reductions: list[float] = []

    for cell in EXPECTED_CELLS:
        row = result.rows.get(cell)
        base = baseline.rows.get(cell)
        if row is None or base is None:
            continue
        for name in PROTECTED_METRICS:
            value = _metric(row, name)
            base_value = _metric(base, name)
            if value is None or base_value is None:
                if name in REQUIRED_PROTECTED_METRICS:
                    failures.append(f"{cell}: missing {name}")
                elif base_value is not None:
                    failures.append(f"{cell}: candidate missing protected {name}")
                continue
            delta = _better_delta(name, value, base_value)
            deltas[name].append(delta)
            if name in MATERIAL_THRESHOLDS:
                candidate_values[name].append(value)
                baseline_values[name].append(base_value)
            if delta < -PROTECTED_REGRESSION_DB:
                protected_regressions.append(f"{cell} {name} {delta:+.6g} dB")
        score = _metric(row, "quality_score")
        if score is None:
            failures.append(f"{cell}: missing constrained_quality_score")
        else:
            scores.append(score)
        render_ms = _metric(row, "render_ms")
        if render_ms is None:
            failures.append(f"{cell}: missing render_ms")
        else:
            runtime_ms += render_ms
        notes = _notes(row)
        base_notes = _notes(base)
        candidate_energy = _parse_float(notes.get("ecbeam2_committed_output_energy_mean"))
        baseline_energy = _parse_float(base_notes.get("ecbeam2_committed_output_energy_mean"))
        if candidate_energy is None or baseline_energy is None:
            failures.append(f"{cell}: missing committed reconstruction energy validation")
        else:
            reconstruction_reductions.append(baseline_energy - candidate_energy)

    failures.extend(f"protected regression: {item}" for item in protected_regressions)
    worst_cell_deltas = {
        name: min(values) if values else float("-inf") for name, values in deltas.items()
    }
    aggregate_gains = {
        name: _better_delta(
            name,
            _worst_aggregate(name, candidate_values[name]),
            _worst_aggregate(name, baseline_values[name]),
        )
        if candidate_values[name] and baseline_values[name]
        else float("-inf")
        for name in MATERIAL_THRESHOLDS
    }
    directionally_consistent_metrics = [
        name
        for name in MATERIAL_THRESHOLDS
        if deltas[name] and all(value >= 0.0 for value in deltas[name])
    ]
    material_wins = [
        name
        for name, threshold in MATERIAL_THRESHOLDS.items()
        if aggregate_gains[name] >= threshold and name in directionally_consistent_metrics
    ]
    if not material_wins:
        failures.append("no predeclared material gain")
    if not directionally_consistent_metrics:
        failures.append("no metric improves in every filter/wire-rate cell")

    eligible = not failures
    return {
        "candidate_id": result.spec.candidate_id,
        "candidate_label": result.spec.candidate.label,
        "modulator": result.spec.candidate.modulator,
        "eligible": eligible,
        "failures": failures,
        "material_wins": material_wins,
        "worst_cell_deltas": worst_cell_deltas,
        "aggregate_gains": aggregate_gains,
        "directionally_consistent_metrics": directionally_consistent_metrics,
        "worst_quality_score": min(scores) if scores else float("-inf"),
        "median_quality_score": statistics.median(scores) if scores else float("-inf"),
        "committed_reconstruction_energy_reduction": (
            min(reconstruction_reductions) if reconstruction_reductions else float("-inf")
        ),
        "runtime_ms": runtime_ms,
    }


def choose_winner(results: Sequence[RunResult]) -> tuple[dict[str, Any] | None, list[dict[str, Any]]]:
    baseline = next(
        (result for result in results if result.spec.candidate.label == "ecbeam-a1-production"),
        None,
    )
    if baseline is None:
        raise ValueError("selection results do not contain the A1 baseline")
    evaluations = [
        evaluate_candidate(result, baseline)
        for result in results
        if result.spec.candidate.modulator == ECBEAM2_MODULATOR
    ]
    admissible = [row for row in evaluations if row["eligible"]]
    admissible.sort(
        key=lambda row: (
            row["worst_quality_score"],
            row["median_quality_score"],
            row["committed_reconstruction_energy_reduction"],
            -row["runtime_ms"],
            row["candidate_id"],
        ),
        reverse=True,
    )
    return (admissible[0] if admissible else None), evaluations


def qualify_stability_candidates(
    results: Sequence[RunResult],
    *,
    retain_limit: int = 2,
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    if retain_limit < 1:
        raise ValueError("stability retain limit must be positive")
    baseline = next(
        (result for result in results if result.spec.candidate.label == "ecbeam-a1-production"),
        None,
    )
    if baseline is None:
        raise ValueError("stability results do not contain the A1 baseline")
    health_counters = QUALIFICATION_HEALTH_COUNTERS
    evaluations: list[dict[str, Any]] = []
    for result in results:
        if result.spec.candidate.modulator != ECBEAM2_MODULATOR:
            continue
        failures = list(result.errors)
        worst_energy_regression_db = float("-inf")
        worst_energy = float("-inf")
        for cell in EXPECTED_CELLS:
            row = result.rows.get(cell)
            base = baseline.rows.get(cell)
            if row is None or base is None:
                failures.append(f"{cell}: missing stability cell")
                continue
            notes = _notes(row)
            for counter in health_counters:
                value = _parse_int(row.get(counter))
                if value is None:
                    value = _note_int(notes, counter)
                if value is None or value != 0:
                    failures.append(f"{cell}: {counter}={value}")
            survivors = _note_int(notes, "ecbeam2_min_survivors")
            if survivors is None or survivors < 1:
                failures.append(f"{cell}: ecbeam2_min_survivors={survivors}")
            energy = _parse_float(notes.get("ecbeam2_committed_output_energy_mean"))
            base_energy = _parse_float(
                _notes(base).get("ecbeam2_committed_output_energy_mean")
            )
            if energy is None or base_energy is None or energy <= 0.0 or base_energy <= 0.0:
                failures.append(f"{cell}: missing positive committed reconstruction energy")
                continue
            regression_db = 10.0 * math.log10(energy / base_energy)
            worst_energy_regression_db = max(worst_energy_regression_db, regression_db)
            worst_energy = max(worst_energy, energy)
            if regression_db > 0.25 + 1.0e-12:
                failures.append(
                    f"{cell}: reconstruction energy regression {regression_db:.6g} dB"
                )
        evaluations.append(
            {
                "candidate_id": result.spec.candidate_id,
                "candidate_label": result.spec.candidate.label,
                "candidate": candidate_document(result.spec.candidate),
                "eligible": not failures,
                "failures": failures,
                "worst_cell_reconstruction_energy": worst_energy,
                "worst_cell_reconstruction_regression_db": worst_energy_regression_db,
            }
        )
    eligible = [evaluation for evaluation in evaluations if evaluation["eligible"]]
    eligible.sort(
        key=lambda row: (
            row["worst_cell_reconstruction_energy"],
            row["worst_cell_reconstruction_regression_db"],
            row["candidate_id"],
        )
    )
    return eligible[:retain_limit], evaluations


def _native_calibration_observations(
    result: RunResult,
) -> dict[int, list[dict[str, Any]]] | None:
    reports = []
    for command in result.commands:
        candidate_dir = Path(command[command.index("--out") + 1])
        report_path = candidate_dir / "ecbeam2_corpus_report.json"
        if report_path.is_file():
            reports.append((report_path, json.loads(report_path.read_text(encoding="utf-8"))))
    if not reports:
        return None
    if len(reports) != len(result.commands):
        raise ValueError("calibration has an incomplete set of native corpus reports")

    by_wire: dict[int, list[dict[str, Any]]] = {rate: [] for rate in WIRE_RATES.values()}
    diagnostic_fields = (
        "ultrasonic_ema_max",
        "signed_error_ema_abs_max",
        "ultrasonic_ema_p99_9",
        "ultrasonic_ema_p99_99",
        "signed_error_ema_abs_p99_9",
        "signed_error_ema_abs_p99_99",
    )
    for report_path, report in reports:
        for row in report.get("measurements", []):
            if not isinstance(row, Mapping) or not _row_matches_modulator(
                row, result.spec.candidate.modulator
            ):
                continue
            diagnostics = row.get("ecbeam2_diagnostics")
            if not isinstance(diagnostics, Mapping):
                raise ValueError(
                    f"calibration corpus row {row.get('case_id')} lacks EcBeam2 diagnostics"
                )
            parsed = {field: _parse_float(str(diagnostics.get(field))) for field in diagnostic_fields}
            missing = [field for field, value in parsed.items() if value is None or value < 0.0]
            if missing:
                raise ValueError(
                    f"calibration corpus row {row.get('case_id')} has invalid diagnostics {missing}"
                )
            wire_rate = int(row.get("wire_rate", 0))
            source_rate = int(row.get("source_rate", 0))
            if wire_rate not in by_wire or WIRE_RATES.get(source_rate) != wire_rate:
                raise ValueError(f"calibration corpus row has invalid wire rate in {report_path}")
            provenance = (
                f"{row.get('case_id')}|{row.get('fixture_id')}|{row.get('filter')}"
                f"@{source_rate}[{row.get('start_sample')}:{row.get('length_samples')}]"
            )
            by_wire[wire_rate].append({**parsed, "provenance": provenance})
    if any(not observations for observations in by_wire.values()):
        raise ValueError("calibration corpus diagnostics do not cover both DSD64 wire families")
    return by_wire


def _freeze_native_calibration_budgets(
    observations: Mapping[int, Sequence[Mapping[str, Any]]],
) -> FrozenBudgets:
    by_wire: dict[int, FrozenWireBudget] = {}
    digest_rows = []
    for wire_rate, rows in sorted(observations.items()):
        maxima = {
            field: max(
                ((float(row[field]), str(row["provenance"])) for row in rows),
                key=lambda item: (item[0], item[1]),
            )
            for field in (
                "ultrasonic_ema_max",
                "signed_error_ema_abs_max",
                "ultrasonic_ema_p99_9",
                "ultrasonic_ema_p99_99",
                "signed_error_ema_abs_p99_9",
                "signed_error_ema_abs_p99_99",
            )
        }
        by_wire[wire_rate] = FrozenWireBudget(
            maxima["ultrasonic_ema_max"][0],
            maxima["signed_error_ema_abs_max"][0],
            maxima["ultrasonic_ema_p99_9"][0],
            maxima["ultrasonic_ema_p99_99"][0],
            maxima["signed_error_ema_abs_p99_9"][0],
            maxima["signed_error_ema_abs_p99_99"][0],
            maxima["ultrasonic_ema_max"][1],
            maxima["signed_error_ema_abs_max"][1],
            maxima["ultrasonic_ema_p99_9"][1],
            maxima["ultrasonic_ema_p99_99"][1],
            maxima["signed_error_ema_abs_p99_9"][1],
            maxima["signed_error_ema_abs_p99_99"][1],
        )
        digest_rows.append(
            {
                "wire_rate": wire_rate,
                "observations": [dict(sorted(row.items())) for row in rows],
                "maxima": maxima,
            }
        )
    return FrozenBudgets(by_wire, _stable_hash({"calibration": digest_rows}, length=64))


def freeze_calibration_budgets(result: RunResult) -> FrozenBudgets:
    native_observations = _native_calibration_observations(result)
    if native_observations is not None:
        return _freeze_native_calibration_budgets(native_observations)
    by_wire: dict[int, FrozenWireBudget] = {}
    digest_rows: list[dict[str, Any]] = []
    for source_rate in SOURCE_RATES:
        wire_rate = WIRE_RATES[source_rate]
        ultrasonic: list[tuple[float, str]] = []
        signed: list[tuple[float, str]] = []
        ultrasonic_p999: list[float] = []
        ultrasonic_p9999: list[float] = []
        signed_p999: list[float] = []
        signed_p9999: list[float] = []
        for filter_name in FILTERS:
            row = result.rows.get((filter_name, source_rate))
            if row is None:
                raise ValueError(f"calibration is missing {(filter_name, source_rate)}")
            notes = _notes(row)
            u = _parse_float(notes.get("ecbeam2_ultrasonic_ema_max"))
            s = _parse_float(notes.get("ecbeam2_signed_error_ema_abs_max"))
            if u is None or s is None:
                raise ValueError(
                    f"calibration row {(filter_name, source_rate)} lacks EcBeam2 observer maxima"
                )
            percentile_keys = (
                "ecbeam2_ultrasonic_ema_p99_9",
                "ecbeam2_ultrasonic_ema_p99_99",
                "ecbeam2_signed_error_ema_abs_p99_9",
                "ecbeam2_signed_error_ema_abs_p99_99",
            )
            percentile_values = {
                key: _parse_float(notes.get(key)) for key in percentile_keys
            }
            missing_percentiles = [
                key for key, value in percentile_values.items() if value is None
            ]
            if missing_percentiles:
                raise ValueError(
                    f"calibration row {(filter_name, source_rate)} lacks percentiles "
                    f"{missing_percentiles}"
                )
            cell = f"{filter_name}@{source_rate}"
            ultrasonic.append((u, cell))
            signed.append((s, cell))
            ultrasonic_p999.append(percentile_values["ecbeam2_ultrasonic_ema_p99_9"])
            ultrasonic_p9999.append(percentile_values["ecbeam2_ultrasonic_ema_p99_99"])
            signed_p999.append(percentile_values["ecbeam2_signed_error_ema_abs_p99_9"])
            signed_p9999.append(percentile_values["ecbeam2_signed_error_ema_abs_p99_99"])
            digest_rows.append(
                {
                    "filter": filter_name,
                    "source_rate": source_rate,
                    "ultrasonic_ema_max": u,
                    "signed_error_ema_abs_max": s,
                    "ultrasonic_ema_p99_9": percentile_values[
                        "ecbeam2_ultrasonic_ema_p99_9"
                    ],
                    "ultrasonic_ema_p99_99": percentile_values[
                        "ecbeam2_ultrasonic_ema_p99_99"
                    ],
                    "signed_error_ema_abs_p99_9": percentile_values[
                        "ecbeam2_signed_error_ema_abs_p99_9"
                    ],
                    "signed_error_ema_abs_p99_99": percentile_values[
                        "ecbeam2_signed_error_ema_abs_p99_99"
                    ],
                }
            )
        ultrasonic_worst = max(ultrasonic)
        signed_worst = max(signed)
        by_wire[wire_rate] = FrozenWireBudget(
            ultrasonic_worst[0],
            signed_worst[0],
            max(ultrasonic_p999),
            max(ultrasonic_p9999),
            max(signed_p999),
            max(signed_p9999),
            ultrasonic_worst[1],
            signed_worst[1],
            ultrasonic_worst[1],
            ultrasonic_worst[1],
            signed_worst[1],
            signed_worst[1],
        )
        digest_rows.append(
            {
                "wire_rate": wire_rate,
                "ultrasonic_worst_cell": ultrasonic_worst[1],
                "signed_error_worst_cell": signed_worst[1],
            }
        )
    return FrozenBudgets(by_wire, _stable_hash({"calibration": digest_rows}, length=64))


def bitstream_digests(result: RunResult) -> dict[str, str]:
    digests: dict[str, str] = {}
    for command in result.commands:
        candidate_dir = Path(command[command.index("--out") + 1])
        index_path = candidate_dir / "dsd_bitstreams.csv"
        if not index_path.exists():
            raise ValueError(f"missing A1 parity bitstream index {index_path}")
        with index_path.open(newline="", encoding="utf-8") as handle:
            for row in csv.DictReader(handle):
                path = candidate_dir / row["file"]
                digest = hashlib.sha256(path.read_bytes()).hexdigest()
                key = "|".join(
                    (
                        row["filter"],
                        row["renderer_source_rate"],
                        row["dsd_rate"],
                        row["channel"],
                    )
                )
                if key in digests and digests[key] != digest:
                    raise ValueError(f"conflicting duplicate bitstream key {key}")
                digests[key] = digest
    return digests


def corpus_bitstream_digests(result: RunResult) -> dict[str, str]:
    digests: dict[str, str] = {}
    for command in result.commands:
        candidate_dir = Path(command[command.index("--out") + 1])
        report_path = candidate_dir / "ecbeam2_corpus_report.json"
        if not report_path.is_file():
            raise ValueError(f"missing native corpus bitstream report {report_path}")
        report = json.loads(report_path.read_text(encoding="utf-8"))
        measurements = report.get("measurements")
        if not isinstance(measurements, list) or not measurements:
            raise ValueError(f"native corpus report {report_path} has no measurements")
        for row in measurements:
            if not isinstance(row, Mapping) or not _row_matches_modulator(
                row, result.spec.candidate.modulator
            ):
                continue
            metric = row.get("metric")
            if not isinstance(metric, Mapping):
                raise ValueError(f"native corpus report {report_path} has no metric object")
            axes = (
                str(row.get("case_id", "")),
                str(row.get("fixture_id", "")),
                str(row.get("filter", "")),
                _canonical_report_modulator(
                    row.get("modulator"), result.spec.candidate.modulator
                ),
                str(row.get("source_rate", "")),
                str(row.get("wire_rate", "")),
            )
            if any(not axis for axis in axes):
                raise ValueError(f"native corpus report {report_path} has incomplete digest axes")
            for channel, field in (
                ("left", "native_left_sha256"),
                ("right", "native_right_sha256"),
            ):
                digest = metric.get(field)
                if (
                    not isinstance(digest, str)
                    or len(digest) != 64
                    or any(character not in "0123456789abcdef" for character in digest)
                ):
                    raise ValueError(
                        f"native corpus report {report_path} has invalid {field}"
                    )
                key = "|".join((*axes, channel))
                if key in digests and digests[key] != digest:
                    raise ValueError(f"conflicting duplicate corpus bitstream key {key}")
                digests[key] = digest
    if not digests:
        raise ValueError("native corpus reports emitted no matching bitstream digests")
    return digests


def observed_baseline_digests(result: RunResult) -> dict[str, str]:
    report_paths = [
        Path(command[command.index("--out") + 1]) / "ecbeam2_corpus_report.json"
        for command in result.commands
    ]
    if report_paths and all(path.is_file() for path in report_paths):
        return corpus_bitstream_digests(result)
    return bitstream_digests(result)


def verify_shadow_a1_parity(
    observer_off: RunResult, shadow_a1: RunResult
) -> dict[str, str]:
    observer_off_digests = observed_baseline_digests(observer_off)
    shadow_digests = observed_baseline_digests(shadow_a1)
    if observer_off_digests != shadow_digests:
        keys = sorted(set(observer_off_digests) | set(shadow_digests))
        mismatches = [
            key
            for key in keys
            if observer_off_digests.get(key) != shadow_digests.get(key)
        ]
        raise ValueError(f"ShadowA1 changed production A1 bitstreams: {mismatches}")
    if not observer_off_digests:
        raise ValueError("A1 parity run emitted no bitstream digests")
    return observer_off_digests


def verify_expected_baseline_digests(
    result: RunResult, corpus: Mapping[str, Any]
) -> dict[str, str]:
    expected = {
        str(key): str(value)
        for key, value in corpus.get("expected_baseline_digests", {}).items()
    }
    if not expected:
        raise ValueError(f"corpus {corpus.get('corpus_id')} has no frozen A1 digests")
    observed = corpus_bitstream_digests(result)
    if observed != expected:
        keys = sorted(set(observed) | set(expected))
        mismatches = [key for key in keys if observed.get(key) != expected.get(key)]
        raise ValueError(
            f"A1 baseline digests changed for corpus {corpus.get('corpus_id')}: {mismatches}"
        )
    return observed


def _json_safe(value: Any) -> Any:
    if isinstance(value, float) and not math.isfinite(value):
        return None
    if isinstance(value, Mapping):
        return {str(key): _json_safe(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_json_safe(item) for item in value]
    return value


def _write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(_canonical_json_bytes(value))


def _sha256_file(path: Path) -> str | None:
    return hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None


def artifact_provenance(binary: Path) -> dict[str, Any]:
    def output(command: Sequence[str]) -> str:
        completed = subprocess.run(
            command, cwd=ROOT, text=True, capture_output=True, check=False
        )
        return completed.stdout.strip() if completed.returncode == 0 else "unknown"

    rustc_verbose = output(("rustc", "-vV"))
    host = next(
        (line.split(":", 1)[1].strip() for line in rustc_verbose.splitlines() if line.startswith("host:")),
        "unknown",
    )
    return {
        "git_commit": output(("git", "rev-parse", "HEAD")),
        "working_tree_dirty": bool(output(("git", "status", "--porcelain"))),
        "binary_sha256": _sha256_file(binary),
        "candidate_config_sha256": None,
        "rustc_version": rustc_verbose.splitlines()[0] if rustc_verbose else "unknown",
        "target_triple": host,
        "target_cpu": "native",
    }


def candidate_configs_sha256(results: Sequence[RunResult]) -> str:
    digests: dict[str, str] = {}
    for result in results:
        for command in result.commands:
            if "--candidate-config" not in command:
                continue
            path = Path(command[command.index("--candidate-config") + 1])
            digest = _sha256_file(path)
            if digest is None:
                raise ValueError(f"missing candidate config {path}")
            digests[str(path)] = digest
    return _stable_hash(digests, length=64)


def with_provenance(
    document: Mapping[str, Any],
    provenance: Mapping[str, Any],
    *,
    candidate_config_sha256: str | None = None,
) -> dict[str, Any]:
    merged = dict(document)
    merged.update(provenance)
    merged["candidate_config_sha256"] = candidate_config_sha256
    return merged


def _canonical_json_bytes(value: Any) -> bytes:
    return (
        json.dumps(_json_safe(value), indent=2, sort_keys=True, allow_nan=False) + "\n"
    ).encode("utf-8")


def _safe_json_number(value: float) -> float | None:
    return value if math.isfinite(value) else None


def write_campaign_outputs(
    out_dir: Path,
    results: Sequence[RunResult],
    winner: Mapping[str, Any] | None,
    evaluations: Sequence[Mapping[str, Any]],
    corpus: Mapping[str, Any],
    provenance: Mapping[str, Any],
) -> None:
    config_digest = candidate_configs_sha256(results)
    by_id = {row["candidate_id"]: row for row in evaluations}
    records = []
    for result in sorted(results, key=lambda item: item.spec.index):
        evaluation = by_id.get(result.spec.candidate_id)
        records.append(
            {
                "schema_version": CAMPAIGN_SCHEMA_VERSION,
                "candidate_index": result.spec.index,
                "candidate_id": result.spec.candidate_id,
                "candidate_label": result.spec.candidate.label,
                "modulator": result.spec.candidate.modulator,
                "params": result.spec.candidate.canonical_params(),
                "status": result.status,
                "exit_code": result.exit_code,
                "elapsed_s": result.elapsed_s,
                "commands": result.commands,
                "errors": result.errors,
                "evaluation": evaluation,
            }
        )
    _write_json(
        out_dir / "campaign_summary.json",
        with_provenance(
            {
                "schema_version": CAMPAIGN_SCHEMA_VERSION,
                "corpus_id": corpus["corpus_id"],
                "winner": winner,
                "runs": records,
            },
            provenance,
            candidate_config_sha256=config_digest,
        ),
    )
    if winner is not None:
        winner = dict(winner)
        winning_result = next(
            result for result in results if result.spec.candidate_id == winner["candidate_id"]
        )
        winner["candidate"] = candidate_document(winning_result.spec.candidate)
        for key in (
            "worst_quality_score",
            "median_quality_score",
            "committed_reconstruction_energy_reduction",
        ):
            winner[key] = _safe_json_number(float(winner[key]))
        _write_json(
            out_dir / "winner.json",
            with_provenance(winner, provenance, candidate_config_sha256=config_digest),
        )


def _copy_manifest_metadata(
    out_dir: Path,
    corpus: Mapping[str, Any],
    *,
    corpus_manifest_sha256: str,
    frozen_budgets: FrozenBudgets | None = None,
    frozen_budget_file_sha256: str | None = None,
    oracle_candidate: Candidate | None = None,
) -> None:
    _write_json(out_dir / "resolved_corpus_manifest.json", corpus)
    _write_json(
        out_dir / "exact_oracle_request.json",
        oracle_request_document(
            corpus,
            corpus_manifest_sha256=corpus_manifest_sha256,
            frozen_budgets=frozen_budgets,
            frozen_budget_file_sha256=frozen_budget_file_sha256,
            candidate=oracle_candidate,
        ),
    )


def execute_specs(specs: Sequence[RunSpec], jobs: int, dry_run: bool) -> list[RunResult]:
    if dry_run or jobs <= 1:
        return [run_spec(spec, dry_run) for spec in specs]
    results: list[RunResult] = []
    with ThreadPoolExecutor(max_workers=jobs) as executor:
        futures = {executor.submit(run_spec, spec, False): spec for spec in specs}
        for future in as_completed(futures):
            results.append(future.result())
    return sorted(results, key=lambda result: result.spec.index)


def reproduction_command(command: Sequence[str]) -> str:
    prefix = "RUSTFLAGS='-C target-cpu=native' FOZMO_DSD_PREMOD_WINDOWS=1"
    return f"{prefix} {' '.join(shlex.quote(value) for value in command)}"


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--phase",
        choices=("calibration", "stability", "budget", "selection", "held-out"),
        default="selection",
    )
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--jobs", type=int, default=1)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument(
        "--allow-dirty-evidence",
        action="store_true",
        help="Permit exploratory real evidence from a dirty working tree.",
    )
    parser.add_argument(
        "--freeze-baseline-only",
        action="store_true",
        help="Run only corpus A1 and emit a reviewed digest fragment; never edit manifests.",
    )
    parser.add_argument("--budgets", type=Path, help="Frozen calibration budget JSON.")
    parser.add_argument("--winner", type=Path, help="Selection winner JSON for held-out replay.")
    parser.add_argument(
        "--scale-probe", type=Path, help="Frozen objective-scale probe for stability weights."
    )
    parser.add_argument(
        "--stability-stage",
        choices=("short", "full"),
        default="short",
        help="Run all candidates on the short corpus or only the frozen shortlist on calibration.",
    )
    parser.add_argument(
        "--freeze-scale-probe-only",
        action="store_true",
        help="Run inert knee probes and freeze measured objective distributions.",
    )
    parser.add_argument(
        "--oracle-candidate",
        type=Path,
        help="Prequalified candidate document used to bind exact-oracle v2.",
    )
    parser.add_argument(
        "--stability-candidates",
        type=Path,
        help="Stability qualification JSON containing one or two retained candidates.",
    )
    parser.add_argument(
        "--freeze-budget-from",
        type=Path,
        action="append",
        default=[],
        help="Combine calibration/held-out budget evidence and freeze the strictest pass.",
    )
    parser.add_argument("--corpus-manifest", type=Path)
    parser.add_argument(
        "--oracle-results",
        type=Path,
        help="Validate and freeze native exact-oracle results before selection.",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    if args.jobs < 1:
        raise SystemExit("--jobs must be at least 1")
    provenance = artifact_provenance(args.binary.resolve())
    if (
        not args.dry_run
        and args.phase in {"calibration", "selection"}
        and provenance["working_tree_dirty"]
        and not args.allow_dirty_evidence
    ):
        raise SystemExit(
            f"real {args.phase} evidence requires a clean tree; "
            "pass --allow-dirty-evidence for explicitly exploratory output"
        )
    role = (
        "held_out"
        if args.phase == "held-out"
        else None
        if args.phase == "budget"
        else (
            "stability_short"
            if args.freeze_scale_probe_only or args.stability_stage == "short"
            else "calibration"
        )
        if args.phase == "stability"
        else args.phase
    )
    corpus_path = args.corpus_manifest or MANIFEST_DIR / f"{role or 'calibration'}.json"
    corpus = load_corpus_manifest(
        corpus_path,
        role,
        require_expected_baseline_digests=(
            not args.dry_run
            and not args.freeze_baseline_only
            and role in {"selection", "held_out"}
        ),
    )
    corpus_manifest_sha256 = hashlib.sha256(corpus_path.read_bytes()).hexdigest()
    frozen_budgets: FrozenBudgets | None = None
    frozen_budget_file_sha256: str | None = None
    oracle_candidate: Candidate | None = None
    if args.oracle_candidate is not None:
        candidate_data = json.loads(args.oracle_candidate.read_text(encoding="utf-8"))
        if isinstance(candidate_data.get("candidate"), Mapping):
            candidate_data = candidate_data["candidate"]
        oracle_candidate = candidate_from_document(candidate_data)
    if args.phase in {"budget", "selection"} and not args.freeze_baseline_only:
        if args.budgets is None:
            raise SystemExit(f"{args.phase} requires --budgets from calibration")
        frozen_budgets = load_frozen_budgets(args.budgets)
        frozen_budget_file_sha256 = hashlib.sha256(args.budgets.read_bytes()).hexdigest()
        if not frozen_budgets.a1_bitstream_digests:
            raise SystemExit("selection budgets do not contain frozen A1 bitstream digests")
    if args.phase == "budget" and args.freeze_budget_from:
        assert frozen_budgets is not None
        selected, document = freeze_budget_qualification(
            args.freeze_budget_from, frozen_budgets
        )
        args.out.mkdir(parents=True, exist_ok=True)
        _write_json(
            args.out / "ecbeam2_qualified_budgets.json",
            with_provenance(
                document,
                provenance,
                candidate_config_sha256=_stable_hash(
                    candidate_document(selected), length=64
                ),
            ),
        )
        _write_json(
            args.out / "ecbeam2_budget_winner.json",
            with_provenance(
                {"candidate": candidate_document(selected)},
                provenance,
                candidate_config_sha256=_stable_hash(
                    candidate_document(selected), length=64
                ),
            ),
        )
        return 0
    args.out.mkdir(parents=True, exist_ok=True)
    _copy_manifest_metadata(
        args.out,
        corpus,
        corpus_manifest_sha256=corpus_manifest_sha256,
        frozen_budgets=frozen_budgets,
        frozen_budget_file_sha256=frozen_budget_file_sha256,
        oracle_candidate=oracle_candidate,
    )
    request = oracle_request_document(
        corpus,
        corpus_manifest_sha256=corpus_manifest_sha256,
        frozen_budgets=frozen_budgets,
        frozen_budget_file_sha256=frozen_budget_file_sha256,
        candidate=oracle_candidate,
    )
    if args.oracle_results is not None:
        oracle_bytes = args.oracle_results.read_bytes()
        results = json.loads(oracle_bytes.decode("utf-8"))
        validate_oracle_results(request, results)
        _write_json(
            args.out / "exact_oracle_summary.json",
            with_provenance(
                oracle_comparison_summary(
                    request,
                    results,
                    results_sha256=hashlib.sha256(oracle_bytes).hexdigest(),
                ),
                provenance,
                candidate_config_sha256=(
                    _stable_hash(candidate_document(oracle_candidate), length=64)
                    if oracle_candidate is not None
                    else None
                ),
            ),
        )
        print(f"validated {args.oracle_results}")

    if args.freeze_baseline_only:
        candidates = [a1_reference_candidate(role=role)]
    elif args.phase == "calibration":
        candidates = calibration_candidates()
    elif args.phase == "stability":
        if args.freeze_scale_probe_only:
            candidates = scale_probe_candidates()
        elif args.stability_stage == "full":
            if args.stability_candidates is None:
                raise SystemExit("full stability requires --stability-candidates shortlist")
            shortlist = json.loads(args.stability_candidates.read_text(encoding="utf-8"))
            retained_rows = shortlist.get("retained", [])
            if not isinstance(retained_rows, list) or not 1 <= len(retained_rows) <= 8:
                raise SystemExit("stability shortlist must retain between one and eight candidates")
            candidates = [
                a1_reference_candidate(role="stability"),
                *[candidate_from_document(row["candidate"]) for row in retained_rows],
            ]
        else:
            if args.scale_probe is None:
                raise SystemExit(
                    "stability qualification requires --scale-probe or --freeze-scale-probe-only"
                )
            candidates = stability_candidates(load_objective_scale_probe(args.scale_probe))
    elif args.phase == "budget":
        if args.stability_candidates is None:
            raise SystemExit("budget qualification requires --stability-candidates")
        qualification = json.loads(args.stability_candidates.read_text(encoding="utf-8"))
        retained_rows = qualification.get("retained", [])
        if not isinstance(retained_rows, list):
            raise SystemExit("stability qualification has no retained candidate list")
        retained = [candidate_from_document(row["candidate"]) for row in retained_rows]
        assert frozen_budgets is not None
        candidates = budget_qualification_candidates(retained, frozen_budgets)
    elif args.phase == "selection":
        if args.oracle_results is None and not args.dry_run:
            raise SystemExit(
                "selection requires --oracle-results with complete frozen N8/N12/N16 results"
            )
        if oracle_candidate is None:
            raise SystemExit(
                "selection requires --oracle-candidate from stability, budget, and oracle qualification"
            )
        assert frozen_budgets is not None
        candidates = qualified_selection_candidates(oracle_candidate)
    else:
        if args.winner is None:
            raise SystemExit("held-out requires --winner from the frozen selection run")
        winner_data = json.loads(args.winner.read_text(encoding="utf-8"))
        if not isinstance(winner_data.get("candidate"), Mapping):
            raise SystemExit("winner JSON does not contain a frozen candidate document")
        candidates = [
            a1_reference_candidate(role="held_out"),
            candidate_from_document(winner_data["candidate"]),
        ]

    specs = [
        RunSpec(
            candidate,
            index,
            args.out,
            args.binary.resolve(),
            source_rate,
            corpus_path.resolve(),
        )
        for index, candidate in enumerate(candidates)
        for source_rate in SOURCE_RATES
    ]
    results = merge_wire_results(execute_specs(specs, args.jobs, args.dry_run))
    for result in results:
        print(
            f"{result.spec.index:02d} {result.spec.candidate_id} "
            f"{result.spec.candidate.label} {result.status}"
        )

    if args.dry_run:
        _write_json(
            args.out / "dry_run.json",
            {
                "schema_version": CAMPAIGN_SCHEMA_VERSION,
                "corpus_id": corpus["corpus_id"],
                "execution_preconditions": {
                    "expected_baseline_digests_required": (
                        args.phase in {"selection", "held-out"}
                        and not args.freeze_baseline_only
                    ),
                    "exact_oracle_results_required": (
                        args.phase == "selection" and not args.freeze_baseline_only
                    ),
                    "baseline_freeze_only": args.freeze_baseline_only,
                },
                "commands": [
                    reproduction_command(command)
                    for result in results
                    for command in result.commands
                ],
            },
        )
        return 0

    if args.freeze_baseline_only:
        baseline = results[0]
        failures = hard_failure_reasons(
            baseline, require_active_survivor_diagnostics=False
        )
        if failures:
            raise SystemExit("A1 baseline freeze health failed: " + "; ".join(failures))
        digests = corpus_bitstream_digests(baseline)
        document = {
            "schema_version": "ecbeam2-baseline-digest-fragment-v1",
            "corpus_id": corpus["corpus_id"],
            "role": corpus["role"],
            "corpus_manifest_sha256": corpus_manifest_sha256,
            "candidate": candidate_document(baseline.spec.candidate),
            "expected_baseline_digests": dict(sorted(digests.items())),
        }
        _write_json(args.out / "expected_baseline_digests.fragment.json", document)
        if corpus["expected_baseline_digests"]:
            verify_expected_baseline_digests(baseline, corpus)
        print(
            "wrote reviewed baseline digest fragment to "
            f"{args.out / 'expected_baseline_digests.fragment.json'}"
        )
        return 0

    if args.phase == "calibration":
        observer_off = next(
            result for result in results if result.spec.candidate.label == "ecbeam-a1-observer-off"
        )
        shadow_a1 = next(
            result
            for result in results
            if result.spec.candidate.label == "ecbeam-a1-shadow-calibration"
        )
        calibration_failures = [
            *(
                f"observer-off {reason}"
                for reason in hard_failure_reasons(
                    observer_off, require_ecbeam2_diagnostics=False
                )
            ),
            *(
                f"ShadowA1 {reason}"
                for reason in hard_failure_reasons(
                    shadow_a1, require_active_survivor_diagnostics=False
                )
            ),
        ]
        if calibration_failures:
            raise SystemExit(
                "calibration A1 health failed: " + "; ".join(calibration_failures)
            )
        digests = verify_shadow_a1_parity(observer_off, shadow_a1)
        if corpus["expected_baseline_digests"]:
            verify_expected_baseline_digests(observer_off, corpus)
        _write_json(
            args.out / "a1_bitstream_digests.json",
            {
                "schema_version": CAMPAIGN_SCHEMA_VERSION,
                "observer_feature": "ecbeam2_observer",
                "digests": digests,
            },
        )
        observed_budgets = freeze_calibration_budgets(shadow_a1)
        budgets = FrozenBudgets(
            observed_budgets.by_wire_rate,
            observed_budgets.calibration_digest,
            digests,
        )
        _write_json(
            args.out / "ecbeam2_frozen_budgets.json",
            with_provenance(
                budgets.document(),
                provenance,
                candidate_config_sha256=candidate_configs_sha256(results),
            ),
        )
        return 0

    if args.phase == "stability":
        if args.freeze_scale_probe_only:
            probe = freeze_objective_scale_probe(results)
            _write_json(
                args.out / "ecbeam2_objective_scale_probe.json",
                with_provenance(
                    probe,
                    provenance,
                    candidate_config_sha256=candidate_configs_sha256(results),
                ),
            )
            return 0
        baseline = next(
            result for result in results if result.spec.candidate.label == "ecbeam-a1-production"
        )
        if corpus["expected_baseline_digests"]:
            verify_expected_baseline_digests(baseline, corpus)
        retain_limit = 8 if args.stability_stage == "short" else 2
        retained, evaluations = qualify_stability_candidates(
            results, retain_limit=retain_limit
        )
        output_name = (
            "ecbeam2_stability_shortlist.json"
            if args.stability_stage == "short"
            else "ecbeam2_stability_qualification.json"
        )
        _write_json(
            args.out / output_name,
            with_provenance(
                {
                    "schema_version": "ecbeam2-stability-qualification-v1",
                    "scale_probe_digest": load_objective_scale_probe(args.scale_probe)[
                        "scale_probe_digest"
                    ],
                    "stability_stage": args.stability_stage,
                    "retention_limit": retain_limit,
                    "retained": retained,
                    "evaluations": evaluations,
                },
                provenance,
                candidate_config_sha256=candidate_configs_sha256(results),
            ),
        )
        if not retained:
            print(
                "no state-control candidate achieved zero repairs; stop fixed-CRFB tuning",
                file=sys.stderr,
            )
            return 2
        print(
            f"retained {args.stability_stage} stability candidates: "
            + ", ".join(row["candidate_label"] for row in retained)
        )
        return 0

    if args.phase == "budget":
        evaluations = []
        for result in results:
            if result.spec.candidate.modulator != ECBEAM2_MODULATOR:
                continue
            escapes = 0
            failures = list(result.errors)
            for cell in EXPECTED_CELLS:
                row = result.rows.get(cell)
                if row is None:
                    failures.append(f"{cell}: missing")
                    continue
                notes = _notes(row)
                value = _note_int(notes, "ecbeam2_constraint_escape")
                if value is None:
                    failures.append(f"{cell}: missing constraint escape counter")
                else:
                    escapes += value
                for counter in QUALIFICATION_HEALTH_COUNTERS:
                    counter_value = _parse_int(row.get(counter))
                    if counter_value is None:
                        counter_value = _note_int(notes, counter)
                    if counter_value is None or counter_value != 0:
                        failures.append(f"{cell}: {counter}={counter_value}")
            evaluations.append(
                {
                    "candidate_id": result.spec.candidate_id,
                    "candidate_label": result.spec.candidate.label,
                    "candidate": candidate_document(result.spec.candidate),
                    "budget_mode": result.spec.candidate.budget_mode,
                    "ultrasonic_allowance_db": result.spec.candidate.ultrasonic_allowance_db,
                    "signed_error_multiplier": result.spec.candidate.signed_error_multiplier,
                    "constraint_escapes": escapes,
                    "health_failures": failures,
                    "corpus_id": corpus["corpus_id"],
                    "corpus_role": corpus["role"],
                }
            )
        _write_json(
            args.out / "ecbeam2_budget_qualification.json",
            with_provenance(
                {
                    "schema_version": "ecbeam2-budget-qualification-v1",
                    "calibration_digest": frozen_budgets.calibration_digest,
                    "evaluations": evaluations,
                },
                provenance,
                candidate_config_sha256=candidate_configs_sha256(results),
            ),
        )
        return 0

    baseline = next(
        result for result in results if result.spec.candidate.label == "ecbeam-a1-production"
    )
    verify_expected_baseline_digests(baseline, corpus)
    winner, evaluations = choose_winner(results)
    write_campaign_outputs(args.out, results, winner, evaluations, corpus, provenance)
    if winner is None:
        print("no EcBeam2 candidate passed the predeclared winner rule", file=sys.stderr)
        return 2
    print(f"winner: {winner['candidate_label']} ({winner['candidate_id']})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
