#!/usr/bin/env bash
set -euo pipefail

echo "Looking for Fozmo Capture CoreAudio device"
if system_profiler SPAudioDataType | grep -A 12 -B 2 "Fozmo Capture"; then
  exit 0
fi

if [[ -d "/Library/Audio/Plug-Ins/HAL/FozmoCapture.driver" ]]; then
  echo "Driver bundle is installed, but the device is not visible yet."
  executable="/Library/Audio/Plug-Ins/HAL/FozmoCapture.driver/Contents/MacOS/FozmoCapture"
  if ! spctl --assess --type execute "${executable}" >/dev/null 2>&1; then
    echo "macOS system policy currently rejects the driver executable."
    echo "Sign it with an Apple Development or Developer ID certificate, then reinstall:"
    echo "  CODESIGN_IDENTITY=\"Apple Development: Your Name (...)\" drivers/fozmo-capture/scripts/install.sh"
  fi
  echo "Try restarting CoreAudio again or opening Audio MIDI Setup."
else
  echo "Fozmo Capture was not found."
fi
exit 1
