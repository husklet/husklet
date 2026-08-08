// SMC detection, not merely SMC admission: patch a leaf that is HOT enough to have been translated.
// The sibling rwx-rewrite/minijit guests call each generated body exactly once, so the translator
// never builds the leaf and the interpreter serves every round -- they pass with SMC detection
// removed entirely. Here each body is called thousands of times before the next patch, so a stale
// translation of the previous body genuinely exists at that address and would be served.
// Deterministic checksum; the per-call check names the round that served stale code.
#include "dbt.h"

#define ROUNDS 240
#define HOT 4000

int main(void) {
    size_t sz = 4096;
    unsigned char *p = dbt_alloc(sz, PROT_READ | PROT_WRITE | PROT_EXEC);
    int (*f)(void) = (int (*)(void))p;
    uint64_t acc = 0;
    uint32_t s = 0x1234567u;
    for (int round = 0; round < ROUNDS; round++) {
        s = s * 1103515245u + 12345u;
        // Every round installs a distinct constant, so any translation retained from an earlier
        // round is detectable by value rather than only by timing.
        int imm = (int)((s >> 16) & 0x7fff);
        dbt_emit_ret_imm(p, imm);
        dbt_flush(p, sz); // a no-op on x86_64: the amd64 guest patches with no explicit flush
        for (int i = 0; i < HOT; i++) {
            int got = f();
            if (got != imm) {
                printf("smc-hotpatch MISMATCH round=%d call=%d want=%d got=%d\n", round, i, imm, got);
                // Exit 3, not 1, so a stale translation is distinguishable in the runner's exit
                // report from `dbt_alloc` failing the mmap.
                return 3;
            }
            acc = acc * 1000003ULL + (uint64_t)got;
        }
    }
    printf("smc-hotpatch acc=%llu\n", (unsigned long long)acc);
    return 0;
}
