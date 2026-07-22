# Audio Pipeline

This document describes the current playback pipeline and module ownership.

The low-level local playback path is organized under `src/audio/engine/`, with
DSP, DSD, hardware output, and network sink behavior owned by sibling audio
module families.

## High-Level Flow

1. A product-level playback request arrives through an API route, queue action,
   Qobuz action, Sonos action, AirPlay-capable output, or remote-agent command.
2. The playback layer resolves source references and builds a `PlaybackIntent`.
3. `PlaybackRouter` selects the zone sink for the target output.
4. Local-player paths call the `Player` API, which sends a `PlayerCommand` to
   the audio worker.
5. The worker resolves the media source, opens a session, and decodes audio
   through Symphonia.
6. The render path applies gain staging, EQ, resampling, dither, DSD rendering,
   and output-mode preparation.
7. The output path writes local PCM, DoP, or native DSD; hands standard PCM and
   coarse control data to the standalone AirPlay helper; or exposes a
   Sonos-accessible stream to the selected target.
8. Status snapshots flow back to outputs, API responses, the frontend, and the
   listening-history tracker.

## Engine

The `Player` API lives in `src/audio/engine/player.rs` and is re-exported as
`crate::audio::player`. It owns the command channel, shared snapshot state,
track metadata, output-device selection, EQ config, stream queue counters, and
worker startup.

Focused engine modules own the rest of the local playback control flow:

- `commands.rs`: Player command, local queue item, and stream queue item types.
- `command_dispatch.rs`: Worker-side command handling.
- `worker_loop.rs`: Audio worker event loop.
- `worker_state.rs`: Shared worker state and runtime buffers.
- `worker_status.rs`: Playback state, metrics, notices, and status publishing.
- `queue_state.rs`: Local and stream queue progression.
- `decode.rs`: Symphonia decode helpers.
- `metadata.rs`: Track metadata and cover extraction.
- `session.rs` and `session_start.rs`: Media session setup and marker/offset
  handling.
- `playback_step.rs`: Per-iteration playback advancement.
- `signal_path.rs`: Output mode, transport, and playback-chain planning types.
- `render.rs`: PCM rendering, headroom, DSP, and DSD handoff.
- `output_stage.rs`, `output_open.rs`, and `output_stream.rs`: Output session
  selection, opening, reset, and retry behavior.
- `pcm_output.rs` and `dsd_output.rs`: PCM and DSD write paths.
- `dsd_path.rs`: DSD mode selection, fallback keys, and retry timing.
- `buffers.rs`: Audio ring-buffer capacity and write helpers.
- `state.rs`: Atomic playback state shared by the worker and output backends.

Engine internals should remain behind the `Player` API outside `src/audio/`.

## Decode

Decode is owned by the audio engine.

Responsibilities:

- Open local files, ranged streams, and proxied service streams.
- Read metadata needed for status and now-playing display.
- Decode supported formats through Symphonia.
- Preserve seek behavior, offset media sources, and marker handling.
- Feed PCM frames into the selected playback chain.

Decode code stays close to worker state, command handling, and seeking because
those pieces share the same control flow.

## PCM DSP

PCM DSP lives under `src/audio/dsp/` and is integrated by the engine render and
output modules:

- `resampler.rs`: Sinc, polyphase, FFT FIR, direct FIR, 2x stages, integer
  cascades, stage planning, and resampler tests.
- `eq.rs`: Parametric EQ model, processor, coefficient ramping, and response
  tests.
- `dither.rs`: Dither modes and dither state for PCM output.

Responsibilities:

- Choose the effective sample rate and output mode.
- Apply profile EQ and ramp coefficient changes safely.
- Apply headroom before clipping-sensitive processing.
- Resample PCM for local hardware, AirPlay, Sonos, and DSD rendering.
- Quantize or dither PCM when writing integer output.
- Preserve status details for the Playback Chain shown in the UI.

The resampler has broad tests; sample-format conversion and output-stage
changes should keep focused coverage near the code they exercise.

## DSD

DSD rendering lives under `src/audio/dsd/` and is integrated by the engine DSD
path:

- `dsd_render.rs`: PCM-to-DSD rendering, rate selection, DSD upsampler chains,
  and delta-sigma integration.
- `delta_sigma.rs`: Delta-sigma modulation and noise helpers.
- `dsd_coeffs.rs`: Modulator coefficient data.
- `dop.rs`: DSD-over-PCM packing and idle markers.
- `native_dsd.rs`: Native DSD byte packing and channel ordering.

Responsibilities:

- Pick the requested DSD target mode from settings and source rules.
- Preserve source clock-family behavior where required.
- Render PCM to DSD64, DSD128, or DSD256.
- Pack DSD as DoP for CoreAudio-compatible paths.
- Pack native DSD for ASIO/vendor-driver paths.
- Fall back to PCM when a requested DSD path is not supported by the selected
  device.

The product's **Split Phase** filter is the promoted
P17 `SplitPhase128kE3` reconstruction path. The former E2v3 and Smooth Phase
identifiers are retained only for migration and internal diagnostics. The
selectable **7th Order Search**
modulator is `EcBeam2`, a fixed M4/N8 beam search used at −2 dB headroom with
zero DSD ISI compensation. The current personal default documented for Fozmo
is Split Phase with 7th Order Search at DSD128; this is a listening preference,
not a pipeline requirement. See [DSP](dsp.md) for the user-facing options and
[Split Phase DSD Measurements](Measurements.md) for the measured digital
results.

DSD code is especially sensitive to platform support and device capabilities.
Keep rate-selection tests and native/DoP packing tests close to the code, and
do not remove fallback paths without device-specific evidence.

## Local Output

Local hardware output lives under `src/audio/output/` and is opened or driven
by the engine output modules:

- `device_caps.rs`: Device capability probing and output-rate policy.
- `device_volume.rs`: Platform-specific device volume support.
- `sample_format.rs`: PCM sample-format packing and conversion.
- `coreaudio_hog.rs`: macOS CoreAudio hog-mode helper.
- `wasapi_exclusive.rs`: Windows WASAPI exclusive output.
- `asio_output.rs`: Windows ASIO and native DSD output.

Responsibilities:

- Enumerate and select local devices.
- Match source, profile, and device capabilities to an output mode.
- Open platform-specific streams.
- Apply volume and device-volume controls.
- Handle PCM, DoP, and native DSD writes.
- Report playback state and transport capabilities.

Keep `#[cfg(target_os = "...")]` and feature-gated modules compiling on their
target platforms. A module that looks unused on macOS may still be the Windows
ASIO or WASAPI path.

## Network Sinks

Network playback sink code lives under `src/audio/sinks/`:

- `airplay/`: MIT-side facade for the standalone helper, coarse receiver
  modeling, opaque-ID selection, helper IPC, and standard PCM conversion.
- `sonos.rs`: Sonos discovery/control/playback service, DIDL metadata,
  cached/proxied audio and art, speaker transport control, and status
  snapshots.

The standalone GPL program under `airplay-helper/` owns AirPlay DNS-SD
discovery and classification, receiver network targets, pairing and
encryption, RTSP/RTP transport, and AirPlay codec handling. Those protocol
details do not live in or cross into the MIT server. The server sees only
helper-published coarse receiver state and opaque IDs, and sends the helper
documented standard PCM plus coarse metadata and control commands.

Remote-agent orchestration lives outside the audio sink modules in `src/agent.rs`
and `src/zones/`, because it is a paired core/agent protocol path rather than a
single audio transport backend.

MIT sink responsibilities:

- Consume helper-published AirPlay receiver availability and discover Sonos
  receivers through the Sonos integration.
- Prepare local or service media for network transport.
- Convert audio to the standard PCM boundary required by the AirPlay helper or
  to the format required by the Sonos stream path.
- Maintain MIT-side helper IPC or Sonos transport sessions and metadata.
- Surface remote status in the same output model as local playback.

## Sink Routing

Sink routing is the boundary between product playback intent and the low-level
audio path.

`src/playback/` resolves a source once and routes it through the target output:

- Local output: call the local `Player` API.
- Remote agent output: send a protocol command to the paired agent.
- AirPlay output: select the local-player-backed opaque AirPlay target and
  stream standard PCM through the MIT facade to the standalone helper.
- Sonos output: prepare a Sonos-accessible stream and DIDL metadata.

This routing preserves one visible playback status shape for the UI and one
listening-history path for completed listens.
