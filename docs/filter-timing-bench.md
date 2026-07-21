# Production filter timing bench

The timing bench measures the exact `SincResampler` implementation used by
Fozmo rather than analyzing design-time coefficient prototypes. It covers the
four filters currently exposed by the product, plus explicitly labelled
experimental candidates:

- Linear Phase (`LinearPhase128k`)
- Minimum Phase (`MinimumPhaseCompact128k`)
- Split Phase (`SplitPhase128kE2v3`)
- Split Phase E3 (`SplitPhase128kE3`, experimental and not the default)
- Smooth Phase (`SmoothPhase128k`)

Run the canonical 44.1 kHz to 176.4 kHz comparison with an optimized build:

```sh
RUSTFLAGS="-C target-cpu=native" \
cargo run --locked --release --bin filter_timing_bench
```

The default output directory is `target/filter-timing`. The bench writes:

- `report.md`: compact impulse, step, packet, and group-delay tables
- `report.json`: complete machine-readable results and measurement controls
- `group-delay.csv`: the 20 Hz to 20 kHz long-form group-delay curves

Use `--out`, `--source-rate`, `--output-rate`, `--headroom-db`,
`--packet-cycles`, or repeated `--filter` arguments to make a focused run.
The comparison intentionally accepts only power-of-two integer upsampling.
That keeps every filter on its tuned, phase-preserving production path and
avoids combining results from different rational-resampling phases.

## Controlled conditions

Every filter receives the same source rate, output rate, -12 dBFS default
stimulus level, 131,072-frame guard, four-second analysis tail, packet shape,
frequency grid, and analysis window. The measured impulse is normalized to the
common interpolation DC gain. Responses are aligned by their measured
principal peak for impulse metrics. Tone-packet reports retain the historical
energy-centroid split for continuity and also provide stricter onset-referenced
measures whose bounds start at the principal impulse peak and end after the
nominal source-packet duration. Nominal buffer latency is reported as runtime
metadata but is never used to align responses.

The production filter itself is the reconstruction filter under test; no
secondary reconstruction or compensating equalizer is applied. This distinction
matters for Smooth Phase, whose gradual high-frequency taper is intentional.
Forcing every magnitude/transition shape to match with another filter would no
longer measure the production paths. The group-delay CSV therefore includes
magnitude at every frequency so phase comparisons can be interpreted in that
context.

## Stimuli and metrics

The impulse measurement reports:

- Pre- and post-peak energy as `10 log10(Eside / Etotal)`. These strict regions
  include the shoulders of the main lobe.
- Maximum pre- and post-ringing sample amplitude relative to the principal
  peak. The search excludes the main lobe, whose boundaries are the nearest
  interpolated zero crossings.
- Post-peak decay to -80 and -120 dB as the last threshold excursion relative
  to the principal peak. A threshold is marked censored unless at least 10 ms
  of the captured suffix remains below it.
- Null-to-null main-lobe width.
- Energy centroid relative to the principal peak.
- Group delay from a local linear derivative of the unwrapped transfer-function
  phase, expressed both absolutely and relative to the measured peak. Bins
  below -100 dB of the maximum response are masked.

Step overshoot and undershoot come from a separate direct source-rate 0-to-1
step through the production resampler. They are not inferred by cumulatively
summing an output-rate impulse, which is not generally valid for sample-rate
conversion.

The musical-transient probes are eight-cycle Hann-windowed quadrature packets
at 5, 10, 15, 18, and 20 kHz. The quadrature pair gives a phase-independent
amplitude envelope. For each packet, the bench reports integrated energy and
the maximum envelope both around the historical centroid-centred window and
outside the actual onset/end bounds. The onset-referenced columns are the
promotion metrics for pre-echo and post-decay; the centroid columns remain a
temporal-asymmetry diagnostic. The packets are rebuilt from the captured
production impulse on the integer output grid. Integer
interpolation makes this reconstruction phase-exact; the captured impulse is
trimmed only outside its first and last -160 dB peak-relative excursions, well
below the deepest reported -120 dB timing threshold.

Generated results belong under `target/` or `audio_tests/out/` and are not
committed as a product quality baseline unless a separate review establishes a
stable comparison contract.

## External-product static-filter comparison

`external_filter_bench` runs an external product's offline `upsampler.exe`
against the Fozmo production filters and labelled E3 experiment. The configured static set contains
one linear-phase, one hybrid-phase, and one minimum-phase preset. Adaptive
presets are deliberately excluded from this ranking.

On Windows PowerShell, run:

```powershell
cargo run --locked --release --bin external_filter_bench -- `
  --external-dir "C:\path\to\External-Upsampler" `
  --out target/external-filter-comparison
```

The runner generates one canonical signed-PCM24 stimulus set and sends those
encoded files to both engines at 44.1 to 176.4 kHz. Tone I/Q components are
rendered as separate dual-mono files so stereo-linked processing cannot
confound the envelope. External presets are rejected if the CLI reports its
silent unknown-preset fallback. Silence and impulse repeats detect
non-determinism; a same-length silence control is subtracted before analysis.

The external CLI does not expose a floating-point output or a no-dither switch
and reports one-LSB TPDF for PCM24 output. Its silence controls were digital
zero in the validation run, but the -120 dB decay result remains sensitive to
the PCM24 quantization floor. The JSON report preserves this limitation. Batch
elapsed time is diagnostic only: it does not isolate startup, steady-state CPU,
or peak memory.

The static report also measures magnitude response relative to normalized DC:
5/10/15/18/20 kHz gain, passband ripple, upper-band gain through source
Nyquist, cutoff landmarks, transition width to the first -100 dB crossing, and
both broad-stopband and first-image rejection. A first -100 dB crossing does
not assert that the rest of the stopband remains below -100 dB; use the
rejection columns for that question.

This is not yet the complete Pareto comparison. Controlled runtime/memory,
adaptive adversarial stimuli, and native 1-bit DSD capture must be reported
separately. In particular, adaptive presets must not be inserted into the
static-filter ranking.
