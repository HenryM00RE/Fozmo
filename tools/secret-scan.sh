#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

fail() {
  echo "error: $*" >&2
  exit 1
}

if ! command -v gitleaks >/dev/null 2>&1; then
  fail "gitleaks is required for secret scanning; install it from https://github.com/gitleaks/gitleaks"
fi

echo "==> Running Gitleaks git history scan"
gitleaks git --redact --config .gitleaks.toml .

echo "==> Running Gitleaks tracked working tree scan"
TRACKED_SNAPSHOT="$(mktemp -d "${TMPDIR:-/tmp}/fozmo-gitleaks.XXXXXX")"
cleanup() {
  rm -rf "$TRACKED_SNAPSHOT"
}
trap cleanup EXIT
while IFS= read -r -d '' path; do
  [[ -f "$path" || -L "$path" ]] || continue
  mkdir -p "$TRACKED_SNAPSHOT/$(dirname "$path")"
  cp -P "./$path" "$TRACKED_SNAPSHOT/$path"
done < <(git ls-files -z)
gitleaks dir --redact --config .gitleaks.toml "$TRACKED_SNAPSHOT"

echo "==> Secret scan passed"
