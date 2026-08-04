// SMC of code that STRADDLES a page boundary. A leaf function is emitted so its instructions span the
// boundary between two pages, then rewritten in place across the boundary and re-run. A DBT that
// tracks SMC/dirty state per-page (rather than per-instruction/per-range) can miss the rewrite of the
// half on the "clean" page and serve a stale translation. Per-arch emission; deterministic checksum.
#include "dbt.h"
#include <unistd.h>

int main(void) {
    size_t page = 4096;
    size_t sz = 2 * page;
    unsigned char *p = dbt_alloc(sz, PROT_READ | PROT_WRITE | PROT_EXEC);
    // Place the function entry a few bytes before the page boundary so its body spans both pages.
    unsigned char *entry = p + page - 4;

    uint64_t acc = 0;
    uint32_t s = 0x9e37u;
    for (int round = 0; round < 15000; round++) {
        s = s * 1103515245u + 12345u;
        // Emit: add x0,x0,#imm ; ret  (aarch64: 8 bytes, straddles boundary at entry+4)
        //       lea rax,[rdi+imm]; ret (x86_64: 8 bytes, straddles boundary at entry+4)
        uint32_t imm = (s >> 16) & 0xfff;
        unsigned char *w = entry;
        int len = dbt_emit_add_imm(w, imm);
        dbt_flush(entry, (size_t)len);
        uint64_t (*f)(uint64_t) = (uint64_t (*)(uint64_t))entry;
        uint64_t x = 0x100 + (uint64_t)round;
        uint64_t got = f(x);
        uint64_t want = x + imm;
        if (got != want) {
            printf("smc-crosspage MISMATCH round=%d want=%llu got=%llu\n", round,
                   (unsigned long long)want, (unsigned long long)got);
            return 1;
        }
        acc = acc * 1000003ULL + got;
    }
    printf("smc-crosspage acc=%llu\n", (unsigned long long)acc);
    return 0;
}
