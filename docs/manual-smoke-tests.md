# Manual Smoke Tests

Automated checks cover build health, core API startup, fresh workspace behavior,
runtime-data hygiene, and trusted-LAN API access. Manual smoke tests
are still needed for hardware, LAN agents, and external services.

Use this matrix before describing a target as tested or public-ready. Record
the date, machine or device, command used, result, and any notes in the
evidence column.

For raw local notes, copy
[manual-smoke-evidence.example.md](manual-smoke-evidence.example.md) to
`manual-smoke-evidence.local.md`. Keep the `.local.md` file private and ignored;
only copy sanitized summaries into this committed matrix.

## Core App

| Area | Command Or Action | Expected Result | Evidence |
| --- | --- | --- | --- |
| Release startup | `./tools/release-startup-smoke.sh` | Release server starts on localhost; UI shell and `/api/status` load. | Automated |
| Fresh workspace | `./tools/fresh-workspace-smoke.sh` | Temporary workspace starts and creates runtime data outside repo. | Automated |
| Public readiness | `./tools/public-readiness.sh` | Runtime data is untracked and generated assets scan cleanly. | Automated |
| Full verification | `./tools/verify.sh` | Frontend build, Rust checks, and tests pass. | Automated |
| Maintainer field use | Run the macOS app and control it from an iPhone browser. | The app remains stable and the mobile browser control path works on the maintainer's setup. | Maintainer-attested 2026-07-13; exact OS and hardware versions were not recorded, so this is pre-alpha evidence rather than a broad compatibility claim. |

## macOS DMG And Updates

| Area | Command Or Action | Expected Result | Evidence |
| --- | --- | --- | --- |
| Development DMG | `./macos/scripts/build-dev-dmg.sh`, mount the result, and run `verify-dmg.sh`. | Arm64 app contains the menu launcher, server, GPL helper, minimal FFmpeg, Sparkle, resources and notices, but no runtime data. | Automated structure; clean-Mac launch pending |
| Unsigned 0.0.1 DMG | From a clean checkout, run `./macos/scripts/build-unsigned-release.sh`. | A normally named arm64 DMG, checksum, exact source archive/checksum, release notes, verification receipt, and unsigned build manifest are created; no development marker, capture driver, update checks, notarization, or Apple-signature claim is present. | Automated structure; clean-Mac launch pending |
| Clean macOS 13 install | On an Apple-silicon macOS 13 Mac with no developer tools, copy the unsigned app to Applications, attempt launch, then approve it in Privacy & Security. | After explicit approval, the menu app and bundled server start without Rust, Node, Xcode or external FFmpeg. | Pending clean-Mac evidence |
| Menu lifecycle | Exercise Start, Stop, Restart, Quit, login launch, LAN toggle and a second app launch. | One server/helper pair exists; login is silent; Stop/Quit are graceful; second launch opens the existing instance. | Pending |
| Sparkle N→N+1 | Install a signed update from the previous release. | User approves download/install; authenticated backup and graceful stop succeed before replacement; settings/library/history/auth survive. | Pending signed releases |
| Update failure cases | Simulate backup failure, child shutdown timeout, tampered archive/feed and failed migration. | Update/migration is refused and the prior app/data remain recoverable. | Pending signed releases |
| Manual DMG replacement | Replace `Fozmo.app` with a newer version whose database schema advances. | Pre-migration verified backup is created and durable state survives. | Pending versioned fixture |

### Unsigned 0.0.1 clean-Mac checklist

Record each result without treating manual Gatekeeper approval as
notarization:

1. From a clean tracked checkout, build with
   `./macos/scripts/build-unsigned-release.sh`.
2. Verify `Fozmo-0.0.1-macos-arm64.dmg` with the adjacent SHA-256 file by
   running `shasum -a 256 -c Fozmo-0.0.1-macos-arm64.dmg.sha256`.
3. Mount the DMG.
4. Copy `Fozmo.app` into `/Applications`.
5. Attempt the first launch.
6. When macOS blocks it, approve Fozmo through System Settings → Privacy &
   Security → Open Anyway.
7. Launch Fozmo again.
8. Confirm the menu-bar application starts.
9. On a clean Mac without Xcode or developer tools, confirm the bundled server
   starts.
10. Open the browser UI through `http://localhost:3001`.
11. Open the browser UI from another device on the same trusted LAN using the
    advertised `.local` address or raw LAN IP.
12. Confirm playback starts and shutdown completes cleanly.
13. Confirm the mounted DMG does not contain `DEVELOPMENT BUILD.txt`.
14. Confirm the ordinary app has no HAL/capture driver with
    `find /Applications/Fozmo.app -iname '*FozmoCapture*' -o -iname '*.driver'`.
15. Confirm the update menu is disabled and the app plist has
    `FozmoUpdatesEnabled`, `SUEnableAutomaticChecks`, `SUAutomaticallyUpdate`,
    and `SUAllowsAutomaticUpdates` set to false.
16. Confirm the release notes and `build-manifest.json` describe the artifact
    as ad-hoc signed, unsigned by Apple, and non-notarized.

Developer ID, notarization, stapling, Gatekeeper automatic acceptance, and
Sparkle N→N+1 tests remain a separate future signed-release checklist and do
not apply to 0.0.1.

## Playback Regression Baseline

Use this matrix as the manual regression baseline for changes to playback
routing, state ownership, and service boundaries.

| Area | Command Or Action | Expected Result | Evidence |
| --- | --- | --- | --- |
| Local playback start | Select a local track from the library and press play. | Active output starts playback, now-playing metadata updates, and the queue advances at end of track. | Pending |
| Qobuz playback start | Search or browse Qobuz and play a track. | Stream resolves, playback starts, Qobuz metadata appears, and listening history updates. | Pending |
| Pause and resume | UI/API route smoke plus real playback when hardware is available. | Route-level controls reach playback routing and update local playback state; real audio pause/resume still needs hardware evidence. | Automated route smoke; hardware pending |
| Stop | UI/API route smoke plus real playback when hardware is available. | Route-level stop reaches playback routing and updates local playback state; real transport/output behavior still needs hardware evidence. | Automated route smoke; hardware pending |
| Seek | UI/API route smoke plus real playback when hardware is available. | Route-level seek reaches playback routing; audible seek convergence still needs hardware evidence. | Automated route smoke; hardware pending |
| Next | UI/API route smoke plus real playback when hardware is available. | Route-level next reaches playback routing; real queue advancement through playback completion still needs hardware evidence. | Automated route smoke; hardware pending |
| Queue persistence | Build a queue, restart the app, and inspect the active output queue. | Upcoming items, loop mode, shuffle state, and now-playing queue state are restored correctly. | Pending |
| Active output switching | Switch between local, remote, Sonos, or AirPlay outputs as available. | Preferred active output is persisted and only the selected output receives playback commands/settings. | Pending |
| Remote agent playback | Pair an agent, select its output, and play a local or stream-backed source. | Agent receives playback/config commands, renders audio, reports status, and survives reconnect. | Pending |

### Repeat current track

The loop button is repeat-one: while enabled, completion restarts the current
track without consuming the upcoming queue. Turning it off restores normal
queue advancement. Manual Next still skips to the next queued item.

| Output | Repeat after natural completion | Loop off advances normally | Evidence |
| --- | --- | --- | --- |
| CoreAudio / local DAC | Pending | Pending | Engine coverage; hardware pending |
| Browser / remote agent | Pending | Pending | Route and fallback coverage; audible check pending |
| AirPlay | Pending | Pending | Engine coverage; hardware pending |
| UPnP renderer | Pending | Pending | Backend fallback coverage; hardware pending |
| Sonos | Pending | Pending | Backend fallback coverage; hardware pending |

## Local Playback

| Area | Command Or Action | Expected Result | Evidence |
| --- | --- | --- | --- |
| Default output PCM | Select default output and play a local track. | Audio starts, status updates, queue advances. | Pending |
| External output PCM | Select a named external DAC/output and play a local track. | Selected device is used and status reports the route. | Pending |
| Device volume | Adjust app volume for a supported local device. | Device volume changes or unsupported state is clearly reported. | Pending |
| Settings persistence | Change playback settings, restart, and inspect UI/status. | Settings survive restart without committing runtime files. | Pending |

## DSD And DSP

| Area | Command Or Action | Expected Result | Evidence |
| --- | --- | --- | --- |
| PCM upsampling | Enable upsampling and play PCM. | Status reports expected target rate and filter. | Pending |
| EQ/headroom | Enable EQ and headroom, then play PCM. | Audio remains stable and status reflects settings. | Pending |
| DoP | Use a known compatible macOS/CoreAudio DAC. | DoP carrier-rate behavior is correct for the selected mode. | Pending |
| DSD256 | Use a known compatible macOS/CoreAudio USB DAC. | Experimental DSD256/DoP path is available and remains stable; Apple Music capture and Windows ASIO are absent from the DMG. | Pending |

## LAN And Agents

| Area | Command Or Action | Expected Result | Evidence |
| --- | --- | --- | --- |
| Trusted-LAN API | Open the `.local` and raw-IP addresses from another device on the same private network. | The UI and playback controls work without a browser login. | Automated config/auth coverage; LAN pending |
| Optional pairing API | `./tools/lan-pairing-smoke.sh` | Explicit pairing mode still rejects missing tokens and accepts issued sessions. | Automated |
| LAN core | `cargo run --release -- --lan --port=3001` | Explicit LAN mode binds to the LAN and advertises `_fozmo._tcp` plus `_http._tcp`; the default and `--local-only` remain loopback-only. | Pending |
| Remote browser authentication | Enable Remote Access and open an unlinked remote browser, then exchange a link code. | The remote listener rejects the unlinked browser and accepts the linked remote session. | Automated auth coverage; external TLS pending |
| Host validation | Try localhost, current `.local`, each active interface IP and an unrelated Host header. | Expected hosts work and the unrelated host receives 421. | Automated unit coverage; LAN pending |
| Remote agent pairing | Start an agent with `--core-url` and token. | Agent appears as an output with capabilities. | Pending |
| Agent reconnect | Restart core while agent is running. | Agent reconnects and status recovers. | Pending |
| Agent playback | Play to a paired agent output. | Agent receives stream, plays audio, and reports status. | Pending |

## Network Targets And Services

| Area | Command Or Action | Expected Result | Evidence |
| --- | --- | --- | --- |
| Qobuz login/status | Log in and check Qobuz status. | Status reports initialized account without leaking tokens. | Pending |
| Qobuz playback | Search or browse Qobuz and play a track. | Stream resolves, playback starts, and history updates. | Pending |
| Qobuz radio | Start radio from a track or artist seed. | Recommendations load and excluded tracks are skipped. | Pending |
| AirPlay helper CLI | Run helper `list`, then play WAV and raw s16le PCM without starting the Fozmo server. | Receivers are listed by opaque ID and RAOP/AirPlay 2 ALAC playback works independently. | 2026-07-19 Hegel H390 transport smoke: sanitized dual-service advertisement captured; direct AirPlay 2 ALAC command completed, while forced RAOP was rejected at `ANNOUNCE` with 403. A known-good modern AirPlay 2 receiver also completed a silent setup/playback regression probe. Audible H390 rendering remains pending maintainer confirmation. CLI/unit automated for other paths; other real devices pending. |
| AirPlay 1/2 isolation | Select RAOP and AirPlay 2 receivers, then terminate or version-mismatch the helper. | ALAC playback works; helper failure reports degraded/missing/incompatible and other Fozmo outputs continue. | Pending real devices |
| Sonos | Select a Sonos target and play PCM. | Stream proxy, transport, volume, and metadata behave as expected. | Pending |
| UPnP / KEF gapless handoff | Select a UPnP renderer such as KEF, play a queued pair of local or Qobuz tracks, then inspect `/api/diagnostics/upnp/:zone_id`. | Diagnostics show `SetNextAVTransportURI`, next asset prepared/armed, any early renderer request for the next asset, and promotion at completion; if the renderer rejects next URI, fallback auto-advance remains active with a clear notice. | Pending |
| Hegel | Configure a Hegel host and send power/input/volume actions. | Commands succeed and status parsing remains stable. | Pending |
| AirPlay volume/status | Select an AirPlay target and change app/device volume. | Volume mapping remains stable and target status stays behind the AirPlay integration path. | Pending |
| Hegel volume/status | Configure a Hegel host and use volume/status controls during playback. | Volume and status behavior remains stable and Hegel details do not leak into generic playback config. | Pending |

## Legacy Data Import

Use a fixture containing a non-empty WAL, multiple profiles, edited metadata,
history, playlists, queues, original artwork, managed and external tracks, a
custom font/preset, TLS files and UUID-scoped Keychain secrets.

| Area | Command Or Action | Expected Result | Evidence |
| --- | --- | --- | --- |
| Staged import | Choose the fixture from first-run onboarding. | Source must be stopped; import commits atomically only after integrity, row-count, settings and copied-file validation. Source is untouched. | WAL/row/file unit coverage; full fixture pending |
| Path rebasing | Inspect imported track, artwork, music-folder and custom TLS paths. | Managed music/TLS and artwork are rebased; external music paths are unchanged; caches/logs are skipped. | Automated fixture coverage |
| Stable identity | Import data with an existing `install.json` and exercise Qobuz, pairing and generated TLS. | Installation UUID and UUID-scoped Keychain identifiers survive relocation. | Automated migration coverage; Keychain integration pending |

## Screenshot Readiness

Only add public screenshots after:

- Runtime data has been removed from Git tracking.
- Screenshots contain no private paths, service account details, LAN addresses,
  personal names, or listening history the user does not want public.
- The UI state shown is representative of a stable workflow, not a transient
  debug screen.
- The screenshot date and app commit are recorded near the image or release
  notes.

Store private screenshot drafts under `docs/screenshots/private/` or
`docs/screenshots/local/`; both paths are ignored.
