from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any

import numpy as np

from .e3_p7_cleanup_search import _frequency_fft_length, _frequency_metrics
from .e3_p9_timing_search import _realize_support
from .e3_phase_search import _cascade_character_and_cleanup, _read_f64le


IDENTITY = "SplitPhase128kE3-P9-full-support-frequency-audit"


def _sha256_f64(values: np.ndarray) -> str:
    return hashlib.sha256(np.asarray(values, dtype="<f8").tobytes()).hexdigest()


def audit(root: Path, search_path: Path) -> dict[str, Any]:
    search = json.loads(search_path.read_text(encoding="utf-8"))
    feasibility_path = root / search["source"]["feasibility"]
    feasibility = json.loads(feasibility_path.read_text(encoding="utf-8"))
    phase_knots_hz = np.asarray(
        feasibility["contract"]["phase_knots_hz"], dtype=np.float64
    )
    e2_path = root / "assets/filters/split_phase_e2v3/character_full_rate.f64le"
    cleanup_path = root / "assets/filters/split_phase_e2v3/cleanup_stage_1.f64le"
    e2 = _read_f64le(e2_path)
    cleanup = _read_f64le(cleanup_path)
    baseline_response = _cascade_character_and_cleanup(e2, cleanup)
    supports = []
    for support_result in search["per_support"]:
        source = support_result["packet_safe"][0]
        support = int(support_result["support"])
        character, structural = _realize_support(
            e2,
            np.asarray(source["coordinates"], dtype=np.float64),
            support,
            phase_knots_hz,
        )
        response = _cascade_character_and_cleanup(character, cleanup)
        frequency = _frequency_metrics(response, baseline_response)
        passes = bool(
            frequency["maximum_passband_delta_db_0_18khz"] <= 1.0e-3
            and frequency["maximum_stopband_db_22k05_nyquist"] <= -150.0
            and frequency["maximum_transition_rebound_linear"] <= 1.0e-6
        )
        supports.append(
            {
                "support": support,
                "source_identifier": source["identifier"],
                "character_sha256": _sha256_f64(character),
                "response_sha256": _sha256_f64(response),
                "response_samples": int(response.size),
                "frequency_fft_length": _frequency_fft_length(
                    response.size, baseline_response.size
                ),
                "structural": structural,
                "frequency": frequency,
                "passes_frequency_gates": passes,
            }
        )
    reference = supports[0]["frequency"]
    for result in supports:
        result["frequency_delta_vs_262145"] = {
            key: float(result["frequency"][key] - reference[key]) for key in reference
        }
    return {
        "schema_version": 1,
        "identity": IDENTITY,
        "production_promoted": False,
        "source": {
            "search": str(search_path.relative_to(root)).replace("\\", "/"),
            "feasibility": str(feasibility_path.relative_to(root)).replace("\\", "/"),
            "e2_character_sha256": _sha256_f64(e2),
            "cleanup_sha256": _sha256_f64(cleanup),
        },
        "contract": {
            "maximum_passband_delta_db_0_18khz": 1.0e-3,
            "maximum_stopband_db_22k05_nyquist": -150.0,
            "maximum_transition_rebound_linear": 1.0e-6,
            "frequency_fft_covers_complete_response": True,
        },
        "supports": supports,
        "all_supports_pass": all(
            result["passes_frequency_gates"] for result in supports
        ),
    }


def main() -> None:
    parser = argparse.ArgumentParser(description="Audit P9 rejection at every support")
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[2]
    )
    parser.add_argument(
        "--search",
        type=Path,
        default=(
            Path(__file__).resolve().parent / "baselines/e3-p9-timing-search-dense.json"
        ),
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=(
            Path(__file__).resolve().parent
            / "work-e3-p9-support-frequency/e3_p9_support_frequency_audit.json"
        ),
    )
    arguments = parser.parse_args()
    report = audit(arguments.root.resolve(), arguments.search.resolve())
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(
        json.dumps(
            {
                "output": str(arguments.output),
                "all_supports_pass": report["all_supports_pass"],
                "supports": [
                    {
                        "support": result["support"],
                        **result["frequency"],
                    }
                    for result in report["supports"]
                ],
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
