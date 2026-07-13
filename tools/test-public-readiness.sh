#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
CHECKER="$ROOT_DIR/tools/check-tracked-public-files.sh"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/fozmo-public-readiness-test.XXXXXX")"

cleanup() {
  rm -rf "$WORK"
}
trap cleanup EXIT

new_fixture() {
  rm -rf "$WORK/repo"
  mkdir -p "$WORK/repo"
  git -C "$WORK/repo" init -q
}

track_fixture() {
  local path="$1"
  mkdir -p "$WORK/repo/$(dirname "$path")"
  printf 'fixture\n' >"$WORK/repo/$path"
  git -C "$WORK/repo" add -f -- "$path"
}

expect_rejected() {
  local path="$1"
  new_fixture
  track_fixture "$path"
  if "$CHECKER" "$WORK/repo" >"$WORK/output" 2>&1; then
    echo "error: checker accepted force-added forbidden path: $path" >&2
    exit 1
  fi
  grep -Fq "$path" "$WORK/output" \
    || { echo "error: checker failed without identifying $path" >&2; exit 1; }
}

new_fixture
track_fixture settings.example.json
track_fixture static/react-app/index.html
"$CHECKER" "$WORK/repo" >/dev/null

for path in \
  .fozmo.lock \
  install.json \
  backups/backup-1/manifest.json \
  logs/fozmo.log \
  secrets.dev.json \
  settings.json.bak \
  settings.json.recovery-required \
  settings.json.corrupt-123 \
  settings.local.json \
  secrets/release.pem \
  release/Fozmo.dmg \
  release/Fozmo.app/Contents/Info.plist \
  drivers/fozmo-capture/build/FozmoCapture.driver/Contents/Info.plist \
  library/runtime.sqlite3 \
  static/user-fonts/private.ttf \
  macos/build/Fozmo.pkg; do
  expect_rejected "$path"
done

expect_rejected "static/react-app/assets/index.js"

echo "==> Public-readiness force-add fixtures passed"
