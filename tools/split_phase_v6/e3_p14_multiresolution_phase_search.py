from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np

from .e3_p12_gaussian_phase_search import (
    DEFAULT_GAUSSIAN_SPECIFICATIONS,
    SEARCH_STATIC_GATES,
    multiresolution_phase_specifications,
    optimize,
)


IDENTITY = "SplitPhase128kE3-P14-multiresolution-packet-aware-phase-search"

# Exact P12 post-lobe finalist used to seed the higher-capacity basis.  P14's
# added directions start at zero, so iteration zero reproduces this anchor.
P12_POST_LOBE_CONTROLS = np.asarray(
    (
        0.035726576169425719,
        -0.075442835482190079,
        -0.099004064805879088,
        -0.15788700058905197,
        -0.10980822162568969,
        -0.016119547765152641,
        0.030169549138185044,
        0.22079579938003721,
        -0.65693329758106567,
        -0.18133545985172173,
        -0.23135765206379294,
        0.065713508170154145,
        -0.055727927319005971,
        -0.0095717813643160163,
        -0.2154599229978818,
        0.081900771437853903,
        1.8840882345480521,
        -0.50096378848008294,
        -0.10559014735129413,
        -0.061089865958671698,
        -0.15805117581348582,
        -0.13590704812955462,
        -0.10704421716345079,
        -0.08474938626808512,
        -0.12054167790783003,
        -0.18683726431206327,
        -0.25872675056672134,
        -0.34361177973574653,
        -0.40094614299672604,
        -0.38878635676576678,
        -0.17985694795447474,
        0.046743930845327554,
        0.66138234641870297,
        1.4636416891492132,
        -1.0531264402183613,
        0.27569081182779176,
        0.73737431729010761,
        0.71764594301177642,
        0.4797882271773577,
        0.21487916694001216,
        -0.043862787335352961,
        -0.18034977581164607,
        -0.23883432450565473,
        -0.26333509144912765,
        -0.27276085732629801,
        -0.26739635388319977,
        -0.22946994199911686,
        -0.10663709235491349,
        0.037701103588267532,
        0.31164685323010899,
        0.56348609947884809,
    ),
    dtype=np.float64,
)


def initial_controls() -> tuple[tuple[tuple[float, float], ...], np.ndarray]:
    specifications = multiresolution_phase_specifications()
    index = {item: position for position, item in enumerate(specifications)}
    controls = np.zeros(len(specifications), dtype=np.float64)
    for specification, value in zip(
        DEFAULT_GAUSSIAN_SPECIFICATIONS,
        P12_POST_LOBE_CONTROLS,
        strict=True,
    ):
        controls[index[specification]] = value
    return specifications, controls


def search(root: Path, output_dir: Path, iterations: int = 4_000):
    specifications, controls = initial_controls()
    static_gates = dict(SEARCH_STATIC_GATES)
    static_gates["post_energy_db_total"] = -2.70
    return optimize(
        root,
        output_dir,
        iterations=iterations,
        profile_names=("multiresolution",),
        initial_controls=controls,
        basis_specifications=specifications,
        search_static_gates=static_gates,
        candidate_prefix="p14",
        report_filename="e3_p14_multiresolution_phase_search.json",
        identity=IDENTITY,
        learning_rate=0.0012,
        regularization=5.0e-5,
        learning_rate_milestones=(0.5, 0.75, 0.9),
    )


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Run the E3 P14 multiresolution phase search"
    )
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[2]
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path(__file__).resolve().parent / "work-e3-p14/multiresolution",
    )
    parser.add_argument("--iterations", type=int, default=4_000)
    arguments = parser.parse_args()
    report = search(
        arguments.root.resolve(), arguments.output_dir.resolve(), arguments.iterations
    )
    print(
        json.dumps(
            {
                "output": str(
                    arguments.output_dir
                    / "e3_p14_multiresolution_phase_search.json"
                ),
                "exact_qualified_count": report["exact_qualified_count"],
                "best_qualified_by_profile": report["best_qualified_by_profile"],
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
