# Settings

Settings storage, profiles, and playback-specific configuration live here.

Current shape:

- `mod.rs`: Public module surface, stable re-exports, and focused tests.
- `model.rs`: Persisted settings aggregate DTO and Hegel/default profile
  defaults.
- `store.rs`: `SettingsStore`, settings load/save behavior, and generic update
  methods.
- `profiles.rs`: Profile normalization, active-profile fallback behavior, and
  profile mutation helpers.
- `playback.rs`: Per-zone playback settings fallback and legacy field mirroring.
- `dsd.rs`: DSD source rule DTOs.
- `validation.rs`: Settings-file parse fallback and validation-focused tests.

The current user-facing audio defaults are documented in
`docs/dsp.md`; this module keeps legacy field compatibility
and per-zone mirroring separate from that product-level guidance.
