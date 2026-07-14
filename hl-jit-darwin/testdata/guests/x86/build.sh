#!/usr/bin/env bash
# Rebuild the prebuilt x86-64 fixture binaries for the `x86` test group (hl-tests/src/cases/
# container.rs::x86). The binaries are COMMITTED next to this script — the harness runs them as-is
# (Bin::Fixture), it never compiles them — because each one pins a specific binary FLAVOR the
# on-the-fly `src()` builds don't cover (nolibc _start guests, static non-PIE glibc, static
# non-PIE Go). Run this from the repo's Linux dev VM only when a fixture must change, then update
# the goldens in container.rs if an output changed, and commit sources + binaries together.
#
# Toolchains:
#   * C:  x86_64-linux-gnu-gcc (the same cross compiler harness `src()` cases use).
#   * Go: `go` from PATH (override with GO=/path/to/go); the committed binaries were cross-built
#         GOARCH=amd64 with go1.26.5 linux/arm64 (same toolchain as guests/arm/go_cgo_stackgrow_arm).
#         CGO_ENABLED=0 + the default buildmode give the static non-PIE (ET_EXEC) image the
#         go-static-* cases exist to pin. -trimpath -buildid= keep the bytes reproducible; symbols
#         are KEPT (no -s -w) so the engine's go_rebase_nonpie takes the runtime.firstmoduledata
#         symbol fast path (stripped Go is covered by real-image cases elsewhere).
set -euo pipefail
cd "$(dirname "$0")"

CC=${CC:-x86_64-linux-gnu-gcc}
GO=${GO:-go}

# nolibc raw-syscall guests: static non-PIE, no crt/libc at all (_start is the entry).
$CC -O2 -static -no-pie -nostdlib -o hello_x86 hello_x86.c
$CC -O2 -static -no-pie -nostdlib -o ctest_x64 ctest_x64.c
$CC -O2 -static -no-pie -nostdlib -o hx hx.c

# glibc guests: g_x64 = static-PIE (ET_DYN), gw = static non-PIE (ET_EXEC) — both glibc startups.
$CC -O2 -static-pie -o g_x64 g_x64.c
$CC -O2 -static -no-pie -o gw gw.c

# Go guests: static non-PIE amd64 (the #250 regression-guard flavor).
if command -v "$GO" >/dev/null 2>&1; then
    export GOCACHE="${GOCACHE:-$PWD/.gocache}" CGO_ENABLED=0 GOOS=linux GOARCH=amd64
    "$GO" build -trimpath -ldflags=-buildid= -o go_goro_x86 go_goro.go
    "$GO" build -trimpath -ldflags=-buildid= -o go_heapgc_x86 go_heapgc.go
    rm -rf .gocache
else
    echo "WARN: go toolchain not found ($GO) — skipped go_goro_x86/go_heapgc_x86" >&2
fi

file hello_x86 ctest_x64 hx g_x64 gw go_goro_x86 go_heapgc_x86 2>/dev/null || true
sha256sum hello_x86 ctest_x64 hx g_x64 gw go_goro_x86 go_heapgc_x86 2>/dev/null || true
