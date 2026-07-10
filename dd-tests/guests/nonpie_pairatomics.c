// aarch64 low non-PIE-address PAIR atomics: LDXP/STXP (exclusive pair LL/SC) and CASP (compare-and-swap
// pair) on a non-PIE ET_EXEC's low absolute .data. These formerly fell through nonpie_fixup (only single-
// register LDXR/STXR/CAS were served) -> the low-address fault re-raised on the SAME instruction forever
// (hang / timeout). Built -static -no-pie so g_nonpie_lo is armed; deterministic, diffed vs native aarch64.
#include <stdint.h>
#include <stdio.h>

static __attribute__((aligned(16))) uint64_t g_casp[2] = {0x1111111122222222ull, 0x3333333344444444ull};
static __attribute__((aligned(16))) uint64_t g_llsc[2] = {100, 200};

// CASP: atomically compare g_casp with {A,B}; on match swap in {C,D}. Returns the pre-op low word via x0.
static void do_casp(uint64_t *old_lo, uint64_t *old_hi) {
    register uint64_t x0 asm("x0") = 0x1111111122222222ull; // compare low (receives old low)
    register uint64_t x1 asm("x1") = 0x3333333344444444ull; // compare high (receives old high)
    register uint64_t x2 asm("x2") = 0xaaaaaaaabbbbbbbbull; // swap low
    register uint64_t x3 asm("x3") = 0xccccccccddddddddull; // swap high
    register uint64_t x5 asm("x5") = (uint64_t)(uintptr_t)g_casp;
    __asm__ volatile(".arch armv8.1-a\n\tcasp x0, x1, x2, x3, [x5]"
                     : "+r"(x0), "+r"(x1)
                     : "r"(x2), "r"(x3), "r"(x5)
                     : "memory");
    *old_lo = x0;
    *old_hi = x1;
}

// LDXP/STXP LL/SC loop: atomically add (1,2) to the 128-bit pair g_llsc, `n` times.
static void llsc_add(int n) {
    for (int i = 0; i < n; i++) {
        uint64_t ok;
        __asm__ volatile("1:\n\t"
                         "ldxp x0, x1, [%1]\n\t"
                         "add  x0, x0, #1\n\t"
                         "add  x1, x1, #2\n\t"
                         "stxp w2, x0, x1, [%1]\n\t"
                         "mov  %0, x2\n\t"
                         "cbnz w2, 1b\n\t"
                         : "=r"(ok)
                         : "r"((uint64_t)(uintptr_t)g_llsc)
                         : "x0", "x1", "x2", "memory");
        (void)ok;
    }
}

int main(void) {
    uint64_t olo = 0, ohi = 0;
    do_casp(&olo, &ohi);
    llsc_add(1000);
    printf("pairatomics casp_old=%016llx:%016llx casp_new=%016llx:%016llx llsc=%llu:%llu\n",
           (unsigned long long)olo, (unsigned long long)ohi, (unsigned long long)g_casp[0],
           (unsigned long long)g_casp[1], (unsigned long long)g_llsc[0], (unsigned long long)g_llsc[1]);
    return 0;
}
