// aarch64 CASP/CASPA/CASPL/CASPAL (paired compare-and-swap; the DWCAS / __int128 lock-free primitive)
// whose Rs pair (Xs,Xs+1), Rt pair (Xt,Xt+1), or base Xn names a STOLEN register (x16/x17). The mangle
// machinery only substitutes NAMED register fields, so the implicit pair partner (Xs+1 / Xt+1) slipped
// through verbatim and read/wrote the engine-private host register -> the high half of the compare saw
// garbage and the swap spuriously failed, silently corrupting the atomic. An intervening indirect branch
// forces host x16/x17 out of sync with the guest registers (else the preceding `mov` mangle leaves the
// value residually in the host reg and hides the bug). Deterministic golden from native aarch64.
#include <stdio.h>
#include <stdint.h>

#ifdef __aarch64__

#define SPLIT "adr x9, 1f\n br x9\n 1:\n"

// CASPAL Xs,Xs+1(=x16,x17), Xnew0,Xnew1, [Xn]: expected pair in x16/x17, new value in x4/x5.
static void casp64_rs16(uint64_t m0, uint64_t m1, uint64_t e0, uint64_t e1,
                        uint64_t n0, uint64_t n1, uint64_t *o0, uint64_t *o1, uint64_t *r0, uint64_t *r1){
    uint64_t mem[2] __attribute__((aligned(16))) = {m0, m1};
    uint64_t s0, s1;
    __asm__ volatile(".arch armv8.2-a\n"
        "mov x16,%[e0]\n mov x17,%[e1]\n mov x4,%[n0]\n mov x5,%[n1]\n" SPLIT
        "caspal x16, x17, x4, x5, [%[p]]\n mov %[s0],x16\n mov %[s1],x17\n"
        : [s0]"=r"(s0), [s1]"=r"(s1)
        : [e0]"r"(e0),[e1]"r"(e1),[n0]"r"(n0),[n1]"r"(n1),[p]"r"(mem)
        : "x9","x16","x17","x4","x5","memory");
    *o0 = s0; *o1 = s1; *r0 = mem[0]; *r1 = mem[1];
}
// 32-bit CASPA with the NEW-value pair in the stolen regs (w16,w17 as Rt).
static void casp32_rt16(uint32_t m0, uint32_t m1, uint32_t e0, uint32_t e1,
                        uint32_t n0, uint32_t n1, uint32_t *r0, uint32_t *r1){
    uint32_t mem[2] __attribute__((aligned(8))) = {m0, m1};
    __asm__ volatile(".arch armv8.2-a\n"
        "mov w4,%w[e0]\n mov w5,%w[e1]\n mov w16,%w[n0]\n mov w17,%w[n1]\n" SPLIT
        "caspa w4, w5, w16, w17, [%[p]]\n"
        :: [e0]"r"(e0),[e1]"r"(e1),[n0]"r"(n0),[n1]"r"(n1),[p]"r"(mem)
        : "x9","x16","x17","x4","x5","memory");
    *r0 = mem[0]; *r1 = mem[1];
}
// 64-bit CASP (no ordering) with the BASE pointer in a stolen reg (x17 as Xn).
static void casp64_rn17(uint64_t m0, uint64_t m1, uint64_t e0, uint64_t e1,
                        uint64_t n0, uint64_t n1, uint64_t *r0, uint64_t *r1){
    uint64_t mem[2] __attribute__((aligned(16))) = {m0, m1};
    __asm__ volatile(".arch armv8.2-a\n"
        "mov x4,%[e0]\n mov x5,%[e1]\n mov x6,%[n0]\n mov x7,%[n1]\n mov x17,%[p]\n" SPLIT
        "casp x4, x5, x6, x7, [x17]\n"
        :: [e0]"r"(e0),[e1]"r"(e1),[n0]"r"(n0),[n1]"r"(n1),[p]"r"(mem)
        : "x9","x17","x4","x5","x6","x7","memory");
    *r0 = mem[0]; *r1 = mem[1];
}

int main(void){
    uint64_t acc = 0;
    for (uint64_t k = 1; k <= 1500; k++){
        uint64_t o0,o1,r0,r1;
        // alternate matching / non-matching expected pairs to exercise both branches
        uint64_t m0 = k*0x1111u, m1 = k*0x2222u;
        uint64_t e0 = (k & 1) ? m0 : m0 ^ 1;               // odd k -> match, even k -> mismatch
        casp64_rs16(m0, m1, e0, m1, k*7u, k*9u, &o0,&o1,&r0,&r1);
        acc = acc*1000003u + o0; acc = acc*1000003u + o1;
        acc = acc*1000003u + r0; acc = acc*1000003u + r1;

        uint32_t q0,q1;
        uint32_t a0 = (uint32_t)(k*3u), a1 = (uint32_t)(k*5u);
        uint32_t xe0 = (k & 2) ? a0 : a0 + 1;
        casp32_rt16(a0, a1, xe0, a1, (uint32_t)(k*13u), (uint32_t)(k*17u), &q0,&q1);
        acc = acc*1000003u + q0; acc = acc*1000003u + q1;

        uint64_t z0,z1;
        uint64_t b0 = k*0x3333u, b1 = k*0x4444u;
        uint64_t be0 = (k & 4) ? b0 : b0 + 1;
        casp64_rn17(b0, b1, be0, b1, k*19u, k*23u, &z0,&z1);
        acc = acc*1000003u + z0; acc = acc*1000003u + z1;
    }
    printf("acc=%016llx\n", (unsigned long long)acc);
    return 0;
}

#else /* CASP is an aarch64-guest instruction; satisfy the x86_64 corpus-wildcard build only. */
int main(void){ printf("acc=%016llx\n", 0ull); return 0; }
#endif
