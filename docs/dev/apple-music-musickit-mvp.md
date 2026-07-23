# Apple Music MusicKit MVP

The `apple_music_musickit` feature is a deliberately isolated slice of the
native Apple Music integration. It contains two independently testable paths:

- Gate A: the signed MusicKit helper and private control IPC;
- Gate B/C proof: a provisional Music.app process tap feeding Fozmo's existing
  live PCM, Player, DSP, and selected-output path.

This path is separate from the legacy `apple_music_capture` virtual-device
experiment.

## What Gate A proves

- A real `LSUIElement` macOS helper app owns
  `ApplicationMusicPlayer.shared`.
- Fozmo launches one helper session and authenticates its protocol version,
  random launch token, session ID, child PID, and bundle ID.
- Communication uses length-prefixed JSON over an owner-only Unix socket.
- The helper implements authorization, catalog song lookup, queue preparation,
  play, pause, resume, stop, metadata, playback time, and clean shutdown.
- The Settings > Apple Music page exercises this lifecycle without creating a
  normal Fozmo source or changing app playback.

No Apple token or PCM crosses this IPC boundary, and the helper does not write
audio to disk.

## Build and run

The Music.app-to-DSP experiment does not need the helper, MusicKit entitlement,
provisioning profile, or paid Apple Developer membership. Start the normal
Music app, play a song, then run:

```sh
FOZMO_AIRPLAY_SOCKET=/tmp/fozmo-airplay/control.sock \
RUSTFLAGS="-C target-cpu=native" \
cargo run --locked --release --features apple_music_musickit -- \
  --lan --port=3001
```

Open Settings > Apple Music, confirm the system-audio experiment, and select
**Start Music → DSP**. macOS may ask once for Screen & System Audio Recording
access. Stopping the experiment destroys the private aggregate device and tap,
then restores Music.app's normal direct audio.

Build an ad-hoc Gate A helper separately for launch, IPC, lifecycle, and UI
testing:

```sh
./apple-music-helper/build-app.sh
cargo run --features apple_music_musickit
```

The helper app is written to
`target/apple-music-helper/FozmoAppleMusicHelper.app`. The Apple Music settings
tab is visible only in a macOS build with the feature enabled.

MusicKit is a restricted entitlement. An ad-hoc signature cannot carry it, so
the development page clearly disables authorization and song playback in an
ad-hoc build. A functional MusicKit build requires both a MusicKit-enabled
signing identity and matching provisioning profile:

```sh
FOZMO_APPLE_MUSIC_SIGN_IDENTITY="Apple Development: …" \
FOZMO_APPLE_MUSIC_PROVISIONING_PROFILE="/path/to/profile.provisionprofile" \
./apple-music-helper/build-app.sh
```

## What the Music.app process-tap proof does

- Finds the running `com.apple.Music` process and resolves its Core Audio
  `AudioHardwareProcess` object.
- Creates a private, include-only stereo process tap.
- Uses `CATapMutedWhenTapped`, so Music's original hardware path is suppressed
  only while Fozmo is actively reading the tap.
- Attaches the tap to a private aggregate device and reads it with a Core Audio
  IOProc.
- Writes F32 stereo PCM into the existing lock-free live-source ring; the
  callback performs no allocation, locking, logging, or IPC.
- Hands the live WAV adapter to the active Fozmo Player, so the normal EQ,
  resampling, volume, and output configuration apply.
- Exposes frames, callbacks, input RMS, callback age, and ring overruns on the
  isolated settings page.

No captured PCM is persisted.

### Source rate versus captured rate

The process tap exposes Music.app's rendered Core Audio stream, not the
catalog asset itself. In the first live proof, Music's AppleScript metadata
reported the current track at 44.1 kHz while `kAudioTapPropertyFormat` remained
48 kHz even after the Hegel output was clocked to 44.1 kHz and the tap was
recreated. Fozmo must therefore process the tap's actual 48 kHz PCM rate; it
must not relabel those samples as 44.1 kHz.

The tap's native representation is 32-bit IEEE Float PCM. Fozmo copies those
sample values into the live-source ring unchanged and widens F32 to F64 exactly
for DSP. It must not quantize the tap to Int16 or Int24: that would add a
lossy conversion without recovering the catalog asset's original depth.
Float32's 24-bit significand can represent every decoded 16-bit and 24-bit
integer PCM value exactly. Status therefore reports the 32-bit float container,
its 24-bit numerical precision, and the original source depth as unknown.

The Music metadata rate can be shown separately as a useful source hint and a
rate above 48 kHz can be labelled as Hi-Res-capable content. It is not proof
that Music selected the lossless variant or of the decoded bit depth. A robust
product implementation should keep the source hint and captured rate distinct.

The live proof also confirmed the captured 48 kHz stream can run through Split
Phase, DSD128, and 7th Order Search to a Hegel over 384 kHz DoP. This converts
the PCM actually received by Fozmo to the matching 48 kHz-family DSD128 rate
(6.144 MHz); it does not recover resolution discarded before the tap.

## Local development API

These routes are registered only in the feature-enabled local router and are
not exposed by Fozmo's remote-access surface:

- `GET /api/apple-music/status`
- `POST /api/apple-music/launch`
- `POST /api/apple-music/authorize`
- `POST /api/apple-music/dev/play-song`
- `POST /api/apple-music/transport`
- `POST /api/apple-music/stop`
- `POST /api/apple-music/shutdown`
- `POST /api/apple-music/process-tap/start`
- `POST /api/apple-music/process-tap/stop`

## Current boundary

The process-tap proof intentionally uses the normal Music app as its source.
Music remains responsible for catalog browsing, subscription playback,
transport, and metadata. Fozmo owns only the captured PCM-to-DSP/output path
for the duration of the experiment.

The full product path still needs the provisioned MusicKit helper, helper-PID
tap ownership, provider and `SourceRef` integration, queue/router behavior,
metadata propagation, interruption handling, and production packaging review.
