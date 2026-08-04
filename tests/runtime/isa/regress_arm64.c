/* AArch64 instruction-lowering regression corpus.
 *
 * Every check below is a translator divergence that the differential ISA fuzzer
 * (tests/fuzz/isa/aarch64) found by running the SAME static binary natively on the ARM64 host and
 * under the engine, and that has since been fixed. The golden output is the native host's, so a
 * regression in any of these lowerings shows up as an exact stdout mismatch.
 *
 * Built -static -no-pie (see the Makefile rule) on purpose: a non-PIE ET_EXEC arms g_nonpie_lo,
 * which is what makes the translator (a) upgrade every ldxr/stxr retry loop to a single LSE atomic
 * and (b) bias-fold low absolute addresses. Both fixed bugs live on exactly those paths.
 *
 * Deterministic by construction: fixed inputs, no environment, no time, no addresses printed.
 */

#include <stdint.h>
#include <stdio.h>

/* A value no store-exclusive can ever produce: the status register is W-sized, so a real
 * `stxr Ws,...` leaves 0 or 1 with the top half zeroed. If the sentinel survives in ANY bit, the
 * lowering skipped the architectural write. */
#define SENTINEL 0xdeadbeefcafef00dULL

static __attribute__((aligned(16))) uint64_t g_word[4] = {0x0102030405060708ULL, 0x1122334455667788ULL,
                                                          0x99aabbccddeeff00ULL, 0x0f0f0f0f0f0f0f0fULL};
static __attribute__((aligned(16))) uint32_t g_word32[4] = {0x01020304u, 0x11223344u, 0x99aabbccu, 0x0f0f0f0fu};

/* ---------------------------------------------------------------------------------------------
 * The LSE upgrade replaces a whole ldxr/stxr RETRY LOOP with one atomic instruction. The loop can
 * only fall out of `cbnz Ws, loop` with the store-exclusive status register Ws == 0, so the
 * rewrite owes the guest that write; a single LSE op never touches Ws. Each helper reports it.
 * ------------------------------------------------------------------------------------------- */

static uint64_t swp_loop(uint64_t *p, uint64_t v, uint64_t *status) {
    uint64_t old, st = SENTINEL;
    __asm__ volatile("1: ldxr %[old], [%[p]]\n\t"
                     "   stxr %w[st], %[v], [%[p]]\n\t"
                     "   cbnz %w[st], 1b\n\t"
                     : [old] "=&r"(old), [st] "+&r"(st)
                     : [p] "r"(p), [v] "r"(v)
                     : "memory");
    *status = st;
    return old;
}

#define RMW_LOOP(name, op)                                                                                             \
    static uint64_t name(uint64_t *p, uint64_t v, uint64_t *status) {                                                  \
        uint64_t old, nv, st = SENTINEL;                                                                               \
        __asm__ volatile("1: ldxr %[old], [%[p]]\n\t" op                                                               \
                         "   stxr %w[st], %[nv], [%[p]]\n\t"                                                           \
                         "   cbnz %w[st], 1b\n\t"                                                                      \
                         : [old] "=&r"(old), [nv] "=&r"(nv), [st] "+&r"(st)                                            \
                         : [p] "r"(p), [v] "r"(v)                                                                      \
                         : "memory");                                                                                  \
        *status = st;                                                                                                  \
        return old;                                                                                                    \
    }

RMW_LOOP(add_loop, "   add %[nv], %[old], %[v]\n\t")
RMW_LOOP(orr_loop, "   orr %[nv], %[old], %[v]\n\t")
RMW_LOOP(eor_loop, "   eor %[nv], %[old], %[v]\n\t")
RMW_LOOP(and_loop, "   and %[nv], %[old], %[v]\n\t")
RMW_LOOP(sub_loop, "   sub %[nv], %[old], %[v]\n\t")

/* fetch-add of a literal: the dedicated immediate form of the matcher, which additionally borrows
 * Ws as the constant holder before the atomic. */
static uint64_t addimm_loop(uint64_t *p, uint64_t *status) {
    uint64_t old, nv, st = SENTINEL;
    __asm__ volatile("1: ldxr %[old], [%[p]]\n\t"
                     "   add %[nv], %[old], #1365\n\t"
                     "   stxr %w[st], %[nv], [%[p]]\n\t"
                     "   cbnz %w[st], 1b\n\t"
                     : [old] "=&r"(old), [nv] "=&r"(nv), [st] "+&r"(st)
                     : [p] "r"(p)
                     : "memory");
    *status = st;
    return old;
}

/* 32-bit variant: the matcher runs for sz==2 as well, and the status write is W-sized there too. */
static uint32_t add_loop32(uint32_t *p, uint32_t v, uint64_t *status) {
    uint32_t old, nv;
    uint64_t st = SENTINEL;
    __asm__ volatile("1: ldxr %w[old], [%[p]]\n\t"
                     "   add %w[nv], %w[old], %w[v]\n\t"
                     "   stxr %w[st], %w[nv], [%[p]]\n\t"
                     "   cbnz %w[st], 1b\n\t"
                     : [old] "=&r"(old), [nv] "=&r"(nv), [st] "+&r"(st)
                     : [p] "r"(p), [v] "r"(v)
                     : "memory");
    *status = st;
    return old;
}

/* The CAS loop is the one shape whose status register is NOT unconditionally zeroed: the guest
 * reaches `stxr Ws` only when the compare matched. On the b.ne-out path Ws keeps its pre-loop
 * value -- ALL SIXTY-FOUR BITS of it (a 32-bit conditional select would silently zero the top
 * half, which is exactly what the fuzzer caught). */
static uint64_t cas_loop(uint64_t *p, uint64_t expected, uint64_t desired, uint64_t *status) {
    uint64_t old, st = SENTINEL;
    __asm__ volatile("1: ldxr %[old], [%[p]]\n\t"
                     "   cmp %[old], %[exp]\n\t"
                     "   b.ne 2f\n\t"
                     "   stxr %w[st], %[des], [%[p]]\n\t"
                     "   cbnz %w[st], 1b\n\t"
                     "2:\n\t"
                     : [old] "=&r"(old), [st] "+&r"(st)
                     : [p] "r"(p), [exp] "r"(expected), [des] "r"(desired)
                     : "memory", "cc");
    *status = st;
    return old;
}

/* ---------------------------------------------------------------------------------------------
 * CASP/CASPA/CASPL/CASPAL share the load/store-EXCLUSIVE encoding box (o2 == 0, and the acquire
 * bit reuses the L bit), so a plain `(in & 0x3FC00000) == 0x08400000` test reads CASPA/CASPAL as a
 * load-exclusive. That latched the translator's in_excl flag ON for the rest of the block, which
 * disables the non-PIE bias fold for EVERY following memory operation -- a fatal SIGSEGV on the
 * next low absolute access. The accesses after the CASPA below must all still work.
 * ------------------------------------------------------------------------------------------- */

static __attribute__((aligned(16))) uint64_t g_pair[2] = {0x1111111122222222ULL, 0x3333333344444444ULL};
static __attribute__((aligned(16))) uint64_t g_after_atomic = 0x00000000000000ffULL;
static __attribute__((aligned(16))) uint64_t g_after_plain = 0x5a5a5a5a5a5a5a5aULL;

static void caspa_then_low_accesses(uint64_t *old_lo, uint64_t *old_hi, uint64_t *atomic_old, uint64_t *plain) {
    register uint64_t x0 __asm__("x0") = 0x1111111122222222ULL;
    register uint64_t x1 __asm__("x1") = 0x3333333344444444ULL;
    register uint64_t x2 __asm__("x2") = 0xaaaaaaaabbbbbbbbULL;
    register uint64_t x3 __asm__("x3") = 0xccccccccddddddddULL;
    uint64_t ao, pv;
    /* One asm block, so all of this is one translated block and the stuck in_excl flag would
     * still be set when the two low-address accesses below are lowered. */
    __asm__ volatile(".arch armv8.1-a\n\t"
                     "caspa x0, x1, x2, x3, [%[pair]]\n\t"
                     "ldumaxal %[one], %[ao], [%[at]]\n\t"
                     "ldsminal %[one], %[ao], [%[at]]\n\t"
                     "swpal %[one], %[ao], [%[at]]\n\t"
                     "ldaddal %[one], %[ao], [%[at]]\n\t"
                     "ldr %[pv], [%[pl]]\n\t"
                     : "+r"(x0), "+r"(x1), [ao] "=&r"(ao), [pv] "=&r"(pv)
                     : "r"(x2), "r"(x3), [pair] "r"(g_pair), [at] "r"(&g_after_atomic), [pl] "r"(&g_after_plain),
                       [one] "r"(0x100ULL)
                     : "memory");
    *old_lo = x0;
    *old_hi = x1;
    *atomic_old = ao;
    *plain = pv;
}

/* ------------------------------------------------------------------------------------------- */

static void report(const char *tag, uint64_t old, uint64_t status, uint64_t mem) {
    printf("%-12s old=%016llx status=%016llx mem=%016llx\n", tag, (unsigned long long)old,
           (unsigned long long)status, (unsigned long long)mem);
}

/* BLR reads its target before writing the link register. This matters specifically
 * for `blr x30`: stealing guest x30 must not turn the newly written return address
 * into the branch target. */
static __attribute__((noinline)) uint64_t blr_x30(void) {
    uint64_t value;
    __asm__ volatile("adr x30, 1f\n\t"
                     "blr x30\n\t"
                     "b 2f\n\t"
                     "1: mov %[value], #0x30\n\t"
                     "ret\n\t"
                     "2:\n\t"
                     : [value] "=r"(value)
                     :
                     : "x30");
    return value;
}

int main(void) {
    uint64_t st, old;

    old = swp_loop(&g_word[0], 0xfeedfacedeadbeefULL, &st);
    report("swp", old, st, g_word[0]);

    old = add_loop(&g_word[1], 0x0000000100000001ULL, &st);
    report("ldadd", old, st, g_word[1]);
    old = orr_loop(&g_word[1], 0x00000000000000ffULL, &st);
    report("ldset", old, st, g_word[1]);
    old = eor_loop(&g_word[1], 0xffffffff00000000ULL, &st);
    report("ldeor", old, st, g_word[1]);
    old = and_loop(&g_word[1], 0x0f0f0f0f0f0f0f0fULL, &st);
    report("ldclr", old, st, g_word[1]);
    old = sub_loop(&g_word[1], 0x0000000000010001ULL, &st);
    report("ldadd-neg", old, st, g_word[1]);

    old = addimm_loop(&g_word[2], &st);
    report("ldadd-imm", old, st, g_word[2]);

    uint32_t o32 = add_loop32(&g_word32[1], 0x01010101u, &st);
    report("ldadd32", o32, st, g_word32[1]);

    /* CAS that MATCHES: the loop ran the stxr, so the status is architecturally 0. */
    old = cas_loop(&g_word[3], 0x0f0f0f0f0f0f0f0fULL, 0x7777777777777777ULL, &st);
    report("cas-hit", old, st, g_word[3]);
    /* CAS that MISSES: the loop branched out before the stxr, so the status keeps every one of
     * its 64 pre-loop bits. */
    old = cas_loop(&g_word[3], 0xdeadbeefdeadbeefULL, 0x0123456789abcdefULL, &st);
    report("cas-miss", old, st, g_word[3]);

    uint64_t lo, hi, ao, pv;
    caspa_then_low_accesses(&lo, &hi, &ao, &pv);
    printf("caspa        old=%016llx:%016llx new=%016llx:%016llx\n", (unsigned long long)lo,
           (unsigned long long)hi, (unsigned long long)g_pair[0], (unsigned long long)g_pair[1]);
    printf("after-caspa  atomic_old=%016llx atomic_now=%016llx plain=%016llx\n", (unsigned long long)ao,
           (unsigned long long)g_after_atomic, (unsigned long long)pv);
    printf("blr-x30      value=%016llx\n", (unsigned long long)blr_x30());
    return 0;
}
