// Code-cache capacity pressure through JITted code: emit 8192 distinct leaf functions, each in its
// own slot of one large RWX arena, then invoke them in a scattered, data-dependent order so the hot
// working set far exceeds any translation cache and forces continuous eviction + re-translation of
// generated code. Each leaf returns a slot-unique constant; a wrong translation served after eviction
// diverges the checksum. Then a subset is REWRITTEN in place and re-run (SMC after eviction).
#include "dbt.h"

#define SLOTS 8192
#define SLOT_SZ 32

int main(void) {
    size_t sz = (size_t)SLOTS * SLOT_SZ;
    unsigned char *base = dbt_alloc(sz, PROT_READ | PROT_WRITE | PROT_EXEC);
    for (int i = 0; i < SLOTS; i++)
        dbt_emit_ret_imm(base + (size_t)i * SLOT_SZ, (i * 7 + 3) & 0x7fff);
    dbt_flush(base, sz);

    uint64_t acc = 0;
    unsigned k = 1;
    for (int pass = 0; pass < 4; pass++)
        for (int n = 0; n < SLOTS * 4; n++) {
            k = (k * 2654435761u + 1u) % SLOTS; // scattered slot order
            int (*f)(void) = (int (*)(void))(base + (size_t)k * SLOT_SZ);
            int want = (int)((k * 7 + 3) & 0x7fff);
            int got = f();
            if (got != want) {
                printf("smc-manyslots MISMATCH slot=%u want=%d got=%d\n", k, want, got);
                return 1;
            }
            acc = acc * 1000003ULL + (uint64_t)got;
        }

    // Rewrite every 64th slot with a new constant (SMC after the cache churned), re-run those.
    for (int i = 0; i < SLOTS; i += 64)
        dbt_emit_ret_imm(base + (size_t)i * SLOT_SZ, (i ^ 0x5AA5) & 0x7fff);
    dbt_flush(base, sz);
    for (int i = 0; i < SLOTS; i += 64) {
        int (*f)(void) = (int (*)(void))(base + (size_t)i * SLOT_SZ);
        int want = (int)((i ^ 0x5AA5) & 0x7fff);
        int got = f();
        if (got != want) {
            printf("smc-manyslots REWRITE MISMATCH slot=%d want=%d got=%d\n", i, want, got);
            return 1;
        }
        acc = acc * 31 + (uint64_t)got;
    }
    printf("smc-manyslots acc=%llu\n", (unsigned long long)acc);
    return 0;
}
