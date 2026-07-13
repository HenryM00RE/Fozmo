#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "$0")" && pwd)/common.sh"

require_command cargo
require_command git
require_command python3
require_command shasum
require_command tar

SOURCE_ARCHIVE="${1:-}"
[[ -f "$SOURCE_ARCHIVE" ]] || die "usage: verify-source-archive.sh <Fozmo-version-source.tar.zst>"

VERSION="${VERSION:-$(project_version)}"
EXPECTED_COMMIT="${EXPECTED_COMMIT:-$(git -C "$ROOT_DIR" rev-parse HEAD)}"
EXPECTED_TREE="${EXPECTED_TREE:-$(git -C "$ROOT_DIR" rev-parse 'HEAD^{tree}')}"
PACKAGE_NAME="Fozmo-$VERSION-source"
WORK="$BUILD_DIR/source-archive-verification-work"
PACKAGE_ROOT="$WORK/$PACKAGE_NAME"
TARGET_DIR="${SOURCE_BUILD_TARGET_DIR:-$BUILD_DIR/source-archive-verification-target}"
RECEIPT="${SOURCE_VERIFICATION_RECEIPT:-$BUILD_DIR/source-archive-verification-$VERSION.json}"
ZSTD_BIN="${ZSTD_BIN:-$BUILD_DIR/source-tools/zstd-1.5.7/programs/zstd}"
[[ -x "$ZSTD_BIN" ]] || die "pinned zstd binary is missing: $ZSTD_BIN"

SOURCE_SHA256="$(shasum -a 256 "$SOURCE_ARCHIVE" | awk '{print $1}')"
STARTED_AT="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

rm -rf "$WORK" "$TARGET_DIR"
mkdir -p "$WORK" "$TARGET_DIR" "$(dirname "$RECEIPT")"

note "Checking corresponding-source archive paths"
"$ZSTD_BIN" -dc --no-progress "$SOURCE_ARCHIVE" \
  | tar -tf - >"$WORK/archive-files.txt"
python3 - "$PACKAGE_NAME" "$WORK/archive-files.txt" <<'PY'
import pathlib
import sys

expected_root = sys.argv[1]
entries = [
    line.rstrip("\n")
    for line in pathlib.Path(sys.argv[2]).read_text().splitlines(keepends=True)
    if line.rstrip("\n")
]
if not entries:
    raise SystemExit("source archive is empty")
for entry in entries:
    path = pathlib.PurePosixPath(entry)
    if path.is_absolute() or ".." in path.parts:
        raise SystemExit(f"unsafe source archive path: {entry}")
    if not path.parts or path.parts[0] != expected_root:
        raise SystemExit(f"source archive has an unexpected top-level path: {entry}")
PY

note "Extracting exact corresponding source"
"$ZSTD_BIN" -dc --no-progress "$SOURCE_ARCHIVE" | tar -xf - -C "$WORK"
[[ -d "$PACKAGE_ROOT" ]] || die "source archive did not contain $PACKAGE_NAME"
[[ -s "$PACKAGE_ROOT/SOURCE-MANIFEST.json" ]] || die "source archive manifest is missing"
[[ -s "$PACKAGE_ROOT/.cargo/config.vendor.toml" ]] || die "offline Cargo configuration is missing"

python3 - "$PACKAGE_ROOT/SOURCE-MANIFEST.json" "$PACKAGE_ROOT" "$VERSION" "$EXPECTED_COMMIT" "$EXPECTED_TREE" <<'PY'
import hashlib
import json
import pathlib
import sys

manifest_arg, package_root_arg, version, commit, tree = sys.argv[1:]
package_root = pathlib.Path(package_root_arg)
manifest = json.loads(pathlib.Path(manifest_arg).read_text())
if manifest.get("format_version") != 1:
    raise SystemExit("unsupported source manifest format")
if manifest.get("version") != version:
    raise SystemExit("source manifest version does not match the release")
if manifest.get("git_commit") != commit or manifest.get("git_tree") != tree:
    raise SystemExit("source manifest does not identify the release snapshot")
if manifest.get("rust_dependencies_vendored") is not True:
    raise SystemExit("source manifest does not record vendored Rust dependencies")
for relative, expected in manifest.get("release_inputs_sha256", {}).items():
    path = package_root / relative
    if not path.is_file():
        raise SystemExit(f"source release input is missing: {relative}")
    actual = hashlib.sha256(path.read_bytes()).hexdigest()
    if actual != expected:
        raise SystemExit(f"source release input checksum mismatch: {relative}")
PY

# These binaries are eligible for packaging by the unsigned release entry
# point. Remapping the extracted source and build roots gives them the same
# source-path privacy properties as binaries built directly by build-app.sh.
CARGO_RELEASE_RUSTFLAGS=""
append_release_rustflag() {
  [[ -z "$CARGO_RELEASE_RUSTFLAGS" ]] || CARGO_RELEASE_RUSTFLAGS+=$'\x1f'
  CARGO_RELEASE_RUSTFLAGS+="$1"
}
append_release_rustflag "--remap-path-prefix=$PACKAGE_ROOT=/usr/src/fozmo"
append_release_rustflag "--remap-path-prefix=$TARGET_DIR=/usr/src/fozmo-target"
if [[ -n "${HOME:-}" && "$HOME" == /* ]]; then
  append_release_rustflag "--remap-path-prefix=${HOME%/}=/usr/src/build-home"
fi

CORE_FEATURES="$(release_core_features)"
note "Building the exported MIT release source offline"
(
  cd "$PACKAGE_ROOT"
  env -u RUSTFLAGS \
    CARGO_TARGET_DIR="$TARGET_DIR" \
    CARGO_ENCODED_RUSTFLAGS="$CARGO_RELEASE_RUSTFLAGS" \
    CARGO_INCREMENTAL=0 \
    MACOSX_DEPLOYMENT_TARGET=13.0 \
    cargo --config .cargo/config.vendor.toml build \
      --locked \
      --offline \
      --release \
      --target aarch64-apple-darwin \
      --no-default-features \
      --features "$CORE_FEATURES" \
      --bin fozmo \
      --bin fozmoctl
)

note "Building the exported GPL helper source offline"
(
  cd "$PACKAGE_ROOT"
  env -u RUSTFLAGS \
    CARGO_TARGET_DIR="$TARGET_DIR" \
    CARGO_ENCODED_RUSTFLAGS="$CARGO_RELEASE_RUSTFLAGS" \
    CARGO_INCREMENTAL=0 \
    MACOSX_DEPLOYMENT_TARGET=13.0 \
    cargo --config .cargo/config.vendor.toml build \
      --manifest-path airplay-helper/Cargo.toml \
      --locked \
      --offline \
      --release \
      --target aarch64-apple-darwin
)

BIN_DIR="$TARGET_DIR/aarch64-apple-darwin/release"
for binary in fozmo fozmoctl fozmo-airplay-helper; do
  [[ -x "$BIN_DIR/$binary" ]] || die "exported-source release binary is missing: $binary"
done

COMPLETED_AT="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
python3 - \
  "$RECEIPT" \
  "$SOURCE_ARCHIVE" \
  "$SOURCE_SHA256" \
  "$PACKAGE_ROOT/SOURCE-MANIFEST.json" \
  "$VERSION" \
  "$EXPECTED_COMMIT" \
  "$EXPECTED_TREE" \
  "$CORE_FEATURES" \
  "$STARTED_AT" \
  "$COMPLETED_AT" \
  "$BIN_DIR" <<'PY'
import hashlib
import json
import pathlib
import subprocess
import sys

(output_arg, archive_arg, archive_sha256, source_manifest_arg, version, commit,
 tree, feature_string, started_at, completed_at, bin_dir_arg) = sys.argv[1:]

def sha256(path):
    digest = hashlib.sha256()
    with pathlib.Path(path).open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()

def command(*arguments):
    return subprocess.check_output(arguments, text=True, stderr=subprocess.STDOUT).strip()

bin_dir = pathlib.Path(bin_dir_arg)
manifest = {
    "binaries_sha256": {
        name: sha256(bin_dir / name)
        for name in ("fozmo", "fozmoctl", "fozmo-airplay-helper")
    },
    "completed_at": completed_at,
    "git_commit": commit,
    "git_tree": tree,
    "included_features": feature_string.split(","),
    "release_profile": True,
    "source_archive": pathlib.Path(archive_arg).name,
    "source_archive_sha256": archive_sha256,
    "source_manifest_sha256": sha256(source_manifest_arg),
    "started_at": started_at,
    "status": "passed",
    "target": "aarch64-apple-darwin",
    "toolchain": {
        "cargo": command("cargo", "--version"),
        "rustc": command("rustc", "--version"),
    },
    "verification": "locked offline release builds of the exported MIT core and GPL helper",
    "version": version,
}
pathlib.Path(output_arg).write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
PY

python3 - "$RECEIPT" <<'PY'
import json
import pathlib
import sys

receipt = json.loads(pathlib.Path(sys.argv[1]).read_text())
if receipt.get("status") != "passed" or receipt.get("release_profile") is not True:
    raise SystemExit("source verification receipt is invalid")
PY

note "Exported source release build passed; receipt written to $RECEIPT"
