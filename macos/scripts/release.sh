#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "$0")" && pwd)/common.sh"

require_command git
require_command xcrun
require_command codesign
require_command shasum

export FOZMO_BUILD_MODE=public
VERSION="${VERSION:-$(project_version)}"
TAG="v$VERSION"
RELEASE_DIR="$BUILD_DIR/release-$VERSION"
APPCAST_INPUT="$RELEASE_DIR/appcast-input"
DMG="$RELEASE_DIR/Fozmo-$VERSION-macos-arm64.dmg"

require_value DEVELOPER_ID_APPLICATION
require_value SPARKLE_FEED_URL
require_value SPARKLE_PUBLIC_ED_KEY
require_value SPARKLE_PRIVATE_KEY_FILE
require_value SPARKLE_DOWNLOAD_URL_PREFIX
require_value RELEASE_NOTES_FILE
SOURCE_ARCHIVE_SHA256="${SOURCE_ARCHIVE_SHA256:-${EXPECTED_SOURCE_SHA256:-}}"
[[ -n "$SOURCE_ARCHIVE_SHA256" ]] || die "SOURCE_ARCHIVE_SHA256 (or EXPECTED_SOURCE_SHA256) is required"
SOURCE_ARCHIVE="${SOURCE_ARCHIVE:-$BUILD_DIR/Fozmo-$VERSION-source.tar.zst}"
if [[ ! -f "$SOURCE_ARCHIVE" ]]; then
  VERSION="$VERSION" SOURCE_ARCHIVE_OUTPUT="$SOURCE_ARCHIVE" "$SCRIPT_DIR/make-source-archive.sh"
fi
[[ "${SOURCE_ARCHIVE_AUDITED:-}" == "1" ]] || die "set SOURCE_ARCHIVE_AUDITED=1 after reviewing corresponding source contents"
verify_gpl_aggregation_policy "$FOZMO_BUILD_MODE" "$VERSION"
[[ "$SPARKLE_DOWNLOAD_URL_PREFIX" == https://* ]] || die "SPARKLE_DOWNLOAD_URL_PREFIX must use HTTPS"
[[ -f "$SPARKLE_PRIVATE_KEY_FILE" ]] || die "Sparkle private key file is missing"
[[ -s "$RELEASE_NOTES_FILE" ]] || die "RELEASE_NOTES_FILE is missing or empty"
[[ -f "$SOURCE_ARCHIVE" ]] || die "corresponding source archive is missing"
[[ "$SOURCE_ARCHIVE" == *.tar.zst ]] || die "SOURCE_ARCHIVE must be a .tar.zst archive"
ACTUAL_SOURCE_SHA256="$(shasum -a 256 "$SOURCE_ARCHIVE" | awk '{print $1}')"
[[ "$ACTUAL_SOURCE_SHA256" == "$SOURCE_ARCHIVE_SHA256" ]] \
  || die "corresponding source archive checksum does not match SOURCE_ARCHIVE_SHA256"

if [[ -n "$(git -C "$ROOT_DIR" status --porcelain --untracked-files=normal)" ]]; then
  die "public releases require a clean worktree"
fi
[[ "$(git -C "$ROOT_DIR" describe --tags --exact-match HEAD 2>/dev/null || true)" == "$TAG" ]] \
  || die "HEAD must be tagged $TAG"

if [[ -z "${NOTARY_KEYCHAIN_PROFILE:-}" ]]; then
  require_value APPLE_NOTARY_KEY_PATH
  require_value APPLE_NOTARY_KEY_ID
  require_value APPLE_NOTARY_ISSUER_ID
  [[ -f "$APPLE_NOTARY_KEY_PATH" ]] || die "Apple notarization key file is missing"
fi

rm -rf "$RELEASE_DIR"
mkdir -p "$APPCAST_INPUT"

VERSION="$VERSION" "$SCRIPT_DIR/build-app.sh"
APP_PATH="$BUILD_DIR/Fozmo.app" VERSION="$VERSION" OUTPUT_DMG="$DMG" "$SCRIPT_DIR/make-dmg.sh"

codesign --force --timestamp --sign "$DEVELOPER_ID_APPLICATION" "$DMG"
codesign --verify --verbose=2 "$DMG"

NOTARY_OUTPUT="$RELEASE_DIR/notarization.json"
if [[ -n "${NOTARY_KEYCHAIN_PROFILE:-}" ]]; then
  xcrun notarytool submit "$DMG" \
    --keychain-profile "$NOTARY_KEYCHAIN_PROFILE" \
    --wait \
    --output-format json | tee "$NOTARY_OUTPUT"
else
  xcrun notarytool submit "$DMG" \
    --key "$APPLE_NOTARY_KEY_PATH" \
    --key-id "$APPLE_NOTARY_KEY_ID" \
    --issuer "$APPLE_NOTARY_ISSUER_ID" \
    --wait \
    --output-format json | tee "$NOTARY_OUTPUT"
fi
[[ "$(plutil -extract status raw "$NOTARY_OUTPUT")" == "Accepted" ]] || die "Apple did not accept the DMG"

xcrun stapler staple "$DMG"
xcrun stapler validate "$DMG"
FOZMO_BUILD_MODE=public "$SCRIPT_DIR/verify-dmg.sh" "$DMG"

cp "$DMG" "$APPCAST_INPUT/"
cp "$RELEASE_NOTES_FILE" "$APPCAST_INPUT/Fozmo-$VERSION-macos-arm64.md"

GENERATE_APPCAST="$LAUNCHER_DIR/.build/artifacts/sparkle/Sparkle/bin/generate_appcast"
SIGN_UPDATE="$LAUNCHER_DIR/.build/artifacts/sparkle/Sparkle/bin/sign_update"
[[ -x "$GENERATE_APPCAST" && -x "$SIGN_UPDATE" ]] || die "Sparkle release tools are missing"
"$GENERATE_APPCAST" \
  --ed-key-file "$SPARKLE_PRIVATE_KEY_FILE" \
  --download-url-prefix "$SPARKLE_DOWNLOAD_URL_PREFIX" \
  --versions "$(bundle_build_number "$VERSION")" \
  -o appcast.xml \
  "$APPCAST_INPUT"
"$SIGN_UPDATE" --verify --ed-key-file "$SPARKLE_PRIVATE_KEY_FILE" "$APPCAST_INPUT/appcast.xml"

cp "$APPCAST_INPUT/Fozmo-$VERSION-macos-arm64.md" \
  "$RELEASE_DIR/Fozmo-$VERSION-macos-arm64.md"
cp "$SOURCE_ARCHIVE" "$RELEASE_DIR/Fozmo-$VERSION-source.tar.zst"
(cd "$RELEASE_DIR" && shasum -a 256 "$(basename "$DMG")") >"$DMG.sha256"
(cd "$RELEASE_DIR" && shasum -a 256 "Fozmo-$VERSION-source.tar.zst") \
  >"$RELEASE_DIR/Fozmo-$VERSION-source.tar.zst.sha256"
cp "$APPCAST_INPUT/appcast.xml" "$RELEASE_DIR/appcast.xml"

python3 - \
  "$BUILD_DIR/Fozmo.app/Contents/Resources/build-manifest.json" \
  "$RELEASE_DIR/build-manifest.json" \
  "$DMG" \
  "$RELEASE_DIR/Fozmo-$VERSION-source.tar.zst" \
  "$RELEASE_DIR/appcast.xml" \
  "$NOTARY_OUTPUT" \
  "$(git -C "$ROOT_DIR" rev-parse HEAD)" \
  "$TAG" <<'PY'
import hashlib
import json
import pathlib
import sys

base_arg, output_arg, dmg_arg, source_arg, appcast_arg, notary_arg, commit, tag = sys.argv[1:]

def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()

manifest = json.loads(pathlib.Path(base_arg).read_text())
manifest.update({
    "artifact_sha256": {
        pathlib.Path(dmg_arg).name: sha256(dmg_arg),
        pathlib.Path(source_arg).name: sha256(source_arg),
        pathlib.Path(appcast_arg).name: sha256(appcast_arg),
        pathlib.Path(notary_arg).name: sha256(notary_arg),
    },
    "bundle_version": int(manifest["version"].split(".")[0]) * 1_000_000
        + int(manifest["version"].split(".")[1]) * 1_000
        + int(manifest["version"].split(".")[2]),
    "git_commit": commit,
    "notarization_status": json.loads(pathlib.Path(notary_arg).read_text()).get("status"),
    "publication_order": "upload artifacts and checksums before appcast.xml",
    "tag": tag,
})
pathlib.Path(output_arg).write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
PY

note "Release artifacts are ready in $RELEASE_DIR"
note "Upload the DMG/source/checksums first; publish appcast.xml only after every asset URL is live"
