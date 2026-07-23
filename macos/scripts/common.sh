#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
MACOS_DIR="$ROOT_DIR/macos"
LAUNCHER_DIR="$MACOS_DIR/FozmoLauncher"
BUILD_DIR="${FOZMO_MACOS_BUILD_DIR:-$ROOT_DIR/target/macos}"

die() {
  echo "error: $*" >&2
  exit 1
}

note() {
  echo "==> $*"
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command is missing: $1"
}

require_value() {
  local name="$1"
  [[ -n "${!name:-}" ]] || die "required environment variable is missing: $name"
}

project_version() {
  sed -n 's/^version = "\([0-9][0-9.]*\)"/\1/p' "$ROOT_DIR/Cargo.toml" | head -1
}

bundle_build_number() {
  local version="$1" major minor patch extra
  IFS=. read -r major minor patch extra <<<"$version"
  [[ "$major" =~ ^[0-9]+$ && "$minor" =~ ^[0-9]+$ && "$patch" =~ ^[0-9]+$ && -z "${extra:-}" ]] \
    || die "VERSION must be numeric major.minor.patch (got '$version')"
  echo $((major * 1000000 + minor * 1000 + patch))
}

codesign_arguments() {
  local mode="$1"
  if [[ "$mode" == "public" ]]; then
    printf '%s\n' --force --options runtime --timestamp --sign "$DEVELOPER_ID_APPLICATION"
  elif [[ "$mode" == "unsigned-public" ]]; then
    printf '%s\n' --force --options runtime --timestamp=none --sign -
  else
    printf '%s\n' --force --options runtime --timestamp=none --sign "${FOZMO_DEV_SIGN_IDENTITY:--}"
  fi
}

is_public_build_mode() {
  [[ "$1" == "public" || "$1" == "unsigned-public" ]]
}

release_core_features() {
  printf '%s\n' 'local_library,qobuz,pcm_output,airplay_helper,sonos,hegel,upnp,experimental_dsd256'
}

verify_gpl_aggregation_policy() {
  local mode="$1" version="$2"
  local policy="$ROOT_DIR/LEGAL/gpl-aggregation-policy.json"
  require_command python3
  [[ -s "$policy" ]] || die "tracked GPL aggregation policy is missing: $policy"
  python3 - "$policy" "$ROOT_DIR" "$mode" "$version" <<'PY'
import json
import pathlib
import sys

policy_arg, root_arg, mode, version = sys.argv[1:]
root = pathlib.Path(root_arg).resolve()
policy_path = pathlib.Path(policy_arg)
policy = json.loads(policy_path.read_text())

if policy.get("schema_version") != 1 or policy.get("decision") != "approved":
    raise SystemExit("GPL aggregation policy is not an approved schema-v1 decision")
if policy.get("version") != version:
    raise SystemExit(f"GPL aggregation policy does not cover version {version}")
if mode not in policy.get("release_modes", []):
    raise SystemExit(f"GPL aggregation policy does not cover build mode {mode}")

relative = pathlib.PurePosixPath(str(policy.get("assessment", "")))
if not relative.parts or relative.is_absolute() or ".." in relative.parts:
    raise SystemExit("GPL aggregation policy has an unsafe assessment path")
assessment = root.joinpath(*relative.parts)
if not assessment.is_file():
    raise SystemExit(f"GPL aggregation assessment is missing: {relative}")
date = str(policy.get("assessment_date", ""))
if f"Assessment recorded: {date}." not in assessment.read_text():
    raise SystemExit("GPL aggregation policy date does not match its assessment")
PY
}

verify_macos_build_toolchain() {
  local sdkroot clang swiftc
  require_command xcrun

  sdkroot="$(xcrun --sdk macosx --show-sdk-path 2>/dev/null)" \
    || die "selected Xcode does not provide a macOS SDK"
  [[ -d "$sdkroot/System/Library/Frameworks" ]] \
    || die "selected Xcode macOS SDK is incomplete: $sdkroot"

  clang="$(xcrun --sdk macosx --find clang 2>/dev/null)" \
    || die "selected Xcode does not provide clang for macOS"
  swiftc="$(xcrun --sdk macosx --find swiftc 2>/dev/null)" \
    || die "selected Xcode does not provide swiftc for macOS"
  [[ -x "$clang" ]] || die "selected Xcode clang is not executable: $clang"
  [[ -x "$swiftc" ]] || die "selected Xcode swiftc is not executable: $swiftc"

  "$swiftc" -print-target-info \
    -sdk "$sdkroot" \
    -target arm64-apple-macos13.0 >/dev/null 2>&1 \
    || die "selected Xcode Swift toolchain cannot target arm64 macOS 13"
  "$clang" \
    --target=arm64-apple-macos13.0 \
    -isysroot "$sdkroot" \
    -fsyntax-only \
    -x c /dev/null >/dev/null 2>&1 \
    || die "selected Xcode clang cannot target arm64 macOS 13"
}

verify_public_toolchain() {
  verify_macos_build_toolchain
  [[ "$(xcodebuild -version | sed -n '1s/^Xcode //p')" == "26.6" ]] \
    || die "public release requires Xcode 26.6"
  [[ "$(rustc --version)" == "rustc 1.96."* ]] \
    || die "public release requires rustc 1.96.x"
  [[ "$(cargo --version)" == "cargo 1.96."* ]] \
    || die "public release requires cargo 1.96.x"
  [[ "$(node --version)" == v22.* ]] \
    || die "public release requires Node 22.x"
  swift --version 2>&1 | head -1 | grep -q 'Apple Swift version 6\.3\.3' \
    || die "public release requires the Swift 6.3.3 toolchain shipped with Xcode 26.6"
}
