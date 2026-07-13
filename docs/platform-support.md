# Platform Support

Fozmo is still experimental. This page describes the intended support shape
without treating untested hardware or network targets as release-ready.

## Current Baseline

- macOS is the current known-good development platform for the normal
  verification loop.
- CI runs frontend Biome checks, frontend tests, frontend build, npm
  audit/license checks, Playwright smoke and real-server E2E, Swift launcher
  tests, LAN and remote-access smoke tests, Rust formatting, Clippy,
  default/no-default checks, named Qobuz/local-playback and network/control
  feature Clippy checks, the MIT AirPlay helper client and standalone helper
  checks, cargo-deny, release-native Rust tests, and no-default-feature Rust
  target checks on macOS, Linux, and Windows. The SDK-dependent ASIO feature is
  not built by the normal CI matrix.
- Weekly scheduled CI also runs the full Playwright E2E suite and broader
  release-native Rust coverage to catch dependency and platform drift.
- Playback quality, latency, DSD behavior, AirPlay, Sonos, Hegel, Qobuz, and
  remote-agent compatibility remain hardware or service specific. Release
  claims should be limited to combinations recorded in the manual evidence.

## Required CI Checks

Protect the public repository's default branch with required checks matching
the stable workflow job names:

- `Frontend fast`
- `E2E smoke`
- `E2E real Rust server`
- `Security hygiene`
- `Swift launcher`
- `LAN pairing smoke`
- `Remote access smoke`
- `Rust quality`
- `Rust cross-platform check (macos-latest)`
- `Rust cross-platform check (ubuntu-latest)`
- `Rust cross-platform check (windows-latest)`
- `Rust release tests`

The branch ruleset should require branches to be up to date before merging and
block merges when any required check fails. During the repository transfer the
workflow accepts direct pushes to both `main` and `master`; narrow that filter
after choosing the public repository's default branch. GitHub stores the
ruleset as repository settings, so the workflow defines the checks and this
document records the required policy.

## macOS

Expected areas:

- CoreAudio playback through available output devices.
- Device volume integration where supported by the selected output.
- DoP carrier-rate behavior on compatible DACs.
- LAN core and agent operation on trusted private networks.

Release checks:

- Smoke-test PCM playback on the default output and a selected external device.
- Smoke-test DoP only on hardware known to support it.
- Confirm startup and playback behavior in release mode, not only debug mode.

## Windows

Expected areas:

- WASAPI exclusive playback.
- ASIO playback when the `asio` feature and driver prerequisites are available.
- Native DSD paths on supported ASIO devices.

The normal CI matrix checks Windows without default features. It does not
compile the optional `asio` feature because that path requires the Steinberg
ASIO SDK and LLVM/libclang in the build environment. Do not treat the ordinary
Windows check as ASIO compile or device evidence.

Release checks:

- Confirm the build environment includes any required ASIO SDK or driver
  prerequisites.
- Smoke-test WASAPI exclusive output with at least one real device.
- Smoke-test ASIO/native DSD only on known compatible hardware.

## Network And Service Targets

These targets are useful but should remain documented as experimental until
they are tested against real devices or services:

- Qobuz: login, search, favorites, stream resolution, and radio.
- AirPlay and AirPlay 2: discovery, unsupported receiver handling, volume, and
  metadata.
- Sonos: discovery, transport control, stream proxy behavior, and metadata.
- Hegel: power, input selection, volume limits, and status parsing.
- Remote agents: pairing, advertised capabilities, command delivery, stream
  pull, reconnect, and status reporting.

## Documentation Rule

Do not claim broad platform support from compile coverage alone. Public-facing
docs should say "tested" only for combinations that have passed both automated
verification and a real playback or device smoke test.

Record real-device and service evidence in
[manual-smoke-tests.md](manual-smoke-tests.md).
