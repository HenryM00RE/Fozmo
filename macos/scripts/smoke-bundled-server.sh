#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "$0")" && pwd)/common.sh"

require_command curl
require_command python3

APP_PATH="${1:-$BUILD_DIR/Fozmo.app}"
SERVER="$APP_PATH/Contents/Helpers/fozmo-server"
RESOURCES="$APP_PATH/Contents/Resources"
[[ -x "$SERVER" ]] || die "bundled server is missing: $SERVER"
[[ -f "$RESOURCES/static/react-app/index.html" ]] \
  || die "bundled frontend is missing"

umask 077
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fozmo-bundled-smoke.XXXXXX")"
LOG_FILE="$TMP_ROOT/server.log"
STATUS_FILE="$TMP_ROOT/status.json"
INDEX_FILE="$TMP_ROOT/index.html"
SERVER_PID=""

cleanup() {
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

PORT="$(python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)"

mkdir -p \
  "$TMP_ROOT/data" \
  "$TMP_ROOT/cache" \
  "$TMP_ROOT/logs" \
  "$TMP_ROOT/home" \
  "$TMP_ROOT/tmp"
printf '%s\n' 'fozmo-release-smoke-v1' > "$TMP_ROOT/.fozmo-release-smoke"
chmod 700 "$TMP_ROOT"
chmod 600 "$TMP_ROOT/.fozmo-release-smoke"
note "Starting the packaged server with isolated fresh runtime roots"
env -i \
  PATH=/usr/bin:/bin:/usr/sbin:/sbin \
  HOME="$TMP_ROOT/home" \
  TMPDIR="$TMP_ROOT/tmp" \
  LANG=C \
  FOZMO_RESOURCE_DIR="$RESOURCES" \
  FOZMO_DATA_DIR="$TMP_ROOT/data" \
  FOZMO_CACHE_DIR="$TMP_ROOT/cache" \
  FOZMO_LOG_DIR="$TMP_ROOT/logs" \
  FOZMO_MODE=core \
  FOZMO_LAN=0 \
  FOZMO_REQUIRE_PAIRING=0 \
  FOZMO_ALLOW_QUERY_TOKEN_AUTH=0 \
  FOZMO_SCAN_ON_STARTUP=0 \
  FOZMO_CORE_MDNS=0 \
  FOZMO_EXIT_ON_STDIN_EOF=0 \
  FOZMO_LOG_FORMAT=compact \
  FOZMO_PORT="$PORT" \
  FOZMO_PUBLIC_BASE_URL="http://127.0.0.1:$PORT" \
  "$SERVER" \
    --release-smoke \
    --local-only \
    --no-core-mdns \
    --no-require-pairing \
    --port="$PORT" >"$LOG_FILE" 2>&1 &
SERVER_PID="$!"

for _ in $(seq 1 360); do
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    tail -80 "$LOG_FILE" >&2 || true
    die "bundled server exited before becoming ready"
  fi
  if curl --fail --silent "http://127.0.0.1:$PORT/api/status" >"$STATUS_FILE"; then
    break
  fi
  sleep 0.25
done

[[ -s "$STATUS_FILE" ]] || {
  tail -80 "$LOG_FILE" >&2 || true
  die "bundled server status endpoint did not become ready"
}
curl --fail --silent "http://127.0.0.1:$PORT/healthz" >/dev/null \
  || die "bundled server health check failed"
curl --fail --silent "http://127.0.0.1:$PORT/" >"$INDEX_FILE" \
  || die "bundled server did not serve the packaged frontend"
grep -q '<title>Fozmo</title>' "$INDEX_FILE" \
  || die "bundled server did not serve the packaged frontend"
python3 - "$STATUS_FILE" <<'PY'
import json
import pathlib
import sys

status = json.loads(pathlib.Path(sys.argv[1]).read_text())
if not isinstance(status.get("state"), str):
    raise SystemExit("bundled status is missing playback state")
if not isinstance(status.get("active_zone_id"), str):
    raise SystemExit("bundled status is missing its active zone")
PY

[[ -f "$TMP_ROOT/data/library/library.db" ]] \
  || die "bundled server did not initialize isolated durable data"
note "Packaged server and frontend startup smoke passed"
