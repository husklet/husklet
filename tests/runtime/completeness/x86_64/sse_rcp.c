// Legacy (non-VEX) RCPPS/RCPSS (0F 53) and RSQRTPS/RSQRTSS (0F 52) -- baseline SSE1, so no CPUID gate
// excuses them, and they were UNIMPL on the aarch64 host (hard engine abort) while only the VEX forms
// were lowered.
//
// WHY NO RAW BIT PATTERNS FOR THE NORMAL RANGE. The SDM specifies these as an approximation with
// |relative error| <= 1.5*2^-12 and never specifies a value; the 12-bit table is microarchitecture-
// specific (unlike VRCP14PS, which IS defined). A golden of raw bits would pin one vendor's ROM. So this
// asserts the architectural contract instead -- the error bound, the exact special-value results, the
// scalar merge rule, and "no SIMD FP exception whatsoever" -- which native silicon and both engine hosts
// must all satisfy. Measured on native Zen 4: rcpps worst relative error 2^-11.63 over [1,4),
// rsqrtps 2^-11.92, so the bound is real and the exact reciprocal (error 0) also satisfies it.
//
// Every instruction under test is one volatile asm block over a volatile source: gcc constant-folds
// intrinsic sequences, if-converts a ternary into BOTH arms, and hoists SSE arithmetic across LDMXCSR.
// MXCSR is read as a raw bit pattern -- never fetestexcept, whose glibc helper raises the flags a probe
// is measuring. Inputs are chosen so no reciprocal is denormal: hardware flushes a denormal RESULT to
// zero here and a full-precision model does not, which is outside the architectural contract either way.
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static uint8_t Src[16] __attribute__((aligned(16)));
static uint8_t Dst[16] __attribute__((aligned(16)));
static volatile float g_in;
static unsigned g_mx;

#define UNARY(nm, insn)                                                                                                \
    static void nm(void) {                                                                                             \
        __asm__ __volatile__("movaps %1, %%xmm0\n\t" insn " %%xmm0, %%xmm1\n\tmovaps %%xmm1, %0"                       \
                             : "=m"(Dst)                                                                               \
                             : "m"(Src)                                                                                \
                             : "xmm0", "xmm1", "memory");                                                              \
    }
// The scalar forms MERGE: xmm1 keeps its bits 127:32 and only lane 0 is written.
#define SCALAR(nm, insn)                                                                                               \
    static void nm(void) {                                                                                             \
        __asm__ __volatile__("movaps %1, %%xmm0\n\tmovaps %0, %%xmm1\n\t" insn " %%xmm0, %%xmm1\n\tmovaps %%xmm1, %0"  \
                             : "+m"(Dst)                                                                               \
                             : "m"(Src)                                                                                \
                             : "xmm0", "xmm1", "memory");                                                              \
    }
UNARY(rcpps, "rcpps")
UNARY(rsqrtps, "rsqrtps")
SCALAR(rcpss, "rcpss")
SCALAR(rsqrtss, "rsqrtss")

// Clear then read MXCSR[5:0] around one operation, with the operation's own asm in between.
static void mx_clear(void) {
    unsigned m;
    __asm__ __volatile__("stmxcsr %0" : "=m"(m)::"memory");
    m &= ~0x3fu;
    __asm__ __volatile__("ldmxcsr %0" ::"m"(m) : "memory");
}

static unsigned mx_read(void) {
    unsigned m;
    __asm__ __volatile__("stmxcsr %0" : "=m"(m)::"memory");
    return m & 0x3fu;
}

static uint32_t b32(float f) {
    uint32_t u;
    memcpy(&u, &f, 4);
    return u;
}

static float fb(uint32_t u) {
    float f;
    memcpy(&f, &u, 4);
    return f;
}

static void put4(float x) {
    float v[4] = {x, x, x, x};
    memcpy(Src, v, 16);
}

static float lane0(void) {
    float v;
    memcpy(&v, Dst, 4);
    return v;
}

// SQRTSD inline: the completeness link line has no -lm, and a libm call is a rounding helper that can
// itself raise the flags section 3 measures.
static double dsqrt(double v) {
    double r;
    __asm__("sqrtsd %1, %0" : "=x"(r) : "x"(v));
    return r;
}

static double dabs(double v) {
    uint64_t u;
    memcpy(&u, &v, 8);
    u &= 0x7fffffffffffffffull;
    memcpy(&v, &u, 8);
    return v;
}

#define TOK(name, cond) printf(" %s=%s", (name), (cond) ? "ok" : "BAD")
#define BOUND (1.5 / 4096.0) // the SDM's 1.5 * 2^-12

int main(void) {
    // -- 1. the error bound, swept over a decade whose reciprocals are all normal --
    int rcp_in_bound = 1, rsqrt_in_bound = 1, all_lanes = 1;
    for (int i = 0; i < 30000; i++) {
        g_in = 1.0f + (float)i * (3.0f / 30000.0f); // [1,4)
        float x = g_in;
        put4(x);
        rcpps();
        double want = 1.0 / (double)x;
        if (dabs(((double)lane0() - want) / want) > BOUND) rcp_in_bound = 0;
        float v[4];
        memcpy(v, Dst, 16);
        if (b32(v[0]) != b32(v[1]) || b32(v[0]) != b32(v[2]) || b32(v[0]) != b32(v[3])) all_lanes = 0;
        rsqrtps();
        want = 1.0 / dsqrt((double)x);
        if (dabs(((double)lane0() - want) / want) > BOUND) rsqrt_in_bound = 0;
    }
    printf("bound");
    TOK("rcpps", rcp_in_bound);
    TOK("rsqrtps", rsqrt_in_bound);
    TOK("lanes_agree", all_lanes);

    // scalar forms answer the same question about lane 0 and must leave 127:32 alone
    int rcpss_bound = 1, rsqrtss_bound = 1, rcpss_merge = 1, rsqrtss_merge = 1;
    for (int i = 0; i < 30000; i++) {
        g_in = 1.0f + (float)i * (3.0f / 30000.0f);
        float x = g_in;
        put4(x);
        float keep[4] = {9.0f, 11.0f, 12.0f, 13.0f};
        memcpy(Dst, keep, 16);
        rcpss();
        double want = 1.0 / (double)x;
        if (dabs(((double)lane0() - want) / want) > BOUND) rcpss_bound = 0;
        float v[4];
        memcpy(v, Dst, 16);
        if (v[1] != 11.0f || v[2] != 12.0f || v[3] != 13.0f) rcpss_merge = 0;
        memcpy(Dst, keep, 16);
        rsqrtss();
        want = 1.0 / dsqrt((double)x);
        if (dabs(((double)lane0() - want) / want) > BOUND) rsqrtss_bound = 0;
        memcpy(v, Dst, 16);
        if (v[1] != 11.0f || v[2] != 12.0f || v[3] != 13.0f) rsqrtss_merge = 0;
    }
    TOK("rcpss", rcpss_bound);
    TOK("rsqrtss", rsqrtss_bound);
    TOK("rcpss_merge", rcpss_merge);
    TOK("rsqrtss_merge", rsqrtss_merge);
    printf("\n");

    // -- 2. specials: exact on every implementation, so these ARE raw bit patterns --
    static const struct {
        const char *name;
        uint32_t in, rcp, rsqrt;
    } sp[] = {
        {"pzero", 0x00000000u, 0x7f800000u, 0x7f800000u},
        {"nzero", 0x80000000u, 0xff800000u, 0xff800000u},
        {"pinf", 0x7f800000u, 0x00000000u, 0x00000000u},
        {"ninf", 0xff800000u, 0x80000000u, 0xffc00000u},
        {"neg", 0xc0800000u, 0u, 0xffc00000u},           // rsqrt of a negative: x86's NEGATIVE indefinite
        {"snan", 0x7f800001u, 0x7fc00001u, 0x7fc00001u}, // quieted, payload and sign kept
        {"qnan", 0x7fc00000u, 0x7fc00000u, 0x7fc00000u},
        {"nsnan", 0xff800001u, 0xffc00001u, 0xffc00001u},
        {"nqnan", 0xffc00000u, 0xffc00000u, 0xffc00000u},
    };

    printf("specials");
    for (unsigned i = 0; i < sizeof sp / sizeof *sp; i++) {
        put4(fb(sp[i].in));
        rsqrtps();
        uint32_t got = b32(lane0());
        char n[24];
        snprintf(n, sizeof n, "rsqrt_%s", sp[i].name);
        TOK(n, got == sp[i].rsqrt);
        if (sp[i].rcp) { // rcp of a negative finite is a normal value, checked by the bound sweep instead
            put4(fb(sp[i].in));
            rcpps();
            got = b32(lane0());
            snprintf(n, sizeof n, "rcp_%s", sp[i].name);
            TOK(n, got == sp[i].rcp);
        }
    }
    printf("\n");

    // -- 3. no SIMD FP exception, for any input class. The stand-in 1/x is a real division and reports
    //       #D/#O/#P/#Z, so an implementation that forgets to park the flags fails here. --
    static const uint32_t fl[] = {0x40000000u, 0x00000000u, 0x80000000u, 0x00000001u, 0x7f800000u,
                                  0xc0800000u, 0x7f800001u, 0x7fc00000u, 0x7f7fffffu, 0x00800000u};
    unsigned raised = 0;
    for (unsigned i = 0; i < sizeof fl / sizeof *fl; i++) {
        put4(fb(fl[i]));
        mx_clear();
        rcpps();
        raised |= mx_read();
        mx_clear();
        rsqrtps();
        raised |= mx_read();
        mx_clear();
        rcpss();
        raised |= mx_read();
        mx_clear();
        rsqrtss();
        raised |= mx_read();
    }
    g_mx = raised;
    mx_clear();
    printf("flags");
    TOK("none_raised", g_mx == 0);
    printf("\n");
    return 0;
}
