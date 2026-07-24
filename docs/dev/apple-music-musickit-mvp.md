# Apple Music MusicKit integration

The `apple_music_musickit` feature contains the backend-first Apple Music
provider for macOS. Everything except Apple's signed entitlement,
authorization, subscription, live catalog, and real playback can be built and
tested without an Apple Developer account.

This path is separate from the legacy `apple_music_capture` virtual-device
experiment.

## Implemented architecture

Fozmo owns the complete provider-neutral queue, listening state, history, and
album versions. The helper owns only the currently loaded contiguous Apple
Music segment:

```text
Settings / future product UI
        |
        v
local-only Apple Music API
        |
        v
PlaybackIntent -> PlaybackRouter -> provider-neutral queue/history/status
        |
        +-- Local / Qobuz use their existing playback paths
        |
        `-- Apple Music service
              |
              +-- authenticated protocol-v2 Unix socket
              +-- MusicKit helper queue and transport
              `-- include-only helper-PID Core Audio process tap
                         |
                         v
                  Player -> DSP -> selected local output
```

The current implementation includes:

- `SourceRef::AppleMusicTrack` and stable `apple_music:<song-id>` identity;
- mixed Local, Qobuz, and Apple queue persistence;
- protocol-v2 song/album lookups and revisioned queue events;
- duplicate Apple song occurrences distinguished by segment index;
- helper-PID process-tap targeting and a guarded PCM/DSP handoff;
- contiguous Apple runs without rebuilding the tap between adjacent songs;
- normal pause, resume, seek, next, stop, status, listening, and history paths;
- stale session/revision/duplicate-event rejection;
- provider-boundary auto-advance;
- schema v2 with generic provider track links;
- Apple versions attached to existing local albums, including preview, link,
  unlink, recording identity, and playback resolution;
- a Settings integration console and in-memory fake-helper tests.

MusicKit's rendered process audio is Float32 PCM. Fozmo preserves those sample
values and uses the tap's actual format; catalog metadata is not treated as
proof of the rendered asset's sample rate or bit depth. Apple album versions
therefore intentionally leave sample rate and bit depth unset.

## What works before account setup

Build the unsigned helper and run all entitlement-free tests:

```sh
./apple-music-helper/build-app.sh
swift test --package-path apple-music-helper
cargo test --lib --features apple_music_musickit
npm --prefix ui test -- AppleMusicMvpPage.test.tsx
```

Start the feature-enabled server:

```sh
cargo run --features apple_music_musickit
```

The helper is written to:

```text
target/apple-music-helper/FozmoAppleMusicHelper.app
```

An unsigned development run should show:

- helper present and launchable;
- authenticated IPC protocol v2;
- `helper_musickit_entitled = false`;
- authorization and catalog actions failing cleanly with
  `musickit_capability_unavailable`;
- the complete Settings integration harness;
- fake catalog, mixed queue, migration, history, and album-version tests.

Ad-hoc signing deliberately omits `com.apple.developer.musickit`. Adding that
restricted entitlement to an ad-hoc signature causes macOS to reject the
helper before it can perform even the IPC handshake.

## Apple Developer setup checklist

Use the exact helper bundle ID already committed in `Info.plist`:

```text
com.fozmo.apple-music-helper
```

1. Enroll the account in the Apple Developer Program and accept any pending
   agreements.
2. In Xcode **Settings → Accounts**, add the Apple Account and select the new
   team. Let Xcode create an **Apple Development** certificate, or create and
   install one manually. Confirm the identity with:

   ```sh
   security find-identity -v -p codesigning
   ```

3. In
   [Certificates, Identifiers & Profiles](https://developer.apple.com/account/resources/identifiers/list),
   register an **explicit App ID** whose bundle ID is
   `com.fozmo.apple-music-helper`.
4. On that App ID's **App Services** tab, enable **MusicKit** and save. Apple's
   current account instructions are:
   [Enable MusicKit for an App ID](https://developer.apple.com/help/account/services/musickit)
   and
   [Register an App ID](https://developer.apple.com/help/account/identifiers/register-an-app-id/).
5. Register this development Mac if the portal or Xcode has not already done
   so.
6. Create a **Mac App Development** provisioning profile using that App ID,
   the Apple Development certificate, and this Mac. Download the resulting
   `.provisionprofile`. See Apple's
   [development-profile instructions](https://developer.apple.com/help/account/provisioning-profiles/create-a-development-provisioning-profile/).
7. Build the provisioned helper:

   ```sh
   FOZMO_APPLE_MUSIC_SIGN_IDENTITY="Apple Development: Your Name (TEAMID)" \
   FOZMO_APPLE_MUSIC_PROVISIONING_PROFILE="/absolute/path/Fozmo_MusicKit.provisionprofile" \
   ./apple-music-helper/build-app.sh
   ```

   The script fails early if the profile lacks MusicKit or targets a different
   bundle ID.

8. Verify the result:

   ```sh
   codesign --verify --strict --verbose=2 \
     target/apple-music-helper/FozmoAppleMusicHelper.app

   codesign -d --entitlements :- \
     target/apple-music-helper/FozmoAppleMusicHelper.app
   ```

   The displayed entitlements must include:

   ```xml
   <key>com.apple.developer.musickit</key>
   <true/>
   ```

9. Start Fozmo with `--features apple_music_musickit`, open
   **Settings → Apple Music**, launch the helper, and choose
   **Authorize Apple Music**. macOS should show the usage text from
   `NSAppleMusicUsageDescription`.
10. Sign into the Music app with an Apple Music subscriber account, then use
    the catalog inspector and the real-Mac test matrix below.

This native Swift MusicKit path does not need a Media ID, private MusicKit key,
or hand-generated developer token. Those are used for Apple Music API or web
integrations and should not be added unless Fozmo later introduces a separate
server/web catalog client.

If MusicKit is enabled after a profile was generated, regenerate the profile;
Apple notes that capability changes invalidate affected provisioning profiles.

## First provisioned test run

Use a local Core Audio zone and keep **System Settings → Privacy & Security →
Screen & System Audio Recording** open in case macOS asks for process-audio
capture permission.

After authorizing once in Settings, the opt-in real-Mac runner can exercise the
automatable matrix against a Fozmo server already running on port 3000:

```sh
FOZMO_APPLE_MUSIC_REAL=1 \
FOZMO_APPLE_MUSIC_CONFIRM_CAPTURE=1 \
FOZMO_APPLE_MUSIC_SONG_IDS="FIRST_SONG_ID,SECOND_SONG_ID" \
FOZMO_APPLE_MUSIC_LOCAL_TRACK_ID="123" \
FOZMO_APPLE_MUSIC_QOBUZ_TRACK_ID="456" \
cargo test --features apple_music_musickit \
  --test apple_music_real_macos -- --nocapture
```

Choose two subscriber-playable songs, a valid local track, and a
subscriber-playable Qobuz track. The runner verifies helper entitlement and
subscriber capability before changing playback. It covers the DSP handoff,
tap identity across Apple adjacency, pause/resume/seek, manual Next, all four
Local/Qobuz/Apple boundaries, ring overruns, and helper-termination cleanup.
Without `FOZMO_APPLE_MUSIC_REAL=1` it self-skips without touching playback.
Authorization revocation and process-capture permission revocation remain
manual because they are OS-owned actions.

Run these in order from Settings:

1. Launch helper.
2. Authorize Apple Music.
3. Lookup one known Song ID and one Album ID in the signed-in storefront.
4. Add two different Apple songs, then the same song twice, to a scenario.
5. Confirm helper-process capture and play from row one.
6. Verify the tap target PID equals the helper PID, not the Music app PID.
7. Exercise normal pause, resume, seek, next, and stop.
8. Test the mixed boundaries:

   - Apple → Apple;
   - Local → Apple;
   - Qobuz → Apple;
   - Apple → Local;
   - Apple → Qobuz;
   - Local → Apple → Apple → Qobuz;
   - duplicate Apple song twice.

9. During playback, quit the helper once and revoke authorization once. Fozmo
   should stop only the owned Player epoch, finalize listening, retain the
   remaining queue, and expose a structured failure.
10. Preview and link an Apple album to an existing local Fozmo album, resolve
    its playback plan, play it, restart Fozmo while stopped, and confirm the
    version and remaining queue persist without auto-resuming.

For adjacent Apple tracks, the helper PID, process-tap object, and Player live
stream should remain stable while only the segment index, current source,
listening entry, and persisted queue advance.

## Settings integration console

The page is split into:

1. capability, entitlement, authorization, helper/tap PID, and zone support;
2. normalized song and album catalog inspection;
3. mixed queue builder, canned cases, and raw `SourceRef[]` validation;
4. normal Fozmo transport;
5. Fozmo status, current source, persisted queue, Apple revision/segment,
   helper events, and tap telemetry;
6. Apple album-version preview, link, unlink, resolve, and play.

Direct helper transport and the old Music.app tap proof remain collapsed under
**Advanced diagnostics**. The primary workflow always exercises the normal
router.

## Local-only API

These routes are added only to the feature-enabled local router and are not
part of the remote-access surface:

- `GET /api/apple-music/status`
- `POST /api/apple-music/launch`
- `POST /api/apple-music/authorize`
- `GET /api/apple-music/catalog/songs/:id`
- `GET /api/apple-music/catalog/albums/:id`
- `POST /api/apple-music/play`
- `GET /api/library/albums/:local_id/apple-music/preview`
- `POST /api/library/albums/:local_id/apple-music/link`
- `POST /api/library/albums/:local_id/apple-music/unlink`
- `POST /api/apple-music/dev/play-song`
- `POST /api/apple-music/transport`
- `POST /api/apple-music/stop`
- `POST /api/apple-music/shutdown`
- `POST /api/apple-music/process-tap/start`
- `POST /api/apple-music/process-tap/stop`
- `POST /api/apple-music/comparison/switch`

Normal playback controls remain `/api/pause`, `/api/resume`, `/api/next`,
`/api/seek`, and `/api/stop`.

## Security and persistence boundaries

- The helper accepts only a matching protocol version, random launch token,
  session ID, child PID, and bundle ID over an owner-only Unix socket.
- Apple credentials and tokens are never sent through the helper protocol.
- Captured PCM stays in memory and is never written to disk.
- Apple queue, history, recent-play, and recording identity reuse the existing
  provider-neutral tables.
- Apple catalog album JSON is retained only as an attached album-version
  payload.
- Network/remote sinks reject Apple sources because helper PCM must enter the
  local DSP path.
- Reorders or removals inside an already prepared Apple run return
  `409 apple_music_queue_edit_requires_restart` until safe in-place MusicKit
  queue mutation exists.

## Current external dependency

Only these checks remain blocked until the Apple account is connected:

- signed MusicKit entitlement accepted by macOS;
- user authorization prompt and revocation behavior;
- subscriber capability;
- live song/album catalog responses and storefront availability;
- real `ApplicationMusicPlayer` queue behavior;
- helper-PID process audio under signed playback;
- audible mixed-provider boundary tests.

All other layers have deterministic Rust, Swift, migration, contract, or
frontend coverage.
