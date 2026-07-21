# Split Phase B campaign archive

The P2-P17 tuning campaign is complete. Its one-off optimizers, unit tests,
candidate binaries, and intermediate reports were removed from the production
tree before promotion. They remain available in git history at campaign commit
`f5cf80f341ffd5db304dd3d4b735ab35f0e09117`.

The production artifacts now live in `assets/filters/split_phase_e3/`:

- `character_full_rate.f64le` is the frozen P17 `p17-balanced` character.
- `manifest.json` binds every coefficient table to its SHA-256.
- `certification.json` retains the promotion metrics and validation summary.
- `group_delay.csv` retains the deterministic 20 Hz-20 kHz phase trace.

Future timing work should use the reusable Rust `filter_timing_bench` and
`dsd_public_quality` executables. The `research-filter-assets` feature still
supports a hash-verified character override without adding another campaign
script collection to the repository.
