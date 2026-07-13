#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "$0")" && pwd)/common.sh"

require_command curl
require_command shasum
require_command xcrun
require_command make
require_command ditto

FFMPEG_VERSION=8.1.2
FFMPEG_SHA256=464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c
OPUS_VERSION=1.6.1
OPUS_SHA256=6ffcb593207be92584df15b32466ed64bbec99109f007c82205f0194572411a1
MIN_MACOS=13.0

THIRD_PARTY_DIR="${FOZMO_THIRD_PARTY_BUILD_DIR:-$BUILD_DIR/third-party}"
DOWNLOAD_DIR="$THIRD_PARTY_DIR/downloads"
SOURCE_DIR="$THIRD_PARTY_DIR/source"
WORK_DIR="$THIRD_PARTY_DIR/work"
OUTPUT_DIR="${FOZMO_FFMPEG_STAGE:-$THIRD_PARTY_DIR/ffmpeg-stage}"
OPUS_CONFIGURE_PREFIX=/fozmo-opus
FFMPEG_CONFIGURE_PREFIX=/fozmo
OPUS_DESTDIR="$WORK_DIR/opus-install"
FFMPEG_DESTDIR="$WORK_DIR/ffmpeg-install"

mkdir -p "$DOWNLOAD_DIR" "$SOURCE_DIR" "$WORK_DIR"

download_and_verify() {
  local url="$1" output="$2" expected="$3"
  if [[ ! -f "$output" ]]; then
    note "Downloading $(basename "$output")"
    curl --fail --location --retry 3 --show-error "$url" -o "$output.partial"
    mv "$output.partial" "$output"
  fi
  local actual
  actual="$(shasum -a 256 "$output" | awk '{print $1}')"
  [[ "$actual" == "$expected" ]] || die "checksum mismatch for $output (expected $expected, got $actual)"
}

FFMPEG_ARCHIVE="$DOWNLOAD_DIR/ffmpeg-$FFMPEG_VERSION.tar.xz"
OPUS_ARCHIVE="$DOWNLOAD_DIR/opus-$OPUS_VERSION.tar.gz"
download_and_verify "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz" "$FFMPEG_ARCHIVE" "$FFMPEG_SHA256"
download_and_verify "https://downloads.xiph.org/releases/opus/opus-$OPUS_VERSION.tar.gz" "$OPUS_ARCHIVE" "$OPUS_SHA256"

rm -rf \
  "$WORK_DIR/ffmpeg-$FFMPEG_VERSION" \
  "$WORK_DIR/opus-$OPUS_VERSION" \
  "$OPUS_DESTDIR" \
  "$FFMPEG_DESTDIR" \
  "$OUTPUT_DIR"
tar -C "$WORK_DIR" -xf "$FFMPEG_ARCHIVE"
tar -C "$WORK_DIR" -xf "$OPUS_ARCHIVE"

JOBS="${JOBS:-$(sysctl -n hw.logicalcpu)}"
XCODE_CLANG="$(xcrun --sdk macosx --find clang)"
SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
export PATH="$(dirname "$XCODE_CLANG"):$PATH"
CLANG=clang
SDK_LINK=fozmo-macos-sdk
ln -s "$SDKROOT" "$WORK_DIR/opus-$OPUS_VERSION/$SDK_LINK"
ln -s "$SDKROOT" "$WORK_DIR/ffmpeg-$FFMPEG_VERSION/$SDK_LINK"
COMMON_CFLAGS="-arch arm64 -isysroot ./$SDK_LINK -mmacosx-version-min=$MIN_MACOS"

note "Building static libopus $OPUS_VERSION"
(
  cd "$WORK_DIR/opus-$OPUS_VERSION"
  env CC="$CLANG" CFLAGS="$COMMON_CFLAGS -O3" LDFLAGS="$COMMON_CFLAGS" \
    ./configure \
      --prefix="$OPUS_CONFIGURE_PREFIX" \
      --disable-shared \
      --enable-static \
      --disable-doc \
      --disable-extra-programs
  make -j"$JOBS"
  make DESTDIR="$OPUS_DESTDIR" install
)

# These are deliberately relative to the FFmpeg source directory. FFmpeg
# records its configure command in the executable, so an absolute staging path
# here would disclose the workspace/user name in every distributed copy.
OPUS_BUILD_PREFIX="../$(basename "$OPUS_DESTDIR")$OPUS_CONFIGURE_PREFIX"
cp "$SCRIPT_DIR/pkg-config-opus.sh" "$WORK_DIR/ffmpeg-$FFMPEG_VERSION/fozmo-pkg-config-opus.sh"

FFMPEG_CONFIGURE_FLAGS=(
  --prefix="$FFMPEG_CONFIGURE_PREFIX"
  --arch=arm64
  --target-os=darwin
  --cc="$CLANG"
  --host-cc="$CLANG"
  --host-cflags="$COMMON_CFLAGS"
  --host-ldflags="$COMMON_CFLAGS"
  --pkg-config=./fozmo-pkg-config-opus.sh
  --pkg-config-flags=--static
  --extra-cflags="$COMMON_CFLAGS -I$OPUS_BUILD_PREFIX/include"
  --extra-ldflags="$COMMON_CFLAGS -L$OPUS_BUILD_PREFIX/lib"
  --extra-libs=-lpthread
  --disable-autodetect
  --disable-everything
  --disable-network
  --disable-gpl
  --disable-nonfree
  --disable-debug
  --disable-doc
  --disable-ffplay
  --disable-ffprobe
  --enable-ffmpeg
  --enable-avcodec
  --enable-avfilter
  --enable-avformat
  --enable-swresample
  --enable-libopus
  --enable-encoder=libopus
  --enable-muxer=ogg
  --enable-protocol=file,pipe
  --enable-demuxer=wav,flac,mp3,mov,ogg,caf,aac,aiff
  --enable-decoder=pcm_s8,pcm_u8,pcm_s16le,pcm_s16be,pcm_s24le,pcm_s24be,pcm_s32le,pcm_s32be,pcm_f32le,pcm_f32be,pcm_f64le,pcm_f64be,flac,mp3,aac,alac,vorbis,opus
  --enable-parser=aac,mpegaudio,opus,vorbis
  --enable-filter=aresample,equalizer,bass,treble,lowpass,highpass,bandreject,allpass,volume,aformat
)

note "Building minimal LGPL FFmpeg $FFMPEG_VERSION"
(
  cd "$WORK_DIR/ffmpeg-$FFMPEG_VERSION"
  OPUS_PREFIX="$OPUS_BUILD_PREFIX" ./configure "${FFMPEG_CONFIGURE_FLAGS[@]}"
  make -j"$JOBS"
  make DESTDIR="$FFMPEG_DESTDIR" install
)
ditto "$FFMPEG_DESTDIR$FFMPEG_CONFIGURE_PREFIX" "$OUTPUT_DIR"
printf '%s\n' "${FFMPEG_CONFIGURE_FLAGS[@]}" >"$OUTPUT_DIR/configure-flags.txt"

mkdir -p "$OUTPUT_DIR/source" "$OUTPUT_DIR/licenses"
cp "$FFMPEG_ARCHIVE" "$OPUS_ARCHIVE" "$OUTPUT_DIR/source/"
cp "$WORK_DIR/ffmpeg-$FFMPEG_VERSION/COPYING.LGPLv2.1" "$OUTPUT_DIR/licenses/FFmpeg-LGPL-2.1.txt"
cp "$WORK_DIR/ffmpeg-$FFMPEG_VERSION/COPYING.LGPLv3" "$OUTPUT_DIR/licenses/FFmpeg-LGPL-3.0.txt"
cp "$WORK_DIR/opus-$OPUS_VERSION/COPYING" "$OUTPUT_DIR/licenses/libopus-BSD-3-Clause.txt"

cat >"$OUTPUT_DIR/provenance.json" <<EOF
{
  "architecture": "arm64",
  "configure_prefix": "$FFMPEG_CONFIGURE_PREFIX",
  "ffmpeg_sha256": "$FFMPEG_SHA256",
  "ffmpeg_version": "$FFMPEG_VERSION",
  "gpl_enabled": false,
  "license_mode": "LGPL",
  "minimum_macos": "$MIN_MACOS",
  "network_enabled": false,
  "nonfree_enabled": false,
  "path_staging": "neutral configure prefixes with DESTDIR",
  "opus_sha256": "$OPUS_SHA256",
  "opus_version": "$OPUS_VERSION"
}
EOF

FFMPEG="$OUTPUT_DIR/bin/ffmpeg"
[[ -x "$FFMPEG" ]] || die "FFmpeg build did not produce $FFMPEG"
file "$FFMPEG" | grep -q 'arm64' || die "FFmpeg output is not arm64"
BUILD_CONFIGURATION="$($FFMPEG -hide_banner -buildconf 2>&1)"
grep -q -- '--disable-gpl' <<<"$BUILD_CONFIGURATION" || die "FFmpeg was not configured with --disable-gpl"
grep -q -- '--disable-nonfree' <<<"$BUILD_CONFIGURATION" || die "FFmpeg was not configured with --disable-nonfree"
grep -q -- '--disable-network' <<<"$BUILD_CONFIGURATION" || die "FFmpeg was not configured with --disable-network"
! grep -q -- '--enable-gpl\|--enable-nonfree\|--enable-network' <<<"$BUILD_CONFIGURATION" \
  || die "forbidden FFmpeg configure flag detected"
$FFMPEG -hide_banner -encoders 2>/dev/null | grep -q 'libopus' || die "libopus encoder is missing"
if otool -L "$FFMPEG" | tail -n +2 | grep -vE '^\s+(/usr/lib/|/System/Library/)' | grep -q .; then
  otool -L "$FFMPEG" >&2
  die "FFmpeg links to a non-system dynamic library"
fi
"$SCRIPT_DIR/audit-ffmpeg.sh" "$OUTPUT_DIR"

note "Minimal FFmpeg staged at $OUTPUT_DIR"
