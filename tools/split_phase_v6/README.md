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

## Fixed-reference DSD transition contract

P5 starts by freezing `transition-envelope-v1-fixed-2ms-rms-0-50ms`. The
contract serializes the 2 ms sliding mean-square restart trace and fixed
0-2, 2-5, 5-10, 10-25, and 25-50 ms interval metrics in linear power. A
frozen E2v3 report is the comparison reference; the candidate-derived
first-crossing recovery threshold remains in reports only as a secondary
diagnostic. Two clean E2v3 renders produced identical traces and native DSD
hashes. The frozen numerical tolerance is `2e-9` RMS, which removes only
rebuild-scale floating-point noise (about -175 dBFS).

Research builds can load one hash-addressed E3 character at process startup:

```powershell
$env:RUSTFLAGS = "-C target-cpu=native"
cargo run --locked --release --features research-filter-assets `
  --bin dsd_public_quality -- `
  --filter SplitPhase128kE3 --rates 128 --modulator Standard,EcBeam2 `
  --experimental-character-file path\to\candidate.f64le `
  --experimental-character-sha256 64_HEX_DIGITS `
  --transition-envelope-reference tools\split_phase_v6\baselines\e3-p5-transition-envelope-e2v3-dsd128.json `
  --transition-envelope-tolerance-rms 2e-9
```

The loader is absent from normal builds and accepts only 262,145 finite
binary64 coefficients with the declared SHA-256 and DC sum within `1e-12` of
one. Cleanup and rational assets remain frozen. The report records the path,
hash, coefficient count, and measured DC sum.

The first exact audit shows that all three E3 finalists improve the immediate
0-2 ms residual RMS by about 0.20 dB but regress 2-5 ms by about 3.24-3.28 dB.
The later difference is already very small in absolute terms: about -117 dBFS
from 5-10 ms in the worst rated cell and about -142 dBFS from 10-25 ms. This
confirms that the old roughly 5 ms recovery-time loss is threshold-sensitive,
but it does not satisfy the proposed fixed-interval non-regression rule.
`refine-0900` therefore remains the incumbent and P5 must explicitly reduce
the 2-5 ms envelope while preserving the better immediate restart event.

`summarize_e3_transition_envelopes.py` regenerates the compact, hash-addressed
candidate audit from full v5 reports. The frozen E2v3 reference and summary
are stored under `baselines/`; no E3 production promotion is implied.

`e3_full_cascade_stage_audit.py` separately propagates the exact finite-support
character response through all six frozen DSD128 cleanup stages and evaluates
principal-peak-aligned physical-time impulse envelopes at every native stage
rate. It shows that the 2-5 ms penalty is already present at the 88.2 kHz
character stage (about 3.03-3.07 dB). Cleanup stage 1 raises it to about
3.42-3.50 dB, while stages 2-6 leave it effectively unchanged. The low-level
5-25 ms difference has the same origin. The P5 character search is therefore
the correct next move; cleanup stage 1 becomes a co-optimization target only
after the structural character search produces a finalist.

## P5 constrained group-delay screen

`e3_p5_group_delay_search.py` replaces local phase controls with four
degree-five group-delay spline families. Each family has exact delay, slope,
and curvature continuity at both joins plus an exact integrated phase-closure
equality. The deterministic screen uses an unscrambled Sobol sequence in the
constraint nullspace, preserves the E2v3 magnitude target and 262,145-sample
support, and applies every impulse guard plus 5/10/15/18/20 kHz packet gates.

The full run evaluated 4,096 candidates. 2,385 passed the structural/impulse
guards, 55 of the top packet-tested designs passed all five packet gates, and
32 hash-addressed finalists were retained. `p5-d-0861` led the 2-5 ms proxy;
`p5-c-0383` was the best broad stage-envelope candidate and simultaneously
improved post-lobe, post-energy, width, overshoot, undershoot, and every packet
cell versus `refine-0900`.

Four finalists were rerun through the exact Standard/EcBeam2 DSD128 path with
the frozen E2v3 envelope. All had zero structural failures and reduced average
2-5 ms residual by 0.047-0.084 dB versus `refine-0900`. None reduced the
primary pointwise positive-excess loss: their 0-2 ms positive excess increased
by roughly 11-19 percent. `p5-c-0383`, despite the strongest multi-metric
filter-only case, increased it by 19.1 percent. This demonstrates that the
principal-peak impulse envelope remains an insufficient surrogate for the
restarted-carrier waveform after reconstruction.

P5 is therefore complete as a structural screen but has no promoted winner.
`refine-0900` remains the immutable E3 incumbent. The next loop should capture
the exact filter-only restarted-carrier waveform entering the modulator and
use its fixed-reference positive excess in finalist selection. P6 bounded
magnitude movement should wait until that surrogate mismatch is closed.

## P6 restarted-carrier search

`e3_transition_probe` closes the P5 surrogate gap by streaming the exact public
18/19 kHz mute-and-restart fixture through the production DSD128 cascade and
capturing the normalized PCM blocks after coefficient gain, headroom, block
riding, and limiting, immediately before the selected modulator. It retains
only the restart and settled-fit windows. The probe is available only with the
`research-filter-assets` feature; normal playback builds do not expose it.

The exact probe reproduced the old P5 finalist ordering and deltas. For example,
its predicted 0-2 ms positive-excess changes were +18.0% for `p5-c-0383` and
+18.4% for `p5-d-0861`, versus +19.1% and +19.3% in the reconstructed one-bit
reports. Its average interval deltas agreed to roughly 0.0002 dB. This validates
the efficient 176.4 kHz restarted-carrier model in
`e3_p6_restarted_carrier_search.py` as a finalist-selection surrogate.

The bounded P6 pass evaluated 1,024 phase-only candidates. Of those, 599 passed
the frozen impulse guards, 282 also preserved both 0-2 ms restarted-carrier
objectives, 72 passed all packet gates, and 32 hash-addressed finalists were
retained. `p6-d-0145` was the first candidate to improve both restart intervals
and all four immediate post-response objectives. A 512-point family-D local
search then froze its restart gains: 65 candidates passed the tighter transition
guards and 63 passed every packet cell.

`p6d-local-0145` (`da418ad185fdd0317c3046598eb40ec205bd33976b563870011d3f058acd51d5`)
is the current exact-validated E3 research incumbent. Relative to embedded
`refine-0900`, its filter-only metrics are:

| Metric | refine-0900 | p6d-local-0145 | Change |
| --- | ---: | ---: | ---: |
| Maximum pre-lobe | -22.8353 dB | -22.8793 dB | -0.0440 dB |
| Pre-energy | -4.8561 dB | -4.8572 dB | -0.0011 dB |
| Maximum post-lobe | -8.7040 dB | -8.7063 dB | -0.0024 dB |
| Post-energy | -2.8520 dB | -2.8524 dB | -0.0004 dB |
| Main-lobe width | 61.9356 us | 61.9275 us | -0.0081 us |
| Step overshoot | 9.1201% | 9.1232% | +0.0031 points |
| Step undershoot | 8.9010% | 8.8903% | -0.0107 points |
| Decay to -120 dB | 6.5873 ms | 6.6610 ms | +0.0737 ms |

The candidate therefore advances the Pareto frontier rather than dominating
every scalar: it spends 0.074 ms of still-sub-7 ms low-level decay and 0.003
overshoot points for the other gains. All fixed guards remain satisfied.

The exact six-cell Standard/EcBeam2 DSD128 run completed with zero structural
failures. In every matched-stress channel, 0-2 ms residual RMS improved by
0.00645 dB and 2-5 ms by 0.06413 dB. E2v3-referenced positive excess fell by
2.79% and 2.02%, and threshold recovery improved by 0.221-0.244 ms. The five
packet deltas versus `refine-0900` are +0.0001, +0.0003, -0.0772, +0.0347,
and -0.1850 dB at 5, 10, 15, 18, and 20 kHz. The fixed-magnitude guard passed;
the largest exact spur increase was 1.60 dB at an absolute -199.64 dBFS, so
there is no meaningful frequency-domain regression.

The search reports, selected coefficient file, and compact exact audit are
frozen under `baselines/`. This does not change the embedded E3 asset and does
not promote E3 to production; E2v3 remains the product default. The remaining
gap to the aspirational -9.5 dB post-lobe and -3.1 dB post-energy targets is much
larger than the gains available in this tightly constrained phase-only region,
so any next expansion should explicitly test a bounded magnitude or cleanup-1
co-optimization while retaining the P6 restarted-carrier gates.

## P7.0 definitive P6 freeze

`freeze_e3_p6.py` archives the immutable P6 research reference before P7 adds
new design variables. The frozen package records the `p6d-local-0145`
coefficient hash and exact sum, alignment, native timing and all five onset
packets, group-delay artifact hash, counterfactual probe summaries, two clean
release-build executable hashes, source/compiler/CPU provenance, and all six
Standard/EcBeam2 native DSD hashes.

The two clean builds used separate Cargo target directories. Their executable
hashes differ, which is explicitly retained as provenance, but the reports are
byte-for-byte identical after removing executable hash and wall-clock render
time. Every native DSD hash and every measurement trace matches exactly. Both
runs completed six cells with zero structural or diagnostic hard failures.

The native timing bench now accepts the same feature-gated, hash-checked
research character loader as the DSD bench. Its exact P6 result confirms
-22.8793 dB maximum pre-lobe, -4.8572 dB pre-energy, -8.7063 dB maximum
post-lobe, -2.8524 dB post-energy, 61.9275 microseconds width, 12.5739 percent
runtime overshoot, 10.1211 percent runtime undershoot, and 6.6610 ms decay to
-120 dB. Normal builds do not expose the loader.

## P7.1 counterfactual restart contract

`e3_p7_counterfactual.py` implements the linear filter-generated restart
residual directly:

```text
r[n] = y_mute_restart[n] - y_continuous_recovered_carrier[n]
```

It supports arbitrary coherent carrier pairs, restart phases, and mute lengths,
and reports the frozen 0-2, 2-5, 5-10, 10-25, and 25-50 ms intervals. Unit tests
show that the bounded-mute formulation converges to the P6 closed form and that
separate character/cleanup propagation matches a directly cascaded FIR to
floating-point tolerance.

`e3_transition_probe` now independently renders the real mute/restart fixture
and a continuously running recovered-phase reference through the production
DSD128 interpolation and normalization path. It retains the old fitted-carrier
report for cross-validation. On P6, the two methods agree within 0.00000011 dB
through the primary 0-5 ms region for both Standard and EcBeam2. This validates
the counterfactual residual as the P7 optimizer domain without making it the
only full-path promotion measurement.

## P7.2/P7.3 cleanup-stage-1 feasibility pilot

`e3_p7_cleanup_search.py` builds cleanup stage 1 in its exact 126-dimensional
halfband equality nullspace: symmetry is intrinsic, the centre and independent
even coefficients stay fixed, and the unique odd-pair perturbations sum to
zero. It maps twelve counterfactual training fixtures spanning three carrier
pairs, two restart phases, and two mute lengths. A stopband-preserving SVD then
retains the 24 most sensitive objective directions before constrained solves at
trust radii from `1e-5` through `2e-4`.

Twenty transition, post-lobe, post-energy, and balanced solutions were exact-
tested. Fifteen passed the complete impulse, packet, passband, monotonic-
transition, and -150 dB rejection guards, but none met a minimum meaningful
effect size. The best changes were only about `7e-7` dB in an individual
counterfactual cell and roughly `1e-7` dB in static timing. Dense rejection
certification restricted useful coefficient movement to about `2e-9`, far
inside every requested trust radius.

No cleanup candidate becomes an incumbent. This is strong evidence that the
existing 509-tap cleanup stage has no locally useful freedom while preserving
the certified rejection floor. P7 should therefore proceed to the neutral and
micro-apodized monotone character-magnitude families; cleanup can be solved
again only after a new character target changes the feasible geometry.

## P7.4 bounded character-magnitude search

`e3_p7_magnitude_sensitivity.py` parameterizes the upper band with eight
compact quintic smoothstep controls at 15, 18, 19, 20, 20.5, 21, 21.5, and
22.05 kHz. The basis is identically zero below 15 kHz and has local support
between adjacent controls, preventing transition controls from leaking into
the protected passband. Neutral and micro-apodized families retain separate
movement limits. Central finite differences measure static timing, all five
production packets, twelve counterfactual restart fixtures, passband, image,
reverse alias, and transition monotonicity before an SVD/linearized feasibility
pass proposes exact trial directions.

The full deterministic screen evaluated 4,096 Sobol points per family. Of
8,192 candidates, 702 passed the cheap linearized and monotonicity gate, 128
received exact timing and packet tests, 53 passed every exact static guard, and
48 received the full counterfactual suite. Twelve diverse finalists were
retained in the report, but no candidate met a minimum meaningful effect size.

The best guarded post-lobe movement was only 0.00679 dB, versus the frozen
0.05 dB threshold. The best worst-fixture counterfactual RMS movement was
0.00134 dB, and the strongest worst-fixture positive-excess reduction was
0.0415 percent, versus the frozen 1 percent threshold. Some candidates improved
mean counterfactual energy by roughly 0.13-0.28 dB, but their worst fixture
barely moved; lexicographic worst-case selection correctly prevents those from
becoming incumbents. Larger finite-difference steps could approach a 0.05 dB
post-lobe gain only by violating packet, decay, or monotonic-transition guards.

No P7.4 candidate warrants exact DSD finalist testing and `p6d-local-0145`
remains the immutable research incumbent. Cleanup-nullspace, neutral-magnitude,
and micro-apodized-magnitude searches are three structurally distinct failures
to reach the campaign's stopping thresholds. Under the frozen guards, the P7
local frontier is therefore considered saturated; an alternating joint search
is not justified without evidence of a material interaction. Any further
engineering search should move to the explicitly separate P8 structural-
capacity experiments rather than select numerical trivia.

## P8 structural-capacity audit

`e3_p8_capacity_audit.py` reconstructs the exact `p6d-local-0145` complex
target from its frozen spline coordinates and independently reproduces the
incumbent within `7e-18` per coefficient across the tested NumPy runtimes. It
then audits the three proposed magnitude-model orders and realizes the same
target at both 262,145 and 524,289 character taps through the complete timing,
packet, rejection, and twelve-fixture counterfactual contracts.

The magnitude autocorrelation energy omitted above order 512 is only
`3.74e-19`; increasing to 768 and 1,024 reduces it to `2.65e-19` and
`2.04e-19`. Maximum passband representation error is already only
`4.22e-8` dB at order 512 and falls to `2.85e-8` dB at order 1,024. Direct
truncated-autocorrelation realizations are retained only as a diagnostic: they
do not meet the frozen stopband gate and therefore are not filter candidates.
The omitted energy and passband results show no evidence that a costly new
768/1,024-order PSD solve would unlock the timing frontier.

Doubling the character support to 524,289 taps is numerically inert. Pre-lobe,
post-lobe, main-lobe width, decay, and every packet cell are unchanged at the
reported precision. Post-energy moves by `1.3e-15` dB and the worst primary
counterfactual cell improves by roughly `3.1e-12` dB. The one-million-tap
reference is therefore not run: the campaign explicitly requires evidence
that finite support is binding before that experiment.

`e3_p8_cleanup_support.py` expands cleanup stage 1 from 509 taps to 765 and
1,021 taps while preserving its exact halfband structure, centre, symmetry,
DC, and branch sums. Eight pinned Clarabel solves cover four trust radii at
both supports. The longer filters reduce the isolated cleanup Chebyshev error
from `7.83e-9` to as little as `1.31e-10` and `2.48e-11`, proving that cleanup
frequency capacity expands. Six candidates pass the complete full-cascade
guards, but none produces a meaningful timing or restart effect: the best
worst-fixture restart movement is about `2.7e-9` dB and static movements are
around `1e-8` dB.

P8 therefore finds no capacity-bound route beyond `p6d-local-0145`. No P8
candidate becomes an incumbent, exact DSD finalist testing is not warranted,
and final production assets are not regenerated. E2v3 remains the production
default and `p6d-local-0145` remains the immutable E3 research frontier.

## P9 timing-first replacement campaign

P9 tests a stricter product question than P6-P8: can one deterministic static
filter deliver a clearly material timing improvement while preserving the
production E2v3 onset behaviour? `e3_p9_feasibility.py` therefore anchors every
hard gate to a fresh E2v3 build rather than treating the packet-regressing P6
incumbent as the safety reference. Integrated onset pre-echo and maximum onset
pre-echo at 5, 10, 15, 18, and 20 kHz may each move by at most `+0.10 dB`.
Core timing tolerances are frozen, passband movement below 18 kHz is limited to
`0.001 dB`, and the deliberately relaxed stopband floor is `-150 dB`.

A replacement must improve maximum pre-lobe by at least `2 dB` and improve at
least three of maximum post-lobe (`0.25 dB`), either side-energy metric
(`0.10 dB`), main-lobe width (`2 us`), overshoot (`0.5` percentage point), or
undershoot (`0.5` percentage point). This prevents a candidate from becoming
the incumbent on numerical trivia.

The fresh native E2v3/P6 report is exactly reproducible across two independent
output directories. Both JSON reports hash to
`180c0811b99b96ebdca5cb2ebdc2fff3aa9b0346145884f453065ae15073fae5`,
both group-delay CSVs hash to
`a17ea2be75c83124394f8b59031d32b28990e7513d4c46bc78a305a47e2ca738`,
and the release benchmark executable hashes to
`38df594549a489b0ba63f14b53cbac9b1d1ad6417dbc22c02b4ad7a5130263e5`.
The E2-to-P6 phase homotopy can retain the strict packet contract only through
an interpolation fraction of `0.004216965`; that amount moves maximum pre-lobe
by only a few hundredths of a decibel. The full P6 timing point remains far
outside the production packet envelope, particularly at 15, 18, and 20 kHz.

Two deterministic Sobol searches then use sparse and dense compact phase grids
plus bounded top-octave micro-apodization. The dense grid has 20 phase and eight
magnitude coordinates. It evaluates 8,192 linearized points and exact-tests
256 static and 64 packet candidates at each of 262,145, 524,289, and 1,048,577
taps. It produces 81 exact packet-safe records but zero clear replacements.
Its strongest pre-lobe result is only `0.01758 dB` better than E2v3.

`e3_p9_nonlinear_refine.py` adds an exact seven-dimensional packet-null timing
subspace, three independent starts, three objectives, feasible-iterate
retention, and deterministic boundary recovery. The best exact/native valid
point is `nonlinear-01-side_energy`:

| Native metric | E2v3 delta |
| --- | ---: |
| Maximum pre-lobe | `-0.07969 dB` |
| Maximum post-lobe | `-0.01612 dB` |
| Pre-energy | `-0.01316 dB` |
| Post-energy | `+0.00465 dB` |
| Main-lobe width | `-0.0293 us` |
| Step overshoot | `-0.0266 percentage point` |
| Step undershoot | `-0.0641 percentage point` |
| Decay to -120 dB | `-0.0850 ms` |
| Worst onset packet | `+0.08921 dB` at 18 kHz |

That is a valid but immaterial movement: it reaches only four percent of the
required pre-lobe effect and no secondary promotion threshold. Exact segment
backtracking confirms the active boundary. A representative rejected endpoint
improves pre-lobe by only `0.08522 dB`, but already increases decay by
`0.26644 ms` and violates the 18 kHz integrated packet gate. The complete
multi-start and boundary reports preserve both valid and rejected endpoints.

P9 explicitly performs the requested million-tap reference even though P8 did
not find evidence that support was binding. Timing differences between the
262,145-tap result and the two longer supports are about `1e-12`, and packet
differences are at most `2.13e-10 dB`. The support-aware spectral audit covers
the complete million-tap response with a 2,097,152-point FFT:

| Character taps | Stopband maximum | Passband delta below 18 kHz |
| ---: | ---: | ---: |
| 262,145 | `-176.72619493 dB` | `0.000004965 dB` |
| 524,289 | `-176.72619343 dB` | `0.000004965 dB` |
| 1,048,577 | `-176.72619377 dB` | `0.000004965 dB` |

The million-tap realization therefore improves neither timing nor meaningful
noise rejection for this target. `e3-p9-best-valid-research.f64le` is retained
only to make the native cross-check reproducible; it is not an incumbent or a
shipping asset. Its SHA-256 is
`3ffaf9b1cf5a407d080b6c378f8286517441449ed2e87919bca32491b23a907c`.

No P9 candidate reaches the predeclared replacement effect size, so the
conditional DSD, real-time, and final-asset gates are intentionally not run.
There is no runtime enum, manifest, coefficient, or default-filter change.
E2v3 remains the production default, and `p6d-local-0145` remains the separate
research timing frontier while failing P9's strict production packet-parity
contract.
