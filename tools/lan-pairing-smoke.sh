#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

fail() {
  echo "error: $*" >&2
  if [[ -n "${LOG_FILE:-}" && -f "$LOG_FILE" ]]; then
    echo "---- server log ----" >&2
    tail -80 "$LOG_FILE" >&2
  fi
  exit 1
}

release_native_rustflags() {
  if [[ -n "${CI:-}" ]]; then
    printf "%s" "${RUSTFLAGS:-}"
    return
  fi
  if [[ "${RUSTFLAGS:-}" == *"target-cpu="* ]]; then
    printf "%s" "$RUSTFLAGS"
  else
    printf "%s" "${RUSTFLAGS:+$RUSTFLAGS }-C target-cpu=native"
  fi
}

PORT="${FOZMO_SMOKE_PORT:-}"
if [[ -z "$PORT" ]]; then
  PORT="$(node -e 'const net=require("net"); const s=net.createServer(); s.listen(0,"127.0.0.1",()=>{console.log(s.address().port); s.close();});')"
fi

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fozmo-lan-pairing.XXXXXX")"
WORKSPACE_DIR="$TMP_ROOT/workspace"
LOG_FILE="$TMP_ROOT/server.log"
PAIRING_JSON="$TMP_ROOT/pairing.json"
SESSION_JSON="$TMP_ROOT/session.json"
COOKIE_JAR="$TMP_ROOT/cookies.txt"
AGENT_JSON="$TMP_ROOT/agent-token.json"
STATUS_JSON="$TMP_ROOT/status.json"
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

echo "==> Building release LAN core"
FOZMO_WORKSPACE_DIR="$WORKSPACE_DIR" \
FOZMO_SCAN_ON_STARTUP=0 \
FOZMO_DEV_SECRETS_FILE=1 \
RUSTFLAGS="$RUSTFLAGS_SMOKE" \
  cargo build --release --features dev-secrets-file

echo "==> Starting release LAN core with pairing required"
FOZMO_WORKSPACE_DIR="$WORKSPACE_DIR" \
FOZMO_SCAN_ON_STARTUP=0 \
FOZMO_DEV_SECRETS_FILE=1 \
RUSTFLAGS="$RUSTFLAGS_SMOKE" \
  cargo run --release --features dev-secrets-file -- --lan --require-pairing --port="$PORT" >"$LOG_FILE" 2>&1 &
SERVER_PID="$!"

echo "==> Waiting for pairing route on 127.0.0.1:$PORT"
for _ in $(seq 1 "$WAIT_ATTEMPTS"); do
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    fail "server exited before becoming ready"
  fi
  if curl --fail --silent --request POST "http://127.0.0.1:$PORT/api/pairing/start" >"$PAIRING_JSON"; then
    break
  fi
  sleep 0.5
done

test -s "$PAIRING_JSON" || fail "pairing route did not become ready"

echo "==> Verifying protected route rejects missing token"
status_code="$(curl --silent --output /dev/null --write-out '%{http_code}' "http://127.0.0.1:$PORT/api/status")"
[[ "$status_code" == "401" ]] || fail "expected /api/status without token to return 401, got $status_code"

echo "==> Reading pairing token"
TOKEN="$(node -e '
const pairing = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
if (!pairing || typeof pairing !== "object") throw new Error("pairing is not an object");
if (pairing.auth_required !== true) throw new Error("auth_required should be true");
if (typeof pairing.token !== "string" || pairing.token.length < 16) throw new Error("missing token");
if (pairing.token_kind !== "pairing_token") throw new Error("expected pairing_token kind");
if (!Array.isArray(pairing.scopes) || !pairing.scopes.includes("session:create")) {
  throw new Error("expected session:create scope");
}
if (typeof pairing.expires_at_unix_secs !== "number" || pairing.expires_at_unix_secs <= 0) {
  throw new Error("missing expires_at_unix_secs");
}
process.stdout.write(pairing.token);
' "$PAIRING_JSON")" || fail "pairing response is not the expected shape"

echo "==> Verifying raw pairing token does not authorize protected route"
pairing_header_status="$(curl --silent --output /dev/null --write-out '%{http_code}' \
  --header "x-fozmo-token: $TOKEN" \
  "http://127.0.0.1:$PORT/api/status")"
[[ "$pairing_header_status" == "401" ]] || fail "expected raw pairing token header to return 401, got $pairing_header_status"

echo "==> Exchanging pairing token for browser session cookie"
curl --fail --silent \
  --cookie-jar "$COOKIE_JAR" \
  --header "Content-Type: application/json" \
  --data "{\"pairing_token\":\"$TOKEN\"}" \
  "http://127.0.0.1:$PORT/api/sessions/browser" >"$SESSION_JSON" \
  || fail "browser session exchange failed"

node -e '
const session = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
if (!session || typeof session !== "object") throw new Error("session is not an object");
if (session.token_kind !== "control_session") throw new Error("expected control_session kind");
if (!Array.isArray(session.scopes) || !session.scopes.includes("control")) {
  throw new Error("expected control scope");
}
if (typeof session.expires_at_unix_secs !== "number" || session.expires_at_unix_secs <= 0) {
  throw new Error("missing session expires_at_unix_secs");
}
' "$SESSION_JSON" || fail "browser session response is not the expected shape"

echo "==> Verifying browser session cookie authorizes protected route"
curl --fail --silent \
  --cookie "$COOKIE_JAR" \
  "http://127.0.0.1:$PORT/api/status" >"$STATUS_JSON" \
  || fail "status request with browser session cookie failed"

echo "==> Verifying consumed pairing token cannot be reused"
reuse_status="$(curl --silent --output /dev/null --write-out '%{http_code}' \
  --header "Content-Type: application/json" \
  --data "{\"pairing_token\":\"$TOKEN\"}" \
  "http://127.0.0.1:$PORT/api/sessions/browser")"
[[ "$reuse_status" == "401" ]] || fail "expected consumed pairing token reuse to return 401, got $reuse_status"

echo "==> Verifying query token is rejected by default"
query_status="$(curl --silent --output /dev/null --write-out '%{http_code}' \
  "http://127.0.0.1:$PORT/api/status?token=$TOKEN")"
[[ "$query_status" == "401" ]] || fail "expected query token to return 401 by default, got $query_status"

echo "==> Verifying local agent token issuance"
curl --fail --silent --request POST "http://127.0.0.1:$PORT/api/agents/token" >"$AGENT_JSON" \
  || fail "agent token request failed"
AGENT_TOKEN="$(node -e '
const agent = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
if (!agent || typeof agent !== "object") throw new Error("agent token response is not an object");
if (agent.token_kind !== "agent_token") throw new Error("expected agent_token kind");
if (typeof agent.token !== "string" || agent.token.length < 16) throw new Error("missing agent token");
if (!Array.isArray(agent.scopes) || !agent.scopes.includes("agent:connect") || !agent.scopes.includes("stream:read")) {
  throw new Error("expected agent scopes");
}
process.stdout.write(agent.token);
' "$AGENT_JSON")" || fail "agent token response is not the expected shape"

agent_status="$(curl --silent --output /dev/null --write-out '%{http_code}' \
  --header "x-fozmo-token: $AGENT_TOKEN" \
  "http://127.0.0.1:$PORT/api/status")"
[[ "$agent_status" == "401" ]] || fail "expected narrow agent token to return 401 for /api/status, got $agent_status"

echo "==> Validating authorized status response"
node -e '
const status = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
if (!status || typeof status !== "object") throw new Error("status is not an object");
if (typeof status.state !== "string") throw new Error("missing string state field");
if (typeof status.active_zone_id !== "string") throw new Error("missing string active_zone_id field");
' "$STATUS_JSON" || fail "authorized status response is not the expected shape"

echo "==> LAN pairing smoke test passed"
