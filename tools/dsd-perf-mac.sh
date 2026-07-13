#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

profile="${DSD_PERF_PROFILE:-release}"
out_dir="${DSD_PERF_OUT:-audio_tests/out/dsd-perf-mac-$(date +%Y%m%d-%H%M%S)}"
native="${DSD_PERF_NATIVE:-1}"

if [[ "$profile" != "release" && "$profile" != "profiling" ]]; then
  echo "DSD_PERF_PROFILE must be 'release' or 'profiling'" >&2
  exit 2
fi

mkdir -p "$out_dir"

if [[ "$native" == "1" ]]; then
  export RUSTFLAGS="${RUSTFLAGS:-} -C target-cpu=native"
fi

{
  echo "timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "profile=$profile"
  echo "native=$native"
  echo "rustc=$(rustc --version)"
  echo "cargo=$(cargo --version)"
  echo "rustflags=${RUSTFLAGS:-}"
  if command -v sw_vers >/dev/null 2>&1; then
    sw_vers | sed 's/^/macos_/'
  fi
  if command -v sysctl >/dev/null 2>&1; then
    sysctl -n machdep.cpu.brand_string 2>/dev/null | sed 's/^/cpu=/'
    sysctl -n hw.memsize 2>/dev/null | sed 's/^/mem_bytes=/'
  fi
} | tee "$out_dir/metadata.txt"

cargo_args=(--release)
if [[ "$profile" == "profiling" ]]; then
  cargo_args=(--profile profiling)
fi

run_and_log() {
  local name="$1"
  shift
  echo "==> $name"
  "$@" 2>&1 | tee "$out_dir/$name.txt"
}

run_and_log dsd_renderer_production \
  cargo run "${cargo_args[@]}" --bin dsd_renderer_bench

run_and_log dsd_modulator_production \
  cargo run "${cargo_args[@]}" --bin dsd_modulator_bench

run_and_log resampler_production \
  cargo run "${cargo_args[@]}" --bin resampler_bench

echo "DSD perf logs written to $out_dir"
