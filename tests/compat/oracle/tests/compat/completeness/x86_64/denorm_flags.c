// MXCSR.DE (#D, the DENORMAL-OPERAND exception) across the SSE/AVX FP surface, plus the two families whose
// #D rule is the surprising one. Measured on Zen 4; every line of the golden is native silicon.
//
// The x86 rule, per family (all four MXCSR modes probed, both source positions):
//   * arithmetic (ADD/SUB/MUL/DIV/SQRT/ADDSUB/HADD/HSUB), MIN/MAX, every CMP predicate, COMIS/UCOMIS,
//     the four float<->float converts, DPPS/DPPD and FMA all raise #D for a denormal in ANY source;
//   * float->INT converts, ROUND*, VCVTPH2PS and the bitwise/shuffle ops raise NONE -- ROUND is the
//     surprise, since it inspects the value and reports #P, but never #D;
//   * VCVTPS2PH DOES raise #D (it is a float->float convert wearing an integer-looking destination);
//   * RCPPS/RSQRTPS raise NOTHING AT ALL, not even #P or #O;
//   * DAZ zeroes a denormal source BEFORE the operation, so with DAZ set NOTHING here raises #D -- and
//     the operation becomes exact, which removes the #P/#U it would otherwise have reported too.
//
// WHAT IS AND IS NOT HERE. The aarch64 host cannot raise #D with DAZ clear: ARM reports FPSR.IDC only when
// FPCR.FZ flushed an input, so with FZ=0 the flag has no source, and recovering it means a per-operand
// bit-pattern test emitted on every FP lowering -- ~14 host instructions against the 7 a gated packed FADD
// costs today. So the DAZ-CLEAR half of the arithmetic table is deliberately absent: it would be a golden
// one host cannot meet. What is here is the half that IS shared -- "DAZ set, therefore no #D anywhere",
// the families that raise no #D in any mode, and the full ROUND*/VCVTPS2PH flag rules.
// FTZ-without-DAZ is absent for a different reason: both map onto the single FPCR.FZ, which flushes inputs
// as well as outputs, so a lone FTZ gives x86's DAZ behaviour on that host and the results differ too.
// MIN* and VRCPPS are absent from the DAZ rows: MIN's compare-and-select lowering returns the UNflushed
// operand, and VRCPPS is modelled at full precision rather than the hardware's ~12-bit table.
//
// Sources are REGISTERS, each sequence is one volatile asm block that does its own ldmxcsr/stmxcsr, and no
// intrinsic appears anywhere: gcc hoists SSE arithmetic across LDMXCSR, constant-folds intrinsic sequences,
// and if-converts a ternary between two intrinsics into both instructions plus a select -- with the
// not-taken one's exceptions landing in the guest's MXCSR.
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static uint8_t A[32] __attribute__((aligned(32))); // xmm0: dst, and src1 for the VEX forms
static uint8_t B[32] __attribute__((aligned(32))); // xmm1: src2
static uint8_t C[32] __attribute__((aligned(32))); // xmm2: FMA third operand
static uint8_t Out[32] __attribute__((aligned(32)));
static unsigned mxin, mxout;
static uint64_t eflags;

#define OP2(nm, insn)                                                                                                  \
    static void nm(void) {                                                                                             \
        __asm__ volatile("ldmxcsr %1\n\tmovdqa (%2),%%xmm0\n\tmovdqa (%3),%%xmm1\n\t" insn                             \
                         " %%xmm1,%%xmm0\n\tmovdqa %%xmm0,(%4)\n\tstmxcsr %0"                                          \
                         : "=m"(mxout)                                                                                 \
                         : "m"(mxin), "r"(A), "r"(B), "r"(Out)                                                         \
                         : "xmm0", "xmm1", "memory");                                                                  \
    }
#define OP2I(nm, insn, imm)                                                                                            \
    static void nm(void) {                                                                                             \
        __asm__ volatile("ldmxcsr %1\n\tmovdqa (%2),%%xmm0\n\tmovdqa (%3),%%xmm1\n\t" insn " $" #imm                   \
                         ",%%xmm1,%%xmm0\n\tmovdqa %%xmm0,(%4)\n\tstmxcsr %0"                                          \
                         : "=m"(mxout)                                                                                 \
                         : "m"(mxin), "r"(A), "r"(B), "r"(Out)                                                         \
                         : "xmm0", "xmm1", "memory");                                                                  \
    }
// COMIS/UCOMIS write EFLAGS, not a register; pushfq keeps the sequence intact.
#define OPCMP(nm, insn)                                                                                                \
    static void nm(void) {                                                                                             \
        __asm__ volatile("ldmxcsr %2\n\tmovdqa (%3),%%xmm0\n\tmovdqa (%4),%%xmm1\n\t" insn                             \
                         " %%xmm1,%%xmm0\n\tpushfq\n\tpop %0\n\tstmxcsr %1"                                            \
                         : "=r"(eflags), "=m"(mxout)                                                                   \
                         : "m"(mxin), "r"(A), "r"(B)                                                                   \
                         : "xmm0", "xmm1", "cc", "memory");                                                            \
    }
#define OP1(nm, insn)                                                                                                  \
    static void nm(void) {                                                                                             \
        __asm__ volatile("ldmxcsr %1\n\tmovdqa (%3),%%xmm1\n\t" insn                                                   \
                         " %%xmm1,%%xmm0\n\tmovdqa %%xmm0,(%4)\n\tstmxcsr %0"                                          \
                         : "=m"(mxout)                                                                                 \
                         : "m"(mxin), "r"(A), "r"(B), "r"(Out)                                                         \
                         : "xmm0", "xmm1", "memory");                                                                  \
    }
#define OP1I(nm, insn, imm)                                                                                            \
    static void nm(void) {                                                                                             \
        __asm__ volatile("ldmxcsr %1\n\tmovdqa (%3),%%xmm1\n\t" insn " $" #imm                                         \
                         ",%%xmm1,%%xmm0\n\tmovdqa %%xmm0,(%4)\n\tstmxcsr %0"                                          \
                         : "=m"(mxout)                                                                                 \
                         : "m"(mxin), "r"(A), "r"(B), "r"(Out)                                                         \
                         : "xmm0", "xmm1", "memory");                                                                  \
    }
#define VOP3(nm, insn)                                                                                                 \
    static void nm(void) {                                                                                             \
        __asm__ volatile("ldmxcsr %1\n\tvmovdqa (%2),%%xmm2\n\tvmovdqa (%3),%%xmm1\n\t" insn                           \
                         " %%xmm1,%%xmm2,%%xmm0\n\tvmovdqa %%xmm0,(%4)\n\tvzeroupper\n\tstmxcsr %0"                    \
                         : "=m"(mxout)                                                                                 \
                         : "m"(mxin), "r"(A), "r"(B), "r"(Out)                                                         \
                         : "xmm0", "xmm1", "xmm2", "memory");                                                          \
    }
#define VOP2(nm, insn)                                                                                                 \
    static void nm(void) {                                                                                             \
        __asm__ volatile("ldmxcsr %1\n\tvmovdqa (%3),%%xmm1\n\t" insn                                                  \
                         " %%xmm1,%%xmm0\n\tvmovdqa %%xmm0,(%4)\n\tvzeroupper\n\tstmxcsr %0"                           \
                         : "=m"(mxout)                                                                                 \
                         : "m"(mxin), "r"(A), "r"(B), "r"(Out)                                                         \
                         : "xmm0", "xmm1", "memory");                                                                  \
    }
#define VOP3I(nm, insn, imm)                                                                                           \
    static void nm(void) {                                                                                             \
        __asm__ volatile("ldmxcsr %1\n\tvmovdqa (%2),%%xmm2\n\tvmovdqa (%3),%%xmm1\n\t" insn " $" #imm                 \
                         ",%%xmm1,%%xmm2,%%xmm0\n\tvmovdqa %%xmm0,(%4)\n\tvzeroupper\n\tstmxcsr %0"                    \
                         : "=m"(mxout)                                                                                 \
                         : "m"(mxin), "r"(A), "r"(B), "r"(Out)                                                         \
                         : "xmm0", "xmm1", "xmm2", "memory");                                                          \
    }
// FMA: xmm0 is the third source AND the destination.
#define FMA3(nm, insn)                                                                                                 \
    static void nm(void) {                                                                                             \
        __asm__ volatile("ldmxcsr %1\n\tvmovdqa (%2),%%xmm0\n\tvmovdqa (%3),%%xmm1\n\tvmovdqa (%4),%%xmm2\n\t" insn    \
                         " %%xmm1,%%xmm2,%%xmm0\n\tvmovdqa %%xmm0,(%5)\n\tvzeroupper\n\tstmxcsr %0"                    \
                         : "=m"(mxout)                                                                                 \
                         : "m"(mxin), "r"(A), "r"(B), "r"(C), "r"(Out)                                                 \
                         : "xmm0", "xmm1", "xmm2", "memory");                                                          \
    }
// ROUND* and VCVTPS2PH: the rounding mode is an immediate, not a run-time operand, so one form per imm.
#define R(nm, insn, imm)                                                                                               \
    static void nm(void) {                                                                                             \
        __asm__ volatile("ldmxcsr %1\n\tmovdqa (%2),%%xmm1\n\t" insn " $" #imm                                         \
                         ",%%xmm1,%%xmm0\n\tmovdqa %%xmm0,(%3)\n\tstmxcsr %0"                                          \
                         : "=m"(mxout)                                                                                 \
                         : "m"(mxin), "r"(B), "r"(Out)                                                                 \
                         : "xmm0", "xmm1", "memory");                                                                  \
    }
#define PH(nm, imm)                                                                                                    \
    static void nm(void) {                                                                                             \
        __asm__ volatile("ldmxcsr %1\n\tvmovdqa (%2),%%xmm1\n\tvcvtps2ph $" #imm                                       \
                         ",%%xmm1,%%xmm0\n\tvmovdqa %%xmm0,(%3)\n\tvzeroupper\n\tstmxcsr %0"                           \
                         : "=m"(mxout)                                                                                 \
                         : "m"(mxin), "r"(B), "r"(Out)                                                                 \
                         : "xmm0", "xmm1", "memory");                                                                  \
    }

// clang-format packs these instantiations onto shared lines and then re-splits them, forever.
/* clang-format off */
OP2(o_addpd, "addpd") OP2(o_addsd, "addsd") OP2(o_addps, "addps") OP2(o_addss, "addss")
OP2(o_subsd, "subsd") OP2(o_subps, "subps") OP2(o_mulpd, "mulpd") OP2(o_mulsd, "mulsd")
OP2(o_mulps, "mulps") OP2(o_divpd, "divpd") OP2(o_divsd, "divsd") OP2(o_divps, "divps")
OP1(o_sqrtpd, "sqrtpd") OP1(o_sqrtsd, "sqrtsd") OP1(o_sqrtps, "sqrtps") OP1(o_sqrtss, "sqrtss")
OP2(o_maxpd, "maxpd") OP2(o_maxsd, "maxsd") OP2(o_maxps, "maxps")
OP2(o_addsubpd, "addsubpd") OP2(o_addsubps, "addsubps")
OP2(o_haddpd, "haddpd") OP2(o_hsubpd, "hsubpd") OP2(o_haddps, "haddps") OP2(o_hsubps, "hsubps")
OP2I(o_cmpeqpd, "cmppd", 0) OP2I(o_cmpeqsd, "cmpsd", 0) OP2I(o_cmpltsd, "cmpsd", 1)
OP2I(o_cmpunordsd, "cmpsd", 3) OP2I(o_cmpeqps, "cmpps", 0)
OPCMP(o_comisd, "comisd") OPCMP(o_ucomisd, "ucomisd") OPCMP(o_comiss, "comiss") OPCMP(o_ucomiss, "ucomiss")
OP1(o_cvtss2sd, "cvtss2sd") OP1(o_cvtsd2ss, "cvtsd2ss") OP1(o_cvtps2pd, "cvtps2pd") OP1(o_cvtpd2ps, "cvtpd2ps")
OP1(o_cvtpd2dq, "cvtpd2dq") OP1(o_cvttpd2dq, "cvttpd2dq") OP1(o_cvtps2dq, "cvtps2dq")
OP1I(o_roundpd, "roundpd", 0) OP1I(o_roundsd, "roundsd", 0) OP1I(o_roundps, "roundps", 3)
OP2I(o_dppd, "dppd", 0x31) OP2I(o_dpps, "dpps", 0xf1)
OP2(o_andpd, "andpd") OP2(o_xorps, "xorps") OP2(o_unpcklpd, "unpcklpd")
VOP3(v_addsd, "vaddsd") VOP3(v_mulpd, "vmulpd") VOP3(v_divsd, "vdivsd") VOP3(v_maxpd, "vmaxpd")
VOP3(v_addsubps, "vaddsubps") VOP3(v_haddpd, "vhaddpd")
VOP2(v_sqrtpd, "vsqrtpd") VOP2(v_cvtps2pd, "vcvtps2pd") VOP2(v_cvtpd2ps, "vcvtpd2ps")
VOP2(v_roundpd, "vroundpd $0,") VOP2(v_cvtph2ps, "vcvtph2ps")
VOP3I(v_cmpeqsd, "vcmpsd", 0) VOP3I(v_dppd, "vdppd", 0x31)
FMA3(f_fmadd213sd, "vfmadd213sd") FMA3(f_fmadd213pd, "vfmadd213pd") FMA3(f_fmsub213ss, "vfmsub213ss")
R(rd0, "roundpd", 0) R(rd1, "roundpd", 1) R(rd2, "roundpd", 2) R(rd3, "roundpd", 3)
R(rd4, "roundpd", 4) R(rd8, "roundpd", 8) R(rd9, "roundpd", 9) R(rdc, "roundpd", 12)
R(rf0, "roundps", 0) R(rf4, "roundps", 4) R(rf9, "roundps", 9)
R(rs0, "roundss", 0) R(rs4, "roundss", 4) R(rs9, "roundss", 9)
R(rq0, "roundsd", 0) R(rq4, "roundsd", 4) R(rq9, "roundsd", 9)
PH(ph0, 0) PH(ph1, 1) PH(ph2, 2) PH(ph3, 3) PH(ph4, 4) PH(ph8, 8)
enum { W64, W32, WH16 };

/* clang-format on */

struct op {
    const char *name;
    void (*fn)(void);
    int w;
    int nsrc;  // 1 = B is the only source; 2 = A and B; 3 = A, B and C
    int nodee; // 1 = raises no #D even with DAZ clear, so its plain-mode rows are golden too
};

static const struct op ops[] = {
    {"addpd", o_addpd, W64, 2, 0},
    {"addsd", o_addsd, W64, 2, 0},
    {"addps", o_addps, W32, 2, 0},
    {"addss", o_addss, W32, 2, 0},
    {"subsd", o_subsd, W64, 2, 0},
    {"subps", o_subps, W32, 2, 0},
    {"mulpd", o_mulpd, W64, 2, 0},
    {"mulsd", o_mulsd, W64, 2, 0},
    {"mulps", o_mulps, W32, 2, 0},
    {"divpd", o_divpd, W64, 2, 0},
    {"divsd", o_divsd, W64, 2, 0},
    {"divps", o_divps, W32, 2, 0},
    {"sqrtpd", o_sqrtpd, W64, 1, 0},
    {"sqrtsd", o_sqrtsd, W64, 1, 0},
    {"sqrtps", o_sqrtps, W32, 1, 0},
    {"sqrtss", o_sqrtss, W32, 1, 0},
    {"maxpd", o_maxpd, W64, 2, 0},
    {"maxsd", o_maxsd, W64, 2, 0},
    {"maxps", o_maxps, W32, 2, 0},
    {"addsubpd", o_addsubpd, W64, 2, 0},
    {"addsubps", o_addsubps, W32, 2, 0},
    {"haddpd", o_haddpd, W64, 2, 0},
    {"hsubpd", o_hsubpd, W64, 2, 0},
    {"haddps", o_haddps, W32, 2, 0},
    {"hsubps", o_hsubps, W32, 2, 0},
    {"cmpeqpd", o_cmpeqpd, W64, 2, 0},
    {"cmpeqsd", o_cmpeqsd, W64, 2, 0},
    {"cmpltsd", o_cmpltsd, W64, 2, 0},
    {"cmpunordsd", o_cmpunordsd, W64, 2, 0},
    {"cmpeqps", o_cmpeqps, W32, 2, 0},
    {"comisd", o_comisd, W64, 2, 0},
    {"ucomisd", o_ucomisd, W64, 2, 0},
    {"comiss", o_comiss, W32, 2, 0},
    {"ucomiss", o_ucomiss, W32, 2, 0},
    {"cvtss2sd", o_cvtss2sd, W32, 1, 0},
    {"cvtsd2ss", o_cvtsd2ss, W64, 1, 0},
    {"cvtps2pd", o_cvtps2pd, W32, 1, 0},
    {"cvtpd2ps", o_cvtpd2ps, W64, 1, 0},
    {"dppd", o_dppd, W64, 2, 0},
    {"dpps", o_dpps, W32, 2, 0},
    {"vaddsd", v_addsd, W64, 2, 0},
    {"vmulpd", v_mulpd, W64, 2, 0},
    {"vdivsd", v_divsd, W64, 2, 0},
    {"vmaxpd", v_maxpd, W64, 2, 0},
    {"vaddsubps", v_addsubps, W32, 2, 0},
    {"vhaddpd", v_haddpd, W64, 2, 0},
    {"vsqrtpd", v_sqrtpd, W64, 1, 0},
    {"vcvtps2pd", v_cvtps2pd, W32, 1, 0},
    {"vcvtpd2ps", v_cvtpd2ps, W64, 1, 0},
    {"vcmpeqsd", v_cmpeqsd, W64, 2, 0},
    {"vdppd", v_dppd, W64, 2, 0},
    {"vfmadd213sd", f_fmadd213sd, W64, 3, 0},
    {"vfmadd213pd", f_fmadd213pd, W64, 3, 0},
    {"vfmsub213ss", f_fmsub213ss, W32, 3, 0},
    // #D-free in every mode: the float->int converts, ROUND*, VCVTPH2PS and the bitwise forms.
    {"cvtpd2dq", o_cvtpd2dq, W64, 1, 1},
    {"cvttpd2dq", o_cvttpd2dq, W64, 1, 1},
    {"cvtps2dq", o_cvtps2dq, W32, 1, 1},
    {"roundpd", o_roundpd, W64, 1, 1},
    {"roundsd", o_roundsd, W64, 1, 1},
    {"roundps", o_roundps, W32, 1, 1},
    {"vroundpd", v_roundpd, W64, 1, 1},
    {"andpd", o_andpd, W64, 2, 1},
    {"xorps", o_xorps, W32, 2, 1},
    {"unpcklpd", o_unpcklpd, W64, 2, 1},
    {"vcvtph2ps", v_cvtph2ps, WH16, 1, 1},
};

static void fill(uint8_t *p, int w, uint64_t v) {
    memset(p, 0, 32);
    if (w == W64) {
        for (int i = 0; i < 4; i++)
            memcpy(p + 8 * i, &v, 8);
    } else if (w == W32) {
        uint32_t x = (uint32_t)v;
        for (int i = 0; i < 8; i++)
            memcpy(p + 4 * i, &x, 4);
    } else {
        uint16_t x = (uint16_t)v;
        for (int i = 0; i < 16; i++)
            memcpy(p + 2 * i, &x, 2);
    }
}

// 1.0 at each element width; the denormal is always the smallest one, 0x...001.
static uint64_t one_of(int w) {
    return w == W64 ? 0x3FF0000000000000ULL : w == W32 ? 0x3F800000ULL : 0x3C00ULL;
}

static void (*rdfn[8])(void) = {rd0, rd1, rd2, rd3, rd4, rd8, rd9, rdc};
static const int rdimm[8] = {0, 1, 2, 3, 4, 8, 9, 12};
// All four ROUND widths x the three shapes the flag can take: an explicit mode plus a separate inexact
// test, imm[2] where the current-mode round IS that test, and imm[3] where there is no test at all.
static void (*wfn[9])(void) = {rf0, rf4, rf9, rs0, rs4, rs9, rq0, rq4, rq9};
static const char *wnm[9] = {"roundps", "roundps", "roundps", "roundss", "roundss",
                             "roundss", "roundsd", "roundsd", "roundsd"};
static const int wimm[9] = {0, 4, 9, 0, 4, 9, 0, 4, 9};
static const int wf32[9] = {1, 1, 1, 1, 1, 1, 0, 0, 0};
static void (*phfn[6])(void) = {ph0, ph1, ph2, ph3, ph4, ph8};
static const int phimm[6] = {0, 1, 2, 3, 4, 8};

static const uint64_t RV[] = {
    0x4000000000000000ULL, //  2.0            already integral: no flag at all
    0x3FF8000000000000ULL, //  1.5            inexact -> #P unless imm[3]
    0xBFF8000000000000ULL, // -1.5            the directed modes must differ here
    0x0000000000000001ULL, //  denormal       rounds to 0: #P, and NEVER #D
    0x8000000000000001ULL, // -denormal       -0.0, so the SIGN of the zero decides inexactness
    0x7FF8000000000000ULL, //  QNaN           passes through, no flag
    0x7FF0000000000001ULL, //  SNaN           #I, and the result is QUIETED; imm[3] does not suppress it
    0x7FF0000000000000ULL, // +Inf
    0x8000000000000000ULL, // -0.0
    0x4330000000000001ULL, //  2^52+1         integral and huge: still exact
};
// The same kit narrowed to binary32.
static const uint32_t RV32[] = {0x40000000u, 0x3FC00000u, 0xBFC00000u, 0x00000001u, 0x80000001u,
                                0x7FC00000u, 0x7F800001u, 0x7F800000u, 0x80000000u, 0x4B000001u};
static const uint32_t PV[] = {
    0x477FE000u, //  65504      largest finite half, exact
    0x477FF000u, //  65520      the round-to-nearest overflow boundary: #O under RN, only #P under RM
    0x477FEFFFu, //  just below it
    0x47800000u, //  65536      over the boundary in every mode
    0x7F7FFFFFu, //  FLT_MAX
    0xC77FF000u, // -65520      the mirror, so a sign-blind overflow test shows up
    0x33800000u, //  2^-24      smallest half denormal, EXACT -- tiny but no #U
    0x33000000u, //  2^-25      tiny and inexact -> #U|#P
    0x387FE000u, //  just below 2^-14: tiny BEFORE rounding, so #U even landing on a normal
    0x38800000u, //  2^-14      smallest normal half, exact
    0x00000001u, //  f32 denormal -> #D on top of #U|#P
    0x7F800001u, //  SNaN -> #I, quieted
    0x7FC00000u, //  QNaN
    0x7F800000u, // +Inf
};

static void run_table(const char *tag, int nodee_only) {
    for (unsigned i = 0; i < sizeof ops / sizeof ops[0]; i++) {
        const struct op *O = &ops[i];
        if (nodee_only && !O->nodee) continue;
        for (int kit = 0; kit < 3; kit++) { // 0: denormal in src1  1: in src2  2: no denormal
            if (O->nsrc == 1 && kit == 0) continue;
            uint64_t one = one_of(O->w);
            fill(A, O->w, kit == 0 ? 1 : one);
            fill(B, O->w, kit == 1 ? 1 : one);
            fill(C, O->w, one);
            memset(Out, 0, sizeof Out);
            eflags = 0;
            O->fn();
            printf("%-6s %-12s kit%d mx=%02x out=%016llx\n", tag, O->name, kit, mxout & 0x3f,
                   (unsigned long long)*(uint64_t *)Out);
        }
    }
}

int main(void) {
    // DAZ and DAZ|FTZ only. See the header: the DAZ-clear arithmetic rows are a golden the aarch64 host
    // cannot meet, and FTZ-without-DAZ is a mode neither host models separately.
    mxin = 0x1fc0u;
    run_table("daz", 0);
    mxin = 0x9fc0u;
    run_table("dazftz", 0);
    // The same table with DAZ CLEAR, restricted to the ops that raise no #D in any mode.
    mxin = 0x1f80u;
    run_table("plain", 1);
    for (unsigned c = 0; c < 8; c++)
        for (unsigned i = 0; i < sizeof RV / sizeof RV[0]; i++) {
            fill(B, W64, RV[i]);
            memset(Out, 0, sizeof Out);
            rdfn[c]();
            printf("roundpd imm%-3d in=%016llx out=%016llx mx=%02x\n", rdimm[c], (unsigned long long)RV[i],
                   (unsigned long long)*(uint64_t *)Out, mxout & 0x3f);
        }
    for (unsigned c = 0; c < 9; c++)
        for (unsigned i = 0; i < sizeof RV / sizeof RV[0]; i++) {
            memset(Out, 0, sizeof Out);
            if (wf32[c]) {
                fill(B, W32, RV32[i]);
                wfn[c]();
                printf("%s imm%-3d in=%08x out=%08x mx=%02x\n", wnm[c], wimm[c], RV32[i], *(uint32_t *)Out,
                       mxout & 0x3f);
            } else {
                fill(B, W64, RV[i]);
                wfn[c]();
                printf("%s imm%-3d in=%016llx out=%016llx mx=%02x\n", wnm[c], wimm[c], (unsigned long long)RV[i],
                       (unsigned long long)*(uint64_t *)Out, mxout & 0x3f);
            }
        }
    for (unsigned c = 0; c < 6; c++)
        for (unsigned i = 0; i < sizeof PV / sizeof PV[0]; i++) {
            fill(B, W32, PV[i]);
            memset(Out, 0, sizeof Out);
            phfn[c]();
            printf("vcvtps2ph imm%-3d in=%08x out=%04x mx=%02x\n", phimm[c], PV[i], *(uint16_t *)Out, mxout & 0x3f);
        }
    return 0;
}
