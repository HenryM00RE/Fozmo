# EcBeam2 Experimental Modulator

## Status

EcBeam2 is an isolated, manually selectable DSD64 research modulator. It does
not replace production EcBeam, and its UI label remains explicitly
experimental. The architecture and research harness are useful; the first
fixed-CRFB candidate family did not pass stability qualification.

As of 2026-07-11:

- the isolated M4/N8 engine, tail-aware reconstruction profiles, diagnostics,
  production-frontier observer, exact horizon oracle, and campaign tooling are
  implemented;
- production EcBeam output, kernels, defaults, aliases, and policy remain
  unchanged;
- both DSD64 wire families are supported, while DSD128 and DSD256 fail closed;
- the fixed OBG 1.65 CRFB stability experiment is stopped before budget,
  oracle, or external-quality qualification because no candidate reached zero
  repairs.

This is a negative result for one plant/objective family, not a reason to
remove the isolated engine or its qualification infrastructure.

## Architecture delivered

EcBeam2 owns a normal EC-mode CRFB core and independent M4/N8 survivor state.
It does not activate or mutate production EcBeam's beam. Its formal objective
can combine:

```text
tail-adjusted reconstruction increment
+ weighted normalized CRFB state-potential increment
+ weighted normalized state barrier
+ weighted raw quantizer regularizer
```

The reconstruction and state-potential increments telescope and are never
double-counted at the terminal state. Ultrasonic budgets use causal nonnegative
power, while signed approximation error has an independent wire-rate-correct
EMA. Repair, escape, replay, output-length, objective-component, scale, and
per-stage localization diagnostics are kept in the `ecbeam2_*` namespace.

The optional `ecbeam2_observer` feature exposes a read-only production EcBeam
frontier for calibration. Production remains the authoritative chooser, and
observer parity tests protect its output and fast-kernel eligibility.

The exact oracle is candidate-bound (`ecbeam2-exact-oracle-v2`) and reuses one
opaque active-prefix seed across N8, N12, and N16. Prefix health is therefore
part of candidate acceptance rather than an unrelated reconstruction-only
precondition.

## Lightweight qualification harness

The original campaign routed every research row through the expensive
selectable quality matrix. That made even an inert objective-scale probe run
decoding, FFT analysis, broad fixtures, and external scoring.

The dedicated path now is:

```sh
ecbeam2_quality --ecbeam2-qualification \
  --mode scale-probe|stability|budget \
  --source-rates 44100,48000 \
  --filters MinimumPhase,SplitPhase \
  --modulator EcBeam2 \
  --candidate-config candidate.json \
  --corpus-manifest corpus.json \
  --out output-directory
```

It reuses the same corpus materialization, resampler, limiter, renderer,
diagnostic-window mapping, native packing, and flush path as the full harness.
It writes native stereo digests, runtime provenance, reconstruction energy,
and mode-specific diagnostics, but skips DSD decoding and quality analysis.

Parity was checked on the short stability corpus for MinimumPhase at 2.8224
MHz. Both measurements matched the full corpus path exactly for native
left/right bitstream digests and all compared health, energy, localization,
and p95 objective diagnostics. The lightweight command completed in under one
second wall time; the corresponding full selectable/corpus command took about
18 seconds.

## Frozen qualification procedure

The campaign order is now:

1. Run four inert barrier-knee scale probes on `stability_short.json`.
2. Assert all zero-weight knee variants are bit-identical.
3. Derive wire-specific weights from p95 contribution ratios.
4. Run the frozen 35-row short stability stage (A1 plus 34 EcBeam2 rows).
5. Reject repairs, resets, invalid inputs, truncation, and output-length
   errors; retain at most eight by worst-cell reconstruction energy.
6. Run those eight on the complete calibration corpus and retain at most two.
7. Qualify ultrasonic-only, signed-error-only, combined, and 3x3 allowance
   budgets.
8. Run the candidate-bound active-prefix exact oracle.
9. Only then run the expensive external quality matrix and held-out replay.

The old raw-weight 28-row campaign is retained only as
`legacy_v1_selection_candidates()` for artifact reproducibility. Normal
selection cannot invoke it.

## 2026-07-11 exploratory evidence

The runs were made from a dirty research tree and are deliberately not frozen
acceptance evidence. The important aggregate results are recorded here so the
failed experiment is not repeated.

Scale-probe digest:

```text
11a7144bd39f31493eba4b57b5053f7c4a39ca86e1aa9b0fab82fb4a6e5b4f68
```

All four zero-weight barrier knees emitted identical bits in every primary
filter/wire cell. The unconstrained control already showed roughly 384,000
repairs per cell, with zero resets, invalid inputs, truncation, or output-length
errors. Representative p95 scales were:

| Wire rate | Reconstruction | State delta | Barrier (rho 0.80) | Quantizer squared |
| --- | ---: | ---: | ---: | ---: |
| 2.8224 MHz | 0.22537 | 0.18755 | 0.53335 | 8.41699 |
| 3.0720 MHz | 0.20711 | 0.18745 | 0.54186 | 8.43547 |

The short stability stage completed all rows in roughly 2.5 minutes:

| Result | Aggregate repairs | Worst-cell reconstruction regression |
| --- | ---: | ---: |
| Tail-aware unconstrained control | 1,538,032 | +14.149 dB |
| Least-repairing state-controlled row (`a0.3/rho0.8/b0.1`) | 1,646,550 | +14.395 dB |
| Full EcBeam2 candidate range | 1,538,032–1,700,997 | +14.149–14.529 dB |

The predeclared requirements were zero repairs and no more than +0.25 dB
worst-cell reconstruction regression. No row was close to either threshold,
and the unconstrained control was better than every state-controlled row.
Consequently the shortlist was empty and later phases were correctly skipped.

## Recommended next work

The next EcBeam2 research step should be an EcBeam2-specific CRFB/NTF design,
not a wider heuristic sweep on the fixed OBG 1.65 plant.

Recommended order:

1. Co-design a plant whose state limits and input range are compatible with
   tail-aware reconstruction ranking, then repeat the cheap stability stage.
2. Use exact N8/N12/N16 comparisons only after the active prefix is repair-free
   to separate beam-pruning loss, horizon loss, and profile mismatch.
3. Consider robust or measured DAC reconstruction profiles after a stable
   plant exists; the current six-state harness profile remains a useful control.
4. Consider future-input viability or Riccati terminal values only with an
   explicit latency/future-input model or robust admissible-input set.
5. Keep psychoacoustic weighting and min-max spur constraints downstream of
   basic stability and causal budget qualification.

Until a new plant passes the short stage, EcBeam2 should remain a manual
experimental selector and research harness. Production EcBeam policy should
not change.
