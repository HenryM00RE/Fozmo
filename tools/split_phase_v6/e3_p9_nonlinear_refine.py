from __future__ import annotations

import argparse
import json
import math
from dataclasses import asdict
from pathlib import Path
from typing import Any

import numpy as np
from scipy import optimize

from .e3_p7_cleanup_search import _frequency_metrics
from .e3_p9_feasibility import (
    MAGNITUDE_LOWER_DB,
    MAGNITUDE_UPPER_DB,
    PACKET_TOLERANCE_DB,
    PHASE_BOUND_RAD,
    _meaningful,
)
from .e3_p9_timing_search import (
    _coordinate_scales,
    _packet_nullspace,
    _packet_measurements,
    _realize_support,
    _sha256_bytes,
    _static_safe,
    _timing_delta,
)
from .e3_phase_search import (
    _cascade_character_and_cleanup,
    _read_f64le,
    _timing_metrics,
)
from .evaluate_e3_packets import PACKET_FREQUENCIES_HZ, _measure_packet


IDENTITY = "SplitPhase128kE3-P9-exact-nonlinear-packet-boundary-refine"
OBJECTIVES = ("pre_lobe", "post_lobe", "side_energy")


def _objective_value(timing: dict[str, Any], name: str) -> float:
    if name == "pre_lobe":
        return float(timing["maximum_pre_lobe_db_peak"])
    if name == "post_lobe":
        return float(timing["maximum_post_lobe_db_peak"])
    return float(timing["pre_energy_db_total"] + timing["post_energy_db_total"])


def _timing_subspace(
    feasibility: dict[str, Any], maximum_dimensions: int
) -> tuple[np.ndarray, dict[str, Any]]:
    """Return orthonormal packet-null directions with first-order timing leverage."""
    nullspace, packet_contract = _packet_nullspace(feasibility)
    jacobian = np.asarray(feasibility["jacobian"], dtype=np.float64)
    names = feasibility["result_names"]
    phase_count = len(feasibility["contract"]["phase_knots_hz"]) - 2
    scales = _coordinate_scales(phase_count)
    normalized = jacobian * scales[None, :]
    rows = {name: index for index, name in enumerate(names)}
    gradients = (
        normalized[rows["timing/maximum_pre_lobe_db_peak"]],
        normalized[rows["timing/maximum_post_lobe_db_peak"]],
        normalized[rows["timing/pre_energy_db_total"]]
        + normalized[rows["timing/post_energy_db_total"]],
        normalized[rows["timing/main_lobe_width_us"]],
        normalized[rows["timing/step_overshoot_percent"]],
        normalized[rows["timing/step_undershoot_percent"]],
        normalized[rows["timing/decay_120_ms"]],
    )
    projected = np.column_stack(
        [nullspace @ (nullspace.T @ gradient) for gradient in gradients]
    )
    left, singular_values, _ = np.linalg.svd(projected, full_matrices=False)
    if singular_values.size == 0:
        raise RuntimeError("the packet-null timing subspace is empty")
    threshold = max(float(singular_values[0]) * 1.0e-10, 1.0e-12)
    rank = min(int(np.sum(singular_values > threshold)), maximum_dimensions)
    if rank == 0:
        raise RuntimeError("the packet-null timing subspace has zero numerical rank")
    basis = left[:, :rank]
    return basis, {
        "maximum_dimensions": maximum_dimensions,
        "selected_dimensions": rank,
        "timing_projected_singular_values": singular_values.tolist(),
        "packet_nullspace": packet_contract,
    }


def _recover_feasible_boundary(
    evaluator: Any,
    start: np.ndarray,
    target: np.ndarray,
    iterations: int = 16,
) -> tuple[np.ndarray, float]:
    """Backtrack an infeasible optimizer step to the exact guarded boundary."""
    start_values = np.asarray(start, dtype=np.float64)
    target_values = np.asarray(target, dtype=np.float64)
    start_result = evaluator.evaluate(start_values)
    if not (
        start_result["passes_static_gates"] and start_result["passes_packet_gates"]
    ):
        raise ValueError("boundary recovery requires a feasible start")
    target_result = evaluator.evaluate(target_values)
    if target_result["passes_static_gates"] and target_result["passes_packet_gates"]:
        return target_values.copy(), 1.0
    low = 0.0
    high = 1.0
    direction = target_values - start_values
    for _ in range(iterations):
        middle = (low + high) * 0.5
        values = start_values + middle * direction
        measured = evaluator.evaluate(values)
        if measured["passes_static_gates"] and measured["passes_packet_gates"]:
            low = middle
        else:
            high = middle
    return start_values + low * direction, low


class ExactEvaluator:
    def __init__(
        self,
        e2: np.ndarray,
        cleanup: np.ndarray,
        phase_knots_hz: np.ndarray,
        scales: np.ndarray,
        baseline_response: np.ndarray,
        baseline_timing: dict[str, Any],
        baseline_packets: dict[str, Any],
        objective_name: str,
    ) -> None:
        self.e2 = e2
        self.cleanup = cleanup
        self.phase_knots_hz = phase_knots_hz
        self.scales = scales
        self.baseline_response = baseline_response
        self.baseline_timing = baseline_timing
        self.baseline_packets = baseline_packets
        self.objective_name = objective_name
        self.last_z: np.ndarray | None = None
        self.last: dict[str, Any] | None = None
        self.calls = 0
        self.feasible_evaluations = 0
        self.best_feasible_score = math.inf
        self.best_feasible_z: np.ndarray | None = None

    def evaluate(self, z: np.ndarray) -> dict[str, Any]:
        values = np.asarray(z, dtype=np.float64)
        if self.last_z is not None and np.array_equal(values, self.last_z):
            assert self.last is not None
            return self.last
        coordinates = values * self.scales
        character, structural = _realize_support(
            self.e2, coordinates, self.e2.size, self.phase_knots_hz
        )
        response = _cascade_character_and_cleanup(character, self.cleanup)
        timing = asdict(_timing_metrics(response))
        packets, packet_safe, packet_failures = _packet_measurements(
            response, self.baseline_packets
        )
        static_safe, static_failures = _static_safe(timing, self.baseline_timing)
        result = {
            "coordinates": coordinates,
            "character": character,
            "response": response,
            "structural": structural,
            "timing": timing,
            "timing_delta_vs_e2v3": _timing_delta(timing, self.baseline_timing),
            "packets": packets,
            "passes_packet_gates": packet_safe,
            "packet_failures": packet_failures,
            "passes_static_gates": static_safe,
            "static_failures": static_failures,
        }
        self.last_z = values.copy()
        self.last = result
        self.calls += 1
        if static_safe and packet_safe:
            self.feasible_evaluations += 1
            score = _objective_value(timing, self.objective_name)
            if score < self.best_feasible_score:
                self.best_feasible_score = score
                self.best_feasible_z = values.copy()
        return result

    def constraints(self, z: np.ndarray) -> np.ndarray:
        result = self.evaluate(z)
        timing = result["timing"]
        tolerances = {
            "pre_energy_db_total": 0.02,
            "maximum_pre_lobe_db_peak": 0.05,
            "post_energy_db_total": 0.02,
            "maximum_post_lobe_db_peak": 0.05,
            "main_lobe_width_us": 0.20,
            "step_overshoot_percent": 0.05,
            "step_undershoot_percent": 0.05,
            "decay_120_ms": 0.10,
        }
        margins = []
        for key, tolerance in tolerances.items():
            value = timing[key]
            margins.append(
                -1.0 if value is None else self.baseline_timing[key] + tolerance - value
            )
        for frequency, packet in result["packets"].items():
            for key in (
                "onset_pre_echo_energy_db_total",
                "maximum_onset_pre_echo_db_peak",
            ):
                margins.append(
                    self.baseline_packets[frequency][key]
                    + PACKET_TOLERANCE_DB
                    - packet[key]
                )
        return np.asarray(margins, dtype=np.float64)

    def objective(self, z: np.ndarray, name: str) -> float:
        return _objective_value(self.evaluate(z)["timing"], name)


def refine(
    root: Path,
    search_path: Path,
    work_dir: Path,
    starts: int,
    max_iterations: int,
    subspace_dimensions: int,
) -> dict[str, Any]:
    search_report = json.loads(search_path.read_text(encoding="utf-8"))
    feasibility_path = root / search_report["source"]["feasibility"]
    feasibility = json.loads(feasibility_path.read_text(encoding="utf-8"))
    subspace, subspace_contract = _timing_subspace(feasibility, subspace_dimensions)
    phase_knots_hz = np.asarray(
        feasibility["contract"]["phase_knots_hz"],
        dtype=np.float64,
    )
    e2_path = root / "assets/filters/split_phase_e2v3/character_full_rate.f64le"
    cleanup_path = root / "assets/filters/split_phase_e2v3/cleanup_stage_1.f64le"
    e2 = _read_f64le(e2_path)
    cleanup = _read_f64le(cleanup_path)
    baseline_response = _cascade_character_and_cleanup(e2, cleanup)
    baseline_timing = asdict(_timing_metrics(baseline_response))
    baseline_packets = {
        str(int(frequency)): asdict(_measure_packet(baseline_response, frequency))
        for frequency in PACKET_FREQUENCIES_HZ
    }
    phase_count = phase_knots_hz.size - 2
    scales = _coordinate_scales(phase_count)
    lower = (
        np.concatenate((np.full(phase_count, -PHASE_BOUND_RAD), MAGNITUDE_LOWER_DB))
        / scales
    )
    upper = (
        np.concatenate((np.full(phase_count, PHASE_BOUND_RAD), MAGNITUDE_UPPER_DB))
        / scales
    )
    source_records = search_report["per_support"][0]["packet_safe"]
    unique_starts = source_records[:starts]
    records = []
    characters: dict[str, np.ndarray] = {}
    for start_index, source in enumerate(unique_starts):
        z0 = np.asarray(source["coordinates"], dtype=np.float64) / scales
        # Restrict each exact solve to a local trust box. Outer multi-starts
        # supply global coverage while keeping packet curvature manageable.
        for objective_name in OBJECTIVES:
            evaluator = ExactEvaluator(
                e2,
                cleanup,
                phase_knots_hz,
                scales,
                baseline_response,
                baseline_timing,
                baseline_packets,
                objective_name,
            )

            def expand(local: np.ndarray) -> np.ndarray:
                return z0 + subspace @ np.asarray(local, dtype=np.float64)

            def constrained(local: np.ndarray) -> np.ndarray:
                expanded = expand(local)
                return np.concatenate(
                    (
                        evaluator.constraints(expanded),
                        expanded - lower,
                        upper - expanded,
                    )
                )

            local_zero = np.zeros(subspace.shape[1], dtype=np.float64)
            result = optimize.minimize(
                lambda local, name=objective_name: evaluator.objective(
                    expand(local), name
                ),
                local_zero,
                method="SLSQP",
                bounds=optimize.Bounds(
                    np.full(subspace.shape[1], -0.05),
                    np.full(subspace.shape[1], 0.05),
                ),
                constraints=[{"type": "ineq", "fun": constrained}],
                options={
                    "maxiter": max_iterations,
                    "ftol": 1.0e-9,
                    "eps": 1.0e-4,
                    "disp": False,
                },
            )
            terminal_z = expand(result.x)
            terminal = evaluator.evaluate(terminal_z)
            boundary_z, boundary_fraction = _recover_feasible_boundary(
                evaluator, z0, terminal_z
            )
            evaluator.evaluate(boundary_z)
            selected_z = (
                evaluator.best_feasible_z
                if evaluator.best_feasible_z is not None
                else terminal_z
            )
            measured = evaluator.evaluate(selected_z)
            frequency = _frequency_metrics(measured["response"], baseline_response)
            passes_frequency = bool(
                frequency["maximum_passband_delta_db_0_18khz"] <= 1.0e-3
                and frequency["maximum_stopband_db_22k05_nyquist"] <= -150.0
            )
            meaningful = _meaningful(
                {"timing": measured["timing"], "packets": measured["packets"]},
                {"timing": baseline_timing, "packets": baseline_packets},
            )
            identifier = f"nonlinear-{start_index:02d}-{objective_name}"
            record = {
                "identifier": identifier,
                "source_identifier": source["identifier"],
                "objective": objective_name,
                "optimizer_success": bool(result.success),
                "optimizer_status": int(result.status),
                "optimizer_message": str(result.message),
                "optimizer_iterations": int(result.nit),
                "exact_evaluations": evaluator.calls,
                "exact_feasible_evaluations": evaluator.feasible_evaluations,
                "selected_best_feasible_iterate": bool(
                    evaluator.best_feasible_z is not None
                    and not np.array_equal(selected_z, terminal_z)
                ),
                "terminal_coordinates": terminal_z.tolist(),
                "terminal_timing_delta_vs_e2v3": terminal["timing_delta_vs_e2v3"],
                "terminal_passes_static_gates": terminal["passes_static_gates"],
                "terminal_static_failures": terminal["static_failures"],
                "terminal_passes_packet_gates": terminal["passes_packet_gates"],
                "terminal_packet_failures": terminal["packet_failures"],
                "terminal_packets": terminal["packets"],
                "recovered_boundary_fraction": boundary_fraction,
                "coordinates": measured["coordinates"].tolist(),
                "structural": measured["structural"],
                "timing": measured["timing"],
                "timing_delta_vs_e2v3": measured["timing_delta_vs_e2v3"],
                "packets": measured["packets"],
                "passes_static_gates": measured["passes_static_gates"],
                "static_failures": measured["static_failures"],
                "passes_packet_gates": measured["passes_packet_gates"],
                "packet_failures": measured["packet_failures"],
                "frequency": frequency,
                "passes_frequency_gates": passes_frequency,
                "meaningful": meaningful,
            }
            record["passes_all_gates"] = bool(
                record["passes_static_gates"]
                and record["passes_packet_gates"]
                and passes_frequency
            )
            records.append(record)
            if record["passes_all_gates"]:
                characters[identifier] = measured["character"]
    qualified = [record for record in records if record["passes_all_gates"]]
    qualified.sort(
        key=lambda record: (
            record["timing_delta_vs_e2v3"]["maximum_pre_lobe_db_peak"],
            record["timing_delta_vs_e2v3"]["maximum_post_lobe_db_peak"],
            record["timing_delta_vs_e2v3"]["pre_energy_db_total"]
            + record["timing_delta_vs_e2v3"]["post_energy_db_total"],
        )
    )
    finalist_dir = work_dir / "finalists"
    finalist_dir.mkdir(parents=True, exist_ok=True)
    finalists = qualified[:8]
    for record in finalists:
        payload = np.asarray(characters[record["identifier"]], dtype="<f8").tobytes()
        path = finalist_dir / f"{record['identifier']}.f64le"
        path.write_bytes(payload)
        record["character_file"] = str(path.relative_to(work_dir)).replace("\\", "/")
        record["character_sha256"] = _sha256_bytes(payload)
    return {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "source": {
            "search": str(search_path.relative_to(root)).replace("\\", "/"),
            "e2_sha256": _sha256_bytes(e2_path.read_bytes()),
            "cleanup_sha256": _sha256_bytes(cleanup_path.read_bytes()),
        },
        "contract": {
            "starts": starts,
            "objectives": OBJECTIVES,
            "maximum_iterations": max_iterations,
            "timing_subspace": subspace_contract,
            "normalized_trust_radius": 0.05,
            "packet_tolerance_db_vs_e2v3": PACKET_TOLERANCE_DB,
            "rejection_floor_db": -150.0,
        },
        "baseline": {"timing": baseline_timing, "packets": baseline_packets},
        "records": records,
        "qualified_count": len(qualified),
        "clear_replacement_count": sum(
            record["meaningful"]["clear_replacement_timing"] for record in qualified
        ),
        "finalists": finalists,
    }


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Refine P9 candidates on exact packet boundaries"
    )
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[2]
    )
    parser.add_argument(
        "--search",
        type=Path,
        default=Path(__file__).resolve().parent
        / "work-e3-p9-search-dense/e3_p9_timing_search.json",
    )
    parser.add_argument(
        "--work-dir",
        type=Path,
        default=Path(__file__).resolve().parent / "work-e3-p9-nonlinear",
    )
    parser.add_argument("--starts", type=int, default=3)
    parser.add_argument("--max-iterations", type=int, default=20)
    parser.add_argument("--subspace-dimensions", type=int, default=7)
    arguments = parser.parse_args()
    report = refine(
        arguments.root.resolve(),
        arguments.search.resolve(),
        arguments.work_dir.resolve(),
        arguments.starts,
        arguments.max_iterations,
        arguments.subspace_dimensions,
    )
    output = arguments.work_dir / "e3_p9_nonlinear_refine.json"
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(
        json.dumps(
            {
                "output": str(output),
                "candidate_count": len(report["records"]),
                "qualified_count": report["qualified_count"],
                "clear_replacement_count": report["clear_replacement_count"],
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
