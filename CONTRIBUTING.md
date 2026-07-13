# Contributing to Fozmo

Fozmo is a public pre-alpha. Focused bug fixes, tests, documentation
corrections and narrowly scoped compatibility improvements are welcome. For a
large feature or a change to an audio, security, persistence or licence
boundary, open an issue before investing in an implementation.

By participating, you agree to follow [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
Report suspected vulnerabilities through the private process in
[SECURITY.md](SECURITY.md), not a public issue.

## Start with the architecture

Read [docs/architecture.md](docs/architecture.md) and
[COMPONENTS.md](COMPONENTS.md) before changing ownership boundaries. The
architecture boundary checker is part of CI. In particular, the MIT server and
the standalone GPL AirPlay helper are separate programs joined only by the
documented IPC/PCM boundary.

Keep pull requests reviewable:

- solve one problem per pull request;
- add or update tests for observable behavior;
- update public documentation when behavior or compatibility changes;
- avoid unrelated formatting, generated-output churn or dependency updates;
- identify any real hardware or service used for manual validation without
  publishing account, network or device identifiers.

## Development setup

Use the versions pinned by the repository: Rust 1.96 and Node 22. The packaged
macOS app additionally requires the Xcode version recorded in
[macos/README.md](macos/README.md).

Install the frontend dependencies exactly from its lockfile:

```sh
npm --prefix ui ci
```

Useful fast checks are:

```sh
python3 tools/check_architecture_boundaries.py
cargo fmt -- --check
./tools/clippy.sh --all-targets
npm --prefix ui run check
npm --prefix ui run test
npm --prefix ui run build
```

Run the tests relevant to the changed feature. DSD integration tests must use
the optimized native path:

```sh
RUSTFLAGS="-C target-cpu=native" cargo test --release --test audio_smoke
```

Before requesting review, run `./tools/verify.sh` when practical. It requires
the repository's audit tools and performs the broader release-oriented checks.
If a check cannot be run locally because it requires a platform, service or
device, say so explicitly in the pull request.

## Privacy and test data

Do not commit credentials, tokens, cookies, private keys, local settings,
databases, logs, crash reports, listening history, commercial audio, account
screenshots, personal paths, network addresses or stable hardware identifiers.
Sanitize diagnostics before attaching them to an issue or pull request.

Audio fixtures should be generated from committed manifests or be small files
whose licence and provenance are documented. New fonts, logos, artwork,
presets, source archives and binary dependencies must be added to the relevant
component/licence inventory before they are distributed.

## Contribution licences

This repository is a mixed-license aggregate. A contribution is accepted under
the licence already governing the component it changes:

- the standalone `airplay-helper/` program and its GPL-identified vendored
  code are `GPL-2.0-only`;
- the main Rust server, DSP, browser client, CLI, launcher and IPC schema are
  MIT unless [COMPONENTS.md](COMPONENTS.md) identifies another licence;
- third-party files retain their stated upstream licences.

By opening a pull request, you represent that you have the right to submit the
work and agree that it may be distributed under the applicable component
licence above. Do not copy code, generated tables, assets or documentation from
another project unless its provenance and licence permit inclusion. Changes
that move implementation across the MIT/GPL process boundary require prior
discussion and an update to the component map.

## Pull requests

Complete the pull request template, including the licence boundary, privacy
review and exact automated/manual checks performed. A passing CI run does not
by itself establish compatibility with a DAC, receiver, driver or streaming
service; report those results only when tested on the named class of setup.
