# Remote Access

Remote Access exposes a second TLS-only Fozmo listener for manual router
port forwarding. It is off by default and should stay off unless you understand
that the forwarded port is reachable from the internet.

## Threat Model

Assume the forwarded port is scanned constantly by bots. Assume an attacker can
read this repository, knows the route names, and can send arbitrary HTTPS
requests to the remote listener. Remote Access therefore uses a separate
allowlisted router, mandatory remote-session cookies, rate-limited auth
failures, and no remote setting mutation routes.

The remote listener is not a reverse-proxy mode. It does not trust
`X-Forwarded-For`, `X-Forwarded-Proto`, or `external_host`. The host value is
only a link/QR hint.

## What Is Exposed

After a remote browser is linked, the remote listener exposes only the
allowlisted app API needed for browsing and control. Sensitive local
administration routes are absent and return `404`, including remote settings
mutation, link-code creation, remote session management, pairing recovery,
agent-token minting, diagnostics, uploads, library folder management, Hegel
control, Qobuz account auth/settings/cache mutation, and Sonos/UPnP media
endpoints.

Remote sessions cannot enable Remote Access, change ports or hosts, create link
codes, list or revoke sessions, add library folders, manage Qobuz auth, or mint
credentials.

## Why Auth Is Mandatory

LAN pairing can be optional during private development. Remote auth is never
optional. Every remote API request except `POST /api/remote/session` requires a
valid `fozmo_remote_session` cookie with the remote scope. LAN control
cookies, bearer/header tokens, query tokens, loopback shortcuts, and local
filesystem bypasses are rejected on the remote listener.

## Setup

1. Open Fozmo from the local machine; enabling or disabling Remote Access is
   intentionally host-only.
2. Go to Settings, Remote Access.
3. Leave the feature off until the port-forwarding plan is ready.
4. Enter the external DNS name or public IP you control. Fozmo does not call
   any public "what is my IP" service.
5. Keep the default TCP port `8443` or choose another port that does not collide
   with the main app port.
6. Reserve the server computer's LAN IP in your router DHCP settings.
7. Forward the external TCP port to the same TCP port on the server computer.
8. Enable Remote Access and confirm the listener is running.
9. From a phone on cellular or another non-LAN network, open the generated URL.

CGNAT and some IPv6-only ISP setups may not support manual IPv4 port forwarding.
In that case, the router may have no public IPv4 address to forward.

## TLS And Fingerprints

When no custom certificate is configured, Fozmo generates a self-signed TLS
certificate and persists the private key through the secret store. Browsers will
usually show a warning on first connection because the certificate is not signed
by a public certificate authority.

Before proceeding through that browser warning, compare the browser certificate
SHA-256 fingerprint with the fingerprint shown in the local Remote Access
settings page. Treat that fingerprint as the trust anchor for the self-signed
path. If the values do not match, stop and investigate.

If you configure your own certificate and key, trust is managed by that
certificate path instead. HSTS is intended for user-managed trusted
certificates, not the generated self-signed path.

## Link A Device

1. Enable Remote Access and confirm it is running.
2. Click Generate link code from the local/LAN settings page.
3. Copy the full code, copy the URL, or scan the QR code.
4. Open the link from the remote browser.
5. Verify the TLS fingerprint before accepting the first connection.
6. The browser exchanges the full high-entropy link token for a remote session
   cookie and clears `#link=` from the address bar on success.

Link codes are high-entropy, single-use, and short-lived. The UI may group the
token for readability, but it does not shorten or replace the backend token.
Codes are not stored in local storage or session storage.

## Revoke A Device

Use the local/LAN Remote Access settings page to refresh active sessions and
revoke a linked device. Revocation affects remote sessions only; it does not
revoke LAN control sessions, agent tokens, stream tokens, or unrelated secrets.

Revoke lost devices promptly. If the server certificate private key or local
machine is suspected compromised, disable Remote Access, revoke sessions, rotate
or recreate the certificate identity, and review router forwarding.

## Session Hygiene

- Keep Remote Access off when you do not need it.
- Link only trusted browsers and devices.
- Revoke devices you no longer use.
- Keep the server OS and browsers updated.
- Keep router firmware current and avoid forwarding unnecessary ports.
- Recreate or rotate the remote TLS identity if its private key may have leaked.

## Mobile Browser Playback

Each browser output has its own stream-format setting under Settings → Outputs.
Authenticated Remote Access browsers can change their own output between
lossless FLAC and Ogg Opus, including the Opus bitrate. The same setting is
available to LAN browsers. It is stored per browser output and applies from the
next track.

Both formats continue to use Fozmo's same-origin proxy. Signed Qobuz CDN URLs,
stream tokens, and remote session tokens are not exposed in browser playback
URLs.

## Known Risks And Limits

- Self-signed certificates produce a browser warning on first connect.
- Browser HSTS behavior with self-signed certificates can be awkward; custom
  trusted certificates are smoother for frequent use.
- CGNAT and IPv6-only ISP setups may not work with manual IPv4 forwarding.
- Remote browser local playback derivatives are only a playback cache, not a
  permanent export/transcode library.
- No public-IP detection call is made. The user supplies `external_host`
  manually.

## Smoke Test

`tools/remote-access-smoke.sh` exercises the backend contract: disabled by
default, TLS-only remote port, unauthenticated rejection, 404s for excluded
routes, link-code exchange, security headers, and auth rate limiting.
