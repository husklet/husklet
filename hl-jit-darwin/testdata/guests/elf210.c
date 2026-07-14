// #210 — deterministic guest for the x86_64 ELF-loader fixed-base collision fallback.
// The workload is irrelevant; what matters is that the image LOADS and RUNS byte-exact when the
// loader's fixed-VA (PC_IMG_BASE) map is forced to collide (HL_X_FORCE_BASE_COLLIDE) and falls back to a
// kernel-chosen base. A little compute + a stdout line proves the biased/rebased image executes correctly.
#include <stdio.h>

int main(void) {
    unsigned long acc = 1469598103934665603ul; // FNV-ish mix so a mis-placed load would diverge loudly
    for (unsigned long i = 1; i <= 200000ul; i++) {
        acc ^= i;
        acc *= 1099511628211ul;
        acc ^= acc >> 29;
    }
    printf("elf210 base-collide ok acc=%016lx\n", acc);
    return 0;
}
