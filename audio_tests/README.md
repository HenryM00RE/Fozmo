# Audio development checks

The retained audio checks cover the public production-quality bench, narrow
EcBeam2 research, functional smoke coverage, and performance measurement.

All EcBeam2 corpus inputs are generated from committed manifests. No commercial
music fixtures are stored in this repository.

## Public PCM-to-DSD quality

The fixed synthetic 28-cell v5 matrix runs without network access or external
media. It scores the production-default Split Phase path and writes JSON plus
Markdown:

```sh
RUSTFLAGS="-C target-cpu=native" \
cargo run --locked --release --bin dsd_public_quality -- \
  --out target/dsd-public-quality \
  --check
```

`SplitPhase128kE2v3`, exposed in the product as Split Phase, is the canonical
filter. A narrower modulator selection
records `matrix_complete: false` and cannot pass `--check`. Add
`--include-linear-reference` to run the legacy modulators' 21-cell
`SincExtreme32k` diagnostic; EcBeam2 does not support that filter. The
diagnostic never affects canonical completeness, structural checking, or the
production-path scores.

The public fixtures can exercise a focused Split Phase E2v3 subset:

```sh
RUSTFLAGS="-C target-cpu=native" \
cargo run --locked --release --bin dsd_public_quality -- \
  --out target/dsd-public-quality-e2v3-dsd64-dsd128 \
  --filter SplitPhase128kE2v3 \
  --rates 64,128 \
  --modulator Standard,EcBeam2
```

Any reduced `--rates` or `--modulator` selection is noncanonical, reports
`matrix_complete: false`, and emits no production-path score. The retired
`Split128k` implementation remains selectable only for non-scoring historical
comparisons.

Add `--include-rate-comparison` for non-scoring DSD128 hi-res cells. This runs
the same 176.4 kHz four-carrier fixture used at DSD256, allowing a direct raw
DSD128/DSD256 comparison without changing the canonical matrix or scores.

The executable binds the result to its release/native build configuration and
source snapshot; setting `RUSTFLAGS` only when launching an old binary does not
satisfy that contract. The versioned 100-point presentation is explicitly a
Split Phase E2v3 production-path comparison, not a `--check` quality gate or a
listening score. The checked-in baseline remains the historical 26-cell v4
result from before EcBeam2 gained DSD256 qualification.
The full methodology is documented in
[docs/dsd-public-quality.md](../docs/dsd-public-quality.md).

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

## Production filter timing

The reconstruction-filter timing bench measures the exact production runtime
with impulse, direct-step, and 5-20 kHz windowed-tone-packet probes:

```sh
RUSTFLAGS="-C target-cpu=native" \
cargo run --locked --release --bin filter_timing_bench
```

It writes Markdown, JSON, and a long-form group-delay CSV under
`target/filter-timing` by default. See
[the measurement contract](../docs/filter-timing-bench.md) for definitions and
controls.

## Functional smoke coverage

The lightweight integration test checks that all four production modulators
complete the real native-DSD EOF path with exact output length and clean health
counters. EcBeam2 additionally covers all three supported filters at DSD64 and
DSD128:

```sh
RUSTFLAGS="-C target-cpu=native" cargo test --release --test audio_smoke
```

Use the optimized native build for DSD integration tests. Debug builds do not
exercise the production SIMD path and can be disproportionately slow.
