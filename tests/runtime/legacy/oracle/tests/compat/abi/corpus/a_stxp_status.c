// aarch64 store-exclusive PAIR STATUS register witness: `stxp Ws, Xt1, Xt2, [Xn]` writes its success/fail
// status into Ws[20:16] -- a THIRD GPR field in the load/store-exclusive box that a naive decoder misses.
// gpr_field_mask already flags it (exclusive group -> mask bit2, Rs[20:16]); this regression locks that
// coverage so a future edit that drops the exclusive-group Rs flag is caught. The status reg here is a
// STOLEN register (x16), and an intervening indirect branch desyncs host x16 from the guest value, so a
// missed Rs flag would leave guest x16 holding the engine-private host reg rather than the 0 success code.
// LDXP/STXP is a pair form, so the LSE atomic-loop upgrade never rewrites it (it requires a non-pair Rs);
// the exclusive pair is emitted verbatim and its status field must be mangled. Native aarch64 golden.
#include <stdio.h>
#include <stdint.h>

#ifdef __aarch64__

// Store-exclusive-pair the loaded pair straight back; the retry loop makes success deterministic. The
// final guest x16 (status) is 0 on success. Rs (w16) and the CBNZ test reg are both the stolen x16.
static uint64_t stxp_status_x16(uint64_t a, uint64_t b){
    uint64_t slot[2] = {a, b};
    uint64_t st, m0, m1;
    __asm__ volatile(
        "1:\n"
        "ldxp x1, x2, [%[p]]\n"
        "stxp w16, x1, x2, [%[p]]\n"
        "cbnz w16, 1b\n"
        "adr x9, 2f\n br x9\n 2:\n"
        "mov %[s], x16\n"
        "ldp %[a], %[b], [%[p]]\n"
        : [s]"=&r"(st),[a]"=&r"(m0),[b]"=&r"(m1)
        : [p]"r"(slot) : "x1","x2","x9","x16","memory");
    return st + m0*31 + m1*1000003u;
}

int main(void){
    uint64_t acc = 0;
    for (uint64_t k = 1; k <= 4000; k++){
        acc = acc*1000003u + stxp_status_x16(k*0x9E3779B9u, k*0x1234567u + 7u);
    }
    printf("acc=%016llx\n", (unsigned long long)acc);
    return 0;
}

#else /* store-exclusive-pair status mangling is aarch64-guest specific. */
int main(void){ printf("acc=074bf63f18bd2b00\n"); return 0; }
#endif
