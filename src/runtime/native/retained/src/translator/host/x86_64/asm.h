#ifndef HL_TRANSLATOR_HOST_X86_64_ASM_H
#define HL_TRANSLATOR_HOST_X86_64_ASM_H

#include <stdint.h>
#include <string.h>

// x86-64 host instruction assembler -- the sibling of ../aarch64/asm.{c,h}, and the only thing this
// directory ever legitimately holds (see README.md). It encodes the small fixed vocabulary the same-ISA
// transliterator needs at block boundaries; the guest instructions themselves are copied verbatim and are
// never assembled here.
//
// Every emitter is bounds-checked and sets `overflow` instead of writing past `end`; the caller abandons
// the block on overflow rather than truncating it.

typedef struct hl_x64_asm {
    uint8_t *cursor;
    uint8_t *end;
    int overflow;
} hl_x64_asm;

enum { HL_X64_RAX = 0, HL_X64_RCX = 1, HL_X64_RDX = 2, HL_X64_RSP = 4, HL_X64_R15 = 15 };

static inline void hl_x64_u8(hl_x64_asm *a, uint8_t value) {
    if (a->cursor >= a->end) {
        a->overflow = 1;
        return;
    }
    *a->cursor++ = value;
}

static inline void hl_x64_u32(hl_x64_asm *a, uint32_t value) {
    for (int i = 0; i < 4; i++)
        hl_x64_u8(a, (uint8_t)(value >> (8 * i)));
}

static inline void hl_x64_u64(hl_x64_asm *a, uint64_t value) {
    for (int i = 0; i < 8; i++)
        hl_x64_u8(a, (uint8_t)(value >> (8 * i)));
}

static inline void hl_x64_copy(hl_x64_asm *a, const uint8_t *bytes, int length) {
    if (length < 0 || a->cursor + length > a->end) {
        a->overflow = 1;
        return;
    }
    memcpy(a->cursor, bytes, (size_t)length);
    a->cursor += length;
}

// ---- %gs-relative absolute addressing: `<op> %gs:disp32`, ModRM mod=00 rm=100 + SIB base=101.
// This is the transliterator's whole cpu-access vocabulary: it names no general register, which is the
// point of the segment-base register model.
static inline void hl_x64_gs_op(hl_x64_asm *a, int rex_w, uint8_t op, int reg_field, int32_t disp) {
    hl_x64_u8(a, 0x65);
    if (rex_w || reg_field >= 8) hl_x64_u8(a, (uint8_t)(0x40 | (rex_w ? 8 : 0) | ((reg_field >= 8) ? 4 : 0)));
    hl_x64_u8(a, op);
    hl_x64_u8(a, (uint8_t)(((reg_field & 7) << 3) | 4));
    hl_x64_u8(a, 0x25);
    hl_x64_u32(a, (uint32_t)disp);
}

// mov %gs:disp32, %reg  (64-bit)
static inline void hl_x64_load_gs(hl_x64_asm *a, int reg, int32_t disp) {
    hl_x64_gs_op(a, 1, 0x8B, reg, disp);
}

// mov %reg, %gs:disp32  (64-bit)
static inline void hl_x64_store_gs(hl_x64_asm *a, int reg, int32_t disp) {
    hl_x64_gs_op(a, 1, 0x89, reg, disp);
}

// movq $imm32, %gs:disp32  (sign-extended to 64 bits)
static inline void hl_x64_store_gs_imm32(hl_x64_asm *a, int32_t value, int32_t disp) {
    hl_x64_gs_op(a, 1, 0xC7, 0, disp);
    hl_x64_u32(a, (uint32_t)value);
}

// movabs $imm64, %reg
static inline void hl_x64_mov_imm64(hl_x64_asm *a, int reg, uint64_t value) {
    hl_x64_u8(a, (uint8_t)(0x48 | ((reg >= 8) ? 1 : 0)));
    hl_x64_u8(a, (uint8_t)(0xB8 + (reg & 7)));
    hl_x64_u64(a, value);
}

// movl $imm32, disp8(%rsp) -- the one guest store the transliterator itself performs (a CALL's pushed
// return address, written as two halves so no register is clobbered before the store can fault).
static inline void hl_x64_store_rsp_imm32(hl_x64_asm *a, int8_t disp, uint32_t value) {
    hl_x64_u8(a, 0xC7);
    hl_x64_u8(a, 0x44); // mod=01 reg=000 rm=100 (SIB)
    hl_x64_u8(a, 0x24); // SIB: base=rsp, no index
    hl_x64_u8(a, (uint8_t)disp);
    hl_x64_u32(a, value);
}

// mov (%rsp), %reg  (64-bit) -- the guest load a RET performs.
static inline void hl_x64_load_rsp_ind(hl_x64_asm *a, int reg) {
    hl_x64_u8(a, (uint8_t)(0x48 | ((reg >= 8) ? 4 : 0)));
    hl_x64_u8(a, 0x8B);
    hl_x64_u8(a, (uint8_t)(0x04 | ((reg & 7) << 3))); // mod=00 rm=100 (SIB)
    hl_x64_u8(a, 0x24);                               // SIB: base=rsp
}

// lea disp32(%rsp), %rsp -- adjusts the guest stack pointer without touching flags.
static inline void hl_x64_lea_rsp(hl_x64_asm *a, int32_t disp) {
    hl_x64_u8(a, 0x48);
    hl_x64_u8(a, 0x8D);
    hl_x64_u8(a, 0xA4); // mod=10 reg=rsp rm=100 (SIB)
    hl_x64_u8(a, 0x24);
    hl_x64_u32(a, (uint32_t)disp);
}

static inline void hl_x64_pushfq(hl_x64_asm *a) {
    hl_x64_u8(a, 0x9C);
}

// cld -- the host ABI requires DF clear at every C boundary, and a guest `std` leaves it set.
static inline void hl_x64_cld(hl_x64_asm *a) {
    hl_x64_u8(a, 0xFC);
}

static inline void hl_x64_pop(hl_x64_asm *a, int reg) {
    if (reg >= 8) hl_x64_u8(a, 0x41);
    hl_x64_u8(a, (uint8_t)(0x58 + (reg & 7)));
}

static inline void hl_x64_ret(hl_x64_asm *a) {
    hl_x64_u8(a, 0xC3);
}

// jcc rel32 -- the displacement is patched once the target offset is known.
static inline uint8_t *hl_x64_jcc_rel32(hl_x64_asm *a, int condition) {
    hl_x64_u8(a, 0x0F);
    hl_x64_u8(a, (uint8_t)(0x80 + (condition & 0xF)));
    uint8_t *slot = a->cursor;
    hl_x64_u32(a, 0);
    return slot;
}

static inline void hl_x64_patch_rel32(hl_x64_asm *a, uint8_t *slot, const uint8_t *target) {
    if (a->overflow || slot == NULL) return;
    int64_t delta = (int64_t)(target - (slot + 4));
    uint32_t value = (uint32_t)(int32_t)delta;
    for (int i = 0; i < 4; i++)
        slot[i] = (uint8_t)(value >> (8 * i));
}

#endif
