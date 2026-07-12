#!/usr/bin/env bash
# Coherent chromiumws repair + cold-boot validation recipe for dd-display.
#
# WHAT THIS FIXES
#   After 5e8c10ee ("stop shim shadowing the guest's real libX11 — inject only shim-owned libs")
#   a cold Chrome boot in the `chromiumws` workspace died with EXIT=127: dozens of rootfs libs
#   (chromium, libcairo, libfreetype, libgobject→ffi_prep_cif, libz→uncompress, libgcc_s→_Unwind_*,
#   libasound→snd_*, …) failed to load with "Exec format error", even though the on-disk .so's are
#   valid and export those symbols.
#
# ROOT CAUSE  (this is NOT a bug in 5e8c10ee — that commit is correct)
#   `chromiumws` runs the image `zenika/alpine-chrome:with-node`: a COHERENT all-musl aarch64 rootfs
#   whose chromium is `interpreter /lib/ld-musl-aarch64.so.1` and which already ships its own musl
#   libz/libffi/libstdc++/libgcc_s/libX11/… .  dd's accelerated display injects the GPU/GL render
#   stack (libEGL/libGLESv2/libwayland-*) into the guest multiarch dir `/usr/lib/aarch64-linux-gnu`
#   and PREPENDS that dir to LD_LIBRARY_PATH (dd-gpu/src/integration.rs, `shim_owns_lib`).
#
#   The workspace OVERLAY UPPER (~/.dd/workspaces/chromiumws/upper/usr/lib/aarch64-linux-gnu/) had
#   accumulated, from the PRE-5e8c10ee "inject EVERY *.so" era, a full set of ZERO-BYTE placeholder
#   files — the bind-mount targets for libz.so.1, libffi.so.8, libstdc++.so.6, libgcc_s.so.1,
#   libX11.so.6, libXext.so.6, libasound.so.2, libpulse.so.0, libpng16.so.16, libxkbcommon.so.0, …
#   Back then every stub was covered at launch by a real bind-mount, so they were invisible.
#
#   5e8c10ee (correctly) stopped bind-mounting the base/distro libs — but the STALE 0-byte stubs
#   remained in the overlay.  Because the injection dir is first on LD_LIBRARY_PATH, musl's loader
#   now finds the 0-byte libz.so.1 (etc.) BEFORE the guest's real /usr/lib/libz.so.1 and returns
#   ENOEXEC ("Exec format error") — a 0-byte file is not a valid ELF.  Every downstream
#   "symbol not found" (ffi_prep_cif, _Unwind_Backtrace, uncompress, snd_*) cascades from that.
#   The libs that DO load — libEGL/libGLESv2/libwayland-* — are exactly the ones still bind-mounted.
#
# THE FIX (coherent workspace; no hack, no distro-lib re-injection, no revert of 5e8c10ee)
#   Prune the orphaned 0-byte injection stubs from the overlay so the Alpine musl guest resolves its
#   OWN musl base libs, while dd's musl render shims are still injected over their (covered) stubs.
#   This realizes 5e8c10ee's intent: a coherent all-musl guest that needs NO base-lib injection.
#   New workspaces created after 5e8c10ee never accumulate these base-lib stubs (dd only creates a
#   target for a lib it actually mounts), so this is a one-time repair of legacy overlay state.
#
#   NOTE for a fully-glibc alternative: build a stock glibc Chromium rootfs and register it as a
#   first-class dd image + workspace exactly like docs/rendering/gtk4-workspace-repro.sh (debian
#   `chromium` / a pinned Chromium deve over ubuntu:24.04, interpreter /lib/ld-linux-aarch64.so.1),
#   pairing it with the glibc render shims in ~/.dd/gui/aarch64/lib.glibc.  That path needs the
#   apt/deb download to succeed on the dev host; the musl repair above needs no network at all.
#
# VALIDATION RESULT (2026-07-12, engine dd-jit-darwin @ current main incl. 5e8c10ee + 91f86efc):
#   * BEFORE prune: cold boot => "Exec format error" on ~19 base libs => chromium EXIT=127, 0 frames.
#   * AFTER  prune: cold boot => chromium boots, no load/reloc errors, no EXIT=127; dd-display logs
#     "client connected" + "GPU bridge cached IOSurface"; 140 PNG frames dumped (512x384).
#     Frame pixel-analysis: 442 distinct colors, ~76% white / ~16% black / ~4% titlebar-blue — a
#     REAL Chrome window (titlebar with the data: URL, _ □ X controls, the "unsupported
#     command-line flag" infobar), NOT a white screen.  Offline target: the built-in data:text/html
#     page (no network dependency; Little Snitch / offline safe).
set -euo pipefail

WS_OVERLAY="${WS_OVERLAY:-$HOME/.dd/workspaces/chromiumws/upper}"
MULTIARCH="$WS_OVERLAY/usr/lib/aarch64-linux-gnu"

echo "== 1. prune stale 0-byte injection stubs that shadow the guest's real musl base libs =="
if [ ! -d "$MULTIARCH" ]; then
  echo "   ($MULTIARCH absent — nothing to prune; a fresh overlay has no legacy stubs)"
else
  # Keep the shim render-stack stubs (they get covered by a real bind-mount at launch); remove every
  # other 0-byte *.so* stub so musl falls through to the guest's own /usr/lib copy.
  find "$MULTIARCH" -maxdepth 1 -name '*.so*' -size 0 -print | while read -r f; do
    b="$(basename "$f")"
    case "$b" in
      libEGL.so*|libGLESv2.so*|libwayland-egl*|libwayland-client*|libwayland-cursor*)
        echo "   keep  $b (shim render lib — covered by bind-mount at launch)";;
      *)
        rm -f "$f" && echo "   prune $b (stale 0-byte stub → unshadow guest's real musl lib)";;
    esac
  done
fi

echo "== 2. cold-boot Chrome against the OFFLINE data: page (bounded) and dump PNGs =="
cat <<'RUN'
  # On the mac, with a CURRENT-MAIN matched build (engine + ddcli + dd-display from the SAME commit):
  #   DDJIT_DIR=<...>/target-<name>/release/build/dd-jit-darwin-*/out
  #   DDISP=<...>/target-<name>/release/dd-display   DDCLI=<...>/target-<name>/release/ddcli
  # Drive the existing bounded harness (uses the built-in offline data:text/html page by default):
  DDJIT_DIR="$DDJIT_DIR" DDISP="$DDISP" \
    bash target-chrome-codex/run_chrome_gpu_bounded.sh 55
  # => WORK=<...>/chromium-run-gpu_retry_HHMMSS ; frames land in $WORK/frames/surface-6-*.png
RUN

echo "== 3. assert the boot is healthy (no reloc failure, real non-white content) =="
cat <<'CHK'
  W="$(cat target-chrome-codex/latest-chrome-run.txt)"
  ! grep -qaE 'Exec format error|symbol not found|EXIT=127' "$W/launch.log"   # must be clean
  test "$(ls "$W"/frames/*.png 2>/dev/null | wc -l)" -gt 0                     # frames dumped
  # (optional) decode the last frame and assert distinct_colors in the hundreds & white_frac < 0.95
CHK
