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

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fozmo-release-startup.XXXXXX")"
WORKSPACE_DIR="$TMP_ROOT/workspace"
STATIC_DIR="$WORKSPACE_DIR/static"
LOG_FILE="$TMP_ROOT/server.log"
STATUS_JSON="$TMP_ROOT/status.json"
INDEX_HTML="$TMP_ROOT/index.html"
SERVER_PID=""

cleanup() {
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

test -f static/react-app/index.html || fail "missing static/react-app/index.html"
test -d static/react-app/assets || fail "missing static/react-app/assets"

mkdir -p "$STATIC_DIR"
cp -R static/react-app "$STATIC_DIR/react-app"

echo "==> Starting release app against a temporary workspace"
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

echo "==> Validating release status response"
node -e '
const status = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
if (!status || typeof status !== "object") throw new Error("status is not an object");
if (typeof status.state !== "string") throw new Error("missing string state field");
if (typeof status.active_zone_id !== "string") throw new Error("missing string active_zone_id field");
if (typeof status.upsampling_enabled !== "boolean") throw new Error("missing boolean upsampling_enabled field");
' "$STATUS_JSON" || fail "status response is not the expected shape"

echo "==> Validating served frontend shell"
curl --fail --silent "http://127.0.0.1:$PORT/" >"$INDEX_HTML" \
  || fail "frontend shell request failed"
node -e '
const html = require("fs").readFileSync(process.argv[1], "utf8");
if (!html.includes("<title>Fozmo</title>")) throw new Error("missing expected title");
if (!html.includes("id=\"root\"")) throw new Error("missing React root");
if (!html.includes("/react-app/assets/")) throw new Error("missing built asset references");
' "$INDEX_HTML" || fail "frontend shell is not the expected built app"

echo "==> Release startup smoke test passed"
