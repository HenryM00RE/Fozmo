from __future__ import annotations

import argparse
import json
import re
import subprocess
import time
from pathlib import Path
from typing import Any


def capture(root: Path, output: Path) -> dict[str, Any]:
    commands = [
        ["cargo", "test", "--lib", "split_phase_v4", "--", "--nocapture"],
        ["cargo", "test", "--lib", "production_ecbeam2_renders_both_dsd128_wire_families", "--", "--nocapture"],
    ]
    history = []
    combined_output = ""
    for command in commands:
        started = time.perf_counter()
        result = subprocess.run(command, cwd=root, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        history.append({"command": command, "exit_code": result.returncode, "elapsed_seconds": time.perf_counter() - started, "output_tail": result.stdout[-20_000:]})
        combined_output += result.stdout
        if result.returncode != 0:
            break
    spectral = re.search(r"SPLIT_PHASE_V4_RUNTIME image_db=([-+0-9.eE]+) alias_db=([-+0-9.eE]+)", combined_output)
    rational_spectral = re.search(r"SPLIT_PHASE_V4_RATIONAL_RUNTIME image_db=([-+0-9.eE]+) alias_db=([-+0-9.eE]+)", combined_output)
    cost = re.search(r"SPLIT_PHASE_V4_COST operation_count_ratio=([-+0-9.eE]+) memory_ratio=([-+0-9.eE]+)", combined_output)
    runtime_metrics = None
    if spectral is not None:
        runtime_metrics = {
            "interpolation_image_db": float(spectral.group(1)),
            "independent_decimation_alias_db": float(spectral.group(2)),
        }
        if cost is not None:
            runtime_metrics["operation_count_ratio_to_c"] = float(cost.group(1))
            runtime_metrics["memory_ratio_to_c"] = float(cost.group(2))
        if rational_spectral is not None:
            runtime_metrics["rational_147_160_image_db"] = float(rational_spectral.group(1))
            runtime_metrics["rational_160_147_alias_db"] = float(rational_spectral.group(2))
    report = {
        "block_sizes": [1, 2, 3, 127, 255, 256, 257, 4095, 4096, 4097, "deterministic-random"],
        "paths": {
            "direct_convolution": "not selected by D's mandatory 262145-tap character support",
            "partitioned_fft_convolution": "measured",
            "integer_interpolation": "measured",
            "integer_decimation": "measured independently",
            "147_160_rational": "measured",
            "160_147_rational": "measured independently",
            "ecbeam2_dsd128_44_1_family": "measured",
            "ecbeam2_dsd128_48_family": "measured",
        },
        "state_cases": {
            "reset": "bit-stable test",
            "repeated_eof_drain": "idempotence test",
            "gapless_carry": "chunk-boundary invariance test",
            "stereo_equality": "complex stereo tone and independent-channel signal tests",
            "random_channel_content": "deterministic mixed-signal test",
            "long_session": "EcBeam2 production render plus rational frame-count coverage",
        },
        "commands": history,
        "measured_runtime_metrics": runtime_metrics,
        "accepted": bool(
            len(history) == len(commands)
            and all(item["exit_code"] == 0 for item in history)
            and runtime_metrics is not None
            and runtime_metrics["interpolation_image_db"] <= -145.0
            and runtime_metrics["independent_decimation_alias_db"] <= -145.0
            and runtime_metrics.get("rational_147_160_image_db", float("inf")) <= -145.0
            and runtime_metrics.get("rational_160_147_alias_db", float("inf")) <= -145.0
            and runtime_metrics.get("operation_count_ratio_to_c", float("inf")) <= 1.05
            and runtime_metrics.get("memory_ratio_to_c", float("inf")) <= 1.0
        ),
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n")
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--output", type=Path)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    print(json.dumps(capture(root, arguments.output or root / "tools/split_phase_v4/work/runtime_capture.json"), indent=2))
