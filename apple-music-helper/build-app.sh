#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CONFIGURATION="${FOZMO_APPLE_MUSIC_BUILD_CONFIGURATION:-release}"
OUTPUT_ROOT="${FOZMO_APPLE_MUSIC_OUTPUT_DIR:-$ROOT_DIR/target/apple-music-helper}"
PUBLISHED_APP_PATH="$OUTPUT_ROOT/FozmoAppleMusicHelper.app"
STAGING_ROOT=""
APP_PATH=""
CONTENTS=""
MACOS=""
PROFILE_PLIST=""
SIGNING_ENTITLEMENTS=""
SIGNER_CERT_DIR=""
PREVIOUS_APP_PATH=""
PUBLISHED_APP_DISPLACED=false
PUBLISH_COMPLETE=false

cleanup() {
  if [[
    "$PUBLISHED_APP_DISPLACED" == "true"
    && "$PUBLISH_COMPLETE" != "true"
    && ! -e "$PUBLISHED_APP_PATH"
    && -e "$PREVIOUS_APP_PATH"
  ]]; then
    mv "$PREVIOUS_APP_PATH" "$PUBLISHED_APP_PATH" || true
  fi
  [[ -z "$PROFILE_PLIST" ]] || rm -f "$PROFILE_PLIST"
  [[ -z "$SIGNING_ENTITLEMENTS" ]] || rm -f "$SIGNING_ENTITLEMENTS"
  [[ -z "$SIGNER_CERT_DIR" ]] || rm -rf "$SIGNER_CERT_DIR"
  [[ -z "$STAGING_ROOT" ]] || rm -rf "$STAGING_ROOT"
}
trap cleanup EXIT

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

mkdir -p "$OUTPUT_ROOT"
STAGING_ROOT="$(mktemp -d "$OUTPUT_ROOT/.FozmoAppleMusicHelper.stage.XXXXXX")"
APP_PATH="$STAGING_ROOT/FozmoAppleMusicHelper.app"
CONTENTS="$APP_PATH/Contents"
MACOS="$CONTENTS/MacOS"
mkdir -p "$MACOS"
cp "$HELPER_BIN" "$MACOS/FozmoAppleMusicHelper"
cp "$SCRIPT_DIR/Resources/Info.plist" "$CONTENTS/Info.plist"

SIGN_IDENTITY="${FOZMO_APPLE_MUSIC_SIGN_IDENTITY:--}"
if [[ "$SIGN_IDENTITY" != "-" && -z "${FOZMO_APPLE_MUSIC_PROVISIONING_PROFILE:-}" ]]; then
  echo "error: a Mac App Development provisioning profile is required with a non-ad-hoc signing identity" >&2
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
  SIGNING_ENTITLEMENTS="$(mktemp "${TMPDIR:-/tmp}/fozmo-apple-entitlements.XXXXXX")"
  security cms -D -i "$FOZMO_APPLE_MUSIC_PROVISIONING_PROFILE" -o "$PROFILE_PLIST"

  BUNDLE_ID="$(
    /usr/libexec/PlistBuddy -c "Print :CFBundleIdentifier" "$CONTENTS/Info.plist"
  )"
  APP_IDENTIFIER="$(
    /usr/libexec/PlistBuddy \
      -c "Print :Entitlements:application-identifier" \
      "$PROFILE_PLIST" 2>/dev/null ||
      /usr/libexec/PlistBuddy \
        -c "Print :Entitlements:com.apple.application-identifier" \
        "$PROFILE_PLIST" 2>/dev/null ||
      true
  )"
  TEAM_IDENTIFIER="$(
    /usr/libexec/PlistBuddy \
      -c "Print :Entitlements:com.apple.developer.team-identifier" \
      "$PROFILE_PLIST" 2>/dev/null ||
      true
  )"
  [[ -n "$TEAM_IDENTIFIER" && "$APP_IDENTIFIER" == "$TEAM_IDENTIFIER.$BUNDLE_ID" ]] || {
    echo "error: provisioning profile App ID does not exactly match $BUNDLE_ID" >&2
    exit 1
  }

  PROFILE_PLATFORM="$(
    /usr/libexec/PlistBuddy -c "Print :Platform:0" "$PROFILE_PLIST" 2>/dev/null ||
      true
  )"
  [[ "$PROFILE_PLATFORM" == "OSX" ]] || {
    echo "error: provisioning profile is not for macOS" >&2
    exit 1
  }

  PROVISIONING_UDID="$(
    system_profiler SPHardwareDataType -json |
      plutil -extract SPHardwareDataType.0.provisioning_UDID raw -o - - 2>/dev/null ||
      true
  )"
  [[ -n "$PROVISIONING_UDID" ]] || {
    echo "error: could not read this Mac's Provisioning UDID" >&2
    exit 1
  }
  DEVICE_ALLOWED=false
  DEVICE_INDEX=0
  while DEVICE_UDID="$(
    /usr/libexec/PlistBuddy \
      -c "Print :ProvisionedDevices:$DEVICE_INDEX" \
      "$PROFILE_PLIST" 2>/dev/null
  )"; do
    if [[ "$DEVICE_UDID" == "$PROVISIONING_UDID" ]]; then
      DEVICE_ALLOWED=true
      break
    fi
    DEVICE_INDEX=$((DEVICE_INDEX + 1))
  done
  [[ "$DEVICE_ALLOWED" == "true" ]] || {
    echo "error: provisioning profile does not allow this Mac ($PROVISIONING_UDID)" >&2
    exit 1
  }

  # MusicKit is an App Service tied to the App ID on Apple's servers. It has
  # no com.apple.developer.musickit entitlement. Sign with only the
  # entitlements Apple actually issued in this profile.
  plutil -extract Entitlements xml1 -o "$SIGNING_ENTITLEMENTS" "$PROFILE_PLIST"
  cp "$FOZMO_APPLE_MUSIC_PROVISIONING_PROFILE" "$CONTENTS/embedded.provisionprofile"
  /usr/libexec/PlistBuddy -c "Set :FozmoMusicKitProvisioned true" "$CONTENTS/Info.plist"
fi

if [[ "$SIGN_IDENTITY" == "-" ]]; then
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
    --entitlements "$SIGNING_ENTITLEMENTS" \
    "$APP_PATH"
fi
codesign --verify --strict --verbose=2 "$APP_PATH"
if [[ "$SIGN_IDENTITY" != "-" ]]; then
  SIGNED_APP_IDENTIFIER="$(
    codesign -d --entitlements - --xml "$APP_PATH" 2>/dev/null |
      plutil \
        -extract 'com\.apple\.application-identifier' \
        raw \
        -o - \
        - 2>/dev/null ||
      true
  )"
  [[ "$SIGNED_APP_IDENTIFIER" == "$APP_IDENTIFIER" ]] || {
    echo "error: signed helper is missing the profile's application identifier" >&2
    exit 1
  }

  SIGNER_CERT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/fozmo-apple-signer.XXXXXX")"
  (
    cd "$SIGNER_CERT_DIR"
    codesign -d --extract-certificates "$APP_PATH" >/dev/null 2>&1
  )
  SIGNER_SHA1="$(
    openssl x509 \
      -inform DER \
      -in "$SIGNER_CERT_DIR/codesign0" \
      -noout \
      -fingerprint \
      -sha1 |
      sed 's/^.*=//; s/://g' |
      tr '[:lower:]' '[:upper:]'
  )"
  PROFILE_CERT_MATCH=false
  PROFILE_CERT_INDEX=0
  while PROFILE_CERT_BASE64="$(
    plutil \
      -extract "DeveloperCertificates.$PROFILE_CERT_INDEX" \
      raw \
      -o - \
      "$PROFILE_PLIST" 2>/dev/null
  )"; do
    PROFILE_CERT_SHA1="$(
      printf '%s' "$PROFILE_CERT_BASE64" |
        base64 -D |
        openssl x509 -inform DER -noout -fingerprint -sha1 |
        sed 's/^.*=//; s/://g' |
        tr '[:lower:]' '[:upper:]'
    )"
    if [[ "$SIGNER_SHA1" == "$PROFILE_CERT_SHA1" ]]; then
      PROFILE_CERT_MATCH=true
      break
    fi
    PROFILE_CERT_INDEX=$((PROFILE_CERT_INDEX + 1))
  done
  [[ "$PROFILE_CERT_MATCH" == "true" ]] || {
    echo "error: signing identity is not included in the provisioning profile" >&2
    exit 1
  }
fi

if [[ "$SIGN_IDENTITY" == "-" ]]; then
  echo "warning: built ad hoc; launch/IPC can be tested, but MusicKit authorization and playback require a provisioned build" >&2
fi

# Never overwrite the executable inside a running signed app bundle. macOS
# validates lazily paged executable data, so mutating that file in place can
# terminate the helper later with CODESIGNING / Invalid Page. Publish a
# completely signed staged bundle by renaming directories instead, with EXIT
# rollback while the old bundle is displaced.
PREVIOUS_APP_PATH="$OUTPUT_ROOT/.FozmoAppleMusicHelper.previous.$$"
HAD_PREVIOUS_APP=false
if [[ -e "$PUBLISHED_APP_PATH" ]]; then
  mv "$PUBLISHED_APP_PATH" "$PREVIOUS_APP_PATH"
  HAD_PREVIOUS_APP=true
  PUBLISHED_APP_DISPLACED=true
fi
if ! mv "$APP_PATH" "$PUBLISHED_APP_PATH"; then
  if [[ "$HAD_PREVIOUS_APP" == "true" && ! -e "$PUBLISHED_APP_PATH" ]]; then
    mv "$PREVIOUS_APP_PATH" "$PUBLISHED_APP_PATH"
  fi
  echo "error: could not publish the signed Apple Music helper" >&2
  exit 1
fi
PUBLISH_COMPLETE=true
if [[ "$HAD_PREVIOUS_APP" == "true" ]]; then
  rm -rf "$PREVIOUS_APP_PATH"
fi

echo "$PUBLISHED_APP_PATH"
