#!/usr/bin/env bash
# Reproducible build + launch recipe for a COHERENT glibc aarch64 GTK4 workspace on dd-display.
#
# Produces a self-contained (all-glibc, no musl mix) ubuntu:24.04 arm64 rootfs with gtk4-demo /
# gtk4-widget-factory, registers it as a first-class dd image + workspace, and launches it headless
# on a PRIVATE dd-display socket (dumping PNGs) so the live chromiumws session is never touched.
#
# Environment assumptions (this repo's dev setup):
#   * Runs from a Linux aarch64 OrbStack VM; the macOS host is reachable via `mac bash -lc '…'`.
#   * Docker is reachable on the mac (`mac bash -lc 'docker …'`), including linux/arm64 emulation.
#   * dd (ddcli/dd-display/engine) is built on the mac from *current main* (both fixes present:
#       5e8c10ee shim_owns_lib — inject only render-stack libs, keep the guest's glibc libstdc++/libz/libX11
#       118a7aca wl_shm pool reads clamped to the fd's real fstat length (no past-EOF SIGBUS)).
#
# Result of the last validation run (2026-07-12, engine dd-jit-darwin @ main 118a7aca):
#   * GTK4 renders a live gtk4-demo window (16 committed frames, 800x600) ONLY with
#     PIXMAN_DISABLE=arm-neon set in the guest.
#   * WITHOUT PIXMAN_DISABLE the guest dies with EXC_BAD_ACCESS on a pixman NEON store
#     (deterministic: gpc=0x5001857d794, hinsn=0xf81f03e9). GTK4's GL/Vulkan GSK renderers cannot
#     realize on the dd shim (GLES3.0 / half-float / Vulkan gaps) so it falls back to cairo/pixman
#     and hits the same fault. => the "hands-off" (no PIXMAN_DISABLE) goal is NOT yet met for the
#     software/pixman path; the wl_shm memfd + server.rs clamp fix DO hold (16 frames composited).
set -euo pipefail

WT="$(cd "$(dirname "$0")/../.." && pwd)"        # repo root of this worktree
IMG_NAME="gtkbase"                                # dd image ref (docker.io/library/gtkbase:latest)
WS_NAME="gtkself"
ENC='docker.io%2Flibrary%2Fgtkbase%3Alatest'     # dd store key: encode_store_component(canonical ref)

echo "== 1. build a coherent glibc aarch64 GTK4 container via the mac docker bridge =="
mac bash -lc '
  set -e
  docker rm -f gtkbuild >/dev/null 2>&1 || true
  docker run --name gtkbuild --platform linux/arm64 ubuntu:24.04 bash -c "
    set -e; export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y --no-install-recommends \
      gtk-4-examples libgtk-4-1 fonts-dejavu-core libgl1-mesa-dri dbus-x11 ca-certificates
    apt-get clean; rm -rf /var/lib/apt/lists/*
    test -x /usr/bin/gtk4-demo && test -x /usr/bin/gtk4-widget-factory"
  mkdir -p /tmp/gtkws
  docker export gtkbuild -o /tmp/gtkws/gtk-rootfs.tar
  echo "exported $(du -h /tmp/gtkws/gtk-rootfs.tar | cut -f1)"
'

echo "== 2. register it as a first-class dd image (self-contained; no base overlay needed) =="
mac bash -lc '
  set -e
  D="$HOME/.dd/images/arm64/'"$ENC"'"
  rm -rf "$D"; mkdir -p "$D/rootfs"
  tar -xf /tmp/gtkws/gtk-rootfs.tar -C "$D/rootfs"
  cat > "$D/dd-image.json" <<JSON
{"arch":"aarch64","cmd":["/bin/bash"],"entrypoint":[],"env":["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"],"exposed_ports":[],"name":"'"$IMG_NAME"':latest","os":"linux","user":"","workdir":"/root"}
JSON
  # self-contained launch script baked into the rootfs (no external mounts)
  cat > "$D/rootfs/run-gtk.sh" <<"SH"
#!/bin/sh
export GDK_BACKEND=wayland
export GSK_RENDERER="${GSK_RENDERER:-cairo}"   # software/pixman path (exercises wl_shm memfd + NEON blit)
export GTK_MEDIA=none GTK_A11Y=none NO_AT_BRIDGE=1 GDK_DISABLE=gl
# To render TODAY, also: export PIXMAN_DISABLE=arm-neon   (removed hack; see header note)
APP="${APP:-/usr/bin/gtk4-demo}"
echo "[run-gtk] WAYLAND_DISPLAY=$WAYLAND_DISPLAY GSK_RENDERER=$GSK_RENDERER APP=$APP"
exec "$APP"
SH
  chmod +x "$D/rootfs/run-gtk.sh"
  file "$D/rootfs/usr/bin/gtk4-demo"   # must be interpreter /lib/ld-linux-aarch64.so.1 (glibc, coherent)
'

echo "== 3. register the workspace (additive; backs up workspaces.conf) =="
mac bash -lc '
  set -e
  cp -p "$HOME/.dd/workspaces.conf" "$HOME/.dd/workspaces.conf.bak.gtkself" || true
  ddcli workspace create '"$WS_NAME"' --image '"$IMG_NAME"' --arch arm64 --gui on
  # add the launch script + drop the per-workspace docker daemon (not needed)
  python3 - <<PY
import os
p=os.path.expanduser("~/.dd/workspaces.conf"); L=open(p).read().splitlines(); out=[]; i=0
while i<len(L):
    out.append(L[i])
    if L[i].strip()=="name = '"$WS_NAME"'":
        i+=1; blk=[]
        while i<len(L) and L[i].strip() and not L[i].startswith("[workspace]"): blk.append(L[i]); i+=1
        nb=[]
        for b in blk:
            if b.strip().startswith("docker_sock"): b="docker_sock = false"
            if b.strip().startswith("shell"): continue
            nb.append(b)
            if b.strip().startswith("arch"): nb.append("shell = /bin/sh /run-gtk.sh")
        out.extend(nb); continue
    i+=1
open(p,"w").write("\n".join(out)+"\n")
PY
  ddcli workspace list | grep '"$WS_NAME"'
'

echo "== 4. launch headless on a PRIVATE dd-display socket (live chromiumws untouched) =="
echo "   (set DDJIT_DIR to a current-main engine out dir; dd-display + ddcli must be current-main builds)"
cat <<'RUN'
  # on the mac, with WT=<worktree>, DDJIT_DIR=<...>/build/dd-jit-darwin-*/out :
  DD_GPU_EXEC_SOCK=~/.dd/run/gtkself-dd-gpu.sock \
    "$WT"/target-*/release/dd-display --socket ~/.dd/run/gtkself-wayland-0 --png /tmp/dd-display-gtkself --metal &
  env -u PIXMAN_DISABLE -u LD_PRELOAD \
    DDJIT_DIR="$DDJIT_DIR" \
    DD_DISPLAY_SOCK=~/.dd/run/gtkself-wayland-0 DD_GPU_EXEC_SOCK=~/.dd/run/gtkself-dd-gpu.sock \
    CRASHDBG=1 DD_FATALSIG_LOG=1 \
    timeout 90 "$WT"/target-*/release/ddcli workspace launch gtkself </dev/null
  # PNGs land in /tmp/dd-display-gtkself/surface-*.png
RUN
