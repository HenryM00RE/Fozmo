# Apple Music Live Capture

Apple Music live capture routes the native Apple Music app's playback through
the `Fozmo Capture` CoreAudio HAL virtual device and feeds the captured PCM
into Fozmo's existing player/DSP engine (resampler, EQ, dither, PCM/DSD
output) as a live stream. It is macOS-only, isolated to Settings > Apple
Music, and does not add Apple Music to library, queue, history, radio, or
provider playback flows.

The HAL driver itself is documented in
[`drivers/fozmo-capture/README.md`](../../drivers/fozmo-capture/README.md).

## How a session works

1. `POST /api/apple-music-capture/start` verifies the guardrails (below), and
   — when auto-routing is enabled — saves the current macOS default output and
   switches it to `Fozmo Capture`.
2. The service reads the driver's current nominal rate and opens a CPAL input
   stream that must match it exactly: F32, stereo, driver rate. The real
   capture path never converts formats or accepts a different rate — a
   mismatch is an error.
3. Captured frames flow through a lock-free ring into a live WAV
   (IEEE float) `MediaSource`, which the engine plays through
   `Player::play_stream_if_epoch` as "Apple Music (Live)". From there the
   normal DSP path applies (upsampling, EQ, dither, DSD rendering, device
   output).
4. While capture runs, a poller reads the Music app once a second via
   AppleScript for player state, track identity, metadata, and output volume.
   On a track change it also reads a tightly filtered eight-second window from
   macOS Unified Logging and parses the source format emitted by Apple's
   lossless decoder. This path controls the native Music app and does not
   require MusicKit.
5. On stop (or app shutdown), the session EOFs the live stream, stops the
   player, closes the capture stream, and restores the saved macOS default
   output.

During Apple Music pauses the session keeps running on the driver's zero-fill
underrun output, so resume is instant.

## Automatic rate switching

When the current Music track changes, the service looks for either of these
private log forms:

- `ACAppleLosslessDecoder ... Input format: 2 ch, 96000 Hz from 24-bit source`
- newer Music `audioCapabilities:` events containing `asbdSampleRate` and,
  when present, `sdBitDepth`

The newest event supplies the native source sample rate and optional source
bit depth. If its rate maps to a different supported device rate, the service
performs a debounced switch: end the live session, set the driver nominal rate
through the CoreAudio configuration-change handshake (confirmed by polling,
3 s timeout), then reopen capture and start a fresh DSP session. Overlapping
log windows carry an event marker so the preceding track's decoder event is
not applied again.

When no new decoder event is available but AppleScript reports a rate (usually
for a downloaded file), that value is used as an explicit fallback. For a
streaming track where both sources are unavailable, the service probes up to
five times and then leaves the capture device at its current rate. It never
silently invents a 44.1 kHz source rate.

A successful manual override suppresses remaining automatic probes for the
current track. Automatic detection resumes when the poller observes the next
track identity.

Limitations:

- Decoder messages are private Apple implementation details. Their subsystem,
  process attribution, or wording can change in a macOS/Music update.
- Reading the local Unified Log requires a non-sandboxed process and can
  require an administrator account. Failure is surfaced in status; the manual
  rate override (`POST /api/apple-music-capture/rate`) remains the escape
  hatch.
- Track polling is ~1 s, so the first moments of a new track can play at the
  previous device rate before the switch lands.
- A rate change causes a brief, audible gap while Apple Music and CoreAudio
  resync.

## Guardrails

- Capture only starts when the local Fozmo player sends audio to an **explicit
  physical output device** — not the system default and not `Fozmo Capture`.
  Anything else could loop Fozmo's own output back into the capture device
  once auto-routing flips the system default.
- The capture device is pinned to the `Fozmo Capture` UID
  (`com.fozmo.audio.capture`); other input devices are rejected.
- Auto-routing saves and restores the user's default output; it can be
  disabled in settings for manual routing.

## Bit-perfect checklist

- Music app volume at 100% (status warns otherwise).
- Sound Check off, EQ off, crossfade off in the Music app.
- Lossless / Hi-Res Lossless enabled in Music playback settings.
- Fozmo zone output set to a real physical device.
- macOS TCC permissions granted: automation (AppleScript control of Music)
  and microphone/input capture for the Fozmo process.

## API surface

- `GET /api/apple-music-capture/status` — includes driver telemetry
  (ring fill, underruns, overruns, latency snaps), detected source rate/bit
  depth, detection source (`apple_decoder_log`,
  `music_audio_capabilities_log`, `music_applescript`, or `manual_override`),
  rate-switch state, Music volume, and quality warnings.
- `GET/POST /api/apple-music-capture/settings` — devices, buffer target,
  auto-routing toggle.
- `GET /api/apple-music-capture/devices`
- `POST /api/apple-music-capture/start`
- `POST /api/apple-music-capture/stop`
- `POST /api/apple-music-capture/rate` — manual rate override
  (`{"rate_hz": 96000}`); restarts a running capture at the new rate.
- `GET /api/apple-music-capture/metrics`
- `GET /api/apple-music-capture/music-app/status`
- `POST /api/apple-music-capture/music-app/control`

## Verification

- Driver level: `drivers/fozmo-capture/scripts/diagnose.sh`,
  `system_profiler SPAudioDataType`, Audio MIDI Setup rate switching, and
  `log show --predicate 'process == "coreaudiod"'`.
- Bit-perfect loopback: `cargo run --release --bin fozmo_capture_loopback`
  (marker + PRBS, sample-exact assert at 44.1/96/192 kHz, flat
  underrun/overrun/snap counters).
- Rate switching: cycle supported rates through the manual rate API and watch
  nominal-rate telemetry; then play known 44.1 and 96 kHz lossless tracks and
  confirm `rate_detection_source`, `detected_track_rate_hz`,
  `detected_source_bit_depth`, nominal-rate telemetry, and the reopened DSP
  session agree.
- Soak: a 30-minute album with `/api/apple-music-capture/status` showing
  stable ring fill and near-zero underruns/overruns/snaps after startup.

## Product boundaries

- Live processing only. No recording, export, PCM dumps, or file creation.
- No hidden background capture — capture runs only after an explicit start.
- No DRM bypass and no Apple Music stream URL extraction; the audio is
  whatever macOS plays to the default output.
- Captured PCM remains stereo Float32 at the CoreAudio device rate. Reported
  source bit depth describes Apple's decoded asset, not the Float32 capture
  container.
