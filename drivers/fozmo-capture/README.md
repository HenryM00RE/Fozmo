# Fozmo Capture HAL Driver

`Fozmo Capture` is a Fozmo-owned macOS CoreAudio HAL virtual duplex device. macOS
(and therefore the native Apple Music app) plays into its output stream; Fozmo
reads the same PCM back from its input stream and feeds it into the existing
resampler / EQ / dither / DSD pipeline as a live source.

The driver is implemented in `src/FozmoCapture.cpp` and installs to:

```text
/Library/Audio/Plug-Ins/HAL/FozmoCapture.driver
```

## Device contract

- Duplex virtual CoreAudio device, UID `com.fozmo.audio.capture`.
- Stereo 32-bit float PCM on both streams.
- Supported nominal rates: 44100, 48000, 88200, 96000, 176400, 192000 Hz.
- `Latency = 0` and `SafetyOffset = 0`.
- Deliberately **no volume or mute controls**: the device must stay
  bit-transparent, so there is nothing for coreaudiod to scale.

## Implementation notes

- **Ring buffer.** Output-to-input transfer uses a lock-free ring with
  monotonic 64-bit sample indices and at most two contiguous `memcpy` slices
  per transfer. Overrun drops oldest samples; underrun zero-fills.
- **Bounded-latency snap.** If the input reader falls more than
  `8 * kBufferFrames` frames behind the writer, the read index snaps to
  `write - 2 * kBufferFrames` frames and the `trsn` snap counter increments,
  keeping live latency bounded instead of serving stale backlog.
- **Clocking.** `GetZeroTimeStamp` returns NullAudio-style quantized
  timestamps: whole `kBufferFrames` (512) periods advanced from a host-time
  anchor using `mach_timebase_info` cached at `Initialize`. The clock anchor
  and ring reset only on the 0 → 1 running-client transition, so a second
  client cannot smash the timeline of a running one.
- **Rate changes.** `SetPropertyData(NominalSampleRate)` (and rate-only stream
  format writes) validate, then request the change through the CoreAudio host
  configuration-change handshake. `PerformDeviceConfigurationChange` is the
  only place a rate is applied: it sets the rate, resets the ring, recomputes
  host ticks per period, re-anchors the clock, bumps the clock seed, and
  publishes `PropertiesChanged` for the device rate and stream formats.
- **Realtime rules.** No allocation, locks, logging, CF calls, filesystem
  access, Objective-C, or Swift runtime calls in `StartIO`, `StopIO`,
  `GetZeroTimeStamp`, or `DoIOOperation`. Wall-clock telemetry uses
  `clock_gettime_nsec_np(CLOCK_REALTIME)` (commpage read, unix-epoch ms).

## Telemetry selectors

Readable via `AudioObjectGetPropertyData` on the device (used by the app's
Apple Music Capture status page and `fozmo_capture_loopback`):

| Selector | Type | Meaning |
| --- | --- | --- |
| `trvr` | CFString | Driver version (matches `CFBundleShortVersionString`) |
| `trbf` | UInt32 | Period size in frames (`kBufferFrames`) |
| `trff` | UInt64 | Ring fill in frames |
| `trfm` | Float64 | Ring fill in ms |
| `trun` | UInt64 | Underruns (input read with empty ring) |
| `trov` | UInt64 | Overruns (output write dropped oldest) |
| `trsn` | UInt64 | Bounded-latency input snaps |
| `trrc` | UInt64 | Last rate change, unix ms |
| `trst` | UInt64 | Last IO start, unix ms |
| `trsp` | UInt64 | Last IO stop, unix ms |

Only the version string is declared in `kAudioObjectPropertyCustomPropertyInfoList`
(scalar types are not valid custom-property data types); the scalar counters
are readable by known selector.

## Build, install, verify

```sh
./scripts/build.sh      # compile + sign the bundle into build/
sudo ./scripts/install.sh   # copy to /Library/Audio/Plug-Ins/HAL and restart coreaudiod
./scripts/diagnose.sh   # check that coreaudiod published the device
```

Signing: the build script prefers an identity from `CODESIGN_IDENTITY`, then
falls back to any Apple Development / Developer ID identity, then ad-hoc.
**Ad-hoc-signed HAL drivers may build and load but be silently rejected by
modern `coreaudiod`** — if the device never appears in Audio MIDI Setup, check
`log show --predicate 'process == "coreaudiod"'` and use an Apple Development
identity for local work.

Bump `CFBundleVersion`/`CFBundleShortVersionString` in `Info.plist` **and** the
`kDriverVersion` string in `FozmoCapture.cpp` together whenever the driver
changes, so the app can prove which bundle CoreAudio actually loaded.

## Bit-perfect loopback check

With the driver installed, run:

```sh
cargo run --release --bin fozmo_capture_loopback -- --rates 44100,96000,192000 --secs 30
```

It plays a marker burst + PRBS signal into the output stream, reads it back
from the input stream, and asserts sample-exact equality plus flat
underrun/overrun/snap counters after lock.
