#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
"${root_dir}/scripts/build.sh"

src="${root_dir}/build/FozmoCapture.driver"
dst="/Library/Audio/Plug-Ins/HAL/FozmoCapture.driver"

echo "Installing ${dst}"
sudo rm -rf "${dst}"
sudo cp -R "${src}" "${dst}"
sudo chown -R root:wheel "${dst}"

"${root_dir}/scripts/restart-coreaudio.sh"
"${root_dir}/scripts/diagnose.sh"
