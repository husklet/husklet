// fdguard.c — LD_PRELOAD that neutralizes Chromium's fatal "FD ownership
// violation" abort on the OrbStack kernel, by disabling the diagnostic
// enforcement flag that gates the abort.
//
// MECHANISM (pinned by disassembling the crash site in the stripped binary):
// Chromium's binary defines its own global `close()` that interposes every
// close. For fd <= 4095 it consults a per-fd "owned" bitmap (base::ScopedFD
// marks a fd owned on Acquire, clears it on Free-before-close). If close() is
// called on a fd whose owned-bit is still set, AND an enforcement byte is
// nonzero, it prints "Crashing due to FD ownership violation:" and executes the
// arm64 IMMEDIATE_CRASH (`brk #0; hlt #0x462`). i.e. the crash fires when some
// code raw-`close()`s a fd that a live base::ScopedFD still owns. On this box a
// fd number churned by the profile/cache bring-up gets closed while still owned;
// on a stock kernel the same build tolerates it because the enforcement byte is
// off. We cannot win the `close` symbol (the executable's own definition
// interposes ahead of any LD_PRELOAD), so instead we hold the enforcement byte
// at zero — exactly what a normal release build ships — which turns the abort
// back into the harmless fall-through to the real close.
//
// Enforcement byte link-time vaddr in this specific binary (Chromium 150 arm64):
//   0xfd028f8  (found via the sole ADRP/ADD xref to the violation string).
// The binary is PIE, so the runtime address is load_base + that offset. We keep
// it zeroed from a background thread through the whole startup window.
#define _GNU_SOURCE
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

// Link-time vaddr of the fd-ownership enforcement byte. Overridable via env in
// case a different Chromium build is targeted.
static uintptr_t flag_off(void) {
  const char *e = getenv("HL_FDGUARD_FLAG_OFFSET");
  if (e && *e) return (uintptr_t)strtoull(e, 0, 0);
  return 0xfd028f8ULL;
}

// Return the load base of the main executable (its first LOAD maps file off 0 at
// vaddr 0, so the base is the lowest mapping backed by the exe path). We match by
// reading /proc/self/exe and finding that path in /proc/self/maps.
static uintptr_t exe_base(void) {
  char exe[512];
  ssize_t n = readlink("/proc/self/exe", exe, sizeof(exe) - 1);
  if (n <= 0) return 0;
  exe[n] = 0;
  int fd = open("/proc/self/maps", O_RDONLY | O_CLOEXEC);
  if (fd < 0) return 0;
  static char buf[1 << 20];
  ssize_t got = 0, r;
  while ((r = read(fd, buf + got, sizeof(buf) - 1 - got)) > 0) {
    got += r;
    if (got >= (ssize_t)sizeof(buf) - 1) break;
  }
  close(fd);
  buf[got > 0 ? got : 0] = 0;
  uintptr_t best = 0;
  char *line = buf;
  while (line && *line) {
    char *nl = strchr(line, '\n');
    if (nl) *nl = 0;
    // Lines that name the exe path.
    if (strstr(line, exe)) {
      uintptr_t start = (uintptr_t)strtoull(line, 0, 16);
      if (best == 0 || start < best) best = start;
    }
    line = nl ? nl + 1 : 0;
  }
  return best;
}

// Link-time vaddrs of the THREE `tbnz w8,#0, <crash>` branches that gate the fd
// ownership abort in this binary — one in each of the tracker's primitives:
//   0x84e814c  ScopedFD Acquire   (fires on double-own: fd already owned)
//   0x84e8198  ScopedFD Free      (fires on release of an un-owned fd)
//   0x84e8234  global close() hook (fires on raw close of an owned fd)
// All three read the enforcement byte then branch to the shared crash printer.
// NOP-ing each makes the tracker fall through to its normal path — exactly how a
// stock release build (no enforcement) behaves. Race-free and complete.
// Overridable via HL_FDGUARD_PATCH_OFFSETS (comma-separated hex).
#define N_PATCH 3
static const uintptr_t k_patch[N_PATCH] = {0x84e814cULL, 0x84e8198ULL, 0x84e8234ULL};

// Patch a single aarch64 instruction to NOP (0xd503201f) at runtime addr `at`,
// but ONLY if it currently holds `tbnz w8,#0,<positive-target>` (mask
// 0xFFF8001F == 0x37000008). If the opcode does not match — e.g. a different
// Chromium build whose offsets differ — leave it untouched (return -2) rather
// than corrupt an unrelated instruction. Returns 0 on patch, -1 mprotect fail,
// -2 opcode mismatch.
static int patch_nop(uintptr_t at) {
  uint32_t cur = __atomic_load_n((uint32_t *)at, __ATOMIC_SEQ_CST);
  if ((cur & 0xFFF8001FU) != 0x37000008U) {
    return -2; // not the expected tbnz w8,#0 — refuse to patch
  }
  long pg = sysconf(_SC_PAGESIZE);
  uintptr_t page = at & ~(uintptr_t)(pg - 1);
  if (mprotect((void *)page, (size_t)pg, PROT_READ | PROT_WRITE | PROT_EXEC) != 0) {
    return -1;
  }
  uint32_t nop = 0xd503201f;
  __atomic_store_n((uint32_t *)at, nop, __ATOMIC_SEQ_CST);
  __builtin___clear_cache((char *)at, (char *)(at + 4));
  // Restore R-X (best effort; leaving it writable would also work).
  mprotect((void *)page, (size_t)pg, PROT_READ | PROT_EXEC);
  return 0;
}

// Only act on the Chromium executable. For any other program that inherits this
// LD_PRELOAD (shell utilities Chromium or the harness spawns), the flag offset
// points at an unmapped address and must never be touched.
static int is_chromium(void) {
  char exe[512];
  ssize_t n = readlink("/proc/self/exe", exe, sizeof(exe) - 1);
  if (n <= 0) return 0;
  exe[n] = 0;
  // Match the ELF basename "chromium" (the run-prefix copy is also named this).
  const char *slash = strrchr(exe, '/');
  const char *bn = slash ? slash + 1 : exe;
  return strcmp(bn, "chromium") == 0;
}

static volatile unsigned char *g_flag = 0;

// Optional debug: write a line to HL_FDGUARD_LOG.
static void dbg(const char *msg, uintptr_t v) {
  const char *p = getenv("HL_FDGUARD_LOG");
  if (!p) return;
  int fd = open(p, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0644);
  if (fd < 0) return;
  char b[128];
  int n = 0;
  while (msg[n] && n < 90) { b[n] = msg[n]; n++; }
  b[n++] = '=';
  char t[20];
  int m = 0;
  if (v == 0) t[m++] = '0';
  while (v) { int d = v & 0xf; t[m++] = (char)(d < 10 ? '0' + d : 'a' + d - 10); v >>= 4; }
  b[n++] = '0'; b[n++] = 'x';
  while (m) b[n++] = t[--m];
  b[n++] = '\n';
  (void)!write(fd, b, (size_t)n);
  close(fd);
}

__attribute__((constructor)) static void fdguard_init(void) {
  if (!is_chromium()) return;
  uintptr_t base = exe_base();
  dbg("base", base);
  if (!base) return;
  for (int i = 0; i < N_PATCH; i++) {
    uintptr_t at = base + k_patch[i];
    int rc = patch_nop(at);
    dbg("patched_at", at);
    dbg("patch_rc", (uintptr_t)rc);
  }
  // Belt-and-suspenders: also clear the enforcement byte.
  g_flag = (volatile unsigned char *)(base + flag_off());
  *g_flag = 0;
}
