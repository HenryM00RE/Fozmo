# Fozmo for macOS

This directory contains the native macOS 13+ menu-bar launcher and the release
tooling for an Apple-silicon DMG. The Swift launcher is only a supervisor: the
browser UI remains in the Rust server, and direct network AirPlay runs as a
separate GPL helper process.

## User experience

After dragging `Fozmo.app` to Applications, launching it starts the bundled
server and places a speaker icon in the menu bar. The menu can open Fozmo in a
browser, copy local and LAN addresses, open authenticated Remote Access setup,
toggle LAN access and launch-at-login, start/stop/restart the server, inspect
data/logs, check for updates, and quit cleanly.

The host browser uses `http://localhost:3001`. LAN access defaults off. After a
user turns on **Allow LAN Access**, other trusted-LAN devices can use the Mac's
Bonjour address (`http://<LocalHostName>.local:3001`) or the displayed IP
fallback. LAN browser access is unauthenticated unless the user also turns on
**Require LAN Authentication**. The separate TLS Remote Access listener always
requires a linked remote session. Use unauthenticated LAN control only on
trusted networks; administrative and credential-sensitive routes retain their
existing protections. The launcher owns the `_fozmo._tcp` and `_http._tcp`
advertisements and republishes them after a debounced primary-address change.

Durable data is outside the app bundle:

```text
~/Library/Application Support/Fozmo/   settings, SQLite library/history, art
~/Library/Caches/Fozmo/                rebuildable service/transcode caches
~/Library/Logs/Fozmo/                  rotating server and helper logs
```

On first run, the launcher offers Start Fresh or a folder picker for an old
workspace. Import is only invoked when the bundled server's `--help` explicitly
advertises `--import-workspace`; the source folder is never deleted. Sparkle
also creates a verified SQLite/settings backup before allowing an update
relaunch, and retains the newest three backups.

## Development build

Prerequisites are Xcode 26.6, the pinned Rust toolchain, Node 22, and normal
command-line build tools. The build scripts validate the selected Xcode
installation's macOS SDK, Clang, and Swift compiler directly. Xcode's global
first-launch package installation is not required for Fozmo's macOS-only
packaging; Xcode may still request it for separate iPhone or device-development
workflows.

Then build an ad-hoc, non-notarized app and DMG:

```sh
./macos/scripts/build-dev-dmg.sh
```

The result is `target/macos/Fozmo-<version>-macos-arm64-dev.dmg`. It contains a
prominent development marker and must not be published. A local stable signing
identity may be selected with `FOZMO_DEV_SIGN_IDENTITY`; otherwise every nested
component is ad-hoc signed.

The development build normally builds the UI, the MIT server feature allowlist
(including only the MIT `airplay_helper` socket client),
the standalone AirPlay helper, and the pinned FFmpeg stack. For a packaging-only
iteration, existing artifacts may be reused with the `SKIP_*_BUILD=1` variables
implemented in `build-app.sh`.

## FFmpeg provenance

`build-ffmpeg.sh` downloads only these upstream source archives and verifies
their recorded SHA-256 values before compiling:

- FFmpeg 8.1.2
- libopus 1.6.1

The build is arm64/macOS 13, static-libopus, network-disabled, and configured
with both GPL and nonfree components disabled. It enables only the local audio
demuxers/decoders, Opus encoder/Ogg muxer, resampler, and EQ filters Fozmo uses.
The stage includes exact source archives, license texts, configure output, and
`provenance.json`. `audit-ffmpeg.sh` rejects a missing manifest, changed pinned
version, GPL/nonfree/network flags, absent libopus, a non-arm64 binary, or a
non-system dynamic dependency. Public assembly accepts this stage—not a
Homebrew or PATH FFmpeg.

## Unsigned 0.0.2 public release

Fozmo 0.0.2 is intentionally unsigned and non-notarized. Build its public
artifact from a clean checkout with the pinned Xcode 26.6, Rust 1.96 and Node
22 toolchains, Gitleaks, and the normal source-build dependencies:

```sh
./macos/scripts/build-unsigned-release.sh
```

The command runs the complete release/native, public-readiness, LAN,
remote-access and Swift verification suites; creates the exact corresponding
source; rebuilds its MIT and GPL executables offline; packages those verified
arm64 binaries; and verifies the app and mounted DMG. It writes release/source
verification receipts, versioned release notes, individual and aggregate
SHA-256 files, and a build manifest. It does not require a Developer ID
identity, Apple credentials, Sparkle keys, or an app icon, and it never invokes
notarization or stapling. Sparkle checks and automatic updates are disabled.

Artifacts are written to `target/macos/unsigned-release-0.0.2/`:

```text
Fozmo-0.0.2-macos-arm64.dmg
Fozmo-0.0.2-macos-arm64.dmg.sha256
Fozmo-0.0.2-macos-arm64.md
Fozmo-0.0.2-source.tar.zst
Fozmo-0.0.2-source.tar.zst.sha256
SHA256SUMS
build-manifest.json
release-verification.json
source-build-verification.json
```

The DMG has the normal public name and no development marker. The app and its
nested code are ad-hoc signed, not signed with an Apple identity. Gatekeeper
approval follows the manual steps in [`docs/install.md`](../docs/install.md).

The scoped same-DMG decision is recorded in
[`docs/gpl-aggregation-assessment.md`](../docs/gpl-aggregation-assessment.md).
Public packaging validates the tracked policy, process boundary, licence
notices, and corresponding-source obligations and fails closed if they drift.

## Future signed and notarized release

`release.sh` remains the fail-closed signed pipeline for a future release. The
GitHub workflow is manual-dispatch only, so pushing `v0.0.2` cannot invoke it.
It requires a clean checkout at the exact `v<version>` tag, Developer ID and
notarization credentials, an `.icns` icon, Sparkle signing inputs, and reviewed
corresponding source.

The signed-only environment values are:

```text
DEVELOPER_ID_APPLICATION
APP_ICON_ICNS
SPARKLE_FEED_URL
SPARKLE_DOWNLOAD_URL_PREFIX
SPARKLE_PUBLIC_ED_KEY
SPARKLE_PRIVATE_KEY_FILE
RELEASE_NOTES_FILE
SOURCE_ARCHIVE
SOURCE_ARCHIVE_AUDITED=1
SOURCE_ARCHIVE_SHA256
# EXPECTED_SOURCE_SHA256 is accepted as the CI alias.

# Either:
NOTARY_KEYCHAIN_PROFILE

# Or all three:
APPLE_NOTARY_KEY_PATH
APPLE_NOTARY_KEY_ID
APPLE_NOTARY_ISSUER_ID
```

Run a future signed release with, for example:

```sh
VERSION=0.1.0 ./macos/scripts/release.sh
```

The pipeline builds and signs nested code individually (no `codesign --deep`
signing shortcut), signs and notarizes the DMG, staples and mounts it, runs
architecture/license/privacy checks, and then generates the EdDSA-signed
Sparkle appcast. Upload the DMG, source archive, checksums and release notes
first; publish `appcast.xml` last so clients never see a dead archive URL.

The signed deliverables are staged in `target/macos/release-<version>/`.
Developer ID, notarization, stapling, Gatekeeper approval, and Sparkle update
tests are deliberately not claimed for the unsigned 0.0.2 artifact.

`make-source-archive.sh` normalizes archive order, ownership, modes and
timestamps, then compresses with a pinned zstd 1.5.7 single-thread build. This
makes `SOURCE_ARCHIVE_SHA256` precomputable and reproducible on the same pinned
toolchain; the public job compares the exact archive bytes before signing.

## Launcher package

`FozmoLauncher` is a Swift Package pinned to Sparkle 2.9.4. `build-app.sh`
turns its executable into the conventional bundle shape:

```text
Fozmo.app/Contents/
├── MacOS/Fozmo
├── Helpers/fozmo-server
├── Helpers/fozmoctl
├── Helpers/fozmo-airplay-helper
├── Helpers/ffmpeg
├── Frameworks/Sparkle.framework
└── Resources/
```

The app is an `LSUIElement` agent with no App Sandbox entitlement. Public builds
use hardened runtime, explicit local-network usage strings, and
`SMAppService.mainApp` for user-approved launch at login. Signed public builds
also use secure timestamps; `unsigned-public` uses ad-hoc signatures with
timestamps disabled.

## Command-line control

The DMG bundles the MIT-licensed agent CLI at the stable installed path:

```text
/Applications/Fozmo.app/Contents/Helpers/fozmoctl
```

It is an on-demand client rather than a background process. With Fozmo running,
end users and local agents can test it with:

```sh
/Applications/Fozmo.app/Contents/Helpers/fozmoctl doctor
/Applications/Fozmo.app/Contents/Helpers/fozmoctl status
```

The menu-bar Support menu can copy the path. Agents should invoke the absolute
path; installing a global shell symlink is deliberately left as an explicit
user choice.
