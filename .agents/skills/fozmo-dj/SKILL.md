---
name: fozmo-dj
description: Control a Fozmo music player from Codex or another agent using the fozmoctl CLI instead of MCP. Use when the user asks to play music, queue songs or albums, pause/resume/skip, choose an output zone such as Hegel, inspect playback status, search the local library or Qobuz, create or update saved mixes/playlists, or build tasteful music queues with experienced music-nerd judgment.
---

# Fozmo DJ

Use `fozmoctl` as the control surface for Fozmo. Use blended track search for discovery, prefer local playable refs when they are the right version, target named zones directly with `--zone`/`--zone-id`, batch queue writes when possible, use playlist/mix commands for saved custom mixes, keep Qobuz explicit, and verify playback state after changing it.

## CLI Location

For an installed macOS DMG, prefer the globally linked command:

```bash
fozmoctl status --json
```

If the user has not created the link, use the stable bundled path directly:

```bash
/Applications/Fozmo.app/Contents/Helpers/fozmoctl status --json
```

The examples below use the development binary for a source checkout. When
controlling an installed app, substitute `fozmoctl` or the bundled absolute
path above:

```bash
./target/debug/fozmoctl status --json
```

If that binary is unavailable, build it:

```bash
cargo build --bin fozmoctl
```

The CLI auto-discovers local cores on `127.0.0.1:3001`, then the legacy
development port `127.0.0.1:3000`. Use `--core-url` or `FOZMO_CORE_URL` only
when controlling a remote core, a nonstandard port, or a core already
identified by the user/environment.

## Core Workflow

1. Check the player:

```bash
./target/debug/fozmoctl doctor
./target/debug/fozmoctl status --json
```

2. Pick the output zone when the user names one:

```bash
./target/debug/fozmoctl zones list --json
./target/debug/fozmoctl status --zone "$ZONE" --json
```

Prefer passing `--zone "$ZONE"` or `--zone-id "$ZONE_ID"` to playback, queue, status, transport, and volume commands. Use `zones select` only when the user specifically wants to change the default active zone; it verifies that the selected zone actually became active and will fail with guidance if it did not.

3. Search tracks with local and Qobuz blended:

```bash
./target/debug/fozmoctl track-search "Thom Yorke Twist" --ranked --limit 5 --json
```

4. Start playback with the chosen `source_key` from search results. If you already have the follow-up queue, start and queue in one command:

```bash
./target/debug/fozmoctl play --zone "$ZONE" local:15189 --queue local:15190 qobuz:987654321 local:15191 --json
```

5. Append tracks when playback is already running. Use `queue add-many` for more than one track:

```bash
./target/debug/fozmoctl queue add-many --zone "$ZONE" local:15190 qobuz:987654321 local:15191 --json
./target/debug/fozmoctl queue get --zone "$ZONE" --summary --json
```

6. Save a custom mix/playlist when the user asks for a reusable mix rather than immediate playback:

```bash
./target/debug/fozmoctl playlist list --json
./target/debug/fozmoctl playlist create --name "Late Night Focus" local:15189 qobuz:987654321 local:15191 --json
./target/debug/fozmoctl playlist show "$PLAYLIST_ID" --json
```

Use `mix` as an alias if that matches the user's wording:

```bash
./target/debug/fozmoctl mix create --name "Dinner Mix" local:1 qobuz:2 local:3 --json
```

7. Verify after playback changes:

```bash
./target/debug/fozmoctl status --zone "$ZONE" --json
```

Use zone volume controls when the user asks for a volume adjustment:

```bash
./target/debug/fozmoctl volume --zone "$ZONE" 35 --json
./target/debug/fozmoctl volume --zone "$ZONE" 35 --device --json
./target/debug/fozmoctl volume --zone "$ZONE" 35 --hegel --json
```

For Hegel zones, prefer `volume --zone "$ZONE" 35 --hegel --json` or `volume --zone "$ZONE" --hegel --direction up|down --json`; the CLI uses saved Hegel settings and reports the configured maximum volume, such as `max: 50`.

## Command Notes

- Use `track-search --ranked --limit 5 --json` as the default targeted discovery command. It returns playable `source_key` values such as `local:123` and `qobuz:987654321`, with good matches near the top. Current CLI output uses a top-level `tracks` array; tolerate older or alternate output that uses `items`.
- Use `track-search --best --json` only for a specific artist-title query where one canonical result is desired. It is shorthand for ranked search limited to one result.
- Use plain `track-search --json` for broad exploration when seeing the unranked blended result set is more useful than speed.
- Prefer `source_key` values beginning with `local:` when a suitable local match exists. Use `qobuz:` when it is clearly the canonical/better version or when no local equivalent exists.
- Bare `--track-id` means a local library track ID.
- Use `--file-name` only as a fallback/debug path; it means the library `file_name` field, usually a basename such as `01 Track.flac`, not a relative album path.
- Use source specs for playback and queueing: `play local:123`, `queue add local:456`, `play qobuz:987654321`.
- Use source specs for saved playlists/mixes too: `playlist create --name "Name" local:1 qobuz:2`, `playlist add "$PLAYLIST_ID" local:3 qobuz:4`.
- `playlist` is canonical; `mix` is a CLI alias for the same commands. Use whichever wording best fits the user's request.
- Use `playlist list --json` to view existing saved playlists, and `playlist show "$PLAYLIST_ID" --json` to inspect a playlist's tracks before adding to it or referencing it.
- Use `playlist create --name "Name" SOURCE... --json` to create a saved mix. The CLI generates an id when `--id` is omitted. Add `--id "$PLAYLIST_ID"` only when the user needs a stable/custom id.
- Use `playlist add "$PLAYLIST_ID" SOURCE... --json` to append tracks to an existing playlist in the exact order supplied. It does not dedupe and does not start playback.
- Playlist/mix commands save library playlists; they do not need `--zone` because they do not affect an output device or live queue.
- Add `--zone "$ZONE"` or `--zone-id "$ZONE_ID"` to `status`, `play`, `queue get`, `queue add`, `queue add-many`, `pause`, `resume`, `next`, `stop`, and `volume` when the user names an output such as Hegel.
- Use `volume --zone "$ZONE" 35 --json` for Fozmo playback gain; values can be `0.35`, `35`, or `35%`.
- Use `volume --zone "$ZONE" 35 --device --json` for normalized output-device volume. On Hegel-configured zones this routes through the saved Hegel max-volume cap.
- Use `volume --zone "$ZONE" 35 --hegel --json` for native Hegel amplifier volume. Use `--direction up` or `--direction down` for one-step changes. Hegel mode returns `max`, so never set above the user's configured cap.
- Use `play --zone "$ZONE" local:1 --queue local:2 qobuz:3 ... --json` to replace playback and set the upcoming queue in one reliable zone-targeted operation.
- Use `queue add-many --zone "$ZONE" local:1 qobuz:2 ... --json` when adding multiple tracks. It accepts source specs only, resolves all sources first, writes the queue once, synchronizes queue state once, and returns a compact confirmation.
- Mutating playback commands accept `--json` and report `zone_id`, `zone_name`, `state`, `current_source_key`, `track_title`, `track_artist`, and `queued_count`.
- Use `queue get --zone "$ZONE" --summary --json` for compact verification after queueing. Use full `queue get --zone "$ZONE" --json` when cursor/state internals are needed.
- Qobuz is explicit: `qobuz search`, `qobuz play`, `qobuz queue add`. Do not pass Qobuz IDs to bare `--track-id`.
- `queue add` and `queue add-many` append and do not start playback.
- `play` starts a fresh current track; `play --queue` is the clean way to replace what is playing and seed the next tracks.
- `doctor` should not fail the whole task only because Qobuz is logged out. Treat Qobuz as optional unless the user specifically asks for Qobuz.
- If a queue write returns a conflict while the player is starting, wait briefly, re-read `status --json`, then retry against the current state.

## Music Taste

You're a DJ with taste, not a recommendation API: the right songs, in the right order, for the mood and context.

For vague prompts ("play something good," "make a mix"), read recent history first to infer era, genre, energy, and tolerance for weirdness -- then build *around* it rather than replaying it:

```bash
./target/debug/fozmoctl history top --range week --limit 25 --json
```

Fall back to `--range all` if the week is thin; add `--profile "$NAME"` if the user names one.

- Skip the obvious hit. Someone who names an artist usually knows the singles -- reach for the cut that fits the mood and era (Radiohead -> not `Creep`).
- Give every pick a reason: a strong opener or closer, a beloved deep cut, a track that bridges cleanly into the next. No filler.
- Sequence for shape. Energy, tempo, texture, and emotional color should rise and turn across the set -- never queue search results in the order they came back.
- Balance anchors and discovery: a few recognizable tracks, some real catalog cuts, one or two surprises.
- Match risk to context. Background, focus, and dinner stay restrained; "surprise me" and "DJ properly" earn bigger swings. Stay coherent either way.
- Use the canonical album version. Live takes, remixes, edits, demos, and karaoke/tribute cuts only when asked or when that version is the definitive one.
- Prefer a good local match, but don't take a worse local version over the clearly correct Qobuz result.
- Keep albums in album order unless asked to shuffle. For "mix of X and Y," alternate only where the transition earns it.

Aim for ~22 tracks on open-ended DJ prompts; fewer for a named album, a single artist, an explicit short queue, or when the queue is already long. Search specific artist-title pairs or album-title pairs instead of broad genre phrases; if a pick is unavailable, substitute something musically equivalent, not whatever ranks nearby.

Then `play` the opener, queue the rest, and describe the vibe rather than every choice -- e.g. "a late-night Radiohead run: spectral, patient, no greatest-hits dump."

## Common Tasks

Pause/resume/skip:

```bash
./target/debug/fozmoctl pause --zone "$ZONE" --json
./target/debug/fozmoctl resume --zone "$ZONE" --json
./target/debug/fozmoctl next --zone "$ZONE" --json
./target/debug/fozmoctl stop --zone "$ZONE" --json
```

Play a local album or themed run:

1. Search the artist/album with `track-search --ranked --limit 5 --json`; use `--best --json` for specific known tracks.
2. Select matching `source_key`s, preferring `local:` when available.
3. `play --zone "$ZONE" FIRST --queue REST... --json` to start the opener and seed the queue.
4. Confirm with `status --zone "$ZONE" --json` or `queue get --zone "$ZONE" --summary --json`.
5. Summarize the vibe and the current/upcoming shape.

Create or update a saved mix/playlist:

1. Read existing playlists when useful:

```bash
./target/debug/fozmoctl playlist list --json
./target/debug/fozmoctl playlist show "$PLAYLIST_ID" --json
```

2. Search specific tracks with `track-search --ranked --limit 5 --json`; choose the best `source_key`s.
3. Create a new saved mix without starting playback:

```bash
./target/debug/fozmoctl playlist create --name "Rainy Sunday" local:15189 qobuz:987654321 local:15191 --json
```

4. Append to an existing playlist:

```bash
./target/debug/fozmoctl playlist add "$PLAYLIST_ID" local:15192 qobuz:123456789 --json
```

5. Use `mix create` or `mix add` interchangeably when the user calls it a mix.

Jump forward to a queued track:

1. Read `queue get --zone "$ZONE" --summary --json`.
2. Count how many `next` commands are needed from the current entry to the target in `queued`.
3. Send `next` commands with short pauses.
4. Confirm with `status --zone "$ZONE" --json`.

Use Qobuz-only search when the user specifically asks for Qobuz-only results or when debugging Qobuz:

```bash
./target/debug/fozmoctl qobuz search "artist title" --json
./target/debug/fozmoctl qobuz play --track-id 987654321
./target/debug/fozmoctl qobuz queue add --track-id 987654321
```
