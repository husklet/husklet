#!/usr/bin/env bash
# Deploy the guest GLES driver into the GUI lib dir, selected by DD_SHIM_IMPL.
#
#   DD_SHIM_IMPL unset / != "rust"  -> no-op: the C shim (gl_shim.c) libs stay in place (the default).
#   DD_SHIM_IMPL=rust               -> build dd-shim-gl's cdylib and install it as the GLES stack:
#                                        libEGL.so.1              (the cdylib; all symbols live here)
#                                        libGLESv2.so.2           (thin DT_NEEDED -> libEGL.so.1 stub)
#                                        libwayland-egl.so.1      (thin DT_NEEDED -> libEGL.so.1 stub)
#
# This is the flagged cutover selector: a validation run sets DD_SHIM_IMPL=rust to swap in the Rust
# driver; the default leaves the C shim untouched until the maintainer flips it after a live check.
#
# Usage:  DD_SHIM_IMPL=rust hl-shim-gl/deploy.sh [ARCH]      # ARCH defaults to the host arch
# Env:    DD_GUI_LIB   override the install dir (default: ~/.dd/gui/<arch>/lib)
#         CC           C compiler for the DT_NEEDED stubs (default: cc; use the cross cc for x86_64)
set -euo pipefail

if [ "${DD_SHIM_IMPL:-}" != "rust" ]; then
    echo "DD_SHIM_IMPL != rust — leaving the default (C shim) GLES libs in place."
    exit 0
fi

ARCH="${1:-$(uname -m)}"
ROOT="$(git rev-parse --show-toplevel)"
LIB="${DD_GUI_LIB:-$HOME/.dd/gui/$ARCH/lib}"
CC="${CC:-cc}"

# The cdylib bakes SONAME libEGL.so.1 (see hl-shim-gl/build.rs). Its output dir honors CARGO_TARGET_DIR.
echo "building dd-shim-gl (Rust GLES driver) for $ARCH ..."
( cd "$ROOT" && cargo build -p hl-shim-gl --release )
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
SO="$TARGET_DIR/release/libdd_shim_gl.so"
[ -f "$SO" ] || { echo "error: cdylib not found at $SO" >&2; exit 1; }

mkdir -p "$LIB"
install -m 0755 "$SO" "$LIB/libEGL.so.1"
# libGLESv2.so.2 / libwayland-egl.so.1 are thin DT_NEEDED -> libEGL.so.1 stubs (empty TU + the lib as
# a linker input via -l:, so the loader pulls libEGL.so.1's symbols when an app links -lGLESv2, etc.).
for stub in libGLESv2.so.2 libwayland-egl.so.1; do
    "$CC" -shared -fPIC -Wl,-soname,"$stub" -Wl,--no-as-needed \
        -o "$LIB/$stub" -xc /dev/null -L"$LIB" -l:libEGL.so.1
done

echo "deployed Rust GLES driver -> $LIB/{libEGL.so.1,libGLESv2.so.2,libwayland-egl.so.1}"
echo "(to revert to the C shim, re-populate the lib dir with the gl_shim.c-built libs.)"
