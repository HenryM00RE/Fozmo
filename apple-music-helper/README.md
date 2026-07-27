# Fozmo Apple Music helper

This macOS `LSUIElement` app is Fozmo's MusicKit authorization and catalog
bridge. It speaks authenticated protocol v4 over a private Unix socket and
supports:

- Music authorization and subscription checks;
- normalized song and album catalog lookup;
- song and album search;
- immutable Apple Music queue generation staging and adoption;
- helper lifecycle.

The helper owns only the cloud-library half of Fozmo's immutable `Fozmo A` /
`Fozmo B` queue generations; it never renders audio. Product playback uses
Music.app → Fozmo Capture → the normal Fozmo Player, DSP, and selected local
output.

## Build and test

```sh
./apple-music-helper/build-app.sh
swift test --package-path apple-music-helper
```

The app is written to
`target/apple-music-helper/FozmoAppleMusicHelper.app`. An ad-hoc signature
supports launch, private IPC, protocol, lifecycle, and Swift tests.
Authorization and live catalog calls return
`musickit_capability_unavailable` until the app is provisioned.

## Provisioned build

Enable MusicKit for the explicit App ID
`com.fozmo.apple-music-helper`, then create a Mac App Development profile for
that App ID and Mac:

```sh
FOZMO_APPLE_MUSIC_SIGN_IDENTITY="Apple Development: Your Name (TEAMID)" \
FOZMO_APPLE_MUSIC_PROVISIONING_PROFILE="/absolute/path/Fozmo_MusicKit.provisionprofile" \
./apple-music-helper/build-app.sh
```

Run Fozmo with:

```sh
cargo run --features apple_music_musickit
```

Then use **Settings → Apple Music**. Full setup and architecture notes are in
[`docs/dev/apple-music-musickit-mvp.md`](../docs/dev/apple-music-musickit-mvp.md).

The helper cannot run independently. Fozmo must launch it with a private
socket, random launch token, and session ID.
