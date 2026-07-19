#!/usr/bin/env bash
set -euo pipefail

repo=/mnt/c/Users/Henry/OneDrive/Documents/GitHub/Fozmo
work=$repo/tools/split_phase_v5/work-spe-e2-targeted-v2-20260719
python=/root/Fozmo/.venv-split-phase-d/bin/python

mkdir -p "$work"
printf '%s\n' "$$" > "$work/e2_wsl_pid.txt"

cd "$repo"
exec nice -n 10 \
  taskset -c 0-2 \
  ionice -c 3 \
  env PYTHONUNBUFFERED=1 \
      OPENBLAS_NUM_THREADS=3 \
      OMP_NUM_THREADS=3 \
      MKL_NUM_THREADS=3 \
      NUMEXPR_NUM_THREADS=3 \
  "$python" -m tools.split_phase_v5.e2_targeted_search \
    --root "$repo" \
    --source-dir "$repo/tools/split_phase_v5/work-spe-direct-factor" \
    --work-dir "$work" \
    --proxy-budget 96 \
    --finalists 3 \
    --lawson-iterations 8 \
    --formal-fft-len 4194304 \
    --formal-iterations 4 \
    > "$work/e2.stdout.log" \
    2> "$work/e2.stderr.log"
