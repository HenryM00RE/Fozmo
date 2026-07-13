#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

ROOT="$(pwd)"
RUNTIME_ROOT="${FOZMO_REAL_E2E_ROOT:-$(mktemp -d "${TMPDIR:-/tmp}/fozmo-real-browser.XXXXXX")}"
APP_PORT="${FOZMO_REAL_E2E_PORT:-4188}"

cleanup() {
  rm -rf "$RUNTIME_ROOT"
}
trap cleanup EXIT INT TERM

mkdir -p "$RUNTIME_ROOT/data/library" "$RUNTIME_ROOT/cache" "$RUNTIME_ROOT/logs"

# Exercise startup recovery through the real process: an invalid primary has
# a valid last-known-good backup, and an empty version-0 SQLite file is
# migrated before the browser suite reaches /api/library/summary.
printf '%s\n' '{broken settings' >"$RUNTIME_ROOT/data/settings.json"
printf '%s\n' '{}' >"$RUNTIME_ROOT/data/settings.json.bak"
: >"$RUNTIME_ROOT/data/library/library.db"

export FOZMO_RESOURCE_DIR="$ROOT"
export FOZMO_DATA_DIR="$RUNTIME_ROOT/data"
export FOZMO_CACHE_DIR="$RUNTIME_ROOT/cache"
export FOZMO_LOG_DIR="$RUNTIME_ROOT/logs"
export FOZMO_SCAN_ON_STARTUP=0
export FOZMO_CORE_MDNS=0
export FOZMO_DEV_SECRETS_FILE=1

exec cargo run --features dev-secrets-file -- --lan --require-pairing --port="$APP_PORT"
