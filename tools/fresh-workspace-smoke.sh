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

PORT="${FOZMO_SMOKE_PORT:-}"
if [[ -z "$PORT" ]]; then
  PORT="$(node -e 'const net=require("net"); const s=net.createServer(); s.listen(0,"127.0.0.1",()=>{console.log(s.address().port); s.close();});')"
fi

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fozmo-fresh-workspace.XXXXXX")"
WORKSPACE_DIR="$TMP_ROOT/workspace"
LOG_FILE="$TMP_ROOT/server.log"
STATUS_JSON="$TMP_ROOT/status.json"
SERVER_PID=""

cleanup() {
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

echo "==> Starting release app against a fresh temporary workspace"
FOZMO_WORKSPACE_DIR="$WORKSPACE_DIR" \
FOZMO_SCAN_ON_STARTUP=0 \
  cargo run --locked --release -- --port="$PORT" >"$LOG_FILE" 2>&1 &
SERVER_PID="$!"

echo "==> Waiting for /api/status on 127.0.0.1:$PORT"
for _ in $(seq 1 180); do
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    fail "server exited before becoming ready"
  fi
  if curl --fail --silent "http://127.0.0.1:$PORT/api/status" >"$STATUS_JSON"; then
    break
  fi
  sleep 0.5
done

test -s "$STATUS_JSON" || fail "status endpoint did not become ready"

echo "==> Validating status response"
node -e '
const status = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
if (!status || typeof status !== "object") throw new Error("status is not an object");
if (typeof status.state !== "string") throw new Error("missing string state field");
if (typeof status.active_zone_id !== "string") throw new Error("missing string active_zone_id field");
if (typeof status.upsampling_enabled !== "boolean") throw new Error("missing boolean upsampling_enabled field");
' "$STATUS_JSON" || fail "status response is not the expected shape"

echo "==> Checking fresh runtime data was created only in the temp workspace"
test -d "$WORKSPACE_DIR/music" || fail "fresh music directory was not created"
test -d "$WORKSPACE_DIR/library" || fail "fresh library directory was not created"
test -f "$WORKSPACE_DIR/library/library.db" || fail "fresh library database was not created"

echo "==> Fresh workspace smoke test passed"
