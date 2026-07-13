#!/usr/bin/env bash
set -euo pipefail

driver_path="/Library/Audio/Plug-Ins/HAL/FozmoCapture.driver"
if [[ -d "${driver_path}" ]]; then
  echo "Removing ${driver_path}"
  sudo rm -rf "${driver_path}"
else
  echo "No Fozmo Capture driver found at ${driver_path}"
fi
