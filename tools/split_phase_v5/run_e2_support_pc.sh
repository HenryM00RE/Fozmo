#!/usr/bin/env bash
set -euo pipefail

repo=/mnt/c/Users/Henry/OneDrive/Documents/GitHub/Fozmo
work=$repo/tools/split_phase_v5/work-spe-e2-support-48-20260719
python=/root/Fozmo/.venv-split-phase-d/bin/python

mkdir -p "$work"
printf '%s\n' "$$" > "$work/e2_support_wsl_pid.txt"

cd "$repo"
exec nice -n 10 \
  taskset -c 0-2 \
  ionice -c 3 \
  env PYTHONUNBUFFERED=1 \
      OPENBLAS_NUM_THREADS=3 \
      OMP_NUM_THREADS=3 \
      MKL_NUM_THREADS=3 \
      NUMEXPR_NUM_THREADS=3 \
  "$python" -m tools.split_phase_v5.e2_support_refine \
    --root "$repo" \
    --source-dir "$repo/tools/split_phase_v5/work-spe-direct-factor" \
    --proxy-work-dir "$repo/tools/split_phase_v5/work-spe-e2-targeted-v2-20260719" \
    --work-dir "$work" \
    --candidate-index 48 \
    --iterations 16 \
    > "$work/e2_support.stdout.log" \
    2> "$work/e2_support.stderr.log"
