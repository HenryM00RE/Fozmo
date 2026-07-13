#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "$0")" && pwd)/common.sh"

require_command python3

STAGE="${1:-}"
[[ -n "$STAGE" ]] || die "usage: audit-ffmpeg.sh <ffmpeg-stage>"
FFMPEG="$STAGE/bin/ffmpeg"
MANIFEST="$STAGE/provenance.json"
[[ -x "$FFMPEG" && -f "$MANIFEST" ]] || die "FFmpeg stage is missing its binary or provenance.json"

[[ "$(plutil -extract ffmpeg_version raw "$MANIFEST")" == "8.1.2" ]] || die "unexpected FFmpeg version"
[[ "$(plutil -extract opus_version raw "$MANIFEST")" == "1.6.1" ]] || die "unexpected libopus version"
[[ "$(plutil -extract license_mode raw "$MANIFEST")" == "LGPL" ]] || die "FFmpeg stage is not marked LGPL"
[[ "$(plutil -extract architecture raw "$MANIFEST")" == "arm64" ]] || die "FFmpeg stage is not arm64"
[[ "$(plutil -extract configure_prefix raw "$MANIFEST")" == "/fozmo" ]] || die "FFmpeg configure prefix is not neutral"
[[ "$(plutil -extract path_staging raw "$MANIFEST")" == "neutral configure prefixes with DESTDIR" ]] \
  || die "FFmpeg path-staging provenance is missing"
[[ "$(plutil -extract gpl_enabled raw "$MANIFEST")" == "false" ]] || die "GPL FFmpeg build rejected"
[[ "$(plutil -extract nonfree_enabled raw "$MANIFEST")" == "false" ]] || die "nonfree FFmpeg build rejected"
[[ "$(plutil -extract network_enabled raw "$MANIFEST")" == "false" ]] || die "network-enabled FFmpeg build rejected"

CONFIGURATION="$($FFMPEG -hide_banner -buildconf 2>&1)"
! grep -q -- '--enable-gpl\|--enable-nonfree\|--enable-network' <<<"$CONFIGURATION" || die "forbidden FFmpeg configuration"
file "$FFMPEG" | grep -q 'arm64' || die "FFmpeg is not arm64"
$FFMPEG -hide_banner -encoders 2>/dev/null | grep -q 'libopus' || die "libopus encoder missing"
if otool -L "$FFMPEG" | tail -n +2 | grep -vE '^\s+(/usr/lib/|/System/Library/)' | grep -q .; then
  die "FFmpeg has non-system dynamic linkage"
fi

python3 - "$FFMPEG" "$ROOT_DIR" "$BUILD_DIR" <<'PY'
import os
import pathlib
import sys

binary = pathlib.Path(sys.argv[1]).read_bytes()
needles = [
    b"/Users/",
    b"/home/runner/work/",
    b"/home/runner/_temp/",
    b"/private/var/folders/",
    b"/var/folders/",
    b"/Applications/Xcode",
    b"/Library/Developer/",
]
for value in [sys.argv[2], sys.argv[3], *(os.environ.get(name, "") for name in ("HOME", "GITHUB_WORKSPACE", "RUNNER_TEMP", "TMPDIR"))]:
    value = value.rstrip("/")
    if value.startswith("/"):
        needles.append(value.encode("utf-8"))
if any(needle and needle in binary for needle in needles):
    raise SystemExit("FFmpeg contains an absolute developer-tool, user, workspace, or runner temporary path")
PY

note "FFmpeg provenance and linkage checks passed"
