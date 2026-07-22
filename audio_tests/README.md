# Audio development checks

The retained audio checks cover the public production-quality bench, focused
EcBeam2 oracle checks, functional smoke coverage, and performance measurement.

## Public PCM-to-DSD quality

The fixed synthetic 14-cell v7 matrix runs without network access or external
media. It scores the production-default Split Phase path and writes JSON plus
Markdown:

```sh
RUSTFLAGS="-C target-cpu=native" \
cargo run --locked --release --bin dsd_public_quality -- \
  --out target/dsd-public-quality \
  --check
```

`SplitPhase128kE3`, exposed in the product as Split Phase, is the canonical
filter. A narrower modulator selection
records `matrix_complete: false` and cannot pass `--check`. Add
`--include-linear-reference` to run the 7-cell `LinearPhase128k` diagnostic. The
diagnostic never affects canonical completeness, structural checking, or the
production-path scores.

The public fixtures can exercise a focused Split Phase E3 subset:

```sh
RUSTFLAGS="-C target-cpu=native" \
cargo run --locked --release --bin dsd_public_quality -- \
  --out target/dsd-public-quality-e3-dsd64-dsd128 \
  --filter SplitPhase128kE3 \
  --rates 64,128 \
  --modulator Standard,EcBeam2
```

Any reduced `--rates` or `--modulator` selection is noncanonical, reports
`matrix_complete: false`, and emits no production-path score.

Version 7 reports also serialize a fixed 2 ms restart-error envelope over
0-50 ms. `--transition-envelope-reference PATH` compares each stress channel
with a frozen report using linear-power positive excess, and
`--transition-envelope-tolerance-rms` supplies the frozen numerical tolerance.
The existing percentile-derived first-crossing recovery time remains a
secondary diagnostic.

The optional `research-filter-assets` Cargo feature adds a Split Phase B-only,
hash-verified `--experimental-character-file` loader for offline filter work.
It is not compiled into ordinary product or canonical bench builds and does
not replace the frozen cleanup or rational assets.

Add `--include-rate-comparison` for non-scoring DSD128 hi-res cells. This runs
the same 176.4 kHz four-carrier fixture used at DSD256, allowing a direct raw
DSD128/DSD256 comparison without changing the canonical matrix or scores.

The executable binds the result to its release/native build configuration and
source snapshot; setting `RUSTFLAGS` only when launching an old binary does not
satisfy that contract. The versioned 100-point presentation is explicitly a
Split Phase E3 production-path comparison, not a `--check` quality gate or a
listening score. The checked-in baseline remains the historical 26-cell v4
result from before EcBeam2 gained DSD256 qualification.
The full methodology is documented in
[docs/dsd-public-quality.md](../docs/dsd-public-quality.md).

## EcBeam2 exact oracle

Build the retained exact-oracle tool with:

```sh
RUSTFLAGS="-C target-cpu=native" cargo build --release \
  --bin ecbeam2_exact_oracle
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

The lightweight integration test checks that both production modulators
complete the real native-DSD EOF path with exact output length and clean health
counters. EcBeam2 additionally covers every supported filter at DSD64 and
DSD128:

```sh
RUSTFLAGS="-C target-cpu=native" cargo test --release --test audio_smoke
```

Use the optimized native build for DSD integration tests. Debug builds do not
exercise the production SIMD path and can be disproportionately slow.
