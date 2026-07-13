#!/usr/bin/env bash
# dd LTP compliance lane — BUILD stage (no autotools).
#
# Fetches LTP at a pinned commit, then cross-compiles the LTP test harness
# ("new API" libltp) + the curated CORE test subset into static-PIE binaries
# for BOTH guest arches (aarch64 native gcc, x86_64 cross gcc). LTP's normal
# build needs autoconf/automake + a `make` in include/lapi to generate a
# per-arch syscall table; neither is available here, so we drive the two pure
# shell generators (generate_syscalls.sh, ltp-version.h) directly and supply a
# hand-built include/config.h (see ./config.h) that mirrors what ./configure
# would detect on a modern glibc host.
#
# Outputs (under $OUT, default dd-tests/compliance/ltp/out):
#   ltp-src/               the pinned LTP checkout
#   bin/<arch>/<test>      one static test binary per (arch, curated test)
#   build.log             per-test compile status
#
# Re-run safe / incremental: an existing ltp-src/ checkout is reused.
set -uo pipefail

# The curated list drives ~900 sequential `cc` invocations (2 arches x the
# full tests.list). Each compile is a fresh process, but an ambient nofile
# soft limit that's already low (macOS default is 256; some CI/agent shells
# inherit a similarly small value from a long-lived session) leaves too
# little headroom once a few hundred short-lived compiler/linker fds have
# cycled through, and `cc` starts failing with "Too many open files" partway
# through the run. Raise the soft limit for this script's own process tree
# (capped at whatever hard limit the host allows — never fails the build if
# the host refuses the change, e.g. a sandboxed environment without
# CAP_SYS_RESOURCE and a hard limit below 8192).
ulimit -n 8192 2>/dev/null || ulimit -n "$(ulimit -Hn)" 2>/dev/null || true

HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="${LTP_OUT:-$HERE/out}"
SRC="$OUT/ltp-src"
PIN="${LTP_PIN:-ae4a01208fa2ce31f4f0a7a92b6e71a32299eb94}"   # linux-test-project/ltp
REPO="${LTP_REPO:-https://github.com/linux-test-project/ltp}"

CC_ARM="${CC_ARM:-gcc}"
CC_X86="${CC_X86:-x86_64-linux-gnu-gcc}"

mkdir -p "$OUT"

# ---- 1. fetch pinned LTP ------------------------------------------------------
if [ ! -d "$SRC/.git" ]; then
  echo "[ltp] cloning $REPO @ $PIN"
  git clone "$REPO" "$SRC" >/dev/null 2>&1 || { echo "clone failed"; exit 1; }
  ( cd "$SRC" && git checkout -q "$PIN" ) || { echo "checkout $PIN failed"; exit 1; }
else
  echo "[ltp] reusing $SRC ($(cd "$SRC" && git rev-parse --short HEAD))"
fi

INC="$SRC/include"

# ---- 2. generate the files LTP's build would generate -------------------------
# 2a. per-arch syscall-number fallback table (pure text merge of the *.in files).
if [ ! -f "$INC/lapi/syscalls.h" ]; then
  ( cd "$INC/lapi/syscalls" && sh generate_syscalls.sh "$INC/lapi/syscalls.h" ) \
    && echo "[ltp] generated lapi/syscalls.h"
fi
# 2b. version header (normally emitted by the top-level Makefile).
if [ ! -f "$INC/ltp-version.h" ]; then
  printf '#define LTP_VERSION "%s"\n' "ltp-$(echo "$PIN" | cut -c1-12)" > "$INC/ltp-version.h"
fi
# 2c. hand-built config.h (mirrors ./configure on a modern glibc host).
cp "$HERE/config.h" "$INC/config.h"

# ---- 3. build libltp.a (the "new API" harness + old-API + IPC helpers) --------
# Compile every lib source we can; unused objects are simply never pulled from
# the archive by the linker. -DLTPLIB disables the "no old API in new code"
# poison macros that guard the mixed old/new headers the runner itself includes.
#
# Two extras beyond lib/*.c:
#   * OLD-API sources (parse_opts.c, tst_sig.c, random_range.c) are pre-ANSI
#     K&R-flavoured and trip gcc-14's now-hard errors ("conflicting types" /
#     "too many arguments" from unprototyped defs). They still build cleanly
#     under -std=gnu89, and supply symbols the old-API tests link against
#     (usc_test_looping, usc_global_setup_hook, tst_sig, def_handler …). Each
#     source is tried with modern flags first and only retried with -std=gnu89
#     on failure, so no new-API source is ever down-graded.
#   * The SysV-IPC test helper (libs/newipc/tse_newipc.c) is folded into the
#     same archive so the msg*/sem*/shm* + kill05 tests resolve getipckey,
#     probe_free_addr, getuserid, …
CFLAGS_LIB="-O2 -w -fpermissive -fPIC -D_GNU_SOURCE -DLTPLIB -I $INC -I $INC/old"
LIB_SRCS=( "$SRC"/lib/*.c "$SRC"/libs/newipc/tse_newipc.c )
build_lib() {
  local cc="$1" arch="$2"; local od="$OUT/lib/$arch"
  rm -rf "$od"; mkdir -p "$od"
  local ok=0 fb=0 fail=0
  for f in "${LIB_SRCS[@]}"; do
    local b; b="$(basename "$f" .c)"
    if $cc $CFLAGS_LIB -c "$f" -o "$od/$b.o" 2>"$od/$b.err"; then ok=$((ok+1));
    elif $cc $CFLAGS_LIB -std=gnu89 -c "$f" -o "$od/$b.o" 2>"$od/$b.err"; then ok=$((ok+1)); fb=$((fb+1));
    else fail=$((fail+1)); fi
  done
  ar rcs "$OUT/lib/libltp-$arch.a" "$od"/*.o 2>/dev/null
  echo "[ltp] libltp-$arch.a: $ok objs ($fb via -std=gnu89 fallback; $fail sources skipped)"
}
build_lib "$CC_ARM" arm64
build_lib "$CC_X86" x86_64

# ---- 4. compile the curated test subset per arch ------------------------------
# One `cc` at a time, on purpose: each compile fully exits (and its fds with
# it) before the next starts, so there's no concurrent-job fd pile-up to
# bound here — the ulimit bump above is what covers the sequential total.
# -fpermissive down-grades gcc-14's incompatible-pointer permerrors (old-API
# tests pass a `void(*)(int)` where tst_sig/signal want `void(*)(void)`) back to
# warnings. -I .../syscalls/utils supplies the shared per-family test headers
# that live there rather than in include/: compat_16.h + compat_tst_16.h (the
# 16-bit uid/gid chown/cred tests) and mq.h + mq_timed.h (POSIX mqueue tests).
# A per-test -std=gnu89 fallback (same rationale as the lib build) rescues the
# few K&R-flavoured old-API tests; new-API tests compile on the first attempt.
UTILS="$SRC/testcases/kernel/syscalls/utils"
CFLAGS_T="-O2 -w -fpermissive -D_GNU_SOURCE -static -pthread -I $INC -I $INC/old -I $UTILS"
: > "$OUT/build.log"
compile_arch() {
  local cc="$1" arch="$2"; local bd="$OUT/bin/$arch"
  rm -rf "$bd"; mkdir -p "$bd"
  local ok=0 fail=0
  while read -r cat sys rel; do
    case "$cat" in ''|\#*) continue;; esac
    local name; name="$(basename "$rel" .c)"
    local src="$SRC/testcases/kernel/syscalls/$rel"
    if [ ! -f "$src" ]; then echo "MISSING $arch $name ($rel)" >>"$OUT/build.log"; fail=$((fail+1)); continue; fi
    if $cc $CFLAGS_T "$src" "$OUT/lib/libltp-$arch.a" -o "$bd/$name" 2>"$bd/$name.err" \
       || $cc $CFLAGS_T -std=gnu89 "$src" "$OUT/lib/libltp-$arch.a" -o "$bd/$name" 2>"$bd/$name.err"; then
      echo "OK    $arch $cat/$name" >>"$OUT/build.log"; ok=$((ok+1))
    else
      echo "FAILC $arch $cat/$name" >>"$OUT/build.log"; fail=$((fail+1))
    fi
  done < "$HERE/tests.list"
  echo "[ltp] $arch tests compiled: $ok ok, $fail failed"
}
compile_arch "$CC_ARM" arm64
compile_arch "$CC_X86" x86_64

echo "[ltp] build complete -> $OUT/bin/{arm64,x86_64}"
