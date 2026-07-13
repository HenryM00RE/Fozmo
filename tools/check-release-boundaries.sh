#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

fail() {
  echo "error: $*" >&2
  exit 1
}

HELPER_MANIFEST="${HELPER_MANIFEST:-airplay-helper/Cargo.toml}"
SERVER_BIN="${SERVER_BIN:-target/release/fozmo}"
HELPER_BIN="${HELPER_BIN:-airplay-helper/target/release/fozmo-airplay-helper}"

[[ -f "$HELPER_MANIFEST" ]] || fail "missing standalone AirPlay helper manifest: $HELPER_MANIFEST"

echo "==> Verifying MIT server dependency boundary"
server_tree="$(cargo tree --locked --manifest-path Cargo.toml -e normal)"
if grep -E '(^|[[:space:]])(airplay-(audio|client|core|crypto|discovery|pairing|resampler|rtsp|timing)|fdk-aac(-sys)?)([[:space:]]|$)' <<<"$server_tree"; then
  fail "MIT server dependency graph contains GPL AirPlay or FDK code"
fi

echo "==> Verifying GPL helper excludes FDK AAC"
helper_tree="$(cargo tree --locked --manifest-path "$HELPER_MANIFEST" -e normal)"
if grep -E '(^|[[:space:]])fdk-aac(-sys)?([[:space:]]|$)' <<<"$helper_tree"; then
  fail "AirPlay helper dependency graph contains FDK AAC"
fi

echo "==> Verifying package license metadata"
server_license="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; d=json.load(sys.stdin); print(next(p for p in d["packages"] if p["name"]=="fozmo").get("license") or "")')"
helper_license="$(cargo metadata --manifest-path "$HELPER_MANIFEST" --no-deps --format-version 1 | python3 -c 'import json,sys; d=json.load(sys.stdin); print(next(p for p in d["packages"] if p["name"]=="fozmo-airplay-helper").get("license") or "")')"
[[ "$server_license" == "MIT" ]] || fail "server package license must be MIT (found '$server_license')"
[[ "$helper_license" == "GPL-2.0-only" ]] || fail "helper package license must be GPL-2.0-only (found '$helper_license')"

check_symbols() {
  local binary="$1"
  local label="$2"
  if [[ ! -f "$binary" ]]; then
    if [[ "${REQUIRE_RELEASE_BINARIES:-0}" == "1" ]]; then
      fail "missing $label release binary: $binary"
    fi
    echo "    skipping $label symbol audit (binary not built)"
    return
  fi
  if nm "$binary" 2>/dev/null | grep -E '(_FDK|FDKaac|aacEnc|fdk_aac)' >/dev/null; then
    fail "$label contains forbidden FDK AAC symbols"
  fi
}

echo "==> Auditing release binary symbols"
check_symbols "$SERVER_BIN" "MIT server"
check_symbols "$HELPER_BIN" "GPL AirPlay helper"

echo "==> Release license/process boundaries passed"
