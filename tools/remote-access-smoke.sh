#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

fail() {
  echo "error: $*" >&2
  if [[ -n "${LOG_FILE:-}" && -f "$LOG_FILE" ]]; then
    echo "---- server log ----" >&2
    tail -120 "$LOG_FILE" >&2
  fi
  exit 1
}

free_port() {
  node -e 'const net=require("net"); const s=net.createServer(); s.listen(0,"127.0.0.1",()=>{console.log(s.address().port); s.close();});'
}

port_listening() {
  local port="$1"
  node - "$port" <<'NODE'
const net = require("net");
const port = Number(process.argv[2]);
const socket = net.createConnection({ host: "127.0.0.1", port });
socket.setTimeout(500);
socket.on("connect", () => { socket.destroy(); process.exit(0); });
socket.on("timeout", () => { socket.destroy(); process.exit(1); });
socket.on("error", () => process.exit(1));
NODE
}

release_native_rustflags() {
  if [[ "${RUSTFLAGS:-}" == *"target-cpu="* ]]; then
    printf "%s" "$RUSTFLAGS"
  else
    printf "%s" "${RUSTFLAGS:+$RUSTFLAGS }-C target-cpu=native"
  fi
}

APP_PORT="${FOZMO_SMOKE_PORT:-}"
if [[ -z "$APP_PORT" ]]; then
  APP_PORT="$(free_port)"
fi
REMOTE_PORT="${FOZMO_REMOTE_SMOKE_PORT:-}"
if [[ -z "$REMOTE_PORT" ]]; then
  REMOTE_PORT="$(free_port)"
fi
while [[ "$REMOTE_PORT" == "$APP_PORT" ]]; do
  REMOTE_PORT="$(free_port)"
done

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fozmo-remote-access.XXXXXX")"
WORKSPACE_DIR="$TMP_ROOT/workspace"
LOG_FILE="$TMP_ROOT/server.log"
LOCAL_STATUS_JSON="$TMP_ROOT/local-status.json"
REMOTE_SETTINGS_JSON="$TMP_ROOT/remote-settings.json"
LINK_JSON="$TMP_ROOT/link-code.json"
SESSION_JSON="$TMP_ROOT/remote-session.json"
REMOTE_STATUS_JSON="$TMP_ROOT/remote-status.json"
REMOTE_HEADERS="$TMP_ROOT/remote-headers.txt"
COOKIE_JAR="$TMP_ROOT/remote-cookies.txt"
SERVER_PID=""
WAIT_ATTEMPTS="${FOZMO_SMOKE_WAIT_ATTEMPTS:-600}"
RUSTFLAGS_SMOKE="$(release_native_rustflags)"

cleanup() {
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

LOCAL_URL="http://127.0.0.1:$APP_PORT"
REMOTE_URL="https://127.0.0.1:$REMOTE_PORT"

echo "==> Building release core with development secrets support"
FOZMO_WORKSPACE_DIR="$WORKSPACE_DIR" \
FOZMO_SCAN_ON_STARTUP=0 \
FOZMO_DEV_SECRETS_FILE=1 \
RUSTFLAGS="$RUSTFLAGS_SMOKE" \
  cargo build --release --features dev-secrets-file

echo "==> Starting release core with remote access disabled"
FOZMO_WORKSPACE_DIR="$WORKSPACE_DIR" \
FOZMO_SCAN_ON_STARTUP=0 \
FOZMO_DEV_SECRETS_FILE=1 \
RUSTFLAGS="$RUSTFLAGS_SMOKE" \
  cargo run --release --features dev-secrets-file -- --lan --port="$APP_PORT" >"$LOG_FILE" 2>&1 &
SERVER_PID="$!"

echo "==> Waiting for local core on 127.0.0.1:$APP_PORT"
for _ in $(seq 1 "$WAIT_ATTEMPTS"); do
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    fail "server exited before becoming ready"
  fi
  if curl --fail --silent "$LOCAL_URL/api/status" >"$LOCAL_STATUS_JSON"; then
    break
  fi
  sleep 0.5
done
test -s "$LOCAL_STATUS_JSON" || fail "local status route did not become ready"

echo "==> Verifying remote port is closed by default"
if port_listening "$REMOTE_PORT"; then
  fail "remote port $REMOTE_PORT is listening before remote access is enabled"
fi

echo "==> Enabling remote access through the local settings API"
curl --fail --silent \
  --header "Content-Type: application/json" \
  --data "{\"enabled\":true,\"port\":$REMOTE_PORT}" \
  "$LOCAL_URL/api/remote/settings" >"$REMOTE_SETTINGS_JSON" \
  || fail "remote settings update failed"

node -e '
const body = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
if (body.settings?.enabled !== true) throw new Error("remote settings should be enabled");
if (body.settings?.port !== Number(process.argv[2])) throw new Error("remote port mismatch");
if (body.status?.running !== true) throw new Error(`remote listener not running: ${JSON.stringify(body.status)}`);
if (typeof body.status?.cert_fingerprint_sha256 !== "string") throw new Error("missing certificate fingerprint");
' "$REMOTE_SETTINGS_JSON" "$REMOTE_PORT" || fail "remote settings response shape was unexpected"

echo "==> Waiting for remote TLS listener on 127.0.0.1:$REMOTE_PORT"
for _ in $(seq 1 "$WAIT_ATTEMPTS"); do
  if port_listening "$REMOTE_PORT"; then
    break
  fi
  sleep 0.5
done
port_listening "$REMOTE_PORT" || fail "remote TLS listener did not become ready"

echo "==> Verifying plaintext HTTP fails on the remote port"
plain_http_code="$(curl --silent --show-error --max-time 5 \
  --output "$TMP_ROOT/plain-http.out" \
  --write-out '%{http_code}' \
  "http://127.0.0.1:$REMOTE_PORT/api/status" 2>"$TMP_ROOT/plain-http.err" || true)"
[[ "$plain_http_code" == "000" ]] || fail "expected plaintext HTTP to fail, got HTTP $plain_http_code"

echo "==> Verifying remote API requires a remote session cookie"
unauth_status="$(curl --insecure --silent --output /dev/null --write-out '%{http_code}' \
  "$REMOTE_URL/api/status")"
[[ "$unauth_status" == "401" ]] || fail "expected unauthenticated remote status to return 401, got $unauth_status"

echo "==> Verifying excluded remote routes are absent"
for method_path in \
  "POST /api/pairing/start" \
  "GET /api/hegel/status" \
  "GET /api/remote/settings"
do
  method="${method_path%% *}"
  path="${method_path#* }"
  status_code="$(curl --insecure --silent --output /dev/null --write-out '%{http_code}' \
    --request "$method" "$REMOTE_URL$path")"
  [[ "$status_code" == "404" ]] || fail "expected $method $path to return 404 remotely, got $status_code"
done

echo "==> Issuing a remote link code on the local router"
curl --fail --silent --request POST "$LOCAL_URL/api/remote/link-code" >"$LINK_JSON" \
  || fail "remote link code request failed"
REMOTE_CODE="$(node -e '
const body = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
if (typeof body.code !== "string" || body.code.length !== 43) throw new Error("expected 256-bit URL token code");
if (typeof body.expires_at_unix_secs !== "number" || body.expires_at_unix_secs <= 0) {
  throw new Error("missing link code expiry");
}
process.stdout.write(body.code);
' "$LINK_JSON")" || fail "remote link code response shape was unexpected"

echo "==> Exchanging link code for a remote session cookie"
curl --insecure --fail --silent \
  --cookie-jar "$COOKIE_JAR" \
  --header "Content-Type: application/json" \
  --data "{\"code\":\"$REMOTE_CODE\"}" \
  "$REMOTE_URL/api/remote/session" >"$SESSION_JSON" \
  || fail "remote session exchange failed"

node -e '
const body = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
if (body.token_kind !== "remote_session") throw new Error("expected remote_session kind");
if (!Array.isArray(body.scopes) || body.scopes.length !== 1 || body.scopes[0] !== "remote") {
  throw new Error("expected only the remote scope");
}
if (typeof body.expires_at_unix_secs !== "number" || body.expires_at_unix_secs <= 0) {
  throw new Error("missing session expiry");
}
' "$SESSION_JSON" || fail "remote session response shape was unexpected"

echo "==> Verifying remote session cookie authorizes status"
curl --insecure --fail --silent \
  --cookie "$COOKIE_JAR" \
  "$REMOTE_URL/api/status" >"$REMOTE_STATUS_JSON" \
  || fail "authenticated remote status request failed"

node -e '
const status = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
if (!status || typeof status !== "object") throw new Error("status is not an object");
if (typeof status.state !== "string") throw new Error("missing string state field");
if (typeof status.active_zone_id !== "string") throw new Error("missing string active_zone_id field");
' "$REMOTE_STATUS_JSON" || fail "remote status response shape was unexpected"

echo "==> Verifying authenticated remote browser stream routes"
local_stream_status="$(curl --insecure --silent --output /dev/null --write-out '%{http_code}' \
  --cookie "$COOKIE_JAR" "$REMOTE_URL/api/stream/local/1")"
[[ "$local_stream_status" == "404" ]] \
  || fail "expected authenticated missing local track to return 404, got $local_stream_status"
qobuz_stream_status="$(curl --insecure --silent --output /dev/null --write-out '%{http_code}' \
  --cookie "$COOKIE_JAR" "$REMOTE_URL/api/stream/qobuz/1")"
[[ "$qobuz_stream_status" == "500" ]] \
  || fail "expected authenticated logged-out Qobuz stream to return 500, got $qobuz_stream_status"

echo "==> Verifying remote security headers and no CORS"
curl --insecure --silent \
  --dump-header "$REMOTE_HEADERS" \
  --output "$REMOTE_STATUS_JSON" \
  --cookie "$COOKIE_JAR" \
  --header "Origin: https://evil.test" \
  "$REMOTE_URL/api/status" \
  || fail "remote security-header request failed"
grep -iq '^x-content-type-options: nosniff' "$REMOTE_HEADERS" || fail "missing X-Content-Type-Options"
grep -iq '^referrer-policy: no-referrer' "$REMOTE_HEADERS" || fail "missing Referrer-Policy"
grep -iq '^content-security-policy: .*default-src '\''self'\''' "$REMOTE_HEADERS" || fail "missing CSP default-src"
grep -iq '^content-security-policy: .*frame-ancestors '\''none'\''' "$REMOTE_HEADERS" || fail "missing CSP frame-ancestors"
if grep -iq '^access-control-allow-origin:' "$REMOTE_HEADERS"; then
  fail "remote response unexpectedly included CORS headers"
fi
if grep -iq '^strict-transport-security:' "$REMOTE_HEADERS"; then
  fail "self-signed remote identity should not emit HSTS"
fi

echo "==> Verifying invalid remote session exchanges are rate-limited"
rate_status=""
for attempt in $(seq 1 12); do
  invalid_headers="$TMP_ROOT/invalid-$attempt.headers"
  rate_status="$(curl --insecure --silent \
    --dump-header "$invalid_headers" \
    --output /dev/null \
    --write-out '%{http_code}' \
    --header "Content-Type: application/json" \
    --data '{"code":"not-a-code"}' \
    "$REMOTE_URL/api/remote/session")"
  if [[ "$rate_status" == "429" ]]; then
    grep -iq '^retry-after:' "$invalid_headers" || fail "429 response was missing Retry-After"
    break
  fi
  [[ "$rate_status" == "401" ]] || fail "expected invalid exchange to return 401 or 429, got $rate_status"
done
[[ "$rate_status" == "429" ]] || fail "invalid exchanges did not trigger rate limiting"

echo "==> Disabling remote access through the local settings API"
curl --fail --silent \
  --header "Content-Type: application/json" \
  --data "{\"enabled\":false,\"port\":$REMOTE_PORT}" \
  "$LOCAL_URL/api/remote/settings" >"$REMOTE_SETTINGS_JSON" \
  || fail "remote settings disable failed"

for _ in $(seq 1 "$WAIT_ATTEMPTS"); do
  if ! port_listening "$REMOTE_PORT"; then
    break
  fi
  sleep 0.5
done
if port_listening "$REMOTE_PORT"; then
  fail "remote port $REMOTE_PORT is still listening after disable"
fi

echo "==> Remote access smoke test passed"
