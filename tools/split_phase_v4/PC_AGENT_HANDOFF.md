# Split Phase D — PC agent handoff

## Mission

Finish and certify `SplitPhase128kV4` (stable ID 36, UI label **Split Phase D**)
without relaxing any production gate. First, race a genuine order-512 PSD-cone
magnitude solve against the still-running Mac solve, then return an independently
accepted magnitude checkpoint for the remainder of the build.

The implementation plan is a guide, but its filter specifications and hard gates
are binding. Do not modify Split Phase C or its assets.

## Repository state

- Branch: `feat/compact-phase-test`.
- Read the commit with `git rev-parse HEAD`.
- `assets/filters/split_phase_v4/generated.rs` identifies a development
  checkpoint. Current V4 binaries are temporary C-derived plumbing assets, not
  production D coefficients.
- Immutable C character SHA-256:
  `9e3e33585cff946b6b74d1d66368b1e837c5e2dffc65b893d38e23e71cac87cf`.
- Solver work under `tools/split_phase_v4/work*` is ignored by Git.

## Why the PC race exists

The live Mac SCS 3.2.11 process uses the CPU-indirect backend. A stack sample
showed roughly 96% of runtime in single-core conjugate-gradient sparse `A`/`A^T`
products. It is healthy but has no completed checkpoint. Do not stop it.

This PC has a Ryzen 7 9800X3D, 64 GB RAM and RTX 4080 Super. Use WSL2/Linux and
follow `tools/split_phase_v4/PC_WSL.md`.

## Chosen execution mode

Run one CUDA GPU-indirect solve. Do not start a simultaneous MKL solve. MKL is
only the fallback if the CUDA-enabled SCS extension cannot be built or loaded.
The backend changes only the linear algebra; it solves the same PSD program and
uses the same independent acceptance audit.

## CPU fallback: threaded MKL

From the repository root inside WSL:

```sh
python3 -m venv .venv-split-phase-d
.venv-split-phase-d/bin/python -m pip install --upgrade pip
.venv-split-phase-d/bin/python -m pip install -r tools/split_phase_v4/requirements.lock
chmod +x tools/split_phase_v4/run_pc_sdp.sh
OMP_NUM_THREADS=8 MKL_NUM_THREADS=8 \
  tools/split_phase_v4/run_pc_sdp.sh mkl initial
```

The `initial` profile matches the Mac (`eps=1e-6`, 20,000 iterations). It does
not weaken acceptance: all dense, PSD, equality and high-precision checks remain
mandatory. If rejected, preserve that directory and launch `mkl strict` with a
different `SPLIT_PHASE_D_WORK_DIR`.

The default output is `tools/split_phase_v4/work-pc-mkl`.

## Durable solver checkpoints

SCS now writes a durable checkpoint every 1,000 iterations. The atomically
published `magnitude_order_512_resume.json` points to an immutable NPZ plus JSON
sidecar containing exact SCS primal/dual/slack state, the raw Gram matrix,
autocorrelation, active grids, iteration totals, residuals/gap, array hashes and
interim verification metrics. A partially written or incompatible checkpoint
is rejected rather than guessed around.

Resume an interrupted GPU directory with the identical invocation:

```sh
SPLIT_PHASE_D_WORK_DIR="$PWD/tools/split_phase_v4/work-pc-gpu" \
SPLIT_PHASE_D_RESUME=1 \
  tools/split_phase_v4/run_pc_sdp.sh gpu initial
```

Resume restores SCS `x`, `y` and `s`; internal scaling and acceleration history
are rebuilt. Consequently, resume is a numerically valid warm continuation, not
a promise of a bitwise-identical trajectory. All original independent final
acceptance gates remain binding.

## Primary run: CUDA GPU indirect

If both `nvidia-smi` and `nvcc` work in WSL, build SCS 3.2.11 with its GPU
indirect backend as described in `PC_WSL.md`, then run:

```sh
SPLIT_PHASE_D_WORK_DIR="$PWD/tools/split_phase_v4/work-pc-gpu" \
  tools/split_phase_v4/run_pc_sdp.sh gpu initial
```

The runner defaults to GPU. Use the dedicated GPU work directory shown above.
If CUDA cannot be enabled promptly, use the MKL fallback; do not run both PC
backends simultaneously.

## Magnitude acceptance

Production magnitude must come from the genuine Fejer-Riesz Gram PSD cone.
Kaiser, Remez and FIRLS are warm starts only and can never be exported as D.
Order escalation is `512 -> 768 -> 1024`, only after independent infeasibility.

Re-audit a completed directory with:

```sh
.venv-split-phase-d/bin/python -m tools.split_phase_v4.magnitude_sdp \
  --order 512 --verification-fft-len 8388608 --exchange-rounds 10 \
  --work-dir tools/split_phase_v4/work-pc-mkl --audit-existing
```

Select nothing unless this exits successfully and the JSON says
`"accepted": true`. Solver status alone is never sufficient.

## Work already implemented

- V4 identity/ID, Rust generic frozen-bundle runtime, UI label and EcBeam2 / 7th
  Order Search wiring.
- Exact interpolation, decimation and rational model; worst randomized model
  comparison is about `1.3e-12`.
- Consistent A/B/C temporal baselines and physical log-frequency derivatives.
- Nullspace-constrained degree-five group-delay spline participating in the
  character outer loop.
- Independent Wilson/homomorphic spectral factoring and high-precision checks.
- Matrix-free FFT-CG Lawson character solver. On the difficult warm target it
  reached complex error `5.06e-10`, stop `4.64e-9`, and exact 0.5 parity sums.
- Complete-cascade cleanup acceptance and joint finite-support rational rows.
- Rust chunk/reset/EOF/drift/runtime tests, UI tests/check/build and EcBeam2 path
  passed with temporary plumbing assets; rerun all with final assets.

## After accepted magnitude

Return the complete accepted PC work directory to the Mac without committing it.
On the Mac select/copy its magnitude files into the primary work directory, then:

```sh
python -m tools.split_phase_v4.report
python -m tools.split_phase_v4.stage_runtime_assets
python -m tools.split_phase_v4.runtime_capture
python -m tools.split_phase_v4.certify
```

If any stage fails, improve the candidate or optimizer; never relax a gate.
Production export is separate, refuses a dirty tree, and requires a clean
CPU-only resume reproducing the final character hash.

## Binding final gates

```text
worst 2x-256x passband ripple       <= 1e-7 dB
worst composite complex error      <= 8e-9
character/image/independent alias  <= -160 dB
Rust runtime image/alias           <= -145 dB
low/high delay error               <= 1e-5 / 1e-4 sample
14 kHz phase join                  <= 2e-9 rad
transition delay overshoot         <= 0.05 sample
edge energy                        <= -215 dB
even/odd sum error                 <= 2e-15
step overshoot                     <= C * 1.005
runtime cost                       <= C * 1.05
runtime memory                     same class as C
C/D magnitude difference 20-20k   <= 0.0001 dB
```

D also needs at least three Pareto improvements of 15% or more and must beat B
and C on physical log-frequency group-delay curvature.

## Report back

Provide backend and exact dependency versions, command/thread counts, elapsed
time, peak RAM/VRAM, audit status, returned archive SHA-256, and the complete
accepted `magnitude_order_512.json` plus `.npz` work directory.
