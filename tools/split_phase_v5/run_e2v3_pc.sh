#!/usr/bin/env bash
set -euo pipefail

repo=/mnt/c/Users/Henry/OneDrive/Documents/GitHub/Fozmo
work=$repo/tools/split_phase_v5/work-spe-e2v3-audio-highres-20260719
python=/root/Fozmo/.venv-split-phase-d/bin/python

mkdir -p "$work"
printf '%s\n' "$$" > "$work/e2v3_wsl_pid.txt"

cd "$repo"
exec nice -n 10 \
  taskset -c 0-2 \
  ionice -c 3 \
  env PYTHONUNBUFFERED=1 \
      OPENBLAS_NUM_THREADS=3 \
      OMP_NUM_THREADS=3 \
      MKL_NUM_THREADS=3 \
      NUMEXPR_NUM_THREADS=3 \
  "$python" -m tools.split_phase_v5.e2_v3_highres \
    --root "$repo" \
    --base-e-dir "$repo/tools/split_phase_v5/work-spe-direct-factor" \
    --audio-dir "$repo/tools/split_phase_v5/work-spe-e2-support-2064-20260719" \
    --work-dir "$work" \
    --fft-len 4194304 \
    --iterations 4 \
    > "$work/e2v3.stdout.log" \
    2> "$work/e2v3.stderr.log"
