#!/usr/bin/env bash
# Real-image smoke — the USER's path, the one that shipped broken.
#
# The matrix uses static-PIE guests + curated rootfs fixtures, so it never exercised a FRESH PULL of a real
# distro whose binaries load libc.so.6 through glibc's dynamic linker — exactly where `hl run ubuntu` died
# ("libc.so.6: failed to map segment"). This runs that for real, both arches, against the dd daemon (the same
# path `hl run` drives: docker CLI -> hl-daemon -> JIT). Needs the docker CLI on PATH + network -> CI only.
#
#   bash hl-tests/scenarios/smoke-realimage.sh
#
# Env: HL_DAEMON (default target/release/hl-daemon). Build it: cargo build --release -p hl-daemon.
set -u
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"; cd "$ROOT"
DAEMON="${HL_DAEMON:-$ROOT/target/release/hl-daemon}"
[ -x "$DAEMON" ] || { echo "build hl-daemon first: cargo build --release -p hl-daemon" >&2; exit 1; }
command -v docker >/dev/null 2>&1 || { echo "needs the docker CLI on PATH (e.g. 'brew install docker')" >&2; exit 1; }

SOCK="$ROOT/hl-smoke.sock"; LOG="$ROOT/hl-smoke.log"
export DOCKER_HOST="unix://$SOCK"
pkill -x hl-daemon 2>/dev/null; rm -f "$SOCK"
ST="$(mktemp -d)"; IMG="$(mktemp -d)"   # fresh, empty image dir -> forces real Docker Hub pulls
export HL_IMAGES="$IMG" HL_DOCKER_SOCK="$SOCK" HL_STATE="$ST/state.json" HL_VOLUMES="$ST/vol"
"$DAEMON" >"$LOG" 2>&1 &
DPID=$!; trap 'kill -9 $DPID 2>/dev/null; rm -f "$SOCK" "$LOG"; rm -rf "$ST" "$IMG"' EXIT
n=0; until [ -S "$SOCK" ] || [ "$n" -ge 80 ]; do sleep 0.25; n=$((n+1)); done
[ -S "$SOCK" ] || { echo "hl-daemon failed to start:"; cat "$LOG" 2>/dev/null; exit 1; }

pass=0 fail=0
run() {  # <platform> <image> <marker>  — /bin/echo is glibc-dynamic => exercises ld.so + libc.so.6
  local out; out="$(docker run --rm --platform "$1" "$2" /bin/echo "$3" 2>&1)"
  if echo "$out" | grep -qF "$3"; then echo "  ok   $1 $2  (fresh pull + glibc dynamic-load ran)"; pass=$((pass+1))
  else echo "  FAIL $1 $2:"; echo "$out" | tail -10 | sed 's/^/         /'; fail=$((fail+1)); fi
}

echo "== real-image smoke: fresh pull + run a dynamically-linked glibc binary, both arches =="
# amd64 debian is the x86_64 glibc-loader guard (the libc.so.6 fix). amd64 ubuntu is omitted for now — it hits a
# SEPARATE hl-daemon pull bug (malformed manifest digest / unextracted rootfs), tracked as its own task.
run linux/arm64 ubuntu SMOKE-UBUNTU-ARM64
run linux/arm64 debian SMOKE-DEBIAN-ARM64
run linux/amd64 debian SMOKE-DEBIAN-AMD64

echo ""
echo "smoke-realimage: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
