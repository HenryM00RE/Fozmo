# AirPlay helper dependency policy

The helper has a separate Cargo graph because it is an independently built
GPL-2.0-only program. Run its policy check from the repository root:

```sh
cargo deny --manifest-path airplay-helper/Cargo.toml check \
  --config airplay-helper/deny.toml
```

The policy permits crates.io plus the `airplay2-rs` repository at a required
full `rev` specifier. The manifest and lockfile pin that repository to
`1baeaae336ca3a9828e732500082f5fd1767d2fd`. It also denies the optional FDK
AAC crates so an accidental feature change cannot add them to the distributed
helper; release builds use ALAC.

Several crates at that upstream revision omit `license.workspace = true` from
their individual manifests even though the workspace declares GPL-2.0 and
ships the GPLv2 text at its root. The helper policy clarifies those packages as
GPL-2.0-only against the hash of that exact root license file. A source or
license-text change invalidates the clarification instead of silently carrying
it forward.

## `RUSTSEC-2023-0071` decision

The resolved graph contains `rsa 0.9.10`, for which no fixed release is
available. RustSec describes a timing side channel in RSA **private-key**
operations that can permit private-key recovery.

This helper does not hold or operate on an RSA private key. Its direct legacy
RAOP path uses `RsaPublicKey::encrypt` to encrypt a freshly generated AES key
for the receiver. The pinned `airplay2-rs` graph likewise uses the `rsa` crate
only for `RsaPublicKey` encryption of an AES key. Pairing signatures use
Ed25519, and authenticated channel encryption uses symmetric ciphers.

The advisory is therefore ignored for this exact graph and use case rather
than hidden globally. The waiver is fail-closed: cargo-deny rejects it once the
advisory no longer applies, prompting its removal. Re-audit the RSA call sites
whenever `rsa`, the helper, or the pinned AirPlay source revision changes. If
an RSA private-key operation is introduced, this waiver is invalid and release
must stop until the dependency is replaced or fixed.

References:

- <https://rustsec.org/advisories/RUSTSEC-2023-0071>
- <https://github.com/RustCrypto/RSA/issues/626>
