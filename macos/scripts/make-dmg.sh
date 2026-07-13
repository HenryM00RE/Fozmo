#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "$0")" && pwd)/common.sh"

require_command hdiutil
require_command ditto

MODE="${FOZMO_BUILD_MODE:-dev}"
[[ "$MODE" == "dev" || "$MODE" == "public" || "$MODE" == "unsigned-public" ]] \
  || die "FOZMO_BUILD_MODE must be dev, public, or unsigned-public"
APP_PATH="${APP_PATH:-$BUILD_DIR/Fozmo.app}"
VERSION="${VERSION:-$(project_version)}"
SUFFIX=""
[[ "$MODE" != "dev" ]] || SUFFIX="-dev"
OUTPUT_DMG="${OUTPUT_DMG:-$BUILD_DIR/Fozmo-$VERSION-macos-arm64$SUFFIX.dmg}"
STAGING="$BUILD_DIR/dmg-staging"

[[ -d "$APP_PATH" ]] || die "app bundle is missing: $APP_PATH"
rm -rf "$STAGING" "$OUTPUT_DMG"
mkdir -p "$STAGING"
ditto "$APP_PATH" "$STAGING/Fozmo.app"
ln -s /Applications "$STAGING/Applications"
if [[ "$MODE" == "dev" ]]; then
  cat >"$STAGING/DEVELOPMENT BUILD.txt" <<'EOF'
This is an ad-hoc development build. It is not notarized and must not be
published or represented as a public Fozmo release.
EOF
fi

hdiutil create \
  -volname "Fozmo $VERSION" \
  -srcfolder "$STAGING" \
  -format UDZO \
  -imagekey zlib-level=9 \
  -ov \
  "$OUTPUT_DMG"
rm -rf "$STAGING"

note "$MODE DMG created at $OUTPUT_DMG"
