#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
build_dir="${root_dir}/build"
bundle_dir="${build_dir}/FozmoCapture.driver"
contents_dir="${bundle_dir}/Contents"
macos_dir="${contents_dir}/MacOS"

rm -rf "${bundle_dir}"
mkdir -p "${macos_dir}"

sdk_path="$(xcrun --sdk macosx --show-sdk-path)"
cxx="${CXX:-clang++}"

"${cxx}" \
  -std=c++17 \
  -O2 \
  -Wall \
  -Wextra \
  -Wpedantic \
  -fvisibility=hidden \
  -dynamiclib \
  -install_name "@loader_path/FozmoCapture" \
  -isysroot "${sdk_path}" \
  -framework CoreAudio \
  -framework CoreFoundation \
  "${root_dir}/src/FozmoCapture.cpp" \
  -o "${macos_dir}/FozmoCapture"

cp "${root_dir}/Info.plist" "${contents_dir}/Info.plist"

codesign_identity="${CODESIGN_IDENTITY:-}"
if [[ -z "${codesign_identity}" ]]; then
  codesign_identity="$(
    security find-identity -v -p codesigning 2>/dev/null \
      | awk -F '"' '/Apple Development|Developer ID Application|Mac Developer/ { print $2; exit }'
  )"
fi

if [[ -n "${codesign_identity}" ]]; then
  echo "Signing with ${codesign_identity}"
  codesign --force --timestamp=none --sign "${codesign_identity}" "${bundle_dir}" >/dev/null
else
  echo "WARNING: no valid code-signing identity found; using ad-hoc signing."
  echo "         Recent macOS builds may load but refuse to publish ad-hoc HAL drivers."
  codesign --force --sign - "${bundle_dir}" >/dev/null
fi

echo "Built ${bundle_dir}"
