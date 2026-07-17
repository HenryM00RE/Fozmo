# Split Phase V3 generator

This package generates the immutable `SplitPhase128kV3` coefficient bundle. The
reduced autocorrelation problem is the only SDP stage. The final long FIR and
the multistage cleanup filters are optimized in their finite runtime supports.

The magnitude solver uses the exact eigenvalue representation of a circulant
positive-spectrum LMI: non-negativity of the circulant eigenvalues is imposed
on an exchange grid, then verified on a 2^22 grid and augmented at every
off-grid extremum. This avoids materializing a dense 1025-square PSD cone on
machines where its factorization would dominate the actual filter design.
Every solver iterate is rechecked on that dense grid before it can replace the
current feasible positive-spectrum incumbent. In particular,
`optimal_inaccurate` is recorded but rejected when its actual ripple,
nonnegativity, monotonicity, or stopband performance regresses.

The minimum-phase reference is factored as the finite order-1024
autocorrelation polynomial. This avoids the half-period cepstral artefact that
appears when a deep-stop spectrum is treated as an infinite periodic impulse
and then truncated.

Create a pinned environment and run the resumable production job:

```bash
python3 -m venv /tmp/fozmo-split-v3
/tmp/fozmo-split-v3/bin/pip install -r tools/split_phase_v3/requirements.txt
/tmp/fozmo-split-v3/bin/python -m tools.split_phase_v3.optimize_system \
  --config tools/split_phase_v3/production.toml \
  --sdp-solver auto \
  --resume
```

Checkpoints and the append-only run log live in `tools/split_phase_v3/work/`
and are intentionally ignored by Git. A completed run writes little-endian
`f64` assets and `manifest.json` to `assets/filters/split_phase_v3/`.

The run is deterministic for a fixed configuration, dependency set, source
tree, and platform. It never uses programme audio or runtime environment
variables.
