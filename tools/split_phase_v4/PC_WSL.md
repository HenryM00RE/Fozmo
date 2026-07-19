# Split Phase D PC race

Run the PC solve alongside the Mac solve. Do not stop the Mac process. Each
backend uses its own work directory, and no candidate is usable until the same
independent audit accepts it.

The selected PC mode is one CUDA GPU-indirect solve. Do not also launch MKL.
MKL remains a fallback only if the CUDA-enabled SCS extension is unavailable.

## WSL setup

Use Ubuntu under WSL2. Give WSL at least 56 GB of memory and 16 GB of swap in
`%UserProfile%\\.wslconfig`, then run `wsl --shutdown` from PowerShell after
changing it:

```ini
[wsl2]
memory=56GB
swap=16GB
processors=16
```

Inside Ubuntu, from the transferred repository root:

```sh
sudo apt update
sudo apt install -y build-essential git python3-venv python3-dev pkg-config
python3 -m venv .venv-split-phase-d
.venv-split-phase-d/bin/python -m pip install --upgrade pip
.venv-split-phase-d/bin/python -m pip install -r tools/split_phase_v4/requirements.lock
```

The Linux x86-64 SCS wheel includes its MKL backend. Use this only as the CPU
fallback if CUDA setup fails:

```sh
chmod +x tools/split_phase_v4/run_pc_sdp.sh
OMP_NUM_THREADS=8 MKL_NUM_THREADS=8 \
  tools/split_phase_v4/run_pc_sdp.sh mkl initial
```

It writes only to `tools/split_phase_v4/work-pc-mkl`.

## Primary 4080 Super run

The GPU path requires an SCS build containing the GPU indirect backend and a
working CUDA toolkit inside WSL (`nvidia-smi` and `nvcc` must both work). The
SCS 3.2.11 release needs the checked-in build-only Meson fix to order its GPU
target after the common sources and link cuBLAS/cuSPARSE explicitly:

```sh
git clone --recursive https://github.com/bodono/scs-python.git /tmp/scs-python-gpu
cd /tmp/scs-python-gpu
git checkout 3.2.11
git submodule update --init --recursive
git apply /path/to/Fozmo/tools/split_phase_v4/scs-3.2.11-gpu-meson.patch
/path/to/Fozmo/.venv-split-phase-d/bin/python -m pip install meson-python
CUDA_PATH=/usr/local/cuda \
  /path/to/Fozmo/.venv-split-phase-d/bin/python -m pip install \
  --no-build-isolation --no-deps --force-reinstall . \
  --config-settings=setup-args=-Duse_gpu=true \
  --config-settings=setup-args=-Dint32=true \
  --config-settings=setup-args=-Dgpu_atrans=true
cd /path/to/Fozmo
```

Then run:

```sh
SPLIT_PHASE_D_WORK_DIR="$PWD/tools/split_phase_v4/work-pc-gpu" \
  tools/split_phase_v4/run_pc_sdp.sh gpu
```

The runner checkpoints every 1,000 SCS iterations. To continue an interrupted
directory after verifying the machine and CUDA backend are healthy:

```sh
SPLIT_PHASE_D_WORK_DIR="$PWD/tools/split_phase_v4/work-pc-gpu" \
SPLIT_PHASE_D_RESUME=1 \
  tools/split_phase_v4/run_pc_sdp.sh gpu initial
```

Never pass `SPLIT_PHASE_D_RESUME=1` for a different work directory or changed
solver configuration. Resume refuses mismatched generator, dependency lock,
specification, backend, accuracy profile, checkpoint interval, file hashes or
array hashes.

Do not run MKL simultaneously. GPU indirect is the selected PC run; MKL is
retained only as the dependable fallback.

The `initial` profile matches the Mac race: SCS may return an inaccurate status,
but the candidate is still rejected unless every independent dense, PSD and
high-precision check passes. If it fails, preserve that work directory and rerun
the same backend with `strict` in a new directory.

## Returning a candidate

Copy the completed `work-pc-mkl` or `work-pc-gpu` directory back to the Mac
without replacing `tools/split_phase_v4/work`. Re-audit it on the Mac with:

```sh
python -m tools.split_phase_v4.magnitude_sdp \
  --order 512 \
  --verification-fft-len 8388608 \
  --exchange-rounds 10 \
  --work-dir /path/to/returned-work-directory \
  --audit-existing
```

Only an accepted audit can enter the Split Phase D production build.
