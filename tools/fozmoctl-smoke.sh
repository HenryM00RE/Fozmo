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

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fozmoctl-smoke.XXXXXX")"
WORKSPACE_DIR="$TMP_ROOT/workspace"
MUSIC_DIR="$WORKSPACE_DIR/music"
LOG_FILE="$TMP_ROOT/server.log"
SEARCH_JSON="$TMP_ROOT/search.json"
QUEUE_JSON="$TMP_ROOT/queue.json"
SUMMARY_JSON="$TMP_ROOT/queue-summary.json"
SERVER_PID=""

cleanup() {
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

mkdir -p "$MUSIC_DIR"

node - "$MUSIC_DIR/01 Smoke.wav" "$MUSIC_DIR/02 Smoke.wav" <<'NODE'
const fs = require('fs');
const paths = process.argv.slice(2);
const sampleRate = 8000;
const frames = 800;
const dataBytes = frames * 2;
const buffer = Buffer.alloc(44 + dataBytes);
buffer.write('RIFF', 0);
buffer.writeUInt32LE(36 + dataBytes, 4);
buffer.write('WAVE', 8);
buffer.write('fmt ', 12);
buffer.writeUInt32LE(16, 16);
buffer.writeUInt16LE(1, 20);
buffer.writeUInt16LE(1, 22);
buffer.writeUInt32LE(sampleRate, 24);
buffer.writeUInt32LE(sampleRate * 2, 28);
buffer.writeUInt16LE(2, 32);
buffer.writeUInt16LE(16, 34);
buffer.write('data', 36);
buffer.writeUInt32LE(dataBytes, 40);
for (const path of paths) fs.writeFileSync(path, buffer);
NODE

echo "==> Building fozmo and fozmoctl"
cargo build --bin fozmo --bin fozmoctl >/dev/null

echo "==> Starting local core on 127.0.0.1:$PORT"
FOZMO_WORKSPACE_DIR="$WORKSPACE_DIR" \
FOZMO_SCAN_ON_STARTUP=1 \
  ./target/debug/fozmo --port="$PORT" >"$LOG_FILE" 2>&1 &
SERVER_PID="$!"

echo "==> Waiting for core"
for _ in $(seq 1 120); do
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    fail "server exited before becoming ready"
  fi
  if curl --fail --silent "http://127.0.0.1:$PORT/api/status" >/dev/null; then
    break
  fi
  sleep 0.5
done

curl --fail --silent "http://127.0.0.1:$PORT/api/status" >/dev/null \
  || fail "core did not become ready"

echo "==> Waiting for startup scan"
for _ in $(seq 1 120); do
  ./target/debug/fozmoctl --core-url "http://127.0.0.1:$PORT" search Smoke --json >"$SEARCH_JSON" \
    || fail "fozmoctl local search failed"
  if node - "$SEARCH_JSON" <<'NODE'
const fs = require('fs');
const search = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const tracks = Array.isArray(search.tracks) ? search.tracks : [];
const names = new Set(tracks.map(track => track && track.file_name));
process.exit(names.has('01 Smoke.wav') && names.has('02 Smoke.wav') ? 0 : 1);
NODE
  then
    break
  fi
  sleep 0.5
done

TRACK_IDS="$(node - "$SEARCH_JSON" <<'NODE'
const fs = require('fs');
const search = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const tracks = search.tracks || [];
const first = tracks.find(track => track && track.file_name === '01 Smoke.wav');
const second = tracks.find(track => track && track.file_name === '02 Smoke.wav');
if (!first || !Number.isFinite(Number(first.id))) throw new Error('missing first Smoke track id');
if (!second || !Number.isFinite(Number(second.id))) throw new Error('missing second Smoke track id');
process.stdout.write(`${first.id} ${second.id}`);
NODE
)" || fail "could not read local track ids"
read -r TRACK_ID TRACK_ID_TWO <<<"$TRACK_IDS"

echo "==> Queueing local tracks $TRACK_ID and $TRACK_ID_TWO"
./target/debug/fozmoctl --core-url "http://127.0.0.1:$PORT" queue add-many "local:$TRACK_ID" "local:$TRACK_ID_TWO" \
  || fail "fozmoctl local queue add-many failed"

./target/debug/fozmoctl --core-url "http://127.0.0.1:$PORT" queue get --json >"$QUEUE_JSON" \
  || fail "fozmoctl queue get failed"

node - "$QUEUE_JSON" "$TRACK_ID" "$TRACK_ID_TWO" <<'NODE'
const fs = require('fs');
const queue = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const trackIds = process.argv.slice(3).map(Number);
const queued = Array.isArray(queue.queued_sources) ? queue.queued_sources : [];
for (const trackId of trackIds) {
  if (!queued.some(source => source && source.kind === 'local_track' && Number(source.track_id) === trackId)) {
    throw new Error(`queued_sources does not include local track ${trackId}`);
  }
}
const items = queue.state && Array.isArray(queue.state.items) ? queue.state.items : [];
for (const trackId of trackIds) {
  if (!items.some(item => item && item.resolvedSource && Number(item.resolvedSource.track_id) === trackId)) {
    throw new Error(`now-playing queue state does not include local track ${trackId}`);
  }
}
NODE

./target/debug/fozmoctl --core-url "http://127.0.0.1:$PORT" queue get --summary --json >"$SUMMARY_JSON" \
  || fail "fozmoctl queue summary failed"

node - "$SUMMARY_JSON" "$TRACK_ID" "$TRACK_ID_TWO" <<'NODE'
const fs = require('fs');
const summary = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const trackIds = process.argv.slice(3).map(id => `local:${id}`);
const queued = Array.isArray(summary.queued) ? summary.queued : [];
for (const sourceKey of trackIds) {
  const item = queued.find(source => source && source.source_key === sourceKey);
  if (!item) throw new Error(`summary missing ${sourceKey}`);
  if (item.kind !== 'local_track') throw new Error(`summary kind mismatch for ${sourceKey}`);
  if (typeof item.title !== 'string') throw new Error(`summary title missing for ${sourceKey}`);
}
NODE

echo "==> fozmoctl smoke test passed"
