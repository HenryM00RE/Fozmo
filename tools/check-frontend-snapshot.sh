#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

build=1
require_clean=0

fail() {
  echo "error: $*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: tools/check-frontend-snapshot.sh [--no-build] [--require-clean]

Rebuild the React frontend and verify the committed static/react-app snapshot is
fresh. Use --no-build when a caller already ran npm --prefix ui run build.
Use --require-clean for release/public-readiness gates that should fail on any
uncommitted generated asset changes.
EOF
}

for arg in "$@"; do
  case "$arg" in
    --no-build)
      build=0
      ;;
    --require-clean)
      require_clean=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $arg"
      ;;
  esac
done

snapshot_manifest() {
  if [[ ! -d static/react-app ]]; then
    return 0
  fi
  find static/react-app -type f -print0 | sort -z | xargs -0 shasum -a 256
}

before_manifest="$(mktemp)"
after_manifest="$(mktemp)"
trap 'rm -f "$before_manifest" "$after_manifest"' EXIT

snapshot_manifest > "$before_manifest"

if [[ "$build" == "1" ]]; then
  echo "==> Building frontend snapshot"
  npm --prefix ui run build
fi

echo "==> Checking committed frontend snapshot"
test -f static/react-app/index.html || fail "missing static/react-app/index.html"
test -d static/react-app/assets || fail "missing static/react-app/assets"

tracked_files="$(git ls-files static/react-app)"
if [[ -z "$tracked_files" ]]; then
  fail "static/react-app is not tracked; update packaging before removing the committed snapshot"
fi

snapshot_manifest > "$after_manifest"
if ! cmp -s "$before_manifest" "$after_manifest"; then
  git status --short -- static/react-app | sed 's/^/    /' >&2
  fail "static/react-app changed during the frontend build; review the generated snapshot, then rerun this check"
fi

if [[ "$require_clean" == "1" ]]; then
  snapshot_status="$(git status --porcelain -- static/react-app)"
  if [[ -n "$snapshot_status" ]]; then
    echo "$snapshot_status" | sed 's/^/    /' >&2
    fail "static/react-app has uncommitted generated asset changes"
  fi
fi

echo "==> Frontend snapshot is fresh"
