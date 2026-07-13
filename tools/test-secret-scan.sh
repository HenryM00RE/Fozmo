#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/fozmo-secret-scan-test.XXXXXX")"

cleanup() {
  rm -rf "$WORK"
}
trap cleanup EXIT

fail() {
  echo "error: $*" >&2
  exit 1
}

command -v gitleaks >/dev/null 2>&1 \
  || fail "gitleaks is required for secret-scan fixtures"

write_fake_credential() {
  printf '%s%s%s\n' \
    'gh' \
    'p_' \
    'aB3dE5fG7hJ9kL2mN4pQ6rS8tV0wX1yZ3A4B'
}

new_fixture() {
  rm -rf "$WORK/repo"
  mkdir -p "$WORK/repo/tools"
  cp "$ROOT_DIR/.gitleaks.toml" "$WORK/repo/.gitleaks.toml"
  cp "$ROOT_DIR/.gitleaks-worktree.toml" "$WORK/repo/.gitleaks-worktree.toml"
  cp "$ROOT_DIR/tools/secret-scan.sh" "$WORK/repo/tools/secret-scan.sh"
  chmod +x "$WORK/repo/tools/secret-scan.sh"
  git -C "$WORK/repo" init -q
  git -C "$WORK/repo" config user.email "fixture@example.invalid"
  git -C "$WORK/repo" config user.name "Fozmo Secret Scan Fixture"
  git -C "$WORK/repo" add .gitleaks.toml .gitleaks-worktree.toml tools/secret-scan.sh
  git -C "$WORK/repo" commit -qm "Add scanner"
}

expect_scan_passes() {
  local label="$1"
  if ! (cd "$WORK/repo" && ./tools/secret-scan.sh) >"$WORK/output" 2>&1; then
    cat "$WORK/output" >&2
    fail "scanner rejected $label"
  fi
}

expect_scan_rejected() {
  local label="$1"
  if (cd "$WORK/repo" && ./tools/secret-scan.sh) >"$WORK/output" 2>&1; then
    fail "scanner accepted $label"
  fi
}

for path in \
  library/deleted-credential.txt \
  docs/screenshots/private/deleted-credential.txt; do
  new_fixture
  base_branch="$(git -C "$WORK/repo" branch --show-current)"
  git -C "$WORK/repo" switch -q -c secret-history
  mkdir -p "$WORK/repo/$(dirname "$path")"
  write_fake_credential >"$WORK/repo/$path"
  git -C "$WORK/repo" add -- "$path"
  git -C "$WORK/repo" commit -qm "Add historical fixture"
  git -C "$WORK/repo" rm -q -- "$path"
  git -C "$WORK/repo" commit -qm "Delete historical fixture"
  git -C "$WORK/repo" switch -q "$base_branch"
  expect_scan_rejected "a deleted credential on another ref at $path"
done

new_fixture
for path in \
  library/untracked-credential.txt \
  docs/screenshots/private/untracked-credential.txt; do
  mkdir -p "$WORK/repo/$(dirname "$path")"
  write_fake_credential >"$WORK/repo/$path"
done
expect_scan_passes "allowlisted untracked local runtime data"

new_fixture
mkdir -p "$WORK/repo/src"
write_fake_credential >"$WORK/repo/src/untracked-credential.txt"
expect_scan_rejected "an untracked credential in an ordinary source path"

new_fixture
printf 'second commit\n' >"$WORK/repo/second.txt"
git -C "$WORK/repo" add second.txt
git -C "$WORK/repo" commit -qm "Add second commit"
rm -rf "$WORK/shallow"
git clone -q --depth 1 "file://$WORK/repo" "$WORK/shallow"
if (cd "$WORK/shallow" && ./tools/secret-scan.sh) >"$WORK/output" 2>&1; then
  fail "scanner accepted a shallow clone"
fi
grep -Fq "requires full history" "$WORK/output" \
  || { cat "$WORK/output" >&2; fail "shallow-clone failure was not actionable"; }

echo "==> Secret-scan regression fixtures passed"
