// SMC replacement bounce: two DIFFERENT function bodies are written to the SAME address, alternating
// tens of thousands of times. This is the pathological case for a translation cache keyed only by
// address -- it must invalidate and re-translate on every flip, never reuse the other body's cached
// translation. Both immediates are checked each flip; a stale hit corrupts the checksum. The arena is
// RW->RX toggled (the mprotect JIT path) to also exercise re-arming SMC coverage across toggles.
#include "dbt.h"

int main(void) {
    size_t sz = 4096;
    unsigned char *p = dbt_alloc(sz, PROT_READ | PROT_WRITE);
    int (*f)(void) = (int (*)(void))p;
    uint64_t acc = 0;
    for (int i = 0; i < 20000; i++) {
        int imm = (i & 1) ? 0x2468 : 0x1357; // strictly alternating, distinct bodies
        if (dbt_make_write(p, sz) != 0) { perror("mprotect rw"); return 1; }
        dbt_emit_ret_imm(p, imm);
        dbt_flush(p, sz);
        if (dbt_make_exec(p, sz) != 0) { perror("mprotect rx"); return 1; }
        int got = f();
        if (got != imm) {
            printf("smc-bounce MISMATCH i=%d want=%d got=%d\n", i, imm, got);
            return 1;
        }
        acc = acc * 31 + (uint64_t)got;
    }
    printf("smc-bounce acc=%llu\n", (unsigned long long)acc);
    return 0;
}
