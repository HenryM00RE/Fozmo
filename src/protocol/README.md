# Protocol

Shared internal contracts and message types live here.

Current shape:

- `mod.rs`: Public re-exports that preserve the `crate::protocol::*` import
  surface.
- `source.rs`: Source references and source identity helpers.
- `sink.rs`: Sink protocols and system audio backend labels.
- `playback.rs`: Playback configuration DTOs.
- `zone.rs`: Zone profile, zone status, DSP profile, and zone capabilities.
- `agent.rs`: Core/agent commands, messages, playback state, buffer state, and
  agent capabilities.
- `signal.rs`: Signal-path snapshots.
