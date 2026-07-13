#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

fail() {
  echo "error: $*" >&2
  exit 1
}

git rev-parse --is-inside-work-tree >/dev/null 2>&1 \
  || fail "secret scanning must run from a Git worktree"

if [[ "$(git rev-parse --is-shallow-repository)" == "true" ]]; then
  fail "secret scanning requires full history; fetch it with 'git fetch --unshallow --tags' (or check out with fetch-depth: 0)"
fi

if ! command -v gitleaks >/dev/null 2>&1; then
  fail "gitleaks is required for secret scanning; install it from https://github.com/gitleaks/gitleaks"
fi

echo "==> Running Gitleaks scan across all fetched git refs"
gitleaks git --redact --config .gitleaks.toml \
  --log-opts="--all --full-history" .

echo "==> Running Gitleaks real worktree scan (including untracked files)"
gitleaks dir --redact --config .gitleaks-worktree.toml .

echo "==> Secret scan passed"
