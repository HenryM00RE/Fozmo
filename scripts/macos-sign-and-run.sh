#!/bin/sh
# Cargo runner for macOS (wired up in .cargo/config.toml).
#
# Debug binaries are ad-hoc signed with a code hash that changes on every
# rebuild, so macOS Keychain "Always Allow" decisions never survive a rebuild
# and each build re-prompts for keychain access. Re-signing with a stable
# local identity and a stable identifier gives every build (including test
# binaries) the same designated requirement, so one "Always Allow" answer
# lasts across rebuilds. Run scripts/setup-macos-dev-signing.sh once to
# create the identity; without it this script just runs the binary as-is.
set -eu

identity="${FOZMO_DEV_SIGN_IDENTITY:-fozmo-dev}"
binary="$1"
shift

if security find-identity -v -p codesigning 2>/dev/null | grep -q "\"$identity\""; then
    if ! output=$(codesign --force --sign "$identity" \
        --identifier com.fozmo.dev "$binary" 2>&1); then
        echo "warning: codesign with '$identity' failed; running unsigned: $output" >&2
    fi
fi

exec "$binary" "$@"
