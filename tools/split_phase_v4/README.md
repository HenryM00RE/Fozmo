# Split Phase D

Split Phase D (`SplitPhase128kV4`, stable ID 36) is an audit-complete frozen-filter build. Its production magnitude begins with a real Fejér–Riesz Gram matrix constrained by a genuine PSD cone. A conventional window, Remez result, FIRLS result, or rejected warm start cannot be exported as D.

The build keeps Split Phase C immutable and records its hashes in `baselines/split_c.json`. A, B, and C are measured with the same temporal origin, band-limited energy, physical log-frequency delay derivatives, interpolation-image model, and independently propagated decimation-alias model.

## Reproduce

Create an isolated environment from `requirements.lock`, then run from the repository root:

```sh
python -m tools.split_phase_v4.multirate_model
python -m tools.split_phase_v4.baseline
python -m tools.split_phase_v4.magnitude_sdp --order 512 --solver SCS
python -m tools.split_phase_v4.report
python -m tools.split_phase_v4.stage_runtime_assets
python -m tools.split_phase_v4.runtime_capture
python -m tools.split_phase_v4.certify
```

The order-512 SDP and genuine arbitrary-precision certification are intentionally long-running. SCS uses its indirect linear solver because a direct order-512 KKT factorization exceeds the practical memory budget on the reference 16 GB build machine. The magnitude solver escalates to orders 768 and 1024 only when the independently verified lower order is infeasible.

Production export is deliberately separate:

```sh
python -m tools.split_phase_v4.export_assets
```

It refuses any dirty source tree. The manifest records the clean source/generator commit, dependency-lock hash, seed, solver objectives and gap, PSD eigenvalue, objective histories, runtime certification, and every coefficient hash. A clean CPU-only resume must reproduce the character hash before release.

The UI label remains **Split Phase D** until the listening gate has been completed. A, B, and C remain visible meanwhile.
