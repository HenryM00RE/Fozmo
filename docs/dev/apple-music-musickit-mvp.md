# Apple Music integration

The `apple_music_musickit` feature provides Fozmo's Apple Music product route
on macOS.

## Architecture

MusicKit is the authorization, catalog, and queue-building plane. It does not
render audio:

```text
Apple Music catalog
        |
        v
signed MusicKit helper (authorization, catalog, queue playlist)
        |
        v
SourceRef::AppleMusicTrack -> PlaybackRouter
        |
        v
Music.app "Fozmo" playlist -> Fozmo Capture -> Player -> DSP -> local output
```

Fozmo rebuilds a dedicated Music.app playlist named `Fozmo` so it holds exactly
the upcoming Apple Music run, starts it with `play playlist "Fozmo"`, verifies
the fresh Music.app decoder format, configures Fozmo Capture to that supported
rate, and starts the normal local Player path. Pause, resume, seek, next, stop,
listening history, and status remain owned by Fozmo.

### Why a playlist

Two Music.app behaviours force this design.

Its AppleScript `current track` does not report Apple Music catalog tracks that
are not in the user's library, so Fozmo could not tell whether a track it asked
for had actually started. Promoting each queued song to a library item inside
the playlist makes `database ID` readable, which is the identity the
verification loops match against.

Music.app also only advances a queue gaplessly when playback started from a
container it owns. `play track N of playlist` starts a single-track transport
that stops at the end of that track, so Fozmo keeps the playlist equal to the
upcoming run and always enters it at the top. Everything Music.app can then
reach on its own is a track Fozmo queued, which is what makes arbitrary
Apple-to-Apple boundaries gapless rather than only same-album ones.

The run stops at the first non-Apple source, and at a sample-rate change, since
neither can be crossed inside one capture session.

### Rejected alternatives

- **`ApplicationMusicPlayer`** (MusicKit's own player) was measured rendering
  AAC rather than Apple Lossless. That breaks the bit-perfect guarantee, so
  MusicKit is not used as a renderer. Measurements are in the spike section
  below.
- **Accessibility automation.** Fozmo used to open a `music://` album URL and
  post a synthetic double-click at the track row. It required Accessibility
  permission, forced Music.app to the foreground, moved the user's pointer, and
  cost several seconds per track. The queue playlist replaced it entirely.
- **`MusicLibrary`** (MusicKit's library write API) is
  `@available(macOS, unavailable)` in every form — `add`, `add(_:to:)`,
  `createPlaylist`, `edit`. The helper therefore builds the playlist through the
  Apple Music Web API using `MusicDataRequest`, which is available on macOS and
  signs requests with the provisioned app's tokens.

### Requirements

Adding subscription tracks to a library needs **Sync Library** enabled in
Music → Settings → General. The helper's `library_status` command reports when
Apple refuses library writes so Fozmo can name that setting rather than failing
opaquely.

Because Fozmo rewrites the `Fozmo` playlist, edits made to it by hand are lost,
and queued tracks are added to the user's Apple Music library and sync to their
other devices.

The Apple Music source is restricted to an explicit local physical Core Audio
output. Fozmo rejects the system-default output, Fozmo Capture itself, virtual
outputs, and network renderers to prevent feedback and unsupported routing.

The helper protocol exposes only:

- authorization and subscription status;
- song and album lookup;
- song and album search;
- library-write status and queue-playlist sync;
- lifecycle commands.

It has no MusicKit player, renderer queue, or playback transport.

## ApplicationMusicPlayer lossless spike (2026-07-26)

The programmatic-player alternative was tested first inside the provisioned,
signed `com.fozmo.apple-music-helper` app. The temporary, environment-gated
`spike_play` command queued Björk's “Jóga” (`1726654451`) followed by “Unravel”
from *Homogenic* (`1726654447`). macOS system output and input were both set to
Fozmo Capture for the test.

The spike failed the mandatory lossless criterion, so its command and player
code were removed again and the product architecture above remains settled:

| Check | Result |
|---|---|
| Plays from signed helper | **Fail** — authorization and subscription checks passed, but the post-`play()` state event reported `paused`, not `playing`. |
| Decoder | **Fail** — `RemotePlayerService` PID 99240 selected `ACMP4AACBaseDecoder`, not `ACAppleLosslessDecoder`. |
| Advertised rate/variant | **Fail** — the catalog exposed lossless variants, while the render decoder was 44.1 kHz AAC. |
| Internal gapless queue advance | Not measured; the spike was stopped when the mandatory decoder check failed. |
| Music.app transport interaction | No transport hijack was observed during the short run; this was not a long enough run to establish compatibility. |
| Published state/current entry | **Pass** — Combine emitted player state and queue/current-entry changes. |
| 44.1 → 96 kHz entry change | Not measured after the mandatory decoder failure. |

This is the raw output from the required PID-independent decoder predicate:

```json
{"timezoneName":"","messageType":"Default","eventType":"logEvent","source":null,"formatString":"%25s:%-5d (%p) Input format: %s","userID":501,"activityIdentifier":0,"subsystem":"com.apple.coreaudio","category":"ac","threadID":11498972,"senderImageUUID":"0F483193-D206-362B-9874-0100BCCB9CBD","backtrace":{"frames":[{"imageOffset":103060,"imageUUID":"0F483193-D206-362B-9874-0100BCCB9CBD"}]},"bootUUID":"3590A0B1-4C7D-4C4A-9F0B-69FAC3D0BD58","processImagePath":"\/System\/Library\/Frameworks\/MediaPlayer.framework\/Versions\/A\/XPCServices\/RemotePlayerService.xpc\/Contents\/MacOS\/RemotePlayerService","senderImagePath":"\/System\/Library\/Components\/AudioCodecs.component\/Contents\/MacOS\/AudioCodecs","timestamp":"2026-07-26 18:11:22.348728+1200","machTimestamp":20147685217463,"eventMessage":"  ACMP4AACBaseDecoder.cpp:310   (0xabc180e00) Input format:  2 ch,  44100 Hz, aac  (0x00000000) 0 bits\/channel, 0 bytes\/packet, 1024 frames\/packet, 0 bytes\/frame","processImageUUID":"4044151C-2EEE-3A42-9F88-BDEE75975064","traceID":41979461356879876,"processID":99240,"senderProgramCounter":103060,"parentActivityIdentifier":0}
{"timezoneName":"","messageType":"Default","eventType":"logEvent","source":null,"formatString":"%25s:%-5d (%p) Input format: %s","userID":501,"activityIdentifier":0,"subsystem":"com.apple.coreaudio","category":"ac","threadID":11498972,"senderImageUUID":"0F483193-D206-362B-9874-0100BCCB9CBD","backtrace":{"frames":[{"imageOffset":103060,"imageUUID":"0F483193-D206-362B-9874-0100BCCB9CBD"}]},"bootUUID":"3590A0B1-4C7D-4C4A-9F0B-69FAC3D0BD58","processImagePath":"\/System\/Library\/Frameworks\/MediaPlayer.framework\/Versions\/A\/XPCServices\/RemotePlayerService.xpc\/Contents\/MacOS\/RemotePlayerService","senderImagePath":"\/System\/Library\/Components\/AudioCodecs.component\/Contents\/MacOS\/AudioCodecs","timestamp":"2026-07-26 18:11:22.357170+1200","machTimestamp":20147685420088,"eventMessage":"  ACMP4AACBaseDecoder.cpp:310   (0xabc180e00) Input format:  2 ch,  44100 Hz, aac  (0x00000000) 0 bits\/channel, 0 bytes\/packet, 1024 frames\/packet, 0 bytes\/frame","processImageUUID":"4044151C-2EEE-3A42-9F88-BDEE75975064","traceID":41979461356879876,"processID":99240,"senderProgramCounter":103060,"parentActivityIdentifier":0}
{"count":2,"finished":1}
```

The command used was:

```sh
log show --last 30s --style ndjson --info --debug --no-pager \
  --predicate 'subsystem == "com.apple.coreaudio" AND eventMessage CONTAINS[c] "Input format:" AND (eventMessage CONTAINS[c] "ACAppleLosslessDecoder" OR eventMessage CONTAINS[c] "ACMP4AACBaseDecoder")'
```

## Boundary implementation

- Playback starts at a cached, per-song verified ALAC rate when available, with
  decoder-log reverification moved off the audible start path. A changed cache
  entry is corrected after capture begins.
- Fozmo launches the provisioned helper app through LaunchServices rather than
  executing its inner Mach-O. This preserves the signed bundle identity and
  `NSAppleMusicUsageDescription` that MusicKit/TCC require.
- The capture session is not rebuilt when the verified rate is unchanged.
- Music.app control runs through compiled, in-process `NSAppleScript` with a
  two-second Apple Event timeout. Catalog selection retries once, transport
  starts before its position is reset, and duration participates in track
  identity matching.
- Distributed `com.apple.Music.playerInfo` events drive monitoring, with a
  two-second liveness fallback rather than a 350 ms poll/debounce loop.
- A gated 20-second capture ring preserves a configurable boundary lead
  (`apple_music_playback.boundary_lead_secs`, default 2.0 seconds). Same-rate arbitrary
  Apple Music successors switch Music.app while the listener consumes the old
  ring tail, without reopening Player, DSP, or the DAC.
- An Apple Music rate change still requires replacing the CoreAudio capture
  format. It takes the cached-format fallback and may expose one short gap of
  up to the configured boundary lead; this is the documented non-gapless case
  when the output carrier cannot remain compatible.
- Qobuz/local successors into Apple Music route and verify Music.app, open the
  capture ring, and prepare the live decoder during the outgoing tail. At the
  endpoint, `begin_seamless_handoff` and
  `play_prepared_stream_if_epoch(..., preserve_output: true)` install the
  already-prefilled ring. If the output carrier cannot be preserved, the same
  prepared ring is installed immediately after natural EOF as the fallback.

## Development setup

Build the helper and run the deterministic test suites:

```sh
./apple-music-helper/build-app.sh
swift test --package-path apple-music-helper
cargo test --lib --features apple_music_musickit
npm --prefix ui test -- AppleMusicMvpPage.test.tsx
```

Start the feature-enabled server with:

```sh
cargo run --features apple_music_musickit
```

The helper app is written to:

```text
target/apple-music-helper/FozmoAppleMusicHelper.app
```

An ad-hoc helper can exercise IPC and failure handling, but live MusicKit
authorization and catalog access require a provisioned App ID.

## Apple Developer setup

Use the committed helper bundle ID:

```text
com.fozmo.apple-music-helper
```

1. Register an explicit App ID for that bundle ID.
2. Enable the MusicKit App Service.
3. Create a Mac App Development provisioning profile for the development Mac.
4. Build the signed helper:

   ```sh
   FOZMO_APPLE_MUSIC_SIGN_IDENTITY="Apple Development: Your Name (TEAMID)" \
   FOZMO_APPLE_MUSIC_PROVISIONING_PROFILE="/absolute/path/Fozmo_MusicKit.provisionprofile" \
   ./apple-music-helper/build-app.sh
   ```

5. Launch Fozmo, open **Settings → Apple Music**, launch the helper, and
   authorize access.

MusicKit is an App Service associated with the App ID; there is no
`com.apple.developer.musickit` code-signing entitlement.

## Local API

The feature-enabled local router adds:

- `GET /api/apple-music/status`
- `POST /api/apple-music/launch`
- `POST /api/apple-music/authorize`
- `GET /api/apple-music/catalog/search`
- `GET /api/apple-music/catalog/songs/:id`
- `GET /api/apple-music/catalog/albums/:id`
- `POST /api/apple-music/play`
- Apple Music album-version preview, match, link, unlink, and detail routes
- `GET /api/apple-music/qobuz-albums/:id/version`
- `POST /api/apple-music/shutdown`

A Qobuz album linked to a local album inherits that album's Apple Music version
through the local album's version list. A Qobuz album with no local counterpart
has no `albums` row to key a version to, so `qobuz-albums/:id/version` matches
the Apple catalog against the Qobuz listing on the same evidence and remembers
the outcome — including "Apple has nothing" — in `qobuz_apple_music_links`.

Normal transport continues through the standard zone playback endpoints.

## Security and persistence

- The helper connection uses a random launch token, session ID, the PID
  reported by the LaunchServices-launched helper, the expected bundle ID, and
  an owner-only Unix socket.
- Apple credentials and tokens never cross the helper protocol.
- Captured PCM stays in memory.
- Queue, history, listening state, and album versions use the existing
  provider-neutral storage.
- Catalog playback requires an Apple Music subscription, Music.app, the Fozmo
  Capture driver, and an explicit local physical output.
