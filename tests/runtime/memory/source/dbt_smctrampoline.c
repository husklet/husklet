// Trampoline patching: a direct branch at a fixed entry is re-pointed between two leaf targets by
// rewriting the branch displacement in place -- the exact mechanism a JIT uses to relink call sites /
// patch inline-cache stubs. The engine must invalidate the CHAINED direct-branch link when the branch
// bytes change and resolve to the new target, not stay chained to the old block. Deterministic.
#include "dbt.h"

// Patch a direct unconditional branch at `site` to jump to `target`. site/target within same arena.
static void patch_branch(unsigned char *site, unsigned char *target) {
#if defined(__aarch64__)
    long delta = (long)(target - site);
    uint32_t imm26 = (uint32_t)((delta >> 2) & 0x3ffffff);
    *(uint32_t *)site = 0x14000000u | imm26; // b <target>
#elif defined(__x86_64__)
    long rel = (long)(target - (site + 5));
    site[0] = 0xE9; // jmp rel32
    site[1] = (unsigned char)(rel & 0xff);
    site[2] = (unsigned char)((rel >> 8) & 0xff);
    site[3] = (unsigned char)((rel >> 16) & 0xff);
    site[4] = (unsigned char)((rel >> 24) & 0xff);
#else
    (void)site; (void)target;
#endif
}

int main(void) {
    size_t sz = 4096;
    unsigned char *p = dbt_alloc(sz, PROT_READ | PROT_WRITE | PROT_EXEC);
    unsigned char *entry = p;             // the trampoline branch lives here
    unsigned char *leafA = p + 0x100;     // return 0xA1
    unsigned char *leafB = p + 0x200;     // return 0xB2
    dbt_emit_ret_imm(leafA, 0xA1);
    dbt_emit_ret_imm(leafB, 0xB2);
    int (*f)(void) = (int (*)(void))entry;

    uint64_t acc = 0;
    for (int i = 0; i < 20000; i++) {
        unsigned char *tgt = (i & 1) ? leafB : leafA;
        int want = (i & 1) ? 0xB2 : 0xA1;
        patch_branch(entry, tgt);
        dbt_flush(p, sz);
        int got = f();
        if (got != want) {
            printf("smc-trampoline MISMATCH i=%d want=%d got=%d\n", i, want, got);
            return 1;
        }
        acc = acc * 31 + (uint64_t)got;
    }
    printf("smc-trampoline acc=%llu\n", (unsigned long long)acc);
    return 0;
}
