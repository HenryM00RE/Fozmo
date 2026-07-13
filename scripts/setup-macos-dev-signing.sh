#!/bin/sh
# One-time setup for stable macOS dev code signing.
#
# Creates a self-signed code-signing certificate in the login keychain and
# trusts it for code signing, so dev builds re-signed with it (see
# scripts/macos-sign-and-run.sh) keep their Keychain access across rebuilds.
#
# Interactive: macOS asks for your login password to confirm the trust
# settings and to authorize codesign's key access. Run it in a local
# terminal session on the dev machine, not over plain SSH.
#
# If this script fails on your macOS version, the GUI fallback is:
#   Keychain Access > Certificate Assistant > Create a Certificate...
#   Name: fozmo-dev, Identity Type: Self-Signed Root,
#   Certificate Type: Code Signing.
set -eu

identity="${FOZMO_DEV_SIGN_IDENTITY:-fozmo-dev}"
keychain="$HOME/Library/Keychains/login.keychain-db"

if security find-identity -v -p codesigning | grep -q "\"$identity\""; then
    echo "Code-signing identity '$identity' already exists; nothing to do."
    exit 0
fi

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT INT TERM

echo "Creating self-signed code-signing certificate '$identity'..."
openssl req -x509 -newkey rsa:2048 -days 3650 -nodes \
    -keyout "$workdir/key.pem" -out "$workdir/cert.pem" \
    -subj "/CN=$identity" \
    -addext "keyUsage=critical,digitalSignature" \
    -addext "extendedKeyUsage=critical,codeSigning" \
    -addext "basicConstraints=critical,CA:FALSE"

# Bundle as PKCS#12 so the key and certificate import as one identity. The
# passphrase only protects the throwaway file in $workdir.
openssl pkcs12 -export -out "$workdir/identity.p12" \
    -inkey "$workdir/key.pem" -in "$workdir/cert.pem" \
    -name "$identity" -passout pass:transit

echo "Importing into the login keychain..."
security import "$workdir/identity.p12" -k "$keychain" -P transit \
    -T /usr/bin/codesign

echo "Trusting the certificate for code signing (macOS will ask to confirm)..."
security add-trusted-cert -p codeSign -k "$keychain" "$workdir/cert.pem"

echo "Authorizing codesign to use the key without per-build prompts"
echo "(this asks for your login keychain password)..."
security set-key-partition-list -S "apple-tool:,apple:,codesign:" \
    -s -l "$identity" "$keychain" >/dev/null

echo
echo "Done. Verify with:"
echo "  security find-identity -v -p codesigning | grep $identity"
echo
echo "The cargo runner (scripts/macos-sign-and-run.sh) now signs dev builds"
echo "automatically. The next signed build triggers one final Keychain"
echo "prompt for the secrets bundle; answer 'Always Allow' and rebuilds"
echo "will never prompt again."
