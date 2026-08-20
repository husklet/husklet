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

void avx_get(struct cpu *c, int r, uint8_t out[64]) {
    if (r < 16) {
        memcpy(out + 0, &c->v[2 * r], 16);
        memcpy(out + 16, &c->vhi[2 * r], 16);
        memcpy(out + 32, &c->vz[4 * r], 32);
    } else {
        memcpy(out, &c->vx[8 * (r - 16)], 64);
    }
}

void avx_put(struct cpu *c, int r, const uint8_t in[64], int wbytes) {
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

void avx_abandon(uint64_t guest, uint64_t length, uint32_t required) {
    struct cpu *c = g_avx_cpu;
    c->bus_ea = guest;
    c->soft_guest_ea = guest;
    c->soft_width = length;
    c->soft_required = required;
    c->rip = g_avx_pc;
    c->reason = R_SOFTMISS;
    longjmp(g_avx_pad, 1);
}

// #UD from an emulated instruction: SIGILL/ILL_ILLOPN with si_addr = the instruction, as interp.c raises it.
void avx_undefined(void) {
    struct cpu *c = g_avx_cpu;
    c->divop = 4u | (2u << 8); // (linux_signo | si_code<<8)
    c->rip = g_avx_pc;
    c->reason = R_TRAP;
    longjmp(g_avx_pad, 1);
}

// Non-abandoning probe+transfer. 1 = done, 0 = the guest may not touch this span and NOTHING was copied.
// The gathers need this form: a faulting element must still commit the elements that completed.
int avx_try_read(const hl_x86_avx_state *state, uint64_t guest, void *destination, size_t length) {
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
    uint8_t a[64], b[64], d[64];

    enum avx_dispatch_result special = avx_dispatch_special(state, c, &I, next, map, op, pp, rd, vv, W);
    if (special == AVX_DISPATCH_HANDLED) return;
    if (special == AVX_DISPATCH_UNIMPLEMENTED) goto avx_unimpl;
    enum avx_dispatch_result vector = avx_dispatch_vector(state, c, &I, next, map, op, W);
    if (vector == AVX_DISPATCH_HANDLED) return;
    if (vector == AVX_DISPATCH_UNIMPLEMENTED) goto avx_unimpl;
    if (avx_dispatch_map2_memory(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_widen(state, c, &I, next, map, op, rd, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_variable_permutation(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_variable_shift(state, c, &I, next, map, op, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_map2_test(state, c, &I, next, map, op, rd, W) == AVX_DISPATCH_HANDLED) return;
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
    if (avx_dispatch_map1_duplicate(state, c, &I, next, map, op, pp, rd, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_scalar_integer_conversion(state, c, &I, next, map, op, pp, rd, vv) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_precision_conversion(state, c, &I, next, map, op, pp, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (avx_dispatch_root_reciprocal(state, c, &I, next, map, op, pp, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (map == 1 && avx_dispatch_map1_move(state, c, &I, next, op, pp, rd, vv, W) == AVX_DISPATCH_HANDLED) return;
    if (map == 1 && avx_dispatch_map1_floating_arithmetic(state, c, &I, next, W) == AVX_DISPATCH_HANDLED) goto done;

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
