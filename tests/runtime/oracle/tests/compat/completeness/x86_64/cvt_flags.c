// Every SSE/AVX float->int conversion, under all four MXCSR.RC modes, checked for RESULT and for MXCSR
// STICKY FLAGS. Measured on Zen 4; every line of the golden is native silicon.
//
// The x86 rule, and each clause is one that was wrong here:
//   * the ROUNDED value decides the range, not the source: 2147483647.5 rounds to 2^31 under RC=near/up
//     and is then out of int32;
//   * out of range or NaN -> the integer indefinite and #I ALONE. Not #P, even from an inexact source;
//     not #D, even from a denormal one;
//   * in range -> #P iff the rounding changed the value, and nothing else -- notably no #D, which the
//     float->float converts DO raise but these never do;
//   * CVT (not CVTT) rounds by MXCSR.RC, and legacy must agree with VEX.
//
// THE KIT IS THE POINT. An out-of-range NON-INTEGER exists for exactly one width pair, f64 -> int32:
// every f32 at or above 2^31, and every f64 at or above 2^63, is already an integer. 2147483648.25 and
// -2147483649.25 are therefore the only values that can tell x86's "#I alone" apart from AArch64 FRINTX's
// "#I and #P", and a kit built on 2^31 -- as the previous one was -- passes both. -2147483648.25 is here
// for the opposite reason: it is out of range only under RC=down, and truncates back INTO range, where
// #P is required. So is 4.9e-324, whose conversion is inexact but raises no #D.
//
// Sources are REGISTERS unless the row says mem, each sequence is one volatile asm block that does its
// own ldmxcsr/stmxcsr, and no intrinsic appears anywhere: gcc hoists SSE arithmetic across LDMXCSR,
// constant-folds intrinsic sequences, and if-converts a ternary between two intrinsics into both
// instructions plus a select -- with the not-taken one's exceptions landing in the guest's MXCSR.
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static uint8_t In[32] __attribute__((aligned(32)));
static uint8_t Out[32] __attribute__((aligned(32)));
static unsigned mxin, mxout;

#define SSE_PACK(nm, insn)                                                                                             \
    static void nm(void) {                                                                                             \
        __asm__ volatile("ldmxcsr %1\n\tmovdqu (%2),%%xmm3\n\t" insn " %%xmm3,%%xmm4\n\tmovdqu %%xmm4,(%3)\n\t"        \
                         "stmxcsr %0"                                                                                  \
                         : "=m"(mxout)                                                                                 \
                         : "m"(mxin), "r"(In), "r"(Out)                                                                \
                         : "xmm3", "xmm4", "memory");                                                                  \
    }
#define SSE_PACK_MEM(nm, insn)                                                                                         \
    static void nm(void) {                                                                                             \
        __asm__ volatile("ldmxcsr %1\n\t" insn " (%2),%%xmm4\n\tmovdqu %%xmm4,(%3)\n\tstmxcsr %0"                      \
                         : "=m"(mxout)                                                                                 \
                         : "m"(mxin), "r"(In), "r"(Out)                                                                \
                         : "xmm4", "memory");                                                                          \
    }
#define SSE_GPR(nm, insn, sz)                                                                                          \
    static void nm(void) {                                                                                             \
        uint64_t r = 0;                                                                                                \
        __asm__ volatile("ldmxcsr %2\n\tmovdqu (%3),%%xmm3\n\t" insn " %%xmm3,%" sz "0\n\tstmxcsr %1"                  \
                         : "=r"(r), "=m"(mxout)                                                                        \
                         : "m"(mxin), "r"(In)                                                                          \
                         : "xmm3", "memory");                                                                          \
        memcpy(Out, &r, 8);                                                                                            \
    }
#define VEX_PACK(nm, insn, sreg, dreg)                                                                                 \
    static void nm(void) {                                                                                             \
        __asm__ volatile("ldmxcsr %1\n\tvmovdqu (%2),%%" sreg "3\n\t" insn " %%" sreg "3,%%" dreg "4\n\t"              \
                         "vmovdqu %%" dreg "4,(%3)\n\tvzeroupper\n\tstmxcsr %0"                                        \
                         : "=m"(mxout)                                                                                 \
                         : "m"(mxin), "r"(In), "r"(Out)                                                                \
                         : "xmm3", "xmm4", "ymm3", "ymm4", "memory");                                                  \
    }
#define VEX_PACK_MEM(nm, insn, sreg, dreg)                                                                             \
    static void nm(void) {                                                                                             \
        __asm__ volatile("ldmxcsr %1\n\t" insn " (%2),%%" dreg "4\n\tvmovdqu %%" dreg "4,(%3)\n\t"                     \
                         "vzeroupper\n\tstmxcsr %0"                                                                    \
                         : "=m"(mxout)                                                                                 \
                         : "m"(mxin), "r"(In), "r"(Out)                                                                \
                         : "xmm4", "ymm4", "memory");                                                                  \
    }
#define VEX_GPR(nm, insn, sz)                                                                                          \
    static void nm(void) {                                                                                             \
        uint64_t r = 0;                                                                                                \
        __asm__ volatile("ldmxcsr %2\n\tvmovdqu (%3),%%xmm3\n\t" insn " %%xmm3,%" sz "0\n\tvzeroupper\n\tstmxcsr %1"   \
                         : "=r"(r), "=m"(mxout)                                                                        \
                         : "m"(mxin), "r"(In)                                                                          \
                         : "xmm3", "memory");                                                                          \
        memcpy(Out, &r, 8);                                                                                            \
    }
// 0F 2C/2D with no F2/F3 prefix name an mm destination, not a GPR.
#define MMX_TOPI(nm, insn)                                                                                             \
    static void nm(void) {                                                                                             \
        __asm__ volatile("ldmxcsr %1\n\tmovdqu (%2),%%xmm3\n\t" insn " %%xmm3,%%mm1\n\tmovq %%mm1,(%3)\n\temms\n\t"    \
                         "stmxcsr %0"                                                                                  \
                         : "=m"(mxout)                                                                                 \
                         : "m"(mxin), "r"(In), "r"(Out)                                                                \
                         : "xmm3", "mm1", "memory");                                                                   \
    }

SSE_GPR(l_cvttss2si32, "cvttss2si", "k")
SSE_GPR(l_cvtss2si32, "cvtss2si", "k")
SSE_GPR(l_cvttss2si64, "cvttss2si", "q")
SSE_GPR(l_cvtss2si64, "cvtss2si", "q")
SSE_GPR(l_cvttsd2si32, "cvttsd2si", "k")
SSE_GPR(l_cvtsd2si32, "cvtsd2si", "k")
SSE_GPR(l_cvttsd2si64, "cvttsd2si", "q")
SSE_GPR(l_cvtsd2si64, "cvtsd2si", "q")
VEX_GPR(v_cvttss2si32, "vcvttss2si", "k")
VEX_GPR(v_cvtss2si32, "vcvtss2si", "k")
VEX_GPR(v_cvttss2si64, "vcvttss2si", "q")
VEX_GPR(v_cvtss2si64, "vcvtss2si", "q")
VEX_GPR(v_cvttsd2si32, "vcvttsd2si", "k")
VEX_GPR(v_cvtsd2si32, "vcvtsd2si", "k")
VEX_GPR(v_cvttsd2si64, "vcvttsd2si", "q")
VEX_GPR(v_cvtsd2si64, "vcvtsd2si", "q")

SSE_PACK(l_cvtps2dq, "cvtps2dq")
SSE_PACK(l_cvttps2dq, "cvttps2dq")
SSE_PACK(l_cvtpd2dq, "cvtpd2dq")
SSE_PACK(l_cvttpd2dq, "cvttpd2dq")
VEX_PACK(v_cvtps2dqx, "vcvtps2dq", "xmm", "xmm")
VEX_PACK(v_cvttps2dqx, "vcvttps2dq", "xmm", "xmm")
VEX_PACK(v_cvtpd2dqx, "vcvtpd2dq", "xmm", "xmm")
VEX_PACK(v_cvttpd2dqx, "vcvttpd2dq", "xmm", "xmm")
VEX_PACK(v_cvtps2dqy, "vcvtps2dq", "ymm", "ymm")
VEX_PACK(v_cvttps2dqy, "vcvttps2dq", "ymm", "ymm")
VEX_PACK(v_cvtpd2dqy, "vcvtpd2dq", "ymm", "xmm")
VEX_PACK(v_cvttpd2dqy, "vcvttpd2dq", "ymm", "xmm")

MMX_TOPI(m_cvtpd2pi, "cvtpd2pi")
MMX_TOPI(m_cvttpd2pi, "cvttpd2pi")

// The memory r/m forms are a distinct lowering: the operand arrives in a scratch vreg, and on a host with
// soft mappings active the whole instruction leaves the JIT for the C emulator.
SSE_PACK_MEM(lm_cvtpd2dq, "cvtpd2dq")
SSE_PACK_MEM(lm_cvtps2dq, "cvtps2dq")
VEX_PACK_MEM(vm_cvtpd2dqy, "vcvtpd2dqy", "ymm", "xmm") // the y suffix: a memory source has no width of its own
VEX_PACK_MEM(vm_cvttps2dqy, "vcvttps2dq", "ymm", "ymm")

enum { S_F32, S_F64 };

struct enc {
    const char *name;
    void (*fn)(void);
    int src;
    int outw;
};

static const struct enc encs[] = {
    {"cvttss2si32", l_cvttss2si32, S_F32, 4},  {"cvtss2si32", l_cvtss2si32, S_F32, 4},
    {"cvttss2si64", l_cvttss2si64, S_F32, 8},  {"cvtss2si64", l_cvtss2si64, S_F32, 8},
    {"cvttsd2si32", l_cvttsd2si32, S_F64, 4},  {"cvtsd2si32", l_cvtsd2si32, S_F64, 4},
    {"cvttsd2si64", l_cvttsd2si64, S_F64, 8},  {"cvtsd2si64", l_cvtsd2si64, S_F64, 8},
    {"vcvttss2si32", v_cvttss2si32, S_F32, 4}, {"vcvtss2si32", v_cvtss2si32, S_F32, 4},
    {"vcvttss2si64", v_cvttss2si64, S_F32, 8}, {"vcvtss2si64", v_cvtss2si64, S_F32, 8},
    {"vcvttsd2si32", v_cvttsd2si32, S_F64, 4}, {"vcvtsd2si32", v_cvtsd2si32, S_F64, 4},
    {"vcvttsd2si64", v_cvttsd2si64, S_F64, 8}, {"vcvtsd2si64", v_cvtsd2si64, S_F64, 8},
    {"cvtps2dq", l_cvtps2dq, S_F32, 16},       {"cvttps2dq", l_cvttps2dq, S_F32, 16},
    {"cvtpd2dq", l_cvtpd2dq, S_F64, 16},       {"cvttpd2dq", l_cvttpd2dq, S_F64, 16},
    {"vcvtps2dqx", v_cvtps2dqx, S_F32, 16},    {"vcvttps2dqx", v_cvttps2dqx, S_F32, 16},
    {"vcvtpd2dqx", v_cvtpd2dqx, S_F64, 16},    {"vcvttpd2dqx", v_cvttpd2dqx, S_F64, 16},
    {"vcvtps2dqy", v_cvtps2dqy, S_F32, 32},    {"vcvttps2dqy", v_cvttps2dqy, S_F32, 32},
    {"vcvtpd2dqy", v_cvtpd2dqy, S_F64, 16},    {"vcvttpd2dqy", v_cvttpd2dqy, S_F64, 16},
    {"cvtpd2pi", m_cvtpd2pi, S_F64, 8},        {"cvttpd2pi", m_cvttpd2pi, S_F64, 8},
};

static const struct enc mem_encs[] = {
    {"m-cvtpd2dq", lm_cvtpd2dq, S_F64, 16},
    {"m-cvtps2dq", lm_cvtps2dq, S_F32, 16},
    {"m-vcvtpd2dqy", vm_cvtpd2dqy, S_F64, 16},
    {"m-vcvttps2dqy", vm_cvttps2dqy, S_F32, 32},
};

static const uint64_t K64[7] = {
    0x3FF8000000000000ULL, //  1.5            in range, inexact -> #P
    0xBFF8000000000000ULL, // -1.5            RC=down must floor to -2
    0x41DFFFFFFFF00000ULL, //  2147483647.5   rounds OUT of int32 under RC=near/up -> #I alone
    0x41E0000000080000ULL, //  2147483648.25  out of range and NON-INTEGRAL: #I, never #P
    0xC1E0000000080000ULL, // -2147483648.25  out of range only under RC=down; truncates back in -> #P
    0x7FF8000000000000ULL, //  QNaN           -> indefinite + #I
    0x0000000000000001ULL, //  denormal       -> #P and NOT #D
};
static const uint32_t K32[7] = {
    0x3FC00000u, //  1.5
    0xBFC00000u, // -1.5
    0x4EFFFFFFu, //  2147483520.0  largest f32 below 2^31, and integral -- see the header note
    0x4F000000u, //  2147483648.0  2^31
    0xCF000001u, // -2147483904.0  out of range, negative
    0x7FC00000u, //  QNaN
    0x00000001u, //  denormal
};
// Per-LANE-distinct sources, so a lane mixup or a high-half that took the low half's mask shows up.
static const uint64_t L64[4] = {0x3FF8000000000000ULL, 0x41E0000000080000ULL, 0xC1E0000000080000ULL,
                                0x7FF8000000000000ULL};
static const uint32_t L32[8] = {0x3FC00000u, 0x4F000000u, 0xCF000001u, 0x7FC00000u,
                                0xBFC00000u, 0x00000001u, 0x40200000u, 0x4EFFFFFFu};

static void run(const struct enc *E, const char *rcn, const char *tag) {
    memset(Out, 0, sizeof Out);
    printf("%-13s rc=%-4s %s", E->name, rcn, tag);
    E->fn();
    printf(" out=");
    for (int i = 0; i < E->outw; i++)
        printf("%02x", Out[i]);
    printf(" mx=%02x\n", mxout & 0x3f);
}

int main(void) {
    static const char *rcn[4] = {"near", "down", "up", "zero"};
    char tag[32];
    for (unsigned rc = 0; rc < 4; rc++) {
        mxin = 0x1f80u | (rc << 13);
        for (unsigned e = 0; e < sizeof encs / sizeof encs[0]; e++) {
            for (unsigned k = 0; k < 7; k++) {
                memset(In, 0, sizeof In);
                if (encs[e].src == S_F32) {
                    for (int i = 0; i < 8; i++)
                        memcpy(In + 4 * i, &K32[k], 4);
                    snprintf(tag, sizeof tag, "in=%08x", K32[k]);
                } else {
                    for (int i = 0; i < 4; i++)
                        memcpy(In + 8 * i, &K64[k], 8);
                    snprintf(tag, sizeof tag, "in=%016llx", (unsigned long long)K64[k]);
                }
                run(&encs[e], rcn[rc], tag);
            }
        }
        // heterogeneous lanes, register and memory operands
        for (unsigned e = 0; e < sizeof mem_encs / sizeof mem_encs[0]; e++) {
            memset(In, 0, sizeof In);
            if (mem_encs[e].src == S_F32)
                memcpy(In, L32, sizeof L32);
            else
                memcpy(In, L64, sizeof L64);
            run(&mem_encs[e], rcn[rc], "lanes");
        }
        for (unsigned e = 0; e < sizeof encs / sizeof encs[0]; e++) {
            if (encs[e].outw < 16) continue; // scalar and MMX forms: covered above
            memset(In, 0, sizeof In);
            if (encs[e].src == S_F32)
                memcpy(In, L32, sizeof L32);
            else
                memcpy(In, L64, sizeof L64);
            run(&encs[e], rcn[rc], "lanes");
        }
    }
    return 0;
}
