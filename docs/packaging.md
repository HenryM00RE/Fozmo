# Packaging And License Notes

Fozmo has an Apple-silicon macOS application/DMG pipeline with three explicit
modes: marked development output, the intentionally unsigned `0.0.1` public
release, and a separate future Developer ID/notarized release. The two public
modes share architecture, source, licence, and privacy gates; only the future
signed mode requires Apple and Sparkle publishing credentials.

## Release Shapes

### Source Checkout

Use this shape for development or private sharing:

- Include Rust source under `src/`.
- Include React source under `ui/`.
- Include docs, presets, design references, tools, and the committed
  `static/react-app` frontend snapshot.
- Exclude runtime data such as `settings.json`, `library/`, `music/`, caches,
  logs, and local databases.

### Built Artifact

The macOS release shape is `Fozmo.app` in a drag-to-Applications DMG:

- Build the frontend, MIT Rust server, GPL AirPlay helper, minimal LGPL FFmpeg,
  and Swift menu-bar launcher using `macos/scripts/build-app.sh`.
- Target arm64 and macOS 13 without `target-cpu=native` or host-only flags.
- Include read-only frontend resources/default presets, Sparkle, complete
  license texts, component metadata, and build provenance.
- Exclude frontend source maps.
- Exclude local runtime data, service tokens, pairing tokens, and local paths.
- Sign nested frameworks/helpers first and the outer app last. The 0.0.1
  unsigned release uses ad-hoc signatures; the future signed release uses its
  Developer ID identity. Never use `codesign --deep` as a signing shortcut.
- Notarization, stapling, Gatekeeper assessment, and Sparkle appcast generation
  apply only to the future signed release, not the unsigned 0.0.1 artifact.

For a local ad-hoc artifact:

```sh
./macos/scripts/build-dev-dmg.sh
```

For the clean, unsigned 0.0.1 public artifact:

```sh
./macos/scripts/build-unsigned-release.sh
```

This produces `Fozmo-0.0.1-macos-arm64.dmg` without a development marker,
alongside individual and aggregate SHA-256 files, the exact source archive,
versioned release notes, release/source verification receipts, and
`build-manifest.json`. It does not invoke Apple notarization or stapling and
keeps Sparkle checks and automatic updates disabled.

## Feature Flags

Current Rust feature defaults are the macOS listening build shape:
`local_library`, `qobuz`, `pcm_output`, `airplay_helper`, `sonos`, `hegel`,
`upnp`, and `experimental_dsd256`. They do not include `asio`:

```sh
cargo build --release
```

The public macOS build uses this explicit allowlist rather than relying on
defaults:

```sh
cargo build --release --target aarch64-apple-darwin --no-default-features \
  --features local_library,qobuz,pcm_output,airplay_helper,sonos,hegel,upnp,experimental_dsd256
```

For explicit ASIO builds:

```sh
cargo build --release --features asio
```

Windows ASIO builds require the Steinberg ASIO SDK and LLVM/libclang setup;
ASIO is not compiled or shipped in the macOS DMG.

Apple Music live capture is an opt-in, macOS-only experiment and is not part
of the default build:

```sh
cargo build --release --features apple_music_capture
```

It is not compiled or shipped in the macOS DMG. Its implementation and driver notes are kept in the
[development archive](dev/apple-music-capture.md).

## Generated Frontend Assets

`static/react-app` is currently committed as a checkout-served frontend
snapshot. Treat it as generated output:

- Update it only with `npm --prefix ui run build`.
- Do not hand-edit generated JavaScript, CSS, or source maps.
- Keep the committed snapshot fresh with `./tools/check-frontend-snapshot.sh`;
  it fails if a build would change `static/react-app`.
- Public-readiness checks run the stricter clean mode and fail if generated
  assets have uncommitted changes.
- Generated bundle diffs are marked as generated in GitHub via `.gitattributes`
  so review can focus on source changes first.
- Before a public release, decide whether source maps are included or removed.
- Scan generated assets with `./tools/public-readiness.sh`.

## Runtime Data Exclusions

The app bundle is read-only. Packaged builds use explicit roots:

```text
Fozmo.app/Contents/Resources/          bundled UI and defaults
~/Library/Application Support/Fozmo/ durable data and backups
~/Library/Caches/Fozmo/               regenerable caches
~/Library/Logs/Fozmo/                 rotating process logs
```

`FOZMO_WORKSPACE_DIR` remains a source-development compatibility override and
is not the packaged runtime model. These paths and file types must never be
bundled in public artifacts:

- `settings.json`
- `settings.local.json`
- `library/`
- `music/`
- `ui/library/`
- `ui/music/`
- `*.db`
- `*.sqlite`
- `*.log`
- credentials, TLS private keys, pairing tokens, caches, and listening history

The bundle assembly and verification scripts scan for these files and for
developer `/Users/...` paths. Database/settings upgrades use verified backups,
SQLite `user_version` migrations, and atomic settings writes; see
[Local Data](local-data.md).

Confirm with:

```sh
git ls-files settings.json ui/library/library.db library/library.db
git status --ignored --short settings.json library ui/library
```

The first command should print nothing. The second should show local runtime
data as ignored, not staged.

## License Review

Automated dependency policy gates now run in `./tools/verify.sh` and CI:

- `npm --prefix ui run audit` fails on high-severity npm advisories.
- `npm --prefix ui run license:check` checks production frontend dependency
  licenses against the approved npm allow list.
- `cargo deny check` audits the MIT server graph and fails on GPL dependencies,
  advisories, yanked crates, unknown licenses, unapproved registries or git
  sources, and source-policy violations.
- `tools/check-release-boundaries.sh` verifies that the MIT graph contains no
  GPL AirPlay/fdk dependencies and scans release binaries for FDK/AAC encoder
  symbols.

Before a public artifact:

- Generate and review third-party Rust and npm dependency license notices.
- Confirm license compatibility for the intended distribution model.
- Review the tracked security debt in [security.md](security.md), including
  any `cargo deny` advisory ignores.
- Keep all direct-network AirPlay behavior inside the separately built
  `fozmo-airplay-helper`: discovery, receiver validation, RAOP/AirPlay 2
  sessions, ALAC, pairing, timing, metadata, and receiver volume. The helper
  accepts standard PCM/WAV and shares no memory, FFI, dynamic libraries,
  database, settings, or internal Rust types with the server.
- Keep `fdk-aac`, `fdk-aac-sys`, FDK symbols, and AAC encoder symbols out of
  both processes. The pinned helper build is ALAC-only.
- The scoped same-DMG distribution decision for version 0.0.1 is recorded in
  [GPL Aggregation Assessment](gpl-aggregation-assessment.md). Public builds
  validate the tracked `LICENSES/gpl-aggregation-policy.json` decision and all
  mechanical process-boundary/source obligations instead of relying on an
  ephemeral environment-variable assertion.
- Reassess the distribution before changing the IPC semantics, introducing
  linking/FFI/shared memory, moving GPL dependencies into the MIT graph,
  changing the helper or distribution licences, or omitting corresponding
  source. If a future assessment rejects same-DMG distribution, omit the GPL
  helper from the MIT DMG and publish it with complete corresponding source as
  a separate download; the MIT server protocol remains unchanged.
- Confirm whether bundled SQLite via `rusqlite` needs any notice in the chosen
  artifact format.
- Confirm ASIO SDK terms before distributing Windows ASIO builds.
- Include licenses for network audio stacks, crypto dependencies, and frontend
  dependencies.
- Ship `LICENSES/MIT.txt`, `LICENSES/GPL-2.0-only.txt`, the component map,
  required third-party texts, and exact corresponding source for the helper and
  its patched/pinned AirPlay stack. Do not apply a restrictive umbrella EULA.

The automated gates are not a substitute for legal review. They keep the
baseline visible and block unapproved dependency drift before packaging.

Recommended command for a first-pass inventory:

```sh
./tools/release-inventory.sh
```

This writes `target/cargo-metadata.json` and `target/npm-tree.json`. These
inventories are not a substitute for legal review, but they make the dependency
set explicit.

## Secret Scanning

Public-readiness checks include a general Gitleaks scan in addition to the
project-specific local-data checks. Run it from a full, non-shallow clone with
all refs fetched:

```sh
./tools/secret-scan.sh
```

The history pass scans every fetched ref with the strict `.gitleaks.toml`, which
extends the Gitleaks defaults without any path allowlist. The worktree pass
scans the real checkout, including untracked files, with
`.gitleaks-worktree.toml`. Its exclusions mirror reviewed local-only
`.gitignore` entries such as build output, runtime databases, local settings,
smoke evidence, and private screenshots. `check-tracked-public-files.sh`
rejects those paths if they are force-added, so the worktree exclusions can
never hide committed content. A shallow clone fails closed; fetch full history
with `git fetch --unshallow --tags` before scanning.

## Release Checklist

Before publishing:

```sh
./tools/public-readiness.sh
./tools/fresh-workspace-smoke.sh
./tools/lan-pairing-smoke.sh
./tools/release-inventory.sh
./tools/check-release-boundaries.sh
./tools/verify.sh
```

For 0.0.1, run the unsigned entry point from a clean checkout and record the
clean-Mac checklist in [manual-smoke-tests.md](manual-smoke-tests.md). The
artifact must be described as ad-hoc signed, unsigned by Apple, and
non-notarized. Do not publish an appcast, claim Gatekeeper approval, or claim a
Sparkle update path for this release.

Real-device RAOP/AirPlay 2 ALAC, CoreAudio/USB/DSD256, Sonos/Hegel/UPnP,
Bonjour/raw-IP, and legacy WAL-import evidence remain separate manual coverage.

The future signed workflow is manual-dispatch only. It and
`macos/scripts/release.sh` additionally require:

- a clean version tag and Xcode 26.6/Rust 1.96/Node 22 toolchain;
- Developer ID Application and notarization credentials;
- Sparkle feed URL plus distinct public/private EdDSA inputs;
- an audited exact mixed-license source archive;
- `SOURCE_ARCHIVE_AUDITED=1` plus a tracked aggregation policy covering the
  release version and build mode;
- successful nested signing, DMG signing/notarization/stapling, Gatekeeper,
  architecture, dependency/symbol, privacy, and appcast-signature checks.

Upload the DMG, checksums, source, build manifest, and release notes first.
Publish the appcast last so an update client cannot observe a partial release.
