#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CONFIGURATION="${FOZMO_APPLE_MUSIC_BUILD_CONFIGURATION:-release}"
OUTPUT_ROOT="${FOZMO_APPLE_MUSIC_OUTPUT_DIR:-$ROOT_DIR/target/apple-music-helper}"
APP_PATH="$OUTPUT_ROOT/FozmoAppleMusicHelper.app"
CONTENTS="$APP_PATH/Contents"
MACOS="$CONTENTS/MacOS"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: the Apple Music helper can only be built on macOS" >&2
  exit 1
fi

swift build \
  --package-path "$SCRIPT_DIR" \
  --configuration "$CONFIGURATION" \
  --arch arm64

BIN_DIR="$(swift build \
  --package-path "$SCRIPT_DIR" \
  --configuration "$CONFIGURATION" \
  --arch arm64 \
  --show-bin-path)"
HELPER_BIN="$BIN_DIR/FozmoAppleMusicHelper"
[[ -x "$HELPER_BIN" ]] || {
  echo "error: helper executable was not produced" >&2
  exit 1
}

mkdir -p "$MACOS"
rm -f "$CONTENTS/embedded.provisionprofile"
cp "$HELPER_BIN" "$MACOS/FozmoAppleMusicHelper"
cp "$SCRIPT_DIR/Resources/Info.plist" "$CONTENTS/Info.plist"

SIGN_IDENTITY="${FOZMO_APPLE_MUSIC_SIGN_IDENTITY:--}"
if [[ "$SIGN_IDENTITY" != "-" && -z "${FOZMO_APPLE_MUSIC_PROVISIONING_PROFILE:-}" ]]; then
  echo "error: a MusicKit-enabled provisioning profile is required with a non-ad-hoc signing identity" >&2
  exit 1
fi
if [[ "$SIGN_IDENTITY" == "-" && -n "${FOZMO_APPLE_MUSIC_PROVISIONING_PROFILE:-}" ]]; then
  echo "error: a provisioning profile must be paired with a non-ad-hoc signing identity" >&2
  exit 1
fi

if [[ -n "${FOZMO_APPLE_MUSIC_PROVISIONING_PROFILE:-}" ]]; then
  [[ -f "$FOZMO_APPLE_MUSIC_PROVISIONING_PROFILE" ]] || {
    echo "error: FOZMO_APPLE_MUSIC_PROVISIONING_PROFILE does not name a file" >&2
    exit 1
  }
  PROFILE_PLIST="$(mktemp "${TMPDIR:-/tmp}/fozmo-apple-profile.XXXXXX")"
  trap 'rm -f "$PROFILE_PLIST"' EXIT
  security cms -D -i "$FOZMO_APPLE_MUSIC_PROVISIONING_PROFILE" -o "$PROFILE_PLIST"
  MUSIC_KIT_VALUE="$(
    /usr/libexec/PlistBuddy \
      -c "Print :Entitlements:com.apple.developer.musickit" \
      "$PROFILE_PLIST" 2>/dev/null || true
  )"
  [[ "$MUSIC_KIT_VALUE" == "true" ]] || {
    echo "error: provisioning profile does not contain com.apple.developer.musickit=true" >&2
    exit 1
  }
  APP_IDENTIFIER="$(
    /usr/libexec/PlistBuddy \
      -c "Print :Entitlements:application-identifier" \
      "$PROFILE_PLIST" 2>/dev/null ||
      /usr/libexec/PlistBuddy \
        -c "Print :Entitlements:com.apple.application-identifier" \
        "$PROFILE_PLIST" 2>/dev/null ||
      true
  )"
  [[ "$APP_IDENTIFIER" == *".$(
    /usr/libexec/PlistBuddy -c "Print :CFBundleIdentifier" "$CONTENTS/Info.plist"
  )" ]] || {
    echo "error: provisioning profile App ID does not match com.fozmo.apple-music-helper" >&2
    exit 1
  }
  cp "$FOZMO_APPLE_MUSIC_PROVISIONING_PROFILE" "$CONTENTS/embedded.provisionprofile"
  /usr/libexec/PlistBuddy -c "Set :FozmoMusicKitEntitled true" "$CONTENTS/Info.plist"
fi

if [[ "$SIGN_IDENTITY" == "-" ]]; then
  # MusicKit is a restricted entitlement. Including it in an ad-hoc signature
  # makes AMFI reject the executable before main(), which prevents even the
  # launch/IPC proof from running.
  codesign \
    --force \
    --options runtime \
    --timestamp=none \
    --sign - \
    "$APP_PATH"
else
  codesign \
    --force \
    --options runtime \
    --timestamp=none \
    --sign "$SIGN_IDENTITY" \
    --entitlements "$SCRIPT_DIR/Resources/FozmoAppleMusicHelper.entitlements" \
    "$APP_PATH"
fi
codesign --verify --strict --verbose=2 "$APP_PATH"
if [[ "$SIGN_IDENTITY" != "-" ]]; then
  SIGNED_ENTITLEMENTS="$(mktemp "${TMPDIR:-/tmp}/fozmo-apple-entitlements.XXXXXX")"
  trap 'rm -f "$PROFILE_PLIST" "$SIGNED_ENTITLEMENTS"' EXIT
  codesign -d --entitlements :- "$APP_PATH" >"$SIGNED_ENTITLEMENTS" 2>/dev/null
  [[ "$(
    /usr/libexec/PlistBuddy \
      -c "Print :com.apple.developer.musickit" \
      "$SIGNED_ENTITLEMENTS" 2>/dev/null || true
  )" == "true" ]] || {
    echo "error: signed helper is missing the MusicKit entitlement" >&2
    exit 1
  }
fi

if [[ "$SIGN_IDENTITY" == "-" ]]; then
  echo "warning: built without the restricted MusicKit entitlement; launch/IPC can be tested, but authorization and playback require a provisioned build" >&2
fi

echo "$APP_PATH"
