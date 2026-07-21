# Public PCM-to-DSD measurement bench

Fozmo includes a deterministic, source-free measurement bench for the
production PCM-to-DSD path. It generates stereo PCM in memory, renders it
through the real `DsdRenderer` and normal EOF flush, reconstructs the native
one-bit output with a fixed measurement decoder, and writes detailed JSON plus
a readable Markdown report.

The bench measures digital behavior that can be reproduced from a clean
checkout: linearity, distortion, noise, discrete spurs, idle behavior,
high-frequency intermodulation, transition recovery, state health, and
wideband reconstruction through 70 kHz. It also presents a versioned
production-path comparison score. That score is not a listening score and does
not replace measurements from a real DAC and analog output filter.

## Canonical run

The canonical result must be built in release mode for the native CPU. This is
a correctness requirement, not just a speed recommendation: some production
SIMD paths are selected only by an optimized native build.

```sh
RUSTFLAGS="-C target-cpu=native" \
cargo run --locked --release --bin dsd_public_quality -- \
  --out target/dsd-public-quality \
  --check
```

The bench verifies build-time evidence embedded in the executable. Setting
`RUSTFLAGS` only when launching an already-built binary is not sufficient. A
canonical executable must record release profile, optimization level 3,
disabled debug assertions, `target-cpu=native` without an explicit feature
disable, and the repository's canonical default Cargo feature set; its runtime
source-snapshot hash must match the source snapshot captured when it was built.
The report also records the build and runtime Git state, compiler, host and
target, encoded flags and features, CPU class, source digest, and executable
SHA-256.

The output directory receives:

- `dsd-public-quality.json`, containing both channels, every section metric,
  renderer identity and health, exact output lengths, fixture/native hashes,
  matrix-completeness state, and build provenance;
- `dsd-public-quality.md`, preserving scenario-specific measurement meanings
  rather than collapsing unrelated sections into a single ranking row.

The CLI is intentionally narrow:

```text
--out PATH
--modulator Standard,EcDepth2,EcBeam,EcBeam2
--filter Split128k|SplitPhase128kE2v3
--rates 64,128,256,512,1024
--check
--include-linear-reference
--hires-only
--standard-obg OBG
--level-probe-dbfs LEVEL
--transition-envelope-reference PATH
--transition-envelope-tolerance-rms RMS
--external-upsampler PATH
--external-preset PRESET
```

`SplitPhase128kE2v3` is the canonical product filter exposed as Split Phase.
The retired `Split128k` implementation remains available only as a non-scoring
historical comparison. The default three rates and four modulators form the
complete matrix. Narrower rate or modulator selections and every retired-filter
selection are useful for investigation, but always produce
`matrix_complete: false`. With `--check`, an incomplete matrix returns a
failure status even when every attempted cell is structurally healthy. A
partial run must never be described as a canonical pass.

DSD512 and DSD1024 selections are non-scoring Standard-only diagnostics.
`--standard-obg` selects a checked-in high-rate Standard candidate table, and
`--level-probe-dbfs` restricts a tuning run to one declared coherent level.
Neither option changes the canonical matrix or application UI.

## Fixed-reference restart envelope

Report schema v5 keeps the existing first-crossing recovery time but demotes
it to a secondary diagnostic. For every high-frequency stress channel, the
bench subtracts the settled carrier model and serializes a 2 ms sliding
mean-square trace aligned to restart over the first 50 ms. It reports residual
energy, maximum RMS, and 95th-percentile RMS in fixed 0-2, 2-5, 5-10, 10-25,
and 25-50 ms intervals.

When `--transition-envelope-reference` names a frozen v5 report, the bench
requires a matching scenario/modulator/channel and computes maximum and
integrated positive excess in linear power. The optional RMS tolerance is
squared before subtraction. Reference contract/version mismatches and missing
cells fail closed. Decibels are presentation only; optimizer inputs remain
linear power.

`research-filter-assets` is a non-production Cargo feature for exact E3
candidate reruns. It adds `--experimental-character-file` and
`--experimental-character-sha256`, requires the E3 filter, validates exact
length, finite binary64 values, SHA-256, and DC normalization, and records the
loaded identity in JSON and Markdown. Normal builds expose neither flag.

For example, a focused Split Phase check across DSD64 and DSD128 is:

```sh
RUSTFLAGS="-C target-cpu=native" \
cargo run --locked --release --bin dsd_public_quality -- \
  --out target/dsd-public-quality-e2v3-dsd64-dsd128 \
  --filter SplitPhase128kE2v3 \
  --rates 64,128 \
  --modulator Standard,EcBeam2
```

`--include-linear-reference` adds the legacy modulators' 21 cells using
`SincExtreme32k` as an explicitly non-scoring diagnostic. EcBeam2 does not
support that filter, so no diagnostic EcBeam2 cell is invented. Diagnostic
cells cannot make the canonical matrix complete or incomplete, affect scores,
or affect the canonical `--check` result.

## External-product EC DSD128 diagnostic

`--external-upsampler` adds an unnamed external-product DSD128 diagnostic
alongside ECBeam2. It requires `--rates 128` and `EcBeam2` in `--modulator`.
The adapter writes the canonical fixtures as stereo float32 WAV, invokes the
offline executable with its undocumented EC selector, validates a
native stereo 5.6448 MHz DSF, reverses its LSB-first bytes to the bench's
canonical MSB-first representation, and analyzes those bits with the same
reconstruction and metrics as Fozmo.

```powershell
$env:RUSTFLAGS = "-C target-cpu=native"
cargo run --locked --release --bin dsd_public_quality -- `
  --out target/external-product-ec-vs-ecbeam2-dsd128 `
  --modulator EcBeam2 `
  --rates 128 `
  --external-upsampler "C:\path\to\upsampler.exe" `
  --external-preset megaextreme
```

Before running cells, the adapter renders an impulse twice to require native
payload determinism and to measure the external product's source-to-bit timing offset. A
separate settled 1 kHz render measures its fixed input calibration; the raw
calibration is recorded as `coefficient_input_peak` and applied using the same
decoder convention as Fozmo's declared modulator input peak.

The external identity is reported as `ExternalProductEcInferred`: enabling its
vendor-specific selector changes only the deterministic DSD payload, while the
disabled selector matches the default, but the CLI does not print an explicit EC
confirmation. The external product also exposes no internal reset/clamp/limiter telemetry,
so external structural health covers observable file, density, reconstruction,
and transition gates only. External cells are always diagnostic and cannot
affect canonical completeness, scores, or `--check`.

`--check` otherwise gates structural invariants. The presentation score is not
a quality gate, and a structural `PASS` is not a claim that one modulator
sounds better than another.

## Fixed production matrix

The canonical v6 result contains 28 production cells, all using Fozmo's
declared default Split Phase path (`SplitPhase128kE2v3`). It evaluates the configured
product chain rather than claiming to isolate an abstract modulator algorithm.

| Scenario | Conversion | Cells | Purpose |
| --- | --- | ---: | --- |
| Coherent level sweep | 44.1 kHz to supported DSD64, DSD128, and DSD256 rates | 12 | Linearity, SINAD, THD, residual noise, spurs, and rate scaling |
| Idle and tiny signal | 44.1 kHz to DSD64 | 4 | Idle tones, tiny DC accuracy, low-level resolution, and density bias |
| High-frequency rated stress | 44.1 kHz to DSD128 | 4 | Behavior at each modulator's declared rated input |
| High-frequency level-matched stress | 44.1 kHz to DSD128 | 4 | Direct modulator comparison at one effective peak |
| Hi-res reconstruction | 176.4 kHz to DSD256 | 4 | Per-carrier accuracy through 70 kHz and split-band residuals |

The bench retains `Standard`, `EcDepth2`, `EcBeam`, and `EcBeam2` as technical
comparison identities. The application exposes only `Standard` as 7th Order
and `EcBeam2` as 7th Order Search; the other algorithms remain available for
diagnostics. EcBeam2 supports DSD64, DSD128, and DSD256, with every rate included
in the canonical matrix and score. Its OBG1.64 production tables retain their
rate-specific NTFs and use the same calibrated `0.4678589885194707` input peak;
their hard-state envelopes scale proportionally with that calibration. Filter
identity can change a modulator's effective production policy, which is why the
public score measures only the actual default product path.

Every cell records its comparison class, whether its levels are matched across
modulators, and whether it exercises the production-default filter. It also
records the selected coefficient table, OSR, out-of-band gain, coefficient
input peak, lookahead depth, ISI penalty, chunk size, fixed channel seeds, and
effective policy identity. These fields make policy changes visible instead of
silently comparing differently configured renderers.

The rated headroom is:

| Modulator | Rated headroom |
| --- | ---: |
| Standard | -4 dB |
| EcDepth2 | -4 dB |
| EcBeam | -2 dB |
| EcBeam2 | -2 dB |

Level-controlled fixtures compensate their source amplitude so the
post-headroom modulator input has the declared effective level. Rated stress
instead preserves each path's rated source peak. The distinct level-matched
stress fixture compensates headroom so all four modulators see the same
effective peak. Rated and matched stress results are intentionally not merged.

## Split Phase E2v3 production-path score

The score is named `dsd-public-production-score-v3` and must be described as:

> Fozmo PCM-to-DSD production-path score using Split Phase E2v3

It evaluates synthetic PCM through the Split Phase E2v3 production upsampler, the
selected modulator and its production policy, native DSD, and the fixed
measurement decoder. Scores are emitted only when all 28 canonical cells
complete with zero canonical structural failures.

There is one 100-point score per DSD rate:

| Rate | Category | Points |
| --- | --- | ---: |
| DSD64 | Coherent level sweep | 60 |
| DSD64 | Idle, tiny DC and tiny tone | 40 |
| DSD128 | Coherent level sweep | 35 |
| DSD128 | Level-matched stress spectral quality | 40 |
| DSD128 | Mute/restart transition quality | 25 |
| DSD256 | Coherent level sweep | 35 |
| DSD256 | Hi-res reconstruction through 70 kHz | 65 |

Rated DSD128 stress remains a structural qualification and awards no ranking
points because the modulators have different rated headroom. Only the matched
effective-peak stress fixture contributes to the score.

Each category first forms a published dB-domain quality index from its raw
metrics. Level sweeps combine SINAD, residual, unexpected spur, and carrier
gain accuracy. Idle combines noise, spurs, relative DC accuracy, and tiny-tone
gain accuracy. Stress combines conventional SINAD, carrier gain, declared
products, product-excluded residual, and unexpected spurs. Transition combines
mute/restart residual levels and recovery time. Hi-res combines carrier gain
with residual and spurs in the 0-20 kHz and 20-80 kHz bands. Exact inner
weights are serialized in `score_policy.categories`.

The v3 policy retains the reviewed historical Split128k category anchors while
scoring the promoted Split Phase E2v3 path. A category receives
100 normalized points at or above its anchor; each average decibel below the
anchor removes one normalized point, clamped to 0-100, before the category
weight is applied. Relative-error rejection terms are capped at 100 dB so
floating-point-scale gain differences cannot dominate a score. The raw quality
index, anchor, normalized score, awarded points, and formula all remain in the
JSON and Markdown rather than exposing only one opaque number.

## Deterministic fixtures

Frequencies are the nearest exact analysis bin and the actual generated value
is stored in the report. Source PCM and each channel's native DSD output are
hashed per cell.

Each analyzed interval is held constant on both sides for the selected
upsampler's complete source-domain support. Linear Phase cells use 16,512
source frames (the 32,769-tap first-stage half-support plus cleanup-stage
margin). Split Phase cells use 131,328 frames to cover the 131,073-tap support
plus cleanup margin. The chosen value is recorded as
`filter_guard_source_frames` in every cell.

### Coherent level sweep

One continuous 44.1 kHz sequence contains four approximately 1 kHz sections
at effective -6, -20, -60, and -100 dBFS. Each section has a complete filter
guard, exactly 16,384 analyzed source frames, and another complete guard before
the next transition. Reconstruction at 176.4 kHz therefore yields 65,536
analyzed frames per section.

Results are retained per level and channel. Carrier gain, SINAD, five-harmonic
THD, residual noise, unexpected spur, DC, density, and reconstructed peak are
not combined as though their worst values came from one operating point. Gain
error is printed with enough precision to distinguish sub-millidecibel values.

### Idle and tiny signal

One DSD64 sequence contains digital silence, +1e-6 effective DC on the left
and -1e-6 on the right, and an approximately 100 Hz tone at -120 dBFS. Each
section has complete pre/post filter guards around 16,384 analyzed frames.
The report includes expected and reconstructed DC, signed DC error and polarity,
integrated 20 Hz-20 kHz noise, unknown spurs, low-level carrier recovery,
bit-density behavior, and explicit left/right spread.

### High-frequency rated and level-matched stress

Both DSD128 stress fixtures use coherent carriers near 18 and 19 kHz with fixed
phases. The rated fixture normalizes the combined source peak to 0.999 before
applying each modulator's rated headroom. The matched fixture instead targets
the common effective peak reached by a 0.999 source peak after -4 dB headroom.

Each fixture contains a settled steady window, full-filter transition guards,
a clean 2,048-frame zero-input center, a phase-reversed restart, and a guarded
recovery window. The zero-input interval includes complete source-filter guards
on both sides of that clean center, distinguishing filter transition
contamination from settled mute behavior.

A conventional two-tone SINAD treats only the two desired carriers as signal.
The difference and third-order IMD products remain distortion and therefore
remain in the SINAD denominator. The JSON separately publishes:

- each declared difference/IMD product and the worst declared product;
- integrated residual after removing carriers and declared products;
- the worst unexpected spur;
- the corresponding steady and recovered measurements.

Unknown spurs are found after jointly fitting and subtracting declared
carriers/products plus DC, then applying a four-term Blackman-Harris window and
integrating over its main lobe. Removing the declared waveform prevents its
window sidelobes from becoming the unknown-spur floor. Exact-bin rectangular
measurements remain limited to deliberately coherent carriers and declared
products. The combined approach also avoids losing a noncoherent line that
falls between FFT bins. As with any finite-record subtraction, a second line
too close to resolve from a declared frequency is absorbed by that fit; known
products therefore have to be declared explicitly.

Transition reporting fits the expected two-tone rather than calling the
resumed program peak a transient. It retains the settled program peak,
transition waveform peak, excess above the settled envelope, separate mute and
restart residual peaks, clean-center mute peak and RMS, restart-residual RMS
over fixed 1, 10, and 50 ms windows, and nullable end-to-end recovery time.
The full zero-input transition peak is kept separate from the clean 2,048-frame
center so filter ring-out is not mislabeled as settled mute noise. Recovery
means a sustained return to the pre-mute residual range. Timing includes the
production and measurement filters, so it describes the complete digital path
rather than claiming to isolate modulator state.

### Hi-res reconstruction

The 176.4 kHz fixture uses coherent carriers near 1, 18, 40, and 70 kHz with
fixed phases. Their combined effective peak is -6 dBFS. DSD256 preserves the
88.2 kHz source Nyquist band.

The report stores gain error for every carrier, residual energy and unknown
spurs from 0-20 kHz and 20-80 kHz separately, fixed-time density, reconstructed
peak, and state health. A single wideband SINAD number is intentionally not
used.

Left and right normally receive identical samples so the renderer's independent
fixed channel seeds provide two repeatable realizations in one render. The
tiny-DC section intentionally uses opposite channel polarity.

## Measurement decoder and metric conventions

The v2 decoder is excluded from playback and is identical for every production
modulator. Its algorithm identifier is stored in every report. Native MSB-first
DSD is downsampled with the fixed 32,769-tap Linear Phase resampler, then a
zero-phase FFT-domain raised-cosine response is applied and the requested center
interval is cropped back to its exact length.

Decoder context is derived from the resampler's reported latency, with a 100 ms
minimum and an additional safety margin. The raised-cosine stage retains its
own 50 ms reflected context. This ensures a requested analysis window is not
silently reconstructed with zero-substituted samples inside the long
downsampler's settled support.

The two reported response profiles remain:

| Profile | Output | Passband | Transition | Stopband |
| --- | ---: | ---: | ---: | ---: |
| `reconstruction-v1-audio-20k-24k` | 176.4 kHz | 0-20 kHz | 20-24 kHz | 24 kHz and above |
| `reconstruction-v1-hires-80k-96k` | 352.8 kHz | 0-80 kHz | 80-96 kHz | 96 kHz and above |

At the FFT grid the final response is unity in the passband and zero in the
stopband. Response and boundary tests enforce passband preservation, stopband
rejection, context sufficiency, and exact output length.

The renderer maps PCM full scale to the selected coefficient table's fixed
`input_peak` before modulation. The decoder divides by that declared constant
to return measurements to the post-headroom PCM contract. It is not fitted to
the rendered result.

All absolute spectral levels use an explicit full-scale-sine reference:

- carrier levels are peak-sine dBFS;
- a spur is reported as equivalent peak-sine dBFS;
- integrated residual/noise power is normalized so a full-scale sine is
  0 dBFS.

FFT band power uses the sum of squared window coefficients, while known-tone
amplitude uses integrated tone-bin power. The report retains window and
integration semantics in its measurement version rather than treating every
quantity as a generic `Spur dBFS` or `Residual dBFS`.

Every spectral interval contains 65,536 reconstructed frames. Each cell records
the resulting bin width for its reconstruction rate. The report also publishes
the exact-bin rectangular policy for coherent lines, the ±6-bin integration
width used for unknown Blackman-Harris spurs, and that window's nominal
2.0044-bin equivalent noise bandwidth.

Density uses a fixed 20 ms rolling duration at every DSD rate; the report stores
the resulting bit count. Whole-fixture density remains a structural sanity
check. Within analyzed sections, raw density-bias hard gates are reserved for
idle silence, the guarded stress clean-mute center, and known DC, where a
deviation from 50% has a clear meaning. AC sections still report density, but
signal-derived partial-cycle variation is not treated as a comparable DC-bias
failure.

## Structural hard gates

The JSON always records diagnostics. With `--check`, any of the following
returns a non-zero status:

- the selected matrix is not the complete canonical 28-cell Split Phase E2v3 production matrix;
- the executable is not a binary-bound release/native build or its runtime
  source snapshot differs from its build snapshot;
- source samples are non-finite or exceed full scale;
- native output before idle padding is not exactly
  `source_frames * wire_rate / source_rate` bits per channel;
- idle flush changes the byte count for these byte-aligned fixtures;
- reconstructed length is wrong or a measurement is non-finite;
- stability resets, committed state clamps, limiter events, truncation, or
  discarded channel bits are nonzero;
- EcBeam commits a clamped survivor or rejects every child;
- EcBeam2 records a constraint escape, repair, non-finite reset,
  desynchronization, input substitution, or output-length event;
- whole-fixture density, or idle-silence/stress-clean-mute/DC whole-window or
  fixed-time rolling density, exceeds its declared deviation allowance;
- reconstructed absolute peak exceeds 1.05.

Noncommitted EcBeam survivor clamps, speculative candidate clamps, and rejected
candidates are reported but are not structural failures.

## Baselines and interpretation

The checked-in [baseline JSON](measurements/dsd-public-baseline.json) and
[readable report](measurements/dsd-public-baseline.md) are generated by the
canonical native release build. They preserve the historical v4
report/measurement contract and 26-cell matrix from before EcBeam2 DSD256
qualification; new canonical runs use the v5 28-cell matrix and
production-score v2.

Future quality thresholds should retain both an absolute engineering bound and
a reviewed margin from a complete compatible baseline. Timing is metadata only
and never affects the quality verdict.

Results may differ slightly across CPU architectures because native SIMD and
fused math can change a chaotic one-bit sequence. Comparisons should use the
reported scenario-specific metrics and margins rather than requiring
byte-identical DSD across machines. The bench also does not measure ultrasonic
noise above its reconstruction bands, music-dependent noise modulation, or the
analog response of a DAC.
