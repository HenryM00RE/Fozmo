# Fozmo Apple Music helper

This is the standalone Gate A proof for the native Apple Music integration. It
is a real agent-style macOS application that owns
`ApplicationMusicPlayer.shared` and speaks the versioned, owner-only Fozmo IPC
protocol. It does not capture PCM or enter Fozmo's playback router yet.

Build the app bundle:

```sh
./apple-music-helper/build-app.sh
```

The default output is:

```text
target/apple-music-helper/FozmoAppleMusicHelper.app
```

An ad-hoc build deliberately omits the restricted MusicKit entitlement so
macOS will launch it for compile, handshake, lifecycle, and UI testing. MusicKit
authorization requires an Apple development identity/App ID with the MusicKit
capability. For a provisioned test build:

```sh
FOZMO_APPLE_MUSIC_SIGN_IDENTITY="Apple Development: …" \
FOZMO_APPLE_MUSIC_PROVISIONING_PROFILE="/path/to/profile.provisionprofile" \
./apple-music-helper/build-app.sh
```

Then start the Fozmo server with:

```sh
cargo run --features apple_music_musickit
```

The helper refuses to run as an independent command-line tool. Fozmo must
launch it with a private socket, a random launch token, and a session ID.
