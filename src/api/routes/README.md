# Routes

Request handlers grouped by product area live here.

Examples: playback, queue, library, settings, zones, Qobuz, Sonos, AirPlay, agents, and presets.

`mod.rs` owns the top-level route composition and any route families that have
not been split yet. New or moved handler groups should prefer a sibling module
with route-local DTOs next to the handlers.

Current split modules:

- `agents.rs`
- `artwork.rs`
- `config.rs`
- `devices.rs`
- `eq.rs`
- `hegel_control.rs`
- `history.rs`
- `library_basic.rs`
- `pairing.rs`
- `playlists.rs`
- `presets.rs`
- `profiles.rs`
- `qobuz/`
- `streams.rs`
- `upload.rs`
- `zones.rs`
