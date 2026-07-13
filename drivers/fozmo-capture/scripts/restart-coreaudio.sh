#!/usr/bin/env bash
set -euo pipefail

echo "Restarting CoreAudio"
sudo killall coreaudiod || true
