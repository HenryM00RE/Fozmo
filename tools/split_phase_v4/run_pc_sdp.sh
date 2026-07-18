#!/usr/bin/env bash
set -euo pipefail

backend="${1:-mkl}"
accuracy="${2:-initial}"
case "$backend" in
  mkl|gpu|indirect|direct) ;;
  *) echo "usage: $0 [mkl|gpu|indirect|direct] [initial|strict]" >&2; exit 2 ;;
esac
case "$accuracy" in
  initial|strict) ;;
  *) echo "usage: $0 [mkl|gpu|indirect|direct] [initial|strict]" >&2; exit 2 ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

python_bin="${SPLIT_PHASE_D_PYTHON:-$repo_root/.venv-split-phase-d/bin/python}"
if [[ ! -x "$python_bin" ]]; then
  echo "missing Python environment: $python_bin" >&2
  echo "follow tools/split_phase_v4/PC_WSL.md first" >&2
  exit 2
fi

export OMP_NUM_THREADS="${OMP_NUM_THREADS:-8}"
export MKL_NUM_THREADS="${MKL_NUM_THREADS:-8}"
export OPENBLAS_NUM_THREADS="${OPENBLAS_NUM_THREADS:-1}"
export NUMEXPR_NUM_THREADS="${NUMEXPR_NUM_THREADS:-8}"

work_dir="${SPLIT_PHASE_D_WORK_DIR:-$repo_root/tools/split_phase_v4/work-pc-$backend}"
mkdir -p "$work_dir"

"$python_bin" - <<'PY'
import cvxpy as cp
import scs
print("cvxpy", cp.__version__)
print("scs", scs.__version__)
print("installed solvers", cp.installed_solvers())
PY

if [[ "$backend" == "gpu" ]]; then
  command -v nvidia-smi >/dev/null
  nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv,noheader
fi

exec "$python_bin" -m tools.split_phase_v4.magnitude_sdp \
  --order 512 \
  --solver SCS \
  --scs-backend "$backend" \
  --scs-accuracy "$accuracy" \
  --verification-fft-len 8388608 \
  --exchange-rounds 10 \
  --work-dir "$work_dir"
