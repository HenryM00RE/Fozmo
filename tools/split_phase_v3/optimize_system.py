from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import sys
from pathlib import Path
from typing import Any

import cvxpy
import jax
import jaxlib
import numpy as np
import scipy
import tomli

from .assets import design_cleanup_assets, design_rational_table, export_assets
from .certify import certify_character
from .capture_v2 import capture_v2_baseline
from .finite_support import (
    initialize_finite_support,
    optimize_character,
    polish_cleanup_filters,
)
from .group_delay import design_group_delay
from .magnitude_sdp import MagnitudeSpec, solve_magnitude_sdp
from .multirate_model import verify_multirate_model
from .spectral_factor import spectral_factor_from_autocorrelation


def _load_config(path: Path) -> dict[str, Any]:
    with path.open("rb") as handle:
        return tomli.load(handle)


class RunLog:
    def __init__(self, path: Path):
        path.parent.mkdir(parents=True, exist_ok=True)
        self.path = path

    def write(self, event: str, **details: Any) -> None:
        record = {
            "time": dt.datetime.now(dt.timezone.utc).isoformat(),
            "event": event,
            **details,
        }
        with self.path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(record, sort_keys=True) + "\n")
            handle.flush()
            os.fsync(handle.fileno())
        print(json.dumps(record, sort_keys=True), flush=True)


def _parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Generate frozen Split Phase V3 assets")
    parser.add_argument(
        "--config", type=Path, default=Path("tools/split_phase_v3/production.toml")
    )
    parser.add_argument("--sdp-solver", default="auto")
    parser.add_argument("--resume", action="store_true")
    parser.add_argument("--verbose-solver", action="store_true")
    parser.add_argument(
        "--stop-after",
        choices=("magnitude", "spectral", "group-delay", "support", "character", "assets"),
    )
    return parser.parse_args()


def main() -> int:
    arguments = _parse_arguments()
    config = _load_config(arguments.config)
    repo_root = Path(__file__).resolve().parents[2]
    work_dir = repo_root / "tools/split_phase_v3/work"
    asset_dir = repo_root / "assets/filters/split_phase_v3"
    work_dir.mkdir(parents=True, exist_ok=True)
    log = RunLog(work_dir / "run.jsonl")
    log.write(
        "run_started",
        config=str(arguments.config),
        resume=arguments.resume,
        versions={
            "python": sys.version,
            "numpy": np.__version__,
            "scipy": scipy.__version__,
            "cvxpy": cvxpy.__version__,
            "jax": jax.__version__,
            "jaxlib": jaxlib.__version__,
        },
    )
    model_report = verify_multirate_model(int(config["identity"]["seed"]), work_dir)
    log.write(
        "multirate_model_verified",
        interpolation_relative_error=model_report["worst_interpolation_relative_error"],
        decimation_relative_error=model_report["worst_decimation_magnitude_relative_error"],
    )
    baseline_report = capture_v2_baseline(
        int(config["certification"]["fft_len"]), work_dir, resume=arguments.resume
    )
    log.write("v2_baseline_captured", **baseline_report["metrics"])

    reference = config["reference"]
    magnitude_config = config["magnitude_sdp"]
    order = int(magnitude_config["initial_order"])
    while True:
        spec = MagnitudeSpec(
            order=order,
            sample_rate_hz=2.0 * float(reference["source_rate_hz"]),
            pass_edge_hz=float(reference["pass_edge_hz"]),
            stop_edge_hz=float(reference["stop_edge_hz"]),
            verification_fft_len=int(magnitude_config["verification_fft_len"]),
            maximum_exchange_rounds=int(magnitude_config["maximum_exchange_rounds"]),
        )
        log.write("magnitude_sdp_started", order=order)
        try:
            autocorrelation, magnitude_report = solve_magnitude_sdp(
                spec,
                work_dir,
                solver=arguments.sdp_solver,
                resume=arguments.resume,
                verbose=arguments.verbose_solver,
            )
            break
        except RuntimeError as error:
            if order >= int(magnitude_config["maximum_order"]):
                log.write("magnitude_sdp_failed", order=order, error=str(error))
                raise
            log.write("magnitude_sdp_escalating", order=order, error=str(error))
            order = int(magnitude_config["maximum_order"])
    log.write(
        "magnitude_sdp_complete",
        order=order,
        verification=magnitude_report["history"][-1]["verification"],
    )
    if arguments.stop_after == "magnitude":
        return 0

    design_fft_len = int(config["certification"]["fft_len"])
    log.write("spectral_factor_started", fft_len=design_fft_len)
    magnitude, minimum_spectrum, spectral_report = spectral_factor_from_autocorrelation(
        autocorrelation, design_fft_len, work_dir, resume=arguments.resume
    )
    log.write("spectral_factor_complete", **spectral_report)
    if arguments.stop_after == "spectral":
        return 0

    group = config["group_delay"]
    identity = config["identity"]
    log.write("group_delay_started")
    residual_phase, target_delay, group_report = design_group_delay(
        minimum_spectrum,
        magnitude,
        design_fft_len,
        2.0 * float(reference["source_rate_hz"]),
        float(reference["split_lo_hz"]),
        float(reference["split_hi_hz"]),
        int(group["spline_degree"]),
        int(group["control_values"]),
        int(group["starts"]),
        int(identity["seed"]),
        work_dir,
        resume=arguments.resume,
    )
    log.write("group_delay_complete", join_error_rad=group_report["join_error_rad"])
    if arguments.stop_after == "group-delay":
        return 0

    support_config = config["support"]
    branch_taps = int(support_config["character_branch_taps"])
    support = 2 * branch_taps - 1
    log.write("support_search_started", support=support)
    initial, bulk_delay, support_report = initialize_finite_support(
        magnitude,
        residual_phase,
        design_fft_len,
        support,
        int(support_config["v2_bulk_shift"]),
        int(support_config["edge_measure_samples"]),
        work_dir,
        resume=arguments.resume,
    )
    log.write(
        "support_search_complete",
        bulk_delay=bulk_delay,
        **support_report["selected_metrics"],
    )
    if arguments.stop_after == "support":
        return 0

    log.write("cleanup_optimization_started")
    cleanups, cleanup_reports = design_cleanup_assets(
        [int(value) for value in support_config["cleanup_branch_taps"]]
    )
    log.write("cleanup_optimization_complete", stages=cleanup_reports)
    optimization = config["optimization"]
    log.write("character_optimization_started")
    character, character_report = optimize_character(
        initial,
        magnitude,
        residual_phase,
        design_fft_len,
        int(optimization["working_fft_len"]),
        bulk_delay,
        2.0 * float(reference["source_rate_hz"]),
        float(reference["pass_edge_hz"]),
        float(reference["stop_edge_hz"]),
        [int(value) for value in optimization["p_continuation"]],
        int(optimization["adam_steps_per_p"]),
        int(optimization["lbfgs_steps"]),
        cleanups,
        work_dir,
        resume=arguments.resume,
    )
    log.write("character_optimization_complete", **character_report)
    if arguments.stop_after == "character":
        return 0

    log.write("block_coordinate_polish_started")
    cleanups, cleanup_polish_report = polish_cleanup_filters(
        character,
        cleanups,
        magnitude,
        residual_phase,
        design_fft_len,
        bulk_delay,
        2.0 * float(reference["source_rate_hz"]),
        float(reference["pass_edge_hz"]),
        float(reference["stop_edge_hz"]),
        int(optimization["block_coordinate_cycles"]),
        work_dir,
        resume=arguments.resume,
    )
    log.write("block_coordinate_polish_complete", **cleanup_polish_report)

    log.write("rational_optimization_started")
    rational_147_160, rational_up_report = design_rational_table(
        160,
        512,
        magnitude,
        residual_phase,
        design_fft_len,
        44100,
        48000,
    )
    rational_160_147, rational_down_report = design_rational_table(
        147,
        1024,
        magnitude,
        residual_phase,
        design_fft_len,
        48000,
        44100,
    )
    log.write(
        "rational_optimization_complete",
        rational_147_160=rational_up_report,
        rational_160_147=rational_down_report,
    )
    for name, rational_report in (
        ("147/160", rational_up_report),
        ("160/147", rational_down_report),
    ):
        if (
            rational_report["passband_ripple_db"] > 0.0002
            or rational_report["stopband_peak_db"] > -130.0
            or rational_report["maximum_row_sum_error"] > 2.0e-15
        ):
            raise RuntimeError(f"rational V3 table {name} failed certification: {rational_report}")

    certification = certify_character(
        character,
        magnitude,
        residual_phase,
        target_delay,
        bulk_delay,
        design_fft_len,
        2.0 * float(reference["source_rate_hz"]),
        float(reference["pass_edge_hz"]),
        float(reference["stop_edge_hz"]),
        float(reference["split_lo_hz"]),
        float(reference["split_hi_hz"]),
        int(support_config["edge_measure_samples"]),
        cleanups,
        work_dir,
    )
    v2_metrics = baseline_report["metrics"]
    pareto_improvements = {
        "group_delay_curvature": bool(
            certification["maximum_group_delay_curvature"]
            <= 0.8 * v2_metrics["maximum_group_delay_curvature"]
        ),
        "pre_peak_energy": bool(
            certification["broadband_pre_peak_energy_db"]
            <= v2_metrics["broadband_pre_peak_energy_db"]
            + 10.0 * np.log10(0.8)
        ),
        "edge_energy": bool(
            certification["edge_energy_db"]
            <= v2_metrics["edge_energy_db"] + 10.0 * np.log10(0.8)
        ),
        "stopband": bool(
            certification["character_stopband_peak_db"]
            <= v2_metrics["stopband_peak_db"] + 20.0 * np.log10(0.8)
        ),
        "complex_passband_approximation": bool(
            certification["worst_composite_passband_complex_error"]
            <= 0.8 * v2_metrics["worst_complex_passband_approximation_error"]
        ),
    }
    pareto_regression_ok = bool(
        certification["passband_ripple_db_peak_to_peak"]
        <= max(
            1.02 * v2_metrics["passband_ripple_db_peak_to_peak"],
            v2_metrics["passband_ripple_db_peak_to_peak"] + 0.0001,
        )
        and certification["step_response_overshoot"]
        <= max(
            1.02 * v2_metrics["step_response_overshoot"],
            v2_metrics["step_response_overshoot"] + 1.0e-6,
        )
        and certification["broadband_pre_peak_energy_db"]
        <= v2_metrics["broadband_pre_peak_energy_db"] + 0.1
    )
    certification["v2_pareto"] = {
        "regression_ok": pareto_regression_ok,
        "improvements": pareto_improvements,
        "improvement_count": int(sum(pareto_improvements.values())),
    }
    certification["accepted"] = bool(
        certification["accepted"]
        and pareto_regression_ok
        and int(sum(pareto_improvements.values())) >= 2
    )
    (work_dir / "certification.json").write_text(
        json.dumps(certification, indent=2) + "\n"
    )
    log.write("certification_complete", **certification)
    if not certification["accepted"]:
        raise RuntimeError(
            "Split Phase V3 certification failed; refusing to export production assets"
        )
    manifest = export_assets(
        asset_dir,
        character,
        cleanups,
        rational_147_160,
        rational_160_147,
        {
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "generator_versions": {
                "python": sys.version,
                "numpy": np.__version__,
                "scipy": scipy.__version__,
                "cvxpy": cvxpy.__version__,
                "jax": jax.__version__,
                "jaxlib": jaxlib.__version__,
            },
            "configuration": config,
            "solvers": {
                "magnitude": magnitude_report,
                "spectral_factor": spectral_report,
                "group_delay": group_report,
            },
            "support_search": support_report,
            "character_optimization": character_report,
            "cleanup_certification": cleanup_reports,
            "cleanup_polish": cleanup_polish_report,
            "rational_certification": {
                "rational_147_160": rational_up_report,
                "rational_160_147": rational_down_report,
            },
            "certification": certification,
            "v2_comparison": {
                "baseline": baseline_report,
                "status": "coefficient-domain comparison captured; Rust runtime comparison required"
            },
        },
    )
    log.write(
        "assets_exported",
        manifest=str(asset_dir / "manifest.json"),
        character_sha256=manifest["files"]["character"]["sha256"],
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
