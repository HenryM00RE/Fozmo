#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "$0")" && pwd)/common.sh"

require_command codesign
require_command cargo
require_command cargo-deny
require_command curl
require_command git
require_command gitleaks
require_command npm
require_command node
require_command python3
require_command rustc
require_command shasum
require_command swift
require_command xcodebuild

export FOZMO_BUILD_MODE=unsigned-public
VERSION="${VERSION:-$(project_version)}"
PROJECT_VERSION="$(project_version)"
[[ "$VERSION" == "$PROJECT_VERSION" ]] \
  || die "unsigned-public VERSION must match Cargo.toml ($PROJECT_VERSION)"
[[ "$VERSION" == "0.0.2" ]] \
  || die "this unsigned pre-alpha release entry point is currently limited to version 0.0.2"
verify_gpl_aggregation_policy "$FOZMO_BUILD_MODE" "$VERSION"

if [[ -n "$(git -C "$ROOT_DIR" status --porcelain --untracked-files=normal)" ]]; then
  die "unsigned-public releases require a clean worktree"
fi
verify_public_toolchain

note "Installing the locked frontend dependency graph for verification"
npm --prefix "$ROOT_DIR/ui" ci

COMMIT="$(git -C "$ROOT_DIR" rev-parse HEAD)"
TREE="$(git -C "$ROOT_DIR" rev-parse 'HEAD^{tree}')"
RELEASE_DIR="$BUILD_DIR/unsigned-release-$VERSION"
DMG="$RELEASE_DIR/Fozmo-$VERSION-macos-arm64.dmg"
SOURCE_ARCHIVE="$RELEASE_DIR/Fozmo-$VERSION-source.tar.zst"
RELEASE_NOTES_SOURCE="$MACOS_DIR/release-notes/$VERSION.md"
RELEASE_NOTES="$RELEASE_DIR/Fozmo-$VERSION-macos-arm64.md"
SOURCE_BUILD_RECEIPT="$RELEASE_DIR/source-build-verification.json"
VERIFICATION_RECEIPT="$RELEASE_DIR/release-verification.json"
SOURCE_BUILD_TARGET_DIR="$BUILD_DIR/source-archive-verification-target"
MANIFEST="$RELEASE_DIR/build-manifest.json"
CHECKSUMS="$RELEASE_DIR/SHA256SUMS"

[[ -s "$RELEASE_NOTES_SOURCE" ]] \
  || die "versioned release notes are missing: $RELEASE_NOTES_SOURCE"

VERIFY_STARTED_AT="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
note "Running the complete release verification suite"
"$ROOT_DIR/tools/verify.sh"

note "Running public snapshot readiness checks"
"$ROOT_DIR/tools/public-readiness.sh"

note "Running LAN and remote-access release smoke checks"
"$ROOT_DIR/tools/lan-pairing-smoke.sh"
"$ROOT_DIR/tools/remote-access-smoke.sh"

note "Running Swift launcher tests from the locked package graph"
swift test \
  --package-path "$LAUNCHER_DIR" \
  --disable-automatic-resolution

[[ -z "$(git -C "$ROOT_DIR" status --porcelain --untracked-files=normal)" ]] \
  || die "release verification modified the clean source worktree"
[[ "$(git -C "$ROOT_DIR" rev-parse HEAD)" == "$COMMIT" ]] \
  || die "HEAD changed while release verification was running"

rm -rf "$RELEASE_DIR"
mkdir -p "$RELEASE_DIR"
cp "$RELEASE_NOTES_SOURCE" "$RELEASE_NOTES"

note "Creating exact corresponding source"
VERSION="$VERSION" \
  SOURCE_ARCHIVE_OUTPUT="$SOURCE_ARCHIVE" \
  "$SCRIPT_DIR/make-source-archive.sh"

note "Proving the exported corresponding source builds offline in release mode"
VERSION="$VERSION" \
  EXPECTED_COMMIT="$COMMIT" \
  EXPECTED_TREE="$TREE" \
  SOURCE_BUILD_TARGET_DIR="$SOURCE_BUILD_TARGET_DIR" \
  SOURCE_VERIFICATION_RECEIPT="$SOURCE_BUILD_RECEIPT" \
  "$SCRIPT_DIR/verify-source-archive.sh" "$SOURCE_ARCHIVE"
VERIFY_COMPLETED_AT="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

python3 - \
  "$VERIFICATION_RECEIPT" \
  "$SOURCE_BUILD_RECEIPT" \
  "$ROOT_DIR/tools/verify.sh" \
  "$ROOT_DIR/tools/public-readiness.sh" \
  "$ROOT_DIR/tools/lan-pairing-smoke.sh" \
  "$ROOT_DIR/tools/remote-access-smoke.sh" \
  "$COMMIT" \
  "$TREE" \
  "$VERSION" \
  "$VERIFY_STARTED_AT" \
  "$VERIFY_COMPLETED_AT" <<'PY'
import hashlib
import json
import pathlib
import sys

(output_arg, source_receipt_arg, verify_script_arg, readiness_script_arg,
 lan_script_arg, remote_script_arg, commit, tree, version, started_at,
 completed_at) = sys.argv[1:]

def sha256(path):
    digest = hashlib.sha256()
    with pathlib.Path(path).open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()

source_receipt = json.loads(pathlib.Path(source_receipt_arg).read_text())
if source_receipt.get("status") != "passed":
    raise SystemExit("exported-source release verification did not pass")
if source_receipt.get("git_commit") != commit or source_receipt.get("git_tree") != tree:
    raise SystemExit("exported-source verification receipt identifies a different snapshot")

receipt = {
    "checks": {
        "complete_verification": {
            "command": "./tools/verify.sh",
            "script_sha256": sha256(verify_script_arg),
            "status": "passed",
        },
        "lan_pairing_smoke": {
            "command": "./tools/lan-pairing-smoke.sh",
            "script_sha256": sha256(lan_script_arg),
            "status": "passed",
        },
        "public_readiness": {
            "command": "./tools/public-readiness.sh",
            "script_sha256": sha256(readiness_script_arg),
            "status": "passed",
        },
        "remote_access_smoke": {
            "command": "./tools/remote-access-smoke.sh",
            "script_sha256": sha256(remote_script_arg),
            "status": "passed",
        },
        "swift_launcher_tests": {
            "command": "swift test --package-path macos/FozmoLauncher --disable-automatic-resolution",
            "status": "passed",
        },
    },
    "completed_at": completed_at,
    "git_commit": commit,
    "git_tree": tree,
    "source_build_receipt": pathlib.Path(source_receipt_arg).name,
    "source_build_receipt_sha256": sha256(source_receipt_arg),
    "started_at": started_at,
    "status": "passed",
    "version": version,
}
pathlib.Path(output_arg).write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
PY

SOURCE_BIN_DIR="$SOURCE_BUILD_TARGET_DIR/aarch64-apple-darwin/release"
note "Building the intentionally unsigned public app"
CORE_BIN="$SOURCE_BIN_DIR/fozmo" \
  CLI_BIN="$SOURCE_BIN_DIR/fozmoctl" \
  AIRPLAY_HELPER_BIN="$SOURCE_BIN_DIR/fozmo-airplay-helper" \
  CORE_BINARY_PROVENANCE="verified corresponding-source archive offline release build" \
  VERSION="$VERSION" \
  "$SCRIPT_DIR/build-app.sh"
VERSION="$VERSION" "$SCRIPT_DIR/verify-app.sh" "$BUILD_DIR/Fozmo.app"

note "Creating and mounting the intentionally unsigned public DMG"
APP_PATH="$BUILD_DIR/Fozmo.app" \
  VERSION="$VERSION" \
  OUTPUT_DMG="$DMG" \
  "$SCRIPT_DIR/make-dmg.sh"
VERSION="$VERSION" "$SCRIPT_DIR/verify-dmg.sh" "$DMG"
codesign --verify --deep --strict --verbose=2 "$BUILD_DIR/Fozmo.app"

BUILD_TIMESTAMP="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
python3 - \
  "$BUILD_DIR/Fozmo.app/Contents/Resources/build-manifest.json" \
  "$MANIFEST" \
  "$DMG" \
  "$SOURCE_ARCHIVE" \
  "$RELEASE_NOTES" \
  "$VERIFICATION_RECEIPT" \
  "$SOURCE_BUILD_RECEIPT" \
  "$VERSION" \
  "$COMMIT" \
  "$TREE" \
  "$BUILD_TIMESTAMP" <<'PY'
import hashlib
import json
import pathlib
import sys

(base_arg, output_arg, dmg_arg, source_arg, notes_arg, verification_arg,
 source_verification_arg, version, commit, tree, timestamp) = sys.argv[1:]

def sha256(path):
    digest = hashlib.sha256()
    with pathlib.Path(path).open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()

manifest = json.loads(pathlib.Path(base_arg).read_text())
verification = json.loads(pathlib.Path(verification_arg).read_text())
source_verification = json.loads(pathlib.Path(source_verification_arg).read_text())
if manifest.get("build_mode") != "unsigned-public" or manifest.get("version") != version:
    raise SystemExit("packaged build manifest is not for this unsigned release")
if manifest.get("git_commit") != commit or manifest.get("git_tree") != tree:
    raise SystemExit("packaged build manifest identifies a different source snapshot")
if verification.get("status") != "passed" or verification.get("git_commit") != commit:
    raise SystemExit("complete release verification receipt is invalid")
if source_verification.get("status") != "passed":
    raise SystemExit("source build verification receipt is invalid")
for binary, expected in source_verification.get("binaries_sha256", {}).items():
    packaged_input = manifest.get("input_binaries", {}).get(binary, {})
    if packaged_input.get("sha256") != expected:
        raise SystemExit(f"packaged {binary} was not built from the verified source archive")

artifacts = [dmg_arg, source_arg, notes_arg, verification_arg, source_verification_arg]
manifest.update({
    "apple_developer_id_signed": False,
    "artifact_sha256": {
        pathlib.Path(path).name: sha256(path)
        for path in artifacts
    },
    "build_timestamp": timestamp,
    "checksums": {"aggregate": "SHA256SUMS", "algorithm": "SHA-256"},
    "distribution": "public pre-alpha",
    "notarized": False,
    "release_notes": pathlib.Path(notes_arg).name,
    "signature": "ad-hoc",
    "source_snapshot": {
        "corresponding_source": pathlib.Path(source_arg).name,
        "export_method": "git archive plus locked vendored dependencies and pinned upstream sources",
        "git_commit": commit,
        "git_tree": tree,
    },
    "update_checks_enabled": False,
    "verification": {
        "release_receipt": pathlib.Path(verification_arg).name,
        "release_receipt_sha256": sha256(verification_arg),
        "source_build_receipt": pathlib.Path(source_verification_arg).name,
        "source_build_receipt_sha256": sha256(source_verification_arg),
        "status": "passed",
    },
})
pathlib.Path(output_arg).write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
PY

(cd "$RELEASE_DIR" && shasum -a 256 "$(basename "$DMG")") >"$DMG.sha256"
(cd "$RELEASE_DIR" && shasum -a 256 "$(basename "$SOURCE_ARCHIVE")") \
  >"$SOURCE_ARCHIVE.sha256"
(cd "$RELEASE_DIR" && shasum -a 256 \
  "$(basename "$DMG")" \
  "$(basename "$SOURCE_ARCHIVE")" \
  "$(basename "$RELEASE_NOTES")" \
  "$(basename "$SOURCE_BUILD_RECEIPT")" \
  "$(basename "$VERIFICATION_RECEIPT")" \
  "$(basename "$MANIFEST")") >"$CHECKSUMS"

(cd "$RELEASE_DIR" && shasum -a 256 -c "$(basename "$DMG.sha256")")
(cd "$RELEASE_DIR" && shasum -a 256 -c "$(basename "$SOURCE_ARCHIVE.sha256")")
(cd "$RELEASE_DIR" && shasum -a 256 -c "$(basename "$CHECKSUMS")")

note "Unsigned public release artifacts are ready in $RELEASE_DIR"
note "Complete verification, exported-source release builds, provenance, and checksums passed"
note "This build is ad-hoc signed, not Developer ID signed, and not notarized"
