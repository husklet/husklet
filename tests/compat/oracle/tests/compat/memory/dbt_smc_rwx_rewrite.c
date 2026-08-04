// SMC via an mmap-RWX arena: write a leaf function, call it, then OVERWRITE the same address with a
// different constant and call again -- for many rounds with a data-dependent sequence of immediates.
// The engine must re-translate on every rewrite (the RWX-arena SMC gate + icache flush), never serve
// a stale translation. If it caches the first body, the checksum diverges. Deterministic checksum.
#include "dbt.h"

int main(void) {
    size_t sz = 4096;
    unsigned char *p = dbt_alloc(sz, PROT_READ | PROT_WRITE | PROT_EXEC);
    int (*f)(void) = (int (*)(void))p;
    uint64_t acc = 0;
    uint32_t s = 0x1234567u;
    for (int round = 0; round < 20000; round++) {
        s = s * 1103515245u + 12345u;
        int imm = (int)((s >> 16) & 0x7fff); // 0..32767 fits movz w0 low 16 bits and mov eax
        dbt_emit_ret_imm(p, imm);
        dbt_flush(p, sz);
        int got = f();                       // must equal the JUST-written imm, not a stale one
        acc = acc * 1000003ULL + (uint64_t)got;
        if (got != imm) {
            printf("smc-rwx MISMATCH round=%d want=%d got=%d\n", round, imm, got);
            return 1;
        }
    }
    printf("smc-rwx acc=%llu\n", (unsigned long long)acc);
    return 0;
}
