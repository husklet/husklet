// translit/flags -- the flag substrate at a transliterated block boundary.
//
// Inside a transliterated block the guest's flags ARE the host's flags; at every exit they are converted
// to cpu->{nzcv,pf,af,df} and back on the next entry (translit.inc translit_flags_in/out). PF is the one
// that is not a bit copy -- cpu->pf stores a BYTE whose parity is the flag -- so a polarity error there is
// silent: no crash, just a different answer. Ordinary guest programs do not detect it (measured: busybox
// ls/awk/sort, python3, perl and gcc all produce byte-identical output with the polarity inverted), which
// is why this fixture exists.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
// Flag substrate across block boundaries: CF/ZF/SF/OF/PF/AF/DF must survive
// translit_flags_out -> cpu -> translit_flags_in and mixed interpreted blocks.
static volatile uint64_t sink;

__attribute__((noinline)) static uint64_t adc_chain(uint64_t a, uint64_t b) {
    uint64_t lo, hi = 0;
    __asm__ volatile("add %2,%0\n\tadc $0,%1" : "=r"(lo), "+r"(hi) : "r"(b), "0"(a) : "cc");
    return lo ^ (hi << 1);
}

__attribute__((noinline)) static int all_cc(uint64_t a, uint64_t b) {
    unsigned char r[16];
    __asm__ volatile("cmp %[b],%[a]\n\t"
                     "seto %0\n\tsetno %1\n\tsetb %2\n\tsetae %3\n\tsete %4\n\tsetne %5\n\t"
                     "setbe %6\n\tseta %7\n\tsets %8\n\tsetns %9\n\tsetp %10\n\tsetnp %11\n\t"
                     "setl %12\n\tsetge %13\n\tsetle %14\n\tsetg %15\n"
                     : "=m"(r[0]), "=m"(r[1]), "=m"(r[2]), "=m"(r[3]), "=m"(r[4]), "=m"(r[5]), "=m"(r[6]), "=m"(r[7]),
                       "=m"(r[8]), "=m"(r[9]), "=m"(r[10]), "=m"(r[11]), "=m"(r[12]), "=m"(r[13]), "=m"(r[14]),
                       "=m"(r[15])
                     : [a] "r"(a), [b] "r"(b)
                     : "cc");
    int v = 0;
    for (int i = 0; i < 16; i++)
        v |= (r[i] & 1) << i;
    return v;
}

// A call between the flag producer and the flag consumer forces a block exit,
// so the flags must round-trip through cpu->nzcv/pf/af/df.
__attribute__((noinline)) static void barrier(void) {
    sink++;
}

__attribute__((noinline)) static int cc_across_block(uint64_t a, uint64_t b) {
    unsigned char p, af, c;
    __asm__ volatile("cmp %[b],%[a]" : : [a] "r"(a), [b] "r"(b) : "cc");
    // no barrier: same block
    __asm__ volatile("setp %0\n\tsetb %1" : "=m"(p), "=m"(c)::"cc");
    (void)af;
    return (p & 1) | ((c & 1) << 1);
}

__attribute__((noinline)) static uint64_t df_copy(void) {
    char src[64], dst[64];
    for (int i = 0; i < 64; i++)
        src[i] = (char)(i * 7 + 1);
    memset(dst, 0, sizeof dst);
    // std; rep movsb backwards; cld
    void *s = src + 63, *d = dst + 63;
    size_t n = 64;
    __asm__ volatile("std\n\trep movsb\n\tcld" : "+D"(d), "+S"(s), "+c"(n)::"memory", "cc");
    uint64_t h = 1469598103934665603ull;
    for (int i = 0; i < 64; i++) {
        h ^= (unsigned char)dst[i];
        h *= 1099511628211ull;
    }
    return h;
}

// AF is the one flag with no direct read in 64-bit mode except through the
// lahf/pushf paths; exercise it through an interpreted pushfq after a
// transliterated arithmetic op.
__attribute__((noinline)) static uint64_t raw_eflags(uint64_t a, uint64_t b) {
    uint64_t f;
    __asm__ volatile("add %[b],%[a]\n\tpushfq\n\tpop %[f]" : [f] "=r"(f), [a] "+r"(a) : [b] "r"(b) : "cc");
    return f & 0x8D5; // CF PF AF ZF SF OF
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0); // unbuffered: the ordering of a forked child\'s output is part of the comparison
    uint64_t h = 0;
    for (uint64_t i = 0; i < 4096; i++) {
        uint64_t a = i * 0x9E3779B97F4A7C15ull, b = (i ^ 0xdeadbeef) * 0x1234567;
        h = h * 31 + adc_chain(a, b);
        h = h * 31 + (uint64_t)all_cc(a, b);
        barrier();
        h = h * 31 + (uint64_t)cc_across_block(a, b);
        h = h * 31 + raw_eflags(a, b);
    }
    printf("flags h=%016llx df=%016llx sink=%llu\n", (unsigned long long)h, (unsigned long long)df_copy(),
           (unsigned long long)sink);
    return 0;
}
