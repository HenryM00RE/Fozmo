# macOS Keychain Prompts And Dev Code Signing

How the app keeps macOS Keychain access prompts down to a single dialog, and
how to make that dialog a one-time event on a dev machine.

## Why prompts happened at all

Secrets (Last.fm API key, Qobuz session, pairing token records, remote TLS
key) live in the macOS keychain under the `com.fozmo.secrets` service. Two
compounding problems used to make prompts frequent and unpredictable:

1. **One keychain item per secret.** Keychain access control is per item, so
   a boot that touched four items raised four separate prompts.
2. **Unstable dev code signature.** Cargo debug builds are ad-hoc signed and
   their code hash changes on every rebuild. The keychain identifies "the
   app" by that signature, so *Always Allow* never survived a rebuild —
   every build re-prompted for every item.

Prompt timing was also lazy: the remote TLS key was only read when the remote
listener started, so a prompt could appear long after launch, and a dismissed
prompt fail-closed the remote listener until restart.

## What the app does now

- **Single bundle item.** All secrets are stored as one JSON bundle in one
  keychain item (`secrets-bundle-v1`), so there is at most one prompt per app
  identity. On first run after upgrade, the old per-secret items are folded
  into the bundle and deleted (this is the last multi-prompt boot).
- **Eager warm-up at boot.** `build_app_state` loads the bundle before any
  listener starts, so the prompt (if any) always appears at launch. A
  dismissed prompt is not cached: the next feature that needs a secret
  retries (and re-prompts) instead of staying broken until restart.

## One-time dev machine setup

To make *Always Allow* stick across rebuilds, dev binaries are re-signed with
a stable local identity by the cargo runner in `.cargo/config.toml`
(`scripts/macos-sign-and-run.sh`). Without the identity the runner is a
transparent pass-through, so nothing breaks on machines that skip this.

Run once, in a local terminal on the dev machine:

```sh
scripts/setup-macos-dev-signing.sh
```

This creates a self-signed `fozmo-dev` code-signing certificate in the login
keychain, trusts it for code signing, and authorizes `codesign` to use it
without per-build prompts. The next `cargo run` signs the binary with a
stable identifier (`com.fozmo.dev`); answer *Always Allow* on the final
keychain prompt and rebuilds (and test binaries) never prompt again.

Notes:

- Override the identity name with `FOZMO_DEV_SIGN_IDENTITY` (both scripts
  honor it).
- The runner path in `.cargo/config.toml` is relative, so run cargo from the
  repository root.
- If the setup script fails on a newer macOS, create the certificate via
  Keychain Access > Certificate Assistant > Create a Certificate…
  (Name: `fozmo-dev`, Self-Signed Root, Certificate Type: Code Signing).
- Zero-prompt alternative for throwaway work: build with the
  `dev-secrets-file` feature and set `FOZMO_DEV_SECRETS_FILE=1` to keep
  secrets in a plaintext JSON file instead of the keychain. This skips the
  real keychain path entirely, so don't use it when testing secret storage.
