#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "$0")" && pwd)/common.sh"

MODE="${FOZMO_BUILD_MODE:-dev}"
[[ "$MODE" == "dev" || "$MODE" == "public" || "$MODE" == "unsigned-public" ]] \
  || die "FOZMO_BUILD_MODE must be dev, public, or unsigned-public"
if is_public_build_mode "$MODE"; then
  [[ -z "${RUSTFLAGS:-}" ]] \
    || die "public release builds reject externally supplied RUSTFLAGS"
  [[ -z "${CARGO_ENCODED_RUSTFLAGS:-}" ]] \
    || die "public release builds reject externally supplied CARGO_ENCODED_RUSTFLAGS"
fi

require_command cargo
require_command git
require_command rustc
require_command npm
require_command node
require_command python3
require_command swift
require_command xcrun
require_command xcodebuild
require_command codesign
require_command plutil
require_command ditto
require_command install_name_tool
require_command otool
require_command dsymutil

VERSION="${VERSION:-$(project_version)}"
BUILD_NUMBER="$(bundle_build_number "$VERSION")"
APP_PATH="$BUILD_DIR/Fozmo.app"
CONTENTS="$APP_PATH/Contents"
MACOS="$CONTENTS/MacOS"
HELPERS="$CONTENTS/Helpers"
RESOURCES="$CONTENTS/Resources"
FRAMEWORKS="$CONTENTS/Frameworks"
DSYMS="$BUILD_DIR/dSYMs"

if ! is_public_build_mode "$MODE"; then
  verify_macos_build_toolchain
fi

if is_public_build_mode "$MODE"; then
  [[ "${SKIP_CORE_BUILD:-0}" != "1" ]] \
    || die "public release builds must compile the MIT server with release-owned Rust flags"
  [[ "${SKIP_AIRPLAY_HELPER_BUILD:-0}" != "1" ]] \
    || die "public release builds must compile the AirPlay helper with release-owned Rust flags"
  verify_public_toolchain
  verify_gpl_aggregation_policy "$MODE" "$VERSION"
  if [[ "$MODE" == "public" ]]; then
    require_value DEVELOPER_ID_APPLICATION
    require_value SPARKLE_FEED_URL
    require_value SPARKLE_PUBLIC_ED_KEY
    [[ "$SPARKLE_FEED_URL" == https://* ]] || die "SPARKLE_FEED_URL must use HTTPS"
    [[ "$SPARKLE_PUBLIC_ED_KEY" =~ ^[A-Za-z0-9+/]{43}=$ ]] || die "SPARKLE_PUBLIC_ED_KEY is not a 32-byte base64 Ed25519 public key"
    require_value APP_ICON_ICNS
    [[ -f "$APP_ICON_ICNS" ]] || die "APP_ICON_ICNS does not exist"
  fi
fi

mkdir -p "$BUILD_DIR"

RUST_PATH_REMAP_STATUS=true
if [[ "$MODE" == "dev" && ( "${SKIP_CORE_BUILD:-0}" == "1" || "${SKIP_AIRPLAY_HELPER_BUILD:-0}" == "1" ) ]]; then
  RUST_PATH_REMAP_STATUS=unknown
fi

# Runtime diagnostics in both first- and third-party Rust crates include source
# locations. Map every host-controlled source/temp prefix for development DMGs
# as well as public releases so all packaged artifacts obey the same leak scan.
RUST_REMAP_FLAGS=()
add_rust_remap() {
  local source="${1%/}" destination="$2" existing
  [[ -n "$source" && "$source" == /* ]] || return 0
  [[ "$source" != *"="* ]] || die "cannot remap a build path containing '='"
  for existing in "${RUST_REMAP_FLAGS[@]:-}"; do
    [[ "$existing" != "--remap-path-prefix=$source="* ]] || return 0
  done
  RUST_REMAP_FLAGS+=("--remap-path-prefix=$source=$destination")
}
add_rust_remap "$ROOT_DIR" /usr/src/fozmo
add_rust_remap "${CARGO_HOME:-${HOME:-}/.cargo}" /usr/src/cargo
add_rust_remap "${GITHUB_WORKSPACE:-}" /usr/src/fozmo
add_rust_remap "${RUNNER_TEMP:-}" /usr/src/runner-temp
add_rust_remap "${TMPDIR:-}" /usr/src/build-temp
add_rust_remap "${HOME:-}" /usr/src/build-home
[[ "${#RUST_REMAP_FLAGS[@]}" -gt 0 ]] || die "could not establish Rust source-path remapping"

CARGO_PACKAGING_RUSTFLAGS=""
append_encoded_rustflag() {
  [[ -z "$CARGO_PACKAGING_RUSTFLAGS" ]] \
    || CARGO_PACKAGING_RUSTFLAGS+=$'\x1f'
  CARGO_PACKAGING_RUSTFLAGS+="$1"
}
if [[ "$MODE" == "dev" && -n "${CARGO_ENCODED_RUSTFLAGS:-}" ]]; then
  CARGO_PACKAGING_RUSTFLAGS="$CARGO_ENCODED_RUSTFLAGS"
elif [[ "$MODE" == "dev" && -n "${RUSTFLAGS:-}" ]]; then
  read -r -a DEVELOPMENT_RUSTFLAGS <<<"$RUSTFLAGS"
  for flag in "${DEVELOPMENT_RUSTFLAGS[@]}"; do
    append_encoded_rustflag "$flag"
  done
fi
for flag in "${RUST_REMAP_FLAGS[@]}"; do
  append_encoded_rustflag "$flag"
done

packaging_cargo_build() {
  env -u RUSTFLAGS \
    CARGO_ENCODED_RUSTFLAGS="$CARGO_PACKAGING_RUSTFLAGS" \
    CARGO_INCREMENTAL=0 \
    MACOSX_DEPLOYMENT_TARGET=13.0 \
    cargo build "$@"
}

if [[ "${SKIP_UI_BUILD:-0}" != "1" ]]; then
  note "Building web client"
  npm --prefix "$ROOT_DIR/ui" ci
  npm --prefix "$ROOT_DIR/ui" run build
  if is_public_build_mode "$MODE"; then
    git -C "$ROOT_DIR" diff --exit-code -- static/react-app \
      || die "frontend build differs from the committed snapshot"
  fi
fi
[[ -f "$ROOT_DIR/static/react-app/index.html" ]] || die "built web client is missing"

CORE_FEATURES="$(release_core_features)"
if [[ "${SKIP_CORE_BUILD:-0}" != "1" ]]; then
  note "Building MIT server without in-process AirPlay, Apple Music capture, or ASIO"
  packaging_cargo_build \
    --manifest-path "$ROOT_DIR/Cargo.toml" \
    --locked \
    --release \
    --target aarch64-apple-darwin \
    --no-default-features \
    --features "$CORE_FEATURES" \
    --bin fozmo \
    --bin fozmoctl
fi
CORE_BIN="${CORE_BIN:-$ROOT_DIR/target/aarch64-apple-darwin/release/fozmo}"
CLI_BIN="${CLI_BIN:-$ROOT_DIR/target/aarch64-apple-darwin/release/fozmoctl}"
if [[ ! -x "$CORE_BIN" && -x "$ROOT_DIR/target/release/fozmo" && "$MODE" == "dev" ]]; then
  CORE_BIN="$ROOT_DIR/target/release/fozmo"
fi
if [[ ! -x "$CLI_BIN" && -x "$ROOT_DIR/target/release/fozmoctl" && "$MODE" == "dev" ]]; then
  CLI_BIN="$ROOT_DIR/target/release/fozmoctl"
fi
[[ -x "$CORE_BIN" ]] || die "server binary is missing: $CORE_BIN"
[[ -x "$CLI_BIN" ]] || die "fozmoctl binary is missing: $CLI_BIN"

AIRPLAY_MANIFEST="$ROOT_DIR/airplay-helper/Cargo.toml"
if [[ -f "$AIRPLAY_MANIFEST" && "${SKIP_AIRPLAY_HELPER_BUILD:-0}" != "1" ]]; then
  note "Building standalone GPL AirPlay helper"
  packaging_cargo_build \
    --manifest-path "$AIRPLAY_MANIFEST" \
    --locked \
    --release \
    --target aarch64-apple-darwin
fi
AIRPLAY_HELPER_BIN="${AIRPLAY_HELPER_BIN:-$ROOT_DIR/airplay-helper/target/aarch64-apple-darwin/release/fozmo-airplay-helper}"
if [[ ! -x "$AIRPLAY_HELPER_BIN" && -x "$ROOT_DIR/airplay-helper/target/release/fozmo-airplay-helper" ]]; then
  AIRPLAY_HELPER_BIN="$ROOT_DIR/airplay-helper/target/release/fozmo-airplay-helper"
fi
if is_public_build_mode "$MODE"; then
  [[ -x "$AIRPLAY_HELPER_BIN" ]] || die "public release build requires the standalone AirPlay helper"
  REQUIRE_RELEASE_BINARIES=1 \
    SERVER_BIN="$CORE_BIN" \
    HELPER_BIN="$AIRPLAY_HELPER_BIN" \
    "$ROOT_DIR/tools/check-release-boundaries.sh"
fi

FFMPEG_STAGE="${FFMPEG_STAGE:-$BUILD_DIR/third-party/ffmpeg-stage}"
if [[ "${SKIP_FFMPEG_BUILD:-0}" != "1" ]]; then
  FOZMO_FFMPEG_STAGE="$FFMPEG_STAGE" "$SCRIPT_DIR/build-ffmpeg.sh"
elif is_public_build_mode "$MODE"; then
  [[ -n "${FFMPEG_STAGE:-}" ]] || die "public release build cannot skip FFmpeg without an audited FFMPEG_STAGE"
fi
if [[ -d "$FFMPEG_STAGE" ]]; then
  "$SCRIPT_DIR/audit-ffmpeg.sh" "$FFMPEG_STAGE"
elif is_public_build_mode "$MODE"; then
  die "public release build requires a source-built FFmpeg stage"
fi

note "Building Swift menu-bar launcher"
MACOSX_DEPLOYMENT_TARGET=13.0 swift build \
  --package-path "$LAUNCHER_DIR" \
  -c release \
  --arch arm64
SWIFT_BIN_DIR="$(MACOSX_DEPLOYMENT_TARGET=13.0 swift build --package-path "$LAUNCHER_DIR" -c release --arch arm64 --show-bin-path)"
LAUNCHER_BIN="$SWIFT_BIN_DIR/FozmoLauncher"
SPARKLE_FRAMEWORK="$LAUNCHER_DIR/.build/artifacts/sparkle/Sparkle/Sparkle.xcframework/macos-arm64_x86_64/Sparkle.framework"
[[ -x "$LAUNCHER_BIN" && -d "$SPARKLE_FRAMEWORK" ]] || die "launcher or Sparkle framework is missing"

rm -rf "$APP_PATH" "$DSYMS"
mkdir -p "$MACOS" "$HELPERS" "$RESOURCES/licenses" "$FRAMEWORKS" "$DSYMS"
cp "$LAUNCHER_DIR/Resources/Info.plist" "$CONTENTS/Info.plist"
plutil -replace CFBundleShortVersionString -string "$VERSION" "$CONTENTS/Info.plist"
plutil -replace CFBundleVersion -string "$BUILD_NUMBER" "$CONTENTS/Info.plist"

if [[ "$MODE" == "public" ]]; then
  plutil -replace FozmoUpdatesEnabled -bool YES "$CONTENTS/Info.plist"
  plutil -replace SUFeedURL -string "$SPARKLE_FEED_URL" "$CONTENTS/Info.plist"
  plutil -replace SUPublicEDKey -string "$SPARKLE_PUBLIC_ED_KEY" "$CONTENTS/Info.plist"
else
  plutil -replace FozmoUpdatesEnabled -bool NO "$CONTENTS/Info.plist"
  plutil -replace SUAllowsAutomaticUpdates -bool NO "$CONTENTS/Info.plist"
  plutil -replace SUAutomaticallyUpdate -bool NO "$CONTENTS/Info.plist"
  plutil -replace SUEnableAutomaticChecks -bool NO "$CONTENTS/Info.plist"
fi

cp "$LAUNCHER_BIN" "$MACOS/Fozmo"
cp "$CORE_BIN" "$HELPERS/fozmo-server"
cp "$CLI_BIN" "$HELPERS/fozmoctl"
[[ ! -x "$AIRPLAY_HELPER_BIN" ]] || cp "$AIRPLAY_HELPER_BIN" "$HELPERS/fozmo-airplay-helper"
if [[ -x "$FFMPEG_STAGE/bin/ffmpeg" ]]; then
  cp "$FFMPEG_STAGE/bin/ffmpeg" "$HELPERS/ffmpeg"
  ditto "$FFMPEG_STAGE/licenses" "$RESOURCES/licenses/ffmpeg"
  cp "$FFMPEG_STAGE/provenance.json" "$RESOURCES/ffmpeg-provenance.json"
fi
ditto "$SPARKLE_FRAMEWORK" "$FRAMEWORKS/Sparkle.framework"
ditto "$ROOT_DIR/static/react-app" "$RESOURCES/static/react-app"
cp "$ROOT_DIR/static/styles.css" "$RESOURCES/static/styles.css"
ditto "$ROOT_DIR/static/fonts" "$RESOURCES/static/fonts"
ditto "$ROOT_DIR/presets" "$RESOURCES/default-presets"
find "$RESOURCES/static/react-app" -type f -name '*.map' -delete

if [[ -f "$ROOT_DIR/LICENSE" ]]; then cp "$ROOT_DIR/LICENSE" "$RESOURCES/licenses/Fozmo-MIT.txt"; fi
if [[ -d "$ROOT_DIR/LICENSES" ]]; then ditto "$ROOT_DIR/LICENSES" "$RESOURCES/licenses/project"; fi
if [[ -f "$ROOT_DIR/airplay-helper/LICENSE" ]]; then cp "$ROOT_DIR/airplay-helper/LICENSE" "$RESOURCES/licenses/AirPlay-helper-GPL-2.0.txt"; fi
cp "$ROOT_DIR/docs/gpl-aggregation-assessment.md" "$RESOURCES/licenses/GPL-AGGREGATION-ASSESSMENT.md"
cp "$LAUNCHER_DIR/.build/artifacts/sparkle/Sparkle/LICENSE" "$RESOURCES/licenses/Sparkle-MIT.txt"
ditto "$MACOS_DIR/licenses" "$RESOURCES/licenses/macos-components"
cp "$MACOS_DIR/licenses/COMPONENTS.md" "$RESOURCES/licenses/COMPONENTS.md"
cp "$MACOS_DIR/licenses/THIRD-PARTY-SOURCES.md" "$RESOURCES/licenses/THIRD-PARTY-SOURCES.md"
if is_public_build_mode "$MODE" || [[ "${COLLECT_LICENSE_NOTICES:-0}" == "1" ]]; then
  "$SCRIPT_DIR/collect-license-notices.sh" "$BUILD_DIR/third-party-notices"
  ditto "$BUILD_DIR/third-party-notices" "$RESOURCES/licenses/third-party"
fi

python3 - \
  "$RESOURCES/build-manifest.json" \
  "$VERSION" \
  "$MODE" \
  "$CORE_FEATURES" \
  "$RUST_PATH_REMAP_STATUS" \
  "$ROOT_DIR" \
  "$FFMPEG_STAGE" \
  "$LAUNCHER_DIR/Package.resolved" \
  "$CORE_BIN" \
  "$CLI_BIN" \
  "$AIRPLAY_HELPER_BIN" \
  "${CORE_BINARY_PROVENANCE:-clean tracked checkout release build}" <<'PY'
import hashlib
import json
import pathlib
import re
import subprocess
import sys

(output_arg, version, mode, feature_string, rust_remap_arg, root_arg,
 ffmpeg_stage_arg, sparkle_resolved_arg, core_bin_arg, cli_bin_arg,
 airplay_helper_bin_arg, binary_provenance) = sys.argv[1:]
root = pathlib.Path(root_arg)
ffmpeg_stage = pathlib.Path(ffmpeg_stage_arg)

def command(*arguments):
    return subprocess.check_output(arguments, text=True, stderr=subprocess.STDOUT).strip()

def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()

airplay_manifest = (root / "airplay-helper/Cargo.toml").read_text()
revision_match = re.search(r'rev\s*=\s*"([0-9a-f]{40})"', airplay_manifest)
if not revision_match:
    raise SystemExit("AirPlay helper git revision is not pinned")
sparkle = json.loads(pathlib.Path(sparkle_resolved_arg).read_text())["pins"][0]["state"]
ffmpeg_provenance = json.loads((ffmpeg_stage / "provenance.json").read_text()) if ffmpeg_stage.exists() else None
input_binaries = {
    "fozmo": {"sha256": sha256(core_bin_arg), "source": binary_provenance},
    "fozmoctl": {"sha256": sha256(cli_bin_arg), "source": binary_provenance},
}
if pathlib.Path(airplay_helper_bin_arg).is_file():
    input_binaries["fozmo-airplay-helper"] = {
        "sha256": sha256(airplay_helper_bin_arg),
        "source": binary_provenance,
    }

manifest = {
    "airplay_git_revision": revision_match.group(1),
    "architecture": "arm64",
    "build_mode": mode,
    "bundle_identifier": "com.fozmo.app",
    "bundled_tools": ["fozmoctl"],
    "distribution_policy": {
        "aggregation_assessment_sha256": sha256(root / "docs/gpl-aggregation-assessment.md"),
        "aggregation_policy_sha256": sha256(root / "LICENSES/gpl-aggregation-policy.json"),
    },
    "excluded_features": ["apple_music_capture", "asio", "in_process_airplay"],
    "ffmpeg": ffmpeg_provenance,
    "ffmpeg_configure_flags_sha256": sha256(ffmpeg_stage / "configure-flags.txt") if ffmpeg_stage.exists() else None,
    "included_features": feature_string.split(","),
    "input_binaries": input_binaries,
    "lockfile_sha256": {
        "Cargo.lock": sha256(root / "Cargo.lock"),
        "airplay-helper/Cargo.lock": sha256(root / "airplay-helper/Cargo.lock"),
        "macos/FozmoLauncher/Package.resolved": sha256(root / "macos/FozmoLauncher/Package.resolved"),
        "ui/package-lock.json": sha256(root / "ui/package-lock.json"),
    },
    "git_commit": command("git", "-C", str(root), "rev-parse", "HEAD"),
    "git_tree": command("git", "-C", str(root), "rev-parse", "HEAD^{tree}"),
    "minimum_macos": "13.0",
    "source_path_hygiene": {
        "bundle_scan": "all regular files, including Mach-O binaries",
        "external_rustflags": "rejected" if mode in {"public", "unsigned-public"} else "preserved before mandatory remapping",
        "rust_path_remapping": {"true": True, "unknown": None}[rust_remap_arg],
    },
    "sparkle": {"revision": sparkle["revision"], "version": sparkle["version"]},
    "target": "aarch64-apple-darwin",
    "toolchain": {
        "cargo": command("cargo", "--version"),
        "git": command("git", "--version"),
        "node": command("node", "--version"),
        "npm": command("npm", "--version"),
        "rustc": command("rustc", "--version"),
        "swift": command("swift", "--version").splitlines()[0],
        "xcode": command("xcodebuild", "-version").replace("\n", "; "),
    },
    "version": version,
}
pathlib.Path(output_arg).write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
PY
if [[ -n "${APP_ICON_ICNS:-}" && -f "$APP_ICON_ICNS" ]]; then
  cp "$APP_ICON_ICNS" "$RESOURCES/Fozmo.icns"
else
  plutil -remove CFBundleIconFile "$CONTENTS/Info.plist"
fi

# SwiftPM adds an absolute fallback rpath into the selected Xcode toolchain.
# macOS 13 supplies the Swift runtime under /usr/lib/swift, so a distributed
# launcher must not retain that developer-machine-only fallback.
while IFS= read -r rpath; do
  case "$rpath" in
    /Applications/Xcode*.app/Contents/Developer/*|/Library/Developer/*)
      install_name_tool -delete_rpath "$rpath" "$MACOS/Fozmo"
      ;;
  esac
done < <(otool -l "$MACOS/Fozmo" | awk '/cmd LC_RPATH/{found=1; next} found && $1 == "path"{print $2; found=0}')
install_name_tool -add_rpath '@executable_path/../Frameworks' "$MACOS/Fozmo"

SPARKLE_EXECUTABLES=(
  "$FRAMEWORKS/Sparkle.framework/Versions/B/Autoupdate"
  "$FRAMEWORKS/Sparkle.framework/Versions/B/Updater.app/Contents/MacOS/Updater"
  "$FRAMEWORKS/Sparkle.framework/Versions/B/XPCServices/Downloader.xpc/Contents/MacOS/Downloader"
  "$FRAMEWORKS/Sparkle.framework/Versions/B/XPCServices/Installer.xpc/Contents/MacOS/Installer"
  "$FRAMEWORKS/Sparkle.framework/Versions/B/Sparkle"
)
for executable in "${SPARKLE_EXECUTABLES[@]}"; do
  if lipo -archs "$executable" | grep -qw x86_64; then
    lipo -thin arm64 "$executable" -output "$executable.arm64"
    mv "$executable.arm64" "$executable"
  fi
done

dsymutil "$MACOS/Fozmo" -o "$DSYMS/Fozmo.dSYM"
dsymutil "$HELPERS/fozmo-server" -o "$DSYMS/fozmo-server.dSYM" || true
dsymutil "$HELPERS/fozmoctl" -o "$DSYMS/fozmoctl.dSYM" || true
[[ ! -x "$HELPERS/fozmo-airplay-helper" ]] || dsymutil "$HELPERS/fozmo-airplay-helper" -o "$DSYMS/fozmo-airplay-helper.dSYM" || true
strip -x "$MACOS/Fozmo" "$HELPERS/fozmo-server" "$HELPERS/fozmoctl"
[[ ! -x "$HELPERS/fozmo-airplay-helper" ]] || strip -x "$HELPERS/fozmo-airplay-helper"
[[ ! -x "$HELPERS/ffmpeg" ]] || strip -x "$HELPERS/ffmpeg"

if is_public_build_mode "$MODE"; then
  [[ -f "$RESOURCES/licenses/AirPlay-helper-GPL-2.0.txt" ]] || die "GPL helper license text is missing"
fi

sign_item() {
  if [[ "$MODE" == "public" ]]; then
    codesign --force --options runtime --timestamp --sign "$DEVELOPER_ID_APPLICATION" "$1"
  elif [[ "$MODE" == "unsigned-public" ]]; then
    codesign --force --options runtime --timestamp=none --sign - "$1"
  else
    codesign --force --options runtime --timestamp=none --sign "${FOZMO_DEV_SIGN_IDENTITY:--}" "$1"
  fi
}

for helper in "$HELPERS/fozmo-server" "$HELPERS/fozmoctl" "$HELPERS/fozmo-airplay-helper" "$HELPERS/ffmpeg"; do
  [[ ! -x "$helper" ]] || sign_item "$helper"
done
sign_item "$FRAMEWORKS/Sparkle.framework/Versions/B/Autoupdate"
sign_item "$FRAMEWORKS/Sparkle.framework/Versions/B/XPCServices/Downloader.xpc"
sign_item "$FRAMEWORKS/Sparkle.framework/Versions/B/XPCServices/Installer.xpc"
sign_item "$FRAMEWORKS/Sparkle.framework/Versions/B/Updater.app"
sign_item "$FRAMEWORKS/Sparkle.framework"
if [[ "$MODE" == "public" ]]; then
  codesign --force --options runtime --timestamp --sign "$DEVELOPER_ID_APPLICATION" \
    --entitlements "$LAUNCHER_DIR/Resources/Fozmo.entitlements" "$APP_PATH"
elif [[ "$MODE" == "unsigned-public" ]]; then
  codesign --force --options runtime --timestamp=none --sign - \
    --entitlements "$LAUNCHER_DIR/Resources/FozmoUnsigned.entitlements" "$APP_PATH"
else
  codesign --force --options runtime --timestamp=none --sign "${FOZMO_DEV_SIGN_IDENTITY:--}" \
    --entitlements "$LAUNCHER_DIR/Resources/FozmoDev.entitlements" "$APP_PATH"
fi

for item in \
  "$HELPERS/fozmo-server" \
  "$HELPERS/fozmoctl" \
  "$FRAMEWORKS/Sparkle.framework/Versions/B/Autoupdate" \
  "$FRAMEWORKS/Sparkle.framework/Versions/B/XPCServices/Downloader.xpc" \
  "$FRAMEWORKS/Sparkle.framework/Versions/B/XPCServices/Installer.xpc" \
  "$FRAMEWORKS/Sparkle.framework/Versions/B/Updater.app" \
  "$FRAMEWORKS/Sparkle.framework" \
  "$APP_PATH"; do
  codesign --verify --strict --verbose=2 "$item"
done

file "$MACOS/Fozmo" "$HELPERS/fozmo-server" "$HELPERS/fozmoctl" | grep -v 'arm64' && die "non-arm64 primary executable found" || true
! find "$APP_PATH" -type f \( -name '*.db' -o -name '*.sqlite' -o -name 'settings.json' -o -name '*.log' \) -print | grep -q . \
  || die "private runtime data was included in the app"

if is_public_build_mode "$MODE"; then
  [[ -z "$(git -C "$ROOT_DIR" status --porcelain --untracked-files=normal)" ]] \
    || die "public build modified the clean source worktree"
fi

note "$MODE app assembled at $APP_PATH"
