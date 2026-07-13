# Audio Engine

Low-level local playback engine code lives here.

Current shape:

- `player.rs`: Public `Player` API, command channel, snapshots, metadata state,
  device selection, and worker startup.
- `commands.rs`: Player command and queue item types.
- `command_dispatch.rs`: Worker-side command dispatch.
- `worker_loop.rs`: Audio worker event loop.
- `worker_state.rs`: Worker runtime state and shared handles.
- `worker_status.rs`: Playback state, metrics, notices, and status publishing.
- `queue_state.rs`: Local file and stream queue progression.
- `decode.rs`: Symphonia decode helpers.
- `metadata.rs`: Track tag and cover extraction.
- `session.rs` and `session_start.rs`: Media session setup, source offsets,
  and marker handling.
- `playback_step.rs`: Per-iteration playback advancement.
- `signal_path.rs`: Output mode, transport, and signal-path planning.
- `render.rs`: PCM rendering, DSP integration, and DSD handoff.
- `output_stage.rs`, `output_open.rs`, and `output_stream.rs`: Output session
  selection, opening, reset, and retry behavior.
- `pcm_output.rs` and `dsd_output.rs`: PCM and DSD write paths.
- `dsd_path.rs`: DSD target selection and fallback timing.
- `buffers.rs`: Audio ring-buffer capacity and write helpers.
- `state.rs`: Atomic playback state shared by the worker and output backends.

Engine internals should stay behind the `Player` API outside `src/audio/`.
