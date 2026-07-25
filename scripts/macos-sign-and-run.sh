#!/bin/sh
# Cargo runner for macOS (wired up in .cargo/config.toml).
#
# Debug binaries are ad-hoc signed with a code hash that changes on every
# rebuild, so macOS Keychain and TCC decisions never survive a rebuild.
# Re-signing with a stable identity and bundle identifier gives every build
# the same designated requirement. Prefer the local fozmo-dev identity when
# present, otherwise use the first installed Apple Development identity.
set -eu

binary="$1"
shift

identities="$(security find-identity -v -p codesigning 2>/dev/null || true)"
identity=""

if [ -n "${FOZMO_DEV_SIGN_IDENTITY:-}" ]; then
    if printf '%s\n' "$identities" | grep -Fq "$FOZMO_DEV_SIGN_IDENTITY"; then
        identity="$FOZMO_DEV_SIGN_IDENTITY"
    else
        echo "warning: codesign identity '$FOZMO_DEV_SIGN_IDENTITY' was not found; running unsigned" >&2
    fi
elif printf '%s\n' "$identities" | grep -Fq '"fozmo-dev"'; then
    identity="fozmo-dev"
else
    identity="$(
        printf '%s\n' "$identities" |
            sed -n 's/^[[:space:]]*[0-9][0-9]*) \([0-9A-F][0-9A-F]*\) "Apple Development:.*$/\1/p' |
            head -n 1
    )"
fi

if [ -n "$identity" ]; then
    if ! output=$(codesign --force --sign "$identity" \
        --identifier com.fozmo.server --timestamp=none "$binary" 2>&1); then
        echo "warning: codesign with '$identity' failed; running unsigned: $output" >&2
    fi
fi

exec "$binary" "$@"
