# LAN Mode And Optional Pairing

Fozmo's packaged core listens on loopback port `3001` by default. LAN access is
an explicit trusted-network option: any device on that LAN can open and control
Fozmo when authentication is disabled. The launcher shows a persistent warning
in that configuration.

`--local-only` binds the web server to `127.0.0.1` for isolated development.
Disabling LAN in the menu restarts the managed server in this mode and removes
its Bonjour records.

LAN access is not Remote Access. Remote Access uses a separate TLS-only,
internet-facing listener, mandatory remote sessions, and a deliberately
allowlisted API surface. Authentication remains mandatory there. See
[Remote Access](remote-access.md) before forwarding any router port.

## Browser Origins And CORS

The server does not use CORS as authentication. CORS is a browser containment
layer that keeps unrelated web origins from reading or issuing API requests
through a user's browser. Local mode allows loopback control origins for the
configured port. LAN mode also allows the configured public base URL origin so
the advertised core URL can serve trusted-network clients.

Browser WebSocket upgrades, including `/api/ws` and `/api/agent/ws`, validate
`Origin` against the same loopback, active LAN address, accepted `.local`, and
configured public origins used by CORS. Originless upgrades remain available
for non-browser clients. Native agents from loopback or a private/link-local
LAN can register without a pairing token; peers outside those ranges need an
explicit agent credential.

Filesystem-sensitive routes retain their own local/cross-site checks, stream
routes require a same-origin browser request or scoped stream credential, and
native agents receive a connection-scoped stream credential over their
WebSocket. Keep LAN mode on trusted private networks.

The core rejects unknown `Host` headers to reduce DNS-rebinding exposure. It
accepts loopback names, the current Mac `.local` and OS-reported hostnames, and
active interface addresses. This permits a locally configured DNS name without
trusting arbitrary aliases that merely resolve to the Mac. Bonjour publishes
`_fozmo._tcp` and `_http._tcp`; Sonos and UPnP
renderer media URLs deliberately continue to use the raw LAN IP because those
renderers do not reliably resolve mDNS names.

Trusted-LAN clients can manage output settings, including renaming outputs and
setting their device type. Browser outputs can also select lossless FLAC or
Ogg Opus delivery and an Opus bitrate.

The menu links to Remote Access settings instead of asking LAN browsers to
pair. Remote link codes are still short-lived and single-use, and are exchanged
only on the authenticated TLS remote listener.

## Core Mode

Start the core with the packaged default (loopback only):

```sh
cargo run --release
```

Start the core explicitly in local-only mode:

```sh
cargo run --release -- --local-only
```

Enable trusted-LAN access:

```sh
cargo run --release -- --lan --port=3001
```

Useful environment equivalents:

```sh
FOZMO_LAN=1 FOZMO_PORT=3001 cargo run --release
```

## Optional LAN Pairing

Pairing remains available for deployments that explicitly want it, but it is
not part of the packaged trusted-LAN experience:

```sh
cargo run --release -- --lan --require-pairing
```

Or with an environment variable:

```sh
FOZMO_LAN=1 FOZMO_REQUIRE_PAIRING=1 cargo run --release
```

In the packaged macOS app, turn on **Require LAN Authentication** from the
Fozmo menu. It is off by default. Use **Pair a Device** on the server Mac to
show a short-lived, single-use QR/link; the paired browser keeps an HttpOnly
control cookie for seven days.

Upgraded source-checkout installations may temporarily continue using the
legacy `TRANSIENT_LAN` and `TRANSIENT_REQUIRE_PAIRING` names. They emit a
deprecation warning; new configuration should use the `FOZMO_*` names.

When pairing is required, browser API requests need a valid control-session
cookie except for the pairing/session bootstrap routes. Qobuz OAuth routing is
exempt from the general pairing middleware so its callback can complete.
Pairing tokens are short-lived, single-use secrets for creating a session; they
are not browser control credentials. New secrets are generated from the
operating system CSPRNG, stored only as hash records in the secret store, and
include kind, scopes, creation, expiry, last-used, rotation, and revocation
metadata.

## Trust And Authentication Boundaries

The optional unauthenticated LAN surface trusts the private network and does
not require a browser session. Browsing, history, playback, local-library
access, queues, settings, and ordinary administration are available to LAN clients. Host
validation, CORS checks, browser-zone ownership, and the separate authenticated
Remote Access listener remain in place, but they are not substitutes for
network isolation.

Only music-folder management and changes to Remote Access configuration remain
behind the local/control-session boundary. This boundary remains enabled even
when `FOZMO_REQUIRE_PAIRING` is off.

LAN exposure does not turn browser stream proxy routes into bare public file
URLs. `/api/stream/local/*` and `/api/stream/qobuz/*` accept same-origin browser
requests, explicit stream tokens, or legacy scoped sessions. A raw cross-site
or unaffiliated LAN request remains rejected.

Some routes are exempt from the pairing middleware so a local operator can
bootstrap or recover access:

- `/api/pairing/start` issues short-lived, single-use pairing tokens.
- `/api/agents/token` issues agent tokens for explicit agent setup.
- `/api/pairing/revoke-all` revokes all active tokens when credentials are
  lost, stale, or suspected compromised.
- `/api/sessions/browser` exchanges a valid pairing token for a browser
  control-session cookie.
- Qobuz OAuth and WebSocket routes perform their own flow-specific checks.

Pairing-exempt does not mean publicly authorized. Local recovery routes such as
`/api/pairing/start`, `/api/agents/token`, and `/api/pairing/revoke-all` still
call the local request guard used for filesystem-sensitive operations. That
guard allows same-origin loopback requests and rejects LAN peers, cross-site
browser origins, and cross-site fetch contexts. This keeps
`/api/pairing/revoke-all` available as an emergency local reset without
allowing another LAN device or unrelated website to revoke sessions.

Create a browser session from the local machine:

```sh
PAIRING_JSON="$(curl -s -X POST http://127.0.0.1:3001/api/pairing/start)"
TOKEN="$(node -e 'process.stdout.write(JSON.parse(process.argv[1]).token)' "$PAIRING_JSON")"
curl -c cookies.txt -X POST -H "Content-Type: application/json" \
  -d "{\"pairing_token\":\"$TOKEN\"}" \
  http://127.0.0.1:3001/api/sessions/browser
curl -b cookies.txt http://127.0.0.1:3001/api/status
```

Legacy header tokens are accepted only for staged compatibility when their
stored record has the required scope. New browser pairing tokens should not be
sent as `x-fozmo-token` or `Authorization: Bearer`. Query-string tokens are
disabled by default because URLs are too easy to log or leak. For local
development only, `FOZMO_ALLOW_QUERY_TOKEN_AUTH=1` or
`--allow-query-token-auth` enables `?token=` auth for loopback requests with
control-scoped legacy/session tokens; LAN peers still cannot use query-string
tokens.

Revoke the current token:

```sh
curl -X POST -b cookies.txt \
  http://127.0.0.1:3001/api/pairing/revoke-current
```

From the local machine, revoke all active tokens:

```sh
curl -X POST http://127.0.0.1:3001/api/pairing/revoke-all
```

Smoke-test the pairing contract:

```sh
./tools/lan-pairing-smoke.sh
```

The optional-pairing script starts the core in LAN mode with pairing required, confirms a
protected route rejects missing credentials, requests a short-lived pairing
token, exchanges it for a browser session cookie, confirms the cookie works,
confirms the consumed pairing token and `?token=` authentication are rejected,
and checks local agent-token issuance.

## Agent Mode

Start an agent and point it at a LAN core:

```sh
cargo run --release -- --agent --core-url=http://192.168.1.42:3001
```

No pairing token is needed for a native agent on the local machine or private
LAN. After registration, the core sends the agent a memory-only stream
credential over the WebSocket and revokes it on disconnect.

An explicit agent token remains available for compatibility or authenticated
connections from outside the private LAN:

```sh
FOZMO_AGENT_TOKEN=replace-me cargo run --release -- --agent --core-url=http://192.168.1.42:3001
```

Create an agent token from the core machine:

```sh
curl -X POST http://127.0.0.1:3001/api/agents/token
```

`FOZMO_PAIRING_TOKEN` and `--token` remain deprecated aliases during the
staged migration.

Music folder management and library rescans are also local-only unless the
request includes a control-scoped credential. This keeps LAN peers and
cross-site browser pages from adding arbitrary filesystem roots to the library.

## Release Checks

Before calling LAN support public-ready:

- Confirm local mode binds only to `127.0.0.1`.
- Confirm the packaged default binds only to `127.0.0.1`.
- Confirm `--lan` binds to `0.0.0.0` and advertises trusted-LAN discovery.
- Confirm packaged LAN browsing works without a browser session on a trusted
  network.
- Smoke-test the optional `--require-pairing` mode and agent-token flow.
- Confirm Remote Access still rejects invalid or missing remote sessions.
- Confirm unrelated browser origins do not receive CORS authorization headers.
- Run `./tools/lan-pairing-smoke.sh`.
- Confirm pairing tokens and pairing records do not appear in logs, generated
  assets, settings examples, settings JSON, or committed files.
- Confirm agents reconnect cleanly after the core restarts.
