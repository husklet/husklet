#include "avx.h"
#include "cpu.h"
#include "decoder.h"
#include "rep_runtime.h"       // the bound guest-access validators + X86_SOFT_READ/WRITE
#include "../../../host/cpu.h" // HL_HOST_CPU_*: the half-precision converter forks per host CPU

#include <fenv.h>
#include <setjmp.h>
#include <math.h>
#if defined(HL_HOST_CPU_X86_64)
#include <xmmintrin.h> // _mm_getcsr == STMXCSR: on this host the guest MXCSR IS the host MXCSR
#endif
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

// x86 SSE/AVX arithmetic rounds every multiply and add separately; never fuse mul+add into an
// FMA on the (aarch64) host, which would change the rounding of DPPS/DPPD and other softmulated
// products. The genuine FMA3 opcodes emulate fusion explicitly via __builtin_fma/fmaf, so this
// does not affect them.
//
// clang defaults to -ffp-contract=on, which fuses mul+add within a single statement, so it needs this
// pragma to force separate rounding. GCC does not recognise #pragma STDC FP_CONTRACT (it would warn
// -Wunknown-pragmas), but under -std=c11 its default is already -ffp-contract=off (no fusion), so the
// intent holds there without the pragma. Guarded to clang to keep both compilers bit-accurate and quiet.
#if defined(__clang__)
#pragma STDC FP_CONTRACT OFF
#endif

// translator/guest/x86_64/avx.c -- AVX/AVX2/AVX-512 (VEX/EVEX) emulation.
//
// The decoder (decode.c) recognises C5/C4/62-prefixed instructions and the translator exits the block to C
// (R_AVX) at each one rather than lowering it to NEON. do_avx() re-decodes the single instruction at
// cpu->rip, emulates it against the guest register file (v[]=xmm low128, vhi[], vz[], vx[], kreg[]) and
// memory, then advances rip past it. Correctness-first: one block exit per AVX insn. Unknown ops report
// the (map, opcode, pp) and exit 70 so coverage can be grown test-driven.
//
// Register model: a logical zmm is 64 bytes. For regs 0..15: [0:16)=v[2r..], [16:32)=vhi, [32:64)=vz.
// For 16..31: vx[]. VEX/EVEX writes ZERO all bits above the operation width (128/256/512) -- avx_put does
// this, so a 128-bit VEX op clears bits[128:512) (the AVX upper-zeroing rule).

static int g_avx_warned;

// ---- EXITSTAT diagnostic (env-gated, zero-cost when off): histogram of the per-insn C-emulation
// block exits (do_avx / do_sse3b) by (map, opcode) and by guest rip, dumped at process exit. Used to
// find which instructions in a real binary's hot loop still shatter translated blocks. ----
static int g_xs_on = -1;
static uint64_t g_xs_sse3b[4][256]; // [map3][op]  (map3 = 2:0F38, 3:0F3A)
static uint64_t g_xs_avx[4][256];   // [vex_map][op]

static struct {
    uint64_t rip, n;
    uint16_t map, op;
    uint8_t vex;
} g_xs_rip[4096];

static int g_xs_nrip;

static void xs_note(int vex, int map, int op, uint64_t rip) {
    if (g_xs_on < 0) g_xs_on = 0;
    if (!g_xs_on) return;
    if (vex)
        g_xs_avx[map & 3][op & 255]++;
    else
        g_xs_sse3b[map & 3][op & 255]++;
    for (int i = 0; i < g_xs_nrip; i++)
        if (g_xs_rip[i].rip == rip) {
            g_xs_rip[i].n++;
            return;
        }
    if (g_xs_nrip < 4096) {
        g_xs_rip[g_xs_nrip].rip = rip;
        g_xs_rip[g_xs_nrip].n = 1;
        g_xs_rip[g_xs_nrip].map = (uint16_t)map;
        g_xs_rip[g_xs_nrip].op = (uint16_t)op;
        g_xs_rip[g_xs_nrip].vex = (uint8_t)vex;
        g_xs_nrip++;
    }
}

void hl_x86_avx_dump(void) { // called from G_PROF_EXTRA at exit_group (destructors are bypassed by _exit)
    if (g_xs_on != 1) return;
    fprintf(stderr, "[exitstat] per-insn C-emulation exits:\n");
    for (int m = 0; m < 4; m++)
        for (int o = 0; o < 256; o++) {
            if (g_xs_sse3b[m][o])
                fprintf(stderr, "[exitstat] sse3b map%d op %02x  %llu\n", m, o, (unsigned long long)g_xs_sse3b[m][o]);
            if (g_xs_avx[m][o])
                fprintf(stderr, "[exitstat] avx map%d op %02x  %llu\n", m, o, (unsigned long long)g_xs_avx[m][o]);
        }
    for (int k = 0; k < 40; k++) { // top 40 hottest exit sites
        int best = -1;
        for (int i = 0; i < g_xs_nrip; i++)
            if (g_xs_rip[i].n && (best < 0 || g_xs_rip[i].n > g_xs_rip[best].n)) best = i;
        if (best < 0 || g_xs_rip[best].n < 2) break;
        fprintf(stderr, "[exitstat] rip %llx %s map%d op %02x  %llu\n", (unsigned long long)g_xs_rip[best].rip,
                g_xs_rip[best].vex ? "avx" : "sse3b", g_xs_rip[best].map, g_xs_rip[best].op,
                (unsigned long long)g_xs_rip[best].n);
        g_xs_rip[best].n = 0;
    }
}

static void avx_get(struct cpu *c, int r, uint8_t out[64]) {
    if (r < 16) {
        memcpy(out + 0, &c->v[2 * r], 16);
        memcpy(out + 16, &c->vhi[2 * r], 16);
        memcpy(out + 32, &c->vz[4 * r], 32);
    } else {
        memcpy(out, &c->vx[8 * (r - 16)], 64);
    }
}

static void avx_put(struct cpu *c, int r, const uint8_t in[64], int wbytes) {
    uint8_t b[64];
    memset(b, 0, 64);
    memcpy(b, in, (size_t)wbytes); // zero-extend above the op width (VEX upper-zeroing)
    if (r < 16) {
        memcpy(&c->v[2 * r], b + 0, 16);
        memcpy(&c->vhi[2 * r], b + 16, 16);
        memcpy(&c->vz[4 * r], b + 32, 32);
    } else {
        memcpy(&c->vx[8 * (r - 16)], b, 64);
    }
}

// Effective address of a memory operand. Guest pointers are host pointers in the in-process model (PIE
// images load 1:1). EVEX disp8 is compressed (disp8*N); for the common full-vector tuple N = vector bytes.
//
// non-PIE bias-fold: a non-PIE ET_EXEC image maps HIGH (+g_nonpie_bias) but its baked absolute
// pointers stay LOW (link vaddr). The JIT-emitted access path folds this at emit time (ea_bias17, decode.c);
// this C-emulator path (do_avx / do_sse3b: every VEX + legacy 0F38/0F3A SSSE3/SSE4/MOVBE guest-memory access
// funnels through here) is its exact analogue and MUST apply the same fold, or a base-register operand
// holding a baked low pointer (e.g. `pshufb xmm,[rsi]` / `vmovdqu ymm,[rax]` with rsi/rax = a low image
// address) dereferences the unmapped low vaddr -> SIGSEGV. Applied to the FINAL address so both the
// rip-relative and base+index forms are covered regardless of whether the guest rip is kept low or high:
// a resolved HIGH address (>= g_nonpie_hi) is never in the low link range, so it is left untouched.
// Inert for PIE / static-PIE / non-ET_EXEC (g_nonpie_lo == 0 -> the range test is always false).
uint64_t hl_x86_avx_address(const hl_x86_avx_state *state, uint64_t address) {
    uint64_t low = state != NULL && state->nonpie_low != NULL ? *state->nonpie_low : 0;
    uint64_t high = state != NULL && state->nonpie_high != NULL ? *state->nonpie_high : 0;
    uint64_t bias = state != NULL && state->nonpie_bias != NULL ? *state->nonpie_bias : 0;
    return low != 0 && address >= low && address < high ? address + bias : address;
}

static uint64_t avx_ea(const hl_x86_avx_state *state, struct cpu *c, struct insn *I, uint64_t rip_after, int wbytes) {
    uint64_t a;
    if (I->rip_rel) {
        a = rip_after + (uint64_t)I->disp;
    } else {
        a = 0;
        if (I->m_hasbase) a += c->r[I->m_base];
        if (I->m_hasindex) a += c->r[I->m_index] << I->m_scale;
        int64_t disp = I->disp;
        if (I->evex && I->mod == 1) disp *= wbytes ? wbytes : 1; // disp8*N, full-vector tuple
        a += (uint64_t)disp;
        if (I->seg == 1)
            a += c->fs_base;
        else if (I->seg == 2)
            a += c->gs_base;
    }
    return hl_x86_avx_address(state, a);
}

// ---- The guest-access fault bracket. -----------------------------------------------------------------
// do_avx/do_sse3b run from the DISPATCH LOOP, not from inside run_block, so neither backend's fault pad is
// armed here: a host SIGSEGV on a guest-chosen address kills the ENGINE where hardware faults the GUEST.
// Measured before this: `vmovdqu ymm0,[0x1000]` and `pshufb xmm0,[0x1000]` both exited 139 instead of
// running the guest's SIGSEGV handler -- and the VEX gathers, whose address is base + a GUEST-DATA vector
// element * scale, reach any host page at all.
//
// So nothing is dereferenced before it is proven, exactly as the string helpers do it (rep_runtime.c).
// A rejected access ABANDONS the instruction with nothing committed and exits to the dispatcher with
// R_SOFTMISS -- the same protocol emit.c's memory guard emits -- which either resolves a logical mapping
// and retries or delivers the guest SIGSEGV with the right si_addr and si_code. cpu->rip stays ON the
// instruction, so a retry re-executes it. The abandon is a plain longjmp, not a signal pad: the decision
// is taken BEFORE the access, so no signal is ever raised and there is no mask to restore.
static __thread jmp_buf g_avx_pad;
static __thread struct cpu *g_avx_cpu; // the cpu the pad was armed for
static __thread uint64_t g_avx_pc;     // the architectural PC of the instruction being emulated

static void avx_abandon(uint64_t guest, uint64_t length, uint32_t required) {
    struct cpu *c = g_avx_cpu;
    c->bus_ea = guest;
    c->soft_width = length;
    c->soft_required = required;
    c->rip = g_avx_pc;
    c->reason = R_SOFTMISS;
    longjmp(g_avx_pad, 1);
}

// #UD from an emulated instruction: SIGILL/ILL_ILLOPN with si_addr = the instruction, as interp.c raises it.
static void avx_undefined(void) {
    struct cpu *c = g_avx_cpu;
    c->divop = 4u | (2u << 8); // (linux_signo | si_code<<8)
    c->rip = g_avx_pc;
    c->reason = R_TRAP;
    longjmp(g_avx_pad, 1);
}

// Non-abandoning probe+transfer. 1 = done, 0 = the guest may not touch this span and NOTHING was copied.
// The gathers need this form: a faulting element must still commit the elements that completed.
static int avx_try_read(const hl_x86_avx_state *state, uint64_t guest, void *destination, size_t length) {
    int handled = state != NULL && state->memory_read != NULL ? state->memory_read(guest, destination, length) : 0;
    if (handled < 0) return 0;
    if (handled > 0) return 1;
    if (!hl_x86_guest_readable(guest, length)) return 0;
    memcpy(destination, (const void *)(uintptr_t)guest, length);
    return 1;
}

static int avx_try_write(const hl_x86_avx_state *state, uint64_t guest, const void *source, size_t length) {
    int handled = state != NULL && state->memory_write != NULL ? state->memory_write(guest, source, length) : 0;
    if (handled < 0) return 0;
    if (handled > 0) return 1;
    if (!hl_x86_guest_writable(guest, length)) return 0;
    memcpy((void *)(uintptr_t)guest, source, length);
    return 1;
}

static int avx_memory_read(const hl_x86_avx_state *state, uint64_t guest, void *destination, size_t length) {
    if (!avx_try_read(state, guest, destination, length)) avx_abandon(guest, length, X86_SOFT_READ);
    return 1;
}

static int avx_memory_write(const hl_x86_avx_state *state, uint64_t guest, const void *source, size_t length) {
    if (!avx_try_write(state, guest, source, length)) avx_abandon(guest, length, X86_SOFT_WRITE);
    return 1;
}

// Read the r/m operand (register or memory) into buf as `wbytes` bytes.
static void avx_get_rm(const hl_x86_avx_state *state, struct cpu *c, struct insn *I, uint64_t rip_after, int wbytes,
                       uint8_t buf[64]) {
    memset(buf, 0, 64);
    if (I->is_mem) {
        uint64_t a = avx_ea(state, c, I, rip_after, wbytes);
        (void)avx_memory_read(state, a, buf, (size_t)wbytes);
    } else {
        uint8_t t[64];
        avx_get(c, I->rm_reg, t);
        memcpy(buf, t, (size_t)wbytes);
    }
}

static void avx_put_rm(const hl_x86_avx_state *state, struct cpu *c, struct insn *I, uint64_t rip_after, int wbytes,
                       const uint8_t buf[64]) {
    if (I->is_mem) {
        uint64_t a = avx_ea(state, c, I, rip_after, wbytes);
        (void)avx_memory_write(state, a, buf, (size_t)wbytes);
    } else {
        avx_put(c, I->rm_reg, buf, wbytes);
    }
}

// ---- scalar FP helpers used by the VEX arithmetic/FMA lowerings ----
// map-1 0x58..0x5F packed/scalar arithmetic, by opcode.
// x = src1 (VEX.vvvv), y = src2 (r/m). H10: x86 MIN/MAX are defined as (src1<src2)?src1:src2 and
// (src1>src2)?src1:src2 -- so on NaN, equal, or +-0 (where the strict comparison is false) the result is
// src2. The C ternaries below reproduce that EXACTLY under strict IEEE: `x < y` is false whenever either
// operand is NaN or they are equal, yielding y (=src2). Built at -O2 with no -ffast-math, so the compiler
// may not fold these to a NaN-propagating fminnm/fmaxnm; do NOT rewrite them as fmin/fmax/fminf/fmaxf.
// x86 default/indefinite-NaN sign: when an FP op GENERATES a NaN with no NaN input (0/0, inf/inf,
// 0*inf, inf-inf), x86 yields the QNaN floating-point INDEFINITE whose sign bit is SET (single
// 0xFFC00000, double 0xFFF8000000000000). The host (arm64) instead returns the DEFAULT NaN with sign
// CLEAR (0x7FC00000 / 0x7FF8000000000000) -- identical payload, opposite sign. A NaN PROPAGATED from an
// input keeps that input's sign on both ISAs, so only the generated case (result NaN AND neither input
// NaN) is fixed up here. Matches the legacy-SSE inline fixup (translate.c emit_dnan_pre/post).
// NaN classification (exponent all-ones, mantissa nonzero).
static int nan32(uint32_t u) {
    return (u & 0x7f800000u) == 0x7f800000u && (u & 0x007fffffu) != 0;
}

static int nan64(uint64_t u) {
    return (u & 0x7ff0000000000000ull) == 0x7ff0000000000000ull && (u & 0x000fffffffffffffull) != 0;
}

// x86 add/sub/mul/div result NaN handling. Two distinct rules, both of which the host NEON FADD/FMUL/FSUB/
// FDIV that computed `r` gets WRONG, so we recompute from the operands here:
//   (1) INPUT NaN: plain SRC1 PRIORITY, quieted -- src1 if it is a NaN, else src2, with the mantissa MSB
//       set (payload + sign preserved). Not commutative, so HADD/HSUB's pairing order below is observable.
//       Measured on Zen 4 across all 64 ordered pairs of {+-SNaN min/max payload, +-QNaN min/max payload},
//       single and double, for ADD/SUB/MUL/DIV in SSE, SSE scalar, VEX, and the SSE3 horizontal/addsub
//       family: src1 wins every non-degenerate pair, on every one of them.
//       This used to implement softfloat's `float_2nan_prop_x87` (QNaN beats SNaN, then larger significand,
//       then positive) because qemu-x86_64 was taken as the oracle where it disagreed with the SDM. The
//       SDM was right and the oracle was modelling the x87 rule for SSE; that cost 107 golden lines.
//       ARM's rule is SNaN-first-else-src1, which agrees with x86 on 12 of 16 NaN pairs and diverges on
//       exactly (src1 QNaN, src2 SNaN) -- so the JIT's NaN-input gate into this code is still load-bearing.
//   (2) GENERATED NaN (result NaN, NO input NaN: 0/0, inf/inf, 0*inf, inf-inf): x86 yields the QNaN
//       floating-point INDEFINITE with the sign bit SET (0xFFC00000 / 0xFFF8000000000000); ARM yields the
//       positive default NaN. Same payload, opposite sign.
// MIN/MAX are NOT covered here: they return src2 VERBATIM (not even quieted) on any NaN, which is what the
// `x < y ? x : y` ternaries in avx_fp_arith_* already produce. Confirmed on the same sweep.
static float avx_dnan_f32(float r, float x, float y) {
    uint32_t xb, yb;
    memcpy(&xb, &x, 4);
    memcpy(&yb, &y, 4);
    int xn = nan32(xb), yn = nan32(yb);
    if (xn || yn) {
        uint32_t w = (xn ? xb : yb) | 0x00400000u; // src1 priority, quieted
        memcpy(&r, &w, 4);
        return r;
    }
    if (r != r) {
        uint32_t u = 0xFFC00000u;
        memcpy(&r, &u, 4);
    }
    return r;
}

static double avx_dnan_f64(double r, double x, double y) {
    uint64_t xb, yb;
    memcpy(&xb, &x, 8);
    memcpy(&yb, &y, 8);
    int xn = nan64(xb), yn = nan64(yb);
    if (xn || yn) {
        uint64_t w = (xn ? xb : yb) | 0x0008000000000000ull; // src1 priority, quieted
        memcpy(&r, &w, 8);
        return r;
    }
    if (r != r) {
        uint64_t u = 0xFFF8000000000000ull;
        memcpy(&r, &u, 8);
    }
    return r;
}

static void fma_raise_ie(void) {
    volatile double z = 0.0, q = z / z; // 0/0 raises #I and nothing else, on either host
    (void)q;
}

// x86 FMA, three-operand analogue of avx_dnan_*. Computes the SDM's a*b+c with `nmul`/`nadd`
// selecting the fmsub/fnmadd/fnmsub sign variants; a and b are the multiplicands, c the addend.
// Measured on Zen 4 over all 1728 triples of {+-SNaN,+-QNaN}x{min,max payload} + {1.0, +0, +-inf}
// for every form (132/213/231), every sign variant, both widths, scalar and packed:
//   (1) OPERAND NaN: the FIRST NaN in a*b+c ORDER wins -- a, then b, then c -- propagated VERBATIM
//       (sign and payload) with only the quiet bit forced. The addend never beats a multiplicand
//       and b never beats a, so the product/addend negation never touches the propagated sign. The
//       only flag raised is #I, and only for an SNaN operand: a NaN operand suppresses even the #D
//       of a denormal operand, and the #I that 0*inf would otherwise raise. So the host FMA must
//       NOT run here. aarch64 FMADD answers this case SNaN-first-then-ADDEND-first, and for 0*inf
//       with a QNaN addend the ARM ARM mandates DefaultNaN + IOC (measured on bare NEON under
//       qemu, so it is the ISA and not the emulator). An x86-64 host is no better: which
//       multiplicand lands in the encoding's first slot is the compiler's choice, and it chose the
//       wrong one for the float path -- 13008 of 43680 NaN triples were wrong on THIS host.
//   (2) GENERATED NaN (no NaN operand: 0*inf, or an inf-inf in the fused add): x86 yields the QNaN
//       indefinite with the sign SET, ARM the positive default NaN -- same payload, opposite sign.
// The negations are FNEG, not a multiply by -1: exact, and it cannot raise a flag of its own.
static float fma_x86_f32(float a, float b, float c, int nmul, int nadd) {
    uint32_t ab, bb, cb;
    memcpy(&ab, &a, 4);
    memcpy(&bb, &b, 4);
    memcpy(&cb, &c, 4);
    if (nan32(ab) || nan32(bb) || nan32(cb)) {
        // #I follows ANY SNaN operand, not the winner: a QNaN a beside an SNaN b still raises it.
        if ((nan32(ab) && !(ab & 0x00400000u)) || (nan32(bb) && !(bb & 0x00400000u)) ||
            (nan32(cb) && !(cb & 0x00400000u)))
            fma_raise_ie();
        uint32_t w = (nan32(ab) ? ab : nan32(bb) ? bb : cb) | 0x00400000u;
        memcpy(&a, &w, 4);
        return a;
    }
    float r = __builtin_fmaf(nmul ? -a : a, b, nadd ? -c : c);
    if (r != r) {
        uint32_t u = 0xFFC00000u;
        memcpy(&r, &u, 4);
    }
    return r;
}

static double fma_x86_f64(double a, double b, double c, int nmul, int nadd) {
    uint64_t ab, bb, cb;
    memcpy(&ab, &a, 8);
    memcpy(&bb, &b, 8);
    memcpy(&cb, &c, 8);
    if (nan64(ab) || nan64(bb) || nan64(cb)) {
        // #I follows ANY SNaN operand, not the winner: a QNaN a beside an SNaN b still raises it.
        if ((nan64(ab) && !(ab & 0x0008000000000000ull)) || (nan64(bb) && !(bb & 0x0008000000000000ull)) ||
            (nan64(cb) && !(cb & 0x0008000000000000ull)))
            fma_raise_ie();
        uint64_t w = (nan64(ab) ? ab : nan64(bb) ? bb : cb) | 0x0008000000000000ull;
        memcpy(&a, &w, 8);
        return a;
    }
    double r = __builtin_fma(nmul ? -a : a, b, nadd ? -c : c);
    if (r != r) {
        uint64_t u = 0xFFF8000000000000ull;
        memcpy(&r, &u, 8);
    }
    return r;
}

// Live guest rounding mode in the MXCSR.RC encoding {0=nearest,1=down,2=up,3=truncate}. The guest MXCSR IS
// the host control register on both hosts: MXCSR itself here, FPCR.RMode on aarch64 -- where ldmxcsr swaps
// the two directed modes on the way in, so this swaps them back.
static int cvt_host_rc(void) {
#if defined(HL_HOST_CPU_X86_64)
    return (int)((_mm_getcsr() >> 13) & 3u);
#elif defined(HL_HOST_CPU_AARCH64)
    unsigned long fpcr;
    __asm__ volatile("mrs %0, fpcr" : "=r"(fpcr));
    unsigned m = (unsigned)((fpcr >> 22) & 3u);
    return m == 1 ? 2 : m == 2 ? 1 : (int)m;
#else
    switch (fegetround()) {
    case FE_DOWNWARD: return 1;
    case FE_UPWARD: return 2;
    case FE_TOWARDZERO: return 3;
    default: return 0;
    }
#endif
}

// Host sticky FP exception state, parked across the rounding step below. Volatile asm, not the _mm_*csr
// intrinsics or fenv: the intrinsics are ordinary function-like and can be reordered around the FP they
// are meant to bracket, and glibc's fesetexceptflag also writes the x87 status word, which is live guest
// state here.
static unsigned cvt_fp_flags(void) {
#if defined(HL_HOST_CPU_X86_64)
    unsigned v;
    __asm__ volatile("stmxcsr %0" : "=m"(v));
    return v & 0x3fu;
#elif defined(HL_HOST_CPU_AARCH64)
    unsigned long f;
    __asm__ volatile("mrs %0, fpsr" : "=r"(f));
    return (unsigned)(f & 0x9ful); // IOC/DZC/OFC/UFC/IXC + IDC(7)
#else
    fexcept_t f = 0;
    fegetexceptflag(&f, FE_ALL_EXCEPT);
    return (unsigned)f;
#endif
}

static void cvt_fp_flags_set(unsigned keep) {
#if defined(HL_HOST_CPU_X86_64)
    unsigned v;
    __asm__ volatile("stmxcsr %0" : "=m"(v));
    v = (v & ~0x3fu) | keep;
    __asm__ volatile("ldmxcsr %0" : : "m"(v));
#elif defined(HL_HOST_CPU_AARCH64)
    unsigned long f;
    __asm__ volatile("mrs %0, fpsr" : "=r"(f));
    f = (f & ~0x9ful) | keep;
    __asm__ volatile("msr fpsr, %0" : : "r"(f));
#else
    fexcept_t f = (fexcept_t)keep;
    fesetexceptflag(&f, FE_ALL_EXCEPT);
#endif
}

// Round to integral in the live mode, contributing NO exception of its own -- the caller raises #I and #P
// itself, and must be able to raise NEITHER. No rounding primitive here is trustworthy enough to do that
// unaided: __builtin_rint on x86-64 with no ROUNDSD assumable becomes |x| + 2^52 - 2^52 with the sign
// reapplied, which rounds the MAGNITUDE (RC=down returned -1 for -1.5); on aarch64 it is FRINTX, which
// reports #P; and glibc's own trunc/floor resolve to a ROUNDSD that reports #P for an inexact source and
// #D for a denormal one, neither of which an x86 convert raises. So park the sticky flags across it.
#define CVT_ROUND(name, ty, sfx)                                                                                       \
    static ty name(ty x, int trunc) {                                                                                  \
        unsigned parked = cvt_fp_flags();                                                                              \
        ty r;                                                                                                          \
        switch (trunc ? 3 : cvt_host_rc()) {                                                                           \
        case 1: r = __builtin_floor##sfx(x); break;                                                                    \
        case 2: r = __builtin_ceil##sfx(x); break;                                                                     \
        case 3: r = __builtin_trunc##sfx(x); break;                                                                    \
        default: r = __builtin_roundeven##sfx(x); break;                                                               \
        }                                                                                                              \
        cvt_fp_flags_set(parked);                                                                                      \
        return r;                                                                                                      \
    }
CVT_ROUND(cvt_round_d, double, )
CVT_ROUND(cvt_round_f, float, f)

static void cvt_raise_pe(void) {
    volatile double a = 1.0, b = 3.0, q = a / b; // 1/3 is inexact in every mode and raises only #P
    (void)q;
}

// OR an exact set of exceptions, named by MXCSR bit (SSE_XI..SSE_XP), into the host sticky state. Setting
// the bits beats synthesising each one with an arithmetic op (cvt_raise_pe's 1/3 above): no operation
// raises exactly one exception in every case, and on aarch64 no operation raises #D at all.
#define SSE_XI 0x01u // invalid
#define SSE_XD 0x02u // denormal operand
#define SSE_XZ 0x04u // divide by zero
#define SSE_XO 0x08u // overflow
#define SSE_XU 0x10u // underflow
#define SSE_XP 0x20u // precision

static void sse_raise(unsigned mxcsr_bits) {
    if (!mxcsr_bits) return;
#if defined(HL_HOST_CPU_AARCH64)
    static const unsigned to_fpsr[6] = {0, 7, 1, 2, 3, 4}; // IE<-IOC DE<-IDC ZE<-DZC OE<-OFC UE<-UFC PE<-IXC
    unsigned host = 0;
    for (unsigned i = 0; i < 6; i++)
        if (mxcsr_bits & (1u << i)) host |= 1u << to_fpsr[i];
    cvt_fp_flags_set(cvt_fp_flags() | host);
#else
    cvt_fp_flags_set(cvt_fp_flags() | mxcsr_bits);
#endif
}

// Guest denormals-are-zero. On an x86-64 host the guest MXCSR IS the host MXCSR, so read DAZ(6) directly;
// on aarch64 the guest's FTZ|DAZ is carried by FPCR.FZ(24), which ldmxcsr set (see translate.c).
static int sse_daz_active(void) {
#if defined(HL_HOST_CPU_AARCH64)
    unsigned long f;
    __asm__ volatile("mrs %0, fpcr" : "=r"(f));
    return (f >> 24) & 1u;
#elif defined(HL_HOST_CPU_X86_64)
    return (_mm_getcsr() >> 6) & 1u;
#else
    return 0;
#endif
}

// A denormal SOURCE, decided on the BIT PATTERN. Any FP comparison would do it in fewer lines and is not
// available: COMISD/UCOMISD themselves report #D for a denormal operand, so the test would raise the flag
// it is measuring. Exponent field zero with a nonzero significand; +-0 is not denormal.
static int sse_is_denorm_f32(uint32_t b) {
    return (b & 0x7f800000u) == 0 && (b & 0x007fffffu) != 0;
}

static int sse_is_denorm_f64(uint64_t b) {
    return (b & UINT64_C(0x7ff0000000000000)) == 0 && (b & UINT64_C(0x000fffffffffffff)) != 0;
}

// x86 CVT[T]xx2{SI,DQ,PI}, the whole rule, for every convert the C emulator owns. Measured on Zen 4 over
// the four RC modes and a kit whose out-of-range values include NON-INTEGERS -- the only ones that can
// tell these three apart, and they exist only for an f64 source with a 32-bit destination:
//   * out of range AFTER rounding, or NaN -> the integer indefinite and #I ALONE. Not #P, even from an
//     inexact source; not #D, even from a denormal one. So no host FP op may run on that path.
//   * in range -> #P iff the rounding changed the value, and nothing else.
// The rounding itself is exception-free, so both flags are raised explicitly. Everything here used to be
// absent: the VEX scalar forms raised no MXCSR bit at all on either host.
// "Is it NaN" and "did the rounding change it" are decided on the BIT PATTERN, not with == : UCOMISD/
// COMISD report #D for a denormal operand (SDM, and measured), and a convert must not. Bitwise inequality
// is exactly inexactness here, because round-to-integral keeps the sign of a zero and NaN is already out.
#define CVT_TO_INT(name, ty, uty, isnan, rnd)                                                                          \
    static int64_t name(ty x, int trunc, int w64) {                                                                    \
        ty lim = w64 ? (ty)9223372036854775808.0 : (ty)2147483648.0;                                                   \
        int64_t indef = w64 ? (int64_t)0x8000000000000000ull : (int64_t)(int32_t)0x80000000u;                          \
        uty xb, rb;                                                                                                    \
        memcpy(&xb, &x, sizeof xb);                                                                                    \
        if (isnan(xb)) {                                                                                               \
            fma_raise_ie();                                                                                            \
            return indef;                                                                                              \
        }                                                                                                              \
        ty r = rnd(x, trunc);                                                                                          \
        if (r >= lim || r < -lim) { /* r is integral, never denormal, so these cannot report #D */                     \
            fma_raise_ie();                                                                                            \
            return indef;                                                                                              \
        }                                                                                                              \
        memcpy(&rb, &r, sizeof rb);                                                                                    \
        if (rb != xb) cvt_raise_pe();                                                                                  \
        return (int64_t)r;                                                                                             \
    }
CVT_TO_INT(cvt_x86_d2i, double, uint64_t, nan64, cvt_round_d)
CVT_TO_INT(cvt_x86_f2i, float, uint32_t, nan32, cvt_round_f)

static float avx_fp_arith_f32(int op, float x, float y) {
    switch (op) {
    case 0x58: return avx_dnan_f32(x + y, x, y);
    case 0x59: return avx_dnan_f32(x * y, x, y);
    case 0x5C: return avx_dnan_f32(x - y, x, y);
    case 0x5E: return avx_dnan_f32(x / y, x, y);
    case 0x5D: return x < y ? x : y; // min: NaN/equal/+-0 -> src2 (x86-exact)
    default: return x > y ? x : y;   // 0x5F max: NaN/equal/+-0 -> src2 (x86-exact)
    }
}

static double avx_fp_arith_f64(int op, double x, double y) {
    switch (op) {
    case 0x58: return avx_dnan_f64(x + y, x, y);
    case 0x59: return avx_dnan_f64(x * y, x, y);
    case 0x5C: return avx_dnan_f64(x - y, x, y);
    case 0x5E: return avx_dnan_f64(x / y, x, y);
    case 0x5D: return x < y ? x : y; // min: NaN/equal/+-0 -> src2 (x86-exact)
    default: return x > y ? x : y;   // 0x5F max: NaN/equal/+-0 -> src2 (x86-exact)
    }
}

// VCMP{PS,PD,SS,SD} predicate (imm[4:0]). Float operands are promoted to double exactly, so a single
// comparator serves both widths; the 16..31 signaling variants yield the same boolean as 0..15.
static int avx_cmp_pred(double x, double y, int pred) {
    switch (pred & 0xf) {
    case 0: return x == y;                             // EQ_OQ
    case 1: return x < y;                              // LT_OS
    case 2: return x <= y;                             // LE_OS
    case 3: return !(x == x) || !(y == y);             // UNORD_Q
    case 4: return !(x == y);                          // NEQ_UQ
    case 5: return !(x < y);                           // NLT_US
    case 6: return !(x <= y);                          // NLE_US
    case 7: return (x == x) && (y == y);               // ORD_Q
    case 8: return (x == y) || !(x == x) || !(y == y); // EQ_UQ
    case 9: return !(x >= y);                          // NGE_US
    case 10: return !(x > y);                          // NGT_US
    case 11: return 0;                                 // FALSE_OQ
    case 12: return (x < y) || (x > y);                // NEQ_OQ
    case 13: return x >= y;                            // GE_OS
    case 14: return x > y;                             // GT_OS
    default: return 1;                                 // TRUE_UQ
    }
}

// F16C uses the host's native fp16 so the half<->single conversion matches x86. `imm` is the vcvtps2ph
// rounding-control immediate: imm[2]=1 -> use MXCSR (host FPCR already tracks the guest rounding mode),
// else imm[1:0] selects 0=nearest-even, 1=down(-inf), 2=up(+inf), 3=truncate(toward-zero). x86 imm[1:0]
// maps onto ARM FPCR.RMode {0=nearest, 1=+inf, 2=-inf, 3=zero}. Do the single->half FCVT in inline asm
// under a locally-set FPCR so a directed mode is honored precisely (a plain _Float16 cast can be a
// round-to-nearest libcall or be reordered around a fesetround), then restore FPCR.
// _Float16 is a pre-C23 GNU/clang extension the half-precision (F16C/AVX-512-FP16) path genuinely needs;
// silence the -Wpedantic noise narrowly rather than dropping the type.
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"

#if !defined(HL_HOST_CPU_AARCH64)
// Portable single->half for every host CPU that is not AArch64. NOT the F16C intrinsic (_cvtss_sh), which
// needs -mf16c that this build cannot assume of the host micro-architecture, and not a _Float16 cast,
// which is round-to-nearest-even whatever the FP environment says. `mode` is the x86 rounding control as
// ROUND* imm[1:0]: 0=nearest-even, 1=down, 2=up, 3=truncate. A NaN result is QUIET even from a signalling
// source; overflow gives Infinity only when the mode rounds away from zero.
// `flags`, when non-NULL, receives the MXCSR bits VCVTPS2PH raises -- measured against native for all six
// imm encodings over the overflow, underflow and denormal boundaries. Every clause falls out of a value
// this function already had: #P from round|sticky, #U from "tiny BEFORE rounding, and inexact" (which is
// exponent < -14, so 65519.996 -> 0x03ff under RM is #U even though it lands on a normal), #O from the
// same exponent16 >= 0x1f the return below tests (so +65520 under RM, which rounds DOWN to the largest
// finite, is correctly NOT an overflow), and #I from an SNaN. #D is the CALLER's: it is the one bit both
// host paths need alike, and the caller already inspects the source for DAZ.
static uint16_t avx_f32_to_f16_software(float f, unsigned mode, unsigned *flags) {
    uint32_t bits;
    memcpy(&bits, &f, 4);
    uint32_t sign = bits >> 31;
    uint32_t biased_exponent = (bits >> 23) & 0xffu;
    uint32_t mantissa = bits & 0x7fffffu;
    uint16_t sign16 = (uint16_t)(sign << 15);
    // Directed modes round by sign alone; the same predicate picks Infinity vs largest-finite on overflow.
    uint32_t away = ((mode == 1 && sign != 0) || (mode == 2 && sign == 0)) ? 1u : 0u;
    if (biased_exponent == 0xffu) {
        if (flags && mantissa != 0 && !(mantissa & 0x400000u)) *flags = SSE_XI; // SNaN
        return mantissa == 0 ? (uint16_t)(sign16 | 0x7c00u) : (uint16_t)(sign16 | 0x7e00u | (uint16_t)(mantissa >> 13));
    }
    if (biased_exponent == 0 && mantissa == 0) return sign16; // +-0 exact, sign preserved
    // significand * 2^(exponent-23), implicit bit explicit (a binary32 subnormal has none; exponent -126).
    uint32_t significand = biased_exponent == 0 ? mantissa : (mantissa | 0x800000u);
    int32_t exponent = (int32_t)(biased_exponent == 0 ? 1u : biased_exponent) - 127;
    // Bits to drop to reach the half's ulp: 13 normally, more when subnormal (ulp pinned at 2^-24). The
    // clamp at 25 only avoids UB; a wider shift gives the same round/sticky.
    int32_t shift = exponent >= -14 ? 13 : -1 - exponent;
    if (shift > 25) shift = 25;
    uint32_t half = significand >> shift;
    uint32_t round_bit = (significand >> (shift - 1)) & 1u;
    uint32_t sticky = (significand & ((1u << (shift - 1)) - 1u)) != 0 ? 1u : 0u;
    if (mode == 0)
        half += round_bit & (sticky | (half & 1u)); // nearest-even: up on >half, or half-to-even
    else
        half += away & (round_bit | sticky);
    if (flags && (round_bit | sticky)) *flags |= SSE_XP | (exponent < -14 ? SSE_XU : 0u);
    if (exponent < -14) return (uint16_t)(sign16 | half); // subnormal; a carry out lands on 0x0400
    int32_t exponent16 = exponent + 15;
    if (half >> 11) { // the increment carried into the next binade
        half >>= 1;
        exponent16++;
    }
    if (exponent16 >= 0x1f) {
        if (flags) *flags |= SSE_XO | SSE_XP;
        return (uint16_t)(sign16 | (away != 0 || mode == 0 ? 0x7c00u : 0x7bffu));
    }
    return (uint16_t)(sign16 | ((uint32_t)exponent16 << 10) | (half & 0x3ffu)); // implicit bit 0x400 drops out
}
#endif

#if defined(HL_HOST_CPU_X86_64)
// Live MXCSR.RC in the ROUND*-immediate encoding; defined with sse_round_d.
static int sse_host_rounding_control(void);
#endif

// The two host paths reach the same exception set from opposite directions: the aarch64 FCVT raises #I/#U/
// #O/#P itself and can only miss #D (ARM reports IDC solely when FPCR.FZ flushed an input), while the
// software converter raises nothing and reports the whole set through its out-param.
static uint16_t avx_f32_to_f16(float f, int imm) {
    uint32_t fb;
    memcpy(&fb, &f, 4);
    if (sse_is_denorm_f32(fb)) { // DAZ zeroes the source first, making the conversion exact and flagless
        if (sse_daz_active()) {
            fb &= 0x80000000u;
            memcpy(&f, &fb, 4);
        } else
            sse_raise(SSE_XD);
    }
#if defined(HL_HOST_CPU_AARCH64)
    _Float16 h;
    uint16_t o;
    if (imm & 4) { // imm[2]=1: MXCSR-controlled (current host FPCR mirrors guest MXCSR rounding)
        h = (_Float16)f;
        memcpy(&o, &h, 2);
        return o;
    }
    static const unsigned rm[4] = {0, 2, 1, 3}; // x86 nearest/down/up/trunc -> ARM RMode nearest/-inf/+inf/zero
    unsigned long fpcr_orig, fpcr_new;
    __asm__ volatile("mrs %0, fpcr" : "=r"(fpcr_orig));
    fpcr_new = (fpcr_orig & ~(3UL << 22)) | ((unsigned long)rm[imm & 3] << 22);
    __asm__ volatile("msr fpcr, %1\n\tisb\n\tfcvt %h0, %s2" : "=w"(h) : "r"(fpcr_new), "w"(f));
    __asm__ volatile("msr fpcr, %0\n\tisb" ::"r"(fpcr_orig));
    memcpy(&o, &h, 2);
    return o;
#else
    // imm[2]=1 asks for the MXCSR-controlled mode; with no host FPCR, use the host FP environment.
    unsigned mode = (unsigned)(imm & 3);
    if (imm & 4) {
#if defined(HL_HOST_CPU_X86_64)
        // NOT fegetround(): glibc's x86-64 fegetround reads the **x87** control word, not the guest's
        // MXCSR (written by LDMXCSR, not FLDCW). MXCSR.RC already uses this encoding.
        mode = (unsigned)sse_host_rounding_control();
#else
        switch (fegetround()) {
        case FE_DOWNWARD: mode = 1; break;
        case FE_UPWARD: mode = 2; break;
        case FE_TOWARDZERO: mode = 3; break;
        default: mode = 0; break;
        }
#endif
    }
    unsigned flags = 0;
    uint16_t o = avx_f32_to_f16_software(f, mode, &flags);
    sse_raise(flags);
    return o;
#endif
}

static float avx_f16_to_f32(uint16_t bits) {
    _Float16 h;
    memcpy(&h, &bits, 2);
    return (float)h;
}

#pragma GCC diagnostic pop

static double sse_round_d(double x, int imm); // imm = the ROUND* imm8 (mode, MXCSR-select, #P suppress)
static float sse_round_f(float x, int imm);

// AES round primitives (defined with the legacy do_sse3b block below) reused by the VEX AES forms.
static const uint8_t k_aes_sbox[256];
static const uint8_t k_aes_isbox[256];
static void aes_subbytes(uint8_t s[16], const uint8_t box[256]);
static void aes_shiftrows(const uint8_t in[16], uint8_t out[16], int inv);
static void aes_mixcolumns(uint8_t s[16], int inv);
static inline int sat_s16(int v);

static uint64_t simd_element_mask(int size) {
    return (UINT64_C(1) << (size * 8)) - 1u;
}

static uint64_t simd_element_negate(uint64_t value, int size) {
    return (UINT64_C(0) - value) & simd_element_mask(size);
}

static int simd_element_negative(uint64_t value, int size) {
    return (value & (UINT64_C(1) << (size * 8 - 1))) != 0;
}

static void do_avx(const hl_x86_avx_state *state, struct cpu *c);
static void do_sse3b(const hl_x86_avx_state *state, struct cpu *c);

enum avx_dispatch_result {
    AVX_DISPATCH_UNIMPLEMENTED = -1,
    AVX_DISPATCH_UNMATCHED = 0,
    AVX_DISPATCH_HANDLED = 1,
};

// BMI instructions share VEX decoding with AVX but operate exclusively on the general register file.
// Keep that integer family outside the vector dispatcher so its flag and destination rules are reviewable
// independently from the packed-vector opcode maps.
static enum avx_dispatch_result avx_dispatch_bmi(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                 uint64_t next, int map, int op, int pp, int rd, int vv) {
    if (!((map == 2 && (op == 0xf2 || op == 0xf3 || op == 0xf5 || op == 0xf6 || op == 0xf7)) ||
          (map == 3 && op == 0xf0)))
        return AVX_DISPATCH_UNMATCHED;

    int wb = I->vex_w ? 64 : 32;
    uint64_t M = I->vex_w ? ~0ull : 0xffffffffull;
    uint64_t rm;
    if (I->is_mem) {
        uint64_t ea = avx_ea(state, c, I, next, I->vex_w ? 8 : 4);
        rm = 0;
        (void)avx_memory_read(state, ea, &rm, I->vex_w ? 8u : 4u);
    } else
        rm = c->r[I->rm_reg] & M;
    uint64_t v2 = c->r[vv] & M, res = 0;
    int setfl = 0, cf = 0, zf, sf, dest = rd;
    if (map == 2 && op == 0xf5 && pp == 0) { // BZHI rd, rm, vvvv: zero bits >= index(vvvv&0xff)
        int idx = (int)(v2 & 0xff);
        res = (idx >= wb) ? rm : (rm & ((idx == 0) ? 0 : ((1ull << idx) - 1)));
        cf = (idx > wb - 1);
        setfl = 1;
    } else if (map == 2 && op == 0xf7 && pp == 0) { // BEXTR rd, rm, vvvv(start:len in al:ah of vvvv)
        int start = (int)(v2 & 0xff), len = (int)((v2 >> 8) & 0xff);
        uint64_t t = (start >= wb) ? 0 : (rm >> start);
        res = (len >= wb) ? t : (t & ((len == 0) ? 0 : ((1ull << len) - 1)));
        setfl = 1;
    } else if (map == 2 && op == 0xf7 && pp == 1) { // SHLX rd, rm, vvvv
        res = rm << (v2 & (uint64_t)(wb - 1));
    } else if (map == 2 && op == 0xf7 && pp == 2) { // SARX rd, rm, vvvv (arithmetic)
        int sh = (int)(v2 & (uint64_t)(wb - 1));
        res = (uint64_t)(I->vex_w ? ((int64_t)rm >> sh) : ((int32_t)rm >> sh));
    } else if (map == 2 && op == 0xf7 && pp == 3) { // SHRX rd, rm, vvvv
        res = rm >> (v2 & (uint64_t)(wb - 1));
    } else if (map == 2 && op == 0xf6 && pp == 3) { // MULX rd(hi):vvvv(lo) = rdx * rm
// __int128 is a pre-C23 GNU/clang extension the widening multiply/carryless paths need; scope the
// -Wpedantic silence to the declaration rather than dropping the type.
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"
        unsigned __int128 p = (unsigned __int128)(c->r[RDX] & M) * (unsigned __int128)rm;
#pragma GCC diagnostic pop
        c->r[vv] = (uint64_t)p & M;
        res = (uint64_t)(I->vex_w ? (p >> 64) : ((p >> 32) & 0xffffffff));
    } else if (map == 3 && op == 0xf0 && pp == 3) { // RORX rd, rm, imm8 (no flags)
        int sh = (int)(I->imm & (wb - 1));
        res = sh ? ((rm >> sh) | (rm << (wb - sh))) : rm;
        if (!I->vex_w) res &= M;
    } else if (map == 2 && op == 0xf5 && pp == 2) { // PEXT rd, vvvv(src), rm(mask) -- F3 prefix => pp=2
        uint64_t src = v2, msk = rm, bit = 1;
        for (uint64_t m = msk; m; m &= m - 1) {
            if (src & (m & (~m + 1))) res |= bit;
            bit <<= 1;
        }
    } else if (map == 2 && op == 0xf5 && pp == 3) { // PDEP rd, vvvv(src), rm(mask) -- F2 prefix => pp=3
        uint64_t src = v2, msk = rm, bit = 1;
        for (uint64_t m = msk; m; m &= m - 1) {
            if (src & bit) res |= (m & (~m + 1));
            bit <<= 1;
        }
    } else if (map == 2 && op == 0xf2 && pp == 0) { // ANDN rd, vvvv, rm: (~src1) & src2; SF/ZF, CF=OF=0
        res = (~v2) & rm;
        cf = 0;
        setfl = 1;
    } else if (map == 2 && op == 0xf3 && pp == 0) { // BMI1 BLS group (ModRM.reg = opcode ext; dest in vvvv)
        int grp = I->reg & 7;
        dest = vv;
        if (grp == 1) { // BLSR vvvv, rm: (rm-1) & rm; CF=(rm==0)
            res = (rm - 1) & rm;
            cf = (rm == 0);
        } else if (grp == 2) { // BLSMSK vvvv, rm: (rm-1) ^ rm; CF=(rm==0)
            res = (rm - 1) ^ rm;
            cf = (rm == 0);
        } else if (grp == 3) { // BLSI vvvv, rm: (-rm) & rm; CF=(rm!=0)
            res = (0 - rm) & rm;
            cf = (rm != 0);
        } else {
            return AVX_DISPATCH_UNIMPLEMENTED;
        }
        setfl = 1;
    } else {
        return AVX_DISPATCH_UNIMPLEMENTED;
    }
    c->r[dest] = res & M; // 32-bit dest zero-extends to 64
    if (setfl) {          // BZHI/BEXTR/ANDN/BLS* set ZF/SF, CF as computed, OF=0
        zf = ((res & M) == 0);
        sf = (int)((res >> (wb - 1)) & 1);
        c->nzcv = ((uint64_t)sf << 31) | ((uint64_t)zf << 30) | ((uint64_t)(!cf) << 29);
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_fma(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                 uint64_t next, int map, int op, int rd, int vv, int width) {
    int alternating = op == 0x96 || op == 0x97 || op == 0xA6 || op == 0xA7 || op == 0xB6 || op == 0xB7;
    int arithmetic = (op >= 0x98 && op <= 0x9F) || (op >= 0xA8 && op <= 0xAF) || (op >= 0xB8 && op <= 0xBF);
    if (map != 2 || (!alternating && !arithmetic)) return AVX_DISPATCH_UNMATCHED;

    int form = (op >> 4) - 9; // 0=132, 1=213, 2=231
    int dbl = I->vex_w;
    int element_size = dbl ? 8 : 4;
    uint8_t destination[64], vvvv[64], operand[64], output[64];
    avx_get(c, rd, destination);
    avx_get(c, vv, vvvv);
    avx_get_rm(state, c, I, next, width, operand);
    uint8_t *multiplier1 = (form == 0) ? destination : vvvv;
    uint8_t *multiplier2 = (form == 0) ? operand : (form == 1) ? destination : operand;
    uint8_t *addend = (form == 0) ? vvvv : (form == 1) ? operand : destination;
    int scalar = arithmetic && (op & 1);
    int count = scalar ? element_size : width;
    memcpy(output, destination, sizeof(output));
    for (int offset = 0; offset < count; offset += element_size) {
        int negate_product = 0;
        int negate_addend;
        if (alternating) {
            int subtract_on_odd = op & 1;
            int even = ((offset / element_size) & 1) == 0;
            negate_addend = subtract_on_odd ? !even : even;
        } else {
            int base = op & 0x0E; // 8=madd,A=msub,C=nmadd,E=nmsub
            negate_product = base == 0x0C || base == 0x0E;
            negate_addend = base == 0x0A || base == 0x0E;
        }
        if (dbl) {
            double x, y, z;
            memcpy(&x, multiplier1 + offset, 8);
            memcpy(&y, multiplier2 + offset, 8);
            memcpy(&z, addend + offset, 8);
            double result = fma_x86_f64(x, y, z, negate_product, negate_addend);
            memcpy(output + offset, &result, 8);
        } else {
            float x, y, z;
            memcpy(&x, multiplier1 + offset, 4);
            memcpy(&y, multiplier2 + offset, 4);
            memcpy(&z, addend + offset, 4);
            float result = fma_x86_f32(x, y, z, negate_product, negate_addend);
            memcpy(output + offset, &result, 4);
        }
    }
    avx_put(c, rd, output, scalar ? 16 : width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

// Keep the AES and carryless-multiply families together: they share the AES-NI
// opcode maps and are easier to audit here than among the general packed lanes.
static enum avx_dispatch_result avx_dispatch_crypto(const hl_x86_avx_state *state, struct cpu *c,
                                                    struct insn *instruction, uint64_t next, int map, int op,
                                                    int destination, int source, int width) {
    uint8_t left[64], right[64], output[64];
    if (map == 2 && op == 0xDB) { // vaesimc xmm, xmm/m128
        avx_get_rm(state, c, instruction, next, 16, right);
        memcpy(output, right, 16);
        aes_mixcolumns(output, 1);
        avx_put(c, destination, output, 16);
    } else if (map == 2 && op >= 0xDC && op <= 0xDF) { // vaesenc/last, vaesdec/last
        avx_get(c, source, left);
        avx_get_rm(state, c, instruction, next, 16, right);
        uint8_t transformed[16];
        int decrypt = op == 0xDE || op == 0xDF;
        aes_shiftrows(left, transformed, decrypt);
        aes_subbytes(transformed, decrypt ? k_aes_isbox : k_aes_sbox);
        if (op == 0xDC) aes_mixcolumns(transformed, 0);
        if (op == 0xDE) aes_mixcolumns(transformed, 1);
        for (int index = 0; index < 16; index++)
            output[index] = transformed[index] ^ right[index];
        avx_put(c, destination, output, 16);
    } else if (map == 3 && op == 0x44) { // vpclmulqdq
        avx_get(c, source, left);
        avx_get_rm(state, c, instruction, next, width, right);
        for (int lane = 0; lane < width; lane += 16) {
            uint64_t lhs, rhs;
            memcpy(&lhs, left + lane + 8 * (instruction->imm & 1), 8);
            memcpy(&rhs, right + lane + 8 * ((instruction->imm >> 4) & 1), 8);
// __int128: pre-C23 GNU/clang extension needed for the PCLMULQDQ carryless product; scope -Wpedantic.
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"
            unsigned __int128 product = 0;
            for (int bit = 0; bit < 64; bit++)
                if ((rhs >> bit) & 1) product ^= (unsigned __int128)lhs << bit;
#pragma GCC diagnostic pop
            memcpy(output + lane, &product, 16);
        }
        avx_put(c, destination, output, width);
    } else if (map == 3 && op == 0xDF) { // vaeskeygenassist
        avx_get_rm(state, c, instruction, next, 16, right);
        uint32_t words[4], result[4];
        memcpy(words, right, 16);
        uint32_t rcon = (uint32_t)(instruction->imm & 0xff);
        for (int index = 1; index <= 3; index += 2) {
            uint32_t word = words[index];
            uint32_t substituted = (uint32_t)k_aes_sbox[word & 0xff] | ((uint32_t)k_aes_sbox[(word >> 8) & 0xff] << 8) |
                                   ((uint32_t)k_aes_sbox[(word >> 16) & 0xff] << 16) |
                                   ((uint32_t)k_aes_sbox[(word >> 24) & 0xff] << 24);
            uint32_t rotated = (substituted >> 8) | (substituted << 24);
            result[index - 1] = substituted;
            result[index] = rotated ^ rcon;
        }
        memcpy(output, result, 16);
        avx_put(c, destination, output, 16);
    } else {
        return AVX_DISPATCH_UNMATCHED;
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map2_memory(const hl_x86_avx_state *state, struct cpu *c,
                                                         struct insn *instruction, uint64_t next, int map, int op,
                                                         int destination, int mask_register, int width) {
    if (map != 2) return AVX_DISPATCH_UNMATCHED;

    uint8_t mask[64], source[64], output[64];
    int element_size;
    if (op == 0x78 || op == 0x79 || op == 0x58 || op == 0x59) { // vpbroadcastb/w/d/q
        element_size = op == 0x78 ? 1 : op == 0x79 ? 2 : op == 0x58 ? 4 : 8;
        avx_get_rm(state, c, instruction, next, element_size, source);
        for (int offset = 0; offset < width; offset += element_size)
            memcpy(output + offset, source, (size_t)element_size);
        avx_put(c, destination, output, width);
    } else if (op == 0x18 || op == 0x19) { // vbroadcastss/sd
        element_size = op == 0x18 ? 4 : 8;
        avx_get_rm(state, c, instruction, next, element_size, source);
        for (int offset = 0; offset < width; offset += element_size)
            memcpy(output + offset, source, (size_t)element_size);
        avx_put(c, destination, output, width);
    } else if (op == 0x1A || op == 0x5A) { // vbroadcastf128/i128
        avx_get_rm(state, c, instruction, next, 16, source);
        memcpy(output, source, 16);
        memcpy(output + 16, source, 16);
        avx_put(c, destination, output, 32);
    } else if (op == 0x2C || op == 0x2D || op == 0x8C) { // vmaskmovps/pd, vpmaskmovd/q loads
        element_size = op == 0x2C ? 4 : op == 0x2D ? 8 : instruction->vex_w ? 8 : 4;
        avx_get(c, mask_register, mask);
        memset(output, 0, sizeof(output));
        uint64_t address = avx_ea(state, c, instruction, next, width);
        for (int offset = 0; offset < width; offset += element_size)
            if (mask[offset + element_size - 1] & 0x80)
                (void)avx_memory_read(state, address + (uint64_t)offset, output + offset, (size_t)element_size);
        avx_put(c, destination, output, width);
    } else if (op == 0x2E || op == 0x2F || op == 0x8E) { // vmaskmovps/pd, vpmaskmovd/q stores
        element_size = op == 0x2E ? 4 : op == 0x2F ? 8 : instruction->vex_w ? 8 : 4;
        avx_get(c, mask_register, mask);
        avx_get(c, destination, source);
        uint64_t address = avx_ea(state, c, instruction, next, width);
        for (int offset = 0; offset < width; offset += element_size)
            if (mask[offset + element_size - 1] & 0x80)
                (void)avx_memory_write(state, address + (uint64_t)offset, source + offset, (size_t)element_size);
    } else {
        return AVX_DISPATCH_UNMATCHED;
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map2_widen(const hl_x86_avx_state *state, struct cpu *c,
                                                        struct insn *instruction, uint64_t next, int map, int op,
                                                        int destination, int width) {
    int sign_extend = op >= 0x20 && op <= 0x25;
    int zero_extend = op >= 0x30 && op <= 0x35;
    if (map != 2 || (!sign_extend && !zero_extend)) return AVX_DISPATCH_UNMATCHED;

    static const int k_source_sizes[6] = {1, 1, 1, 2, 2, 4};
    static const int k_destination_sizes[6] = {2, 4, 8, 4, 8, 8};
    int index = op - (sign_extend ? 0x20 : 0x30);
    int source_size = k_source_sizes[index];
    int destination_size = k_destination_sizes[index];
    int count = width / destination_size;
    uint8_t source[64], output[64];
    avx_get_rm(state, c, instruction, next, count * source_size, source);
    for (int element = 0; element < count; element++) {
        uint64_t value = 0;
        memcpy(&value, source + element * source_size, (size_t)source_size);
        if (sign_extend) {
            uint64_t sign = UINT64_C(1) << (source_size * 8 - 1);
            if (value & sign) value |= ~(sign * 2 - 1);
        }
        memcpy(output + element * destination_size, &value, (size_t)destination_size);
    }
    avx_put(c, destination, output, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map2_variable_permutation(const hl_x86_avx_state *state, struct cpu *c,
                                                                      struct insn *instruction, uint64_t next,
                                                                      int map, int op, int destination,
                                                                      int data_register, int width) {
    if (map != 2 || (op != 0x0C && op != 0x0D && op != 0x16 && op != 0x36))
        return AVX_DISPATCH_UNMATCHED;

    uint8_t data[64], control[64], output[64];
    avx_get(c, data_register, data);
    avx_get_rm(state, c, instruction, next, width, control);
    if (op == 0x16 || op == 0x36) { // vpermps/vpermd, full-width dword selection
        for (int offset = 0; offset < width; offset += 4) {
            uint32_t index;
            memcpy(&index, data + offset, 4);
            memcpy(output + offset, control + 4 * (index & 7), 4);
        }
    } else if (op == 0x0C) { // vpermilps, per-128-lane dword selection
        for (int lane = 0; lane < width; lane += 16)
            for (int element = 0; element < 4; element++) {
                uint32_t index;
                memcpy(&index, control + lane + 4 * element, 4);
                memcpy(output + lane + 4 * element, data + lane + 4 * (index & 3), 4);
            }
    } else { // vpermilpd, per-128-lane qword selection
        for (int lane = 0; lane < width; lane += 16)
            for (int element = 0; element < 2; element++) {
                uint64_t index;
                memcpy(&index, control + lane + 8 * element, 8);
                memcpy(output + lane + 8 * element, data + lane + 8 * ((index >> 1) & 1), 8);
            }
    }
    avx_put(c, destination, output, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map2_variable_shift(const hl_x86_avx_state *state, struct cpu *c,
                                                                struct insn *instruction, uint64_t next, int map,
                                                                int op, int destination, int value_register,
                                                                int width) {
    if (map != 2 || (op != 0x45 && op != 0x46 && op != 0x47)) return AVX_DISPATCH_UNMATCHED;

    int element_size = instruction->vex_w ? 8 : 4;
    uint8_t values[64], counts[64], output[64];
    avx_get(c, value_register, values);
    avx_get_rm(state, c, instruction, next, width, counts);
    for (int offset = 0; offset < width; offset += element_size) {
        uint64_t value = 0, count = 0, result;
        memcpy(&value, values + offset, (size_t)element_size);
        memcpy(&count, counts + offset, (size_t)element_size);
        int bits = element_size * 8;
        if (op == 0x46) { // vpsravd: arithmetic dword right shift
            int32_t signed_value;
            memcpy(&signed_value, values + offset, 4);
            result = (uint32_t)(count >= 32 ? signed_value >> 31 : signed_value >> count);
        } else if (op == 0x45) { // vpsrlvd/q
            result = count >= (uint64_t)bits ? 0 : value >> count;
        } else { // vpsllvd/q
            result = count >= (uint64_t)bits ? 0 : value << count;
        }
        memcpy(output + offset, &result, (size_t)element_size);
    }
    avx_put(c, destination, output, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map2_test(const hl_x86_avx_state *state, struct cpu *c,
                                                       struct insn *instruction, uint64_t next, int map, int op,
                                                       int value_register, int width) {
    if (map != 2 || (op != 0x0E && op != 0x0F && op != 0x17)) return AVX_DISPATCH_UNMATCHED;

    uint8_t values[64], masks[64];
    avx_get(c, value_register, values);
    avx_get_rm(state, c, instruction, next, width, masks);
    uint64_t zero_accumulator = 0, carry_accumulator = 0;
    if (op == 0x17) { // vptest: inspect every bit
        for (int offset = 0; offset < width; offset += 8) {
            uint64_t value, mask;
            memcpy(&value, values + offset, 8);
            memcpy(&mask, masks + offset, 8);
            zero_accumulator |= value & mask;
            carry_accumulator |= mask & ~value;
        }
    } else { // vtestps/vtestpd: inspect each element's sign bit
        int element_size = op == 0x0E ? 4 : 8;
        uint64_t sign_mask = UINT64_C(1) << (element_size * 8 - 1);
        for (int offset = 0; offset < width; offset += element_size) {
            uint64_t value = 0, mask = 0;
            memcpy(&value, values + offset, (size_t)element_size);
            memcpy(&mask, masks + offset, (size_t)element_size);
            zero_accumulator |= value & mask & sign_mask;
            carry_accumulator |= mask & ~value & sign_mask;
        }
    }
    int zero = zero_accumulator == 0;
    int carry = carry_accumulator == 0;
    c->nzcv = ((uint64_t)zero << 30) | ((uint64_t)(!carry) << 29);
    c->pf = 1;
    c->af = 0;
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_fp16_conversion(const hl_x86_avx_state *state, struct cpu *c,
                                                             struct insn *instruction, uint64_t next, int map,
                                                             int op, int destination, int width) {
    uint8_t input[64], output[64];
    int count = width / 4;
    if (map == 2 && op == 0x13) { // vcvtph2ps
        avx_get_rm(state, c, instruction, next, width / 2, input);
        for (int element = 0; element < count; element++) {
            uint16_t bits;
            memcpy(&bits, input + 2 * element, 2);
            float value = avx_f16_to_f32(bits);
            memcpy(output + 4 * element, &value, 4);
        }
        avx_put(c, destination, output, width);
    } else if (map == 3 && op == 0x1D) { // vcvtps2ph
        avx_get(c, destination, input);
        for (int element = 0; element < count; element++) {
            float value;
            memcpy(&value, input + 4 * element, 4);
            uint16_t bits = avx_f32_to_f16(value, (int)instruction->imm);
            memcpy(output + 2 * element, &bits, 2);
        }
        avx_put_rm(state, c, instruction, next, width / 2, output);
    } else {
        return AVX_DISPATCH_UNMATCHED;
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_immediate_blend(const hl_x86_avx_state *state, struct cpu *c,
                                                             struct insn *instruction, uint64_t next, int map, int op,
                                                             int destination, int first_register, int width) {
    if (map != 3 || (op != 0x0C && op != 0x0D)) return AVX_DISPATCH_UNMATCHED;

    int element_size = op == 0x0C ? 4 : 8;
    uint8_t first[64], second[64], output[64];
    avx_get(c, first_register, first);
    avx_get_rm(state, c, instruction, next, width, second);
    for (int element = 0; element < width / element_size; element++) {
        const uint8_t *source = (instruction->imm & (1 << element)) ? second : first;
        memcpy(output + element * element_size, source + element * element_size, (size_t)element_size);
    }
    avx_put(c, destination, output, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_immediate_permutation(const hl_x86_avx_state *state, struct cpu *c,
                                                                   struct insn *instruction, uint64_t next, int map,
                                                                   int op, int destination, int width) {
    if (map != 3 || (op != 0x04 && op != 0x05)) return AVX_DISPATCH_UNMATCHED;

    uint8_t input[64], output[64];
    avx_get_rm(state, c, instruction, next, width, input);
    if (op == 0x04) { // vpermilps: two immediate bits per output dword
        for (int lane = 0; lane < width; lane += 16)
            for (int element = 0; element < 4; element++) {
                int index = (instruction->imm >> (2 * element)) & 3;
                memcpy(output + lane + 4 * element, input + lane + 4 * index, 4);
            }
    } else { // vpermilpd: consecutive immediate bits select qwords
        int immediate_bit = 0;
        for (int lane = 0; lane < width; lane += 16)
            for (int element = 0; element < 2; element++, immediate_bit++) {
                int index = (instruction->imm >> immediate_bit) & 1;
                memcpy(output + lane + 8 * element, input + lane + 8 * index, 8);
            }
    }
    avx_put(c, destination, output, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map2_qword_comparison(const hl_x86_avx_state *state, struct cpu *c,
                                                                  struct insn *instruction, uint64_t next, int map,
                                                                  int op, int destination, int first_register,
                                                                  int width) {
    if (map != 2 || (op != 0x29 && op != 0x37)) return AVX_DISPATCH_UNMATCHED;

    uint8_t first[64], second[64], output[64];
    avx_get(c, first_register, first);
    avx_get_rm(state, c, instruction, next, width, second);
    for (int offset = 0; offset < width; offset += 8) {
        int64_t left, right;
        memcpy(&left, first + offset, 8);
        memcpy(&right, second + offset, 8);
        uint64_t result = op == 0x29 ? (left == right ? UINT64_MAX : 0) : (left > right ? UINT64_MAX : 0);
        memcpy(output + offset, &result, 8);
    }
    avx_put(c, destination, output, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_saturating_pack(const hl_x86_avx_state *state, struct cpu *c,
                                                             struct insn *instruction, uint64_t next, int map,
                                                             int op, int destination, int first_register,
                                                             int width) {
    int unsigned_output = (map == 1 && op == 0x67) || (map == 2 && op == 0x2B);
    int source_size = (map == 1 && (op == 0x63 || op == 0x67)) ? 2 : 4;
    if (!((map == 1 && (op == 0x63 || op == 0x67 || op == 0x6B)) || (map == 2 && op == 0x2B)))
        return AVX_DISPATCH_UNMATCHED;

    int destination_size = source_size / 2;
    int elements_per_source = 16 / source_size;
    int64_t minimum = unsigned_output ? 0 : destination_size == 1 ? INT8_MIN : INT16_MIN;
    int64_t maximum = unsigned_output ? (destination_size == 1 ? UINT8_MAX : UINT16_MAX)
                                      : (destination_size == 1 ? INT8_MAX : INT16_MAX);
    uint8_t first[64], second[64], output[64];
    avx_get(c, first_register, first);
    avx_get_rm(state, c, instruction, next, width, second);
    for (int lane = 0; lane < width; lane += 16)
        for (int element = 0; element < elements_per_source; element++) {
            int64_t values[2] = {0, 0};
            memcpy(&values[0], first + lane + element * source_size, (size_t)source_size);
            memcpy(&values[1], second + lane + element * source_size, (size_t)source_size);
            for (int source = 0; source < 2; source++) {
                uint64_t sign = UINT64_C(1) << (source_size * 8 - 1);
                uint64_t value = (uint64_t)values[source];
                if (value & sign) value |= ~(sign * 2 - 1);
                int64_t signed_value = (int64_t)value;
                int64_t clamped = signed_value < minimum ? minimum : signed_value > maximum ? maximum : signed_value;
                int output_element = source * elements_per_source + element;
                memcpy(output + lane + output_element * destination_size, &clamped, (size_t)destination_size);
            }
        }
    avx_put(c, destination, output, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_packed_low_multiply(const hl_x86_avx_state *state, struct cpu *c,
                                                                struct insn *instruction, uint64_t next, int map,
                                                                int op, int destination, int first_register,
                                                                int width) {
    if (!((map == 1 && op == 0xF4) || (map == 2 && op == 0x40))) return AVX_DISPATCH_UNMATCHED;

    uint8_t first[64], second[64], output[64];
    avx_get(c, first_register, first);
    avx_get_rm(state, c, instruction, next, width, second);
    if (map == 1) { // vpmuludq: multiply the low unsigned dword in each qword lane
        for (int offset = 0; offset < width; offset += 8) {
            uint32_t left, right;
            memcpy(&left, first + offset, 4);
            memcpy(&right, second + offset, 4);
            uint64_t result = (uint64_t)left * right;
            memcpy(output + offset, &result, 8);
        }
    } else { // vpmulld: retain the low dword of every product
        for (int offset = 0; offset < width; offset += 4) {
            uint32_t left, right;
            memcpy(&left, first + offset, 4);
            memcpy(&right, second + offset, 4);
            uint32_t result = left * right;
            memcpy(output + offset, &result, 4);
        }
    }
    avx_put(c, destination, output, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map1_sign_mask(const hl_x86_avx_state *state, struct cpu *c,
                                                           struct insn *instruction, uint64_t next, int map, int op,
                                                           int prefix, int destination, int width) {
    if (map != 1 || (op != 0x50 && op != 0xD7)) return AVX_DISPATCH_UNMATCHED;

    uint8_t input[64];
    int element_size;
    if (op == 0x50) { // vmovmskps/pd is register-only
        avx_get(c, instruction->rm_reg, input);
        element_size = prefix == 1 ? 8 : 4;
    } else { // vpmovmskb
        avx_get_rm(state, c, instruction, next, width, input);
        element_size = 1;
    }
    uint64_t mask = 0;
    for (int element = 0; element < width / element_size; element++)
        if (input[(element + 1) * element_size - 1] & 0x80) mask |= UINT64_C(1) << element;
    c->r[destination] = mask;
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map1_unpack(const hl_x86_avx_state *state, struct cpu *c,
                                                        struct insn *instruction, uint64_t next, int map, int op,
                                                        int prefix, int destination, int first_register, int width) {
    int floating = op == 0x14 || op == 0x15;
    int integer =
        op == 0x60 || op == 0x61 || op == 0x62 || op == 0x6C || op == 0x68 || op == 0x69 || op == 0x6A || op == 0x6D;
    if (map != 1 || (!floating && !integer)) return AVX_DISPATCH_UNMATCHED;

    int element_size;
    int high;
    if (floating) {
        element_size = prefix == 1 ? 8 : 4;
        high = op == 0x15;
    } else {
        element_size = (op == 0x60 || op == 0x68)   ? 1
                       : (op == 0x61 || op == 0x69) ? 2
                       : (op == 0x62 || op == 0x6A) ? 4
                                                    : 8;
        high = op == 0x68 || op == 0x69 || op == 0x6A || op == 0x6D;
    }
    uint8_t first[64], second[64], output[64];
    avx_get(c, first_register, first);
    avx_get_rm(state, c, instruction, next, width, second);
    int source_base = high ? 8 : 0;
    int elements = 8 / element_size;
    for (int lane = 0; lane < width; lane += 16)
        for (int element = 0; element < elements; element++) {
            int source_offset = lane + source_base + element * element_size;
            int output_offset = lane + 2 * element * element_size;
            memcpy(output + output_offset, first + source_offset, (size_t)element_size);
            memcpy(output + output_offset + element_size, second + source_offset, (size_t)element_size);
        }
    avx_put(c, destination, output, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map1_bitwise(const hl_x86_avx_state *state, struct cpu *c,
                                                         struct insn *instruction, uint64_t next, int map, int op,
                                                         int destination, int first_register, int width) {
    int exclusive_or = op == 0xEF || op == 0x57;
    int inclusive_or = op == 0xEB || op == 0x56;
    int and = op == 0xDB || op == 0x54;
    int and_not = op == 0xDF || op == 0x55;
    if (map != 1 || (!exclusive_or && !inclusive_or && !and && !and_not)) return AVX_DISPATCH_UNMATCHED;

    uint8_t first[64], second[64], output[64];
    avx_get(c, first_register, first);
    avx_get_rm(state, c, instruction, next, width, second);
    for (int offset = 0; offset < width; offset++) {
        if (exclusive_or)
            output[offset] = first[offset] ^ second[offset];
        else if (inclusive_or)
            output[offset] = first[offset] | second[offset];
        else if (and)
            output[offset] = first[offset] & second[offset];
        else
            output[offset] = (uint8_t)(~first[offset] & second[offset]);
    }
    avx_put(c, destination, output, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map1_duplicate(const hl_x86_avx_state *state, struct cpu *c,
                                                            struct insn *instruction, uint64_t next, int map, int op,
                                                            int prefix, int destination, int width) {
    int double_duplicate = op == 0x12 && prefix == 3;
    int low_single_duplicate = op == 0x12 && prefix == 2;
    int high_single_duplicate = op == 0x16 && prefix == 2;
    if (map != 1 || (!double_duplicate && !low_single_duplicate && !high_single_duplicate))
        return AVX_DISPATCH_UNMATCHED;

    uint8_t input[64], output[64];
    if (double_duplicate && instruction->is_mem) {
        int read_size = width == 16 ? 8 : width;
        uint64_t address = avx_ea(state, c, instruction, next, read_size);
        (void)avx_memory_read(state, address, input, (size_t)read_size);
    } else {
        avx_get_rm(state, c, instruction, next, width, input);
    }
    for (int lane = 0; lane < width; lane += 16) {
        if (double_duplicate) {
            int source = instruction->is_mem && width == 16 ? 0 : lane;
            memcpy(output + lane, input + source, 8);
            memcpy(output + lane + 8, input + source, 8);
        } else {
            int source = lane + (high_single_duplicate ? 4 : 0);
            memcpy(output + lane, input + source, 4);
            memcpy(output + lane + 4, input + source, 4);
            memcpy(output + lane + 8, input + source + 8, 4);
            memcpy(output + lane + 12, input + source + 8, 4);
        }
    }
    avx_put(c, destination, output, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_scalar_integer_conversion(const hl_x86_avx_state *state, struct cpu *c,
                                                                       struct insn *instruction, uint64_t next,
                                                                       int map, int op, int prefix, int destination,
                                                                       int merge_register) {
    if (map != 1 || (op != 0x2A && op != 0x2C && op != 0x2D)) return AVX_DISPATCH_UNMATCHED;

    int double_precision = prefix == 3;
    int integer_size = instruction->vex_w ? 8 : 4;
    if (op == 0x2A) { // vcvtsi2ss/sd
        int64_t integer;
        if (instruction->is_mem) {
            integer = 0;
            uint64_t address = avx_ea(state, c, instruction, next, integer_size);
            (void)avx_memory_read(state, address, &integer, (size_t)integer_size);
            if (!instruction->vex_w) integer = (int32_t)integer;
        } else {
            integer =
                instruction->vex_w ? (int64_t)c->r[instruction->rm_reg] : (int64_t)(int32_t)c->r[instruction->rm_reg];
        }
        uint8_t output[64];
        avx_get(c, merge_register, output);
        if (double_precision) {
            double converted = (double)integer;
            memcpy(output, &converted, 8);
        } else {
            float converted = (float)integer;
            memcpy(output, &converted, 4);
        }
        avx_put(c, destination, output, 16);
    } else { // vcvttss/sd2si or vcvtss/sd2si
        int element_size = double_precision ? 8 : 4;
        uint8_t input[64];
        avx_get_rm(state, c, instruction, next, element_size, input);
        int64_t converted;
        if (double_precision) {
            double value;
            memcpy(&value, input, 8);
            converted = cvt_x86_d2i(value, op == 0x2C, instruction->vex_w);
        } else {
            float value;
            memcpy(&value, input, 4);
            converted = cvt_x86_f2i(value, op == 0x2C, instruction->vex_w);
        }
        c->r[destination] = instruction->vex_w ? (uint64_t)converted : (uint32_t)converted;
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_precision_conversion(const hl_x86_avx_state *state, struct cpu *c,
                                                                 struct insn *instruction, uint64_t next, int map,
                                                                 int op, int prefix, int destination,
                                                                 int merge_register, int width) {
    if (map != 1 || op != 0x5A) return AVX_DISPATCH_UNMATCHED;

    uint8_t input[64], output[64];
    if (prefix == 2 || prefix == 3) { // scalar ss->sd or sd->ss
        int input_size = prefix == 2 ? 4 : 8;
        avx_get(c, merge_register, output);
        avx_get_rm(state, c, instruction, next, input_size, input);
        if (prefix == 2) {
            float value;
            memcpy(&value, input, 4);
            double converted = (double)value;
            memcpy(output, &converted, 8);
        } else {
            double value;
            memcpy(&value, input, 8);
            float converted = (float)value;
            memcpy(output, &converted, 4);
        }
        avx_put(c, destination, output, 16);
    } else if (prefix == 0) { // packed ps->pd
        avx_get_rm(state, c, instruction, next, width / 2, input);
        for (int element = 0; element < width / 8; element++) {
            float value;
            memcpy(&value, input + 4 * element, 4);
            double converted = (double)value;
            memcpy(output + 8 * element, &converted, 8);
        }
        avx_put(c, destination, output, width);
    } else { // packed pd->ps
        avx_get_rm(state, c, instruction, next, width, input);
        for (int element = 0; element < width / 8; element++) {
            double value;
            memcpy(&value, input + 8 * element, 8);
            float converted = (float)value;
            memcpy(output + 4 * element, &converted, 4);
        }
        avx_put(c, destination, output, width / 2);
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_root_reciprocal(const hl_x86_avx_state *state, struct cpu *c,
                                                             struct insn *instruction, uint64_t next, int map, int op,
                                                             int prefix, int destination, int merge_register,
                                                             int width) {
    if (map != 1 || (op != 0x51 && op != 0x52 && op != 0x53)) return AVX_DISPATCH_UNMATCHED;

    int scalar = prefix == 2 || prefix == 3;
    int double_precision = op == 0x51 && (prefix == 1 || prefix == 3);
    int element_size = double_precision ? 8 : 4;
    uint8_t input[64], output[64];
    avx_get_rm(state, c, instruction, next, scalar ? element_size : width, input);
    if (scalar) avx_get(c, merge_register, output);
    unsigned parked_flags = op == 0x51 ? 0 : cvt_fp_flags();
    int limit = scalar ? element_size : width;
    for (int offset = 0; offset < limit; offset += element_size) {
        if (double_precision) {
            double value;
            memcpy(&value, input + offset, 8);
            double result = avx_dnan_f64(__builtin_sqrt(value), value, value);
            memcpy(output + offset, &result, 8);
        } else {
            float value;
            memcpy(&value, input + offset, 4);
            float result;
            if (op == 0x51)
                result = avx_dnan_f32(__builtin_sqrtf(value), value, value);
            else if (op == 0x52)
                result = avx_dnan_f32(1.0f / __builtin_sqrtf(value), value, value);
            else
                result = avx_dnan_f32(1.0f / value, value, value);
            memcpy(output + offset, &result, 4);
        }
    }
    avx_put(c, destination, output, scalar ? 16 : width);
    // RCP/RSQRT specify an accuracy bound rather than exact bits and raise no SIMD FP exceptions. The exact
    // result satisfies the bound; restore sticky flags parked around the host arithmetic.
    if (op != 0x51) cvt_fp_flags_set(parked_flags);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_lane_transfer(const hl_x86_avx_state *state, struct cpu *c,
                                                           struct insn *instruction, uint64_t next) {
    int op = instruction->op;
    int immediate = (int)instruction->imm;
    uint8_t source[64], result[64];
    if (op >= 0x14 && op <= 0x17) { // vpextrb/w/d/q and vextractps
        avx_get(c, instruction->reg, source);
        uint64_t value;
        int bytes;
        if (op == 0x14) {
            bytes = 1;
            value = source[immediate & 15];
        } else if (op == 0x15) {
            uint16_t word;
            bytes = 2;
            memcpy(&word, source + 2 * (immediate & 7), 2);
            value = word;
        } else if (op == 0x16) {
            bytes = instruction->vex_w ? 8 : 4;
            value = 0;
            memcpy(&value, source + (instruction->vex_w ? 8 * (immediate & 1) : 4 * (immediate & 3)), (size_t)bytes);
        } else {
            uint32_t word;
            bytes = 4;
            memcpy(&word, source + 4 * (immediate & 3), 4);
            value = word;
        }
        if (instruction->is_mem) {
            uint64_t address = avx_ea(state, c, instruction, next, bytes);
            (void)avx_memory_write(state, address, &value, (size_t)bytes);
        } else {
            c->r[instruction->rm_reg] = bytes == 8 ? value : (uint32_t)value;
        }
        return AVX_DISPATCH_HANDLED;
    }
    if (op >= 0x20 && op <= 0x22) { // vpinsrb, vinsertps, vpinsrd/q
        avx_get(c, instruction->vvvv, result);
        if (op == 0x20) {
            uint8_t value = (uint8_t)c->r[instruction->rm_reg];
            if (instruction->is_mem) (void)avx_memory_read(state, avx_ea(state, c, instruction, next, 1), &value, 1);
            result[immediate & 15] = value;
        } else if (op == 0x22) {
            int bytes = instruction->vex_w ? 8 : 4;
            uint64_t value = c->r[instruction->rm_reg];
            if (instruction->is_mem)
                (void)avx_memory_read(state, avx_ea(state, c, instruction, next, bytes), &value, (size_t)bytes);
            memcpy(result + bytes * (immediate & (instruction->vex_w ? 1 : 3)), &value, (size_t)bytes);
        } else {
            uint32_t value;
            if (instruction->is_mem)
                (void)avx_memory_read(state, avx_ea(state, c, instruction, next, 4), &value, 4);
            else {
                avx_get(c, instruction->rm_reg, source);
                memcpy(&value, source + 4 * ((immediate >> 6) & 3), 4);
            }
            memcpy(result + 4 * ((immediate >> 4) & 3), &value, 4);
            for (int lane = 0; lane < 4; lane++)
                if (immediate & (1 << lane)) memset(result + 4 * lane, 0, 4);
        }
        avx_put(c, instruction->reg, result, 16);
        return AVX_DISPATCH_HANDLED;
    }
    if (op == 0x18 || op == 0x38) { // vinsertf128/vinserti128
        avx_get(c, instruction->vvvv, result);
        avx_get_rm(state, c, instruction, next, 16, source);
        memcpy(result + ((immediate & 1) ? 16 : 0), source, 16);
        avx_put(c, instruction->reg, result, 32);
        return AVX_DISPATCH_HANDLED;
    }
    if (op == 0x19 || op == 0x39) { // vextractf128/vextracti128
        avx_get(c, instruction->reg, source);
        memcpy(result, source + ((immediate & 1) ? 16 : 0), 16);
        avx_put_rm(state, c, instruction, next, 16, result);
        return AVX_DISPATCH_HANDLED;
    }
    return AVX_DISPATCH_UNMATCHED;
}

static enum avx_dispatch_result avx_dispatch_lane_permutation(const hl_x86_avx_state *state, struct cpu *c,
                                                              struct insn *instruction, uint64_t next, int width) {
    int op = instruction->op;
    int immediate = (int)instruction->imm;
    uint8_t left[64], right[64], result[64];
    if (op == 0x00 || op == 0x01) { // vpermq/vpermpd
        avx_get_rm(state, c, instruction, next, width, right);
        for (int lane = 0; lane < 4; lane++) {
            int selected = (immediate >> (2 * lane)) & 3;
            memcpy(result + 8 * lane, right + 8 * selected, 8);
        }
    } else if (op == 0x02 || op == 0x0E) { // vpblendd/vpblendw
        int element = op == 0x02 ? 4 : 2;
        avx_get(c, instruction->vvvv, left);
        avx_get_rm(state, c, instruction, next, width, right);
        memcpy(result, left, (size_t)width);
        for (int offset = 0; offset < width; offset += element) {
            int lane = op == 0x0E ? (offset % 16) / element : offset / element;
            if ((immediate >> lane) & 1) memcpy(result + offset, right + offset, (size_t)element);
        }
    } else if (op >= 0x4A && op <= 0x4C) { // vblendvps/vblendvpd/vpblendvb
        uint8_t mask[64];
        int element = op == 0x4C ? 1 : op == 0x4A ? 4 : 8;
        avx_get(c, instruction->vvvv, left);
        avx_get_rm(state, c, instruction, next, width, right);
        avx_get(c, (immediate >> 4) & 0xF, mask);
        for (int offset = 0; offset < width; offset += element)
            memcpy(result + offset, (mask[offset + element - 1] & 0x80) ? right + offset : left + offset,
                   (size_t)element);
    } else if (op == 0x06 || op == 0x46) { // vperm2f128/vperm2i128
        uint8_t lanes[64];
        avx_get(c, instruction->vvvv, left);
        avx_get_rm(state, c, instruction, next, 32, right);
        memcpy(lanes, left, 32);
        memcpy(lanes + 32, right, 32);
        for (int half = 0; half < 2; half++) {
            int control = (immediate >> (half * 4)) & 0xF;
            if (control & 0x8)
                memset(result + half * 16, 0, 16);
            else
                memcpy(result + half * 16, lanes + (control & 3) * 16, 16);
        }
        width = 32;
    } else {
        return AVX_DISPATCH_UNMATCHED;
    }
    avx_put(c, instruction->reg, result, width);
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_immediate_floating(const hl_x86_avx_state *state, struct cpu *c,
                                                                struct insn *instruction, uint64_t next, int width) {
    int op = instruction->op;
    int immediate = (int)instruction->imm;
    uint8_t left[64], right[64], result[64];
    if (op == 0x08 || op == 0x09) { // vroundps/vroundpd
        avx_get_rm(state, c, instruction, next, width, right);
        if (op == 0x08) {
            float input[16], output[16];
            memcpy(input, right, (size_t)width);
            for (int lane = 0; lane < width / 4; lane++) output[lane] = sse_round_f(input[lane], immediate);
            memcpy(result, output, (size_t)width);
        } else {
            double input[8], output[8];
            memcpy(input, right, (size_t)width);
            for (int lane = 0; lane < width / 8; lane++) output[lane] = sse_round_d(input[lane], immediate);
            memcpy(result, output, (size_t)width);
        }
    } else if (op == 0x0A || op == 0x0B) { // vroundss/vroundsd
        int bytes = op == 0x0A ? 4 : 8;
        avx_get(c, instruction->vvvv, result);
        avx_get_rm(state, c, instruction, next, bytes, right);
        if (op == 0x0A) {
            float value;
            memcpy(&value, right, 4);
            value = sse_round_f(value, immediate);
            memcpy(result, &value, 4);
        } else {
            double value;
            memcpy(&value, right, 8);
            value = sse_round_d(value, immediate);
            memcpy(result, &value, 8);
        }
        width = 16;
    } else if (op == 0x40 || op == 0x41) { // vdpps/vdppd
        int element = op == 0x40 ? 4 : 8;
        avx_get(c, instruction->vvvv, left);
        avx_get_rm(state, c, instruction, next, width, right);
        for (int block = 0; block < width; block += 16) {
            if (element == 4) {
                float a[4], b[4], sum = 0;
                memcpy(a, left + block, 16);
                memcpy(b, right + block, 16);
                for (int lane = 0; lane < 4; lane++)
                    if (immediate & (0x10 << lane)) sum += a[lane] * b[lane];
                for (int lane = 0; lane < 4; lane++) {
                    float value = (immediate & (1 << lane)) ? sum : 0.0f;
                    memcpy(result + block + 4 * lane, &value, 4);
                }
            } else {
                double a[2], b[2], sum = 0;
                memcpy(a, left + block, 16);
                memcpy(b, right + block, 16);
                for (int lane = 0; lane < 2; lane++)
                    if (immediate & (0x10 << lane)) sum += a[lane] * b[lane];
                for (int lane = 0; lane < 2; lane++) {
                    double value = (immediate & (1 << lane)) ? sum : 0.0;
                    memcpy(result + block + 8 * lane, &value, 8);
                }
            }
        }
        if (op == 0x41) width = 16;
    } else {
        return AVX_DISPATCH_UNMATCHED;
    }
    avx_put(c, instruction->reg, result, width);
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map1_move(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                       uint64_t next, int op, int pp, int rd, int vv, int W) {
    uint8_t d[64];
    if (op == 0x10 || op == 0x28) {
        if (op == 0x10 && (pp == 2 || pp == 3) && !I->is_mem) {
            int es = pp == 2 ? 4 : 8;
            uint8_t source[64];
            avx_get(c, vv, d);
            avx_get(c, I->rm_reg, source);
            memcpy(d, source, (size_t)es);
            avx_put(c, rd, d, 16);
        } else {
            int width = op == 0x10 && (pp == 2 || pp == 3) ? (pp == 2 ? 4 : 8) : W;
            avx_get_rm(state, c, I, next, width, d);
            avx_put(c, rd, d, W);
        }
    } else if (op == 0x11 || op == 0x29) {
        if (op == 0x11 && (pp == 2 || pp == 3) && !I->is_mem) {
            int es = pp == 2 ? 4 : 8;
            uint8_t source[64];
            avx_get(c, vv, d);
            avx_get(c, rd, source);
            memcpy(d, source, (size_t)es);
            avx_put(c, I->rm_reg, d, 16);
        } else {
            int width = op == 0x11 && (pp == 2 || pp == 3) ? (pp == 2 ? 4 : 8) : W;
            avx_get(c, rd, d);
            avx_put_rm(state, c, I, next, width, d);
        }
    } else if (op == 0x6F || op == 0x7F) {
        if (op == 0x6F) {
            avx_get_rm(state, c, I, next, W, d);
            avx_put(c, rd, d, W);
        } else {
            avx_get(c, rd, d);
            avx_put_rm(state, c, I, next, W, d);
        }
    } else if (op == 0x6E) { // vmovd/q from GPR or memory, zero extending the vector destination
        int width = I->vex_w ? 8 : 4;
        memset(d, 0, sizeof(d));
        if (I->is_mem) {
            uint64_t address = avx_ea(state, c, I, next, width);
            (void)avx_memory_read(state, address, d, (size_t)width);
        } else {
            memcpy(d, &c->r[I->rm_reg], (size_t)width);
        }
        avx_put(c, rd, d, 16);
    } else if (op == 0x7E) { // vmovq load or vmovd/q store
        if (pp == 2) {
            avx_get_rm(state, c, I, next, 8, d);
            avx_put(c, rd, d, 16);
        } else {
            int width = I->vex_w ? 8 : 4;
            avx_get(c, rd, d);
            if (I->is_mem) {
                uint64_t address = avx_ea(state, c, I, next, width);
                (void)avx_memory_write(state, address, d, (size_t)width);
            } else {
                uint64_t value = 0;
                memcpy(&value, d, (size_t)width);
                c->r[I->rm_reg] = value;
            }
        }
    } else if (op == 0xD6) { // vmovq store
        avx_get(c, rd, d);
        avx_put_rm(state, c, I, next, 8, d);
    } else if (op == 0xF0) { // vlddqu
        avx_get_rm(state, c, I, next, W, d);
        avx_put(c, rd, d, W);
    } else if (op == 0x13 || op == 0x17) { // vmovlps/pd or vmovhps/pd store
        avx_get(c, rd, d);
        uint64_t address = avx_ea(state, c, I, next, 8);
        (void)avx_memory_write(state, address, d + (op == 0x17 ? 8 : 0), 8);
    } else {
        return AVX_DISPATCH_UNMATCHED;
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map1_floating_arithmetic(const hl_x86_avx_state *state, struct cpu *c,
                                                                      struct insn *instruction, uint64_t next,
                                                                      int width) {
    int op = instruction->op;
    if (op != 0x58 && op != 0x59 && op != 0x5C && op != 0x5D && op != 0x5E && op != 0x5F)
        return AVX_DISPATCH_UNMATCHED;
    int dbl = instruction->vex_pp == 1 || instruction->vex_pp == 3;
    int scalar = instruction->vex_pp == 2 || instruction->vex_pp == 3;
    int element = dbl ? 8 : 4;
    uint8_t left[64], right[64], result[64];
    avx_get(c, instruction->vvvv, left);
    avx_get_rm(state, c, instruction, next, scalar ? element : width, right);
    if (scalar) {
        memcpy(result, left, 16);
        if (dbl) {
            double x, y;
            memcpy(&x, left, 8);
            memcpy(&y, right, 8);
            double value = avx_fp_arith_f64(op, x, y);
            memcpy(result, &value, 8);
        } else {
            float x, y;
            memcpy(&x, left, 4);
            memcpy(&y, right, 4);
            float value = avx_fp_arith_f32(op, x, y);
            memcpy(result, &value, 4);
        }
        width = 16;
    } else {
        for (int offset = 0; offset < width; offset += element) {
            if (dbl) {
                double x, y;
                memcpy(&x, left + offset, 8);
                memcpy(&y, right + offset, 8);
                double value = avx_fp_arith_f64(op, x, y);
                memcpy(result + offset, &value, 8);
            } else {
                float x, y;
                memcpy(&x, left + offset, 4);
                memcpy(&y, right + offset, 4);
                float value = avx_fp_arith_f32(op, x, y);
                memcpy(result + offset, &value, 4);
            }
        }
    }
    avx_put(c, instruction->reg, result, width);
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map1_packed_integer_arithmetic(const hl_x86_avx_state *state,
                                                                            struct cpu *c, struct insn *instruction,
                                                                            uint64_t next, int width) {
    int op = instruction->op;
    int source = instruction->vvvv;
    uint8_t left[64], right[64], result[64];
    int supported = op == 0xFC || op == 0xFD || op == 0xFE || op == 0xD4 || (op >= 0xF8 && op <= 0xFB) || op == 0xEC ||
                    op == 0xED || op == 0xE8 || op == 0xE9 || op == 0xDC || op == 0xDD || op == 0xD8 || op == 0xD9 ||
                    op == 0xDA || op == 0xDE || op == 0xEA || op == 0xEE || op == 0xE0 || op == 0xE3 || op == 0xD5 ||
                    op == 0xE5 || op == 0xE4 || op == 0xF5 || op == 0xF6;
    if (!supported) return AVX_DISPATCH_UNMATCHED;
    avx_get(c, source, left);
    avx_get_rm(state, c, instruction, next, width, right);

    if (op == 0xFC || op == 0xFD || op == 0xFE || op == 0xD4 ||
        (op >= 0xF8 && op <= 0xFB)) { // vpaddb/w/d/q and vpsubb/w/d/q
        int element = (op == 0xFC || op == 0xF8)   ? 1
                      : (op == 0xFD || op == 0xF9) ? 2
                      : (op == 0xFE || op == 0xFA) ? 4
                                                   : 8;
        int subtract = op >= 0xF8 && op <= 0xFB;
        for (int offset = 0; offset < width; offset += element) {
            uint64_t x = 0, y = 0;
            memcpy(&x, left + offset, (size_t)element);
            memcpy(&y, right + offset, (size_t)element);
            uint64_t value = subtract ? x - y : x + y;
            memcpy(result + offset, &value, (size_t)element);
        }
    } else if (op == 0xEC || op == 0xED || op == 0xE8 || op == 0xE9 || op == 0xDC || op == 0xDD || op == 0xD8 ||
               op == 0xD9) { // signed/unsigned saturating add/sub
        int word = op == 0xED || op == 0xE9 || op == 0xDD || op == 0xD9;
        int uns = op == 0xDC || op == 0xDD || op == 0xD8 || op == 0xD9;
        int subtract = op == 0xE8 || op == 0xE9 || op == 0xD8 || op == 0xD9;
        int element = word ? 2 : 1;
        for (int offset = 0; offset < width; offset += element) {
            uint64_t x = 0, y = 0;
            memcpy(&x, left + offset, (size_t)element);
            memcpy(&y, right + offset, (size_t)element);
            int64_t value;
            if (uns) {
                int64_t candidate = subtract ? (int64_t)x - (int64_t)y : (int64_t)x + (int64_t)y;
                int64_t maximum = word ? 65535 : 255;
                value = candidate < 0 ? 0 : candidate > maximum ? maximum : candidate;
            } else {
                int shift = 64 - element * 8;
                int64_t signed_x = ((int64_t)x << shift) >> shift;
                int64_t signed_y = ((int64_t)y << shift) >> shift;
                int64_t candidate = subtract ? signed_x - signed_y : signed_x + signed_y;
                int64_t minimum = word ? -32768 : -128;
                int64_t maximum = word ? 32767 : 127;
                value = candidate < minimum ? minimum : candidate > maximum ? maximum : candidate;
            }
            memcpy(result + offset, &value, (size_t)element);
        }
    } else if (op == 0xDA || op == 0xDE || op == 0xEA || op == 0xEE) { // pminub/pmaxub/pminsw/pmaxsw
        int word = op == 0xEA || op == 0xEE;
        int maximum = op == 0xDE || op == 0xEE;
        if (word) {
            for (int offset = 0; offset < width; offset += 2) {
                int16_t x, y;
                memcpy(&x, left + offset, 2);
                memcpy(&y, right + offset, 2);
                int16_t value = maximum ? (x > y ? x : y) : (x < y ? x : y);
                memcpy(result + offset, &value, 2);
            }
        } else {
            for (int offset = 0; offset < width; offset++) {
                uint8_t x = left[offset], y = right[offset];
                result[offset] = maximum ? (x > y ? x : y) : (x < y ? x : y);
            }
        }
    } else if (op == 0xE0 || op == 0xE3) { // pavgb/pavgw
        int element = op == 0xE0 ? 1 : 2;
        for (int offset = 0; offset < width; offset += element) {
            uint64_t x = 0, y = 0;
            memcpy(&x, left + offset, (size_t)element);
            memcpy(&y, right + offset, (size_t)element);
            uint64_t value = (x + y + 1) >> 1;
            memcpy(result + offset, &value, (size_t)element);
        }
    } else if (op == 0xD5 || op == 0xE5 || op == 0xE4) { // pmullw/pmulhw/pmulhuw
        for (int offset = 0; offset < width; offset += 2) {
            uint16_t x, y;
            memcpy(&x, left + offset, 2);
            memcpy(&y, right + offset, 2);
            uint16_t value = op == 0xD5   ? (uint16_t)(x * y)
                             : op == 0xE4 ? (uint16_t)(((uint32_t)x * (uint32_t)y) >> 16)
                                          : (uint16_t)(((int32_t)(int16_t)x * (int32_t)(int16_t)y) >> 16);
            memcpy(result + offset, &value, 2);
        }
    } else if (op == 0xF5) { // vpmaddwd
        for (int offset = 0; offset < width; offset += 4) {
            int16_t x0, x1, y0, y1;
            memcpy(&x0, left + offset, 2);
            memcpy(&x1, left + offset + 2, 2);
            memcpy(&y0, right + offset, 2);
            memcpy(&y1, right + offset + 2, 2);
            int32_t value = (int32_t)x0 * (int32_t)y0 + (int32_t)x1 * (int32_t)y1;
            memcpy(result + offset, &value, 4);
        }
    } else if (op == 0xF6) { // vpsadbw
        memset(result, 0, sizeof(result));
        for (int block = 0; block < width; block += 8) {
            int sum = 0;
            for (int offset = 0; offset < 8; offset++) {
                int difference = (int)left[block + offset] - (int)right[block + offset];
                sum += difference < 0 ? -difference : difference;
            }
            uint16_t value = (uint16_t)sum;
            memcpy(result + block, &value, 2);
        }
    } else {
        return AVX_DISPATCH_UNMATCHED;
    }
    avx_put(c, instruction->reg, result, width);
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map1_horizontal_floating(const hl_x86_avx_state *state, struct cpu *c,
                                                                      struct insn *instruction, uint64_t next, int map,
                                                                      int width) {
    int op = instruction->op;
    if (map != 1 || (op != 0x7C && op != 0x7D && op != 0xD0)) return AVX_DISPATCH_UNMATCHED;
    int dbl = instruction->vex_pp == 1;
    int subtract = op == 0x7D;
    uint8_t left[64], right[64], result[64];
    avx_get(c, instruction->vvvv, left);
    avx_get_rm(state, c, instruction, next, width, right);
    for (int lane = 0; lane < width; lane += 16) {
        if (!dbl) {
            float x[4], y[4], output[4];
            memcpy(x, left + lane, 16);
            memcpy(y, right + lane, 16);
            if (op == 0xD0) {
                for (int element = 0; element < 4; element++)
                    output[element] = (element & 1) ? avx_dnan_f32(x[element] + y[element], x[element], y[element])
                                                    : avx_dnan_f32(x[element] - y[element], x[element], y[element]);
            } else {
                output[0] = subtract ? avx_dnan_f32(x[0] - x[1], x[0], x[1]) : avx_dnan_f32(x[0] + x[1], x[0], x[1]);
                output[1] = subtract ? avx_dnan_f32(x[2] - x[3], x[2], x[3]) : avx_dnan_f32(x[2] + x[3], x[2], x[3]);
                output[2] = subtract ? avx_dnan_f32(y[0] - y[1], y[0], y[1]) : avx_dnan_f32(y[0] + y[1], y[0], y[1]);
                output[3] = subtract ? avx_dnan_f32(y[2] - y[3], y[2], y[3]) : avx_dnan_f32(y[2] + y[3], y[2], y[3]);
            }
            memcpy(result + lane, output, 16);
        } else {
            double x[2], y[2], output[2];
            memcpy(x, left + lane, 16);
            memcpy(y, right + lane, 16);
            if (op == 0xD0) {
                output[0] = avx_dnan_f64(x[0] - y[0], x[0], y[0]);
                output[1] = avx_dnan_f64(x[1] + y[1], x[1], y[1]);
            } else {
                output[0] = subtract ? avx_dnan_f64(x[0] - x[1], x[0], x[1]) : avx_dnan_f64(x[0] + x[1], x[0], x[1]);
                output[1] = subtract ? avx_dnan_f64(y[0] - y[1], y[0], y[1]) : avx_dnan_f64(y[0] + y[1], y[0], y[1]);
            }
            memcpy(result + lane, output, 16);
        }
    }
    avx_put(c, instruction->reg, result, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

// Arm the abandon pad, then emulate. A rejected guest access longjmps back here with *c already carrying
// R_SOFTMISS (or R_TRAP for #UD) and cpu->rip left on the instruction.
void hl_x86_avx_run(const hl_x86_avx_state *state, struct cpu *c) {
    g_avx_cpu = c;
    g_avx_pc = c->rip;
    if (setjmp(g_avx_pad) != 0) return;
    do_avx(state, c);
}

void hl_x86_sse_run(const hl_x86_avx_state *state, struct cpu *c) {
    g_avx_cpu = c;
    g_avx_pc = c->rip;
    if (setjmp(g_avx_pad) != 0) return;
    do_sse3b(state, c);
}

static void do_avx(const hl_x86_avx_state *state, struct cpu *c) {
    struct insn I;
    hl_x86_decode(c->rip, &I);
    uint64_t next = c->rip + (uint64_t)I.len;
    int L = I.vex_l;                            // 0=128,1=256,2=512
    int W = (L == 0) ? 16 : (L == 1) ? 32 : 64; // operation width in bytes
    int map = I.vex_map, op = I.op, pp = I.vex_pp;
    int rd = I.reg, vv = I.vvvv;
    xs_note(1, map, op, c->rip); // EXITSTAT diagnostic (no-op unless env set)
    uint8_t a[64], b[64], d[64];

    enum avx_dispatch_result bmi = avx_dispatch_bmi(state, c, &I, next, map, op, pp, rd, vv);
    if (bmi == AVX_DISPATCH_HANDLED) return;
    if (bmi == AVX_DISPATCH_UNIMPLEMENTED) goto avx_unimpl;
    if (avx_dispatch_fma(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_crypto(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_memory(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_widen(state, c, &I, next, map, op, rd, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_variable_permutation(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED)
        return;
    if (avx_dispatch_map2_variable_shift(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_test(state, c, &I, next, map, op, rd, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_fp16_conversion(state, c, &I, next, map, op, rd, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_immediate_blend(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_immediate_permutation(state, c, &I, next, map, op, rd, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_qword_comparison(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED)
        return;
    if (avx_dispatch_saturating_pack(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_packed_low_multiply(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map1_sign_mask(state, c, &I, next, map, op, pp, rd, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map1_unpack(state, c, &I, next, map, op, pp, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map1_bitwise(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map1_horizontal_floating(state, c, &I, next, map, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map1_duplicate(state, c, &I, next, map, op, pp, rd, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_scalar_integer_conversion(state, c, &I, next, map, op, pp, rd, vv) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_precision_conversion(state, c, &I, next, map, op, pp, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_root_reciprocal(state, c, &I, next, map, op, pp, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (map == 1 && avx_dispatch_map1_move(state, c, &I, next, op, pp, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (map == 1 && avx_dispatch_map1_floating_arithmetic(state, c, &I, next, W) == AVX_DISPATCH_HANDLED) goto done;
    if (map == 1 && avx_dispatch_map1_packed_integer_arithmetic(state, c, &I, next, W) == AVX_DISPATCH_HANDLED)
        goto done;

    // ---- map 1 (0F) ----
    if (map == 1) {
        switch (op) {
        case 0xC5: { // vpextrw: gpr(reg) <- zero-extended word lane imm[2:0] of xmm rm (reg-only src)
            avx_get(c, I.rm_reg, a);
            uint16_t w;
            memcpy(&w, a + 2 * (I.imm & 7), 2);
            c->r[I.reg] = w; // zero-extends into the 64-bit GPR
            goto done;
        }
        case 0xC4: { // vpinsrw: dst = src1(vvvv); word lane imm[2:0] <- low 16 of r/m16 (gpr or mem)
            avx_get(c, vv, d);
            uint16_t w;
            if (I.is_mem) {
                uint64_t addr = avx_ea(state, c, &I, next, 2);
                (void)avx_memory_read(state, addr, &w, 2);
            } else
                w = (uint16_t)c->r[I.rm_reg];
            memcpy(d + 2 * (I.imm & 7), &w, 2);
            avx_put(c, rd, d, 16); // zero bits [128:VLMAX)
            goto done;
        }
        case 0xC6: { // vshufps (pp=0) / vshufpd (pp=1) imm8; src1=vvvv, src2=rm, per-128-lane
            avx_get(c, vv, a);
            avx_get_rm(state, c, &I, next, W, b);
            int imm = (int)I.imm;
            if (pp == 1) { // shufpd: one imm bit per output qword; even qword from src1, odd from src2
                for (int q = 0; q < W / 8; q++) {
                    const uint8_t *srcp = (q & 1) ? b : a;
                    int lane = (q / 2) * 16;
                    memcpy(d + q * 8, srcp + lane + (((imm >> q) & 1) ? 8 : 0), 8);
                }
            } else { // shufps: low two dwords from src1, high two from src2 (same imm per 128-lane)
                for (int lane = 0; lane < W; lane += 16) {
                    memcpy(d + lane + 0, a + lane + 4 * ((imm >> 0) & 3), 4);
                    memcpy(d + lane + 4, a + lane + 4 * ((imm >> 2) & 3), 4);
                    memcpy(d + lane + 8, b + lane + 4 * ((imm >> 4) & 3), 4);
                    memcpy(d + lane + 12, b + lane + 4 * ((imm >> 6) & 3), 4);
                }
            }
            avx_put(c, rd, d, W);
            goto done;
        }
        case 0x12: {
            // pp==0: VMOVHLPS (reg-reg) / VMOVLPS (m64); pp==1: VMOVLPD (m64). dst=reg, src1=vvvv.
            avx_get(c, vv, d);
            if (I.is_mem) { // VMOVLPS/VMOVLPD: dst.q0 = m64, dst.q1 = src1.q1
                uint64_t ea = avx_ea(state, c, &I, next, 8);
                (void)avx_memory_read(state, ea, d, 8);
            } else { // VMOVHLPS: dst.q0 = src2.q1
                avx_get(c, I.rm_reg, b);
                memcpy(d, b + 8, 8);
            }
            avx_put(c, rd, d, 16);
            goto done;
        }
        case 0x16: {
            // pp==0: VMOVLHPS (reg-reg) / VMOVHPS (m64); pp==1: VMOVHPD (m64). dst=reg, src1=vvvv.
            avx_get(c, vv, d);
            if (I.is_mem) { // VMOVHPS/VMOVHPD: dst.q1 = m64, dst.q0 = src1.q0
                uint64_t ea = avx_ea(state, c, &I, next, 8);
                (void)avx_memory_read(state, ea, d + 8, 8);
            } else { // VMOVLHPS: dst.q1 = src2.q0
                avx_get(c, I.rm_reg, b);
                memcpy(d + 8, b, 8);
            }
            avx_put(c, rd, d, 16);
            goto done;
        }
        // variable (scalar-count) shifts: count = low 64 bits of rm applied to every element.
        case 0xD1:
        case 0xD2:
        case 0xD3: // vpsrlw/d/q (logical right)
        case 0xE1:
        case 0xE2: // vpsraw/d (arithmetic right)
        case 0xF1:
        case 0xF2:
        case 0xF3: { // vpsllw/d/q (logical left)
            int es = (op == 0xD1 || op == 0xE1 || op == 0xF1) ? 2 : (op == 0xD2 || op == 0xE2 || op == 0xF2) ? 4 : 8;
            int arith = (op == 0xE1 || op == 0xE2);
            int left = (op == 0xF1 || op == 0xF2 || op == 0xF3);
            avx_get(c, vv, a);
            avx_get_rm(state, c, &I, next, 16, b); // count is the low 64 bits of the r/m 128-bit operand
            uint64_t cnt;
            memcpy(&cnt, b, 8);
            int bits = es * 8;
            for (int i = 0; i < W; i += es) {
                uint64_t x = 0;
                memcpy(&x, a + i, (size_t)es);
                uint64_t o;
                if (cnt >= (uint64_t)bits) {
                    if (arith) {
                        int64_t sh = 64 - bits;
                        o = (uint64_t)(((int64_t)(x << sh) >> sh) < 0 ? ~0ull : 0ull);
                    } else
                        o = 0;
                } else if (left) {
                    o = x << cnt;
                } else if (arith) {
                    int64_t sh = 64 - bits;
                    int64_t sx = ((int64_t)(x << sh)) >> sh;
                    o = (uint64_t)(sx >> cnt);
                } else {
                    o = x >> cnt;
                }
                memcpy(d + i, &o, (size_t)es);
            }
            avx_put(c, rd, d, W);
            goto done;
        }
        // compare-equal / greater (signed) by element width -> all-ones/zero mask
        case 0x74:
        case 0x75:
        case 0x76: // vpcmpeqb/w/d
        case 0x64:
        case 0x65:
        case 0x66: // vpcmpgtb/w/d (signed)
        {
            int es = (op == 0x74 || op == 0x64) ? 1 : (op == 0x75 || op == 0x65) ? 2 : 4;
            int gt = (op >= 0x64 && op <= 0x66);
            avx_get(c, vv, a);
            avx_get_rm(state, c, &I, next, W, b);
            for (int i = 0; i < W; i += es) {
                int64_t x = 0, y = 0;
                memcpy(&x, a + i, (size_t)es);
                memcpy(&y, b + i, (size_t)es);
                if (es < 8) { // sign-extend for gt
                    int sh = 64 - es * 8;
                    x = (x << sh) >> sh;
                    y = (y << sh) >> sh;
                }
                int t = gt ? (x > y) : (x == y);
                uint64_t m = t ? ~0ull : 0ull;
                memcpy(d + i, &m, (size_t)es);
            }
            avx_put(c, rd, d, W);
            goto done;
        }
        case 0x70: { // vpshufd(66)/vpshuflw(F2)/vpshufhw(F3) reg <- rm, imm8 (per 128-bit lane)
            avx_get_rm(state, c, &I, next, W, b);
            uint8_t imm = (uint8_t)I.imm;
            for (int lane = 0; lane < W; lane += 16) {
                if (pp == 1) { // vpshufd: 4 dwords
                    for (int j = 0; j < 4; j++) {
                        int sel = (imm >> (2 * j)) & 3;
                        memcpy(d + lane + 4 * j, b + lane + 4 * sel, 4);
                    }
                } else { // pshuflw(F2)/pshufhw(F3): shuffle low/high 4 words, copy the other half
                    memcpy(d + lane, b + lane, 16);
                    int base = (pp == 3) ? 8 : 0; // F3=high half, F2=low half
                    for (int j = 0; j < 4; j++) {
                        int sel = (imm >> (2 * j)) & 3;
                        memcpy(d + lane + base + 2 * j, b + lane + base + 2 * sel, 2);
                    }
                }
            }
            avx_put(c, rd, d, W);
            goto done;
        }
        case 0x71: // shift-by-imm8 group: dst=vvvv, src=rm(reg), ModRM.reg=opcode extension.
        case 0x72: // 0x71 word /2 psrlw /4 psraw /6 psllw; 0x72 dword /2 psrld /4 psrad /6 pslld;
        case 0x73: // 0x73 qword /2 psrlq /6 psllq; /3 psrldq /7 pslldq (per-128-lane byte shift).
        {
            int ext = rd, imm = (uint8_t)I.imm;
            int es = (op == 0x71) ? 2 : (op == 0x72) ? 4 : 8;
            avx_get(c, I.rm_reg, a); // source
            if (op == 0x73 && (ext == 3 || ext == 7)) {
                for (int lane = 0; lane < W; lane += 16)
                    for (int i = 0; i < 16; i++) {
                        if (ext == 3) // psrldq
                            d[lane + i] = (i + imm < 16) ? a[lane + i + imm] : 0;
                        else // pslldq
                            d[lane + i] = (i - imm >= 0) ? a[lane + i - imm] : 0;
                    }
            } else {
                int left = (ext == 6), arith = (ext == 4), bits = es * 8;
                for (int i = 0; i < W; i += es) {
                    uint64_t v = 0, z;
                    memcpy(&v, a + i, (size_t)es);
                    if (left)
                        z = (imm >= bits) ? 0 : (v << imm);
                    else if (arith) {
                        int sh = 64 - bits;
                        int64_t sv = ((int64_t)v << sh) >> sh;
                        z = (uint64_t)(sv >> (imm >= bits ? bits - 1 : imm));
                    } else
                        z = (imm >= bits) ? 0 : (v >> imm);
                    memcpy(d + i, &z, (size_t)es);
                }
            }
            avx_put(c, vv, d, W); // dst = VEX.vvvv
            goto done;
        }
        case 0xC2: { // vcmpps/pd/ss/sd: per-lane predicate compare -> all-ones/zero mask
            avx_get(c, vv, a);
            avx_get_rm(state, c, &I, next, W, b);
            int pred = (int)(I.imm & 0x1f);
            int dbl = (pp == 1 || pp == 3), scalar = (pp == 2 || pp == 3), es = dbl ? 8 : 4;
            memcpy(d, a, 64); // scalar keeps src1's upper lanes; packed overwrites fully
            int n = scalar ? es : W;
            for (int i = 0; i < n; i += es) {
                double x, y;
                if (dbl) {
                    memcpy(&x, a + i, 8);
                    memcpy(&y, b + i, 8);
                } else {
                    float xf, yf;
                    memcpy(&xf, a + i, 4);
                    memcpy(&yf, b + i, 4);
                    x = xf;
                    y = yf;
                }
                int t = avx_cmp_pred(x, y, pred);
                uint64_t m = t ? ~0ull : 0ull;
                memcpy(d + i, &m, (size_t)es);
            }
            avx_put(c, rd, d, scalar ? 16 : W);
            goto done;
        }
        case 0x5B: { // vcvtdq2ps(NP) / vcvtps2dq(66) / vcvttps2dq(F3): packed 32-bit int<->float
            avx_get_rm(state, c, &I, next, W, b);
            for (int i = 0; i < W; i += 4) {
                if (pp == 0) { // cvtdq2ps: int32 -> float32
                    int32_t v;
                    memcpy(&v, b + i, 4);
                    float f = (float)v;
                    memcpy(d + i, &f, 4);
                } else { // cvtps2dq(66, round)/cvttps2dq(F3, truncate) -> int32
                    float f;
                    memcpy(&f, b + i, 4);
                    int32_t r = (int32_t)cvt_x86_f2i(f, pp == 2, 0); // F3(pp==2)=truncate, 66(pp==1)=round
                    memcpy(d + i, &r, 4);
                }
            }
            avx_put(c, rd, d, W);
            goto done;
        }
        case 0xE6: {       // vcvttpd2dq(66)/vcvtdq2pd(F3)/vcvtpd2dq(F2): packed double<->int32
            if (pp == 2) { // F3 cvtdq2pd: W/2 bytes int32 -> W bytes double
                avx_get_rm(state, c, &I, next, W / 2, b);
                int n = W / 8;
                for (int i = 0; i < n; i++) {
                    int32_t v;
                    memcpy(&v, b + 4 * i, 4);
                    double y = (double)v;
                    memcpy(d + 8 * i, &y, 8);
                }
                avx_put(c, rd, d, W);
            } else { // 66 cvttpd2dq(truncate) / F2 cvtpd2dq(round): W bytes double -> W/2 bytes int32
                avx_get_rm(state, c, &I, next, W, b);
                int n = W / 8;
                for (int i = 0; i < n; i++) {
                    double f;
                    memcpy(&f, b + 8 * i, 8);
                    int32_t r = (int32_t)cvt_x86_d2i(f, pp == 1, 0); // 66(pp==1)=truncate, F2(pp==3)=round
                    memcpy(d + 4 * i, &r, 4);
                }
                avx_put(c, rd, d, W / 2);
            }
            goto done;
        }
        case 0x77: { // vzeroupper (L=0): zero bits[128:256) of ymm0..15. vzeroall (L=1): zero all of 0..15.
            uint8_t z[64];
            memset(z, 0, 64);
            for (int r = 0; r < 16; r++) {
                if (L == 1) { // vzeroall
                    memset(&c->v[2 * r], 0, 16);
                }
                memset(&c->vhi[2 * r], 0, 16);
                memset(&c->vz[4 * r], 0, 32);
            }
            goto done;
        }
        }
    }
    // ---- map 2 (0F38) ----
    if (map == 2) {
        switch (op) {
        case 0x00: { // vpshufb: per-128-lane byte shuffle (src1=vvvv, control=rm)
            avx_get(c, vv, a);
            avx_get_rm(state, c, &I, next, W, b);
            for (int lane = 0; lane < W; lane += 16)
                for (int i = 0; i < 16; i++) {
                    uint8_t ctl = b[lane + i];
                    d[lane + i] = (ctl & 0x80) ? 0 : a[lane + (ctl & 0x0F)];
                }
            avx_put(c, rd, d, W);
            goto done;
        }
        case 0x01:   // vphaddw    per-128-lane horizontal add/sub of adjacent pairs; src1=vvvv, src2=rm.
        case 0x02:   // vphaddd    (03/07 saturate the 16-bit results)
        case 0x03:   // vphaddsw
        case 0x05:   // vphsubw
        case 0x06:   // vphsubd
        case 0x07: { // vphsubsw
            avx_get(c, vv, a);
            avx_get_rm(state, c, &I, next, W, b);
            int sub = (op >= 0x05), sat = (op == 0x03 || op == 0x07), dword = (op == 0x02 || op == 0x06);
            for (int lane = 0; lane < W; lane += 16) {
                if (dword) {
                    int32_t x[4], y[4], o[4];
                    memcpy(x, a + lane, 16);
                    memcpy(y, b + lane, 16);
                    o[0] = sub ? x[0] - x[1] : x[0] + x[1];
                    o[1] = sub ? x[2] - x[3] : x[2] + x[3];
                    o[2] = sub ? y[0] - y[1] : y[0] + y[1];
                    o[3] = sub ? y[2] - y[3] : y[2] + y[3];
                    memcpy(d + lane, o, 16);
                } else {
                    int16_t x[8], y[8], o[8];
                    memcpy(x, a + lane, 16);
                    memcpy(y, b + lane, 16);
                    for (int i = 0; i < 4; i++) {
                        int va = sub ? x[2 * i] - x[2 * i + 1] : x[2 * i] + x[2 * i + 1];
                        int vb = sub ? y[2 * i] - y[2 * i + 1] : y[2 * i] + y[2 * i + 1];
                        o[i] = sat ? (int16_t)sat_s16(va) : (int16_t)va;
                        o[i + 4] = sat ? (int16_t)sat_s16(vb) : (int16_t)vb;
                    }
                    memcpy(d + lane, o, 16);
                }
            }
            avx_put(c, rd, d, W);
            goto done;
        }
        case 0x04: { // vpmaddubsw: word = sat16(uD[2k]*sB[2k] + uD[2k+1]*sB[2k+1]); src1 unsigned, src2 signed
            avx_get(c, vv, a);
            avx_get_rm(state, c, &I, next, W, b);
            for (int lane = 0; lane < W; lane += 16) {
                int16_t o[8];
                for (int k = 0; k < 8; k++) {
                    int p = (int)(uint8_t)a[lane + 2 * k] * (int)(int8_t)b[lane + 2 * k] +
                            (int)(uint8_t)a[lane + 2 * k + 1] * (int)(int8_t)b[lane + 2 * k + 1];
                    o[k] = (int16_t)sat_s16(p);
                }
                memcpy(d + lane, o, 16);
            }
            avx_put(c, rd, d, W);
            goto done;
        }
        case 0x08:   // vpsignb: dst = (src2<0)?-src1 : (src2==0)?0 : src1  (per element width)
        case 0x09:   // vpsignw
        case 0x0A: { // vpsignd
            avx_get(c, vv, a);
            avx_get_rm(state, c, &I, next, W, b);
            int es = (op == 0x08) ? 1 : (op == 0x09) ? 2 : 4;
            for (int i = 0; i < W; i += es) {
                uint64_t x = 0, y = 0;
                memcpy(&x, a + i, (size_t)es);
                memcpy(&y, b + i, (size_t)es);
                uint64_t o = simd_element_negative(y, es) ? simd_element_negate(x, es) : y == 0 ? 0 : x;
                memcpy(d + i, &o, (size_t)es);
            }
            avx_put(c, rd, d, W);
            goto done;
        }
        case 0x0B: { // vpmulhrsw: o = (((a*b)>>14)+1)>>1 signed words; src1=vvvv, src2=rm
            avx_get(c, vv, a);
            avx_get_rm(state, c, &I, next, W, b);
            for (int i = 0; i < W; i += 2) {
                int16_t x, y;
                memcpy(&x, a + i, 2);
                memcpy(&y, b + i, 2);
                int16_t o = (int16_t)((((x * y) >> 14) + 1) >> 1);
                memcpy(d + i, &o, 2);
            }
            avx_put(c, rd, d, W);
            goto done;
        }
        case 0x1C:   // vpabsb: dst = |src| (single source r/m), per element width
        case 0x1D:   // vpabsw
        case 0x1E: { // vpabsd
            avx_get_rm(state, c, &I, next, W, b);
            int es = (op == 0x1C) ? 1 : (op == 0x1D) ? 2 : 4;
            for (int i = 0; i < W; i += es) {
                uint64_t x = 0;
                memcpy(&x, b + i, (size_t)es);
                uint64_t o = simd_element_negative(x, es) ? simd_element_negate(x, es) : x;
                memcpy(d + i, &o, (size_t)es);
            }
            avx_put(c, rd, d, W);
            goto done;
        }
        case 0x28: { // vpmuldq: signed even-dword products -> qwords, per-128-lane
            avx_get(c, vv, a);
            avx_get_rm(state, c, &I, next, W, b);
            for (int lane = 0; lane < W; lane += 16) {
                int32_t x[4], y[4];
                int64_t o[2];
                memcpy(x, a + lane, 16);
                memcpy(y, b + lane, 16);
                o[0] = (int64_t)x[0] * (int64_t)y[0];
                o[1] = (int64_t)x[2] * (int64_t)y[2];
                memcpy(d + lane, o, 16);
            }
            avx_put(c, rd, d, W);
            goto done;
        }
        case 0x2A: { // vmovntdqa: streaming aligned load m128/m256 -> reg
            avx_get_rm(state, c, &I, next, W, b);
            avx_put(c, rd, b, W);
            goto done;
        }
        case 0x38: // vpminsb/vpmaxsb (byte), vpminsd/vpmaxsd (dword),
        case 0x39: // vpminuw/vpmaxuw (word), vpminud/vpmaxud (dword) -- src1=vvvv, src2=rm
        case 0x3A:
        case 0x3B:
        case 0x3C:
        case 0x3D:
        case 0x3E:
        case 0x3F: {
            avx_get(c, vv, a);
            avx_get_rm(state, c, &I, next, W, b);
            int is_max = (op >= 0x3C);
            if (op == 0x38 || op == 0x3C) { // signed byte
                for (int i = 0; i < W; i++) {
                    int8_t x = (int8_t)a[i], y = (int8_t)b[i];
                    d[i] = (uint8_t)(is_max ? (x > y ? x : y) : (x < y ? x : y));
                }
            } else if (op == 0x3A || op == 0x3E) { // unsigned word
                for (int i = 0; i < W; i += 2) {
                    uint16_t x, y;
                    memcpy(&x, a + i, 2);
                    memcpy(&y, b + i, 2);
                    uint16_t o = is_max ? (x > y ? x : y) : (x < y ? x : y);
                    memcpy(d + i, &o, 2);
                }
            } else if (op == 0x39 || op == 0x3D) { // signed dword
                for (int i = 0; i < W; i += 4) {
                    int32_t x, y;
                    memcpy(&x, a + i, 4);
                    memcpy(&y, b + i, 4);
                    int32_t o = is_max ? (x > y ? x : y) : (x < y ? x : y);
                    memcpy(d + i, &o, 4);
                }
            } else { // 0x3B/0x3F unsigned dword
                for (int i = 0; i < W; i += 4) {
                    uint32_t x, y;
                    memcpy(&x, a + i, 4);
                    memcpy(&y, b + i, 4);
                    uint32_t o = is_max ? (x > y ? x : y) : (x < y ? x : y);
                    memcpy(d + i, &o, 4);
                }
            }
            avx_put(c, rd, d, W);
            goto done;
        }
        case 0x41: { // vphminposuw: 128-bit only; word0=min unsigned word of rm, word1=its index, rest 0
            avx_get_rm(state, c, &I, next, 16, b);
            uint16_t w[8];
            memcpy(w, b, 16);
            uint16_t best = w[0];
            int idx = 0;
            for (int i = 1; i < 8; i++)
                if (w[i] < best) {
                    best = w[i];
                    idx = i;
                }
            // Sized to 64 bytes (avx_put's declared parameter width) though only the low 16 are used, so
            // the compiler sees a source object large enough for the in[64] contract (-Wstringop-overread).
            uint16_t o[32] = {best, (uint16_t)idx};
            avx_put(c, rd, (uint8_t *)o, 16);
            goto done;
        }
        case 0x90:   // vpgatherd{d,q}: 32-bit (dword) indices
        case 0x92:   // vgatherd{ps,pd}: 32-bit indices (same addressing; element bits are float)
        case 0x91:   // vpgatherq{d,q}: 64-bit (qword) indices
        case 0x93: { // vgatherq{ps,pd}: 64-bit indices
            // AVX2 masked gather. VSIB: the index is a VECTOR register (I.m_index) scaled by 1<<m_scale off
            // a GPR base+disp; the mask is VEX.vvvv, one element per destination element, tested on its top
            // bit. elem = destination element size (W0=dword, W1=qword), isz = index element size.
            //
            // THE ADDRESS IS GUEST DATA: base + (int)index_element * scale spans the whole 64-bit space
            // whatever the base register holds, so every element goes through the fault bracket above.
            //
            // RESTARTABILITY is what the mask semantics exist for, and it is exact here (measured on
            // silicon: a gather whose 3rd element hits a guard page faults ONCE, and after the handler maps
            // the page the completed elements are NOT re-gathered while the remaining ones read the page's
            // new contents). So: clear each mask element as its element COMPLETES, and on a fault commit the
            // partial destination and the partially cleared mask before abandoning -- rip is left on the
            // instruction, so the retry gathers exactly the elements whose mask bit survived. The whole
            // mask register is cleared only on full completion.
            if (I.evex) goto avx_unimpl; // EVEX gathers mask on k1, not vvvv; emulating them as VEX is wrong
            // #UD if any two of destination, index and mask are the same register (SDM); measured: SIGILL,
            // ILL_ILLOPN. `c4 e2 78 91 04 8b` -- dest == mask == xmm0 -- is this case, not a memory fault.
            if (rd == I.m_index || rd == vv || vv == I.m_index) avx_undefined();
            int elem = I.vex_w ? 8 : 4;
            int isz = (op == 0x90 || op == 0x92) ? 4 : 8;
            int nlanes = (isz == 4) ? (W / elem) : (W / 8);
            int result_bytes = nlanes * elem;
            uint8_t idxv[64], mask[64];
            avx_get(c, I.m_index, idxv); // VSIB vector index register
            avx_get(c, vv, mask);        // mask register (VEX.vvvv)
            avx_get(c, rd, d);           // masked-off lanes keep the old destination value
            uint64_t base = 0;
            if (I.m_hasbase) base += c->r[I.m_base];
            base += (uint64_t)I.disp;
            if (I.seg == 1)
                base += c->fs_base;
            else if (I.seg == 2)
                base += c->gs_base;
            int64_t scale = (int64_t)1 << I.m_scale;
            for (int i = 0; i < nlanes; i++) {
                if (mask[(i + 1) * elem - 1] & 0x80) {
                    int64_t index;
                    if (isz == 4) {
                        int32_t t;
                        memcpy(&t, idxv + i * 4, 4);
                        index = t;
                    } else {
                        memcpy(&index, idxv + i * 8, 8);
                    }
                    uint64_t addr = hl_x86_avx_address(state, base + (uint64_t)(index * scale));
                    if (!avx_try_read(state, addr, d + i * elem, (size_t)elem)) {
                        // Suspended, not abandoned: elements < i are done and must survive. 64-byte writes,
                        // so nothing this instruction has not touched moves (no upper-zeroing on a fault).
                        avx_put(c, rd, d, 64);
                        avx_put(c, vv, mask, 64);
                        avx_abandon(addr, (uint64_t)elem, X86_SOFT_READ);
                    }
                }
                memset(mask + i * elem, 0, (size_t)elem); // this element is complete
            }
            avx_put(c, rd, d, result_bytes); // dst above the result width is zeroed
            uint8_t zero[64];
            memset(zero, 0, 64);
            avx_put(c, vv, zero, W); // the entire mask register is cleared after a completed gather
            goto done;
        }
        }
    }
    // ---- map 3 (0F3A) ----
    if (map == 3) {
        if (avx_dispatch_lane_transfer(state, c, &I, next) == AVX_DISPATCH_HANDLED) goto done;
        if (avx_dispatch_lane_permutation(state, c, &I, next, W) == AVX_DISPATCH_HANDLED) goto done;
        if (avx_dispatch_immediate_floating(state, c, &I, next, W) == AVX_DISPATCH_HANDLED) goto done;
        switch (op) {
        case 0x0F: { // vpalignr imm8: per-128-lane byte concat(src1:src2) >> imm
            avx_get(c, vv, a);
            avx_get_rm(state, c, &I, next, W, b);
            int sh = (uint8_t)I.imm;
            for (int lane = 0; lane < W; lane += 16) {
                uint8_t t[32];
                memcpy(t, b + lane, 16);
                memcpy(t + 16, a + lane, 16);
                for (int i = 0; i < 16; i++)
                    d[lane + i] = sh < 32 - i ? t[sh + i] : 0;
            }
            avx_put(c, rd, d, W);
            goto done;
        }
        case 0x42: { // vmpsadbw imm8: per-128-lane SAD windows; src1=vvvv, src2=rm.
            // Low lane uses imm[2:0] (imm[1:0]=src2 block, imm[2]=src1 block); the high 128-bit
            // lane (256-bit form) uses imm[5:3] the same way.
            avx_get(c, vv, a);
            avx_get_rm(state, c, &I, next, W, b);
            for (int lane = 0; lane < W; lane += 16) {
                int ctl = (I.imm >> ((lane / 16) * 3)) & 7;
                int boff = (ctl & 3) * 4;
                int aoff = ((ctl >> 2) & 1) * 4;
                uint16_t o[8];
                for (int i = 0; i < 8; i++) {
                    int sum = 0;
                    for (int k = 0; k < 4; k++) {
                        int diff = (int)a[lane + aoff + i + k] - (int)b[lane + boff + k];
                        sum += diff < 0 ? -diff : diff;
                    }
                    o[i] = (uint16_t)sum;
                }
                memcpy(d + lane, o, 16);
            }
            avx_put(c, rd, d, W);
            goto done;
        }
        }
    }

    // ---- unimplemented: report precisely + exit 70 so coverage is grown test-driven ----
avx_unimpl:
    if (!g_avx_warned) {
        g_avx_warned = 1;
        fprintf(stderr, "[avx] UNIMPLEMENTED %s map=%d op=0x%02x pp=%d L=%d w=%d rip=%llx\n", I.evex ? "EVEX" : "VEX",
                map, op, pp, L, I.vex_w, (unsigned long long)c->rip);
    }
    c->exited = 1;
    c->exit_code = 70;
    return;

done:
    c->rip = next;
}

// ============================================================================================
// Legacy (non-VEX) 0F38 / 0F3A emulation (R_SSE3B): SSSE3, SSE4.1, SSE4.2, AES-NI, SHA, PCLMUL,
// CRC32 and MOVBE. Mirrors do_avx(): the translator exits the block at each such insn, this
// re-decodes it at cpu->rip, emulates against the xmm file (v[]) / GPRs (r[]) / memory and
// advances rip. Legacy SSE is destructive: ModRM.reg is both src1 and dst; ModRM.r/m is src2.
// ============================================================================================

static const uint8_t k_aes_sbox[256] = {
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76, 0xca, 0x82, 0xc9,
    0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0, 0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f,
    0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15, 0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07,
    0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75, 0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3,
    0x29, 0xe3, 0x2f, 0x84, 0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58,
    0xcf, 0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8, 0x51, 0xa3,
    0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2, 0xcd, 0x0c, 0x13, 0xec, 0x5f,
    0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73, 0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88,
    0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb, 0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac,
    0x62, 0x91, 0x95, 0xe4, 0x79, 0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a,
    0xae, 0x08, 0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a, 0x70,
    0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e, 0xe1, 0xf8, 0x98, 0x11,
    0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf, 0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42,
    0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16};
static const uint8_t k_aes_isbox[256] = {
    0x52, 0x09, 0x6a, 0xd5, 0x30, 0x36, 0xa5, 0x38, 0xbf, 0x40, 0xa3, 0x9e, 0x81, 0xf3, 0xd7, 0xfb, 0x7c, 0xe3, 0x39,
    0x82, 0x9b, 0x2f, 0xff, 0x87, 0x34, 0x8e, 0x43, 0x44, 0xc4, 0xde, 0xe9, 0xcb, 0x54, 0x7b, 0x94, 0x32, 0xa6, 0xc2,
    0x23, 0x3d, 0xee, 0x4c, 0x95, 0x0b, 0x42, 0xfa, 0xc3, 0x4e, 0x08, 0x2e, 0xa1, 0x66, 0x28, 0xd9, 0x24, 0xb2, 0x76,
    0x5b, 0xa2, 0x49, 0x6d, 0x8b, 0xd1, 0x25, 0x72, 0xf8, 0xf6, 0x64, 0x86, 0x68, 0x98, 0x16, 0xd4, 0xa4, 0x5c, 0xcc,
    0x5d, 0x65, 0xb6, 0x92, 0x6c, 0x70, 0x48, 0x50, 0xfd, 0xed, 0xb9, 0xda, 0x5e, 0x15, 0x46, 0x57, 0xa7, 0x8d, 0x9d,
    0x84, 0x90, 0xd8, 0xab, 0x00, 0x8c, 0xbc, 0xd3, 0x0a, 0xf7, 0xe4, 0x58, 0x05, 0xb8, 0xb3, 0x45, 0x06, 0xd0, 0x2c,
    0x1e, 0x8f, 0xca, 0x3f, 0x0f, 0x02, 0xc1, 0xaf, 0xbd, 0x03, 0x01, 0x13, 0x8a, 0x6b, 0x3a, 0x91, 0x11, 0x41, 0x4f,
    0x67, 0xdc, 0xea, 0x97, 0xf2, 0xcf, 0xce, 0xf0, 0xb4, 0xe6, 0x73, 0x96, 0xac, 0x74, 0x22, 0xe7, 0xad, 0x35, 0x85,
    0xe2, 0xf9, 0x37, 0xe8, 0x1c, 0x75, 0xdf, 0x6e, 0x47, 0xf1, 0x1a, 0x71, 0x1d, 0x29, 0xc5, 0x89, 0x6f, 0xb7, 0x62,
    0x0e, 0xaa, 0x18, 0xbe, 0x1b, 0xfc, 0x56, 0x3e, 0x4b, 0xc6, 0xd2, 0x79, 0x20, 0x9a, 0xdb, 0xc0, 0xfe, 0x78, 0xcd,
    0x5a, 0xf4, 0x1f, 0xdd, 0xa8, 0x33, 0x88, 0x07, 0xc7, 0x31, 0xb1, 0x12, 0x10, 0x59, 0x27, 0x80, 0xec, 0x5f, 0x60,
    0x51, 0x7f, 0xa9, 0x19, 0xb5, 0x4a, 0x0d, 0x2d, 0xe5, 0x7a, 0x9f, 0x93, 0xc9, 0x9c, 0xef, 0xa0, 0xe0, 0x3b, 0x4d,
    0xae, 0x2a, 0xf5, 0xb0, 0xc8, 0xeb, 0xbb, 0x3c, 0x83, 0x53, 0x99, 0x61, 0x17, 0x2b, 0x04, 0x7e, 0xba, 0x77, 0xd6,
    0x26, 0xe1, 0x69, 0x14, 0x63, 0x55, 0x21, 0x0c, 0x7d};

static uint8_t aes_gfmul(uint8_t a, uint8_t b) {
    uint8_t p = 0;
    for (int i = 0; i < 8; i++) {
        if (b & 1) p ^= a;
        uint8_t hi = a & 0x80;
        a <<= 1;
        if (hi) a ^= 0x1b;
        b >>= 1;
    }
    return p;
}

static void aes_subbytes(uint8_t s[16], const uint8_t box[256]) {
    for (int i = 0; i < 16; i++)
        s[i] = box[s[i]];
}

// ShiftRows (inv=0) / InvShiftRows (inv=1). State is column-major: s[4*col+row].
static void aes_shiftrows(const uint8_t in[16], uint8_t out[16], int inv) {
    for (int col = 0; col < 4; col++)
        for (int row = 0; row < 4; row++) {
            int sc = inv ? ((col - row) & 3) : ((col + row) & 3);
            out[4 * col + row] = in[4 * sc + row];
        }
}

static void aes_mixcolumns(uint8_t s[16], int inv) {
    for (int col = 0; col < 4; col++) {
        uint8_t a0 = s[4 * col], a1 = s[4 * col + 1], a2 = s[4 * col + 2], a3 = s[4 * col + 3];
        if (!inv) {
            s[4 * col] = aes_gfmul(a0, 2) ^ aes_gfmul(a1, 3) ^ a2 ^ a3;
            s[4 * col + 1] = a0 ^ aes_gfmul(a1, 2) ^ aes_gfmul(a2, 3) ^ a3;
            s[4 * col + 2] = a0 ^ a1 ^ aes_gfmul(a2, 2) ^ aes_gfmul(a3, 3);
            s[4 * col + 3] = aes_gfmul(a0, 3) ^ a1 ^ a2 ^ aes_gfmul(a3, 2);
        } else {
            s[4 * col] = aes_gfmul(a0, 14) ^ aes_gfmul(a1, 11) ^ aes_gfmul(a2, 13) ^ aes_gfmul(a3, 9);
            s[4 * col + 1] = aes_gfmul(a0, 9) ^ aes_gfmul(a1, 14) ^ aes_gfmul(a2, 11) ^ aes_gfmul(a3, 13);
            s[4 * col + 2] = aes_gfmul(a0, 13) ^ aes_gfmul(a1, 9) ^ aes_gfmul(a2, 14) ^ aes_gfmul(a3, 11);
            s[4 * col + 3] = aes_gfmul(a0, 11) ^ aes_gfmul(a1, 13) ^ aes_gfmul(a2, 9) ^ aes_gfmul(a3, 14);
        }
    }
}

static inline uint32_t rotr32(uint32_t x, int n) {
    return (x >> n) | (x << (32 - n));
}

static inline uint32_t rotl32(uint32_t x, int n) {
    return (x << n) | (x >> (32 - n));
}

// CRC-32C (Castagnoli, reflected poly 0x82F63B78) -- the polynomial used by the x86 CRC32 instruction.
static uint32_t crc32c_step(uint32_t crc, uint64_t v, int nbytes) {
    for (int b = 0; b < nbytes; b++) {
        crc ^= (uint8_t)(v >> (8 * b));
        for (int i = 0; i < 8; i++)
            crc = (crc >> 1) ^ (0x82F63B78u & (uint32_t)(-(int32_t)(crc & 1)));
    }
    return crc;
}

// ROUND* rounding. When the imm's bit2 is set the op must use the CURRENT rounding mode (MXCSR.RC),
// which ldmxcsr has already threaded into the host FPCR.RMode -- so __builtin_rint (current-mode) honors
// it here (fixes the "treated as nearest" gap). The explicit modes 0..3 must instead force that specific
// direction regardless of MXCSR: floor/ceil/trunc already do, and explicit nearest is round-to-nearest-
// EVEN independent of the current mode (__builtin_roundeven), not __builtin_rint (which would follow RC).
//
// __builtin_rint cannot serve that on an x86-64 host, and fails silently: with no ROUNDSD available
// (-msse4.1 is not assumable) gcc inlines `|x| + 2^52 - 2^52` with the sign re-applied, rounding the
// MAGNITUDE -- valid only under round-to-nearest, i.e. not the mode LDMXCSR just set. So resolve MXCSR.RC
// (bit-for-bit the ROUND* imm[1:0] encoding) instead. AArch64 keeps the builtin; it lowers to FRINTX.
#if defined(HL_HOST_CPU_X86_64)
static int sse_host_rounding_control(void) {
    return (int)((_mm_getcsr() >> 13) & 3u); // MXCSR bits 14:13 = RC
}
#endif

// ROUND* raises a SMALLER set than any rounding primitive available here, so the whole step runs with the
// sticky flags parked and the caller's set is raised explicitly. Measured on Zen 4, all imm encodings:
//   #P iff the result differs from the source, and imm[3] (the suppress-precision bit) is clear;
//   #I iff the source is an SNaN, which the result also QUIETS (imm[3] suppresses neither);
//   NOTHING else -- notably NEVER #D, which is what glibc's trunc/floor add via ROUNDSD (roundps of a
//   denormal reported 22 here against native's 20).
// Difference is decided on the BIT PATTERN: an FP compare of the two is a COMISD, which raises #D for a
// denormal operand, and the sign of a zero must count (trunc(-0.4) = -0.0 is inexact). DAZ has to be
// applied by hand for the same reason -- the hardware zeroes a denormal source before ROUND* runs, making
// it exact, but a bit-pattern comparison against the UNzeroed source would call it inexact.
static double sse_round_d(double x, int imm) {
    unsigned parked = cvt_fp_flags(), raise = 0;
    uint64_t xb;
    memcpy(&xb, &x, 8);
    if (sse_daz_active() && sse_is_denorm_f64(xb)) { // DAZ: the source reads as a zero of the same sign
        xb &= UINT64_C(0x8000000000000000);
        memcpy(&x, &xb, 8);
    }
    int mode = imm & 3, use_mxcsr = (imm & 4) != 0;
#if defined(HL_HOST_CPU_X86_64)
    if (use_mxcsr) {
        mode = sse_host_rounding_control();
        use_mxcsr = 0;
    }
#endif
    double r;
    if (use_mxcsr)
        r = __builtin_rint(x); // honor MXCSR.RC (mirrored into host FPCR by ldmxcsr)
    else
        switch (mode) {
        case 1: r = __builtin_floor(x); break;
        case 2: r = __builtin_ceil(x); break;
        case 3: r = __builtin_trunc(x); break;
        default: r = __builtin_roundeven(x); break; // explicit round-to-nearest-even
        }
    uint64_t rb;
    memcpy(&rb, &r, 8);
    if ((xb & UINT64_C(0x7ff0000000000000)) == UINT64_C(0x7ff0000000000000) &&
        (xb & UINT64_C(0x000fffffffffffff)) != 0) { // NaN
        if (!(xb & UINT64_C(0x0008000000000000))) { // signalling
            raise = SSE_XI;
            rb = xb | UINT64_C(0x0008000000000000); // x86 returns it QUIET; glibc trunc/floor do not
            memcpy(&r, &rb, 8);
        }
    } else if (rb != xb && !(imm & 8))
        raise = SSE_XP;
    cvt_fp_flags_set(parked);
    sse_raise(raise);
    return r;
}

static float sse_round_f(float x, int imm) {
    unsigned parked = cvt_fp_flags(), raise = 0;
    uint32_t xb;
    memcpy(&xb, &x, 4);
    if (sse_daz_active() && sse_is_denorm_f32(xb)) {
        xb &= 0x80000000u;
        memcpy(&x, &xb, 4);
    }
    int mode = imm & 3, use_mxcsr = (imm & 4) != 0;
#if defined(HL_HOST_CPU_X86_64)
    if (use_mxcsr) {
        mode = sse_host_rounding_control();
        use_mxcsr = 0;
    }
#endif
    float r;
    if (use_mxcsr)
        r = __builtin_rintf(x);
    else
        switch (mode) {
        case 1: r = __builtin_floorf(x); break;
        case 2: r = __builtin_ceilf(x); break;
        case 3: r = __builtin_truncf(x); break;
        default: r = __builtin_roundevenf(x); break;
        }
    uint32_t rb;
    memcpy(&rb, &r, 4);
    if ((xb & 0x7f800000u) == 0x7f800000u && (xb & 0x007fffffu) != 0) {
        if (!(xb & 0x00400000u)) {
            raise = SSE_XI;
            rb = xb | 0x00400000u;
            memcpy(&r, &rb, 4);
        }
    } else if (rb != xb && !(imm & 8))
        raise = SSE_XP;
    cvt_fp_flags_set(parked);
    sse_raise(raise);
    return r;
}

static inline int sat_s16(int v) {
    return v < -32768 ? -32768 : v > 32767 ? 32767 : v;
}

static inline int sat_u16(int v) {
    return v < 0 ? 0 : v > 65535 ? 65535 : v;
}

// Read the 16-byte r/m operand (xmm register or m128) of a legacy SSE insn.
static void sse_get_rm(const hl_x86_avx_state *state, struct cpu *c, struct insn *I, uint64_t next, uint8_t buf[16]) {
    if (I->is_mem)
        (void)avx_memory_read(state, avx_ea(state, c, I, next, 16), buf, 16);
    else
        memcpy(buf, &c->v[2 * I->rm_reg], 16);
}

// ---- SSE4.2 packed string compare core (PCMP{I,E}STR{I,M}) --------------------------------------
// imm8 control byte: [0]=word(1)/byte(0) elements, [1]=signed, [3:2]=aggregation (0 equal-any,
// 1 ranges, 2 equal-each, 3 equal-ordered), [4]=negate polarity, [5]=negate valid positions only,
// [6]=index lsb(0)/msb(1) OR mask bit(0)/element-wide(1). a=operand1 (reg/xmm1), b=operand2 (r/m);
// la/lb are the element lengths of a/b (implicit: first-null scan; explicit: EAX/EDX, saturated).
static int64_t sse42_elem(const uint8_t *p, int i, int wordsz, int sgn) {
    if (wordsz == 1) return sgn ? (int64_t)(int8_t)p[i] : (int64_t)p[i];
    uint16_t w;
    memcpy(&w, p + 2 * i, 2);
    return sgn ? (int64_t)(int16_t)w : (int64_t)w;
}

// Implicit length: index of the first null element, else n.
static int sse42_ilen(const uint8_t *p, int wordsz, int n) {
    for (int i = 0; i < n; i++) {
        uint64_t e = 0;
        memcpy(&e, p + i * wordsz, (size_t)wordsz);
        if (e == 0) return i;
    }
    return n;
}

// Explicit length (PCMPESTR*): abs value of the GPR, saturated to the element count.
static int sse42_elen(int64_t v, int n) {
    if (v < 0) v = -v;
    return (v > n) ? n : (int)v;
}

// Compute the polarity-adjusted intermediate mask (IntRes2) over n elements per the imm8 control.
static int sse42_intres(const uint8_t *a, const uint8_t *b, int la, int lb, int imm, int n) {
    int wordsz = (imm & 1) ? 2 : 1;
    int sgn = (imm >> 1) & 1;
    int agg = (imm >> 2) & 3;
    int res = 0;
    for (int i = 0; i < n; i++) {
        int bit = 0;
        switch (agg) {
        case 0: // equal-any: b[i] occurs anywhere in the set a[0..la)
            if (i < lb)
                for (int j = 0; j < la; j++)
                    if (sse42_elem(a, j, wordsz, sgn) == sse42_elem(b, i, wordsz, sgn)) {
                        bit = 1;
                        break;
                    }
            break;
        case 1: // ranges: a[] holds [lo,hi] pairs; test lo <= b[i] <= hi
            if (i < lb) {
                int64_t bi = sse42_elem(b, i, wordsz, sgn);
                for (int j = 0; j + 1 < la; j += 2)
                    if (sse42_elem(a, j, wordsz, sgn) <= bi && bi <= sse42_elem(a, j + 1, wordsz, sgn)) {
                        bit = 1;
                        break;
                    }
            }
            break;
        case 2: // equal-each: a[i]==b[i], with both-invalid forced equal
            if (i < la && i < lb)
                bit = (sse42_elem(a, i, wordsz, sgn) == sse42_elem(b, i, wordsz, sgn));
            else if (i >= la && i >= lb)
                bit = 1;
            break;
        case 3: // equal-ordered: needle a[0..la) matches b starting at position i
            // The inner loop is bounded by the ELEMENT COUNT (k = i+j < n), not by la: the SDM's
            // per-element validity table decides the rest, and it is NOT symmetric --
            //   a valid,   b valid   -> compare
            //   a valid,   b invalid -> force FALSE   (needle runs off the end of the haystack)
            //   a invalid, b either  -> force TRUE    (needle exhausted -> the match stands)
            // Bounding the loop by `j < la` instead evaluated a k that had walked past the last
            // element, so a needle reaching the end of a full (null-free) operand2 -- e.g. the
            // all-ones vector a `cmppd` leaves behind -- was wrongly reported as a mismatch.
            bit = 1;
            for (int j = 0; i + j < n; j++) {
                int k = i + j;
                if (j >= la) break; // a invalid -> forced TRUE
                if (k >= lb) {      // a valid, b invalid -> FALSE
                    bit = 0;
                    break;
                }
                if (sse42_elem(a, j, wordsz, sgn) != sse42_elem(b, k, wordsz, sgn)) {
                    bit = 0;
                    break;
                }
            }
            break;
        }
        if (bit) res |= (1 << i);
    }
    if ((imm >> 4) & 1) { // negate polarity
        if ((imm >> 5) & 1)
            res ^= ((1 << lb) - 1); // masked: negate only valid operand2 positions
        else
            res ^= ((1 << n) - 1);
    }
    return res;
}

// SSE4.2 string-compare flags (Intel SDM): CF=(IntRes2!=0), ZF=operand2 has a null (lb<n),
// SF=operand1 has a null (la<n), OF=IntRes2[0], AF=PF=0. glibc's SSE4.2 strlen/strchr/strstr branch
// on these (jbe/ja/jc/jz) right after the op, so they MUST be set. Substrate: x86 CF = NOT stored-C.
static void sse42_flags(struct cpu *c, int res, int la, int lb, int n) {
    int cf = (res != 0), zf = (lb < n), sf = (la < n), of = (res & 1);
    c->nzcv = ((uint64_t)sf << 31) | ((uint64_t)zf << 30) | ((uint64_t)(!cf) << 29) | ((uint64_t)of << 28);
    c->pf = 1; // PF source byte with odd popcount -> x86 PF=0
    c->af = 0; // Intel SDM: PCMP*STR* clears AF
}

// Index output (PCMP*STRI) -> ECX: first/last set bit of IntRes2 (imm[6] picks lsb/msb), else n.
static void sse42_index(struct cpu *c, int res, int imm, int n) {
    int idx;
    if (res == 0)
        idx = n;
    else if ((imm >> 6) & 1)
        idx = 31 - __builtin_clz((unsigned)res);
    else
        idx = __builtin_ctz((unsigned)res);
    c->r[RCX] = (uint64_t)idx;
}

// Mask output (PCMP*STRM) -> XMM0. imm[6]=0: bit mask zero-extended; 1: element-wide (per-element) mask.
static void sse42_mask(struct cpu *c, int res, int imm, int n) {
    int wordsz = (imm & 1) ? 2 : 1;
    uint8_t out[16];
    memset(out, 0, 16);
    if ((imm >> 6) & 1) {
        for (int i = 0; i < n; i++)
            if (res & (1 << i)) memset(out + i * wordsz, 0xFF, (size_t)wordsz);
    } else {
        out[0] = (uint8_t)(res & 0xFF);
        if (n > 8) out[1] = (uint8_t)((res >> 8) & 0xFF);
    }
    memcpy(&c->v[0], out, 16); // XMM0 == c->v[0..1]; legacy SSE leaves the upper YMM bits intact
}

static enum avx_dispatch_result sse_dispatch_string_compare(const hl_x86_avx_state *state, struct cpu *c,
                                                            struct insn *I, uint64_t next, const uint8_t operand1[16]) {
    int op = I->op;
    if (I->map3 != 3 || (op != 0x60 && op != 0x61 && op != 0x62 && op != 0x63)) return AVX_DISPATCH_UNMATCHED;

    uint8_t operand2[16];
    sse_get_rm(state, c, I, next, operand2);
    int immediate = (int)I->imm;
    int element_width = (immediate & 1) ? 2 : 1;
    int elements = 16 / element_width;
    int operand1_length;
    int operand2_length;
    if (op == 0x60 || op == 0x61) {
        operand1_length = sse42_elen(I->rexW ? (int64_t)c->r[RAX] : (int32_t)c->r[RAX], elements);
        operand2_length = sse42_elen(I->rexW ? (int64_t)c->r[RDX] : (int32_t)c->r[RDX], elements);
    } else {
        operand1_length = sse42_ilen(operand1, element_width, elements);
        operand2_length = sse42_ilen(operand2, element_width, elements);
    }
    int result = sse42_intres(operand1, operand2, operand1_length, operand2_length, immediate, elements);
    if (op == 0x60 || op == 0x62)
        sse42_mask(c, result, immediate, elements);
    else
        sse42_index(c, result, immediate, elements);
    sse42_flags(c, result, operand1_length, operand2_length, elements);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_floating(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                      uint64_t next, uint8_t destination[16]) {
    int op = I->op;
    if (I->two && I->map3 == 0 && (op == 0x58 || op == 0x59 || op == 0x5C || op == 0x5E)) {
        int packed = !I->repne && !I->rep;
        int dbl = packed ? I->p66 : I->repne;
        int element_size = dbl ? 8 : 4;
        uint8_t operand[64];
        avx_get_rm(state, c, I, next, packed ? 16 : element_size, operand);
        int bytes = packed ? 16 : element_size;
        for (int offset = 0; offset < bytes; offset += element_size) {
            if (dbl) {
                double x, y;
                memcpy(&x, destination + offset, 8);
                memcpy(&y, operand + offset, 8);
                double result = avx_fp_arith_f64(op, x, y);
                memcpy(destination + offset, &result, 8);
            } else {
                float x, y;
                memcpy(&x, destination + offset, 4);
                memcpy(&y, operand + offset, 4);
                float result = avx_fp_arith_f32(op, x, y);
                memcpy(destination + offset, &result, 4);
            }
        }
        c->rip = next;
        return AVX_DISPATCH_HANDLED;
    }
    if (!(I->two && I->map3 == 0 && (op == 0x7C || op == 0x7D || op == 0xD0))) return AVX_DISPATCH_UNMATCHED;

    int dbl = I->p66 != 0;
    int subtract = op == 0x7D;
    uint8_t operand[64], output[16];
    avx_get_rm(state, c, I, next, 16, operand);
    if (dbl) {
        double x[2], y[2], result[2];
        memcpy(x, destination, 16);
        memcpy(y, operand, 16);
        if (op == 0xD0) {
            result[0] = avx_dnan_f64(x[0] - y[0], x[0], y[0]);
            result[1] = avx_dnan_f64(x[1] + y[1], x[1], y[1]);
        } else {
            result[0] = subtract ? avx_dnan_f64(x[0] - x[1], x[0], x[1]) : avx_dnan_f64(x[0] + x[1], x[0], x[1]);
            result[1] = subtract ? avx_dnan_f64(y[0] - y[1], y[0], y[1]) : avx_dnan_f64(y[0] + y[1], y[0], y[1]);
        }
        memcpy(output, result, 16);
    } else {
        float x[4], y[4], result[4];
        memcpy(x, destination, 16);
        memcpy(y, operand, 16);
        if (op == 0xD0) {
            for (int i = 0; i < 4; i++)
                result[i] = (i & 1) ? avx_dnan_f32(x[i] + y[i], x[i], y[i]) : avx_dnan_f32(x[i] - y[i], x[i], y[i]);
        } else {
            result[0] = subtract ? avx_dnan_f32(x[0] - x[1], x[0], x[1]) : avx_dnan_f32(x[0] + x[1], x[0], x[1]);
            result[1] = subtract ? avx_dnan_f32(x[2] - x[3], x[2], x[3]) : avx_dnan_f32(x[2] + x[3], x[2], x[3]);
            result[2] = subtract ? avx_dnan_f32(y[0] - y[1], y[0], y[1]) : avx_dnan_f32(y[0] + y[1], y[0], y[1]);
            result[3] = subtract ? avx_dnan_f32(y[2] - y[3], y[2], y[3]) : avx_dnan_f32(y[2] + y[3], y[2], y[3]);
        }
        memcpy(output, result, 16);
    }
    memcpy(destination, output, 16);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_crc_movbe(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                       uint64_t next) {
    int op = I->op;
    if (I->map3 != 2 || (op != 0xF0 && op != 0xF1)) return AVX_DISPATCH_UNMATCHED;

    if (I->repne) { // CRC32 r, r/m
        int bytes = op == 0xF0 ? 1 : I->opsize;
        uint64_t value;
        if (I->is_mem) {
            uint64_t address = avx_ea(state, c, I, next, bytes);
            value = 0;
            (void)avx_memory_read(state, address, &value, (size_t)bytes);
        } else if (bytes == 1 && !I->has_rex && I->rm_reg >= 4 && I->rm_reg <= 7) {
            value = (c->r[I->rm_reg - 4] >> 8) & 0xff; // AH/CH/DH/BH without REX
        } else {
            value = c->r[I->rm_reg];
        }
        c->r[I->reg] = crc32c_step((uint32_t)c->r[I->reg], value, bytes);
    } else { // MOVBE: byte-swapping memory load/store
        int bytes = I->opsize;
        uint64_t address = avx_ea(state, c, I, next, bytes);
        uint64_t value = 0;
        if (op == 0xF0)
            (void)avx_memory_read(state, address, &value, (size_t)bytes);
        else
            value = c->r[I->reg];
        uint64_t swapped = 0;
        for (int index = 0; index < bytes; index++)
            swapped |= ((value >> (8 * index)) & 0xff) << (8 * (bytes - 1 - index));
        if (op == 0xF1) {
            (void)avx_memory_write(state, address, &swapped, (size_t)bytes);
        } else if (bytes == 2) {
            c->r[I->reg] = (c->r[I->reg] & ~0xffffull) | (swapped & 0xffff);
        } else {
            c->r[I->reg] = swapped;
        }
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_lane_transfer(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                           uint64_t next, uint8_t destination[16]) {
    int op = I->op;
    if (I->map3 != 3) return AVX_DISPATCH_UNMATCHED;

    if (op == 0x14 || op == 0x15 || op == 0x16 || op == 0x17) { // PEXTR* / EXTRACTPS
        uint8_t immediate = (uint8_t)I->imm;
        uint64_t value;
        int width;
        if (op == 0x14) {
            width = 1;
            value = destination[immediate & 15];
        } else if (op == 0x15) {
            uint16_t word;
            width = 2;
            memcpy(&word, destination + 2 * (immediate & 7), 2);
            value = word;
        } else if (op == 0x16) {
            width = I->rexW ? 8 : 4;
            memcpy(&value, destination + (I->rexW ? 8 * (immediate & 1) : 4 * (immediate & 3)), (size_t)width);
        } else {
            uint32_t word;
            width = 4;
            memcpy(&word, destination + 4 * (immediate & 3), 4);
            value = word;
        }
        if (I->is_mem) {
            uint64_t address = avx_ea(state, c, I, next, width);
            (void)avx_memory_write(state, address, &value, (size_t)width);
        } else if (width == 8) {
            c->r[I->rm_reg] = value;
        } else {
            c->r[I->rm_reg] = (uint32_t)value;
        }
        c->rip = next;
        return AVX_DISPATCH_HANDLED;
    }

    if (op != 0x20 && op != 0x21 && op != 0x22) return AVX_DISPATCH_UNMATCHED;

    int immediate = (int)I->imm;
    if (op == 0x20) { // PINSRB
        uint8_t value = (uint8_t)c->r[I->rm_reg];
        if (I->is_mem) (void)avx_memory_read(state, avx_ea(state, c, I, next, 1), &value, 1);
        destination[immediate & 15] = value;
    } else if (op == 0x22) { // PINSRD / PINSRQ
        if (I->rexW) {
            uint64_t value = c->r[I->rm_reg];
            if (I->is_mem) (void)avx_memory_read(state, avx_ea(state, c, I, next, 8), &value, 8);
            memcpy(destination + 8 * (immediate & 1), &value, 8);
        } else {
            uint32_t value = (uint32_t)c->r[I->rm_reg];
            if (I->is_mem) (void)avx_memory_read(state, avx_ea(state, c, I, next, 4), &value, 4);
            memcpy(destination + 4 * (immediate & 3), &value, 4);
        }
    } else { // INSERTPS
        uint32_t source;
        if (I->is_mem)
            (void)avx_memory_read(state, avx_ea(state, c, I, next, 4), &source, 4);
        else
            memcpy(&source, (uint8_t *)&c->v[2 * I->rm_reg] + 4 * ((immediate >> 6) & 3), 4);
        memcpy(destination + 4 * ((immediate >> 4) & 3), &source, 4);
        for (int lane = 0; lane < 4; lane++)
            if (immediate & (1 << lane)) memset(destination + 4 * lane, 0, 4);
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_integer_extension(const hl_x86_avx_state *state, struct cpu *c,
                                                               struct insn *I, uint64_t next, uint8_t destination[16]) {
    int op = I->op;
    int sign_extend = op >= 0x20 && op <= 0x25;
    int zero_extend = op >= 0x30 && op <= 0x35;
    if (I->map3 != 2 || (!sign_extend && !zero_extend)) return AVX_DISPATCH_UNMATCHED;

    uint8_t source[16];
    uint8_t result[16];
    sse_get_rm(state, c, I, next, source);
    if (sign_extend) {
        int8_t bytes[16];
        int16_t words[8];
        int32_t dwords[4];
        memcpy(bytes, source, 16);
        memcpy(words, source, 16);
        memcpy(dwords, source, 16);
        if (op == 0x20) {
            int16_t output[8];
            for (int index = 0; index < 8; index++)
                output[index] = bytes[index];
            memcpy(result, output, 16);
        } else if (op == 0x21) {
            int32_t output[4];
            for (int index = 0; index < 4; index++)
                output[index] = bytes[index];
            memcpy(result, output, 16);
        } else if (op == 0x22) {
            int64_t output[2];
            for (int index = 0; index < 2; index++)
                output[index] = bytes[index];
            memcpy(result, output, 16);
        } else if (op == 0x23) {
            int32_t output[4];
            for (int index = 0; index < 4; index++)
                output[index] = words[index];
            memcpy(result, output, 16);
        } else if (op == 0x24) {
            int64_t output[2];
            for (int index = 0; index < 2; index++)
                output[index] = words[index];
            memcpy(result, output, 16);
        } else {
            int64_t output[2];
            for (int index = 0; index < 2; index++)
                output[index] = dwords[index];
            memcpy(result, output, 16);
        }
    } else {
        uint8_t bytes[16];
        uint16_t words[8];
        uint32_t dwords[4];
        memcpy(bytes, source, 16);
        memcpy(words, source, 16);
        memcpy(dwords, source, 16);
        if (op == 0x30) {
            uint16_t output[8];
            for (int index = 0; index < 8; index++)
                output[index] = bytes[index];
            memcpy(result, output, 16);
        } else if (op == 0x31) {
            uint32_t output[4];
            for (int index = 0; index < 4; index++)
                output[index] = bytes[index];
            memcpy(result, output, 16);
        } else if (op == 0x32) {
            uint64_t output[2];
            for (int index = 0; index < 2; index++)
                output[index] = bytes[index];
            memcpy(result, output, 16);
        } else if (op == 0x33) {
            uint32_t output[4];
            for (int index = 0; index < 4; index++)
                output[index] = words[index];
            memcpy(result, output, 16);
        } else if (op == 0x34) {
            uint64_t output[2];
            for (int index = 0; index < 2; index++)
                output[index] = words[index];
            memcpy(result, output, 16);
        } else {
            uint64_t output[2];
            for (int index = 0; index < 2; index++)
                output[index] = dwords[index];
            memcpy(result, output, 16);
        }
    }
    memcpy(destination, result, 16);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_packed_arithmetic(const hl_x86_avx_state *state, struct cpu *c,
                                                               struct insn *I, uint64_t next, uint8_t destination[16]) {
    int op = I->op;
    if (I->map3 != 2 || op < 0x01 || op > 0x0B) return AVX_DISPATCH_UNMATCHED;

    uint8_t source[16];
    uint8_t result[16];
    sse_get_rm(state, c, I, next, source);
    if (op == 0x04) { // PMADDUBSW
        int16_t output[8];
        for (int lane = 0; lane < 8; lane++) {
            int product = (int)(uint8_t)destination[2 * lane] * (int)(int8_t)source[2 * lane] +
                          (int)(uint8_t)destination[2 * lane + 1] * (int)(int8_t)source[2 * lane + 1];
            output[lane] = (int16_t)sat_s16(product);
        }
        memcpy(result, output, 16);
    } else if (op <= 0x07) { // PHADD / PHSUB
        int subtract = op >= 0x05;
        int saturate = op == 0x03 || op == 0x07;
        if (op == 0x02 || op == 0x06) {
            int32_t left[4], right[4], output[4];
            memcpy(left, destination, 16);
            memcpy(right, source, 16);
            output[0] = subtract ? left[0] - left[1] : left[0] + left[1];
            output[1] = subtract ? left[2] - left[3] : left[2] + left[3];
            output[2] = subtract ? right[0] - right[1] : right[0] + right[1];
            output[3] = subtract ? right[2] - right[3] : right[2] + right[3];
            memcpy(result, output, 16);
        } else {
            int16_t left[8], right[8], output[8];
            memcpy(left, destination, 16);
            memcpy(right, source, 16);
            for (int lane = 0; lane < 4; lane++) {
                int left_value = subtract ? left[2 * lane] - left[2 * lane + 1] : left[2 * lane] + left[2 * lane + 1];
                int right_value =
                    subtract ? right[2 * lane] - right[2 * lane + 1] : right[2 * lane] + right[2 * lane + 1];
                output[lane] = saturate ? (int16_t)sat_s16(left_value) : (int16_t)left_value;
                output[lane + 4] = saturate ? (int16_t)sat_s16(right_value) : (int16_t)right_value;
            }
            memcpy(result, output, 16);
        }
    } else if (op <= 0x0A) { // PSIGNB / PSIGNW / PSIGND
        int element_width = op == 0x08 ? 1 : op == 0x09 ? 2 : 4;
        for (int offset = 0; offset < 16; offset += element_width) {
            uint64_t value = 0;
            uint64_t control = 0;
            memcpy(&value, destination + offset, (size_t)element_width);
            memcpy(&control, source + offset, (size_t)element_width);
            uint64_t output = simd_element_negative(control, element_width) ? simd_element_negate(value, element_width)
                              : control == 0                                ? 0
                                                                            : value;
            memcpy(result + offset, &output, (size_t)element_width);
        }
    } else { // PMULHRSW
        int16_t left[8], right[8], output[8];
        memcpy(left, destination, 16);
        memcpy(right, source, 16);
        for (int lane = 0; lane < 8; lane++)
            output[lane] = (int16_t)((((left[lane] * right[lane]) >> 14) + 1) >> 1);
        memcpy(result, output, 16);
    }
    memcpy(destination, result, 16);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static uint32_t aes_subword(uint32_t value) {
    return (uint32_t)k_aes_sbox[value & 0xff] | ((uint32_t)k_aes_sbox[(value >> 8) & 0xff] << 8) |
           ((uint32_t)k_aes_sbox[(value >> 16) & 0xff] << 16) | ((uint32_t)k_aes_sbox[(value >> 24) & 0xff] << 24);
}

static enum avx_dispatch_result sse_dispatch_aes(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                 uint64_t next, uint8_t destination[16]) {
    int op = I->op;
    int round = I->map3 == 2 && op >= 0xDB && op <= 0xDF;
    int keygen = I->map3 == 3 && op == 0xDF;
    if (!round && !keygen) return AVX_DISPATCH_UNMATCHED;

    uint8_t source[16];
    uint8_t result[16];
    sse_get_rm(state, c, I, next, source);
    if (keygen) {
        uint32_t words[4];
        uint32_t output[4];
        memcpy(words, source, 16);
        uint32_t subword1 = aes_subword(words[1]);
        uint32_t subword3 = aes_subword(words[3]);
        uint32_t round_constant = (uint32_t)((uint8_t)I->imm);
        output[0] = subword1;
        output[1] = rotr32(subword1, 8) ^ round_constant;
        output[2] = subword3;
        output[3] = rotr32(subword3, 8) ^ round_constant;
        memcpy(result, output, 16);
    } else if (op == 0xDB) { // AESIMC
        memcpy(result, source, 16);
        aes_mixcolumns(result, 1);
    } else {
        int inverse = op == 0xDE || op == 0xDF;
        uint8_t transformed[16];
        aes_shiftrows(destination, transformed, inverse);
        aes_subbytes(transformed, inverse ? k_aes_isbox : k_aes_sbox);
        if (op == 0xDC || op == 0xDE) aes_mixcolumns(transformed, inverse);
        for (int byte = 0; byte < 16; byte++)
            result[byte] = transformed[byte] ^ source[byte];
    }
    memcpy(destination, result, 16);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_test(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                  uint64_t next, const uint8_t destination[16]) {
    if (I->map3 != 2 || I->op != 0x17) return AVX_DISPATCH_UNMATCHED;
    uint8_t source[16];
    uint64_t destination_low, destination_high, source_low, source_high;
    sse_get_rm(state, c, I, next, source);
    memcpy(&destination_low, destination, 8);
    memcpy(&destination_high, destination + 8, 8);
    memcpy(&source_low, source, 8);
    memcpy(&source_high, source + 8, 8);
    int zero = (destination_low & source_low) == 0 && (destination_high & source_high) == 0;
    int carry = (source_low & ~destination_low) == 0 && (source_high & ~destination_high) == 0;
    c->nzcv = ((uint64_t)zero << 30) | ((uint64_t)(!carry) << 29);
    c->pf = 1;
    c->af = 0;
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_absolute(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                      uint64_t next, uint8_t destination[16]) {
    int op = I->op;
    if (I->map3 != 2 || (op != 0x1C && op != 0x1D && op != 0x1E)) return AVX_DISPATCH_UNMATCHED;
    uint8_t source[16];
    int element_width = op == 0x1C ? 1 : op == 0x1D ? 2 : 4;
    sse_get_rm(state, c, I, next, source);
    for (int offset = 0; offset < 16; offset += element_width) {
        uint64_t value = 0;
        memcpy(&value, source + offset, (size_t)element_width);
        uint64_t result =
            simd_element_negative(value, element_width) ? simd_element_negate(value, element_width) : value;
        memcpy(destination + offset, &result, (size_t)element_width);
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_minmax(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                    uint64_t next, uint8_t destination[16]) {
    int op = I->op;
    if (I->map3 != 2 || op < 0x38 || op > 0x3F) return AVX_DISPATCH_UNMATCHED;
    uint8_t source[16];
    sse_get_rm(state, c, I, next, source);
    int maximum = op >= 0x3C;
    if (op == 0x38 || op == 0x3C) {
        int8_t *left = (int8_t *)destination;
        int8_t *right = (int8_t *)source;
        for (int lane = 0; lane < 16; lane++)
            left[lane] = maximum ? (left[lane] > right[lane] ? left[lane] : right[lane])
                                 : (left[lane] < right[lane] ? left[lane] : right[lane]);
    } else if (op == 0x3A || op == 0x3E) {
        uint16_t left[8], right[8];
        memcpy(left, destination, 16);
        memcpy(right, source, 16);
        for (int lane = 0; lane < 8; lane++)
            left[lane] = maximum ? (left[lane] > right[lane] ? left[lane] : right[lane])
                                 : (left[lane] < right[lane] ? left[lane] : right[lane]);
        memcpy(destination, left, 16);
    } else if (op == 0x39 || op == 0x3D) {
        int32_t left[4], right[4];
        memcpy(left, destination, 16);
        memcpy(right, source, 16);
        for (int lane = 0; lane < 4; lane++)
            left[lane] = maximum ? (left[lane] > right[lane] ? left[lane] : right[lane])
                                 : (left[lane] < right[lane] ? left[lane] : right[lane]);
        memcpy(destination, left, 16);
    } else {
        uint32_t left[4], right[4];
        memcpy(left, destination, 16);
        memcpy(right, source, 16);
        for (int lane = 0; lane < 4; lane++)
            left[lane] = maximum ? (left[lane] > right[lane] ? left[lane] : right[lane])
                                 : (left[lane] < right[lane] ? left[lane] : right[lane]);
        memcpy(destination, left, 16);
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_implicit_blend(const hl_x86_avx_state *state, struct cpu *c,
                                                            struct insn *I, uint64_t next, uint8_t destination[16]) {
    int op = I->op;
    if (I->map3 != 2 || (op != 0x10 && op != 0x14 && op != 0x15)) return AVX_DISPATCH_UNMATCHED;
    uint8_t source[16];
    uint8_t mask[16];
    sse_get_rm(state, c, I, next, source);
    memcpy(mask, &c->v[0], 16);
    if (op == 0x10) {
        for (int lane = 0; lane < 16; lane++)
            if (mask[lane] & 0x80) destination[lane] = source[lane];
    } else {
        int lane_width = op == 0x14 ? 4 : 8;
        for (int offset = 0; offset < 16; offset += lane_width)
            if (mask[offset + lane_width - 1] & 0x80) memcpy(destination + offset, source + offset, (size_t)lane_width);
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result sse_dispatch_immediate_blend(const hl_x86_avx_state *state, struct cpu *c,
                                                             struct insn *I, uint64_t next, uint8_t destination[16]) {
    int op = I->op;
    if (I->map3 != 3 || op < 0x0C || op > 0x0E) return AVX_DISPATCH_UNMATCHED;
    uint8_t source[16];
    unsigned immediate = (uint8_t)I->imm;
    int lane_width = op == 0x0C ? 4 : op == 0x0D ? 8 : 2;
    sse_get_rm(state, c, I, next, source);
    for (int lane = 0; lane < 16 / lane_width; lane++)
        if (immediate & (1u << lane))
            memcpy(destination + lane * lane_width, source + lane * lane_width, (size_t)lane_width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static void sse_sha1_rounds4(uint8_t imm, const uint8_t state_bytes[16], const uint8_t word_bytes[16],
                             uint8_t result[16]) {
    uint32_t function = imm & 3;
    uint32_t constant = (function == 0)   ? 0x5A827999u
                        : (function == 1) ? 0x6ED9EBA1u
                        : (function == 2) ? 0x8F1BBCDCu
                                          : 0xCA62C1D6u;
    uint32_t state[4], words[4];
    memcpy(state, state_bytes, 16);
    memcpy(words, word_bytes, 16);
    uint32_t a = state[3], b = state[2], c = state[1], d = state[0];
    uint32_t schedule[4] = {words[3], words[2], words[1], words[0]};
    uint32_t e = 0;
    for (int i = 0; i < 4; i++) {
        uint32_t f = (function == 0)   ? ((b & c) | (~b & d))
                     : (function == 2) ? ((b & c) | (b & d) | (c & d))
                                       : (b ^ c ^ d);
        uint32_t next = f + rotl32(a, 5) + schedule[i] + constant + e;
        e = d;
        d = c;
        c = rotl32(b, 30);
        b = a;
        a = next;
    }
    uint32_t output[4] = {d, c, b, a};
    memcpy(result, output, 16);
}

static enum avx_dispatch_result sse_dispatch_immediate_arithmetic(int op, uint8_t imm,
                                                                  const uint8_t destination[16],
                                                                  const uint8_t source[16], uint8_t result[16]) {
    switch (op) {
    case 0x08:
    case 0x09:
    case 0x0A:
    case 0x0B: {          // roundps/pd/ss/sd, mode in imm[3:0]; bit2 set = use MXCSR.RC (current host FPCR)
        if (op == 0x08) { // roundps
            float input[4], output[4];
            memcpy(input, source, 16);
            for (int i = 0; i < 4; i++) output[i] = sse_round_f(input[i], imm);
            memcpy(result, output, 16);
        } else if (op == 0x09) { // roundpd
            double input[2], output[2];
            memcpy(input, source, 16);
            for (int i = 0; i < 2; i++) output[i] = sse_round_d(input[i], imm);
            memcpy(result, output, 16);
        } else if (op == 0x0A) { // roundss: low lane from src, rest from dst
            float input;
            memcpy(&input, source, 4);
            input = sse_round_f(input, imm);
            memcpy(result, &input, 4);
        } else { // roundsd
            double input;
            memcpy(&input, source, 8);
            input = sse_round_d(input, imm);
            memcpy(result, &input, 8);
        }
        return AVX_DISPATCH_HANDLED;
    }
    case 0x0F: { // palignr: (dst:src) >> imm8 bytes
        uint8_t combined[32];
        memcpy(combined, source, 16);
        memcpy(combined + 16, destination, 16);
        for (int i = 0; i < 16; i++) result[i] = imm < (unsigned)(32 - i) ? combined[imm + (unsigned)i] : 0;
        return AVX_DISPATCH_HANDLED;
    }
    case 0x40: { // dpps: packed-single dot product
        float left[4], right[4];
        memcpy(left, destination, 16);
        memcpy(right, source, 16);
        float sum = 0;
        for (int i = 0; i < 4; i++)
            if (imm & (0x10 << i)) sum += left[i] * right[i];
        float output[4];
        for (int i = 0; i < 4; i++) output[i] = (imm & (1 << i)) ? sum : 0.0f;
        memcpy(result, output, 16);
        return AVX_DISPATCH_HANDLED;
    }
    case 0x41: { // dppd: packed-double dot product
        double left[2], right[2];
        memcpy(left, destination, 16);
        memcpy(right, source, 16);
        double sum = 0;
        for (int i = 0; i < 2; i++)
            if (imm & (0x10 << i)) sum += left[i] * right[i];
        double output[2];
        for (int i = 0; i < 2; i++) output[i] = (imm & (1 << i)) ? sum : 0.0;
        memcpy(result, output, 16);
        return AVX_DISPATCH_HANDLED;
    }
    case 0x42: { // mpsadbw: eight 4-byte sum-of-absolute-differences windows.
        int source_offset = (imm & 3) * 4;
        int destination_offset = ((imm >> 2) & 1) * 4;
        uint16_t output[8];
        for (int i = 0; i < 8; i++) {
            int sum = 0;
            for (int k = 0; k < 4; k++) {
                int difference =
                    (int)destination[destination_offset + i + k] - (int)source[source_offset + k];
                sum += difference < 0 ? -difference : difference;
            }
            output[i] = (uint16_t)sum;
        }
        memcpy(result, output, 16);
        return AVX_DISPATCH_HANDLED;
    }
    case 0x44: { // pclmulqdq: carryless multiply of selected 64-bit halves
        uint64_t left, right;
        memcpy(&left, destination + 8 * (imm & 1), 8);
        memcpy(&right, source + 8 * ((imm >> 4) & 1), 8);
// __int128: pre-C23 GNU/clang extension needed for the PCLMULQDQ carryless product; scope -Wpedantic.
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"
        unsigned __int128 product = 0;
        for (int i = 0; i < 64; i++)
            if ((right >> i) & 1) product ^= (unsigned __int128)left << i;
#pragma GCC diagnostic pop
        memcpy(result, &product, 16);
        return AVX_DISPATCH_HANDLED;
    }
    case 0xCC: { // sha1rnds4: 4 SHA-1 rounds, function/constant from imm[1:0]
        sse_sha1_rounds4(imm, destination, source, result);
        return AVX_DISPATCH_HANDLED;
    }
    default: return AVX_DISPATCH_UNIMPLEMENTED;
    }
}

static enum avx_dispatch_result sse_dispatch_sha1_message(int op, const uint8_t destination[16],
                                                          const uint8_t source[16], uint8_t result[16]) {
    uint32_t left[4], right[4], output[4];
    memcpy(left, destination, 16);
    memcpy(right, source, 16);
    switch (op) {
    case 0xC8: // sha1nexte
        output[3] = right[3] + rotl32(left[3], 30);
        output[2] = right[2];
        output[1] = right[1];
        output[0] = right[0];
        break;
    case 0xC9: // sha1msg1
        output[3] = left[1] ^ left[3];
        output[2] = left[0] ^ left[2];
        output[1] = right[3] ^ left[1];
        output[0] = right[2] ^ left[0];
        break;
    case 0xCA: // sha1msg2
        output[3] = rotl32(left[3] ^ right[2], 1);
        output[2] = rotl32(left[2] ^ right[1], 1);
        output[1] = rotl32(left[1] ^ right[0], 1);
        output[0] = rotl32(left[0] ^ output[3], 1);
        break;
    default: return AVX_DISPATCH_UNMATCHED;
    }
    memcpy(result, output, 16);
    return AVX_DISPATCH_HANDLED;
}

static void do_sse3b(const hl_x86_avx_state *state, struct cpu *c) {
    struct insn I;
    hl_x86_decode(c->rip, &I);
    uint64_t next = c->rip + (uint64_t)I.len;
    int map = I.map3, op = I.op;
    xs_note(0, map, op, c->rip);              // EXITSTAT diagnostic (no-op unless env set)
    uint8_t *D = (uint8_t *)&c->v[2 * I.reg]; // dst xmm == src1 (destructive)
    uint8_t s[16], r[16];

    if (sse_dispatch_floating(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;
    if (sse_dispatch_crc_movbe(state, c, &I, next) == AVX_DISPATCH_HANDLED) return;
    if (sse_dispatch_lane_transfer(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;
    if (sse_dispatch_string_compare(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;

    if (sse_dispatch_test(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;

    if (sse_dispatch_integer_extension(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;
    if (sse_dispatch_packed_arithmetic(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;
    if (sse_dispatch_aes(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;
    if (sse_dispatch_absolute(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;
    if (sse_dispatch_minmax(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;
    if (sse_dispatch_implicit_blend(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;
    if (sse_dispatch_immediate_blend(state, c, &I, next, D) == AVX_DISPATCH_HANDLED) return;

    // ---- the remaining ops are xmm-destructive: load the r/m source, compute into r, write to D -----
    sse_get_rm(state, c, &I, next, s);
    memcpy(r, D, 16);

    if (map == 2) {
        if (sse_dispatch_sha1_message(op, D, s, r) == AVX_DISPATCH_HANDLED) {
            memcpy(D, r, 16);
            c->rip = next;
            return;
        }
        switch (op) {
        case 0x00: { // pshufb
            uint8_t t[16];
            memcpy(t, D, 16);
            for (int i = 0; i < 16; i++)
                r[i] = (s[i] & 0x80) ? 0 : t[s[i] & 0x0f];
            break;
        }
        case 0x28: { // pmuldq: signed (a.dword[0]*s.dword[0], a.dword[2]*s.dword[2]) -> 2 qwords
            int32_t a[4], b[4];
            int64_t o[2];
            memcpy(a, D, 16);
            memcpy(b, s, 16);
            o[0] = (int64_t)a[0] * (int64_t)b[0];
            o[1] = (int64_t)a[2] * (int64_t)b[2];
            memcpy(r, o, 16);
            break;
        }
        case 0x29: { // pcmpeqq
            uint64_t a[2], b[2], o[2];
            memcpy(a, D, 16);
            memcpy(b, s, 16);
            o[0] = (a[0] == b[0]) ? ~0ull : 0;
            o[1] = (a[1] == b[1]) ? ~0ull : 0;
            memcpy(r, o, 16);
            break;
        }
        case 0x2A: { // movntdqa (66): streaming aligned load m128 -> xmm reg (src already in s)
            memcpy(r, s, 16);
            break;
        }
        case 0x2B: { // packusdw: pack signed dword -> unsigned word (saturate); dst low, src high
            int32_t a[4], b[4];
            uint16_t o[8];
            memcpy(a, D, 16);
            memcpy(b, s, 16);
            for (int i = 0; i < 4; i++)
                o[i] = (uint16_t)sat_u16(a[i]);
            for (int i = 0; i < 4; i++)
                o[i + 4] = (uint16_t)sat_u16(b[i]);
            memcpy(r, o, 16);
            break;
        }
        case 0x37: { // pcmpgtq (signed)
            int64_t a[2], b[2];
            uint64_t o[2];
            memcpy(a, D, 16);
            memcpy(b, s, 16);
            o[0] = (a[0] > b[0]) ? ~0ull : 0;
            o[1] = (a[1] > b[1]) ? ~0ull : 0;
            memcpy(r, o, 16);
            break;
        }
        case 0x40: { // pmulld: 32-bit low product
            int32_t a[4], b[4], o[4];
            memcpy(a, D, 16);
            memcpy(b, s, 16);
            for (int i = 0; i < 4; i++)
                o[i] = a[i] * b[i];
            memcpy(r, o, 16);
            break;
        }
        case 0x41: { // phminposuw: single source r/m -> word0 = min unsigned word, word1 = its
            // (lowest) index, words2..7 = 0. dst src1(D) is ignored.
            uint16_t w[8];
            memcpy(w, s, 16);
            uint16_t best = w[0];
            int idx = 0;
            for (int i = 1; i < 8; i++)
                if (w[i] < best) {
                    best = w[i];
                    idx = i;
                }
            uint16_t o[8] = {best, (uint16_t)idx, 0, 0, 0, 0, 0, 0};
            memcpy(r, o, 16);
            break;
        }
        case 0xCB: { // sha256rnds2: dst,src, implicit xmm0 = WK0/WK1
            uint32_t st1[4], st2[4], wk[4];
            memcpy(st1, D, 16); // C0=st1[3],D0=st1[2],G0=st1[1],H0=st1[0]
            memcpy(st2, s, 16); // A0=st2[3],B0=st2[2],E0=st2[1],F0=st2[0]
            memcpy(wk, &c->v[0], 16);
            uint32_t A = st2[3], B = st2[2], Cc = st1[3], Dd = st1[2];
            uint32_t E = st2[1], F = st2[0], G = st1[1], H = st1[0];
            for (int i = 0; i < 2; i++) {
                uint32_t WK = wk[i];
                uint32_t s1 = rotr32(E, 6) ^ rotr32(E, 11) ^ rotr32(E, 25);
                uint32_t ch = (E & F) ^ (~E & G);
                uint32_t s0 = rotr32(A, 2) ^ rotr32(A, 13) ^ rotr32(A, 22);
                uint32_t maj = (A & B) ^ (A & Cc) ^ (B & Cc);
                uint32_t t1 = H + s1 + ch + WK;
                uint32_t An = t1 + s0 + maj;
                uint32_t En = t1 + Dd;
                H = G;
                G = F;
                F = E;
                E = En;
                Dd = Cc;
                Cc = B;
                B = A;
                A = An;
            }
            uint32_t o[4] = {F, E, B, A}; // DEST: [31:0]=F2,[63:32]=E2,[95:64]=B2,[127:96]=A2
            memcpy(r, o, 16);
            break;
        }
        case 0xCC: { // sha256msg1
            uint32_t w[4], w4;
            memcpy(w, D, 16);  // W0=w[0]..W3=w[3]
            memcpy(&w4, s, 4); // W4 = src[31:0]
            uint32_t in[5] = {w[0], w[1], w[2], w[3], w4};
            uint32_t o[4];
            for (int i = 0; i < 4; i++) {
                uint32_t x = in[i + 1];
                uint32_t s0 = rotr32(x, 7) ^ rotr32(x, 18) ^ (x >> 3);
                o[i] = in[i] + s0;
            }
            memcpy(r, o, 16);
            break;
        }
        case 0xCD: { // sha256msg2: W14=SRC2.dw2, W15=SRC2.dw3; W16..19 = SRC1.dw + sigma1(prev W)
            uint32_t Dw[4], sw[4], o[4];
            memcpy(Dw, D, 16);
            memcpy(sw, s, 16);
#define SHA_SIG1(x) (rotr32((x), 17) ^ rotr32((x), 19) ^ ((x) >> 10))
            uint32_t W16 = Dw[0] + SHA_SIG1(sw[2]);
            uint32_t W17 = Dw[1] + SHA_SIG1(sw[3]);
            uint32_t W18 = Dw[2] + SHA_SIG1(W16);
            uint32_t W19 = Dw[3] + SHA_SIG1(W17);
#undef SHA_SIG1
            o[0] = W16;
            o[1] = W17;
            o[2] = W18;
            o[3] = W19;
            memcpy(r, o, 16);
            break;
        }
        default: goto unimpl;
        }
        memcpy(D, r, 16);
        c->rip = next;
        return;
    }

    // ---- map == 3 (0F3A), the xmm-destructive imm8 forms ------------------------------------------
    if (sse_dispatch_immediate_arithmetic(op, (uint8_t)I.imm, D, s, r) == AVX_DISPATCH_HANDLED) {
        memcpy(D, r, 16);
        c->rip = next;
        return;
    }

unimpl:
    if (!g_avx_warned) {
        g_avx_warned = 1;
        fprintf(stderr, "[sse3b] UNIMPLEMENTED map=%d op=0x%02x p66=%d rep=%d repne=%d rip=%llx\n", map, op, I.p66,
                I.rep, I.repne, (unsigned long long)c->rip);
    }
    c->exited = 1;
    c->exit_code = 70;
}
