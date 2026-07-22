---
name: fozmo-dj
description: Control Fozmo with the fozmoctl CLI instead of MCP. Use for requests to play, search, queue, pause, skip, change volume or output zone, inspect playback, use Qobuz, manage playlists, or build a tasteful DJ set.
---

# Fozmo DJ

Run the request with `fozmoctl`; do not merely explain commands. Prefer `--json` output.

Use the first available executable: `fozmoctl`, `/Applications/Fozmo.app/Contents/Helpers/fozmoctl`, or `./target/debug/fozmoctl`. In a source checkout, build a missing binary with `cargo build --bin fozmoctl`. Use `<command> --help` for uncommon syntax.

## Workflow

1. Read `status --json`. If the user names an output, pass `--zone NAME` (or `--zone-id ID`) to status and every playback command; list zones only when needed. Do not change the default zone unless asked.
2. Find music with `track-search "artist title" --ranked --limit 5 --json`. Use returned `source_key` values (`local:ID` or `qobuz:ID`), preferring the correct canonical local version over Qobuz. Avoid live, remix, demo, tribute, and karaoke versions unless intended.
3. Start a fresh run with `play SOURCE --queue SOURCE...`; append with `queue add-many SOURCE...`. Keep albums in order.
4. Verify mutations with `status --json` or `queue get --summary --json`. On a startup conflict, reread status and retry once.

Examples (add the zone flags when applicable):

```bash
fozmoctl play local:123 --queue local:124 qobuz:987 --json
fozmoctl queue add-many local:125 qobuz:654 --json
fozmoctl pause --json                 # also: resume, next, stop
fozmoctl volume 35 --json             # playback gain
fozmoctl volume 35 --device --json    # output device
fozmoctl volume 35 --hegel --json     # Hegel amplifier
```

For Hegel, respect the configured `max`; use `--hegel --direction up|down` for one native step.

## Playlists and Qobuz

- Use `playlist list|show|create|add` for saved mixes (`mix` is an alias). Playlist commands do not start playback or take a zone.
- Use blended `track-search` normally. Use `qobuz` subcommands only when Qobuz-only behavior is requested or being diagnosed. Qobuz login is optional unless the request requires it.

## DJ Judgment

For vague prompts, inspect `history top --range week --limit 25 --json`, then make a coherent set rather than replaying history or queueing search order.

- Shape energy, texture, and mood; every track should earn its place.
- Favor deep cuts over obvious hits, balanced with a few anchors and surprises.
- Match risk to context: restrained for focus or dinner, adventurous for “surprise me.”
- Prefer musical correctness over a merely available local copy.

Play the opener, queue the rest, verify, and summarize the vibe briefly.
