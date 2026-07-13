#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "$0")" && pwd)/common.sh"

require_command cargo
require_command curl
require_command git
require_command make
require_command python3
require_command rg
require_command shasum
require_command tar

VERSION="${VERSION:-$(project_version)}"
COMMIT="$(git -C "$ROOT_DIR" rev-parse HEAD)"
TREE="$(git -C "$ROOT_DIR" rev-parse 'HEAD^{tree}')"
PACKAGE_NAME="Fozmo-$VERSION-source"
WORK="$BUILD_DIR/source-archive-work"
PACKAGE_ROOT="$WORK/$PACKAGE_NAME"
OUTPUT="${1:-${SOURCE_ARCHIVE_OUTPUT:-$BUILD_DIR/Fozmo-$VERSION-source.tar.zst}}"

[[ -z "$(git -C "$ROOT_DIR" status --porcelain --untracked-files=normal)" ]] \
  || die "corresponding source must be made from a clean tracked checkout"
[[ -f "$ROOT_DIR/airplay-helper/Cargo.toml" ]] || die "standalone AirPlay helper source is missing"
[[ -f "$ROOT_DIR/airplay-helper/Cargo.lock" ]] || die "AirPlay helper lockfile is missing"

rm -rf "$WORK"
mkdir -p "$WORK"
git -C "$ROOT_DIR" archive --format=tar --prefix="$PACKAGE_NAME/" "$COMMIT" | tar -xf - -C "$WORK"

note "Vendoring locked Rust sources, including pinned GPL git dependencies"
mkdir -p "$PACKAGE_ROOT/vendor" "$PACKAGE_ROOT/.cargo"
(
  cd "$PACKAGE_ROOT"
  cargo vendor \
    --locked \
    --manifest-path Cargo.toml \
    --sync airplay-helper/Cargo.toml \
    --versioned-dirs \
    vendor/cargo >.cargo/config.vendor.toml
)

if rg -n 'name = "fdk-aac(-sys)?"' "$PACKAGE_ROOT/vendor/cargo" -g Cargo.toml >/dev/null; then
  die "corresponding source unexpectedly contains FDK AAC"
fi
for crate in airplay-core airplay-client; do
  find "$PACKAGE_ROOT/vendor/cargo" -maxdepth 1 -type d -name "$crate-*" | grep -q . \
    || die "cargo vendor did not preserve pinned GPL crate $crate"
done
[[ -f "$PACKAGE_ROOT/airplay-helper/vendor/airplay-audio/Cargo.toml" ]] \
  || die "patched GPL airplay-audio source is missing"

FFMPEG_STAGE="${FFMPEG_STAGE:-$BUILD_DIR/third-party/ffmpeg-stage}"
if [[ ! -f "$FFMPEG_STAGE/provenance.json" ]]; then
  FOZMO_FFMPEG_STAGE="$FFMPEG_STAGE" "$SCRIPT_DIR/build-ffmpeg.sh"
fi
"$SCRIPT_DIR/audit-ffmpeg.sh" "$FFMPEG_STAGE"
mkdir -p "$PACKAGE_ROOT/vendor/upstream-sources"
FFMPEG_SOURCE="$FFMPEG_STAGE/source/ffmpeg-8.1.2.tar.xz"
OPUS_SOURCE="$FFMPEG_STAGE/source/opus-1.6.1.tar.gz"
FFMPEG_SOURCE_SHA256=464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c
OPUS_SOURCE_SHA256=6ffcb593207be92584df15b32466ed64bbec99109f007c82205f0194572411a1
[[ -f "$FFMPEG_SOURCE" && "$(shasum -a 256 "$FFMPEG_SOURCE" | awk '{print $1}')" == "$FFMPEG_SOURCE_SHA256" ]] \
  || die "staged FFmpeg source archive checksum mismatch"
[[ -f "$OPUS_SOURCE" && "$(shasum -a 256 "$OPUS_SOURCE" | awk '{print $1}')" == "$OPUS_SOURCE_SHA256" ]] \
  || die "staged libopus source archive checksum mismatch"
cp "$FFMPEG_SOURCE" "$OPUS_SOURCE" "$PACKAGE_ROOT/vendor/upstream-sources/"
cp "$FFMPEG_STAGE/provenance.json" "$PACKAGE_ROOT/vendor/upstream-sources/ffmpeg-provenance.json"

cat >"$PACKAGE_ROOT/SOURCE-BUILD.md" <<'EOF'
# Building the exact Fozmo release source

This archive contains the tracked MIT Fozmo source, the standalone
GPL-2.0-only AirPlay helper, the MIT IPC protocol, every locked Cargo registry
and git dependency used by either Rust package, and the exact FFmpeg/libopus
source archives used by the macOS build.

Use the generated offline Cargo source configuration:

```sh
cargo --config .cargo/config.vendor.toml build --locked --offline --release
cargo --config .cargo/config.vendor.toml build --locked --offline \
  --manifest-path airplay-helper/Cargo.toml --release
```

The AirPlay helper's locally patched `airplay-audio` source and patch notes are
under `airplay-helper/vendor/`. It is pinned to the revision in
`airplay-helper/Cargo.lock`; its FDK AAC dependency is disabled and absent.

The macOS packaging instructions and pinned LGPL FFmpeg configuration are in
`macos/README.md` and `macos/scripts/build-ffmpeg.sh`. The upstream archives
and their provenance manifest are under `vendor/upstream-sources/`.
EOF

python3 - "$PACKAGE_ROOT/SOURCE-MANIFEST.json" "$PACKAGE_ROOT" "$COMMIT" "$TREE" "$PACKAGE_NAME" "$VERSION" <<'PY'
import hashlib
import json
import pathlib
import sys

output_arg, package_root_arg, commit, tree, package_name, version = sys.argv[1:]
package_root = pathlib.Path(package_root_arg)

def sha256(relative_path):
    return hashlib.sha256((package_root / relative_path).read_bytes()).hexdigest()

manifest = {
    "format_version": 1,
    "git_commit": commit,
    "git_tree": tree,
    "package": package_name,
    "release_inputs_sha256": {
        "Cargo.lock": sha256("Cargo.lock"),
        "Cargo.toml": sha256("Cargo.toml"),
        "airplay-helper/Cargo.lock": sha256("airplay-helper/Cargo.lock"),
        "airplay-helper/Cargo.toml": sha256("airplay-helper/Cargo.toml"),
        "macos/FozmoLauncher/Package.resolved": sha256("macos/FozmoLauncher/Package.resolved"),
        "ui/package-lock.json": sha256("ui/package-lock.json"),
    },
    "rust_dependencies_vendored": True,
    "upstream_source_archives": {
        "ffmpeg": "8.1.2",
        "libopus": "1.6.1",
    },
    "version": version,
}
pathlib.Path(output_arg).write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
PY

note "Verifying vendored Rust source works offline"
(
  cd "$PACKAGE_ROOT"
  cargo --config .cargo/config.vendor.toml metadata --locked --offline --format-version 1 --no-deps >/dev/null
  cargo --config .cargo/config.vendor.toml metadata --locked --offline \
    --manifest-path airplay-helper/Cargo.toml --format-version 1 --no-deps >/dev/null
)

# Normalize ownership, modes, order, and timestamps before archiving. zstd
# frames do not carry wall-clock timestamps; a single compression thread keeps
# output stable across repeated builds from the same commit/toolchain.
chmod -R u=rwX,go=rX "$PACKAGE_ROOT"
find "$PACKAGE_ROOT" -exec touch -h -t 200001010000 {} +
(
  cd "$WORK"
  find "$PACKAGE_NAME" -print | LC_ALL=C sort >archive-files.txt
  COPYFILE_DISABLE=1 tar \
    --format gnutar \
    --no-recursion \
    --no-acls \
    --no-fflags \
    --no-mac-metadata \
    --no-xattrs \
    --uid 0 \
    --gid 0 \
    --uname root \
    --gname root \
    -cf source.tar \
    -T archive-files.txt
)

ZSTD_VERSION=1.5.7
ZSTD_SHA256=eb33e51f49a15e023950cd7825ca74a4a2b43db8354825ac24fc1b7ee09e6fa3
ZSTD_ROOT="$BUILD_DIR/source-tools/zstd-$ZSTD_VERSION"
ZSTD_ARCHIVE="$BUILD_DIR/source-tools/zstd-$ZSTD_VERSION.tar.gz"
ZSTD_BIN="$ZSTD_ROOT/programs/zstd"
if [[ ! -x "$ZSTD_BIN" ]]; then
  mkdir -p "$BUILD_DIR/source-tools"
  curl --fail --location --retry 3 --show-error \
    "https://github.com/facebook/zstd/releases/download/v$ZSTD_VERSION/zstd-$ZSTD_VERSION.tar.gz" \
    -o "$ZSTD_ARCHIVE.partial"
  mv "$ZSTD_ARCHIVE.partial" "$ZSTD_ARCHIVE"
  [[ "$(shasum -a 256 "$ZSTD_ARCHIVE" | awk '{print $1}')" == "$ZSTD_SHA256" ]] \
    || die "zstd source checksum mismatch"
  rm -rf "$ZSTD_ROOT"
  tar -xf "$ZSTD_ARCHIVE" -C "$BUILD_DIR/source-tools"
  make -C "$ZSTD_ROOT" -j"${JOBS:-$(sysctl -n hw.logicalcpu)}" zstd
fi

mkdir -p "$(dirname "$OUTPUT")"
rm -f "$OUTPUT"
"$ZSTD_BIN" -19 --threads=1 --no-progress --force "$WORK/source.tar" -o "$OUTPUT"
(cd "$(dirname "$OUTPUT")" && shasum -a 256 "$(basename "$OUTPUT")") >"$OUTPUT.sha256"
"$ZSTD_BIN" --test "$OUTPUT"

note "Corresponding source archive created at $OUTPUT"
