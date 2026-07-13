from __future__ import annotations

import json
import hashlib
import copy
import tempfile
import unittest
from pathlib import Path

import dsd64_ecbeam2_experiment as experiment
from dsd_candidate_config import canonical_candidate_params, validate_candidate_tiers


def budgets() -> experiment.FrozenBudgets:
    return experiment.FrozenBudgets(
        {
            2_822_400: experiment.FrozenWireBudget(0.10, 0.010),
            3_072_000: experiment.FrozenWireBudget(0.20, 0.020),
        },
        "a" * 64,
    )


def matrix_row(
    *,
    sinad: float = 100.0,
    spur: float = 20.0,
    hf: float = 5.0,
    multitone: float = -3.0,
    overload: float = -60.0,
    score: float = 10.0,
    notes: str = "",
    reconstruction_energy: float = 1.0,
    include_ecbeam2_diagnostics: bool = True,
    limiter_events: int = 0,
) -> dict[str, str]:
    diagnostic_notes = ""
    if include_ecbeam2_diagnostics:
        diagnostic_notes = ";".join(
            (
                "ecbeam2_constraint_escape=0",
                "ecbeam2_state_repair_fallback=0",
                "ecbeam2_all_nonfinite_resets=0",
                "ecbeam2_output_length_error=0",
                "ecbeam2_observer_desynchronizations=0",
                "ecbeam2_invalid_input_substitutions=0",
                "ecbeam2_renderer_truncation_events=0",
                "ecbeam2_renderer_discarded_left_bits=0",
                "ecbeam2_renderer_discarded_right_bits=0",
                "ecbeam2_committed_samples=1000",
                "ecbeam2_min_survivors=1",
                f"ecbeam2_committed_output_energy_mean={reconstruction_energy}",
            )
        )
    combined_notes = ";".join(value for value in (diagnostic_notes, notes) if value)
    return {
        "status": "pass",
        "hard_failure_count": "0",
        "hard_failures": "",
        "inband_snr_worst_db": str(sinad),
        "inband_noise_spur_margin_db": str(spur),
        "high_freq_worst_residual_db": str(hf),
        "multitone_residual_db": str(multitone),
        "overload_recovery_dbfs": str(overload),
        "constrained_quality_score": str(score),
        "render_ms": "1.0",
        "bit_density_max_deviation": "0.0001",
        "limiter_limited_events": str(limiter_events),
        "limiter_limited_samples": str(limiter_events),
        "stability_resets": "0",
        "state_clamps": "0",
        "stress_stability_resets": "0",
        "stress_state_clamps": "0",
        "candidate_notes": combined_notes,
    }


def result(
    candidate: experiment.Candidate,
    rows: dict[tuple[str, int], dict[str, str]],
    index: int = 0,
) -> experiment.RunResult:
    spec = experiment.RunSpec(candidate, index, Path("out"), Path("ecbeam2_quality"), 44_100)
    return experiment.RunResult(spec, [], "complete", 0, 1.0, rows, [])


class EcBeam2ExperimentTests(unittest.TestCase):
    def test_stability_and_budget_commands_use_lightweight_qualification(self) -> None:
        candidate = experiment.scale_probe_candidates()[0]
        with tempfile.TemporaryDirectory() as temp:
            spec = experiment.RunSpec(
                candidate,
                0,
                Path(temp),
                Path("ecbeam2_quality"),
                44_100,
                experiment.MANIFEST_DIR / "stability_short.json",
            )
            command = experiment.build_command(spec)
            self.assertIn("--ecbeam2-qualification", command)
            self.assertNotIn("--selectable-dsd-matrix", command)
            self.assertEqual(command[command.index("--mode") + 1], "scale-probe")
            self.assertEqual(
                command[command.index("--filters") + 1],
                "MinimumPhase,SplitPhase",
            )

    def test_scale_probe_uses_four_inert_knees_without_a1_matrix(self) -> None:
        candidates = experiment.scale_probe_candidates()
        self.assertEqual(len(candidates), 4)
        self.assertTrue(all(candidate.modulator == "EcBeam2" for candidate in candidates))
        self.assertEqual(
            [candidate.params["ecbeam2_state_deadzone"] for candidate in candidates],
            [0.0, 0.7, 0.8, 0.88],
        )

    def test_stability_grid_is_deterministic_and_wire_scaled(self) -> None:
        by_wire = {}
        for wire_rate in experiment.WIRE_RATES.values():
            rows = {}
            for rho in (0.0, *experiment.STABILITY_BARRIER_KNEES):
                rows[f"{rho:.2f}"] = {
                    term: {
                        "median": 0.5,
                        "p95": 1.0 if term == "reconstruction_increment_abs" else 2.0,
                        "p99": 3.0,
                        "max": 4.0,
                    }
                    for term in experiment.SCALE_TERMS
                }
            by_wire[str(wire_rate)] = rows
        probe = {"by_wire_rate": by_wire, "scale_probe_digest": "a" * 64}
        first = experiment.stability_candidates(probe)
        second = experiment.stability_candidates(probe)
        self.assertEqual([row.stable_id() for row in first], [row.stable_id() for row in second])
        self.assertEqual(len(first), 35)
        terminal = next(row for row in first if row.label == "terminal-a0.1")
        for params in terminal.wire_params.values():
            self.assertAlmostEqual(params["ecbeam2_state_terminal_weight"], 0.05)

    def test_budget_power_allowance_and_strictest_selection(self) -> None:
        self.assertAlmostEqual(
            experiment.ultrasonic_power_allowance(2.0, 0.5),
            2.0 * 10.0 ** 0.05,
            places=10,
        )
        chosen = experiment.choose_strictest_budget_allowance(
            [
                {
                    "candidate_id": "loose",
                    "ultrasonic_allowance_db": 0.5,
                    "signed_error_multiplier": 1.5,
                    "constraint_escapes": 0,
                    "health_failures": [],
                    "all_required_corpora": True,
                },
                {
                    "candidate_id": "strict",
                    "ultrasonic_allowance_db": 0.25,
                    "signed_error_multiplier": 1.25,
                    "constraint_escapes": 0,
                    "health_failures": [],
                    "all_required_corpora": True,
                },
                {
                    "candidate_id": "too-strict",
                    "ultrasonic_allowance_db": 0.0,
                    "signed_error_multiplier": 1.0,
                    "constraint_escapes": 1,
                    "health_failures": [],
                    "all_required_corpora": True,
                },
            ]
        )
        self.assertEqual(chosen["candidate_id"], "strict")

    def test_shared_candidate_schema_canonicalizes_and_gates_ecbeam2_controls(self) -> None:
        params = canonical_candidate_params(
            {
                "ecbeam2_run_mode": "active",
                "ecbeam2_profile": "harness24to32-v1",
                "ecbeam2_state_deadzone": 0.45,
                "ecbeam2_state_deadzone_weight": 0.0,
                "ecbeam2_quantizer_regularizer": 0.0,
            }
        )
        self.assertEqual(params["ecbeam2_state_deadzone"], 0.45)
        self.assertNotIn("ecbeam2_quantizer_regularizer", params)
        validate_candidate_tiers(params, allow_exploratory=True)
        with self.assertRaisesRegex(ValueError, "exploratory"):
            validate_candidate_tiers(params, allow_exploratory=False)

    def test_candidate_matrix_is_bounded_unique_and_namespaced(self) -> None:
        frozen = budgets()
        candidates = experiment.legacy_v1_selection_candidates(frozen)
        self.assertEqual(len(candidates), 28)
        self.assertEqual(len({candidate.stable_id() for candidate in candidates}), 28)
        active = [candidate for candidate in candidates if candidate.modulator == "EcBeam2"]
        self.assertTrue(active)
        for candidate in active:
            for key in candidate.params:
                if key.startswith("ecbeam2_"):
                    self.assertIn(
                        key,
                        {
                            "ecbeam2_run_mode",
                            "ecbeam2_profile",
                            "ecbeam2_state_terminal_weight",
                            "ecbeam2_state_deadzone",
                            "ecbeam2_state_deadzone_weight",
                            "ecbeam2_quantizer_regularizer",
                        },
                    )
            self.assertNotIn("ec_beam_m", candidate.params)
        budgeted = [candidate for candidate in active if candidate.wire_budgets is not None]
        self.assertEqual(len(budgeted), 21)
        for candidate in budgeted:
            self.assertEqual(candidate.wire_budgets, frozen.by_wire_rate)
        ecdepth2 = next(
            candidate for candidate in candidates if candidate.label == "ecdepth2-reference"
        )
        self.assertEqual(
            dict(ecdepth2.params),
            {"headroom_db": -4.0, "expected_gain_db": -4.0},
        )
        self.assertNotIn("ec_obg", ecdepth2.params)
        self.assertNotIn("dither_scale", ecdepth2.params)

    def test_wire_specific_budgets_become_scalar_native_config(self) -> None:
        candidate = experiment.legacy_v1_selection_candidates(budgets())[-1]
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp)
            spec_44 = experiment.RunSpec(candidate, 0, out, Path("ecbeam2_quality"), 44_100)
            spec_48 = experiment.RunSpec(candidate, 0, out, Path("ecbeam2_quality"), 48_000)
            doc_44 = json.loads(experiment.write_run_config(spec_44).read_text())
            doc_48 = json.loads(experiment.write_run_config(spec_48).read_text())
            p44 = doc_44["params"]
            p48 = doc_48["params"]
            self.assertIn("ecbeam2_ultrasonic_budget", p44)
            self.assertIn("ecbeam2_signed_error_budget", p44)
            self.assertNotEqual(p44["ecbeam2_ultrasonic_budget"], p48["ecbeam2_ultrasonic_budget"])
            self.assertFalse(any(key.endswith("2822400") for key in p44))

    def test_loading_frozen_budgets_requires_percentiles_and_provenance(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "budgets.json"
            path.write_text(
                json.dumps(
                    {
                        "schema_version": experiment.FROZEN_BUDGET_SCHEMA_VERSION,
                        "calibration_digest": "a" * 64,
                        "by_wire_rate": {
                            "2822400": {
                                "ultrasonic_ema_max": 0.1,
                                "signed_error_ema_abs_max": 0.01,
                            }
                        },
                    }
                ),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "lacks frozen fields"):
                experiment.load_frozen_budgets(path)

    def test_winner_candidate_document_round_trips_for_held_out_replay(self) -> None:
        candidate = experiment.legacy_v1_selection_candidates(budgets())[-1]
        restored = experiment.candidate_from_document(experiment.candidate_document(candidate))
        self.assertEqual(restored.stable_id(), candidate.stable_id())
        self.assertEqual(restored.wire_budgets, candidate.wire_budgets)

    def test_build_command_runs_one_wire_family_and_two_filters(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            spec = experiment.RunSpec(
                experiment.calibration_candidate(),
                0,
                Path(temp),
                Path("/tmp/ecbeam2_quality"),
                48_000,
            )
            command = experiment.build_command(spec)
            self.assertEqual(command[command.index("--source-rates") + 1], "48000")
            self.assertEqual(command[command.index("--rates") + 1], "64")
            self.assertEqual(command[command.index("--budget-cell-cap") + 1], "2")
            self.assertEqual(
                command[command.index("--selectable-modulator") + 1], experiment.A1_MODULATOR
            )

            corpus = experiment.MANIFEST_DIR / "calibration.json"
            spec = experiment.RunSpec(
                experiment.calibration_candidate(),
                0,
                Path(temp),
                Path("/tmp/ecbeam2_quality"),
                48_000,
                corpus,
            )
            command = experiment.build_command(spec)
            self.assertEqual(
                Path(command[command.index("--ecbeam2-corpus-manifest") + 1]),
                corpus.resolve(),
            )

    def test_native_corpus_report_is_hash_and_coverage_bound(self) -> None:
        corpus = experiment.MANIFEST_DIR / "calibration.json"
        manifest = experiment.load_corpus_manifest(corpus, "calibration")
        expected_cases = experiment._expected_native_corpus_cases(manifest, 44_100)
        expected_fixture_cells = len(manifest["fixtures"]) * len(experiment.FILTERS)
        with tempfile.TemporaryDirectory() as temp:
            spec = experiment.RunSpec(
                experiment.calibration_candidate(),
                0,
                Path(temp),
                Path("ecbeam2_quality"),
                44_100,
                corpus,
            )
            spec.candidate_dir.mkdir(parents=True)
            report = {
                "schema_version": "ecbeam2-corpus-report-v1",
                "corpus_schema_version": experiment.CORPUS_SCHEMA_VERSION,
                "manifest_sha256": hashlib.sha256(corpus.read_bytes()).hexdigest(),
                "corpus_id": manifest["corpus_id"],
                "role": manifest["role"],
                "measurement_version": manifest["measurement_version"],
                "scoring_version": manifest["scoring_version"],
                "fixture_set_version": manifest["fixture_set_version"],
                "declared_source_rates": list(experiment.SOURCE_RATES),
                "declared_wire_rates": [
                    experiment.WIRE_RATES[rate] for rate in experiment.SOURCE_RATES
                ],
                "declared_filters": list(experiment.FILTERS),
                "declared_seeds": list(manifest["seeds"]),
                "selected_source_rates": [44_100],
                "selected_wire_rates": [2_822_400],
                "selected_filters": list(experiment.FILTERS),
                "selected_modulators": [experiment.A1_MODULATOR],
                "expected_fixture_cells": expected_fixture_cells,
                "rendered_fixture_cells": expected_fixture_cells,
                "cell_summaries": [
                    {
                        "filter": filter_name,
                        "modulator": "EcBeamM4N8",
                        "source_rate": 44_100,
                        "wire_rate": 2_822_400,
                        "rendered_cases": len(expected_cases),
                        "rendered_fixtures": len(manifest["fixtures"]),
                        "hard_failure_count": 0,
                    }
                    for filter_name in experiment.FILTERS
                ],
                "measurements": [
                    {
                        "manifest_sha256": hashlib.sha256(corpus.read_bytes()).hexdigest(),
                        "corpus_id": manifest["corpus_id"],
                        "role": manifest["role"],
                        "case_id": case_id,
                        "fixture_id": case["fixture_id"],
                        "category": case["category"],
                        "filter": filter_name,
                        "modulator": "EcBeamM4N8",
                        "source_rate": 44_100,
                        "wire_rate": 2_822_400,
                        "generator_seed": case["generator_seed"],
                        "start_sample": case["start_sample"],
                        "length_samples": case["length_samples"],
                        "metric": {
                            "native_left_sha256": hashlib.sha256(
                                f"{case_id}|{filter_name}|left".encode()
                            ).hexdigest(),
                            "native_right_sha256": hashlib.sha256(
                                f"{case_id}|{filter_name}|right".encode()
                            ).hexdigest(),
                        },
                    }
                    for case_id, case in expected_cases.items()
                    for filter_name in experiment.FILTERS
                ],
                "hard_failures": [],
            }
            report_path = spec.candidate_dir / "ecbeam2_corpus_report.json"
            report_path.write_text(json.dumps(report), encoding="utf-8")
            self.assertEqual(experiment.validate_native_corpus_report(spec), [])
            run = experiment.RunResult(
                spec,
                [["ecbeam2_quality", "--out", str(spec.candidate_dir)]],
                "complete",
                0,
                1.0,
            )
            expected_digests = experiment.corpus_bitstream_digests(run)
            self.assertEqual(len(expected_digests), len(expected_cases) * 4)
            self.assertTrue(
                all(key.split("|")[3] == experiment.A1_MODULATOR for key in expected_digests)
            )
            self.assertEqual(
                experiment.verify_expected_baseline_digests(
                    run,
                    {
                        "corpus_id": "test",
                        "expected_baseline_digests": expected_digests,
                    },
                ),
                expected_digests,
            )
            report["measurements"][0]["metric"]["native_left_sha256"] = "3" * 64
            report_path.write_text(json.dumps(report), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "baseline digests changed"):
                experiment.verify_expected_baseline_digests(
                    run,
                    {
                        "corpus_id": "test",
                        "expected_baseline_digests": expected_digests,
                    },
                )
            report["rendered_fixture_cells"] = 5
            report_path.write_text(json.dumps(report), encoding="utf-8")
            self.assertTrue(
                any(
                    "fixture coverage mismatch" in error
                    for error in experiment.validate_native_corpus_report(spec)
                )
            )

    def test_baseline_freeze_dry_run_is_single_a1_candidate_and_fail_closed_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp)
            self.assertEqual(
                experiment.main(
                    [
                        "--phase",
                        "selection",
                        "--freeze-baseline-only",
                        "--dry-run",
                        "--binary",
                        "/tmp/ecbeam2_quality",
                        "--out",
                        str(out),
                    ]
                ),
                0,
            )
            dry_run = json.loads((out / "dry_run.json").read_text(encoding="utf-8"))
            self.assertEqual(len(dry_run["commands"]), 2)
            self.assertTrue(
                all("--selectable-modulator EcBeam" in command for command in dry_run["commands"])
            )
            self.assertTrue(dry_run["execution_preconditions"]["baseline_freeze_only"])
            self.assertFalse(
                dry_run["execution_preconditions"]["expected_baseline_digests_required"]
            )
            self.assertFalse(
                dry_run["execution_preconditions"]["exact_oracle_results_required"]
            )

    def test_checked_in_corpora_are_valid_and_disjoint(self) -> None:
        corpora = [
            experiment.load_corpus_manifest(
                experiment.MANIFEST_DIR / f"{role}.json", role
            )
            for role in ("calibration", "selection", "held_out")
        ]
        experiment.validate_disjoint_corpora(corpora)

    def test_real_selection_requires_frozen_baseline_digest_coverage(self) -> None:
        with self.assertRaisesRegex(ValueError, "non-empty expected baseline digests"):
            experiment.load_corpus_manifest(
                experiment.MANIFEST_DIR / "selection.json",
                "selection",
                require_expected_baseline_digests=True,
            )

    def test_manifest_validation_freezes_wire_rates_fixture_version_and_seeds(self) -> None:
        source = json.loads(
            (experiment.MANIFEST_DIR / "calibration.json").read_text(encoding="utf-8")
        )
        for field, value, message in (
            ("wire_rates", [2_822_400], "wire rates"),
            ("fixture_set_version", "changed", "fixture-set version"),
            ("seeds", [1, 1], "unique non-negative integer seeds"),
        ):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as temp:
                data = dict(source)
                data[field] = value
                path = Path(temp) / "manifest.json"
                path.write_text(json.dumps(data), encoding="utf-8")
                with self.assertRaisesRegex(ValueError, message):
                    experiment.load_corpus_manifest(path, "calibration")

        with tempfile.TemporaryDirectory() as temp:
            data = copy.deepcopy(source)
            repeated_spec = "pink_noise|seed=0xc001|v1"
            data["fixtures"][1]["generator"] = repeated_spec
            data["fixtures"][1]["generator_spec_sha256"] = hashlib.sha256(
                repeated_spec.encode("utf-8")
            ).hexdigest()
            data["seeds"] = [0xC001]
            path = Path(temp) / "manifest.json"
            path.write_text(json.dumps(data), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "reuse seed"):
                experiment.load_corpus_manifest(path, "calibration")

    def test_oracle_request_requires_complete_n8_n12_n16_results(self) -> None:
        corpus = experiment.load_corpus_manifest(
            experiment.MANIFEST_DIR / "selection.json", "selection"
        )
        request = experiment.oracle_request_document(
            corpus,
            corpus_manifest_sha256=hashlib.sha256(
                (experiment.MANIFEST_DIR / "selection.json").read_bytes()
            ).hexdigest(),
            frozen_budgets=budgets(),
            frozen_budget_file_sha256="d" * 64,
        )
        fixture_seeds = {
            fixture["id"]: experiment._parse_ecbeam2_generator_spec(fixture["generator"])
            for fixture in corpus["fixtures"]
            if fixture["kind"] == "generated"
        }
        self.assertEqual(request["plant"], experiment.ECBEAM2_V1_PLANT)
        self.assertEqual(
            {case["channel"] for case in request["cases"]}, set(experiment.CHANNELS)
        )
        self.assertEqual(
            len(request["cases"]),
            len(corpus["difficult_windows"])
            * len(experiment.FILTERS)
            * len(experiment.CHANNELS),
        )
        self.assertTrue(
            all(case["seed"] == fixture_seeds[case["fixture_id"]] for case in request["cases"])
        )
        rows = []
        for case in request["cases"]:
            for horizon in request["exact_horizons"]:
                rows.append(
                    {
                        "case_id": case["case_id"],
                        "source_case_id": case["source_case_id"],
                        "fixture_id": case["fixture_id"],
                        "filter": case["filter"],
                        "channel": case["channel"],
                        "source_rate": case["source_rate"],
                        "wire_rate": case["wire_rate"],
                        "seed": case["seed"],
                        "ultrasonic_budget": budgets()
                        .by_wire_rate[case["wire_rate"]]
                        .ultrasonic_ema_max,
                        "signed_error_budget": budgets()
                        .by_wire_rate[case["wire_rate"]]
                        .signed_error_ema_abs_max,
                        "horizon": horizon,
                        "first_bit": 1,
                        "m4n8_first_bit": 1,
                        "sequence_bits": [1] * horizon,
                        "objective": 1.0,
                        "reconstruction_objective": 1.0,
                        "starting_state_potential": 0.25,
                        "terminal_state_potential": 0.25,
                        "state_terminal_delta": 0.0,
                        "state_terminal_cost": 0.0,
                        "state_barrier_raw": 0.0,
                        "state_barrier_cost": 0.0,
                        "quantizer_error_energy": 0.0,
                        "quantizer_regularizer_cost": 0.0,
                        "total_objective": 1.0,
                        "starting_tail_energy": 0.5,
                        "causal_reconstruction_energy": 1.0,
                        "remaining_tail_energy": 0.5,
                        "tail_adjusted_energy": 1.0,
                        "causal_ultrasonic_energy": 0.25,
                        "maximum_state_overflow": 0.0,
                        "maximum_budget_violation": 0.0,
                        "constraint_escapes": 0,
                        "state_repairs": 0,
                        "complete_sequences": 2**horizon,
                        "state_feasible": True,
                        "budgets_feasible": True,
                        "reconstructed_outputs": [1.0] + [0.0] * (horizon - 1),
                        "source_window_start_sample": case["start_sample"],
                        "prefix_sample_count": (
                            case["start_sample"] * case["wire_rate"] // case["source_rate"]
                        ),
                        "prefix_constraint_escapes": 0,
                        "prefix_state_repairs": 0,
                        "prefix_all_nonfinite_resets": 0,
                        "prefix_invalid_input_substitutions": 0,
                        "prefix_output_length_events": 0,
                        "prefix_sha256": "c" * 64,
                        "window_sha256": f"{horizon // 4:x}" * 64,
                    }
                )
        results = {
            "schema_version": experiment.ORACLE_SCHEMA_VERSION,
            "request_digest": request["request_digest"],
            "request_sha256": request["request_sha256"],
            "request_file_sha256": hashlib.sha256(
                experiment._canonical_json_bytes(request)
            ).hexdigest(),
            "corpus_id": request["corpus_id"],
            "corpus_manifest_sha256": request["corpus_manifest_sha256"],
            "profile": request["profile"],
            "profile_bindings": request["profile_bindings"],
            "input_hash_encoding": request["input_hash_encoding"],
            "plant": request["plant"],
            "constraint_budgets": request["constraint_budgets"],
            "objective": request["objective"],
            "candidate_id": request["candidate_id"],
            "objective_configs": request["objective_configs"],
            "objective_scale_bindings": request["objective_scale_bindings"],
            "start_mode": request["start_mode"],
            "results": rows,
        }
        experiment.validate_oracle_results(request, results)
        altered_request = json.loads(json.dumps(request))
        altered_request["objective_configs"]["2822400"]["state_terminal_weight"] = 0.1
        with self.assertRaisesRegex(ValueError, "request digest mismatch"):
            experiment.validate_oracle_results(altered_request, results)
        summary = experiment.oracle_comparison_summary(
            request, results, results_sha256="b" * 64
        )
        self.assertEqual(summary["case_count"], len(request["cases"]))
        self.assertEqual(
            {case["channel"] for case in summary["cases"]}, set(experiment.CHANNELS)
        )
        self.assertTrue(
            all(case["n16_external"]["prefix_eligible"] for case in summary["cases"])
        )
        self.assertEqual(summary["m4n8_exact_n8_first_bit_disagreements"], 0)
        self.assertEqual(summary["n16_infeasible_cases"], 0)
        self.assertAlmostEqual(
            summary["cases"][0]["n16_external"]["reconstructed_output_rms"],
            0.25,
        )
        results["results"][0]["ultrasonic_budget"] += 0.01
        with self.assertRaisesRegex(ValueError, "frozen wire budget"):
            experiment.validate_oracle_results(request, results)
        first_wire = str(results["results"][0]["wire_rate"])
        results["results"][0]["ultrasonic_budget"] = request["constraint_budgets"][
            "by_wire_rate"
        ][first_wire]["ultrasonic_ema_max"]
        results["results"][0]["first_bit"] = -1
        with self.assertRaisesRegex(ValueError, "first bit disagrees"):
            experiment.validate_oracle_results(request, results)
        results["results"][0]["first_bit"] = 1
        results["results"][0]["prefix_sample_count"] += 1
        with self.assertRaisesRegex(ValueError, "inconsistent exact prefix coverage"):
            experiment.validate_oracle_results(request, results)
        results["results"][0]["prefix_sample_count"] -= 1
        results["results"][0]["prefix_state_repairs"] = 1
        with self.assertRaisesRegex(ValueError, "ineligible prefix"):
            experiment.validate_oracle_results(request, results)
        results["results"][0]["prefix_state_repairs"] = 0
        results["results"].pop()
        with self.assertRaisesRegex(ValueError, "coverage mismatch"):
            experiment.validate_oracle_results(request, results)

        def resign(changed: dict) -> None:
            body = copy.deepcopy(changed)
            body.pop("request_digest", None)
            body.pop("request_sha256", None)
            digest = experiment._stable_hash(body, length=64)
            changed["request_digest"] = digest
            changed["request_sha256"] = digest

        changed_request = copy.deepcopy(request)
        changed_request["plant"]["input_peak"] += 0.001
        resign(changed_request)
        with self.assertRaisesRegex(ValueError, "frozen EcBeam2 v1 plant"):
            experiment.validate_oracle_results(changed_request, results)

        changed_request = copy.deepcopy(request)
        changed_request["cases"][0]["seed"] += 1
        resign(changed_request)
        with self.assertRaisesRegex(ValueError, "generator/seed binding"):
            experiment.validate_oracle_results(changed_request, results)

        legacy_mono_request = copy.deepcopy(request)
        legacy_mono_request["cases"][0].pop("channel")
        legacy_mono_request["cases"][0]["case_id"] = (
            f"{legacy_mono_request['cases'][0]['source_case_id']}--"
            f"{legacy_mono_request['cases'][0]['filter']}"
        )
        resign(legacy_mono_request)
        with self.assertRaisesRegex(ValueError, "invalid expanded case identity"):
            experiment.validate_oracle_results(legacy_mono_request, results)

    def test_calibration_freezes_maxima_separately_by_wire_rate(self) -> None:
        rows = {}
        for filter_index, filter_name in enumerate(experiment.FILTERS):
            for source_rate in experiment.SOURCE_RATES:
                base = 0.1 if source_rate == 44_100 else 0.2
                rows[(filter_name, source_rate)] = matrix_row(
                    notes=(
                        f"ecbeam2_ultrasonic_ema_max={base + filter_index * 0.01};"
                        f"ecbeam2_signed_error_ema_abs_max={base / 10 + filter_index * 0.001};"
                        f"ecbeam2_ultrasonic_ema_p99_9={base / 2 + filter_index * 0.01};"
                        f"ecbeam2_ultrasonic_ema_p99_99={base / 1.5 + filter_index * 0.01};"
                        f"ecbeam2_signed_error_ema_abs_p99_9={base / 20 + filter_index * 0.001};"
                        f"ecbeam2_signed_error_ema_abs_p99_99={base / 15 + filter_index * 0.001}"
                    )
                )
        frozen = experiment.freeze_calibration_budgets(
            result(experiment.calibration_candidate(), rows)
        )
        self.assertAlmostEqual(frozen.by_wire_rate[2_822_400].ultrasonic_ema_max, 0.11)
        self.assertAlmostEqual(frozen.by_wire_rate[3_072_000].ultrasonic_ema_max, 0.21)
        self.assertGreater(frozen.by_wire_rate[2_822_400].ultrasonic_ema_p99_99, 0.0)
        self.assertEqual(
            frozen.by_wire_rate[2_822_400].ultrasonic_worst_cell,
            "SplitPhase@44100",
        )
        self.assertEqual(len(frozen.calibration_digest), 64)

    def test_native_calibration_freezes_worst_fixture_window_provenance(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            commands = []
            for source_rate, wire_rate in experiment.WIRE_RATES.items():
                out = root / str(source_rate)
                out.mkdir()
                base = 0.1 if source_rate == 44_100 else 0.2
                measurements = []
                for index, filter_name in enumerate(experiment.FILTERS):
                    diagnostics = {
                        "ultrasonic_ema_max": base + 0.01 * index,
                        "signed_error_ema_abs_max": base / 10 + 0.001 * index,
                        "ultrasonic_ema_p99_9": base / 2 + 0.02 * index,
                        "ultrasonic_ema_p99_99": base / 1.5 + 0.03 * index,
                        "signed_error_ema_abs_p99_9": base / 20 + 0.002 * index,
                        "signed_error_ema_abs_p99_99": base / 15 + 0.003 * index,
                    }
                    measurements.append(
                        {
                            "case_id": f"case-{source_rate}-{index}",
                            "fixture_id": f"fixture-{source_rate}-{index}",
                            "filter": filter_name,
                            "modulator": experiment.A1_MODULATOR,
                            "source_rate": source_rate,
                            "wire_rate": wire_rate,
                            "start_sample": 100 * index,
                            "length_samples": 2048,
                            "ecbeam2_diagnostics": diagnostics,
                        }
                    )
                (out / "ecbeam2_corpus_report.json").write_text(
                    json.dumps({"measurements": measurements}), encoding="utf-8"
                )
                commands.append(["ecbeam2_quality", "--out", str(out)])
            spec = experiment.RunSpec(
                experiment.calibration_candidate(),
                0,
                root,
                Path("ecbeam2_quality"),
                44_100,
            )
            result = experiment.RunResult(spec, commands, "complete", 0, 1.0)
            observations = experiment._native_calibration_observations(result)
            self.assertIsNotNone(observations)
            frozen = experiment._freeze_native_calibration_budgets(observations)
            budget = frozen.by_wire_rate[2_822_400]
            self.assertAlmostEqual(budget.ultrasonic_ema_max, 0.11)
            self.assertIn("case-44100-1|fixture-44100-1|SplitPhase", budget.ultrasonic_worst_cell)
            self.assertIn("[100:2048]", budget.ultrasonic_p99_99_worst_window)
            self.assertEqual(len(frozen.calibration_digest), 64)

    def test_shadow_parity_compares_dumped_bitstream_digests(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)

            def parity_result(label: str, index: int, payload: bytes) -> experiment.RunResult:
                candidate = experiment.Candidate(
                    label, experiment.A1_MODULATOR, experiment._base_params(), role="calibration"
                )
                spec = experiment.RunSpec(candidate, index, root, Path("ecbeam2_quality"), 44_100)
                spec.candidate_dir.mkdir(parents=True)
                (spec.candidate_dir / "bits.dsd").write_bytes(payload)
                (spec.candidate_dir / "dsd_bitstreams.csv").write_text(
                    "filter,renderer_source_rate,dsd_rate,channel,file\n"
                    "SplitPhase,44100,DSD64,left,bits.dsd\n",
                    encoding="utf-8",
                )
                command = ["ecbeam2_quality", "--out", str(spec.candidate_dir)]
                return experiment.RunResult(spec, [command], "complete", 0, 1.0)

            off = parity_result("off", 0, b"same")
            shadow = parity_result("shadow", 1, b"same")
            expected = experiment.bitstream_digests(off)
            self.assertEqual(experiment.verify_shadow_a1_parity(off, shadow), expected)
            (shadow.spec.candidate_dir / "bits.dsd").write_bytes(b"different")
            with self.assertRaisesRegex(ValueError, "changed production A1"):
                experiment.verify_shadow_a1_parity(off, shadow)

    def test_winner_rule_uses_all_four_cells_and_material_threshold(self) -> None:
        baseline_candidate = experiment.Candidate(
            "ecbeam-a1-production", experiment.A1_MODULATOR, experiment._base_params(), baseline=True
        )
        active_candidate = experiment.Candidate(
            "passing", experiment.ECBEAM2_MODULATOR, experiment._active_params()
        )
        baseline_rows = {cell: matrix_row() for cell in experiment.EXPECTED_CELLS}
        active_rows = {
            cell: matrix_row(sinad=100.6, spur=21.6, hf=3.9, multitone=-3.6, score=12.0)
            for cell in experiment.EXPECTED_CELLS
        }
        winner, evaluations = experiment.choose_winner(
            [result(baseline_candidate, baseline_rows), result(active_candidate, active_rows, 1)]
        )
        self.assertIsNotNone(winner)
        self.assertTrue(evaluations[0]["eligible"])
        self.assertIn("worst_sinad_db", winner["material_wins"])

        active_rows[("MinimumPhase", 48_000)] = matrix_row(
            sinad=99.7, spur=21.6, hf=3.9, multitone=-3.6, score=12.0
        )
        winner, evaluations = experiment.choose_winner(
            [result(baseline_candidate, baseline_rows), result(active_candidate, active_rows, 1)]
        )
        self.assertIsNone(winner)
        self.assertTrue(any("protected regression" in item for item in evaluations[0]["failures"]))

    def test_material_gain_uses_worst_aggregate_but_requires_consistent_direction(self) -> None:
        baseline_candidate = experiment.a1_reference_candidate()
        active_candidate = experiment.Candidate(
            "aggregate", experiment.ECBEAM2_MODULATOR, experiment._active_params()
        )
        baseline_sinad = {
            ("MinimumPhase", 44_100): 100.0,
            ("SplitPhase", 44_100): 110.0,
            ("MinimumPhase", 48_000): 108.0,
            ("SplitPhase", 48_000): 109.0,
        }
        baseline_rows = {
            cell: matrix_row(sinad=value) for cell, value in baseline_sinad.items()
        }
        active_rows = {
            cell: matrix_row(sinad=value + (0.6 if value == 100.0 else 0.1))
            for cell, value in baseline_sinad.items()
        }
        winner, evaluations = experiment.choose_winner(
            [result(baseline_candidate, baseline_rows), result(active_candidate, active_rows, 1)]
        )
        self.assertIsNotNone(winner)
        self.assertAlmostEqual(evaluations[0]["aggregate_gains"]["worst_sinad_db"], 0.6)
        self.assertAlmostEqual(evaluations[0]["worst_cell_deltas"]["worst_sinad_db"], 0.1)

        active_rows[("SplitPhase", 44_100)] = matrix_row(sinad=109.9)
        winner, evaluations = experiment.choose_winner(
            [result(baseline_candidate, baseline_rows), result(active_candidate, active_rows, 1)]
        )
        self.assertIsNone(winner)
        self.assertNotIn(
            "worst_sinad_db", evaluations[0]["directionally_consistent_metrics"]
        )

    def test_overload_limiter_and_missing_diagnostics_are_hard_failures(self) -> None:
        baseline_candidate = experiment.a1_reference_candidate()
        active_candidate = experiment.Candidate(
            "health", experiment.ECBEAM2_MODULATOR, experiment._active_params()
        )
        baseline_rows = {cell: matrix_row() for cell in experiment.EXPECTED_CELLS}
        active_rows = {
            cell: matrix_row(sinad=101.0, spur=22.0, hf=3.0, multitone=-4.0)
            for cell in experiment.EXPECTED_CELLS
        }
        active_rows[("MinimumPhase", 44_100)] = matrix_row(
            sinad=101.0,
            spur=22.0,
            hf=3.0,
            multitone=-4.0,
            overload=-59.7,
            limiter_events=1,
            include_ecbeam2_diagnostics=False,
        )
        winner, evaluations = experiment.choose_winner(
            [result(baseline_candidate, baseline_rows), result(active_candidate, active_rows, 1)]
        )
        self.assertIsNone(winner)
        failures = ";".join(evaluations[0]["failures"])
        self.assertIn("overload_recovery_db", failures)
        self.assertIn("limiter_limited_events=1", failures)
        self.assertIn("missing ecbeam2_constraint_escape diagnostic", failures)

    def test_general_harness_quality_rejection_is_not_campaign_health_failure(self) -> None:
        baseline_candidate = experiment.a1_reference_candidate()
        rows = {cell: matrix_row() for cell in experiment.EXPECTED_CELLS}
        for row in rows.values():
            row["status"] = "reject"
            row["hard_failure_count"] = "4"
            row["hard_failures"] = "broad-harness absolute quality threshold"

        failures = experiment.hard_failure_reasons(
            result(baseline_candidate, rows),
            require_active_survivor_diagnostics=False,
        )

        self.assertEqual(failures, [])

    def test_missing_density_and_unhealthy_a1_reject_an_otherwise_passing_row(self) -> None:
        baseline_candidate = experiment.a1_reference_candidate()
        active_candidate = experiment.Candidate(
            "health", experiment.ECBEAM2_MODULATOR, experiment._active_params()
        )
        baseline_rows = {cell: matrix_row() for cell in experiment.EXPECTED_CELLS}
        baseline_rows[("MinimumPhase", 44_100)]["state_clamps"] = "1"
        active_rows = {
            cell: matrix_row(sinad=101.0, spur=22.0, hf=3.0, multitone=-4.0)
            for cell in experiment.EXPECTED_CELLS
        }
        active_rows[("SplitPhase", 48_000)].pop("bit_density_max_deviation")
        winner, evaluations = experiment.choose_winner(
            [result(baseline_candidate, baseline_rows), result(active_candidate, active_rows, 1)]
        )
        self.assertIsNone(winner)
        failures = ";".join(evaluations[0]["failures"])
        self.assertIn("missing bit_density_max_deviation", failures)
        self.assertIn("A1 baseline health", failures)
        self.assertIn("state_clamps=1", failures)

    def test_available_existing_quality_inputs_are_protected_per_cell(self) -> None:
        baseline_candidate = experiment.a1_reference_candidate()
        active_candidate = experiment.Candidate(
            "protected", experiment.ECBEAM2_MODULATOR, experiment._active_params()
        )
        baseline_rows = {cell: matrix_row() for cell in experiment.EXPECTED_CELLS}
        active_rows = {
            cell: matrix_row(sinad=101.0, spur=22.0, hf=3.0, multitone=-4.0)
            for cell in experiment.EXPECTED_CELLS
        }
        cell = ("MinimumPhase", 44_100)
        baseline_rows[cell]["high_freq_worst_spur_dbfs"] = "-90.0"
        active_rows[cell]["high_freq_worst_spur_dbfs"] = "-89.7"
        winner, evaluations = experiment.choose_winner(
            [result(baseline_candidate, baseline_rows), result(active_candidate, active_rows, 1)]
        )
        self.assertIsNone(winner)
        self.assertTrue(
            any(
                "protected regression" in failure
                and "high_freq_worst_spur_dbfs" in failure
                for failure in evaluations[0]["failures"]
            )
        )

    def test_reconstruction_energy_is_same_run_tie_break(self) -> None:
        baseline_candidate = experiment.a1_reference_candidate()
        baseline_rows = {
            cell: matrix_row(reconstruction_energy=1.0)
            for cell in experiment.EXPECTED_CELLS
        }
        candidates = []
        for index, (label, energy) in enumerate(
            (("higher", 0.9), ("lower", 0.8)), start=1
        ):
            candidate = experiment.Candidate(
                label, experiment.ECBEAM2_MODULATOR, experiment._active_params()
            )
            rows = {
                cell: matrix_row(
                    sinad=101.0,
                    spur=22.0,
                    hf=3.0,
                    multitone=-4.0,
                    reconstruction_energy=energy,
                )
                for cell in experiment.EXPECTED_CELLS
            }
            candidates.append(result(candidate, rows, index))
        winner, _ = experiment.choose_winner(
            [result(baseline_candidate, baseline_rows), *candidates]
        )
        self.assertEqual(winner["candidate_label"], "lower")

    def test_hard_ecbeam2_counter_rejects_candidate(self) -> None:
        baseline_candidate = experiment.Candidate(
            "ecbeam-a1-production", experiment.A1_MODULATOR, experiment._base_params(), baseline=True
        )
        active_candidate = experiment.Candidate(
            "escape", experiment.ECBEAM2_MODULATOR, experiment._active_params()
        )
        baseline_rows = {cell: matrix_row() for cell in experiment.EXPECTED_CELLS}
        active_rows = {
            cell: matrix_row(sinad=101.0, spur=22.0, hf=3.0, multitone=-4.0, score=12.0)
            for cell in experiment.EXPECTED_CELLS
        }
        active_rows[("SplitPhase", 44_100)] = matrix_row(
            sinad=101.0,
            spur=22.0,
            hf=3.0,
            multitone=-4.0,
            notes="ecbeam2_constraint_escape=1",
        )
        winner, evaluations = experiment.choose_winner(
            [result(baseline_candidate, baseline_rows), result(active_candidate, active_rows, 1)]
        )
        self.assertIsNone(winner)
        self.assertTrue(any("constraint_escape=1" in item for item in evaluations[0]["failures"]))


if __name__ == "__main__":
    unittest.main()
