#!/usr/bin/env bash
# Cross-build dd's libcuda.so.1 (CUDA Driver-API) + libcudart.so (CUDA Runtime-API, layered on it) for
# the Linux guest arches, and run the ABI tests natively. MUST run Linux-side (cross-gcc is Linux-only)
# — see the repo memory note "matrix runs linux-side". Mirrors ../../dd-nvml/build.sh.
#
#   ./build.sh            # build aarch64 + x86_64 .so into ./out/<arch>/ and run the native ABI tests
#   ./build.sh install    # also copy the built .so into ~/.dd/cuda/<arch>/ (where the launcher looks)
set -euo pipefail
cd "$(dirname "$0")"
OUT="out"
# -Wl,--version-script hides internal helpers and exports only the public surface, matching real
# libcuda/libcudart symbol visibility. Anonymous version node => no @@VERSION tags.
CU_CFLAGS="-O2 -fPIC -shared -Wall -Wextra -fvisibility=default -Wl,-soname,libcuda.so.1 -Wl,--version-script,libcuda.map"
RT_CFLAGS="-O2 -fPIC -shared -Wall -Wextra -fvisibility=default -Wl,-soname,libcudart.so.1 -Wl,--version-script,libcudart.map -Wl,-rpath,\$ORIGIN"

build() { # <arch> <cc>
  local arch="$1" cc="$2"
  mkdir -p "$OUT/$arch"
  echo "[build] $arch via $cc"
  # driver shim
  "$cc" $CU_CFLAGS cuda_shim.c -o "$OUT/$arch/libcuda.so.1" -lm  # -lm: fmaf (fma.rn.f32) on x86_64
  ln -sf libcuda.so.1 "$OUT/$arch/libcuda.so"
  # runtime shim: DT_NEEDED libcuda.so.1, resolved beside it via rpath $ORIGIN
  "$cc" $RT_CFLAGS cudart_shim.c -o "$OUT/$arch/libcudart.so.1" -L"$OUT/$arch" -l:libcuda.so.1 -lm
  ln -sf libcudart.so.1 "$OUT/$arch/libcudart.so"
}

build aarch64 aarch64-linux-gnu-gcc
build x86_64  x86_64-linux-gnu-gcc

# Native ABI tests: build native shims + harnesses (host is aarch64 or x86_64 Linux) and run them.
HOST_ARCH="$(uname -m)"
case "$HOST_ARCH" in
  aarch64|arm64) HOST_ARCH=aarch64 ;;
  x86_64|amd64)  HOST_ARCH=x86_64 ;;
esac
mkdir -p "$OUT/native"
gcc -O2 -fPIC -shared -Wl,-soname,libcuda.so.1 -Wl,--version-script,libcuda.map \
    -o "$OUT/native/libcuda.so.1" cuda_shim.c -lm
gcc -O2 -fPIC -shared -Wl,-soname,libcudart.so.1 -Wl,--version-script,libcudart.map -Wl,-rpath,'$ORIGIN' \
    -o "$OUT/native/libcudart.so.1" cudart_shim.c -L"$OUT/native" -l:libcuda.so.1 -lm
ln -sf libcuda.so.1   "$OUT/native/libcuda.so"
ln -sf libcudart.so.1 "$OUT/native/libcudart.so"

export DD_CUDA_NAME="${DD_CUDA_NAME:-dd Metal (CUDA-sim) Device}"
export DD_CUDA_CC="${DD_CUDA_CC:-8.6}"
export DD_CUDA_VRAM="${DD_CUDA_VRAM:-4096}"

echo "[test] native driver-API ABI test ($HOST_ARCH)"
gcc -O2 -o "$OUT/test_cuda" test_cuda.c -ldl -lm
"$OUT/test_cuda" "$OUT/native/libcuda.so.1"

# The cudart harness links directly against libcudart (as a real app does); rpath resolves the whole
# chain (test -> libcudart.so.1 -> libcuda.so.1) at load time, proving the .so is load-clean.
echo "[test] native runtime-API (cudart) ABI test ($HOST_ARCH)"
gcc -O2 -o "$OUT/test_cudart" test_cudart.c -L"$OUT/native" -l:libcudart.so.1 \
    -Wl,-rpath,"\$ORIGIN/native" -ldl -lm
"$OUT/test_cudart"

if [ "${1:-}" = "install" ]; then
  for arch in aarch64 x86_64; do
    dst="$HOME/.dd/cuda/$arch"
    mkdir -p "$dst"
    cp "$OUT/$arch/libcuda.so.1"   "$dst/"; ln -sf libcuda.so.1   "$dst/libcuda.so"
    cp "$OUT/$arch/libcudart.so.1" "$dst/"; ln -sf libcudart.so.1 "$dst/libcudart.so"
    echo "[install] $dst/{libcuda,libcudart}.so.1"
  done
fi
echo "[done]"
