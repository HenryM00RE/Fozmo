#!/usr/bin/env python3
"""Shared DSD candidate JSON, canonicalization, and tier guards."""

from __future__ import annotations

import hashlib
import json
import math
from pathlib import Path
from typing import Any, Iterable, Mapping


CANDIDATE_SCHEMA_VERSION = "dsd-candidate-schema-v1"
DEFAULT_TAPER_START = 0.60
KNOWN_BAD_SCHEMA_VERSION = "dsd-known-bad-v1"


def clean_float(value: float) -> float:
    return float(f"{value:.12g}")


def canonical_candidate_params(params: Mapping[str, Any]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for key, value in params.items():
        if value is None:
            continue
        if isinstance(value, float):
            out[key] = clean_float(value)
        else:
            out[key] = value

    taper_strength = float(out.get("ec2_pressure_taper_strength", 0.0))
    if taper_strength == 0.0:
        out["ec2_pressure_taper_start"] = DEFAULT_TAPER_START

    # Collapse policy-inert knobs so stable IDs respect bitstream identity:
    # taper only applies under pressure-taper/combined, the ambiguity margin
    # only under ambiguity-pressure/combined (mirrors Ec2LongFilterPolicy).
    policy = out.get("ec2_policy")
    if policy in ("off", "ambiguity-pressure"):
        out["ec2_pressure_taper_start"] = DEFAULT_TAPER_START
        out["ec2_pressure_taper_strength"] = 0.0
    if policy in ("off", "pressure-taper"):
        out["ec2_ambiguity_margin"] = 0.0

    # Stage weights are scale-invariant (the modulator normalizes to sum 1) and
    # a uniform profile is bit-identical to no profile: canonicalize to mean 1.0
    # and drop uniform sets so stable IDs respect those equivalences.
    for weights_key in ("ec2_pressure_stage_weights", "beam_pressure_stage_weights"):
        weights = out.get(weights_key)
        if isinstance(weights, (list, tuple)) and len(weights) == 7:
            try:
                floats = [float(w) for w in weights]
            except (TypeError, ValueError):
                floats = None
            if (
                floats is not None
                and all(math.isfinite(w) and w >= 0.0 for w in floats)
                and sum(floats) > 0.0
            ):
                total = sum(floats)
                normalized = [clean_float(w * 7.0 / total) for w in floats]
                if all(abs(w - 1.0) <= 1.0e-9 for w in normalized):
                    del out[weights_key]
                else:
                    out[weights_key] = normalized

    if float(out.get("beam_terminal_weight", 0.0)) == 0.0:
        out.pop("beam_terminal_weight", None)
    if float(out.get("beam_alternation_weight", 0.0)) == 0.0:
        out.pop("beam_alternation_weight", None)
    if float(out.get("beam_alternation_rank_weight", 0.0)) == 0.0:
        out.pop("beam_alternation_rank_weight", None)
    if float(out.get("beam_filtered_error_weight", 0.0)) == 0.0:
        out.pop("beam_filtered_error_weight", None)
    if float(out.get("beam_filtered_error_rank_weight", 0.0)) == 0.0:
        out.pop("beam_filtered_error_rank_weight", None)
    if float(out.get("beam_reconstruction_error_weight", 0.0)) == 0.0:
        out.pop("beam_reconstruction_error_weight", None)
    if float(out.get("beam_pressure_deadzone", 0.0)) == 0.0:
        out.pop("beam_pressure_deadzone", None)
    if float(out.get("beam_periodicity_weight", 0.0)) == 0.0:
        out.pop("beam_periodicity_weight", None)
        out.pop("beam_periodicity_lags", None)
        out.pop("beam_periodicity_window", None)
    else:
        lags = out.get("beam_periodicity_lags")
        if isinstance(lags, (list, tuple)):
            out["beam_periodicity_lags"] = sorted(int(lag) for lag in lags)
        if "beam_periodicity_window" in out:
            out["beam_periodicity_window"] = int(out["beam_periodicity_window"])
    if (
        float(out.get("beam_alternation_threshold", 0.0)) == 0.0
        or (
            "beam_alternation_weight" not in out
            and "beam_alternation_rank_weight" not in out
        )
    ):
        out.pop("beam_alternation_threshold", None)

    # EcBeam2 is an isolated experiment namespace. The barrier knee is retained
    # at zero weight because it changes the reported raw barrier distribution
    # used by the objective scale probe even though it cannot change the bits.
    # Other zero-valued ablations and disabled budgets remain equivalent to
    # absence; none of this touches the historic `beam_*` proxy controls above.
    if float(out.get("ecbeam2_state_deadzone_weight", 0.0)) == 0.0:
        out.pop("ecbeam2_state_deadzone_weight", None)
    if float(out.get("ecbeam2_quantizer_regularizer", 0.0)) == 0.0:
        out.pop("ecbeam2_quantizer_regularizer", None)
    for budget_key in ("ecbeam2_ultrasonic_budget", "ecbeam2_signed_error_budget"):
        if float(out.get(budget_key, 0.0)) <= 0.0:
            out.pop(budget_key, None)

    # Gated dither needs both margin and scale positive to have any effect;
    # anything else is bit-identical to absent.
    if "ec_gated_dither_margin" in out or "ec_gated_dither_scale" in out:
        margin = float(out.get("ec_gated_dither_margin", 0.0))
        scale = float(out.get("ec_gated_dither_scale", 0.0))
        if margin <= 0.0 or scale <= 0.0:
            out.pop("ec_gated_dither_margin", None)
            out.pop("ec_gated_dither_scale", None)

    return out


def stable_candidate_id(params: Mapping[str, Any]) -> str:
    payload = json.dumps(canonical_candidate_params(params), sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()[:12]


def candidate_document(label: str, params: Mapping[str, Any], *, baseline: bool = False) -> dict[str, Any]:
    canonical = canonical_candidate_params(params)
    return {
        "candidate_schema_version": CANDIDATE_SCHEMA_VERSION,
        "candidate_id": stable_candidate_id(canonical),
        "candidate_label": label,
        "baseline": baseline,
        "params": canonical,
    }


def write_candidate_config(path: Path, label: str, params: Mapping[str, Any], *, baseline: bool = False) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(candidate_document(label, params, baseline=baseline), indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


def load_known_bad_ids(path: Path) -> set[str]:
    out: set[str] = set()
    if not path.exists():
        return out
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            if not line.strip():
                continue
            record = json.loads(line)
            candidate_id = record.get("candidate_id")
            if isinstance(candidate_id, str) and candidate_id:
                out.add(candidate_id)
    return out


def known_bad_record(
    *,
    candidate_id: str,
    label: str,
    stage: str,
    params: Mapping[str, Any],
    reasons: Iterable[str],
    candidate_dir: Path,
) -> dict[str, Any]:
    return {
        "known_bad_schema_version": KNOWN_BAD_SCHEMA_VERSION,
        "candidate_id": candidate_id,
        "candidate_label": label,
        "stage": stage,
        "params": canonical_candidate_params(params),
        "reasons": list(reasons),
        "candidate_dir": str(candidate_dir),
    }


def append_known_bad(path: Path, record: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(dict(record), sort_keys=True) + "\n")


def validate_candidate_tiers(params: Mapping[str, Any], *, allow_exploratory: bool = False, baseline: bool = False) -> None:
    if baseline:
        return
    canonical = canonical_candidate_params(params)
    _tier_range(canonical, "dither_scale", 0.0, 0.25, 0.30, allow_exploratory)
    _tier_range(canonical, "beam_dither_scale", 0.0, 0.25, 0.50, allow_exploratory)
    _tier_min(canonical, "leak_alpha", 0.99, 0.98, allow_exploratory)
    _tier_range(canonical, "lf_floor_gamma", 0.0, 0.03, 0.05, allow_exploratory)
    _tier_range(canonical, "ec_dc_bias_corner_hz", 0.0, 250.0, 2000.0, allow_exploratory)
    _tier_inner(canonical, "ec2_quantizer_weight", 0.60, 1.00, 0.50, 1.00, allow_exploratory)
    _tier_inner(canonical, "ec2_pressure_weight", 0.75, 5.0, 0.375, 7.5, allow_exploratory)
    _tier_inner(canonical, "beam_quantizer_weight", 0.60, 1.00, 0.50, 4.0, allow_exploratory)
    _tier_inner(canonical, "beam_pressure_weight", 0.75, 5.0, 0.375, 7.5, allow_exploratory)
    if "ec2_limit_weight" in canonical and abs(float(canonical["ec2_limit_weight"]) - 80.0) > 1.0e-9:
        raise ValueError("ec2_limit_weight is pinned at 80.0")
    _tier_range(canonical, "beam_limit_weight", 0.0, 80.0, 320.0, allow_exploratory)
    _tier_range(canonical, "ec2_transition_weight", 0.0, 0.006, 0.010, allow_exploratory)
    _tier_range(canonical, "beam_transition_weight", 0.0, 0.006, 0.010, allow_exploratory)
    _tier_range(canonical, "ec2_dc_weight", 0.0, 0.10, 0.10, allow_exploratory)
    _tier_range(canonical, "beam_dc_weight", 0.0, 0.10, 0.20, allow_exploratory)
    _tier_inner(canonical, "ec2_lookahead_discount", 0.4, 0.8, 0.3, 0.9, allow_exploratory)
    _tier_range(canonical, "ec2_ambiguity_margin", 0.0, 0.01, 0.02, allow_exploratory)
    _strict_range(canonical, "ec2_pressure_taper_start", 0.45, 0.72)
    _tier_range(canonical, "ec2_pressure_taper_strength", 0.0, 2.0, 2.0, allow_exploratory)
    if canonical.get("dither_prng") not in (None, "splitmix64", "splitmix") and not allow_exploratory:
        raise ValueError("dither_prng is exploratory; pass --allow-exploratory")
    _stage_weights_tier(canonical, allow_exploratory)
    _beam_stage_weights_tier(canonical, allow_exploratory)
    _tier_range(canonical, "beam_terminal_weight", 0.0, 1.0, 1.0, allow_exploratory)
    _tier_range(canonical, "beam_alternation_weight", 0.0, 0.05, 0.05, allow_exploratory)
    _tier_range(canonical, "beam_alternation_rank_weight", 0.0, 0.05, 0.05, allow_exploratory)
    _strict_range(canonical, "beam_alternation_threshold", 0.0, 1.0)
    _tier_range(canonical, "beam_filtered_error_weight", 0.0, 1.0, 4.0, allow_exploratory)
    _tier_range(canonical, "beam_filtered_error_rank_weight", 0.0, 1.0, 4.0, allow_exploratory)
    _require_beam_candidate(canonical, "beam_reconstruction_error_weight")
    _tier_range(canonical, "beam_reconstruction_error_weight", 0.0, 0.0, 1000.0, allow_exploratory)
    _require_beam_candidate(canonical, "beam_pressure_deadzone")
    _tier_range(canonical, "beam_pressure_deadzone", 0.0, 0.0, 1.0, allow_exploratory)
    _require_beam_candidate(canonical, "beam_periodicity_weight")
    _tier_range(canonical, "beam_periodicity_weight", 0.0, 0.0, 0.05, allow_exploratory)
    _periodicity_lags_tier(canonical, allow_exploratory)
    _require_beam_candidate(canonical, "beam_periodicity_window")
    _tier_range(canonical, "beam_periodicity_window", 2.0, 2.0, 48.0, allow_exploratory)
    _tier_range(canonical, "ec_gated_dither_margin", 0.0, 0.0, 0.25, allow_exploratory)
    _tier_range(canonical, "ec_gated_dither_scale", 0.0, 0.0, 0.50, allow_exploratory)
    _validate_ecbeam2_candidate(canonical, allow_exploratory)


def _validate_ecbeam2_candidate(params: Mapping[str, Any], allow_exploratory: bool) -> None:
    keys = {key for key in params if key.startswith("ecbeam2_")}
    if not keys:
        return
    if not allow_exploratory:
        raise ValueError("EcBeam2 controls are exploratory; pass --allow-exploratory")
    run_mode = params.get("ecbeam2_run_mode")
    if run_mode not in (None, "active", "shadow-a1", "shadow_a1"):
        raise ValueError("ecbeam2_run_mode must be active or shadow-a1")
    profile = params.get("ecbeam2_profile")
    if profile not in (
        None,
        "harness24to32-v1",
        "harness24_to_32_v1",
        "Harness24To32V1",
    ):
        raise ValueError("ecbeam2_profile must be harness24to32-v1")
    _strict_range(params, "ecbeam2_state_terminal_weight", 0.0, 1.0e6)
    _strict_range(params, "ecbeam2_state_deadzone", 0.0, 1.0)
    _strict_range(params, "ecbeam2_state_deadzone_weight", 0.0, 1.0e6)
    _strict_range(params, "ecbeam2_quantizer_regularizer", 0.0, 1.0e6)
    _strict_range(params, "ecbeam2_ultrasonic_budget", 0.0, 16.0)
    _strict_range(params, "ecbeam2_signed_error_budget", 0.0, 2.0)


def _stage_weights_tier(params: Mapping[str, Any], allow_exploratory: bool) -> None:
    if "ec2_pressure_stage_weights" not in params:
        return
    weights = params["ec2_pressure_stage_weights"]
    if not isinstance(weights, (list, tuple)) or len(weights) != 7:
        raise ValueError("ec2_pressure_stage_weights requires exactly 7 weights")
    values = [float(w) for w in weights]
    if any(not math.isfinite(v) or not (0.1 <= v <= 4.0) for v in values):
        raise ValueError("ec2_pressure_stage_weights is outside the allowed search space")
    if not allow_exploratory:
        raise ValueError("ec2_pressure_stage_weights is exploratory; pass --allow-exploratory")


def _beam_stage_weights_tier(params: Mapping[str, Any], allow_exploratory: bool) -> None:
    if "beam_pressure_stage_weights" not in params:
        return
    weights = params["beam_pressure_stage_weights"]
    if not isinstance(weights, (list, tuple)) or len(weights) != 7:
        raise ValueError("beam_pressure_stage_weights requires exactly 7 weights")
    values = [float(w) for w in weights]
    if any(not math.isfinite(v) or not (0.1 <= v <= 4.0) for v in values):
        raise ValueError("beam_pressure_stage_weights is outside the allowed search space")
    if not allow_exploratory:
        raise ValueError("beam_pressure_stage_weights is exploratory; pass --allow-exploratory")


def _periodicity_lags_tier(params: Mapping[str, Any], allow_exploratory: bool) -> None:
    if "beam_periodicity_lags" not in params:
        return
    _require_beam_candidate(params, "beam_periodicity_lags")
    if not allow_exploratory:
        raise ValueError("beam_periodicity_lags is exploratory; pass --allow-exploratory")
    lags = params["beam_periodicity_lags"]
    if not isinstance(lags, (list, tuple)) or not (1 <= len(lags) <= 4):
        raise ValueError("beam_periodicity_lags requires 1 to 4 lags")
    values = [int(lag) for lag in lags]
    if len(set(values)) != len(values) or any(not (1 <= lag <= 47) for lag in values):
        raise ValueError("beam_periodicity_lags must be unique integers in 1..=47")


def _require_beam_candidate(params: Mapping[str, Any], key: str) -> None:
    if key in params and ("ec_beam_m" not in params or "ec_beam_n" not in params):
        raise ValueError(f"{key} requires ec_beam_m/ec_beam_n")


def _tier_range(
    params: Mapping[str, Any],
    key: str,
    core_min: float,
    core_max: float,
    exploratory_max: float,
    allow_exploratory: bool,
) -> None:
    if key not in params:
        return
    value = float(params[key])
    if value < core_min or value > exploratory_max:
        raise ValueError(f"{key} is outside the allowed search space")
    if value > core_max and not allow_exploratory:
        raise ValueError(f"{key} is exploratory; pass --allow-exploratory")


def _tier_min(
    params: Mapping[str, Any],
    key: str,
    core_min: float,
    exploratory_min: float,
    allow_exploratory: bool,
) -> None:
    if key not in params:
        return
    value = float(params[key])
    if value < exploratory_min:
        raise ValueError(f"{key} is outside the allowed search space")
    if value < core_min and not allow_exploratory:
        raise ValueError(f"{key} is exploratory; pass --allow-exploratory")


def _tier_inner(
    params: Mapping[str, Any],
    key: str,
    core_min: float,
    core_max: float,
    hard_min: float,
    hard_max: float,
    allow_exploratory: bool,
) -> None:
    if key not in params:
        return
    value = float(params[key])
    if value < hard_min or value > hard_max:
        raise ValueError(f"{key} is outside the allowed search space")
    if not (core_min <= value <= core_max) and not allow_exploratory:
        raise ValueError(f"{key} is exploratory; pass --allow-exploratory")


def _strict_range(params: Mapping[str, Any], key: str, minimum: float, maximum: float) -> None:
    if key not in params:
        return
    value = float(params[key])
    if value < minimum or value > maximum:
        raise ValueError(f"{key} is outside the allowed search space")
