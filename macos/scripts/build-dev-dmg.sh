#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

export FOZMO_BUILD_MODE=dev
"$SCRIPT_DIR/build-app.sh"
"$SCRIPT_DIR/make-dmg.sh"
