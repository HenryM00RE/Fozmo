#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

release_native_rustflags() {
  if [[ -n "${CI:-}" ]]; then
    printf "%s" "${RUSTFLAGS:-}"
    return
  fi
  if [[ "${RUSTFLAGS:-}" == *"target-cpu="* ]]; then
    printf "%s" "$RUSTFLAGS"
  else
    printf "%s" "${RUSTFLAGS:+$RUSTFLAGS }-C target-cpu=native"
  fi
}

AUDIO_RUSTFLAGS="$(release_native_rustflags)"

echo "==> Checking architecture boundaries"
python3 tools/check_architecture_boundaries.py

echo "==> Checking frontend lint and formatting"
npm --prefix ui run check

echo "==> Running frontend tests"
npm --prefix ui test

echo "==> Building frontend and checking generated snapshot"
./tools/check-frontend-snapshot.sh

echo "==> Auditing frontend dependencies"
npm --prefix ui run audit

echo "==> Checking frontend production dependency licenses"
npm --prefix ui run license:check

echo "==> Checking Rust formatting"
cargo fmt -- --check
cargo fmt --manifest-path airplay-helper/Cargo.toml -- --check

echo "==> Running Rust clippy"
./tools/clippy.sh --all-targets

echo "==> Running Rust clippy without default features"
./tools/clippy.sh --all-targets --no-default-features

echo "==> Running Rust clippy with Qobuz/local playback features"
./tools/clippy.sh --all-targets --no-default-features --features qobuz,pcm_output,local_library

echo "==> Running Rust clippy with network/control integration features"
./tools/clippy.sh --all-targets --no-default-features --features hegel,sonos,upnp

echo "==> Checking Rust targets"
cargo check --all-targets

echo "==> Checking Rust targets without default features"
cargo check --all-targets --no-default-features

echo "==> Checking Rust targets with the standalone AirPlay helper client"
cargo check --all-targets --no-default-features --features airplay_helper

echo "==> Checking the standalone GPL AirPlay helper"
cargo check --manifest-path airplay-helper/Cargo.toml --all-targets --locked
cargo test --manifest-path airplay-helper/Cargo.toml --locked

echo "==> Checking the MIT/GPL process and dependency boundary"
./tools/check-release-boundaries.sh

echo "==> Checking Rust dependency policy"
cargo deny check
cargo deny --manifest-path airplay-helper/Cargo.toml \
  --config airplay-helper/deny.toml check

echo "==> Running release Rust library and binary tests"
RUSTFLAGS="$AUDIO_RUSTFLAGS" cargo test --release --lib --bins

echo "==> Running release Rust library and binary tests without default features"
RUSTFLAGS="$AUDIO_RUSTFLAGS" cargo test --release --lib --bins --no-default-features

echo "==> Running release audio smoke checks"
RUSTFLAGS="$AUDIO_RUSTFLAGS" cargo test --release --test audio_smoke

echo "==> Running EcBeam2 campaign unit tests"
PYTHONPATH=tools python3 -m unittest tools/test_dsd64_ecbeam2_experiment.py
