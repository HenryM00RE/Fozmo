# Zones

Zone registry, selection, snapshots, capabilities, and pairing logic live here.

This layer should decide where playback goes, not how audio samples are rendered.

Current shape:

- `mod.rs`: Public module surface and stable re-exports.
- `capabilities.rs`: Playback config assembly, DSP profile snapshots, protocol
  selection, backend labels, and local/remote device capability helpers.
- `active_zone_policy.rs`: Active/preferred/fallback selection rules and local
  controllability checks.
- `agent_bridge.rs`: Remote-agent registration, command dispatch, playback
  snapshots, and remote buffer/signal updates.
- `manager.rs`: Zone manager facade over registry, policy, remote-agent bridge,
  persistence, snapshots, capabilities, and local discovery sync.
- `model.rs`: Zone id normalization, deterministic local-device zone ids, and
  local zone display-name helpers.
- `pairing.rs`: Pairing token creation and verification.
- `persistence.rs`: Application of DB-backed zone definitions to in-memory
  zones.
- `registry.rs`: Local and remote zone storage types.
- `snapshot.rs`: UI-facing `ZoneProfile` construction.
