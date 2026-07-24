# Fozmo Apple Music helper

This macOS `LSUIElement` app owns `ApplicationMusicPlayer.shared` for Fozmo. It
speaks authenticated protocol v2 over Fozmo's private Unix socket and supports:

- Music authorization and subscription checks;
- normalized song and album catalog lookups;
- revisioned multi-song queues with duplicate-song occurrence indexes;
- play, pause, resume, seek, next, stop, and shutdown;
- deterministic playback, entry-change, interruption, failure, and queue-end
  events.

Fozmo captures only this helper process's rendered PCM and sends it through the
normal Player, DSP, and selected local output. Tokens and PCM never cross the
control protocol, and PCM is not written to disk.

## Build without an Apple Developer account

```sh
./apple-music-helper/build-app.sh
swift test --package-path apple-music-helper
```

The app is written to
`target/apple-music-helper/FozmoAppleMusicHelper.app`. Its ad-hoc signature
deliberately omits the restricted MusicKit entitlement, so launch, private IPC,
protocol, lifecycle, and Swift tests work now. Authorization, catalog calls,
and real playback return `musickit_capability_unavailable` until the app is
provisioned.

## Build after provisioning MusicKit

The provisioning profile must belong to the explicit App ID
`com.fozmo.apple-music-helper` and contain
`com.apple.developer.musickit = true`.

```sh
FOZMO_APPLE_MUSIC_SIGN_IDENTITY="Apple Development: Your Name (TEAMID)" \
FOZMO_APPLE_MUSIC_PROVISIONING_PROFILE="/absolute/path/Fozmo_MusicKit.provisionprofile" \
./apple-music-helper/build-app.sh
```

The build script validates the profile's bundle ID and MusicKit entitlement,
embeds it, signs with
`Resources/FozmoAppleMusicHelper.entitlements`, and verifies the final
signature.

Run Fozmo with:

```sh
cargo run --features apple_music_musickit
```

Then use **Settings → Apple Music**. Full account setup, verification, routes,
architecture, and the real-Mac test checklist are in
[`docs/dev/apple-music-musickit-mvp.md`](../docs/dev/apple-music-musickit-mvp.md).

The helper refuses to run as an independent command-line tool. Fozmo must
launch it with a private socket, random launch token, and session ID.
