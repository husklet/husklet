// The six MMX <-> floating-point conversions: CVTPI2PS/PD (0F 2A), CVTTPS2PI/CVTTPD2PI (0F 2C) and
// CVTPS2PI/CVTPD2PI (0F 2D). Three things are checked at once and each has been wrong here:
//   * the RESULT under all four MXCSR rounding modes -- 2D rounds by RC, 2C always truncates;
//   * the "integer indefinite" 0x80000000 an out-of-range or NaN source must produce;
//   * the MXCSR STICKY FLAGS, which are guest-visible engine state on a host that runs guest FP natively.
// The flags are the sharp edge: the unused high lanes of a 4-wide host conversion must see ZEROES, not the
// source's own upper half (CVTPS2PI reads only two of xmm's four floats -- the 0x7f poison below is the
// probe for that), and the conversion the guest did NOT ask for must never execute at all. A compiler that
// if-converts `truncating ? cvtt : cvt` into both instructions plus a select was observed ORing the
// not-taken one's #I/#P into these flags.
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static unsigned get_mxcsr(void) {
    unsigned v;
    __asm__ volatile("stmxcsr %0" : "=m"(v));
    return v;
}

static void set_mxcsr(unsigned v) {
    __asm__ volatile("ldmxcsr %0" : : "m"(v));
}

static void clear_flags(void) {
    set_mxcsr(get_mxcsr() & ~0x3fu);
}

static unsigned flags(void) {
    return get_mxcsr() & 0x3f;
}

static void set_rc(unsigned rc) {
    set_mxcsr((get_mxcsr() & ~0x6000u) | (rc << 13));
}

static void cvtpi2ps(const int32_t in[2], const unsigned char seed[16], unsigned char out[16]) {
    __asm__ volatile("movdqu %1, %%xmm0\n\tcvtpi2ps %2, %%xmm0\n\tmovdqu %%xmm0, %0\n\temms"
                     : "=m"(*(char(*)[16])out)
                     : "m"(*(const char(*)[16])seed), "m"(*(const char(*)[8])in)
                     : "xmm0", "memory");
}

static void cvtpi2pd(const int32_t in[2], const unsigned char seed[16], unsigned char out[16]) {
    __asm__ volatile("movdqu %1, %%xmm0\n\tcvtpi2pd %2, %%xmm0\n\tmovdqu %%xmm0, %0\n\temms"
                     : "=m"(*(char(*)[16])out)
                     : "m"(*(const char(*)[16])seed), "m"(*(const char(*)[8])in)
                     : "xmm0", "memory");
}

// A REGISTER source, so the xmm upper half really is the poison and not an implicit zero.
#define CVT_TO_PI(name, mnemonic)                                                                                      \
    static void name(const unsigned char in[16], int32_t out[2]) {                                                     \
        __asm__ volatile("movdqu %1, %%xmm4\n\t" mnemonic " %%xmm4, %%mm1\n\tmovq %%mm1, %0\n\temms"                   \
                         : "=m"(*(char(*)[8])out)                                                                      \
                         : "m"(*(const char(*)[16])in)                                                                 \
                         : "xmm4", "mm1", "memory");                                                                   \
    }
CVT_TO_PI(cvtps2pi, "cvtps2pi")
CVT_TO_PI(cvttps2pi, "cvttps2pi")
CVT_TO_PI(cvtpd2pi, "cvtpd2pi")
CVT_TO_PI(cvttpd2pi, "cvttpd2pi")

static void hex(const char *tag, const void *p, int n) {
    const unsigned char *b = p;
    printf("%s:", tag);
    for (int i = 0; i < n; i++)
        printf(" %02x", b[i]);
}

static const float fsrc[][4] = {
    {1.5f, -1.5f, 0, 0},
    {2.5f, -2.5f, 0, 0},                   // ties: RC decides, and 2C never rounds
    {0.5f, -0.5f, 0, 0},
    {2147483648.0f, -2147483904.0f, 0, 0}, // both out of int32 range -> indefinite
    {2147483520.0f, -2147483648.0f, 0, 0}, // largest representable in range, and exactly INT_MIN
    {1.0f / 0.0f, -1.0f / 0.0f, 0, 0},
    {0.0f / 0.0f, 3.0f, 0, 0},             // NaN -> indefinite, with a good lane beside it
    {1e-45f, -1e-45f, 0, 0},               // denormals
};

static const double dsrc[][2] = {
    {1.5, -1.5},   {2.5, -2.5},
    {0.5, -0.5},   {2147483648.0, -2147483649.0},
    {2147483647.0, -2147483648.0}, {1.0 / 0.0, -1.0 / 0.0},
    {0.0 / 0.0, 3.0}, {4.9e-324, -4.9e-324},
};

static const int32_t isrc[][2] = {
    {0, 1}, {-1, INT32_MIN}, {INT32_MAX, -16777217}, {16777217, -16777217}, // the last two are inexact as float
};

int main(void) {
    unsigned char seed[16], out[16], in[16];
    memset(seed, 0xa5, sizeof seed); // CVTPI2PS merges: bytes 8..15 must survive
    static const char *rcname[4] = {"near", "down", "up", "zero"};

    for (unsigned rc = 0; rc < 4; rc++) {
        set_rc(rc);
        for (unsigned i = 0; i < sizeof isrc / sizeof isrc[0]; i++) {
            clear_flags();
            cvtpi2ps(isrc[i], seed, out);
            hex("cvtpi2ps", out, 16);
            printf(" rc=%s mx=%02x\n", rcname[rc], flags());
            clear_flags();
            cvtpi2pd(isrc[i], seed, out);
            hex("cvtpi2pd", out, 16);
            printf(" rc=%s mx=%02x\n", rcname[rc], flags());
        }
        for (unsigned i = 0; i < sizeof fsrc / sizeof fsrc[0]; i++) {
            int32_t r[2];
            memcpy(in, fsrc[i], 16);
            memset(in + 8, 0x7f, 8); // poison the lanes CVT*PS2PI must not convert
            clear_flags();
            cvtps2pi(in, r);
            hex("cvtps2pi", r, 8);
            printf(" rc=%s mx=%02x\n", rcname[rc], flags());
            clear_flags();
            cvttps2pi(in, r);
            hex("cvttps2pi", r, 8);
            printf(" rc=%s mx=%02x\n", rcname[rc], flags());
        }
        for (unsigned i = 0; i < sizeof dsrc / sizeof dsrc[0]; i++) {
            int32_t r[2];
            memcpy(in, dsrc[i], 16);
            clear_flags();
            cvtpd2pi(in, r);
            hex("cvtpd2pi", r, 8);
            printf(" rc=%s mx=%02x\n", rcname[rc], flags());
            clear_flags();
            cvttpd2pi(in, r);
            hex("cvttpd2pi", r, 8);
            printf(" rc=%s mx=%02x\n", rcname[rc], flags());
        }
    }
    set_rc(0);
    return 0;
}
