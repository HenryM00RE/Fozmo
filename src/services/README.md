# Services

External integrations live here.

Current modules:

- `qobuz/`: Qobuz authentication, catalog API, streaming, radio, and cache
  behavior.
- `lastfm.rs`: Last.fm discovery API client used by the radio tester.
- `hegel/`: Hegel amplifier status and control commands.
- `discovery/`: LAN mDNS advertisement helpers.

Keep external client behavior here, away from route-local DTOs and app
bootstrap orchestration.
