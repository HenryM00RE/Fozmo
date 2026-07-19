# Split Phase E experiment

Split Phase E is an experimental C-guided fast path. It leaves the production
Split Phase C assets and the running Split Phase D cold SDP solve unchanged.

The magnitude generator searches a deterministic family of 513-tap structured
FIR seeds, ranks only candidates with a 10x magnitude-gate margin by their
20 Hz-20 kHz agreement with frozen Split C, converts the selected seed to an
explicit minimum-phase factor, and constructs its Fejer-Riesz Gram matrix with
a tiny positive power floor. It then runs the same dense, exchange-grid, PSD,
equality and high-precision magnitude checks used by Split Phase D. Because the
factor is known by construction, E certifies its reconstruction, zero radii and
an independent homomorphic crosscheck instead of rediscovering it with a cold
Wilson search. No production identity or UI wiring is created at this stage.

Run the magnitude build and spectral factorization from the repository root:

```sh
python -m tools.split_phase_v5.c_derived_magnitude \
  --work-dir tools/split_phase_v5/work-spe \
  --factor
```

Re-audit an existing artifact without searching again:

```sh
python -m tools.split_phase_v5.c_derived_magnitude \
  --work-dir tools/split_phase_v5/work-spe \
  --audit-existing
```

An E artifact is usable only when both the magnitude report and, when requested,
the spectral-factor report say `"accepted": true`. Full phase, temporal,
runtime and comparison certification remains mandatory before E can become a
production filter.

After magnitude and factor acceptance, run the inherited D phase, temporal,
cleanup and rational pipeline with E's certified known-factor hook:

```sh
python -m tools.split_phase_v5.build \
  --work-dir tools/split_phase_v5/work-spe-direct-factor
```

If a late comparison/report step is interrupted after the character, cleanup
and rational blocks are saved, resume only final comparison assembly with:

```sh
python -m tools.split_phase_v5.build \
  --work-dir tools/split_phase_v5/work-spe-direct-factor \
  --finalize-saved
```

Continue a completed E result with the checkpointed E2 audio/formal refinement:

```sh
python -m tools.split_phase_v5.e2_targeted_search \
  --source-dir tools/split_phase_v5/work-spe-direct-factor \
  --work-dir tools/split_phase_v5/work-spe-e2-targeted
```

E2 freezes the accepted E magnitude and factor, evaluates at most 96 constrained
group-delay candidates, refines only audio/formal finalists, and separately runs
a high-resolution complex-error refinement. Every proxy candidate and Lawson
iteration is durable and hash-verified on resume. A screening winner is rerun
through cleanup and rational optimization before E2 can report acceptance; it
still never creates or promotes a production filter identity.

If a delay candidate has a strong raw-support temporal score but cannot be
reached from the old E character in a few trust-region steps, refine directly
from that saved support candidate without repeating the 96-candidate search:

```sh
python -m tools.split_phase_v5.e2_support_refine \
  --proxy-work-dir tools/split_phase_v5/work-spe-e2-targeted-v2-20260719 \
  --candidate-index 48 \
  --iterations 16
```

This path checkpoints every Lawson iteration and names each full audit by its
iteration count, so a later invocation can safely extend 16 iterations to 32
or 40 without invalidating the earlier result.

The narrow audio-gradient interval between the smooth improvement region and a
dominant-peak discontinuity can be probed without rerunning the broad search:

```sh
python -m tools.split_phase_v5.e2_audio_line_search
```

Its nine immutable proxy checkpoints can be passed directly to
`e2_support_refine` by using the line-search work directory and selected index.

For the last small search around the best stable line point, run:

```sh
python -m tools.split_phase_v5.e2_audio_local_search
```

This evaluates 73 immutable coordinate probes around line candidate 1005. It
is intended as a bounded stopping test, not another open-ended candidate run.

E2v3 combines the strongest independently validated results: it starts from
the audio-2064 character and applies the four-million-point refinement that
made formal E2 pass its third Pareto gate:

```sh
python -m tools.split_phase_v5.e2_v3_highres
```

The four high-resolution iterations are individually checkpointed. If the
screening cascade passes, cleanup and rational stages are regenerated before
`e2v3_report.json` can report acceptance. No production identity is promoted.

E2v4 performs the final bounded audio search around candidate 2064:

```sh
python -m tools.split_phase_v5.e2_v4_audio_search
```

It runs five shrinking coordinate sweeps followed by deterministic Sobol local
probes, with a hard cap of 256 immutable candidates. Expensive refinement is
reserved for at most two candidates that cross the 15% dominant pre-energy
target while preserving E2v3-derived midband and overshoot guards.

E2v5 is a bounded structural experiment for when E2v4 exhausts the original
18-coordinate basin. It projects candidate 2064 into four expanded delay
models (30/36 controls with 14/15 kHz joins), evaluates exactly 64 candidates
per model, and writes an immutable, hash-verifiable checkpoint for every
candidate. Only candidates that beat the binding dominant-pre-energy target
while retaining the E2v3 midband and overshoot guards may enter the expensive
1M/4M Lawson and complete-cascade pipeline:

```bash
python -m tools.split_phase_v5.e2_v5_structural_search
```

E2v5 is experimental and never promotes a production identity.
On the Windows CUDA workstation, `run_e2v5_pc.sh` pins this CPU experiment to
cores 0-2 at low CPU and I/O priority so the independent D solve remains
undisturbed.

To package the accepted E2v3 winner for an explicitly experimental runtime
audition without changing the production identity of C or D, run:

```bash
python -m tools.split_phase_v5.export_e2v3_experimental \
  --source-dir tools/split_phase_v5/work-spe-e2v3-audio-highres-20260719
```

The exporter requires both the screening and regenerated full-pipeline audits
to be accepted, requires `production_promoted=false`, validates every array
shape, and writes a hash-manifested `split_phase_e2v3` asset bundle.
