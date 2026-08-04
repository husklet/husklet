// FMA NaN semantics: which operand's NaN survives, with what sign and payload, and which MXCSR
// exception it raises. Measured on Zen 4; every line of the golden is native silicon.
//
// The rule is stated over the SDM's a*b+c, and the three FORMS permute which architectural operand
// is a, b and c -- so a fix that gets the permutation wrong passes one form and fails another.
// 132: dst*rm + vvvv.   213: vvvv*dst + rm.   231: vvvv*rm + dst.
//   * OPERAND NaN -> the FIRST NaN in a, b, c order wins, VERBATIM (sign and payload), quiet bit
//     forced. The addend never beats a multiplicand; b never beats a. The product/addend negation of
//     fmsub/fnmadd/fnmsub does NOT touch the propagated NaN's sign.
//   * #I iff SOME operand is an SNaN -- including when a QNaN is the one that wins. A NaN operand
//     SUPPRESSES everything else: no #D from a denormal operand, and not even the #I that a bare
//     0*inf raises.
//   * GENERATED NaN (no NaN operand at all: 0*inf, or inf-inf in the fused add) -> the QNaN
//     indefinite with the sign SET, plus #I.
// Both host backends were wrong here, in opposite directions. aarch64 FMADD is
// SNaN-first-then-ADDEND-first and, for 0*inf with a QNaN addend, the ARM ARM mandates DefaultNaN +
// IOC where x86 propagates the addend silently. An x86-64 host is no safer: which multiplicand lands
// in the encoding's first slot is the compiler's choice for whatever emulates this in C.
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>

static unsigned mxcsr_in = 0x1f80;

// 0..7 are NaN; 8..11 are not, and exist to make NaN arity 1 and 2 reachable and to drive the
// generated-NaN cases that never reach the NaN rule at all.
static const uint64_t K64[12] = {
    0x7FF0000000000001ULL, /*  0 +SNaN min payload */
    0x7FF7FFFFFFFFFFFFULL, /*  1 +SNaN max payload */
    0xFFF0000000000001ULL, /*  2 -SNaN min payload */
    0xFFF7FFFFFFFFFFFFULL, /*  3 -SNaN max payload */
    0x7FF8000000000001ULL, /*  4 +QNaN min payload */
    0x7FFFFFFFFFFFFFFFULL, /*  5 +QNaN max payload */
    0xFFF8000000000001ULL, /*  6 -QNaN min payload */
    0xFFFFFFFFFFFFFFFFULL, /*  7 -QNaN max payload */
    0x3FF0000000000000ULL, /*  8 1.0   */
    0x0000000000000000ULL, /*  9 +0.0  */
    0x7FF0000000000000ULL, /* 10 +inf  */
    0xFFF0000000000000ULL, /* 11 -inf  */
};
static const uint32_t K32[12] = {0x7F800001u, 0x7FBFFFFFu, 0xFF800001u, 0xFFBFFFFFu, 0x7FC00001u, 0x7FFFFFFFu,
                                 0xFFC00001u, 0xFFFFFFFFu, 0x3F800000u, 0x00000000u, 0x7F800000u, 0xFF800000u};

static uint8_t Vd[32] __attribute__((aligned(32)));
static uint8_t Vv[32] __attribute__((aligned(32)));
static uint8_t Vm[32] __attribute__((aligned(32)));
static uint8_t Vo[32] __attribute__((aligned(32)));

// ymm0 = dst, ymm1 = vvvv, ymm2 (or memory) = r/m. The memory r/m form is a distinct lowering on the
// aarch64 host -- the operand arrives in a scratch vreg any NaN gate has to include, and at 256 bits
// its high half comes from mem+16 instead of cpu->vhi.
#define BODY(insn, reg)                                                                                                \
    __asm__ volatile("ldmxcsr %1\n\tvmovdqu (%2)," reg "0\n\tvmovdqu (%3)," reg "1\n\tvmovdqu (%4)," reg "2\n\t" insn   \
                         " " reg "2," reg "1," reg "0\n\tvmovdqu " reg "0,(%5)\n\tstmxcsr %0"                           \
                     : "=m"(mx)                                                                                        \
                     : "m"(mxcsr_in), "r"(Vd), "r"(Vv), "r"(Vm), "r"(Vo)                                               \
                     : "ymm0", "ymm1", "ymm2", "memory")
#define BODYM(insn, reg)                                                                                               \
    __asm__ volatile("ldmxcsr %1\n\tvmovdqu (%2)," reg "0\n\tvmovdqu (%3)," reg "1\n\t" insn " (%4)," reg "1," reg      \
                         "0\n\tvmovdqu " reg "0,(%5)\n\tstmxcsr %0"                                                    \
                     : "=m"(mx)                                                                                        \
                     : "m"(mxcsr_in), "r"(Vd), "r"(Vv), "r"(Vm), "r"(Vo)                                               \
                     : "ymm0", "ymm1", "ymm2", "memory")

// Packed ops take a width axis; the scalar ones have no ymm encoding at all, and every arm of these
// ifs is ASSEMBLED whatever the runtime flags say, so the two cannot share one macro.
#define XP(mn, fo, ty)                                                                                                 \
    case OP_##mn##fo##ty:                                                                                              \
        if (mem) {                                                                                                     \
            if (w256) BODYM("v" #mn #fo #ty, "%%ymm");                                                                 \
            else BODYM("v" #mn #fo #ty, "%%xmm");                                                                      \
        } else if (w256) BODY("v" #mn #fo #ty, "%%ymm");                                                               \
        else BODY("v" #mn #fo #ty, "%%xmm");                                                                           \
        break;
#define XS(mn, fo, ty)                                                                                                 \
    case OP_##mn##fo##ty:                                                                                              \
        if (mem) BODYM("v" #mn #fo #ty, "%%xmm");                                                                      \
        else BODY("v" #mn #fo #ty, "%%xmm");                                                                           \
        break;

#define FORMS(M, mn, ty) M(mn, 132, ty) M(mn, 213, ty) M(mn, 231, ty)
#define SIGNS(M, ty) FORMS(M, fmadd, ty) FORMS(M, fmsub, ty) FORMS(M, fnmadd, ty) FORMS(M, fnmsub, ty)
#define SUBADD(M, ty) FORMS(M, fmaddsub, ty) FORMS(M, fmsubadd, ty)
#define ALL2(MP, MS) SIGNS(MP, pd) SIGNS(MP, ps) SIGNS(MS, sd) SIGNS(MS, ss) SUBADD(MP, pd) SUBADD(MP, ps)
#define ALL(M) ALL2(M, M)

#define ENUMX(mn, fo, ty) OP_##mn##fo##ty,
enum { ALL(ENUMX) OP_N };
#define NAMEX(mn, fo, ty) "v" #mn #fo #ty,
static const char *OPNAME[OP_N] = {ALL(NAMEX)};

__attribute__((target("fma,avx"))) static unsigned run(int sel, int w256, int mem) {
    unsigned mx = 0;
    (void)w256;
    switch (sel) { ALL2(XP, XS) }
    return mx;
}

static int is_dbl(const char *nm) {
    return strstr(nm, "pd") != 0 || strstr(nm, "sd") != 0;
}

static int is_scalar(const char *nm) {
    return strstr(nm, "sd") != 0 || strstr(nm, "ss") != 0;
}

// Exact mnemonic match: "fmadd" must not also accept vfmaddsub, so the form digits have to follow.
static int mnem_is(const char *nm, const char *m) {
    size_t n = strlen(m);
    return strncmp(nm + 1, m, n) == 0 && nm[1 + n] >= '0' && nm[1 + n] <= '9';
}

// Which architectural slot (0 = dst, 1 = vvvv, 2 = r/m) holds a, b and c of a*b+c. 213 and 231 both
// begin with '2', so the second digit decides -- keying on the first alone silently relabels 231.
static void roles(const char *nm, int *ia, int *ib, int *ic) {
    const char *p = nm;
    while (*p < '0' || *p > '9') p++;
    if (*p == '1') { *ia = 0, *ib = 2, *ic = 1; }        // 132: dst*rm + vvvv
    else if (p[1] == '1') { *ia = 1, *ib = 0, *ic = 2; } // 213: vvvv*dst + rm
    else { *ia = 1, *ib = 2, *ic = 0; }                  // 231: vvvv*rm + dst
}

// Place one (a, b, c) triple BY ROLE, so a printed case means the same thing for all three forms and
// a permutation error surfaces as a diff. Lanes above the first keep the 0x5a witness for the scalar
// forms, whose dst[127:32] (ss) / dst[127:64] (sd) is architecturally preserved.
static void lanes(const char *nm, int a, int b, int c, int nb) {
    uint8_t *slot[3] = {Vd, Vv, Vm};
    int ia, ib, ic, idx[3];
    roles(nm, &ia, &ib, &ic);
    idx[ia] = a, idx[ib] = b, idx[ic] = c;
    int es = is_dbl(nm) ? 8 : 4, lim = is_scalar(nm) ? es : nb;
    memset(Vd, 0x5a, 32);
    memset(Vv, 0x5a, 32);
    memset(Vm, 0x5a, 32);
    for (int s = 0; s < 3; s++)
        for (int l = 0; l < lim; l += es) {
            if (es == 8) memcpy(slot[s] + l, &K64[idx[s]], 8);
            else memcpy(slot[s] + l, &K32[idx[s]], 4);
        }
}

static void show(const char *tag, const char *nm, const char *arg, unsigned mx, int nb) {
    printf("%-5s %-14s %-9s ", tag, nm, arg);
    for (int l = nb - 1; l >= 0; l--) printf("%02x", Vo[l]);
    printf(" %04x\n", mx);
}

int main(void) {
    char arg[16];

    // ---- prio: every triple over three DISTINCT NaNs. Three distinct operands is what pins the
    // order a > b > c: with two, the addend's loss is visible but b-over-a is not, and with one every
    // ordering agrees. All three forms; the ps element size is covered by the 3-NaN case below.
    static const int P[3] = {0, 5, 6}; // +SNaN min, +QNaN max, -QNaN min
    for (int sel = 0; sel < OP_N; sel++) {
        const char *nm = OPNAME[sel];
        if (!mnem_is(nm, "fmadd") || !strstr(nm, "pd")) continue;
        for (int i = 0; i < 3; i++)
            for (int j = 0; j < 3; j++)
                for (int k = 0; k < 3; k++) {
                    snprintf(arg, sizeof arg, "%d,%d,%d", P[i], P[j], P[k]);
                    lanes(nm, P[i], P[j], P[k], 16);
                    show("prio", nm, arg, run(sel, 0, 0), 16);
                }
    }

    // ---- arity: all 60 encodings -- four sign variants x three forms x {pd, ps, sd, ss} plus the
    // addsub/subadd pair. Covers NaN arity 1 (each position alone), 2 (including a QNaN a beside an
    // SNaN b, where a wins and #I is still raised), 3, the 0*inf-with-QNaN-addend case x86
    // propagates silently, and the two generated NaNs that carry no NaN operand at all.
    static const int A[8][3] = {
        {0, 8, 8},  {8, 0, 8},  {8, 8, 2}, // one SNaN: a, b, then the addend alone
        {5, 0, 8},                         // QNaN a beside SNaN b: a wins, #I raised anyway
        {4, 8, 1},                         // a + c
        {2, 5, 4},                         // all three, each quieting to a DIFFERENT value
        {9, 10, 4},                        // 0*inf with a QNaN addend: propagates, no #I
        {10, 8, 11},                       // generated: inf-inf in the fused add
    };
    for (int sel = 0; sel < OP_N; sel++) {
        const char *nm = OPNAME[sel];
        for (int t = 0; t < 8; t++) {
            snprintf(arg, sizeof arg, "%d,%d,%d", A[t][0], A[t][1], A[t][2]);
            lanes(nm, A[t][0], A[t][1], A[t][2], 16);
            show("arity", nm, arg, run(sel, 0, 0), 16);
        }
    }

    // ---- wide: 256-bit and memory-operand lowerings with a DIFFERENT triple in every lane, so a
    // per-lane gate is exercised rather than an all-or-nothing one, and so the addsub/subadd lane
    // parity is visible. Both element sizes; the fmaddsub pair is included because it is the only
    // family whose even and odd lanes differ.
    static const int Wq[8][3] = {{0, 8, 8}, {8, 5, 2}, {9, 10, 6}, {8, 8, 3},
                                 {1, 4, 8}, {8, 11, 8}, {7, 0, 5}, {10, 9, 8}};
    for (int sel = 0; sel < OP_N; sel++) {
        const char *nm = OPNAME[sel];
        // vfmadd in all three forms (the 256-bit high half picks its operand roles separately), plus
        // one fmaddsub for the even/odd lane parity.
        if (is_scalar(nm)) continue;
        if (!mnem_is(nm, "fmadd") && !(mnem_is(nm, "fmaddsub") && strstr(nm, "231"))) continue;
        int ia, ib, ic, rr[3];
        roles(nm, &ia, &ib, &ic);
        rr[ia] = 0, rr[ib] = 1, rr[ic] = 2;
        int es = is_dbl(nm) ? 8 : 4;
        for (int w256 = 0; w256 < 2; w256++)
            for (int mem = 0; mem < 2; mem++) {
                int nb = w256 ? 32 : 16;
                uint8_t *slot[3] = {Vd, Vv, Vm};
                memset(Vd, 0x5a, 32);
                memset(Vv, 0x5a, 32);
                memset(Vm, 0x5a, 32);
                for (int l = 0, n = 0; l < nb; l += es, n++)
                    for (int s = 0; s < 3; s++) {
                        int v = Wq[n & 7][rr[s]];
                        if (es == 8) memcpy(slot[s] + l, &K64[v], 8);
                        else memcpy(slot[s] + l, &K32[v], 4);
                    }
                snprintf(arg, sizeof arg, "w%d m%d", nb * 8, mem);
                show("wide", nm, arg, run(sel, w256, mem), nb);
            }
    }
    return 0;
}
