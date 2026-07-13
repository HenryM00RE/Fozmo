# Security And Trust Debt

This page tracks known security or trust debt that is intentionally tolerated
for private development. These entries are not release waivers; review them
before publishing a public binary or package.

## Remote Access Invariants

Remote Access is documented in [remote-access.md](remote-access.md). It must
remain off by default and use a separate TLS-only allowlisted router. Remote
auth is mandatory, cookie-only, remote-scoped, rate-limited, and independent of
LAN pairing flags. Remote sessions must not mutate Remote Access settings,
create link codes, list or revoke sessions, add library folders, manage Qobuz
auth, mint credentials, or reach local recovery/admin routes.

Excluded remote routes must be absent and return `404`, not a weaker denylist
decision. Do not add stream, diagnostics, upload, Hegel, Sonos, UPnP, Qobuz auth
mutation, or credential-minting routes to the remote router without a focused
security review and tests.

## AirPlay helper dependency isolation

Direct AirPlay pairing and cryptography are no longer part of the MIT server's
dependency graph. They live in the separately built GPL-2.0-only helper and are
checked through its independent lockfile and `airplay-helper/deny.toml`. The
former root-level `RUSTSEC-2023-0071` exception has therefore been removed.
The helper retains a narrowly documented exception because its resolved RSA
call sites perform public-key encryption only, while the advisory concerns
private-key timing. The evidence, invalidation conditions, and re-review
requirements are recorded in
[`airplay-helper/DEPENDENCY_POLICY.md`](../airplay-helper/DEPENDENCY_POLICY.md).

## MIT dependency exceptions

The MIT graph currently carries two explicit unmaintained-crate notices, not
known exploitable-vulnerability waivers:

- `RUSTSEC-2026-0192`: `ttf-parser` parses display-font metadata. Replace it
  with the maintained fontations/skrifa path when practical; until then, keep
  upload limits, parsing isolation, and trusted-LAN exposure under review.
- `RUSTSEC-2025-0134`: `rustls-pemfile` remains as a transitive dependency
  after its parsing functionality moved to `rustls-pki-types`. Remove the
  exception when the remaining transitive users update.

Both exceptions live in `deny.toml`, include their rationale, and are checked
with `unused-ignored-advisory = "deny"` so a stale waiver fails rather than
surviving silently. New advisories remain release failures unless they receive
an equally specific, reviewed, and documented decision.
