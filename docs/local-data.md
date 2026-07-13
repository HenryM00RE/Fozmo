# Local Data and Upgrade Safety

Fozmo keeps user state outside `Fozmo.app`. Replacing the application or
installing a Sparkle update therefore does not replace metadata, playlists,
history, settings, artwork, or locally uploaded music.

## Packaged macOS layout

The menu-bar launcher supplies four explicit roots:

```text
FOZMO_RESOURCE_DIR  Fozmo.app/Contents/Resources
FOZMO_DATA_DIR      ~/Library/Application Support/Fozmo
FOZMO_CACHE_DIR     ~/Library/Caches/Fozmo
FOZMO_LOG_DIR       ~/Library/Logs/Fozmo
```

Persistent data beneath `FOZMO_DATA_DIR` includes:

```text
install.json                 stable UUID and last successfully bound server/schema version
settings.json                non-secret preferences and profiles
settings.json.bak            last known-good settings copy
profile-images/              validated profile image assets; settings stores URL identifiers only
library/library.db           metadata, playlists, favorites, history, queues and zones
library/art/                 content-addressed original artwork
music/                       uploads and the default managed music folder
presets/                     user-created EQ presets
appearance/custom-display.ttf
tls/remote-cert.pem          generated remote-access public certificate
backups/                     latest three validated database/settings snapshots
```

Rebuildable Qobuz, Sonos, transcode and thumbnail data lives under the cache
root. Logs live under the log root. Static UI files and built-in presets are
served read-only from the resource root; user presets override a built-in
preset with the same name without modifying the app bundle.

Secrets remain in one macOS Keychain bundle under `com.fozmo.secrets`. Pairing,
Qobuz, and generated remote-TLS key account names use the stable UUID from
`install.json`, so one Fozmo installation cannot accidentally reuse another
installation's trust material. The first scoped startup verifies and moves the
legacy global TLS key before deleting its old alias.

## Development workspace compatibility

Source builds keep writable state outside the checkout by default. On macOS,
development data, caches, and logs use `Fozmo-dev` below Application Support,
Caches, and Logs respectively. On Linux they use the matching XDG directories
(or `~/.local/share/Fozmo-dev` and `~/.cache/Fozmo-dev` when XDG variables are
unset). The current working directory supplies read-only source resources.

`FOZMO_WORKSPACE_DIR` retains the historical single-root layout when it is
explicitly supplied, which keeps smoke tests and legacy development workflows
isolated. Split roots take precedence if a packaged launcher supplies any of
them.

## Safe writes, migrations and backups

- Settings are written to a temporary file, flushed, and atomically renamed.
  A known-good `.bak` is maintained. Invalid settings are quarantined; Fozmo
  restores a valid backup or refuses startup rather than silently replacing
  existing preferences with defaults.
- The data root has an advisory process lock. A second server refuses to use
  the same database instead of risking concurrent writers.
- SQLite migrations are ordered by `PRAGMA user_version`, run in a transaction,
  and reject databases newer than the running binary.
- Before a schema migration, Fozmo creates a WAL-aware SQLite snapshot plus a
  validated settings copy. Manual/pre-update backups use the same path and only
  prune older copies after the new snapshot passes `integrity_check`.
- Artwork database records use filenames relative to the managed artwork root;
  external music paths remain external.

## Importing an existing workspace

Quit the old Fozmo server first. The packaged launcher can call the same
machine-readable maintenance command directly:

```sh
FOZMO_RESOURCE_DIR="/Applications/Fozmo.app/Contents/Resources" \
FOZMO_DATA_DIR="$HOME/Library/Application Support/Fozmo" \
FOZMO_CACHE_DIR="$HOME/Library/Caches/Fozmo" \
FOZMO_LOG_DIR="$HOME/Library/Logs/Fozmo" \
fozmo-server --import-workspace "/path/to/old/workspace"
```

The command emits one JSON object per progress stage and exits nonzero on
failure. It refuses a destination that already contains state, creates a
staging tree beside the destination, takes a SQLite snapshot that includes
committed WAL data, compares representative metadata/history/playlist/queue
row counts, verifies every copied regular file by size, records those checks in
`import.json`, then atomically renames the staged data into place.

It imports settings, the database, original artwork, managed music, user
presets, the custom display font, and all regular files in the durable TLS
directory. Track paths below the old `music/` root and custom TLS paths below
the old managed TLS directory are rebased; external paths are preserved. The
generated TLS private key remains in the macOS Keychain and is never exported
into the data directory. Qobuz/Sonos/transcode/thumbnail caches and logs are
deliberately skipped. The source workspace is never deleted or modified by the
importer.

If the source is already a packaged data root with a valid `install.json`, the
importer preserves its installation UUID so UUID-scoped Keychain identities
survive a data-root relocation. A new UUID is generated only for a truly
legacy workspace without installation metadata.

## Fresh local state and release checks

For isolated development smoke tests, continue to use:

```sh
./tools/fresh-workspace-smoke.sh
./tools/release-startup-smoke.sh
```

Before release, confirm runtime databases/settings are ignored, no private
paths or credentials are bundled, exercise the legacy importer against a
non-empty WAL fixture, and complete the unsigned clean-Mac checklist for 0.0.1.
For a future signed release, also perform an update from the previous signed
version while verifying metadata/history counts before and after.
