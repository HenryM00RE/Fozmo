#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "$0")" && pwd)/common.sh"

require_command otool
require_command python3
require_command vtool

APP_PATH="${1:-$BUILD_DIR/Fozmo.app}"
[[ -d "$APP_PATH" ]] || die "app bundle is missing: $APP_PATH"
MODE="${FOZMO_BUILD_MODE:-dev}"
[[ "$MODE" == "dev" || "$MODE" == "public" || "$MODE" == "unsigned-public" ]] \
  || die "FOZMO_BUILD_MODE must be dev, public, or unsigned-public"
EXPECTED_VERSION="${VERSION:-$(project_version)}"

INFO="$APP_PATH/Contents/Info.plist"
HELPERS="$APP_PATH/Contents/Helpers"
FRAMEWORK="$APP_PATH/Contents/Frameworks/Sparkle.framework"

[[ "$(plutil -extract CFBundleIdentifier raw "$INFO")" == "com.fozmo.app" ]] || die "unexpected bundle identifier"
[[ "$(plutil -extract CFBundleShortVersionString raw "$INFO")" == "$EXPECTED_VERSION" ]] || die "unexpected application version"
[[ "$(plutil -extract LSMinimumSystemVersion raw "$INFO")" == "13.0" ]] || die "unexpected deployment target"
[[ "$(plutil -extract LSUIElement raw "$INFO")" == "true" ]] || die "launcher is not an agent app"
for resource in \
  "$APP_PATH/Contents/Resources/static/react-app/index.html" \
  "$APP_PATH/Contents/Resources/static/styles.css" \
  "$APP_PATH/Contents/Resources/static/fonts/anton.ttf" \
  "$APP_PATH/Contents/Resources/licenses/Fozmo-MIT.txt" \
  "$APP_PATH/Contents/Resources/licenses/Sparkle-MIT.txt" \
  "$APP_PATH/Contents/Resources/licenses/COMPONENTS.md" \
  "$APP_PATH/Contents/Resources/licenses/THIRD-PARTY-SOURCES.md" \
  "$APP_PATH/Contents/Resources/licenses/GPL-AGGREGATION-ASSESSMENT.md" \
  "$APP_PATH/Contents/Resources/licenses/project/QBZ-MIT.txt" \
  "$APP_PATH/Contents/Resources/licenses/project/gpl-aggregation-policy.json" \
  "$APP_PATH/Contents/Resources/licenses/macos-components/Anton-OFL-1.1.txt"; do
  [[ -f "$resource" ]] || die "required resource is missing: $resource"
done
if is_public_build_mode "$MODE"; then
  for notice in \
    "$APP_PATH/Contents/Resources/licenses/third-party/cargo-packages.json" \
    "$APP_PATH/Contents/Resources/licenses/third-party/npm-packages.json" \
    "$APP_PATH/Contents/Resources/licenses/third-party/spdx-text/MIT.txt" \
    "$APP_PATH/Contents/Resources/licenses/third-party/spdx-text/MPL-2.0.txt"; do
    [[ -s "$notice" ]] || die "public dependency notice is missing: $notice"
  done
fi

MACH_O_BINARIES=(
  "$APP_PATH/Contents/MacOS/Fozmo"
  "$HELPERS/fozmo-server"
  "$HELPERS/fozmoctl"
  "$FRAMEWORK/Versions/B/Autoupdate"
  "$FRAMEWORK/Versions/B/Updater.app/Contents/MacOS/Updater"
  "$FRAMEWORK/Versions/B/XPCServices/Downloader.xpc/Contents/MacOS/Downloader"
  "$FRAMEWORK/Versions/B/XPCServices/Installer.xpc/Contents/MacOS/Installer"
  "$FRAMEWORK/Versions/B/Sparkle"
)
for optional in "$HELPERS/fozmo-airplay-helper" "$HELPERS/ffmpeg"; do
  [[ ! -x "$optional" ]] || MACH_O_BINARIES+=("$optional")
done
if is_public_build_mode "$MODE"; then
  [[ -x "$HELPERS/fozmo-airplay-helper" ]] || die "public release app is missing the AirPlay helper"
  [[ -x "$HELPERS/ffmpeg" ]] || die "public release app is missing FFmpeg"
fi

for executable in "${MACH_O_BINARIES[@]}"; do
  [[ -x "$executable" ]] || die "missing executable: $executable"
  file "$executable" | grep -q 'arm64' || die "non-arm64 executable: $executable"
  MINIMUM_OS="$(vtool -show-build "$executable" | awk '/minos/{print $2; exit}')"
  [[ -n "$MINIMUM_OS" ]] || die "Mach-O build version is missing: $executable"
  python3 - "$MINIMUM_OS" "$executable" <<'PY'
import sys
minimum = tuple(int(part) for part in sys.argv[1].split("."))
if minimum > (13, 0):
    raise SystemExit(f"{sys.argv[2]} requires macOS {sys.argv[1]}, newer than the supported 13.0 floor")
PY
  while read -r dependency _; do
    case "$dependency" in
      /System/Library/*|/usr/lib/*|@rpath/Sparkle.framework/*) ;;
      *) die "unexpected dynamic dependency in $executable: $dependency" ;;
    esac
  done < <(otool -L "$executable" | tail -n +2)
done
for primary in "$APP_PATH/Contents/MacOS/Fozmo" "$HELPERS/fozmo-server" "$HELPERS/fozmoctl" "$HELPERS/fozmo-airplay-helper" "$HELPERS/ffmpeg"; do
  [[ ! -x "$primary" ]] || [[ "$(vtool -show-build "$primary" | awk '/minos/{print $2; exit}')" == "13.0" ]] \
    || die "Fozmo executable is not pinned to minimum macOS 13.0: $primary"
done
if [[ -x "$HELPERS/fozmo-airplay-helper" ]]; then
  [[ -f "$APP_PATH/Contents/Resources/licenses/AirPlay-helper-GPL-2.0.txt" ]] \
    || die "AirPlay helper GPL license is missing"
fi

BUILD_MANIFEST="$APP_PATH/Contents/Resources/build-manifest.json"
[[ -s "$BUILD_MANIFEST" ]] || die "build provenance manifest is missing"
python3 - "$BUILD_MANIFEST" "$MODE" "$EXPECTED_VERSION" <<'PY'
import json
import sys
manifest = json.load(open(sys.argv[1]))
expected_mode = sys.argv[2]
expected_version = sys.argv[3]
required = {"local_library", "qobuz", "pcm_output", "airplay_helper", "sonos", "hegel", "upnp", "experimental_dsd256"}
excluded = {"apple_music_capture", "asio", "in_process_airplay"}
if manifest.get("build_mode") != expected_mode or manifest.get("version") != expected_version:
    raise SystemExit("packaged build mode/version provenance is invalid")
if set(manifest.get("included_features", [])) != required:
    raise SystemExit("packaged core feature allowlist does not match the release contract")
if set(manifest.get("excluded_features", [])) != excluded:
    raise SystemExit("packaged core exclusions do not match the release contract")
if manifest.get("target") != "aarch64-apple-darwin" or manifest.get("minimum_macos") != "13.0":
    raise SystemExit("packaged target/deployment provenance is invalid")
if manifest.get("bundled_tools") != ["fozmoctl"]:
    raise SystemExit("packaged command-line tool manifest is invalid")
if manifest.get("build_mode") in {"public", "unsigned-public"}:
    hygiene = manifest.get("source_path_hygiene", {})
    if hygiene.get("rust_path_remapping") is not True:
        raise SystemExit("public Rust binaries were not built with source-path remapping")
    if hygiene.get("external_rustflags") != "rejected":
        raise SystemExit("public build did not enforce release-owned Rust flags")
PY

"$HELPERS/fozmoctl" --help | grep -q "Control a Fozmo core over HTTP" \
  || die "bundled fozmoctl failed its command-line smoke test"
if is_public_build_mode "$MODE"; then
  "$SCRIPT_DIR/smoke-bundled-server.sh" "$APP_PATH"
fi

SIGN_ITEMS=(
  "$HELPERS/fozmo-server"
  "$HELPERS/fozmoctl"
  "$FRAMEWORK/Versions/B/Autoupdate"
  "$FRAMEWORK/Versions/B/XPCServices/Downloader.xpc"
  "$FRAMEWORK/Versions/B/XPCServices/Installer.xpc"
  "$FRAMEWORK/Versions/B/Updater.app"
  "$FRAMEWORK"
  "$APP_PATH"
)
for optional in "$HELPERS/fozmo-airplay-helper" "$HELPERS/ffmpeg"; do
  [[ ! -x "$optional" ]] || SIGN_ITEMS+=("$optional")
done
for item in "${SIGN_ITEMS[@]}"; do
  codesign --verify --strict --verbose=2 "$item"
done
if [[ "$MODE" == "unsigned-public" ]]; then
  codesign --verify --deep --strict --verbose=2 "$APP_PATH"
  SIGN_DETAILS="$(codesign -dv --verbose=4 "$APP_PATH" 2>&1)"
  grep -q '^Signature=adhoc$' <<<"$SIGN_DETAILS" \
    || die "unsigned-public app is not ad-hoc signed"
  grep -q '^TeamIdentifier=not set$' <<<"$SIGN_DETAILS" \
    || die "unsigned-public app unexpectedly has an Apple signing team"
fi

APP_ENTITLEMENTS="$(codesign -d --entitlements - "$APP_PATH" 2>/dev/null || true)"
if [[ "$MODE" == "public" ]]; then
  ! grep -q 'com.apple.security.cs.disable-library-validation' <<<"$APP_ENTITLEMENTS" \
    || die "public launcher must enforce hardened-runtime library validation"
else
  grep -q 'com.apple.security.cs.disable-library-validation' <<<"$APP_ENTITLEMENTS" \
    || die "ad-hoc launcher cannot load separately signed frameworks"
fi

if [[ "$MODE" == "unsigned-public" ]]; then
  for key in FozmoUpdatesEnabled SUAllowsAutomaticUpdates SUAutomaticallyUpdate SUEnableAutomaticChecks; do
    [[ "$(plutil -extract "$key" raw "$INFO")" == "false" ]] \
      || die "unsigned-public build must disable $key"
  done
fi

for binary in "$HELPERS/fozmo-server" "$HELPERS/fozmoctl" "$HELPERS/fozmo-airplay-helper"; do
  [[ -x "$binary" ]] || continue
  if nm -u "$binary" 2>/dev/null | grep -Eiq 'fdk|aacenc|_FDK'; then
    die "forbidden FDK/AAC encoder symbol in $binary"
  fi
done

if [[ -x "$HELPERS/ffmpeg" ]]; then
  BUILD_CONFIGURATION="$($HELPERS/ffmpeg -hide_banner -buildconf 2>&1)"
  ! grep -q -- '--enable-gpl\|--enable-nonfree\|--enable-network' <<<"$BUILD_CONFIGURATION" \
    || die "bundled FFmpeg has a forbidden configuration"
  "$HELPERS/ffmpeg" -hide_banner -encoders 2>/dev/null | grep -q libopus || die "bundled FFmpeg lacks libopus"
fi

! find "$APP_PATH" -type f \( \
    -name '*.db' -o -name '*.sqlite' -o -name '*.sqlite3' -o \
    -iname 'settings*.json' -o -name '*.log' -o -name '*.pem' -o \
    -name '*.p12' -o -name '*.key' \
  \) -print | grep -q . \
  || die "private runtime data is present"

! find "$APP_PATH" \( -iname '*FozmoCapture*' -o -iname '*.driver' \) -print | grep -q . \
  || die "Apple Music capture driver output is present in the ordinary app"

# Search every regular file, not only Resources: Rust diagnostics and FFmpeg's
# compiled-in configuration are strings inside Mach-O executables. Exact host
# paths cover nonstandard builders, while generic macOS/GitHub runner prefixes
# catch paths inherited from dependency builds and temporary directories.
python3 - "$APP_PATH" "$ROOT_DIR" "$BUILD_DIR" <<'PY'
import os
import pathlib
import sys

app = pathlib.Path(sys.argv[1])
exact_paths = [sys.argv[2], sys.argv[3]]
for name in ("HOME", "GITHUB_WORKSPACE", "RUNNER_TEMP", "TMPDIR"):
    value = os.environ.get(name, "").rstrip("/")
    if value.startswith("/"):
        exact_paths.append(value)

needles = [
    ("macOS user directory", b"/Users/"),
    ("GitHub runner workspace", b"/home/runner/work/"),
    ("GitHub runner temporary directory", b"/home/runner/_temp/"),
    ("macOS per-user temporary directory", b"/private/var/folders/"),
    ("macOS per-user temporary directory", b"/var/folders/"),
    ("Xcode developer directory", b"/Applications/Xcode"),
    ("Apple developer tools directory", b"/Library/Developer/"),
]
for value in exact_paths:
    encoded = value.encode("utf-8")
    if encoded and all(encoded != needle for _, needle in needles):
        needles.append(("host build path", encoded))

offenders = []
for path in app.rglob("*"):
    if not path.is_file():
        continue
    data = path.read_bytes()
    labels = sorted({label for label, needle in needles if needle in data})
    if labels:
        offenders.append((str(path.relative_to(app)), ", ".join(labels)))

if offenders:
    for relative, labels in offenders:
        print(f"embedded build path ({labels}): {relative}", file=sys.stderr)
    raise SystemExit("developer, workspace, or runner temporary paths leaked into the app bundle")
PY

note "App structure, signatures, architecture, licenses, and privacy checks passed"
