// LEGACY SSE / SSE2 (the 0F map), implemented here because only this map has no C emulator.
//
// Scope: data movement, bitwise logic, integer SIMD. FP arithmetic is reported instead; it needs an
// authoritative MXCSR (rounding mode, DAZ/FTZ, sticky flags) that only x87state.c owns.
//
// UPPER-BITS RULE, the opposite of the AArch64 one: legacy (non-VEX) SSE writes the low 128 bits and LEAVES
// BITS 128 AND ABOVE UNTOUCHED (VEX zeroes them). vhi[]/vz[] hold the AVX upper state, so interp_xmm_put
// must never clear vhi -- `pxor %xmm0,%xmm0` must not truncate ymm0.
//
// cpu->vdirty stays untouched: it only tells the JIT's R_SYSCALL exit to spill guest xmm out of host v
// registers, and cpu->v[] here IS the register file at every instruction boundary.

static void interp_xmm_get(const struct cpu *cpu, int number, uint8_t out[16]) {
    memcpy(out, &cpu->v[2 * number], 16);
}

static void interp_xmm_put(struct cpu *cpu, int number, const uint8_t in[16]) {
    memcpy(&cpu->v[2 * number], in, 16); // low 128 bits only -- see the upper-bits rule above
}

// MM0-7 ALIAS THE LOW 64 BITS OF XMM0-7, which is what the ARM64 JIT does (guest mm and xmm both live in
// host v0..v7; translate.c's EMMS comment states it). Architecturally they alias the x87 mantissas
// instead, and that is deliberately NOT modelled: struct cpu is the checkpoint format, so an mm[8] of its
// own changes sizeof(struct cpu), and hl_x86_fxsave -- which writes cpu->st[] to the x87 area and cpu->v[]
// to the xmm area -- would then report different guest-visible bytes on the two backends. The cost is a
// guest that interleaves MMX with the matching XMM register, or reads ST(i) after MMX without FNINIT,
// which both backends get identically wrong; EMMS is a no-op in both. REX.R/REX.B do not extend an MMX
// operand, hence the mask.
static void interp_mm_get(const struct cpu *cpu, int number, uint8_t out[16]) {
    memset(out, 0, 16); // zero-extended so the 16-byte lane helpers below serve both widths
    memcpy(out, &cpu->v[2 * (number & 7)], 8);
}

static void interp_mm_put(struct cpu *cpu, int number, const uint8_t in[16]) {
    memcpy(&cpu->v[2 * (number & 7)], in, 8);
    cpu->v[2 * (number & 7) + 1] = 0; // the JIT's 64-bit NEON writes zero the host register's high half
}

// Lane accessors. memcpy, not a cast: an odd lane offset (PINSRW word 3) stays defined.
static uint16_t interp_lane16(const uint8_t *p, int index) {
    uint16_t value;
    memcpy(&value, p + 2 * index, 2);
    return value;
}

static void interp_put16(uint8_t *p, int index, uint16_t value) {
    memcpy(p + 2 * index, &value, 2);
}

static uint32_t interp_lane32(const uint8_t *p, int index) {
    uint32_t value;
    memcpy(&value, p + 4 * index, 4);
    return value;
}

static void interp_put32(uint8_t *p, int index, uint32_t value) {
    memcpy(p + 4 * index, &value, 4);
}

static uint64_t interp_lane64(const uint8_t *p, int index) {
    uint64_t value;
    memcpy(&value, p + 8 * index, 8);
    return value;
}

static void interp_put64(uint8_t *p, int index, uint64_t value) {
    memcpy(p + 8 * index, &value, 8);
}

// F2/F3 outrank 0x66, as in hardware, so these tests are ordered rather than combined.
enum { SSE_NP = 0, SSE_66 = 1, SSE_F3 = 2, SSE_F2 = 3 };

static int interp_sse_prefix(const struct insn *insn) {
    if (insn->rep) return SSE_F3;
    if (insn->repne) return SSE_F2;
    if (insn->p66) return SSE_66;
    return SSE_NP;
}

// SSE r/m operand as `bytes` bytes: 16 packed, 4/8 scalar. `out`'s tail is zeroed for low-lane callers.
static void interp_sse_rm_get(struct cpu *cpu, const struct insn *insn, uint64_t next, unsigned bytes,
                              uint8_t out[16]) {
    memset(out, 0, 16);
    if (insn->is_mem)
        interp_load_bytes(interp_ea(cpu, insn, next), out, bytes);
    else
        memcpy(out, &cpu->v[2 * insn->rm_reg], bytes);
}

static void interp_sse_rm_put(struct cpu *cpu, const struct insn *insn, uint64_t next, unsigned bytes,
                              const uint8_t in[16]) {
    if (insn->is_mem)
        interp_store_bytes(interp_ea(cpu, insn, next), in, bytes);
    else
        memcpy(&cpu->v[2 * insn->rm_reg], in, bytes); // register destination: merge, upper lanes preserved
}

// Integer-SIMD operand access at either width. Only interp_punpck and interp_pack take the width, because
// they are the cross-lane ones; every other helper is lane-local, so at MMX width it computes on the
// zero-extension and the write-back drops it.
static void interp_simd_get(const struct cpu *cpu, int mmx, int number, uint8_t out[16]) {
    if (mmx)
        interp_mm_get(cpu, number, out);
    else
        interp_xmm_get(cpu, number, out);
}

static void interp_simd_put(struct cpu *cpu, int mmx, int number, const uint8_t in[16]) {
    if (mmx)
        interp_mm_put(cpu, number, in);
    else
        interp_xmm_put(cpu, number, in);
}

static void interp_simd_rm_get(struct cpu *cpu, const struct insn *insn, int mmx, uint64_t next, uint8_t out[16]) {
    if (mmx && !insn->is_mem)
        interp_mm_get(cpu, insn->rm_reg, out);
    else
        interp_sse_rm_get(cpu, insn, next, mmx ? 8u : 16u, out);
}

static void interp_simd_rm_put(struct cpu *cpu, const struct insn *insn, int mmx, uint64_t next, const uint8_t in[16]) {
    if (mmx && !insn->is_mem)
        interp_mm_put(cpu, insn->rm_reg, in);
    else
        interp_sse_rm_put(cpu, insn, next, mmx ? 8u : 16u, in);
}

// MOVDQA/MOVAPS/MOVAPD/MOVNTDQ/MOVNTPS need a 16-byte-aligned memory operand and #GP(0) otherwise; honour
// the fault, a guest can depend on it. Linux delivers #GP as SIGSEGV/SI_KERNEL/si_addr 0.
static int interp_sse_unaligned(const struct cpu *cpu, const struct insn *insn, uint64_t next) {
    return insn->is_mem && (interp_ea(cpu, insn, next) & 15u) != 0;
}

static void interp_padd(uint8_t *d, const uint8_t *s, int lane) {
    for (int i = 0; i < 16 / lane; i++) {
        if (lane == 1)
            d[i] = (uint8_t)(d[i] + s[i]);
        else if (lane == 2)
            interp_put16(d, i, (uint16_t)(interp_lane16(d, i) + interp_lane16(s, i)));
        else if (lane == 4)
            interp_put32(d, i, interp_lane32(d, i) + interp_lane32(s, i));
        else
            interp_put64(d, i, interp_lane64(d, i) + interp_lane64(s, i));
    }
}

static void interp_psub(uint8_t *d, const uint8_t *s, int lane) {
    for (int i = 0; i < 16 / lane; i++) {
        if (lane == 1)
            d[i] = (uint8_t)(d[i] - s[i]);
        else if (lane == 2)
            interp_put16(d, i, (uint16_t)(interp_lane16(d, i) - interp_lane16(s, i)));
        else if (lane == 4)
            interp_put32(d, i, interp_lane32(d, i) - interp_lane32(s, i));
        else
            interp_put64(d, i, interp_lane64(d, i) - interp_lane64(s, i));
    }
}

static int32_t interp_sat_s8(int32_t v) {
    return v < -128 ? -128 : v > 127 ? 127 : v;
}

static int32_t interp_sat_s16(int32_t v) {
    return v < -32768 ? -32768 : v > 32767 ? 32767 : v;
}

static int32_t interp_sat_u8(int32_t v) {
    return v < 0 ? 0 : v > 255 ? 255 : v;
}

static int32_t interp_sat_u16(int32_t v) {
    return v < 0 ? 0 : v > 65535 ? 65535 : v;
}

static void interp_padds(uint8_t *d, const uint8_t *s, int lane, int subtract, int signed_form) {
    for (int i = 0; i < 16 / lane; i++) {
        if (lane == 1) {
            int32_t a = signed_form ? (int32_t)(int8_t)d[i] : (int32_t)d[i];
            int32_t b = signed_form ? (int32_t)(int8_t)s[i] : (int32_t)s[i];
            int32_t r = subtract ? a - b : a + b;
            d[i] = (uint8_t)(signed_form ? interp_sat_s8(r) : interp_sat_u8(r));
        } else {
            int32_t a = signed_form ? (int32_t)(int16_t)interp_lane16(d, i) : (int32_t)interp_lane16(d, i);
            int32_t b = signed_form ? (int32_t)(int16_t)interp_lane16(s, i) : (int32_t)interp_lane16(s, i);
            int32_t r = subtract ? a - b : a + b;
            interp_put16(d, i, (uint16_t)(signed_form ? interp_sat_s16(r) : interp_sat_u16(r)));
        }
    }
}

static void interp_pcmpeq(uint8_t *d, const uint8_t *s, int lane) {
    for (int i = 0; i < 16 / lane; i++) {
        if (lane == 1)
            d[i] = d[i] == s[i] ? 0xff : 0x00;
        else if (lane == 2)
            interp_put16(d, i, interp_lane16(d, i) == interp_lane16(s, i) ? 0xffffu : 0);
        else
            interp_put32(d, i, interp_lane32(d, i) == interp_lane32(s, i) ? 0xffffffffu : 0);
    }
}

// PCMPGT is SIGNED at every lane width; unsigned has no SSE2 encoding (string code uses PMINUB/PCMPEQB).
static void interp_pcmpgt(uint8_t *d, const uint8_t *s, int lane) {
    for (int i = 0; i < 16 / lane; i++) {
        if (lane == 1)
            d[i] = (int8_t)d[i] > (int8_t)s[i] ? 0xff : 0x00;
        else if (lane == 2)
            interp_put16(d, i, (int16_t)interp_lane16(d, i) > (int16_t)interp_lane16(s, i) ? 0xffffu : 0);
        else
            interp_put32(d, i, (int32_t)interp_lane32(d, i) > (int32_t)interp_lane32(s, i) ? 0xffffffffu : 0);
    }
}

// Per-lane shifts. A count >= the lane width gives zero (or a full sign fill), not a modulo -- x86's rule.
static void interp_pshift(uint8_t *d, int lane, unsigned count, int direction, int arithmetic) {
    unsigned bits = (unsigned)lane * 8u;
    for (int i = 0; i < 16 / lane; i++) {
        uint64_t value = lane == 2 ? interp_lane16(d, i) : lane == 4 ? interp_lane32(d, i) : interp_lane64(d, i);
        uint64_t result;
        if (arithmetic) {
            int64_t signed_value = lane == 2 ? (int64_t)(int16_t)value : (int64_t)(int32_t)value;
            result = (uint64_t)(signed_value >> (count >= bits ? bits - 1 : count));
        } else if (count >= bits) {
            result = 0;
        } else {
            result = direction ? (value >> count) : (value << count);
        }
        if (lane == 2)
            interp_put16(d, i, (uint16_t)result);
        else if (lane == 4)
            interp_put32(d, i, (uint32_t)result);
        else
            interp_put64(d, i, result);
    }
}

// PSLLDQ / PSRLDQ shift the WHOLE register by a BYTE count, not per-lane bits.
static void interp_pshift_bytes(uint8_t *d, unsigned count, int right) {
    uint8_t out[16] = {0};
    if (count < 16) {
        if (right)
            memcpy(out, d + count, 16 - count);
        else
            memcpy(out + count, d, 16 - count);
    }
    memcpy(d, out, 16);
}

// PUNPCK*: the destination lane comes first in each pair, which makes PUNPCKLBW with itself a broadcast.
static void interp_punpck(uint8_t *d, const uint8_t *s, int lane, int high, int bytes) {
    uint8_t out[16];
    int lanes = bytes / lane;
    int base = high ? lanes / 2 : 0;
    for (int i = 0; i < lanes / 2; i++) {
        memcpy(out + (2 * i) * lane, d + (base + i) * lane, (size_t)lane);
        memcpy(out + (2 * i + 1) * lane, s + (base + i) * lane, (size_t)lane);
    }
    memcpy(d, out, (size_t)bytes);
}

// PACKSSWB / PACKUSWB / PACKSSDW: narrow with saturation, destination lanes then source lanes. `per` is
// bytes/source_lane, NOT 8: with a constant 8 the result's high half was left UNINITIALISED (stack garbage).
static void interp_pack(uint8_t *d, const uint8_t *s, int source_lane, int signed_result, int bytes) {
    uint8_t out[16];
    int per = bytes / source_lane;
    for (int i = 0; i < per; i++) {
        if (source_lane == 2) {
            int32_t a = (int32_t)(int16_t)interp_lane16(d, i);
            int32_t b = (int32_t)(int16_t)interp_lane16(s, i);
            out[i] = (uint8_t)(signed_result ? interp_sat_s8(a) : interp_sat_u8(a));
            out[per + i] = (uint8_t)(signed_result ? interp_sat_s8(b) : interp_sat_u8(b));
        } else {
            int32_t a = (int32_t)interp_lane32(d, i);
            int32_t b = (int32_t)interp_lane32(s, i);
            interp_put16(out, i, (uint16_t)interp_sat_s16(a));
            interp_put16(out, per + i, (uint16_t)interp_sat_s16(b));
        }
    }
    memcpy(d, out, (size_t)bytes);
}

// STEP_SSE_UNHANDLED: not in the 0F SSE space; interp_step_two_byte diagnoses it.
enum { STEP_SSE_UNHANDLED = -1 };

static int interp_sse_is_float_arithmetic(uint8_t op) {
    if (op >= 0x58 && op <= 0x5F) return 1;                 // add/mul/cvt/sub/min/div/max
    if (op == 0x51 || op == 0x52 || op == 0x53) return 1;   // sqrt / rsqrt / rcp
    if (op == 0x2A || (op >= 0x2C && op <= 0x2F)) return 1; // cvtsi2s* / cvt*2si / ucomis* / comis*
    if (op == 0x5A || op == 0x5B) return 1;                 // cvtps2pd / cvtdq2ps
    if (op == 0xC2) return 1;                               // cmpps/cmppd/cmpss/cmpsd
    if (op == 0xE6) return 1;                               // cvtdq2pd / cvtpd2dq / cvttpd2dq
    if (op == 0x7C || op == 0x7D) return 1;                 // SSE3 hadd / hsub
    if (op == 0xD0) return 1;                               // SSE3 addsub
    return 0;
}

// SSE / SSE2 FLOATING POINT.
//
// There is no cpu->mxcsr: the guest MXCSR IS the host FP control register, because the host SSE unit
// executes the guest's FP (AArch64 projects onto FPCR/FPSR; see translate.c). So set nothing per operation
// -- run each guest instruction as the matching HOST instruction and let RC, DAZ/FZ, the sticky flags,
// x86's NaN generation and selection, MIN/MAX's non-IEEE ties and denormals come from hardware. Hence the
// _mm_* intrinsics (fixed operand order), not C `a + b`, which GCC may commute. Cost: engine FP between
// guest instructions can OR in a sticky flag the guest never raised.
//
// UPPER LANES: every scalar form MERGES, writing only the low 32 (SS) or 64 (SD) bits. _mm_*_ss/_sd have
// that shape; the one-argument unary intrinsics do not, hence `_mm_move_ss(a, _mm_sqrt_ss(b))`.

#if defined(HL_HOST_CPU_X86_64)

static int interp_fp_is_double(int prefix) {
    return prefix == SSE_66 || prefix == SSE_F2; // PD, SD
}

static int interp_fp_is_scalar(int prefix) {
    return prefix == SSE_F3 || prefix == SSE_F2; // SS, SD
}

// Bytes read from a MEMORY r/m operand: too many faults at the end of a mapping, too few substitutes zeros
// for real lanes. Trap: "F2/F3 means scalar" is the rule for the ARITHMETIC BLOCK ONLY, hence the list.
static unsigned interp_fp_source_bytes(uint8_t op, int prefix) {
    if (op == 0x5A && prefix == SSE_NP) return 8;          // CVTPS2PD: m64, two floats
    if (op == 0xE6 && prefix == SSE_F3) return 8;          // CVTDQ2PD: m64, two int32
    if (op == 0xE6 && prefix == SSE_F2) return 16;         // CVTPD2DQ: PACKED despite the F2 prefix
    if (op == 0x5B) return 16;                             // CVTDQ2PS / CVTPS2DQ / CVTTPS2DQ: 4-lane
    if (op == 0x7C || op == 0x7D || op == 0xD0) return 16; // HADD/HSUB/ADDSUB: packed under 66 AND F2
    // The MMX conversions (no F2/F3): CVTPI2PS/PD read mm/m64, CVTP{S,T}2PI read xmm/m64, and only the
    // 66 forms of 2C/2D (CVT[T]PD2PI) read a full m128.
    if (op == 0x2A && !interp_fp_is_scalar(prefix)) return 8;
    if ((op == 0x2C || op == 0x2D) && prefix == SSE_NP) return 8;
    if (!interp_fp_is_scalar(prefix)) return 16;
    return interp_fp_is_double(prefix) ? 8u : 4u;
}

static __m128 interp_fp_get_ps(const uint8_t image[16]) {
    __m128 value;
    memcpy(&value, image, 16);
    return value;
}

static __m128d interp_fp_get_pd(const uint8_t image[16]) {
    __m128d value;
    memcpy(&value, image, 16);
    return value;
}

static __m128i interp_fp_get_dq(const uint8_t image[16]) {
    __m128i value;
    memcpy(&value, image, 16);
    return value;
}

// SPECULATION BARRIER. The host MXCSR is the guest MXCSR, so a host FP instruction the guest did not
// execute is guest-visible: GCC if-converts `truncating ? _mm_cvttps_epi32(v) : _mm_cvtps_epi32(v)` into
// BOTH instructions plus a select, and the not-taken one ORs its own #I/#P into the flags the guest is
// about to read (observed: cvtpd2dq hoisted over the CVTPS2PI branch, reinterpreting two floats as one
// double). Passing the operand through an asm volatile pins the conversion inside its arm. Only the
// conversions need this -- an arm chosen by the PREFIX (0x5A/0x5B/0xE6) is a separate basic block already.
static __m128 interp_fp_opaque_ps(__m128 value) {
    __asm__ volatile("" : "+x"(value));
    return value;
}

static __m128d interp_fp_opaque_pd(__m128d value) {
    __asm__ volatile("" : "+x"(value));
    return value;
}

static void interp_fp_put_ps(uint8_t image[16], __m128 value) {
    memcpy(image, &value, 16);
}

static void interp_fp_put_pd(uint8_t image[16], __m128d value) {
    memcpy(image, &value, 16);
}

static void interp_fp_put_dq(uint8_t image[16], __m128i value) {
    memcpy(image, &value, 16);
}

// SCALAR-VS-PACKED IS THE SAME TRAP ONE LEVEL UP, and the barrier above does not cover it: `scalar ?
// _mm_min_ss(a, b) : _mm_min_ps(a, b)` is if-converted into BOTH instructions plus a select, so every
// SCALAR SSE arithmetic instruction also ran its packed twin over the destination's upper lanes and OR-ed
// their exceptions into the guest MXCSR. Measured on hardware: `MINSS %xmm0,%xmm0` with a QNaN in lane 3
// raised #I here and nothing on silicon. Writing the host instruction as asm is the only form that
// guarantees exactly one of them executes; the mnemonic IS the guest opcode, so there is nothing to keep
// in step. Operand order is AT&T: %1 = source, %0 = destination, which is also the merge target.
#define INTERP_FP_BIN(mnemonic) __asm__ volatile(mnemonic " %1,%0" : "+x"(a) : "x"(b))
#define INTERP_FP_CMP(scalar_mnemonic, packed_mnemonic, predicate)                                                     \
    do {                                                                                                               \
        if (scalar)                                                                                                    \
            __asm__ volatile(scalar_mnemonic " $" #predicate ",%1,%0" : "+x"(a) : "x"(b));                             \
        else                                                                                                           \
            __asm__ volatile(packed_mnemonic " $" #predicate ",%1,%0" : "+x"(a) : "x"(b));                             \
    } while (0)

// CMPPS/CMPSS predicate (imm8[2:0]): EQ/NEQ/UNORD/ORD are QUIET, LT/LE/NLT/NLE SIGNAL #IE on a QNaN.
static __m128 interp_fp_cmp_ps(__m128 a, __m128 b, unsigned predicate, int scalar) {
    switch (predicate & 7u) {
    case 0: INTERP_FP_CMP("cmpss", "cmpps", 0); break;
    case 1: INTERP_FP_CMP("cmpss", "cmpps", 1); break;
    case 2: INTERP_FP_CMP("cmpss", "cmpps", 2); break;
    case 3: INTERP_FP_CMP("cmpss", "cmpps", 3); break;
    case 4: INTERP_FP_CMP("cmpss", "cmpps", 4); break;
    case 5: INTERP_FP_CMP("cmpss", "cmpps", 5); break;
    case 6: INTERP_FP_CMP("cmpss", "cmpps", 6); break;
    default: INTERP_FP_CMP("cmpss", "cmpps", 7); break;
    }
    return a;
}

static __m128d interp_fp_cmp_pd(__m128d a, __m128d b, unsigned predicate, int scalar) {
    switch (predicate & 7u) {
    case 0: INTERP_FP_CMP("cmpsd", "cmppd", 0); break;
    case 1: INTERP_FP_CMP("cmpsd", "cmppd", 1); break;
    case 2: INTERP_FP_CMP("cmpsd", "cmppd", 2); break;
    case 3: INTERP_FP_CMP("cmpsd", "cmppd", 3); break;
    case 4: INTERP_FP_CMP("cmpsd", "cmppd", 4); break;
    case 5: INTERP_FP_CMP("cmpsd", "cmppd", 5); break;
    case 6: INTERP_FP_CMP("cmpsd", "cmppd", 6); break;
    default: INTERP_FP_CMP("cmpsd", "cmppd", 7); break;
    }
    return a;
}

// COMIS* (0F 2F) and UCOMIS* (0F 2E) are the only SSE instructions that write EFLAGS, and differ only in
// which NaN raises #IE (UCOMIS*: signalling only). ZF/PF/CF only (greater 000, less 001, equal 100,
// unordered 111), OF/SF/AF architecturally ZERO. setcc, not PUSHFQ: `pushfq` writes into the red zone.
static void interp_fp_comis_flags(struct cpu *cpu, unsigned char zf, unsigned char pf, unsigned char cf) {
    interp_flags_nzcv(cpu, 0 /*SF*/, zf, cf, 0 /*OF*/);
    // cpu->pf is a byte whose EVEN parity is x86 PF, so PF=1 (unordered) is byte 0 and PF=0 is 1.
    cpu->pf = pf ? 0u : 1u;
    cpu->af = 0;
}

#define INTERP_FP_COMIS(cpu, mnemonic, left, right)                                                                    \
    do {                                                                                                               \
        unsigned char zf_, pf_, cf_;                                                                                   \
        __asm__ volatile(mnemonic " %[b], %[a]\n\tsetz %[z]\n\tsetp %[p]\n\tsetc %[c]"                                 \
                         : [z] "=r"(zf_), [p] "=r"(pf_), [c] "=r"(cf_)                                                 \
                         : [a] "x"(left), [b] "x"(right)                                                               \
                         : "cc");                                                                                      \
        interp_fp_comis_flags((cpu), zf_, pf_, cf_);                                                                   \
    } while (0)

static void interp_fp_arithmetic(uint8_t op, int dbl, int scalar, uint8_t d[16], const uint8_t s[16]) {
    if (dbl) {
        __m128d a = interp_fp_get_pd(d), b = interp_fp_get_pd(s);
        if (op == 0x58 && scalar)
            INTERP_FP_BIN("addsd");
        else if (op == 0x58)
            INTERP_FP_BIN("addpd");
        else if (op == 0x59 && scalar)
            INTERP_FP_BIN("mulsd");
        else if (op == 0x59)
            INTERP_FP_BIN("mulpd");
        else if (op == 0x5C && scalar)
            INTERP_FP_BIN("subsd");
        else if (op == 0x5C)
            INTERP_FP_BIN("subpd");
        else if (op == 0x5D && scalar)
            INTERP_FP_BIN("minsd");
        else if (op == 0x5D)
            INTERP_FP_BIN("minpd");
        else if (op == 0x5E && scalar)
            INTERP_FP_BIN("divsd");
        else if (op == 0x5E)
            INTERP_FP_BIN("divpd");
        else if (scalar)
            INTERP_FP_BIN("maxsd");
        else
            INTERP_FP_BIN("maxpd");
        interp_fp_put_pd(d, a);
    } else {
        __m128 a = interp_fp_get_ps(d), b = interp_fp_get_ps(s);
        if (op == 0x58 && scalar)
            INTERP_FP_BIN("addss");
        else if (op == 0x58)
            INTERP_FP_BIN("addps");
        else if (op == 0x59 && scalar)
            INTERP_FP_BIN("mulss");
        else if (op == 0x59)
            INTERP_FP_BIN("mulps");
        else if (op == 0x5C && scalar)
            INTERP_FP_BIN("subss");
        else if (op == 0x5C)
            INTERP_FP_BIN("subps");
        else if (op == 0x5D && scalar)
            INTERP_FP_BIN("minss");
        else if (op == 0x5D)
            INTERP_FP_BIN("minps");
        else if (op == 0x5E && scalar)
            INTERP_FP_BIN("divss");
        else if (op == 0x5E)
            INTERP_FP_BIN("divps");
        else if (scalar)
            INTERP_FP_BIN("maxss");
        else
            INTERP_FP_BIN("maxps");
        interp_fp_put_ps(d, a);
    }
}

static void interp_fp_mmx_convert(struct cpu *cpu, struct insn *insn, uint64_t next, uint8_t op, int dbl,
                                  unsigned source_bytes) {
    uint8_t d[16], s[16], result[16] = {0};
    int destination = insn->reg;
    if (op == 0x2A) {
        interp_simd_rm_get(cpu, insn, 1, next, s);
        interp_xmm_get(cpu, destination, d);
        if (dbl)
            interp_fp_put_pd(d, _mm_cvtepi32_pd(interp_fp_get_dq(s)));
        else {
            interp_fp_put_ps(result, _mm_cvtepi32_ps(interp_fp_get_dq(s)));
            memcpy(d, result, 8);
        }
        interp_xmm_put(cpu, destination, d);
        return;
    }
    interp_sse_rm_get(cpu, insn, next, source_bytes, s);
    if (dbl && op == 0x2C)
        interp_fp_put_dq(result, _mm_cvttpd_epi32(interp_fp_opaque_pd(interp_fp_get_pd(s))));
    else if (dbl)
        interp_fp_put_dq(result, _mm_cvtpd_epi32(interp_fp_opaque_pd(interp_fp_get_pd(s))));
    else if (op == 0x2C)
        interp_fp_put_dq(result, _mm_cvttps_epi32(interp_fp_opaque_ps(interp_fp_get_ps(s))));
    else
        interp_fp_put_dq(result, _mm_cvtps_epi32(interp_fp_opaque_ps(interp_fp_get_ps(s))));
    interp_mm_put(cpu, destination, result);
}

static void interp_fp_compare(struct cpu *cpu, struct insn *insn, uint64_t next, int dbl, int scalar,
                              unsigned source_bytes) {
    uint8_t d[16], s[16];
    interp_xmm_get(cpu, insn->reg, d);
    interp_sse_rm_get(cpu, insn, next, source_bytes, s);
    unsigned predicate = (unsigned)insn->imm & 7u;
    if (dbl)
        interp_fp_put_pd(d, interp_fp_cmp_pd(interp_fp_get_pd(d), interp_fp_get_pd(s), predicate, scalar));
    else
        interp_fp_put_ps(d, interp_fp_cmp_ps(interp_fp_get_ps(d), interp_fp_get_ps(s), predicate, scalar));
    interp_xmm_put(cpu, insn->reg, d);
}

static void interp_fp_integer_to_scalar(struct cpu *cpu, struct insn *insn, uint64_t next, int dbl) {
    uint8_t d[16];
    interp_operand operand = interp_rm(cpu, insn, next);
    int width = insn->rexW ? 8 : 4;
    uint64_t raw = interp_rm_read(cpu, insn, &operand, width);
    interp_xmm_get(cpu, insn->reg, d);
    if (dbl) {
        __m128d a = interp_fp_get_pd(d);
        interp_fp_put_pd(d, width == 8 ? _mm_cvtsi64_sd(a, (long long)raw) : _mm_cvtsi32_sd(a, (int)(uint32_t)raw));
    } else {
        __m128 a = interp_fp_get_ps(d);
        interp_fp_put_ps(d, width == 8 ? _mm_cvtsi64_ss(a, (long long)raw) : _mm_cvtsi32_ss(a, (int)(uint32_t)raw));
    }
    interp_xmm_put(cpu, insn->reg, d);
}

static int interp_step_sse_fp(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    uint8_t op = insn->op;
    int prefix = interp_sse_prefix(insn);
    int destination = insn->reg;
    int dbl = interp_fp_is_double(prefix);
    int scalar = interp_fp_is_scalar(prefix);
    unsigned source_bytes = interp_fp_source_bytes(op, prefix);
    uint8_t d[16], s[16];
    if ((op == 0x2A || op == 0x2C || op == 0x2D) && !scalar) {
        interp_fp_mmx_convert(cpu, insn, next, op, dbl, source_bytes);
        cpu->rip = next;
        return STEP_NEXT;
    }
    if ((op == 0x52 || op == 0x53) && dbl) return interp_undefined(cpu, insn, pc, "reserved (no RSQRTPD/RCPPD)");
    switch (op) {
    case 0x2A: {
        interp_fp_integer_to_scalar(cpu, insn, next, dbl);
        cpu->rip = next;
        return STEP_NEXT;
    }

    case 0x2C:
    case 0x2D: {
        int width = insn->rexW ? 8 : 4;
        int64_t value;
        interp_sse_rm_get(cpu, insn, next, source_bytes, s);
        if (dbl) {
            __m128d b = interp_fp_get_pd(s);
            if (width == 8)
                value = op == 0x2C ? _mm_cvttsd_si64(b) : _mm_cvtsd_si64(b);
            else
                value = op == 0x2C ? (int64_t)_mm_cvttsd_si32(b) : (int64_t)_mm_cvtsd_si32(b);
        } else {
            __m128 b = interp_fp_get_ps(s);
            if (width == 8)
                value = op == 0x2C ? _mm_cvttss_si64(b) : _mm_cvtss_si64(b);
            else
                value = op == 0x2C ? (int64_t)_mm_cvttss_si32(b) : (int64_t)_mm_cvtss_si32(b);
        }
        interp_reg_write(cpu, insn, destination, width, (uint64_t)value);
        cpu->rip = next;
        return STEP_NEXT;
    }

    case 0x2E:
    case 0x2F: {
        interp_xmm_get(cpu, destination, d);
        interp_sse_rm_get(cpu, insn, next, dbl ? 8u : 4u, s);
        if (dbl) {
            __m128d a = interp_fp_get_pd(d), b = interp_fp_get_pd(s);
            if (op == 0x2E)
                INTERP_FP_COMIS(cpu, "ucomisd", a, b);
            else
                INTERP_FP_COMIS(cpu, "comisd", a, b);
        } else {
            __m128 a = interp_fp_get_ps(d), b = interp_fp_get_ps(s);
            if (op == 0x2E)
                INTERP_FP_COMIS(cpu, "ucomiss", a, b);
            else
                INTERP_FP_COMIS(cpu, "comiss", a, b);
        }
        cpu->rip = next;
        return STEP_NEXT;
    }

    case 0x51:
    case 0x52:
    case 0x53: {
        interp_xmm_get(cpu, destination, d);
        interp_sse_rm_get(cpu, insn, next, source_bytes, s);
        if (dbl) {
            __m128d a = interp_fp_get_pd(d), b = interp_fp_get_pd(s);
            if (scalar)
                INTERP_FP_BIN("sqrtsd");
            else
                INTERP_FP_BIN("sqrtpd");
            interp_fp_put_pd(d, a);
        } else {
            __m128 a = interp_fp_get_ps(d), b = interp_fp_get_ps(s);
            if (op == 0x51 && scalar)
                INTERP_FP_BIN("sqrtss");
            else if (op == 0x51)
                INTERP_FP_BIN("sqrtps");
            else if (op == 0x52 && scalar)
                INTERP_FP_BIN("rsqrtss");
            else if (op == 0x52)
                INTERP_FP_BIN("rsqrtps");
            else if (scalar)
                INTERP_FP_BIN("rcpss");
            else
                INTERP_FP_BIN("rcpps");
            interp_fp_put_ps(d, a);
        }
        interp_xmm_put(cpu, destination, d);
        cpu->rip = next;
        return STEP_NEXT;
    }

    // ADD (58) MUL (59) SUB (5C) MIN (5D) DIV (5E) MAX (5F) -- singly, since 0x5A/0x5B are conversions.
    case 0x58:
    case 0x59:
    case 0x5C:
    case 0x5D:
    case 0x5E:
    case 0x5F: {
        interp_xmm_get(cpu, destination, d);
        interp_sse_rm_get(cpu, insn, next, source_bytes, s);
        interp_fp_arithmetic(op, dbl, scalar, d, s);
        interp_xmm_put(cpu, destination, d);
        cpu->rip = next;
        return STEP_NEXT;
    }

    // 0F 5A: CVTPS2PD (np) / CVTPD2PS (66) / CVTSS2SD (F3) / CVTSD2SS (F2). The packed forms write the whole
    // destination (CVTPD2PS zeroes the upper 64 bits); the scalar forms merge.
    case 0x5A: {
        interp_xmm_get(cpu, destination, d);
        interp_sse_rm_get(cpu, insn, next, source_bytes, s);
        if (prefix == SSE_NP)
            interp_fp_put_pd(d, _mm_cvtps_pd(interp_fp_get_ps(s)));
        else if (prefix == SSE_66)
            interp_fp_put_ps(d, _mm_cvtpd_ps(interp_fp_get_pd(s)));
        else if (prefix == SSE_F3)
            interp_fp_put_pd(d, _mm_cvtss_sd(interp_fp_get_pd(d), interp_fp_get_ps(s)));
        else
            interp_fp_put_ps(d, _mm_cvtsd_ss(interp_fp_get_ps(d), interp_fp_get_pd(s)));
        interp_xmm_put(cpu, destination, d);
        cpu->rip = next;
        return STEP_NEXT;
    }

    // 0F 5B: CVTDQ2PS (np) / CVTPS2DQ (66, per MXCSR.RC) / CVTTPS2DQ (F3, truncates). All 4-lane, 16 bytes.
    case 0x5B: {
        interp_sse_rm_get(cpu, insn, next, source_bytes, s);
        if (prefix == SSE_NP)
            interp_fp_put_ps(d, _mm_cvtepi32_ps(interp_fp_get_dq(s)));
        else if (prefix == SSE_66)
            interp_fp_put_dq(d, _mm_cvtps_epi32(interp_fp_get_ps(s)));
        else if (prefix == SSE_F3)
            interp_fp_put_dq(d, _mm_cvttps_epi32(interp_fp_get_ps(s)));
        else
            return interp_undefined(cpu, insn, pc, "reserved (F2 0F 5B)");
        interp_xmm_put(cpu, destination, d);
        cpu->rip = next;
        return STEP_NEXT;
    }

    // 0F E6: CVTTPD2DQ (66) / CVTDQ2PD (F3, widens the low half) / CVTPD2DQ (F2). PD->DQ ZEROes the upper.
    case 0xE6: {
        interp_sse_rm_get(cpu, insn, next, source_bytes, s);
        if (prefix == SSE_66)
            interp_fp_put_dq(d, _mm_cvttpd_epi32(interp_fp_get_pd(s)));
        else if (prefix == SSE_F3)
            interp_fp_put_pd(d, _mm_cvtepi32_pd(interp_fp_get_dq(s)));
        else if (prefix == SSE_F2)
            interp_fp_put_dq(d, _mm_cvtpd_epi32(interp_fp_get_pd(s)));
        else
            return interp_undefined(cpu, insn, pc, "reserved (0F E6 with no mandatory prefix)");
        interp_xmm_put(cpu, destination, d);
        cpu->rip = next;
        return STEP_NEXT;
    }

    // SSE3 horizontal add/sub (0F 7C/7D) and ADDSUBPS/PD (0F D0), from SSE2 shuffles rather than
    // _mm_hadd_ps: those need -msse3 and one binary ships per host OS/CPU pair, so HADDPS would SIGILL on a
    // pre-Prescott x86-64 (avx.c refuses -mf16c likewise). Exact, not approximate: the shuffles preserve
    // operand ORDER, which decides NaN selection. Prefix trap: 0x66 is PD and 0xF2 is PS, not scalar.
    case 0x7C:
    case 0x7D:
    case 0xD0: {
        int pd = prefix == SSE_66;
        if (!pd && prefix != SSE_F2) return interp_undefined(cpu, insn, pc, "reserved (SSE3 0F 7C/7D/D0 prefix)");
        interp_xmm_get(cpu, destination, d);
        interp_sse_rm_get(cpu, insn, next, 16, s);
        // The host instruction, not a blend of both halves: composing ADDSUB from a full-width SUB OR-ed
        // with a full-width ADD runs BOTH on every lane, so the guest MXCSR collects the exceptions of the
        // operation each lane did not perform (measured: ADDSUBPD raising #I that hardware does not). The
        // horizontal pair has the same problem in the other direction -- its addend order is x86's, and
        // reconstructing it from unpacks gets NaN propagation right only by accident.
        if (pd) {
            __m128d a = interp_fp_get_pd(d), b = interp_fp_get_pd(s);
            if (op == 0xD0)
                INTERP_FP_BIN("addsubpd");
            else if (op == 0x7C)
                INTERP_FP_BIN("haddpd");
            else
                INTERP_FP_BIN("hsubpd");
            interp_fp_put_pd(d, a);
        } else {
            __m128 a = interp_fp_get_ps(d), b = interp_fp_get_ps(s);
            if (op == 0xD0)
                INTERP_FP_BIN("addsubps");
            else if (op == 0x7C)
                INTERP_FP_BIN("haddps");
            else
                INTERP_FP_BIN("hsubps");
            interp_fp_put_ps(d, a);
        }
        interp_xmm_put(cpu, destination, d);
        cpu->rip = next;
        return STEP_NEXT;
    }

    case 0xC2: {
        interp_fp_compare(cpu, insn, next, dbl, scalar, source_bytes);
        cpu->rip = next;
        return STEP_NEXT;
    }

    default: break;
    }
    return interp_undefined(cpu, insn, pc, "SSE floating-point opcode");
}

#else // !HL_HOST_CPU_X86_64

// Neither AArch64 nor x86-64: no host FP unit to be authoritative, and a software MXCSR would need a new
// cpu->mxcsr field, i.e. a checkpoint-format change. Report instead of computing plausible numbers.
static int interp_step_sse_fp(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    (void)next;
    return interp_undefined(cpu, insn, pc,
                            "SSE floating point on a non-x86-64 host (needs a software MXCSR: rounding, "
                            "DAZ/FZ and the sticky exception flags)");
}

#endif
static int interp_sse_integer_arithmetic(struct cpu *cpu, struct insn *insn, uint64_t next) {
    uint8_t op = insn->op;
    int mmx = interp_sse_prefix(insn) == SSE_NP;
    int wide = mmx ? 8 : 16;
    uint8_t d[16], s[16];
    if (op == 0xDB || op == 0xDF || op == 0xEB || op == 0xEF) {
        interp_simd_get(cpu, mmx, insn->reg, d);
        interp_simd_rm_get(cpu, insn, mmx, next, s);
        for (int i = 0; i < wide; i++)
            d[i] = op == 0xDB   ? (uint8_t)(d[i] & s[i])
                   : op == 0xDF ? (uint8_t)(~d[i] & s[i])
                   : op == 0xEB ? (uint8_t)(d[i] | s[i])
                                : (uint8_t)(d[i] ^ s[i]);
    } else if (op == 0xFC || op == 0xFD || op == 0xFE || op == 0xD4 || op == 0xF8 || op == 0xF9 || op == 0xFA ||
               op == 0xFB || op == 0xEC || op == 0xED || op == 0xDC || op == 0xDD || op == 0xE8 || op == 0xE9 ||
               op == 0xD8 || op == 0xD9) {
        interp_simd_get(cpu, mmx, insn->reg, d);
        interp_simd_rm_get(cpu, insn, mmx, next, s);
        switch (op) {
        case 0xFC: interp_padd(d, s, 1); break;
        case 0xFD: interp_padd(d, s, 2); break;
        case 0xFE: interp_padd(d, s, 4); break;
        case 0xD4: interp_padd(d, s, 8); break;
        case 0xF8: interp_psub(d, s, 1); break;
        case 0xF9: interp_psub(d, s, 2); break;
        case 0xFA: interp_psub(d, s, 4); break;
        case 0xFB: interp_psub(d, s, 8); break;
        case 0xEC: interp_padds(d, s, 1, 0, 1); break;
        case 0xED: interp_padds(d, s, 2, 0, 1); break;
        case 0xDC: interp_padds(d, s, 1, 0, 0); break;
        case 0xDD: interp_padds(d, s, 2, 0, 0); break;
        case 0xE8: interp_padds(d, s, 1, 1, 1); break;
        case 0xE9: interp_padds(d, s, 2, 1, 1); break;
        case 0xD8: interp_padds(d, s, 1, 1, 0); break;
        default: interp_padds(d, s, 2, 1, 0); break;
        }
    } else if (op == 0xDA || op == 0xDE || op == 0xEA || op == 0xEE) {
        interp_simd_get(cpu, mmx, insn->reg, d);
        interp_simd_rm_get(cpu, insn, mmx, next, s);
        if (op == 0xDA || op == 0xDE) {
            for (int i = 0; i < wide; i++)
                d[i] = (op == 0xDA) == (d[i] < s[i]) ? d[i] : s[i];
        } else {
            for (int i = 0; i < wide / 2; i++) {
                int16_t a = (int16_t)interp_lane16(d, i), b = (int16_t)interp_lane16(s, i);
                interp_put16(d, i, (uint16_t)(((op == 0xEA) == (a < b)) ? a : b));
            }
        }
    } else {
        return -1;
    }
    interp_simd_put(cpu, mmx, insn->reg, d);
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_sse_integer_multiply_reduce(struct cpu *cpu, struct insn *insn, uint64_t next) {
    uint8_t op = insn->op;
    if (op != 0xD5 && op != 0xE4 && op != 0xE5 && op != 0xF4 && op != 0xF5 && op != 0xF6 && op != 0xE0 && op != 0xE3)
        return -1;
    int mmx = interp_sse_prefix(insn) == SSE_NP;
    int wide = mmx ? 8 : 16;
    uint8_t d[16], s[16], out[16] = {0};
    interp_simd_get(cpu, mmx, insn->reg, d);
    interp_simd_rm_get(cpu, insn, mmx, next, s);
    if (op == 0xD5 || op == 0xE4 || op == 0xE5) {
        for (int i = 0; i < wide / 2; i++) {
            uint32_t product =
                op == 0xE5 ? (uint32_t)(int32_t)((int16_t)interp_lane16(d, i) * (int32_t)(int16_t)interp_lane16(s, i))
                           : (uint32_t)interp_lane16(d, i) * (uint32_t)interp_lane16(s, i);
            interp_put16(d, i, (uint16_t)(op == 0xD5 ? product & 0xffff : product >> 16));
        }
    } else if (op == 0xF4) {
        for (int i = 0; i < wide / 8; i++)
            interp_put64(out, i, (uint64_t)interp_lane32(d, 2 * i) * (uint64_t)interp_lane32(s, 2 * i));
        memcpy(d, out, (size_t)wide);
    } else if (op == 0xF5) {
        for (int i = 0; i < wide / 4; i++) {
            int32_t low = (int32_t)(int16_t)interp_lane16(d, 2 * i) * (int32_t)(int16_t)interp_lane16(s, 2 * i);
            int32_t high =
                (int32_t)(int16_t)interp_lane16(d, 2 * i + 1) * (int32_t)(int16_t)interp_lane16(s, 2 * i + 1);
            interp_put32(out, i, (uint32_t)(low + high));
        }
        memcpy(d, out, (size_t)wide);
    } else if (op == 0xF6) {
        for (int half = 0; half < wide / 8; half++) {
            uint32_t total = 0;
            for (int i = 0; i < 8; i++) {
                int index = 8 * half + i;
                total += (uint32_t)(d[index] > s[index] ? d[index] - s[index] : s[index] - d[index]);
            }
            interp_put16(out, 4 * half, (uint16_t)total);
        }
        memcpy(d, out, (size_t)wide);
    } else if (op == 0xE0) {
        for (int i = 0; i < wide; i++)
            d[i] = (uint8_t)(((uint32_t)d[i] + (uint32_t)s[i] + 1u) >> 1);
    } else {
        for (int i = 0; i < wide / 2; i++) {
            uint32_t sum = (uint32_t)interp_lane16(d, i) + (uint32_t)interp_lane16(s, i) + 1u;
            interp_put16(d, i, (uint16_t)(sum >> 1));
        }
    }
    interp_simd_put(cpu, mmx, insn->reg, d);
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_sse_integer_shift(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    uint8_t op = insn->op;
    int immediate = op >= 0x71 && op <= 0x73;
    int variable =
        op == 0xD1 || op == 0xD2 || op == 0xD3 || op == 0xE1 || op == 0xE2 || op == 0xF1 || op == 0xF2 || op == 0xF3;
    if (!immediate && !variable) return -1;
    int mmx = interp_sse_prefix(insn) == SSE_NP;
    uint8_t value[16];
    if (immediate) {
        int sub = insn->reg & 7;
        unsigned count = (unsigned)(insn->imm & 0xff);
        int lane = op == 0x71 ? 2 : op == 0x72 ? 4 : 8;
        interp_simd_get(cpu, mmx, insn->rm_reg, value);
        if (op == 0x73 && !mmx && (sub == 3 || sub == 7))
            interp_pshift_bytes(value, count, sub == 3);
        else if (sub == 2)
            interp_pshift(value, lane, count, 1, 0);
        else if (sub == 4 && op != 0x73)
            interp_pshift(value, lane, count, 1, 1);
        else if (sub == 6)
            interp_pshift(value, lane, count, 0, 0);
        else
            return interp_undefined(cpu, insn, pc, "unallocated SSE shift-group sub-opcode");
        interp_simd_put(cpu, mmx, insn->rm_reg, value);
    } else {
        uint8_t count_operand[16];
        interp_simd_get(cpu, mmx, insn->reg, value);
        interp_simd_rm_get(cpu, insn, mmx, next, count_operand);
        uint64_t raw = interp_lane64(count_operand, 0);
        unsigned count = raw > 255 ? 255u : (unsigned)raw;
        int lane = (op == 0xD1 || op == 0xE1 || op == 0xF1) ? 2 : (op == 0xD2 || op == 0xE2 || op == 0xF2) ? 4 : 8;
        int arithmetic = op == 0xE1 || op == 0xE2;
        int right = arithmetic || op == 0xD1 || op == 0xD2 || op == 0xD3;
        interp_pshift(value, lane, count, right, arithmetic);
        interp_simd_put(cpu, mmx, insn->reg, value);
    }
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_sse_integer_pack_compare(struct cpu *cpu, struct insn *insn, uint64_t next) {
    uint8_t op = insn->op;
    int unpack =
        op == 0x60 || op == 0x61 || op == 0x62 || op == 0x6C || op == 0x68 || op == 0x69 || op == 0x6A || op == 0x6D;
    int pack = op == 0x63 || op == 0x67 || op == 0x6B;
    int greater = op == 0x64 || op == 0x65 || op == 0x66;
    int equal = op == 0x74 || op == 0x75 || op == 0x76;
    if (!unpack && !pack && !greater && !equal) return -1;
    int mmx = interp_sse_prefix(insn) == SSE_NP;
    int wide = mmx ? 8 : 16;
    uint8_t d[16], s[16];
    interp_simd_get(cpu, mmx, insn->reg, d);
    interp_simd_rm_get(cpu, insn, mmx, next, s);
    if (unpack) {
        int lane = (op == 0x60 || op == 0x68) ? 1 : (op == 0x61 || op == 0x69) ? 2 : (op == 0x62 || op == 0x6A) ? 4 : 8;
        int high = (op >= 0x68 && op <= 0x6A) || op == 0x6D;
        interp_punpck(d, s, lane, high, wide);
    } else if (pack) {
        interp_pack(d, s, op == 0x6B ? 4 : 2, op != 0x67, wide);
    } else if (greater) {
        interp_pcmpgt(d, s, op == 0x64 ? 1 : op == 0x65 ? 2 : 4);
    } else {
        interp_pcmpeq(d, s, op == 0x74 ? 1 : op == 0x75 ? 2 : 4);
    }
    interp_simd_put(cpu, mmx, insn->reg, d);
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_sse_permute_extract(struct cpu *cpu, struct insn *insn, uint64_t next) {
    uint8_t op = insn->op;
    if (op != 0x50 && op != 0x70 && op != 0xC4 && op != 0xC5 && op != 0xC6 && op != 0xD7) return -1;
    int prefix = interp_sse_prefix(insn);
    int mmx = prefix == SSE_NP && op != 0x50 && op != 0xC6;
    int wide = mmx ? 8 : 16;
    uint8_t d[16], s[16];
    if (op == 0x50 || op == 0xD7) {
        if (op == 0x50)
            interp_xmm_get(cpu, insn->rm_reg, s);
        else
            interp_simd_get(cpu, mmx, insn->rm_reg, s);
        uint64_t mask = 0;
        if (op == 0x50 && prefix == SSE_66) {
            for (int i = 0; i < 2; i++)
                mask |= (uint64_t)((interp_lane64(s, i) >> 63) & 1) << i;
        } else if (op == 0x50) {
            for (int i = 0; i < 4; i++)
                mask |= (uint64_t)((interp_lane32(s, i) >> 31) & 1) << i;
        } else {
            for (int i = 0; i < wide; i++)
                mask |= (uint64_t)((s[i] >> 7) & 1) << i;
        }
        interp_reg_write(cpu, insn, insn->reg, 4, mask);
    } else if (op == 0x70) {
        interp_simd_rm_get(cpu, insn, mmx, next, s);
        unsigned control = (unsigned)(insn->imm & 0xff);
        memset(d, 0, 16);
        if (prefix == SSE_66) {
            for (int i = 0; i < 4; i++)
                interp_put32(d, i, interp_lane32(s, (int)((control >> (2 * i)) & 3)));
        } else if (prefix == SSE_F3) {
            memcpy(d, s, 8);
            for (int i = 0; i < 4; i++)
                interp_put16(d, 4 + i, interp_lane16(s, 4 + (int)((control >> (2 * i)) & 3)));
        } else {
            if (prefix == SSE_F2) memcpy(d + 8, s + 8, 8);
            for (int i = 0; i < 4; i++)
                interp_put16(d, i, interp_lane16(s, (int)((control >> (2 * i)) & 3)));
        }
        interp_simd_put(cpu, mmx, insn->reg, d);
    } else if (op == 0xC4) {
        interp_operand operand = interp_rm(cpu, insn, next);
        uint64_t value = interp_rm_read(cpu, insn, &operand, 2);
        interp_simd_get(cpu, mmx, insn->reg, d);
        interp_put16(d, (int)(insn->imm & (mmx ? 3 : 7)), (uint16_t)value);
        interp_simd_put(cpu, mmx, insn->reg, d);
    } else if (op == 0xC5) {
        interp_simd_get(cpu, mmx, insn->rm_reg, s);
        interp_reg_write(cpu, insn, insn->reg, 4, interp_lane16(s, (int)(insn->imm & (mmx ? 3 : 7))));
    } else {
        unsigned control = (unsigned)(insn->imm & 0xff);
        uint8_t out[16];
        interp_xmm_get(cpu, insn->reg, d);
        interp_sse_rm_get(cpu, insn, next, 16, s);
        if (prefix == SSE_66) {
            interp_put64(out, 0, interp_lane64(d, (int)(control & 1)));
            interp_put64(out, 1, interp_lane64(s, (int)((control >> 1) & 1)));
        } else {
            interp_put32(out, 0, interp_lane32(d, (int)(control & 3)));
            interp_put32(out, 1, interp_lane32(d, (int)((control >> 2) & 3)));
            interp_put32(out, 2, interp_lane32(s, (int)((control >> 4) & 3)));
            interp_put32(out, 3, interp_lane32(s, (int)((control >> 6) & 3)));
        }
        interp_xmm_put(cpu, insn->reg, out);
    }
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_sse_xmm_binary(struct cpu *cpu, struct insn *insn, uint64_t next) {
    uint8_t op = insn->op;
    if (op != 0x14 && op != 0x15 && op != 0x54 && op != 0x55 && op != 0x56 && op != 0x57) return -1;
    uint8_t destination[16], source[16];
    interp_xmm_get(cpu, insn->reg, destination);
    interp_sse_rm_get(cpu, insn, next, 16, source);
    if (op == 0x14 || op == 0x15) {
        interp_punpck(destination, source, interp_sse_prefix(insn) == SSE_66 ? 8 : 4, op == 0x15, 16);
    } else {
        for (int i = 0; i < 16; i++)
            destination[i] = op == 0x54   ? (uint8_t)(destination[i] & source[i])
                             : op == 0x55 ? (uint8_t)(~destination[i] & source[i])
                             : op == 0x56 ? (uint8_t)(destination[i] | source[i])
                                          : (uint8_t)(destination[i] ^ source[i]);
    }
    interp_xmm_put(cpu, insn->reg, destination);
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_sse_aligned_move(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    uint8_t op = insn->op;
    if (op != 0x28 && op != 0x29 && op != 0x2B) return -1;
    if (interp_sse_unaligned(cpu, insn, next)) return interp_guest_trap(cpu, pc, 11, 128);
    uint8_t value[16];
    if (op == 0x28) {
        interp_sse_rm_get(cpu, insn, next, 16, value);
        interp_xmm_put(cpu, insn->reg, value);
    } else {
        interp_xmm_get(cpu, insn->reg, value);
        interp_sse_rm_put(cpu, insn, next, 16, value);
    }
    cpu->rip = next;
    return STEP_NEXT;
}

static int interp_step_sse(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next) {
    uint8_t op = insn->op;
    int prefix = interp_sse_prefix(insn);
    int destination = insn->reg; // the xmm destination in most forms

    // Native host FP under the guest's MXCSR.
    if (interp_sse_is_float_arithmetic(op)) return interp_step_sse_fp(cpu, insn, pc, next);
    int delegated = interp_sse_integer_arithmetic(cpu, insn, next);
    if (delegated >= 0) return delegated;
    delegated = interp_sse_integer_multiply_reduce(cpu, insn, next);
    if (delegated >= 0) return delegated;
    delegated = interp_sse_integer_shift(cpu, insn, pc, next);
    if (delegated >= 0) return delegated;
    delegated = interp_sse_integer_pack_compare(cpu, insn, next);
    if (delegated >= 0) return delegated;
    delegated = interp_sse_permute_extract(cpu, insn, next);
    if (delegated >= 0) return delegated;
    delegated = interp_sse_xmm_binary(cpu, insn, next);
    if (delegated >= 0) return delegated;
    delegated = interp_sse_aligned_move(cpu, insn, pc, next);
    if (delegated >= 0) return delegated;

    // These have both MMX (no prefix, 64-bit) and SSE2 (0x66, 128-bit xmm) encodings. The MMX half of the
    // opcode is the SAME operation at half the width, so one arm serves both: `mmx` picks the register
    // file (interp_mm_get) and `wide` the operand width. Getting `wide` wrong is not cosmetic -- a 16-byte
    // MMX store writes 8 bytes past the destination and corrupts the guest's next object.
    int integer_simd = (op >= 0x60 && op <= 0x6D) || op == 0x6E || op == 0x6F || (op >= 0x71 && op <= 0x76) ||
                       op == 0x7E || op == 0x7F || op == 0xD4 || op == 0xD5 || op == 0xD7 ||
                       (op >= 0xD1 && op <= 0xD3) || (op >= 0xD8 && op <= 0xDF) || (op >= 0xE0 && op <= 0xE5) ||
                       op == 0xE7 || (op >= 0xE8 && op <= 0xEF) || (op >= 0xF1 && op <= 0xFE) || op == 0x70 ||
                       op == 0xC4 || op == 0xC5;
    int mmx = integer_simd && prefix == SSE_NP;
    int wide = mmx ? 8 : 16;

    uint8_t d[16], s[16];

    switch (op) {
    case 0x10:
        if (prefix == SSE_F3) { // MOVSS: upper 96 bits ZEROED from memory, kept from a register
            interp_sse_rm_get(cpu, insn, next, 4, s);
            if (insn->is_mem) {
                memset(d, 0, 16);
                memcpy(d, s, 4);
            } else {
                interp_xmm_get(cpu, destination, d);
                memcpy(d, s, 4);
            }
        } else if (prefix == SSE_F2) { // MOVSD, at 64 bits
            interp_sse_rm_get(cpu, insn, next, 8, s);
            if (insn->is_mem) {
                memset(d, 0, 16);
                memcpy(d, s, 8);
            } else {
                interp_xmm_get(cpu, destination, d);
                memcpy(d, s, 8);
            }
        } else {
            interp_sse_rm_get(cpu, insn, next, 16, d); // unaligned permitted
        }
        interp_xmm_put(cpu, destination, d);
        cpu->rip = next;
        return STEP_NEXT;

    // MOVUPS/MOVUPD/MOVSS/MOVSD store
    case 0x11: {
        unsigned bytes = prefix == SSE_F3 ? 4u : prefix == SSE_F2 ? 8u : 16u;
        interp_xmm_get(cpu, destination, d);
        interp_sse_rm_put(cpu, insn, next, bytes, d);
        cpu->rip = next;
        return STEP_NEXT;
    }

    case 0x12:
    case 0x16: {
        int high = (op == 0x16);
        interp_xmm_get(cpu, destination, d);
        if (prefix == SSE_F3) {
            // Duplicate the EVEN (resp. ODD) single-precision lanes of a full m128; the F3 prefix alone
            // separates these from the MOVLPS/MOVHPS arm below, which reads 8 bytes.
            interp_sse_rm_get(cpu, insn, next, 16, s);
            for (int i = 0; i < 4; i++)
                interp_put32(d, i, interp_lane32(s, (i & ~1) | (high ? 1 : 0)));
        } else if (prefix == SSE_F2 && op == 0x12) { // MOVDDUP: low qword into both halves
            interp_sse_rm_get(cpu, insn, next, 8, s);
            memcpy(d + 0, s, 8);
            memcpy(d + 8, s, 8);
        } else if (insn->is_mem) { // MOVLPS low half, MOVHPS high half
            interp_sse_rm_get(cpu, insn, next, 8, s);
            memcpy(d + (high ? 8 : 0), s, 8);
        } else if (high) { // MOVLHPS: dest high := source LOW
            interp_xmm_get(cpu, insn->rm_reg, s);
            memcpy(d + 8, s + 0, 8);
        } else { // MOVHLPS: dest low := source HIGH
            interp_xmm_get(cpu, insn->rm_reg, s);
            memcpy(d + 0, s + 8, 8);
        }
        interp_xmm_put(cpu, destination, d);
        cpu->rip = next;
        return STEP_NEXT;
    }

    // MOVLPS/MOVLPD, MOVHPS/MOVHPD store
    case 0x13:
    case 0x17: {
        uint8_t half[16] = {0};
        interp_xmm_get(cpu, destination, d);
        memcpy(half, d + (op == 0x17 ? 8 : 0), 8);
        interp_sse_rm_put(cpu, insn, next, 8, half);
        cpu->rip = next;
        return STEP_NEXT;
    }

    // An INTEGER r/m operand; REX.W selects 64-bit; upper bits ZEROED.
    case 0x6E: {
        interp_operand operand = interp_rm(cpu, insn, next);
        int width = insn->rexW ? 8 : 4;
        uint64_t value = interp_rm_read(cpu, insn, &operand, width);
        memset(d, 0, 16);
        memcpy(d, &value, (size_t)width);
        interp_simd_put(cpu, mmx, destination, d);
        cpu->rip = next;
        return STEP_NEXT;
    }

    // MOVDQA (66) / MOVDQU (F3) / MMX MOVQ (none) load
    case 0x6F:
        if (prefix == SSE_66 && interp_sse_unaligned(cpu, insn, next)) return interp_guest_trap(cpu, pc, 11, 128);
        interp_simd_rm_get(cpu, insn, mmx, next, d);
        interp_simd_put(cpu, mmx, destination, d);
        cpu->rip = next;
        return STEP_NEXT;

    // LDDQU: MOVDQU plus a micro-architectural hint. Memory only.
    case 0xF0:
        if (prefix != SSE_F2 || !insn->is_mem) return interp_undefined(cpu, insn, pc, "reserved (0F F0)");
        interp_sse_rm_get(cpu, insn, next, 16, d);
        interp_xmm_put(cpu, destination, d);
        cpu->rip = next;
        return STEP_NEXT;

    // MOVDQA (66) / MOVDQU (F3) / MMX MOVQ (none) store
    case 0x7F:
        if (prefix == SSE_66 && interp_sse_unaligned(cpu, insn, next)) return interp_guest_trap(cpu, pc, 11, 128);
        interp_simd_get(cpu, mmx, destination, d);
        interp_simd_rm_put(cpu, insn, mmx, next, d);
        cpu->rip = next;
        return STEP_NEXT;

    case 0x7E:
        if (prefix == SSE_F3) { // MOVQ load: upper 64 bits zeroed
            interp_sse_rm_get(cpu, insn, next, 8, s);
            memset(d, 0, 16);
            memcpy(d, s, 8);
            interp_xmm_put(cpu, destination, d);
            cpu->rip = next;
            return STEP_NEXT;
        }
        if (prefix == SSE_66 || mmx) { // MOVD/MOVQ to a GPR or memory; no prefix is the MMX source form
            interp_operand operand = interp_rm(cpu, insn, next);
            int width = insn->rexW ? 8 : 4;
            uint64_t value = 0;
            interp_simd_get(cpu, mmx, destination, d);
            memcpy(&value, d, (size_t)width);
            interp_rm_write(cpu, insn, &operand, width, value);
            cpu->rip = next;
            return STEP_NEXT;
        }
        return interp_undefined(cpu, insn, pc, "reserved (F2 0F 7E)");

    // 0F D6: MOVQ xmm/m64, xmm (66) / MOVQ2DQ xmm, mm (F3) / MOVDQ2Q mm, xmm (F2). The prefix picks which
    // REGISTER FILE each side names, so ignoring it wrote the right bytes to the wrong place. F3/F2 name a
    // register-only operand (Nq / Uq) and NP 0F D6 has no encoding at all: both are #UD, verified native.
    case 0xD6: {
        uint8_t half[16] = {0};
        if (prefix == SSE_NP || (insn->is_mem && prefix != SSE_66))
            return interp_guest_trap(cpu, pc, 4 /*SIGILL*/, 2 /*ILL_ILLOPN*/);
        if (prefix == SSE_F3) { // MOVQ2DQ: xmm := mm, upper 64 bits zeroed
            interp_mm_get(cpu, insn->rm_reg, half);
            interp_xmm_put(cpu, destination, half);
            cpu->rip = next;
            return STEP_NEXT;
        }
        if (prefix == SSE_F2) { // MOVDQ2Q: mm := xmm low 64
            interp_xmm_get(cpu, insn->rm_reg, s);
            memcpy(half, s, 8);
            interp_mm_put(cpu, destination, half);
            cpu->rip = next;
            return STEP_NEXT;
        }
        interp_xmm_get(cpu, destination, d);
        memcpy(half, d, 8);
        if (insn->is_mem) {
            interp_store_bytes(interp_ea(cpu, insn, next), half, 8);
        } else {
            // MOVQ zeroes the upper 64 bits.
            interp_xmm_put(cpu, insn->rm_reg, half);
        }
        cpu->rip = next;
        return STEP_NEXT;
    }

    // MOVNTDQ (66) / MOVNTQ (none): the hint has no effect. Only the 128-bit form demands alignment.
    case 0xE7:
        if (!mmx && interp_sse_unaligned(cpu, insn, next)) return interp_guest_trap(cpu, pc, 11, 128);
        interp_simd_get(cpu, mmx, destination, d);
        interp_simd_rm_put(cpu, insn, mmx, next, d);
        cpu->rip = next;
        return STEP_NEXT;

    // MASKMOVDQU (66) / MASKMOVQ (none): a byte-granular store to DS:[RDI] selected by the per-byte high
    // bits of the MASK in r/m; the data comes from ModRM.reg. There is no explicit memory operand, so a
    // memory ModRM is #UD.
    case 0xF7: {
        if (prefix != SSE_66 && !mmx) return interp_undefined(cpu, insn, pc, "reserved (F2/F3 0F F7)");
        if (insn->is_mem) return interp_guest_trap(cpu, pc, 4 /*SIGILL*/, 2 /*ILL_ILLOPN*/);
        uint64_t address = interp_implicit_address(cpu, insn, cpu->r[RDI]);
        interp_simd_get(cpu, mmx, destination, d);
        interp_simd_get(cpu, mmx, insn->rm_reg, s);
        // Per SELECTED byte rather than a read-modify-write blend of the whole operand: an unselected byte
        // is architecturally not stored, and rewriting it would lose a concurrent store through a shared
        // mapping. Each store carries its own fault-marker bracket, which is also the architectural
        // granularity -- an unselected byte on an unmapped page must not fault.
        for (int i = 0; i < wide; i++)
            if (s[i] & 0x80) interp_store(address + (uint64_t)i, 1, d[i]);
        cpu->rip = next;
        return STEP_NEXT;
    }

    default: break;
    }
    return STEP_SSE_UNHANDLED;
}

// ---- The XSAVE family (0F AE /4 XSAVE, /5 XRSTOR, /6 XSAVEOPT). ------------------------------------
//
// WHICH COMPONENTS THIS ENGINE MODELS. XCR0 is x87|SSE and nothing else -- XGETBV above reports 3, matching
// cpuid.c's deliberately withheld AVX -- so RFBM = EDX:EAX & XCR0 can only ever name component 0 (x87) and
// component 1 (SSE). Both are exactly the 512-byte legacy region hl_x86_fxsave/hl_x86_fxrstor already own,
// there is no extended region at all, and XSTATE_BV therefore cannot claim a component that was not written.
// (CPUID leaf 0xD is not reported either: leaf 0 caps max-leaf at 7. A guest that reads the enumeration
// before using XSAVE finds no XSAVE bit in leaf 1 ECX and takes its FXSAVE path -- which is why glibc's
// dynamic linker selects _dl_runtime_resolve_fxsave here, not _dl_runtime_resolve_xsave.)
//
// THE LAYOUT IS HARDWARE'S, measured byte for byte into a 0xAA-filled buffer (AMD Zen4; the SDM agrees):
//   RFBM bit 0 (x87) writes [0,24) and [32,160)      RFBM bit 1 (SSE) writes [24,32) (MXCSR) and [160,416)
//   [416,512) is written by neither component.
//   The 64-byte header at 512 is NOT bulk-written: XSAVE read-modify-writes only the XSTATE_BV bits named
//   by RFBM and leaves XCOMP_BV and the reserved bytes alone (byte 512 went 0xAA -> 0xAB, byte 513 stayed
//   0xAA). Getting that wrong would corrupt a caller's XCOMP_BV.
#define INTERP_XCR0 UINT64_C(3)

// The legacy image as this engine's FXSAVE produces it, staged so a faulting store commits nothing.
static void interp_xsave_legacy(struct cpu *cpu, uint8_t image[512]) {
    uint64_t saved = cpu->x87_ea;
    memset(image, 0, 512);
    cpu->x87_ea = (uint64_t)(uintptr_t)image;
    hl_x86_fxsave(cpu);
    cpu->x87_ea = saved;
    uint32_t mxcsr_mask = 0xffffu;      // exactly the bits LDMXCSR keeps (case 0xAE below); hl_x86_fxsave leaves
    memcpy(image + 28, &mxcsr_mask, 4); // this field untouched, and a zero mask reads as "nothing settable"
}

// XINUSE, derived rather than tracked: a component whose state EQUALS its initial configuration is in it.
// The SDM permits either answer (an implementation may report 1 for a component that is at init), so the
// derived form is legal and additionally agrees with silicon on the case guests test -- never touched.
static uint64_t interp_xsave_xinuse(const uint8_t image[512]) {
    uint64_t inuse = 0;
    uint16_t fcw, fsw;
    uint32_t mxcsr;
    memcpy(&fcw, image + 0, 2);
    memcpy(&fsw, image + 2, 2);
    memcpy(&mxcsr, image + 24, 4);
    int x87_init = fcw == 0x037fu && fsw == 0 && image[4] == 0; // abridged tag 0 == every register empty
    for (unsigned i = 6; i < 24 && x87_init; i++)
        x87_init = image[i] == 0;
    for (unsigned i = 32; i < 160 && x87_init; i++)
        x87_init = image[i] == 0;
    int sse_init = mxcsr == 0x1f80u;
    for (unsigned i = 160; i < 416 && sse_init; i++)
        sse_init = image[i] == 0;
    if (!x87_init) inuse |= 1;
    if (!sse_init) inuse |= 2;
    return inuse;
}

// The initial configuration of each component, for XRSTOR of a header whose XSTATE_BV bit is clear.
static void interp_xsave_init(uint8_t image[512], uint64_t components) {
    if (components & 1) {
        uint16_t fcw = 0x037fu;
        memset(image + 0, 0, 24);
        memset(image + 32, 0, 128);
        memcpy(image + 0, &fcw, 2);
    }
    if (components & 2) {
        uint32_t mxcsr = 0x1f80u;
        memcpy(image + 24, &mxcsr, 4);
        memset(image + 160, 0, 256);
    }
}

// The 0F (two-byte) opcode map.

// The 0F-map SSE/SSE2 space; a residual gap then reports as "SSE", not a bare opcode byte.
