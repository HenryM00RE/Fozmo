# Apple Music integration

The `apple_music_musickit` feature provides Fozmo's Apple Music product route
on macOS.

## Architecture

MusicKit is the authorization and catalog plane. It does not render audio:

```text
Apple Music catalog
        |
        v
signed MusicKit helper (authorization and catalog only)
        |
        v
SourceRef::AppleMusicTrack -> PlaybackRouter
        |
        v
Music.app -> Fozmo Capture -> Player -> DSP -> selected local output
```

Fozmo activates the exact catalog track in Music.app, verifies the fresh
Music.app decoder format, configures Fozmo Capture to that supported rate, and
starts the normal local Player path. Pause, resume, seek, next, stop, queue
advance, listening history, and status remain owned by Fozmo.

The Apple Music source is restricted to an explicit local physical Core Audio
output. Fozmo rejects the system-default output, Fozmo Capture itself, virtual
outputs, and network renderers to prevent feedback and unsupported routing.

The helper protocol exposes only:

- authorization and subscription status;
- song and album lookup;
- song and album search;
- lifecycle commands.

It has no MusicKit player, renderer queue, or playback transport.

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
- `POST /api/apple-music/shutdown`

Normal transport continues through the standard zone playback endpoints.

## Security and persistence

- The helper connection uses a random launch token, session ID, child PID,
  expected bundle ID, and an owner-only Unix socket.
- Apple credentials and tokens never cross the helper protocol.
- Captured PCM stays in memory.
- Queue, history, listening state, and album versions use the existing
  provider-neutral storage.
- Catalog playback requires an Apple Music subscription, Music.app, the Fozmo
  Capture driver, and an explicit local physical output.
