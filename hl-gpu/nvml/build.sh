#!/usr/bin/env bash
# Cross-build dd's libnvidia-ml.so.1 NVML shim for Linux guest arches, and run the
# dlopen ABI test natively. MUST run Linux-side (cross-gcc is Linux-only) — see the
# repo memory note "matrix runs linux-side".
#
#   ./build.sh            # build aarch64 + x86_64 .so into ./out/<arch>/ and run the native test
#   ./build.sh install    # also copy the built .so into ~/.hl/nvml/<arch>/ (where the launcher looks)
set -euo pipefail
cd "$(dirname "$0")"
OUT="out"
CFLAGS="-O2 -fPIC -shared -Wall -Wextra -fvisibility=default -Wl,-soname,libnvidia-ml.so.1"

build() { # <arch> <cc>
  local arch="$1" cc="$2"
  mkdir -p "$OUT/$arch"
  echo "[build] $arch via $cc"
  "$cc" $CFLAGS nvml_shim.c -o "$OUT/$arch/libnvidia-ml.so.1"
  # A convenience unversioned symlink some loaders/ldconfig setups want.
  ln -sf libnvidia-ml.so.1 "$OUT/$arch/libnvidia-ml.so"
}

build aarch64 aarch64-linux-gnu-gcc
build x86_64  x86_64-linux-gnu-gcc

# Native ABI test: build a native shim + harness (host is aarch64 or x86_64 Linux) and run it.
HOST_ARCH="$(uname -m)"
case "$HOST_ARCH" in
  aarch64|arm64) HOST_ARCH=aarch64 ;;
  x86_64|amd64)  HOST_ARCH=x86_64 ;;
esac
echo "[test] native ABI test ($HOST_ARCH)"
gcc -O2 -fPIC -shared -o "$OUT/native-libnvidia-ml.so.1" nvml_shim.c
gcc -O2 -o "$OUT/test_nvml" test_nvml.c -ldl
HL_CUDA_NAME="${HL_CUDA_NAME:-Tesla hl-Metal 4C}" HL_CUDA_CC="${HL_CUDA_CC:-8.9}" HL_CUDA_VRAM="${HL_CUDA_VRAM:-16384}" \
  "$OUT/test_nvml" "$OUT/native-libnvidia-ml.so.1"

if [ "${1:-}" = "install" ]; then
  for arch in aarch64 x86_64; do
    dst="$HOME/.hl/nvml/$arch"
    mkdir -p "$dst"
    cp "$OUT/$arch/libnvidia-ml.so.1" "$dst/"
    ln -sf libnvidia-ml.so.1 "$dst/libnvidia-ml.so"
    echo "[install] $dst/libnvidia-ml.so.1"
  done
fi
echo "[done]"
