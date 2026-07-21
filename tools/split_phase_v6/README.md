# Split Phase E3

E3 is an experimental successor to `SplitPhase128kE2v3`. It never mutates or
relabels the stable ID 37 asset bundle. E2v3 remains the product default until
an E3 candidate passes the complete timing, reconstruction, DSD, determinism,
and real-time promotion gates.

The first bounded phase experiment is:

```powershell
python -m tools.split_phase_v6.e3_phase_search
```

It preserves the accepted E2v3 magnitude samples, makes 14–20.5 kHz phase a
searchable region, and closes smoothly back to the original phase by 22.05
kHz. The search is a fixed grid of join frequencies, bulk-delay offsets, and
blend strengths. Every candidate retains the 262,145-coefficient support.

Candidates must pass the pre-energy, pre-lobe, main-lobe, and -120 dB decay
safety gates before Pareto ranking. The exact discontinuous timing metrics are
used only after coefficient generation; the search does not claim production
promotion. Outputs go to `tools/split_phase_v6/work-e3-p/` and include the
complete candidate audit plus a hash-addressed winner coefficient file.

To export a safety-qualified winner into the separate experimental asset
identity, run:

```powershell
python -m tools.split_phase_v6.export_e3_experimental
```

The exporter replays the winner hash and safety gates, copies only the frozen
E2v3 cleanup/rational tables, and marks the resulting E3 bundle as not yet
accepted by the full production pipeline.

The initial runtime identity is `SplitPhase128kE3`, stable ID 38. It is an
explicit audition target and does not replace the E2v3 default.

## Packet qualification

Impulse-only Pareto ranking is not sufficient. Qualify every impulse-safe
candidate with onset-referenced 5, 10, 15, 18, and 20 kHz Hann packets:

```powershell
python -m tools.split_phase_v6.evaluate_e3_packets
```

This second stage uses the principal impulse peak plus the nominal packet
bounds. It does not use the packet energy centroid as a proxy for onset.
Candidates receive a non-regression pass only when pre-echo at every packet
frequency is no more than 0.10 dB above E2v3.

The first search evaluated 315 candidates. Thirty-two passed the impulse
safety gates, but none passed packet non-regression. The initial P1 finalist
(`join-16500_delay--37_strength-0.10`) remains useful as an experimental
ablation because it improves several impulse and group-delay metrics, but it
is not promotion-qualified: its onset pre-echo regresses materially at 10,
15, 18, and 20 kHz. `production_promoted` and `accepted_full_pipeline`
therefore remain false.

Frozen reports are in `baselines/`:

- `e3-p3-onset-timing.json` is the corrected historical E2v3/early-E3 timing run;
  it is not the retained `refine-0900` measurement.
- `e3-phase-search-packet-qualification.json` contains every impulse-safe
  candidate and its packet-onset deltas.
- `baseline-lock.json` records the source commit, hashes, test settings, and
  external-product render hashes.

The next E3 search family must include packet onset in its hard gates rather
than selecting on impulse metrics first and discovering packet regressions
after export.

## Guard-constrained frontier refinement

The P2 and P3 searches use the stricter frontier guards directly:

- maximum pre-lobe at or below -22.5 dB;
- pre-energy at or below -4.85 dB;
- main lobe at or below 62.5 microseconds;
- runtime overshoot at or below 13.4 percent;
- decay to -120 dB at or below 7.0 ms;
- fixed E2v3 magnitude with exact production rejection and DSD checks for
  finalists.

Run the deterministic searches with:

```powershell
python -m tools.split_phase_v6.e3_packet_phase_search
python -m tools.split_phase_v6.e3_phase_refine_search
```

The retained experimental asset is P3 candidate `refine-0900`. Its exact
44.1-to-176.4 kHz runtime result is -8.70 dB maximum post-lobe, -2.85 dB post
energy, 61.94 microseconds main-lobe width, 12.56 percent overshoot, 10.14
percent undershoot, and 6.59 ms decay to -120 dB. It passes every timing guard
and improves the initial P1 post-lobe by 0.41 dB. The 15 kHz onset pre-echo
cell is -31.02 dB, 2.77 dB better than P1 but still behind E2v3.

Frequency rejection passes: the 2x runtime image and reverse alias are both
about -182.45 dB; the rational path measures -186.68 dB image and -190.34 dB
reverse alias. The narrowed DSD128 Standard/EcBeam matrix has zero structural
hard failures and negligible carrier-gain movement. It does expose a Pareto
cost in high-frequency transition diagnostics: recovery is about 4.9-5.3 ms
slower and transition residual peak is 1.10 dB higher than E2v3. Consequently
the full DSD-path non-regression gate and production promotion remain false.

The next search should either add the DSD transition tail as a hard objective
or begin the wider joint magnitude/phase family; further cleanup-only changes
are too small to move these transient cells materially.

## Recovery-tail refinement

P4 keeps `refine-0900` and `sobol-0370` immutable, interpolates only their
unwrapped phase, reprojects every trial onto the accepted E2v3 magnitude, and
adds bounded local high-frequency phase curvature. Run the deterministic 2,048
point search with:

```powershell
python -m tools.split_phase_v6.e3_recovery_phase_search
```

The tighter search guards add maximum post-lobe at or below -8.6 dB, 15 kHz
onset pre-echo at or below -30.5 dB, and a conservative 9.22 percent proxy for
the 12.8 percent exact-runtime overshoot limit. The 4 and 8 ms tail energies
are ranking objectives, not substitutes for the exact DSD transition gate.

Forty of 2,048 trials passed every search guard. Two representative finalists
were exact-tested:

- `recovery-2047` improved the retained filter's 15 kHz onset pre-echo by
  1.006 dB and made small simultaneous gains in post-lobe, post-energy, width,
  and undershoot. DSD128 transition residual improved by 0.0148 dB and Standard
  recovery improved by 0.22-0.27 ms, but EcBeam recovery regressed by
  0.023-0.051 ms.
- `recovery-0575` preserved the retained decay time, improved 15/18/20 kHz
  onset pre-echo by 0.22-0.25 dB, and made small post-response gains. DSD128
  transition residual improved by 0.0179 dB and Standard recovery improved by
  0.22-0.27 ms, but EcBeam recovery regressed by 0.023-0.045 ms.

A reprojected 50 percent phase step toward `recovery-0575` made EcBeam matched
recovery 0.266 ms worse despite improving the transition residual. This proves
that the current percentile-derived recovery threshold is non-monotonic with
the scalar impulse-tail proxy. No P4 candidate is promoted; `refine-0900`
remains the experimental incumbent. The next optimizer must score a fixed-
reference filter-only transition envelope or the actual DSD transition cells,
then use the existing recovery time as a reported secondary diagnostic.

The P4 reports under `baselines/` contain the complete search audit, corrected
current-incumbent timing, exact finalist timing, and timestamp-forced DSD128
runs. On Windows, coefficient swaps for an `include_bytes!` asset must update
the destination timestamp before invoking Cargo; otherwise an older copied
timestamp can incorrectly reuse a previously embedded coefficient set. Native
DSD hashes must change before a candidate run is accepted as valid evidence.
