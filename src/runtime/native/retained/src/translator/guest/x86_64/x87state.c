#include "x87state.h"
#include "cpu.h"
#include "../../../host/host_cpu.h" // HL_HOST_CPU_*: MXCSR is projected onto the host FP control

#include <string.h>

#if defined(HL_HOST_CPU_X86_64)
#include <xmmintrin.h> // _mm_getcsr/_mm_setcsr == STMXCSR/LDMXCSR; baseline SSE, no -m flag needed
#endif

// FSW bit i (IE DE ZE OE UE PE) <- host FPSR bit. MXCSR already uses the FSW positions.
#if defined(HL_HOST_CPU_AARCH64)
static const unsigned g_fpsr_bit[6] = {0, 7, 1, 2, 3, 4};
#endif

unsigned hl_x87_exceptions_get(void) {
#if defined(HL_HOST_CPU_AARCH64)
    uint64_t fpsr;
    unsigned flags = 0;
    __asm__ volatile("mrs %0, fpsr" : "=r"(fpsr));
    for (unsigned bit = 0; bit < 6; ++bit)
        flags |= (unsigned)((fpsr >> g_fpsr_bit[bit]) & 1u) << bit;
    return flags;
#elif defined(HL_HOST_CPU_X86_64)
    return _mm_getcsr() & 0x3fu;
#else
    return 0;
#endif
}

void hl_x87_exceptions_set(unsigned flags) {
#if defined(HL_HOST_CPU_AARCH64)
    uint64_t fpsr;
    __asm__ volatile("mrs %0, fpsr" : "=r"(fpsr));
    fpsr &= ~UINT64_C(0x9f);
    for (unsigned bit = 0; bit < 6; ++bit)
        fpsr |= (uint64_t)((flags >> bit) & 1u) << g_fpsr_bit[bit];
    __asm__ volatile("msr fpsr, %0" : : "r"(fpsr));
#elif defined(HL_HOST_CPU_X86_64)
    _mm_setcsr((_mm_getcsr() & ~0x3fu) | (flags & 0x3fu));
#else
    (void)flags;
#endif
}

void hl_x87_exceptions_raise(unsigned flags) {
    hl_x87_exceptions_set(hl_x87_exceptions_get() | (flags & 0x3fu));
}

// #IS. C1 tells the two apart: 1 = OVERFLOW (a push onto a non-empty slot), 0 = UNDERFLOW (a read of an
// empty one). Mirrors interp_x87_stack_fault.
static void x87_stack_fault(struct cpu *cpu, unsigned overflow) {
    hl_x87_exceptions_raise(1u); // IE
    cpu->fpsw = (cpu->fpsw & ~(UINT64_C(1) << 9)) | UINT64_C(0x40) | (overflow ? UINT64_C(1) << 9 : 0);
}

double hl_x86_ext80_load(const uint8_t image[10]) {
    uint64_t significand;
    uint16_t sign_exponent;
    int sign;
    int exponent;
    double value;
    memcpy(&significand, image, sizeof(significand));
    memcpy(&sign_exponent, image + 8, sizeof(sign_exponent));
    sign = sign_exponent >> 15;
    exponent = sign_exponent & 0x7fff;
    if (significand == 0 && exponent == 0) {
        uint64_t bits = (uint64_t)sign << 63;
        memcpy(&value, &bits, sizeof(value));
    } else if (exponent == 0x7fff) {
        uint64_t fraction = significand & ((UINT64_C(1) << 63) - 1);
        uint64_t bits = (uint64_t)sign << 63 | UINT64_C(0x7ff) << 52 | (fraction != 0 ? UINT64_C(1) << 51 : 0);
        memcpy(&value, &bits, sizeof(value));
    } else {
        uint64_t bits;
        int scaled_exponent;
        value = (double)significand;
        memcpy(&bits, &value, sizeof(bits));
        scaled_exponent = (int)((bits >> 52) & 0x7ff) + exponent - 16383 - 63;
        if (scaled_exponent <= 0) {
            bits = (uint64_t)sign << 63;
        } else if (scaled_exponent >= 0x7ff) {
            bits = (uint64_t)sign << 63 | UINT64_C(0x7ff) << 52;
        } else {
            bits = (bits & ~(UINT64_C(0x7ff) << 52)) | (uint64_t)scaled_exponent << 52;
            bits = (bits & ~(UINT64_C(1) << 63)) | (uint64_t)sign << 63;
        }
        memcpy(&value, &bits, sizeof(value));
    }
    return value;
}

void hl_x86_ext80_store(double value, uint8_t image[10]) {
    uint64_t bits;
    uint64_t fraction;
    uint64_t significand;
    uint16_t sign_exponent;
    int sign;
    int exponent;
    memcpy(&bits, &value, sizeof(bits));
    sign = (int)(bits >> 63);
    exponent = (int)((bits >> 52) & 0x7ff);
    fraction = bits & ((UINT64_C(1) << 52) - 1);
    if (exponent == 0) {
        significand = 0;
        sign_exponent = (uint16_t)(sign != 0 ? 0x8000 : 0);
    } else if (exponent == 0x7ff) {
        significand = (UINT64_C(1) << 63) | (fraction << 11);
        sign_exponent = (uint16_t)((sign << 15) | 0x7fff);
    } else {
        significand = (UINT64_C(1) << 63) | (fraction << 11);
        sign_exponent = (uint16_t)((sign << 15) | ((exponent - 1023 + 16383) & 0x7fff));
    }
    memcpy(image, &significand, sizeof(significand));
    memcpy(image + 8, &sign_exponent, sizeof(sign_exponent));
}

// TOP moves must preserve cpu->fptop's tag bits (x87state.h), not just rewrite the low three.
static void x87_top_add(struct cpu *cpu, int delta) {
    cpu->fptop = (cpu->fptop & ~UINT64_C(7)) | (((cpu->fptop + (uint64_t)(int64_t)delta)) & 7);
}

// FLD m80. A push onto a non-empty slot is #IS overflow: TOP still moves and the destination is destroyed.
void hl_x86_x87_load_ext80(struct cpu *cpu) {
    double value = hl_x86_ext80_load((const uint8_t *)(uintptr_t)cpu->x87_ea);
    if (hl_x87_tags_modelled() && !(cpu->fptop & HL_X87_ARMED)) cpu->fptop |= HL_X87_ARMED | HL_X87_EMPTY_ALL;
    int overflow = hl_x87_phys_live(cpu->fptop, (int)((cpu->fptop - 1) & 7));
    x87_top_add(cpu, -1);
    if (overflow) {
        x87_stack_fault(cpu, 1);
        value = hl_x87_indefinite();
    }
    hl_x87_phys_mark(&cpu->fptop, (int)(cpu->fptop & 7), 0);
    cpu->st[cpu->fptop & 7] = value;
}

// FSTP m80. An empty ST(0) is #IS underflow: the m80 indefinite reaches memory and the pop still happens.
void hl_x86_x87_store_ext80_pop(struct cpu *cpu) {
    double value = cpu->st[cpu->fptop & 7];
    if (hl_x87_phys_empty(cpu->fptop, (int)(cpu->fptop & 7))) {
        x87_stack_fault(cpu, 0);
        value = hl_x87_indefinite();
    }
    hl_x87_phys_mark(&cpu->fptop, (int)(cpu->fptop & 7), 1);
    x87_top_add(cpu, 1);
    hl_x86_ext80_store(value, (uint8_t *)(uintptr_t)cpu->x87_ea);
}

// TOP at 13:11 and ES(7)/B(15) when a raised exception is UNMASKED per FCW. 0x4740, not 0x4700: SF (bit 6)
// is a stack fault, not an MXCSR bit, so it lives in cpu->fpsw. Must match interp_x87_status_word.
uint16_t hl_x86_x87_status_word(const struct cpu *cpu) {
    uint16_t status = (uint16_t)((cpu->fpsw & 0x4740) | ((cpu->fptop & 7) << 11));
    uint16_t raised = (uint16_t)hl_x87_exceptions_get();
    status |= raised;
    if (raised & (uint16_t)(~cpu->fpcw & 0x3f)) status |= (uint16_t)0x8080;
    return status;
}

static void x87_put32(uint8_t *area, unsigned offset, uint32_t value) {
    memcpy(area + offset, &value, 4);
}

static uint32_t x87_get16(const uint8_t *area, unsigned offset) {
    uint16_t value;
    memcpy(&value, area + offset, 2);
    return value;
}

// FCW@0, FSW@4, FTW@8, FIP@12, FCS+FOP@16, FDP@20, FDS@24 -- the 28-byte 32-bit protected-mode image.
// FNSTENV writes ALL 28 bytes: hardware fills each 16-bit field's upper half with 0xffff, so 2-byte stores
// would leave the guest's own prior bytes there. FIP/FCS/FOP/FDP/FDS stay ZERO for the reason interp.c's
// copy of this routine records: they need two more 64-bit cpu fields, i.e. the checkpoint-format change the
// tag word exists to avoid, and only 16/32-bit unmasked-#FPU handlers -- impossible under this ABI -- read
// them, so writing the two selectors beside a zero FIP would be a plausible-looking lie.
static void x87_store_environment(struct cpu *cpu, uint8_t *area) {
    x87_put32(area, 0, 0xffff0000u | (uint32_t)(cpu->fpcw & 0xffff));
    x87_put32(area, 4, 0xffff0000u | hl_x86_x87_status_word(cpu));
    x87_put32(area, 8, 0xffff0000u | hl_x87_tag_word(cpu->fptop, cpu->st));
    x87_put32(area, 12, 0);
    x87_put32(area, 16, 0);
    x87_put32(area, 20, 0);
    x87_put32(area, 24, 0xffff0000u);
    cpu->fpcw |= 0x3f; // FNSTENV masks every exception afterwards (measured: fcw 0300 -> 037f)
}

// Tags are restored per PHYSICAL register and the data registers are NOT touched, so re-tagging a slot
// valid makes its old value readable again -- what hardware does (measured: FNINIT then FLDENV of a saved
// env reads back the pre-FNINIT ST(0)).
static void x87_load_environment(struct cpu *cpu, const uint8_t *area) {
    uint32_t control = x87_get16(area, 0);
    uint32_t status = x87_get16(area, 4);
    uint32_t tags = x87_get16(area, 8);
    cpu->fpcw = HL_X87_FCW(control);
    cpu->fpsw = (cpu->fpsw & ~UINT64_C(0x4740)) | (status & 0x4740);
    cpu->fptop = (cpu->fptop & HL_X87_STATE_BITS) | ((status >> 11) & 7);
    cpu->fptop |= HL_X87_ARMED;
    for (int slot = 0; slot < 8; ++slot)
        hl_x87_phys_mark(&cpu->fptop, slot, ((tags >> (2 * slot)) & 3u) == 3u);
}

// FNSAVE/FRSTOR m108: the environment above then eight 10-byte ext80. The register area is TOP-RELATIVE
// (slot i is ST(i)) while the tag word is physical -- the same asymmetry FXSAVE has, measured the same way.
void hl_x86_x87_environment(struct cpu *cpu) {
    uint8_t *area = (uint8_t *)(uintptr_t)cpu->x87_ea;
    switch (cpu->divop) {
    case X87ENV_STORE: x87_store_environment(cpu, area); break;
    case X87ENV_LOAD: x87_load_environment(cpu, area); break;
    case X87ENV_SAVE:
        x87_store_environment(cpu, area);
        for (int index = 0; index < 8; ++index)
            hl_x86_ext80_store(cpu->st[(cpu->fptop + (unsigned)index) & 7], area + 28 + index * 10);
        cpu->fptop = HL_X87_EMPTY_ALL | HL_X87_ARMED; // FNSAVE then reinitialises exactly as FNINIT does
        cpu->fpsw = 0;
        cpu->fpcw = 0x037f;
        hl_x87_exceptions_set(0);
        break;
    default:
        x87_load_environment(cpu, area);
        for (int index = 0; index < 8; ++index)
            cpu->st[(cpu->fptop + (unsigned)index) & 7] = hl_x86_ext80_load(area + 28 + index * 10);
        break;
    }
}

void hl_x86_fxsave(struct cpu *cpu) {
    uint8_t *image = (uint8_t *)(uintptr_t)cpu->x87_ea;
    uint32_t mxcsr = 0x1f80;
#if defined(HL_HOST_CPU_AARCH64)
    uint64_t fpcr;
    __asm__ volatile("mrs %0, fpcr" : "=r"(fpcr));
    {
        uint32_t arm_rounding = (uint32_t)((fpcr >> 22) & 3u);
        mxcsr |= (((arm_rounding & 1u) << 1) | ((arm_rounding >> 1) & 1u)) << 13;
        mxcsr |= hl_x87_exceptions_get(); // MXCSR's exception bits sit at the FSW positions
        // MXCSR.DE only: ARM sets IDC exactly when FPCR.FZ flushed a denormal input, i.e. in the mode that
        // carries the guest's DAZ -- and x86 with DAZ raises no #D. Same suppression as stmxcsr's
        // emit_fpsr_to_mxcsr in translate.c, so the two projections cannot disagree. FSW.DE is left alone:
        // x87 has no DAZ, and the FSW projection does not suppress it either.
        if (fpcr & (UINT64_C(1) << 24)) mxcsr &= ~UINT32_C(0x02);
    }
#elif defined(HL_HOST_CPU_X86_64)
    // On an x86-64 host the guest MXCSR IS the host MXCSR: no projection (the AArch64 arm has to remap
    // rounding and scatter the flags across FPSR). Take the whole word rather than OR-ing fields into the
    // 0x1f80 default -- masks, FZ and DAZ are the guest's too.
    mxcsr = _mm_getcsr();
#endif
    memcpy(image, &cpu->fpcw, 2);
    {
        uint16_t status = hl_x86_x87_status_word(cpu);
        memcpy(image + 2, &status, sizeof(status));
    }
    image[4] = hl_x87_abridged_tag(cpu->fptop);
    image[5] = 0;
    // The register area is TOP-RELATIVE -- slot i is ST(i), NOT physical st[i] -- while the tag byte above
    // is physical. Measured: with TOP=5 and 1,2,3 pushed, FXSAVE puts 3.0 (= ST(0)) at offset 32 and reports
    // tag byte e0 (physical R5..R7 live). Writing st[index] there was wrong for every guest with TOP != 0,
    // i.e. every guest that had pushed an odd number of values -- setjmp and signal handlers routinely do.
    for (int index = 0; index < 8; ++index)
        hl_x86_ext80_store(cpu->st[(cpu->fptop + (unsigned)index) & 7], image + 32 + index * 16);
    memcpy(image + 24, &mxcsr, sizeof(mxcsr));
    memcpy(image + 160, cpu->v, 16 * 16);
}

void hl_x86_fxrstor(struct cpu *cpu) {
    const uint8_t *image = (const uint8_t *)(uintptr_t)cpu->x87_ea;
    uint16_t status;
    uint16_t control;
    uint32_t mxcsr;
    memcpy(&control, image, sizeof(control));
    memcpy(&status, image + 2, sizeof(status));
    memcpy(&mxcsr, image + 24, sizeof(mxcsr));
    cpu->fpcw = HL_X87_FCW(control);
    cpu->fpsw = (cpu->fpsw & ~UINT64_C(0x4740)) | (status & 0x4740); // codes + SF
    cpu->fptop = (cpu->fptop & HL_X87_STATE_BITS) | ((status >> 11) & 7);
    if (hl_x87_tags_modelled()) {
        cpu->fptop |= HL_X87_ARMED;
        for (int slot = 0; slot < 8; ++slot)
            hl_x87_phys_mark(&cpu->fptop, slot, !((image[4] >> slot) & 1));
    }
    for (int index = 0; index < 8; ++index) // TOP-relative, matching hl_x86_fxsave
        cpu->st[(cpu->fptop + (unsigned)index) & 7] = hl_x86_ext80_load(image + 32 + index * 16);
    memcpy(cpu->v, image + 160, 16 * 16);
#if defined(HL_HOST_CPU_AARCH64)
    {
        uint64_t fpcr;
        uint64_t fpsr;
        uint32_t x86_rounding = (mxcsr >> 13) & 3u;
        uint32_t arm_rounding = ((x86_rounding & 1u) << 1) | ((x86_rounding >> 1) & 1u);
        static const unsigned fpsr_bit[6] = {0, 7, 1, 2, 3, 4};
        __asm__ volatile("mrs %0, fpcr" : "=r"(fpcr));
        __asm__ volatile("mrs %0, fpsr" : "=r"(fpsr));
        fpcr = (fpcr & ~(UINT64_C(3) << 22)) | (uint64_t)arm_rounding << 22;
        fpsr &= ~UINT64_C(0x9f);
        for (unsigned bit = 0; bit < 6; ++bit)
            fpsr |= (uint64_t)(((mxcsr | status) >> bit) & 1u) << fpsr_bit[bit];
        __asm__ volatile("msr fpcr, %0" : : "r"(fpcr));
        __asm__ volatile("msr fpsr, %0" : : "r"(fpsr));
    }
#elif defined(HL_HOST_CPU_X86_64)
    // Direct mapping again (see hl_x86_fxsave); FSW bits are OR-ed in because cpu->fpsw keeps only the
    // condition codes. MASK the word: the image is GUEST-CONTROLLED and LDMXCSR #GPs on any bit outside
    // MXCSR_MASK, killing the ENGINE where real FXRSTOR faults the GUEST (findings 1.12). 0xffff keeps
    // every architecturally defined field, DAZ included.
    _mm_setcsr((mxcsr | (uint32_t)(status & 0x3fu)) & 0xffffu);
#endif
}
