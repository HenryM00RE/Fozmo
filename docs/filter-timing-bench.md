# Production filter timing bench

The timing bench measures the exact `SincResampler` implementation used by
Fozmo rather than analyzing design-time coefficient prototypes. It covers the
four filters currently exposed by the product:

- Linear Phase (`LinearPhase128k`)
- Minimum Phase (`MinimumPhaseCompact128k`)
- Split Phase (`SplitPhase128kE2v3`)
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
principal peak for impulse metrics and by the quadrature-envelope energy
centroid for tone-packet metrics. Nominal buffer latency is reported as runtime
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
- Group delay from the exact frequency-domain moment identity
  `Re(sum(n h[n] exp(-jwn)) / H(w))`, expressed both absolutely and relative to
  the measured peak. Bins below -100 dB of the maximum response are masked.

Step overshoot and undershoot come from a separate direct source-rate 0-to-1
step through the production resampler. They are not inferred by cumulatively
summing an output-rate impulse, which is not generally valid for sample-rate
conversion.

The musical-transient probes are eight-cycle Hann-windowed quadrature packets
at 5, 10, 15, 18, and 20 kHz. The quadrature pair gives a phase-independent
amplitude envelope. For each packet, the bench reports integrated energy and
the maximum envelope before and after the nominal packet window once that
window is centered on the measured energy centroid. The packets are rebuilt
from the captured production impulse on the integer output grid. Integer
interpolation makes this reconstruction phase-exact; the captured impulse is
trimmed only outside its first and last -160 dB peak-relative excursions, well
below the deepest reported -120 dB timing threshold.

Generated results belong under `target/` or `audio_tests/out/` and are not
committed as a product quality baseline unless a separate review establishes a
stable comparison contract.
