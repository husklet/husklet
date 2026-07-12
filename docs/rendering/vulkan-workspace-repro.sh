#!/usr/bin/env bash
# Reproducible build + launch recipe for a COHERENT glibc aarch64 SOFTWARE-VULKAN workspace
# on dd-display (the "software Vulkan app renders on dd" milestone).
#
# Produces a self-contained (all-glibc, no musl mix) ubuntu:24.04 arm64 rootfs with Mesa lavapipe
# (libvulkan_lvp.so = a pure-CPU Vulkan 1.3 device), the Vulkan loader (libvulkan1) and
# vulkan-tools (vulkaninfo + vkcube/vkcube-wayland), registers it as a first-class dd image +
# workspace, and launches vkcube headless on a PRIVATE dd-display socket (dumping PNGs) so any live
# session is never touched. Vulkan is BAKED INTO the image (installed at docker-build time, which
# has egress via the mac docker bridge) so the guest needs NO launch-time apt — offline/Little-Snitch
# safe, unlike an apt-at-launch workspace.
#
# WHY THIS PROVES THE MILESTONE
#   lavapipe (Mesa's llvmpipe Vulkan driver) renders the vkcube frame entirely on the CPU into a
#   wl_shm shared-memory buffer, then presents it over Wayland to dd-display — the SAME software
#   present lane GTK4-cairo uses (docs/rendering/gtk4-workspace-repro.sh). No dd GPU/Vulkan ICD is in
#   the guest; this is independent of the dd-shim-vk HOST path. It proves the guest Vulkan
#   environment (loader + ICD wiring) and the wl_shm present path work for a real Vulkan app.
#
# Environment assumptions (this repo's dev setup):
#   * Runs from a Linux aarch64 OrbStack VM; the macOS host is reachable via `mac bash -lc '…'`.
#   * Docker is reachable on the mac (`mac bash -lc 'docker …'`), including linux/arm64 emulation.
#   * dd (ddcli/dd-display/engine) built on the mac from *current main*. Required fixes present:
#       - shim_owns_lib: inject only the render stack (libEGL/libGLESv2/libwayland-*); the guest
#         keeps its OWN glibc libstdc++/libc/libz AND its own Mesa Vulkan loader (no libvulkan.so.1
#         in ~/.dd/gui/aarch64/lib => the shim injects no Vulkan loader; lavapipe is used as-is).
#       - the aarch64 NEON JIT store fix + wl_shm memfd clamp: lavapipe's LLVM-JITted SIMD kernels
#         no longer crash the JIT (earlier attempts hit an EXC_BAD_ACCESS / SIGBUS here).
#
# VALIDATION RESULT (2026-07-12, engine dd-jit-darwin @ current main a855ca16 + this worktree):
#   * vulkaninfo ENUMERATES the lavapipe device (no ERROR_INCOMPATIBLE_DRIVER):
#       deviceName = llvmpipe (LLVM 20.1.2, 128 bits)
#       driverID   = DRIVER_ID_MESA_LLVMPIPE   apiVersion 1.4.318   Mesa 25.2.8
#   * vkcube-wayland --c 100 ran to a CLEAN exit; dd-display logged 100 committed frames
#     ("client disconnected (100 frame(s))"), each presented 500x500 titled "vkcube".
#   * Final frame pixel-analysis (surface-6.png, 500x500): 2954 distinct colors, a centered 3D
#     shape (non-bg bbox x[87..406] y[104..435], 29.8% non-background) — the classic vkcube:
#     a perspective 3D cube with the LunarG teal-textured logo on three visible faces. NOT blank.
#   * KEY WIRING GOTCHAS (both fatal if wrong):
#       1. VK_ICD_FILENAMES MUST pin the lvp manifest alone. The rootfs ships ~10 ICD manifests
#          (asahi/intel/radeon/…); without the pin the loader tries them all -> INCOMPATIBLE_DRIVER.
#       2. Use the `vkcube-wayland` binary, NOT `vkcube`. Plain `vkcube` auto-selects XCB and dies
#          with "Environment variable DISPLAY requires a valid value" under a headless Wayland-only
#          session. `vkcube-wayland` binds the Wayland WSI directly.
set -euo pipefail

IMG_NAME="vkbase"                                 # dd image ref (docker.io/library/vkbase:latest)
WS_NAME="vkself"
ENC='docker.io%2Flibrary%2Fvkbase%3Alatest'      # dd store key: encode_store_component(canonical ref)

echo "== 1. build a coherent glibc aarch64 lavapipe + vulkan-tools container via the mac docker bridge =="
mac bash -lc '
  set -e
  docker rm -f vkbuild >/dev/null 2>&1 || true
  docker run --name vkbuild --platform linux/arm64 ubuntu:24.04 bash -c "
    set -e; export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y --no-install-recommends \
      libvulkan1 mesa-vulkan-drivers vulkan-tools \
      libwayland-client0 libwayland-server0 libxcb1 libzstd1 \
      libx11-6 libxcb-randr0 fonts-dejavu-core ca-certificates
    apt-get clean; rm -rf /var/lib/apt/lists/*
    test -x /usr/bin/vkcube-wayland && test -x /usr/bin/vulkaninfo
    test -f /usr/share/vulkan/icd.d/lvp_icd.json
    test -f /usr/lib/aarch64-linux-gnu/libvulkan_lvp.so"
  mkdir -p /tmp/vkws
  docker export vkbuild -o /tmp/vkws/vk-rootfs.tar
  echo "exported $(du -h /tmp/vkws/vk-rootfs.tar | cut -f1)"
'

echo "== 2. register it as a first-class dd image (self-contained; no base overlay needed) =="
mac bash -lc '
  set -e
  D="$HOME/.dd/images/arm64/'"$ENC"'"
  rm -rf "$D"; mkdir -p "$D/rootfs"
  tar -xf /tmp/vkws/vk-rootfs.tar -C "$D/rootfs"
  cat > "$D/dd-image.json" <<JSON
{"arch":"aarch64","cmd":["/bin/bash"],"entrypoint":[],"env":["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"],"exposed_ports":[],"name":"'"$IMG_NAME"':latest","os":"linux","user":"","workdir":"/root"}
JSON
  # self-contained launch script baked into the rootfs (no external mounts, no launch-time apt)
  cat > "$D/rootfs/run-vk.sh" <<"SH"
#!/bin/sh
# Force lavapipe: pin the loader to ONLY the lvp ICD manifest (the rootfs ships ~10; without this
# the loader tries them all and fails INCOMPATIBLE_DRIVER on this virtual GPU).
echo "=== run-vk (pid $$) ==="
LVP_ICD=""
for c in /usr/share/vulkan/icd.d/lvp_icd.aarch64.json /usr/share/vulkan/icd.d/lvp_icd.json; do
  [ -f "$c" ] && LVP_ICD="$c" && break
done
export VK_ICD_FILENAMES="$LVP_ICD"
export VK_DRIVER_FILES="$LVP_ICD"
export GDK_BACKEND=wayland
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/0}"
echo "VK_ICD_FILENAMES=$VK_ICD_FILENAMES WAYLAND_DISPLAY=$WAYLAND_DISPLAY"
echo "=== vulkaninfo (device enumeration) ==="
vulkaninfo 2>/dev/null | grep -E "deviceName|driverID|apiVersion|deviceType" | head -6
# vkcube auto-picks XCB (needs DISPLAY); use the wayland-native binary.
APP="${APP:-vkcube-wayland}"
FRAMES="${FRAMES:-100}"
echo "=== launching $APP --c $FRAMES on wayland ==="
exec "$APP" --c "$FRAMES"
SH
  chmod +x "$D/rootfs/run-vk.sh"
'

echo "== 3. register the workspace (additive; backs up workspaces.conf) =="
mac bash -lc '
  set -e
  cp -p "$HOME/.dd/workspaces.conf" "$HOME/.dd/workspaces.conf.bak.vkself" || true
  ddcli workspace create '"$WS_NAME"' --image '"$IMG_NAME"' --arch arm64 --gui on
  python3 - <<PY
import os
p=os.path.expanduser("~/.dd/workspaces.conf"); L=open(p).read().splitlines(); out=[]; i=0
while i<len(L):
    line=L[i]
    if line.strip()=="name = '"$WS_NAME"'":
        out.append(line); i+=1; blk=[]
        while i<len(L) and L[i].strip() and L[i].strip()!="[workspace]": blk.append(L[i]); i+=1
        for b in blk:
            s=b.strip()
            if s.startswith("docker_sock"): out.append("docker_sock = false"); continue
            if s.startswith("shell"): continue
            out.append(b)
            if s.startswith("arch"): out.append("shell = /bin/sh /run-vk.sh")
        continue
    out.append(line); i+=1
open(p,"w").write("\n".join(out)+"\n")
PY
  ddcli workspace list | grep '"$WS_NAME"'
'

echo "== 4. launch headless on a PRIVATE dd-display socket (CPU png path; lavapipe = wl_shm) =="
echo "   (set DDJIT_DIR to a current-main engine out dir; dd-display + ddcli must be current-main builds)"
cat <<'RUN'
  # on the mac, with DDJIT_DIR=<...>/build/dd-jit-darwin-*/out and the current-main target dir:
  RUN=$HOME/.dd/run/vkself-priv; PNG=$RUN/png; rm -rf "$RUN"; mkdir -p "$PNG"
  DD_GPU_EXEC_SOCK="$RUN/dd-gpu.sock" \
    <target>/release/dd-display --socket "$RUN/wayland-0" --png "$PNG" > "$RUN/dd-display.log" 2>&1 &
  for i in $(seq 1 50); do [ -S "$RUN/wayland-0" ] && break; sleep 0.2; done
  env -u LD_PRELOAD -u PIXMAN_DISABLE \
    DDJIT_DIR="$DDJIT_DIR" \
    DD_DISPLAY_SOCK="$RUN/wayland-0" DD_GPU_EXEC_SOCK="$RUN/dd-gpu.sock" \
    CRASHDBG=1 DD_FATALSIG_LOG=1 \
    timeout 120 <target>/release/ddcli workspace launch vkself </dev/null
  # dd-display logs "client disconnected (100 frame(s))"; the last frame is $PNG/surface-6.png
  #   -> a non-blank vkcube (LunarG-textured 3D cube). Pixel-check: >2900 distinct colors.
RUN
