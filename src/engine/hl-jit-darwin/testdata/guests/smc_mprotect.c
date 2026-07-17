// H9 / #423 regression: mprotect(PROT_EXEC) must ARM self-modifying-code (SMC) re-translation.
//
// The JIT toggle that .NET/Wasm/managed x86 runtimes use is: mmap(RW) a code arena, WRITE machine code
// into it, mprotect(RX) to make it executable, run it -- then later mprotect(RW), REWRITE the same bytes,
// mprotect(RX), and run again expecting the NEW code. This is DISTINCT from the mmap(RWX) arena (which the
// engine's mmap case already arms SMC for): here the page is never mapped PROT_EXEC via mmap, only via
// mprotect. If mprotect(PROT_EXEC) fails to set the SMC gate (g_rwx_guest), the engine caches the FIRST
// translation forever and the second (rewritten) call returns the STALE result -> silent miscompile.
//
// x86 is the exposed arch: it has a coherent i-cache, so a rewrite carries NO architectural i-cache-flush
// instruction for the DBT to hook -- the ONLY invalidation signal is the write-fault the SMC page-protect
// arms. (aarch64 must execute `ic ivau`, which __builtin___clear_cache emits and the engine hooks
// independently, so aarch64 passes either way -- it rides along as a second witness.)
//
// Golden output (native and a CORRECT engine): "smc mprotect r1=111 r2=222 r3=333". A broken engine that
// never invalidates returns r1=r2=r3=111. Three rounds prove coverage is re-armed/KEPT across re-toggles,
// not lost after the first.
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>

// Emit a leaf function `int f(void){ return imm; }` at buf; return its length in bytes.
static int emit_ret_imm(unsigned char *buf, int imm) {
#if defined(__aarch64__)
    uint32_t *w = (uint32_t *)buf;
    w[0] = 0x52800000u | ((uint32_t)(imm & 0xffff) << 5); // movz w0, #imm
    w[1] = 0xD65F03C0u;                                   // ret
    return 8;
#elif defined(__x86_64__)
    buf[0] = 0xB8; // mov eax, imm32
    buf[1] = (unsigned char)(imm & 0xff);
    buf[2] = (unsigned char)((imm >> 8) & 0xff);
    buf[3] = (unsigned char)((imm >> 16) & 0xff);
    buf[4] = (unsigned char)((imm >> 24) & 0xff);
    buf[5] = 0xC3; // ret
    return 6;
#else
    (void)buf;
    (void)imm;
    return 0;
#endif
}

// Rewrite the arena to `return imm`, flush, mprotect(RX), call it. Returns the callee's result.
static int rewrite_and_call(unsigned char *p, size_t sz, int imm) {
    if (mprotect(p, sz, PROT_READ | PROT_WRITE) != 0) { perror("mprotect rw"); return -1; }
    emit_ret_imm(p, imm);
    __builtin___clear_cache((char *)p, (char *)p + sz);
    if (mprotect(p, sz, PROT_READ | PROT_EXEC) != 0) { perror("mprotect rx"); return -1; }
    int (*f)(void) = (int (*)(void))p;
    return f();
}

int main(void) {
    size_t sz = 4096;
    // Deliberately RW-only (NOT PROT_EXEC): this is the path the mmap case does NOT arm; mprotect must.
    unsigned char *p = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) { perror("mmap"); return 1; }

    int r1 = rewrite_and_call(p, sz, 111);
    int r2 = rewrite_and_call(p, sz, 222); // must invalidate the r1 translation
    int r3 = rewrite_and_call(p, sz, 333); // must invalidate the r2 translation (coverage kept)

    printf("smc mprotect r1=%d r2=%d r3=%d\n", r1, r2, r3);
    return 0;
}
