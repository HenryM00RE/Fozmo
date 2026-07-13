#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "$0")" && pwd)/common.sh"

DMG="${1:-}"
[[ -f "$DMG" ]] || die "usage: verify-dmg.sh <Fozmo.dmg>"
MODE="${FOZMO_BUILD_MODE:-dev}"
[[ "$MODE" == "dev" || "$MODE" == "public" || "$MODE" == "unsigned-public" ]] \
  || die "FOZMO_BUILD_MODE must be dev, public, or unsigned-public"
MOUNT_POINT="$(mktemp -d /tmp/fozmo-dmg.XXXXXX)"

cleanup() {
  hdiutil detach "$MOUNT_POINT" -quiet >/dev/null 2>&1 || true
  rmdir "$MOUNT_POINT" >/dev/null 2>&1 || true
}
trap cleanup EXIT

hdiutil attach "$DMG" -readonly -nobrowse -mountpoint "$MOUNT_POINT" -quiet
[[ -d "$MOUNT_POINT/Fozmo.app" ]] || die "DMG does not contain Fozmo.app"
[[ -L "$MOUNT_POINT/Applications" ]] || die "DMG does not contain an Applications link"
"$SCRIPT_DIR/verify-app.sh" "$MOUNT_POINT/Fozmo.app"

if is_public_build_mode "$MODE"; then
  [[ ! -f "$MOUNT_POINT/DEVELOPMENT BUILD.txt" ]] || die "public DMG is marked as development"
fi
if [[ "$MODE" == "public" ]]; then
  xcrun stapler validate "$DMG"
  spctl --assess --type open --context context:primary-signature --verbose=2 "$DMG"
elif [[ "$MODE" == "unsigned-public" ]]; then
  note "Skipping Gatekeeper, notarization, and stapling checks for the intentionally unsigned release"
fi

note "DMG mount and content checks passed"
