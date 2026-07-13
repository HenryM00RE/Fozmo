# Qobuz Service

Qobuz integration code lives here.

Current shape:

- `mod.rs`: Service construction, Qobuz home API calls, cover fetching, and
  tests.
- `auth.rs`: Bundle token extraction, login/OAuth, session persistence, header
  construction, and request signing helpers.
- `client.rs`: Shared authenticated, optional-session, signed-search, and
  signed discovery Qobuz JSON request helpers.
- `search.rs`: Track, album, and artist search endpoints.
- `album.rs`: Album detail fetching and album-track enrichment.
- `artist.rs`: Artist core/detail/similar/top-track endpoints and
  MusicBrainz/ListenBrainz top-track resolution.
- `radio.rs`: Qobuz radio recommendation flow and artist-track fallbacks.
- `stream.rs`: Progressive stream source/handle types, stream URL resolution,
  format fallback/selection, Sonos/proxy streaming, and prefetch helpers.
- `cache.rs`: Disk cache DTOs, home cache warming, album detail cache behavior,
  artist top-track cache persistence, and cache summary/clear helpers.
- `parser.rs`: Qobuz JSON parsing, home response normalization, radio response
  shape helpers, and track-detail merge helpers.
- `model.rs`: Public Qobuz request, response, and DTO types shared by routes,
  playback, and library matching.

Most catalog endpoint GET/request-shaping behavior is centralized in
`client.rs`; auth/session, stream resolution/download, and radio endpoints with
custom status handling remain in their feature modules.
