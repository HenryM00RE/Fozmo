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

expect_ignored() {
  local path="$1"
  git -C "$ROOT_DIR" check-ignore --quiet --no-index -- "$path" \
    || { echo "error: worktree-allowlisted fixture is not ignored by Git: $path" >&2; exit 1; }
}

new_fixture
track_fixture settings.example.json
track_fixture static/react-app/index.html
"$CHECKER" "$WORK/repo" >/dev/null

for path in \
  .DS_Store \
  .fozmo.lock \
  install.json \
  target/debug/fozmo \
  airplay-helper/target/debug/fozmo-airplay-helper \
  macos/FozmoLauncher/.build/workspace-state.json \
  macos/FozmoLauncher/.swiftpm/configuration.json \
  macos/build/Fozmo.pkg \
  dist/Fozmo.dmg \
  drivers/fozmo-capture/build/FozmoCapture.driver/Contents/Info.plist \
  music/local.flac \
  library/runtime.json \
  backups/backup-1/manifest.json \
  logs/fozmo.log \
  secrets.dev.json \
  settings.json \
  settings.json.bak \
  settings.json.recovery-required \
  settings.json.corrupt-123 \
  settings.local.json \
  static/user-fonts/private.ttf \
  runtime/fozmo.log \
  runtime/library.sqlite \
  runtime/library.db \
  scratch/note.txt \
  docs/manual-smoke-evidence.local.md \
  docs/screenshots/local/capture.png \
  docs/screenshots/private/capture.png \
  ui/node_modules/example/index.js \
  ui/.vite/deps/example.js \
  ui/coverage/index.html \
  ui/playwright-report/index.html \
  ui/test-results/results.json \
  ui/library/runtime.json \
  ui/music/local.flac \
  ui/presets/local.json \
  ui/static/runtime.json \
  audio_tests/out/result.wav \
  tools/.venv/bin/python; do
  expect_ignored "$path"
  expect_rejected "$path"
done

for path in \
  secrets/release.pem \
  release/Fozmo.dmg \
  release/Fozmo.app/Contents/Info.plist \
  library/runtime.sqlite3 \
  runtime/library.sqlite3; do
  expect_rejected "$path"
done

expect_rejected "static/react-app/assets/index.js"

echo "==> Public-readiness force-add fixtures passed"
