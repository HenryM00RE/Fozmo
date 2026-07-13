# Audio development checks

The retained audio checks have two purposes: EcBeam2 research and performance
measurement. Historical production-modulator tuning and subjective quality
harnesses were retired after tuning and QA were completed.

All EcBeam2 corpus inputs are generated from committed manifests. No commercial
music fixtures are stored in this repository.

## EcBeam2

Build the narrow EcBeam2 qualification CLI and exact oracle:

```sh
RUSTFLAGS="-C target-cpu=native" cargo build --release \
  --bin ecbeam2_quality \
  --bin ecbeam2_exact_oracle \
  --features ecbeam2_observer
```

The bounded campaign driver owns calibration, stability, budget, selection, and
held-out phases:

```sh
python3 tools/dsd64_ecbeam2_experiment.py \
  --phase calibration \
  --dry-run \
  --out audio_tests/out/ecbeam2-calibration-dryrun
```

Committed manifests live in `audio_tests/ecbeam2/manifests/`. The native
qualification implementation is isolated under `audio_tests/ecbeam2/` and is
not a general-purpose production tuning surface.

Run its orchestration tests with:

```sh
PYTHONPATH=tools python3 -m unittest tools/test_dsd64_ecbeam2_experiment.py
```

## Performance

The macOS performance wrapper records host metadata and runs the DSD renderer,
modulator, and resampler benchmarks:

```sh
tools/dsd-perf-mac.sh
```

Generated output belongs under `audio_tests/out/`, which is ignored by Git.

## Functional smoke coverage

The lightweight integration test checks that retained production modulators
render native DSD without resets:

```sh
RUSTFLAGS="-C target-cpu=native" cargo test --release --test audio_smoke
```

Use the optimized native build for DSD integration tests. Debug builds do not
exercise the production SIMD path and can be disproportionately slow.
