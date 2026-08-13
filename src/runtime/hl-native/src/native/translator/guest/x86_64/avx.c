#include "avx_internal.h"
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

void xs_note(int vex, int map, int op, uint64_t rip) {
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

uint64_t avx_ea(const hl_x86_avx_state *state, struct cpu *c, struct insn *I, uint64_t rip_after, int wbytes) {
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

int avx_memory_read(const hl_x86_avx_state *state, uint64_t guest, void *destination, size_t length) {
    if (!avx_try_read(state, guest, destination, length)) avx_abandon(guest, length, X86_SOFT_READ);
    return 1;
}

int avx_memory_write(const hl_x86_avx_state *state, uint64_t guest, const void *source, size_t length) {
    if (!avx_try_write(state, guest, source, length)) avx_abandon(guest, length, X86_SOFT_WRITE);
    return 1;
}

// Read the r/m operand (register or memory) into buf as `wbytes` bytes.
void avx_get_rm(const hl_x86_avx_state *state, struct cpu *c, struct insn *I, uint64_t rip_after, int wbytes,
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
float avx_dnan_f32(float r, float x, float y) {
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

double avx_dnan_f64(double r, double x, double y) {
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
unsigned cvt_fp_flags(void) {
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

void cvt_fp_flags_set(unsigned keep) {
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
_Static_assert(1, "conversion helpers declared");

static void cvt_raise_pe(void) {
    volatile double a = 1.0, b = 3.0, q = a / b; // 1/3 is inexact in every mode and raises only #P
    (void)q;
}

// OR an exact set of exceptions, named by MXCSR bit (SSE_XI..SSE_XP), into the host sticky state. Setting
// the bits beats synthesising each one with an arithmetic op (cvt_raise_pe's 1/3 above): no operation
// raises exactly one exception in every case, and on aarch64 no operation raises #D at all.
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

void hl_x86_sse_raise(unsigned mxcsr_bits) { sse_raise(mxcsr_bits); }

// Guest denormals-are-zero. On an x86-64 host the guest MXCSR IS the host MXCSR, so read DAZ(6) directly;
// on aarch64 the guest's FTZ|DAZ is carried by FPCR.FZ(24), which ldmxcsr set (see translate.c).
int sse_daz_active(void) {
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
int sse_is_denorm_f32(uint32_t b) {
    return (b & 0x7f800000u) == 0 && (b & 0x007fffffu) != 0;
}

int sse_is_denorm_f64(uint64_t b) {
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

float avx_fp_arith_f32(int op, float x, float y) {
    switch (op) {
    case 0x58: return avx_dnan_f32(x + y, x, y);
    case 0x59: return avx_dnan_f32(x * y, x, y);
    case 0x5C: return avx_dnan_f32(x - y, x, y);
    case 0x5E: return avx_dnan_f32(x / y, x, y);
    case 0x5D: return x < y ? x : y; // min: NaN/equal/+-0 -> src2 (x86-exact)
    default: return x > y ? x : y;   // 0x5F max: NaN/equal/+-0 -> src2 (x86-exact)
    }
}

double avx_fp_arith_f64(int op, double x, double y) {
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

static uint64_t simd_element_mask(int size) {
    return (UINT64_C(1) << (size * 8)) - 1u;
}

uint64_t simd_element_negate(uint64_t value, int size) {
    return (UINT64_C(0) - value) & simd_element_mask(size);
}

int simd_element_negative(uint64_t value, int size) {
    return (value & (UINT64_C(1) << (size * 8 - 1))) != 0;
}

static void do_avx(const hl_x86_avx_state *state, struct cpu *c);

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
                                                                       struct insn *instruction, uint64_t next, int map,
                                                                       int op, int destination, int data_register,
                                                                       int width) {
    if (map != 2 || (op != 0x0C && op != 0x0D && op != 0x16 && op != 0x36)) return AVX_DISPATCH_UNMATCHED;

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
                                                             struct insn *instruction, uint64_t next, int map, int op,
                                                             int destination, int width) {
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

static enum avx_dispatch_result avx_dispatch_map1_integer_comparison(const hl_x86_avx_state *state, struct cpu *c,
                                                                     struct insn *instruction, uint64_t next, int map,
                                                                     int op, int destination, int first_register,
                                                                     int width) {
    int equal = op >= 0x74 && op <= 0x76;
    int greater = op >= 0x64 && op <= 0x66;
    if (map != 1 || (!equal && !greater)) return AVX_DISPATCH_UNMATCHED;

    int element_size = (op == 0x74 || op == 0x64) ? 1 : (op == 0x75 || op == 0x65) ? 2 : 4;
    uint8_t first[64], second[64], output[64];
    avx_get(c, first_register, first);
    avx_get_rm(state, c, instruction, next, width, second);
    for (int offset = 0; offset < width; offset += element_size) {
        uint64_t left = 0, right = 0;
        memcpy(&left, first + offset, (size_t)element_size);
        memcpy(&right, second + offset, (size_t)element_size);
        int matched;
        if (equal) {
            matched = left == right;
        } else {
            uint64_t sign = UINT64_C(1) << (element_size * 8 - 1);
            int64_t signed_left = (int64_t)((left ^ sign) - sign);
            int64_t signed_right = (int64_t)((right ^ sign) - sign);
            matched = signed_left > signed_right;
        }
        uint64_t mask = matched ? UINT64_MAX : 0;
        memcpy(output + offset, &mask, (size_t)element_size);
    }
    avx_put(c, destination, output, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_saturating_pack(const hl_x86_avx_state *state, struct cpu *c,
                                                             struct insn *instruction, uint64_t next, int map, int op,
                                                             int destination, int first_register, int width) {
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
                                                                       struct insn *instruction, uint64_t next, int map,
                                                                       int op, int prefix, int destination,
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
            for (int lane = 0; lane < width / 4; lane++)
                output[lane] = sse_round_f(input[lane], immediate);
            memcpy(result, output, (size_t)width);
        } else {
            double input[8], output[8];
            memcpy(input, right, (size_t)width);
            for (int lane = 0; lane < width / 8; lane++)
                output[lane] = sse_round_d(input[lane], immediate);
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
    if (op != 0x58 && op != 0x59 && op != 0x5C && op != 0x5D && op != 0x5E && op != 0x5F) return AVX_DISPATCH_UNMATCHED;
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

static enum avx_dispatch_result avx_dispatch_map1_scalar_shift(const hl_x86_avx_state *state, struct cpu *c,
                                                               struct insn *instruction, uint64_t next, int map,
                                                               int width) {
    int op = instruction->op;
    int supported =
        op == 0xD1 || op == 0xD2 || op == 0xD3 || op == 0xE1 || op == 0xE2 || op == 0xF1 || op == 0xF2 || op == 0xF3;
    if (map != 1 || !supported) return AVX_DISPATCH_UNMATCHED;
    int element = (op == 0xD1 || op == 0xE1 || op == 0xF1) ? 2 : (op == 0xD2 || op == 0xE2 || op == 0xF2) ? 4 : 8;
    int arithmetic = op == 0xE1 || op == 0xE2;
    int left = op == 0xF1 || op == 0xF2 || op == 0xF3;
    uint8_t source[64], count_source[64], result[64];
    avx_get(c, instruction->vvvv, source);
    avx_get_rm(state, c, instruction, next, 16, count_source);
    uint64_t count;
    memcpy(&count, count_source, 8);
    int bits = element * 8;
    for (int offset = 0; offset < width; offset += element) {
        uint64_t value = 0;
        memcpy(&value, source + offset, (size_t)element);
        uint64_t shifted;
        if (count >= (uint64_t)bits) {
            if (arithmetic) {
                int64_t sign_shift = 64 - bits;
                shifted = (uint64_t)(((int64_t)(value << sign_shift) >> sign_shift) < 0 ? ~0ull : 0ull);
            } else {
                shifted = 0;
            }
        } else if (left) {
            shifted = value << count;
        } else if (arithmetic) {
            int64_t sign_shift = 64 - bits;
            int64_t signed_value = ((int64_t)(value << sign_shift)) >> sign_shift;
            shifted = (uint64_t)(signed_value >> count);
        } else {
            shifted = value >> count;
        }
        memcpy(result + offset, &shifted, (size_t)element);
    }
    avx_put(c, instruction->reg, result, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map1_immediate_shift(struct cpu *c, struct insn *instruction,
                                                                  uint64_t next, int map, int width) {
    int op = instruction->op;
    if (map != 1 || (op != 0x71 && op != 0x72 && op != 0x73)) return AVX_DISPATCH_UNMATCHED;
    int extension = instruction->reg;
    int immediate = (uint8_t)instruction->imm;
    int element = op == 0x71 ? 2 : op == 0x72 ? 4 : 8;
    uint8_t source[64], result[64];
    avx_get(c, instruction->rm_reg, source);
    if (op == 0x73 && (extension == 3 || extension == 7)) {
        for (int lane = 0; lane < width; lane += 16)
            for (int offset = 0; offset < 16; offset++) {
                if (extension == 3)
                    result[lane + offset] = offset + immediate < 16 ? source[lane + offset + immediate] : 0;
                else
                    result[lane + offset] = offset - immediate >= 0 ? source[lane + offset - immediate] : 0;
            }
    } else {
        int left = extension == 6;
        int arithmetic = extension == 4;
        int bits = element * 8;
        for (int offset = 0; offset < width; offset += element) {
            uint64_t value = 0, shifted;
            memcpy(&value, source + offset, (size_t)element);
            if (left) {
                shifted = immediate >= bits ? 0 : value << immediate;
            } else if (arithmetic) {
                int sign_shift = 64 - bits;
                int64_t signed_value = ((int64_t)value << sign_shift) >> sign_shift;
                shifted = (uint64_t)(signed_value >> (immediate >= bits ? bits - 1 : immediate));
            } else {
                shifted = immediate >= bits ? 0 : value >> immediate;
            }
            memcpy(result + offset, &shifted, (size_t)element);
        }
    }
    avx_put(c, instruction->vvvv, result, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_packed_numeric_conversion(const hl_x86_avx_state *state, struct cpu *c,
                                                                       struct insn *instruction, uint64_t next, int map,
                                                                       int op, int destination, int width) {
    if (map != 1 || (op != 0x5B && op != 0xE6)) return AVX_DISPATCH_UNMATCHED;
    int prefix = instruction->vex_pp;
    uint8_t source[64], output[64];
    if (op == 0x5B) {
        avx_get_rm(state, c, instruction, next, width, source);
        for (int offset = 0; offset < width; offset += 4) {
            if (prefix == 0) {
                int32_t value;
                memcpy(&value, source + offset, 4);
                float converted = (float)value;
                memcpy(output + offset, &converted, 4);
            } else {
                float value;
                memcpy(&value, source + offset, 4);
                int32_t converted = (int32_t)cvt_x86_f2i(value, prefix == 2, 0);
                memcpy(output + offset, &converted, 4);
            }
        }
        avx_put(c, destination, output, width);
    } else if (prefix == 2) {
        avx_get_rm(state, c, instruction, next, width / 2, source);
        for (int index = 0; index < width / 8; index++) {
            int32_t value;
            memcpy(&value, source + 4 * index, 4);
            double converted = (double)value;
            memcpy(output + 8 * index, &converted, 8);
        }
        avx_put(c, destination, output, width);
    } else {
        avx_get_rm(state, c, instruction, next, width, source);
        for (int index = 0; index < width / 8; index++) {
            double value;
            memcpy(&value, source + 8 * index, 8);
            int32_t converted = (int32_t)cvt_x86_d2i(value, prefix == 1, 0);
            memcpy(output + 4 * index, &converted, 4);
        }
        avx_put(c, destination, output, width / 2);
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map1_immediate_shuffle(const hl_x86_avx_state *state, struct cpu *c,
                                                                    struct insn *instruction, uint64_t next, int map,
                                                                    int width) {
    int op = instruction->op;
    if (map != 1 || (op != 0xC6 && op != 0x70)) return AVX_DISPATCH_UNMATCHED;
    int immediate = (uint8_t)instruction->imm;
    int prefix = instruction->vex_pp;
    uint8_t left[64], right[64], result[64];
    if (op == 0xC6) {
        avx_get(c, instruction->vvvv, left);
        avx_get_rm(state, c, instruction, next, width, right);
        if (prefix == 1) {
            for (int qword = 0; qword < width / 8; qword++) {
                const uint8_t *source = (qword & 1) ? right : left;
                int lane = (qword / 2) * 16;
                memcpy(result + qword * 8, source + lane + (((immediate >> qword) & 1) ? 8 : 0), 8);
            }
        } else {
            for (int lane = 0; lane < width; lane += 16) {
                memcpy(result + lane, left + lane + 4 * (immediate & 3), 4);
                memcpy(result + lane + 4, left + lane + 4 * ((immediate >> 2) & 3), 4);
                memcpy(result + lane + 8, right + lane + 4 * ((immediate >> 4) & 3), 4);
                memcpy(result + lane + 12, right + lane + 4 * ((immediate >> 6) & 3), 4);
            }
        }
    } else {
        avx_get_rm(state, c, instruction, next, width, right);
        for (int lane = 0; lane < width; lane += 16) {
            if (prefix == 1) {
                for (int dword = 0; dword < 4; dword++) {
                    int selected = (immediate >> (2 * dword)) & 3;
                    memcpy(result + lane + 4 * dword, right + lane + 4 * selected, 4);
                }
            } else {
                memcpy(result + lane, right + lane, 16);
                int base = prefix == 3 ? 8 : 0;
                for (int word = 0; word < 4; word++) {
                    int selected = (immediate >> (2 * word)) & 3;
                    memcpy(result + lane + base + 2 * word, right + lane + base + 2 * selected, 2);
                }
            }
        }
    }
    avx_put(c, instruction->reg, result, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map2_horizontal_integer(const hl_x86_avx_state *state, struct cpu *c,
                                                                     struct insn *instruction, uint64_t next, int map,
                                                                     int width) {
    int op = instruction->op;
    int supported = op == 0x01 || op == 0x02 || op == 0x03 || op == 0x05 || op == 0x06 || op == 0x07;
    if (map != 2 || !supported) return AVX_DISPATCH_UNMATCHED;
    uint8_t left[64], right[64], result[64];
    avx_get(c, instruction->vvvv, left);
    avx_get_rm(state, c, instruction, next, width, right);
    int subtract = op >= 0x05;
    int saturate = op == 0x03 || op == 0x07;
    int dword = op == 0x02 || op == 0x06;
    for (int lane = 0; lane < width; lane += 16) {
        if (dword) {
            int32_t x[4], y[4], output[4];
            memcpy(x, left + lane, 16);
            memcpy(y, right + lane, 16);
            output[0] = subtract ? x[0] - x[1] : x[0] + x[1];
            output[1] = subtract ? x[2] - x[3] : x[2] + x[3];
            output[2] = subtract ? y[0] - y[1] : y[0] + y[1];
            output[3] = subtract ? y[2] - y[3] : y[2] + y[3];
            memcpy(result + lane, output, 16);
        } else {
            int16_t x[8], y[8], output[8];
            memcpy(x, left + lane, 16);
            memcpy(y, right + lane, 16);
            for (int pair = 0; pair < 4; pair++) {
                int left_value = subtract ? x[2 * pair] - x[2 * pair + 1] : x[2 * pair] + x[2 * pair + 1];
                int right_value = subtract ? y[2 * pair] - y[2 * pair + 1] : y[2 * pair] + y[2 * pair + 1];
                output[pair] = saturate ? (int16_t)sat_s16(left_value) : (int16_t)left_value;
                output[pair + 4] = saturate ? (int16_t)sat_s16(right_value) : (int16_t)right_value;
            }
            memcpy(result + lane, output, 16);
        }
    }
    avx_put(c, instruction->reg, result, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map2_minimum_maximum(const hl_x86_avx_state *state, struct cpu *c,
                                                                  struct insn *instruction, uint64_t next, int map,
                                                                  int op, int width) {
    if (map != 2 || op < 0x38 || op > 0x3F) return AVX_DISPATCH_UNMATCHED;
    uint8_t left[64], right[64], output[64];
    avx_get(c, instruction->vvvv, left);
    avx_get_rm(state, c, instruction, next, width, right);
    int maximum = op >= 0x3C;
    if (op == 0x38 || op == 0x3C) {
        for (int offset = 0; offset < width; offset++) {
            int8_t x = (int8_t)left[offset], y = (int8_t)right[offset];
            output[offset] = (uint8_t)(maximum ? (x > y ? x : y) : (x < y ? x : y));
        }
    } else if (op == 0x3A || op == 0x3E) {
        for (int offset = 0; offset < width; offset += 2) {
            uint16_t x, y;
            memcpy(&x, left + offset, 2);
            memcpy(&y, right + offset, 2);
            uint16_t result = maximum ? (x > y ? x : y) : (x < y ? x : y);
            memcpy(output + offset, &result, 2);
        }
    } else if (op == 0x39 || op == 0x3D) {
        for (int offset = 0; offset < width; offset += 4) {
            int32_t x, y;
            memcpy(&x, left + offset, 4);
            memcpy(&y, right + offset, 4);
            int32_t result = maximum ? (x > y ? x : y) : (x < y ? x : y);
            memcpy(output + offset, &result, 4);
        }
    } else {
        for (int offset = 0; offset < width; offset += 4) {
            uint32_t x, y;
            memcpy(&x, left + offset, 4);
            memcpy(&y, right + offset, 4);
            uint32_t result = maximum ? (x > y ? x : y) : (x < y ? x : y);
            memcpy(output + offset, &result, 4);
        }
    }
    avx_put(c, instruction->reg, output, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map2_minimum_position(const hl_x86_avx_state *state, struct cpu *c,
                                                                   struct insn *instruction, uint64_t next, int map,
                                                                   int op) {
    if (map != 2 || op != 0x41) return AVX_DISPATCH_UNMATCHED;
    uint16_t words[32];
    avx_get_rm(state, c, instruction, next, 16, (uint8_t *)words);
    uint16_t best = words[0];
    int index = 0;
    for (int candidate = 1; candidate < 8; candidate++)
        if (words[candidate] < best) {
            best = words[candidate];
            index = candidate;
        }
    uint16_t output[32] = {best, (uint16_t)index};
    avx_put(c, instruction->reg, (uint8_t *)output, 16);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map2_ssse3_arithmetic(const hl_x86_avx_state *state, struct cpu *c,
                                                                   struct insn *instruction, uint64_t next, int map,
                                                                   int width) {
    int op = instruction->op;
    int supported = op == 0x04 || (op >= 0x08 && op <= 0x0B) || (op >= 0x1C && op <= 0x1E);
    if (map != 2 || !supported) return AVX_DISPATCH_UNMATCHED;
    uint8_t left[64], right[64], result[64];
    if (op >= 0x1C) {
        avx_get_rm(state, c, instruction, next, width, right);
        int element = op == 0x1C ? 1 : op == 0x1D ? 2 : 4;
        for (int offset = 0; offset < width; offset += element) {
            uint64_t value = 0;
            memcpy(&value, right + offset, (size_t)element);
            uint64_t output = simd_element_negative(value, element) ? simd_element_negate(value, element) : value;
            memcpy(result + offset, &output, (size_t)element);
        }
    } else {
        avx_get(c, instruction->vvvv, left);
        avx_get_rm(state, c, instruction, next, width, right);
        if (op == 0x04) {
            for (int lane = 0; lane < width; lane += 16) {
                int16_t output[8];
                for (int pair = 0; pair < 8; pair++) {
                    int product = (int)(uint8_t)left[lane + 2 * pair] * (int)(int8_t)right[lane + 2 * pair] +
                                  (int)(uint8_t)left[lane + 2 * pair + 1] * (int)(int8_t)right[lane + 2 * pair + 1];
                    output[pair] = (int16_t)sat_s16(product);
                }
                memcpy(result + lane, output, 16);
            }
        } else if (op <= 0x0A) {
            int element = op == 0x08 ? 1 : op == 0x09 ? 2 : 4;
            for (int offset = 0; offset < width; offset += element) {
                uint64_t value = 0, sign = 0;
                memcpy(&value, left + offset, (size_t)element);
                memcpy(&sign, right + offset, (size_t)element);
                uint64_t output = simd_element_negative(sign, element) ? simd_element_negate(value, element)
                                  : sign == 0                          ? 0
                                                                       : value;
                memcpy(result + offset, &output, (size_t)element);
            }
        } else {
            for (int offset = 0; offset < width; offset += 2) {
                int16_t x, y;
                memcpy(&x, left + offset, 2);
                memcpy(&y, right + offset, 2);
                int16_t output = (int16_t)((((x * y) >> 14) + 1) >> 1);
                memcpy(result + offset, &output, 2);
            }
        }
    }
    avx_put(c, instruction->reg, result, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map2_gather(const hl_x86_avx_state *state, struct cpu *c,
                                                         struct insn *instruction, uint64_t next, int map, int op,
                                                         int width) {
    if (map != 2 || op < 0x90 || op > 0x93) return AVX_DISPATCH_UNMATCHED;
    if (instruction->evex) return AVX_DISPATCH_UNIMPLEMENTED;
    int destination = instruction->reg;
    int mask_register = instruction->vvvv;
    if (destination == instruction->m_index || destination == mask_register || mask_register == instruction->m_index)
        avx_undefined();
    int element_size = instruction->vex_w ? 8 : 4;
    int index_size = (op == 0x90 || op == 0x92) ? 4 : 8;
    int lane_count = index_size == 4 ? width / element_size : width / 8;
    int result_bytes = lane_count * element_size;
    uint8_t indices[64], mask[64], output[64];
    avx_get(c, instruction->m_index, indices);
    avx_get(c, mask_register, mask);
    avx_get(c, destination, output);
    uint64_t base = instruction->m_hasbase ? c->r[instruction->m_base] : 0;
    base += (uint64_t)instruction->disp;
    if (instruction->seg == 1)
        base += c->fs_base;
    else if (instruction->seg == 2)
        base += c->gs_base;
    int64_t scale = (int64_t)1 << instruction->m_scale;
    for (int lane = 0; lane < lane_count; lane++) {
        if (mask[(lane + 1) * element_size - 1] & 0x80) {
            int64_t index;
            if (index_size == 4) {
                int32_t narrow_index;
                memcpy(&narrow_index, indices + lane * 4, 4);
                index = narrow_index;
            } else {
                memcpy(&index, indices + lane * 8, 8);
            }
            uint64_t address = hl_x86_avx_address(state, base + (uint64_t)(index * scale));
            if (!avx_try_read(state, address, output + lane * element_size, (size_t)element_size)) {
                avx_put(c, destination, output, 64);
                avx_put(c, mask_register, mask, 64);
                avx_abandon(address, (uint64_t)element_size, X86_SOFT_READ);
            }
        }
        memset(mask + lane * element_size, 0, (size_t)element_size);
    }
    avx_put(c, destination, output, result_bytes);
    uint8_t zero[64] = {0};
    avx_put(c, mask_register, zero, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map3_byte_immediate(const hl_x86_avx_state *state, struct cpu *c,
                                                                 struct insn *instruction, uint64_t next, int map,
                                                                 int width) {
    if (map != 3 || (instruction->op != 0x0F && instruction->op != 0x42)) return AVX_DISPATCH_UNMATCHED;
    uint8_t left[64], right[64], output[64];
    avx_get(c, instruction->vvvv, left);
    avx_get_rm(state, c, instruction, next, width, right);
    for (int lane = 0; lane < width; lane += 16) {
        if (instruction->op == 0x0F) {
            uint8_t concatenated[32];
            memcpy(concatenated, right + lane, 16);
            memcpy(concatenated + 16, left + lane, 16);
            int shift = (uint8_t)instruction->imm;
            for (int byte = 0; byte < 16; byte++)
                output[lane + byte] = shift < 32 - byte ? concatenated[shift + byte] : 0;
        } else {
            int control = (instruction->imm >> ((lane / 16) * 3)) & 7;
            int right_offset = (control & 3) * 4;
            int left_offset = ((control >> 2) & 1) * 4;
            uint16_t sums[8];
            for (int byte = 0; byte < 8; byte++) {
                int sum = 0;
                for (int component = 0; component < 4; component++) {
                    int difference =
                        (int)left[lane + left_offset + byte + component] - (int)right[lane + right_offset + component];
                    sum += difference < 0 ? -difference : difference;
                }
                sums[byte] = (uint16_t)sum;
            }
            memcpy(output + lane, sums, 16);
        }
    }
    avx_put(c, instruction->reg, output, width);
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
    hl_x86_sse_execute(state, c);
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
    enum avx_dispatch_result gather = avx_dispatch_map2_gather(state, c, &I, next, map, op, W);
    if (gather == AVX_DISPATCH_HANDLED) return;
    if (gather == AVX_DISPATCH_UNIMPLEMENTED) goto avx_unimpl;
    if (avx_dispatch_map3_byte_immediate(state, c, &I, next, map, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_fma(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_crypto(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_memory(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_widen(state, c, &I, next, map, op, rd, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_variable_permutation(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_variable_shift(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_test(state, c, &I, next, map, op, rd, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_minimum_maximum(state, c, &I, next, map, op, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_minimum_position(state, c, &I, next, map, op) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_fp16_conversion(state, c, &I, next, map, op, rd, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_immediate_blend(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_immediate_permutation(state, c, &I, next, map, op, rd, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_qword_comparison(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map1_integer_comparison(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_saturating_pack(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_packed_low_multiply(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map1_sign_mask(state, c, &I, next, map, op, pp, rd, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map1_unpack(state, c, &I, next, map, op, pp, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map1_bitwise(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map1_horizontal_floating(state, c, &I, next, map, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map1_scalar_shift(state, c, &I, next, map, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map1_immediate_shift(c, &I, next, map, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_packed_numeric_conversion(state, c, &I, next, map, op, rd, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map1_immediate_shuffle(state, c, &I, next, map, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_horizontal_integer(state, c, &I, next, map, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_ssse3_arithmetic(state, c, &I, next, map, W) == AVX_DISPATCH_HANDLED) return;
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
        }
    }
    // ---- map 3 (0F3A) ----
    if (map == 3) {
        if (avx_dispatch_lane_transfer(state, c, &I, next) == AVX_DISPATCH_HANDLED) goto done;
        if (avx_dispatch_lane_permutation(state, c, &I, next, W) == AVX_DISPATCH_HANDLED) goto done;
        if (avx_dispatch_immediate_floating(state, c, &I, next, W) == AVX_DISPATCH_HANDLED) goto done;
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
