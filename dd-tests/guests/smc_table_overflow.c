// x86 SMC protection-table overflow: after SMC_MAX (8192) distinct 16 KB code pages have been translated and
// write-protected, smc_protect used to mprotect(PROT_READ) a further page BEFORE checking table capacity --
// leaving it read-only but UNTRACKED. A later guest rewrite of such a page faults on the write, but
// smc_on_write() cannot find it, so the fault is not handled -> SIGSEGV / hang. This fills the table past
// SMC_MAX, then rewrites+re-executes a LATE (overflow) page; a correct engine returns the patched value, a
// broken one crashes/hangs. x86 machine code -> LinuxX86_64 only; golden-checked.
#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
static void emit_ret(unsigned char *p, uint32_t v) {
    p[0] = 0xB8; // mov eax, imm32
    memcpy(p + 1, &v, 4);
    p[5] = 0xC3; // ret
}
typedef int (*fn)(void);
#define STRIDE 0x4000u // one distinct 16 KB SMC page per function
#define NPAGES 8300u   // > SMC_MAX (8192) -> overflow the table
int main(void) {
    size_t span = (size_t)NPAGES * STRIDE;
    unsigned char *base = mmap(0, span, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (base == MAP_FAILED) {
        printf("smc overflow mmap-failed\n");
        return 1;
    }
    // translate + protect one page per 16 KB (fills the SMC table well past SMC_MAX)
    uint64_t acc = 0;
    for (unsigned i = 0; i < NPAGES; i++) {
        unsigned char *p = base + (size_t)i * STRIDE;
        emit_ret(p, i & 0xff);
        __builtin___clear_cache((char *)p, (char *)p + 6);
        acc += (unsigned)((fn)p)();
    }
    // rewrite + re-run a LATE page (an overflow page, index > SMC_MAX): must observe the new value
    unsigned li = NPAGES - 40;
    unsigned char *lp = base + (size_t)li * STRIDE;
    emit_ret(lp, 4242 & 0x7fff);
    __builtin___clear_cache((char *)lp, (char *)lp + 6);
    int patched = ((fn)lp)();
    printf("smc overflow patched=%d\n", patched);
    return 0;
}
