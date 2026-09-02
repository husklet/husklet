#ifndef HL_TRANSLATOR_HOST_X86_64_ASM_H
#define HL_TRANSLATOR_HOST_X86_64_ASM_H

#include <stdint.h>
#include <string.h>

#ifndef HL_X64_NOTE_EXTERNAL_ABSOLUTE
#define HL_X64_NOTE_EXTERNAL_ABSOLUTE(slot, address, kind) ((void)(slot), (void)(address), (void)(kind))
#define HL_X64_NOTE_EXTERNAL_ABSOLUTE_DEFAULT 1
#endif

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

enum { HL_X64_RAX = 0, HL_X64_RCX = 1, HL_X64_RDX = 2, HL_X64_RSP = 4, HL_X64_R11 = 11, HL_X64_R15 = 15 };

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

// movl $imm32, %gs:disp32. Every caller names a 32-bit CPU field; a REX.W movq would overwrite the
// adjacent field too (jcc_ibtc_miss is immediately followed by the low half of tpending).
static inline void hl_x64_store_gs_imm32(hl_x64_asm *a, int32_t value, int32_t disp) {
    hl_x64_gs_op(a, 0, 0xC7, 0, disp);
    hl_x64_u32(a, (uint32_t)value);
}

// movdqa disp32(%base), %xmm. The IBTC table and every entry are 16-byte aligned, so this is the
// indivisible pair load matched by cache.c's movdqa publish. Never split it into target/body loads.
static inline void hl_x64_load_xmm_aligned_disp32(hl_x64_asm *a, int xmm, int base, int32_t disp) {
    hl_x64_u8(a, 0x66);
    if (xmm >= 8 || base >= 8) hl_x64_u8(a, (uint8_t)(0x40 | ((xmm >= 8) ? 4 : 0) | ((base >= 8) ? 1 : 0)));
    hl_x64_u8(a, 0x0F);
    hl_x64_u8(a, 0x6F);
    hl_x64_u8(a, (uint8_t)(0x80 | ((xmm & 7) << 3) | (base & 7)));
    if ((base & 7) == 4) hl_x64_u8(a, (uint8_t)(0x20 | (base & 7)));
    hl_x64_u32(a, (uint32_t)disp);
}

// movq %xmm, %reg (SSE2). The caller has already made the guest XMM image canonical in cpu.
static inline void hl_x64_movq_xmm_to_reg(hl_x64_asm *a, int reg, int xmm) {
    hl_x64_u8(a, 0x66);
    hl_x64_u8(a, (uint8_t)(0x48 | ((xmm >= 8) ? 4 : 0) | ((reg >= 8) ? 1 : 0)));
    hl_x64_u8(a, 0x0F);
    hl_x64_u8(a, 0x7E);
    hl_x64_u8(a, (uint8_t)(0xC0 | ((xmm & 7) << 3) | (reg & 7)));
}

static inline void hl_x64_movq_reg_to_xmm(hl_x64_asm *a, int xmm, int reg) {
    hl_x64_u8(a, 0x66);
    hl_x64_u8(a, (uint8_t)(0x48 | ((xmm >= 8) ? 4 : 0) | ((reg >= 8) ? 1 : 0)));
    hl_x64_u8(a, 0x0F); hl_x64_u8(a, 0x6E);
    hl_x64_u8(a, (uint8_t)(0xC0 | ((xmm & 7) << 3) | (reg & 7)));
}

static inline void hl_x64_lea_rip_rel32(hl_x64_asm *a, int reg, const uint8_t *target) {
    hl_x64_u8(a, (uint8_t)(0x48 | ((reg >= 8) ? 4 : 0)));
    hl_x64_u8(a, 0x8D); hl_x64_u8(a, (uint8_t)(0x05 | ((reg & 7) << 3)));
    int64_t delta = target - (a->cursor + 4);
    if (delta < INT32_MIN || delta > INT32_MAX) { a->overflow = 1; delta = 0; }
    hl_x64_u32(a, (uint32_t)(int32_t)delta);
}

static inline void hl_x64_psrldq(hl_x64_asm *a, int xmm, uint8_t bytes) {
    hl_x64_u8(a, 0x66);
    if (xmm >= 8) hl_x64_u8(a, 0x41);
    hl_x64_u8(a, 0x0F);
    hl_x64_u8(a, 0x73);
    hl_x64_u8(a, (uint8_t)(0xD8 | (xmm & 7))); // mod=11 /3
    hl_x64_u8(a, bytes);
}

static inline void hl_x64_xmm_reg_op(hl_x64_asm *a, uint8_t prefix, uint8_t opcode,
                                     int destination, int source) {
    if (prefix != 0) hl_x64_u8(a, prefix);
    if (destination >= 8 || source >= 8)
        hl_x64_u8(a, (uint8_t)(0x40 | ((destination >= 8) ? 4 : 0) | ((source >= 8) ? 1 : 0)));
    hl_x64_u8(a, 0x0f); hl_x64_u8(a, opcode);
    hl_x64_u8(a, (uint8_t)(0xc0 | ((destination & 7) << 3) | (source & 7)));
}

static inline void hl_x64_movdqa_xmm(hl_x64_asm *a, int destination, int source) {
    hl_x64_xmm_reg_op(a, 0x66, 0x6f, destination, source);
}

static inline void hl_x64_pcmpeqd(hl_x64_asm *a, int destination, int source) {
    hl_x64_xmm_reg_op(a, 0x66, 0x76, destination, source);
}

static inline void hl_x64_pand(hl_x64_asm *a, int destination, int source) {
    hl_x64_xmm_reg_op(a, 0x66, 0xdb, destination, source);
}

static inline void hl_x64_pandn(hl_x64_asm *a, int destination, int source) {
    hl_x64_xmm_reg_op(a, 0x66, 0xdf, destination, source);
}

static inline void hl_x64_por(hl_x64_asm *a, int destination, int source) {
    hl_x64_xmm_reg_op(a, 0x66, 0xeb, destination, source);
}

static inline void hl_x64_pxor(hl_x64_asm *a, int destination, int source) {
    hl_x64_xmm_reg_op(a, 0x66, 0xef, destination, source);
}

static inline void hl_x64_pshufd(hl_x64_asm *a, int destination, int source, uint8_t order) {
    hl_x64_u8(a, 0x66);
    if (destination >= 8 || source >= 8)
        hl_x64_u8(a, (uint8_t)(0x40 | ((destination >= 8) ? 4 : 0) | ((source >= 8) ? 1 : 0)));
    hl_x64_u8(a, 0x0f); hl_x64_u8(a, 0x70);
    hl_x64_u8(a, (uint8_t)(0xc0 | ((destination & 7) << 3) | (source & 7)));
    hl_x64_u8(a, order);
}

static inline void hl_x64_cmp_reg(hl_x64_asm *a, int left, int right) {
    hl_x64_u8(a, (uint8_t)(0x48 | ((right >= 8) ? 4 : 0) | ((left >= 8) ? 1 : 0)));
    hl_x64_u8(a, 0x39); // left - right
    hl_x64_u8(a, (uint8_t)(0xC0 | ((right & 7) << 3) | (left & 7)));
}

static inline void hl_x64_test_reg(hl_x64_asm *a, int reg) {
    hl_x64_u8(a, (uint8_t)(0x48 | ((reg >= 8) ? 5 : 0)));
    hl_x64_u8(a, 0x85);
    hl_x64_u8(a, (uint8_t)(0xC0 | ((reg & 7) << 3) | (reg & 7)));
}

static inline void hl_x64_mov_reg(hl_x64_asm *a, int destination, int source) {
    hl_x64_u8(a, (uint8_t)(0x48 | ((source >= 8) ? 4 : 0) | ((destination >= 8) ? 1 : 0)));
    hl_x64_u8(a, 0x89);
    hl_x64_u8(a, (uint8_t)(0xC0 | ((source & 7) << 3) | (destination & 7)));
}

static inline void hl_x64_shift_imm8(hl_x64_asm *a, int reg, int operation, uint8_t count) {
    hl_x64_u8(a, (uint8_t)(0x48 | ((reg >= 8) ? 1 : 0)));
    hl_x64_u8(a, 0xC1);
    hl_x64_u8(a, (uint8_t)(0xC0 | ((operation & 7) << 3) | (reg & 7)));
    hl_x64_u8(a, count);
}

static inline void hl_x64_shr_imm8(hl_x64_asm *a, int reg, uint8_t count) {
    hl_x64_shift_imm8(a, reg, 5, count);
}

static inline void hl_x64_shl_imm8(hl_x64_asm *a, int reg, uint8_t count) {
    hl_x64_shift_imm8(a, reg, 4, count);
}

static inline void hl_x64_and_imm32(hl_x64_asm *a, int reg, uint32_t value) {
    hl_x64_u8(a, (uint8_t)(0x48 | ((reg >= 8) ? 1 : 0)));
    hl_x64_u8(a, 0x81);
    hl_x64_u8(a, (uint8_t)(0xE0 | (reg & 7))); // mod=11 /4
    hl_x64_u32(a, value);
}

static inline void hl_x64_add_reg(hl_x64_asm *a, int destination, int source) {
    hl_x64_u8(a, (uint8_t)(0x48 | ((source >= 8) ? 4 : 0) | ((destination >= 8) ? 1 : 0)));
    hl_x64_u8(a, 0x01);
    hl_x64_u8(a, (uint8_t)(0xC0 | ((source & 7) << 3) | (destination & 7)));
}

// jmpq *%gs:disp32. ModRM /4 is the near transfer; /5 would consume a far pointer and fault.
static inline void hl_x64_jmp_gs_indirect(hl_x64_asm *a, int32_t disp) {
    hl_x64_u8(a, 0x65);
    hl_x64_u8(a, 0xFF);
    hl_x64_u8(a, 0x24);
    hl_x64_u8(a, 0x25);
    hl_x64_u32(a, (uint32_t)disp);
}

// cmpq $imm8, %gs:disp32. Used only after the guest register file and flags have been spilled, so the
// condition-code clobber is private to an emitted boundary stub.
static inline void hl_x64_cmp_gs_imm8(hl_x64_asm *a, int32_t disp, uint8_t value) {
    hl_x64_u8(a, 0x65);
    hl_x64_u8(a, 0x48);
    hl_x64_u8(a, 0x83);
    hl_x64_u8(a, 0x3C); // mod=00 /7 rm=100 (SIB)
    hl_x64_u8(a, 0x25); // no base/index, disp32
    hl_x64_u32(a, (uint32_t)disp);
    hl_x64_u8(a, value);
}

// movabs $imm64, %reg
static inline void hl_x64_mov_imm64(hl_x64_asm *a, int reg, uint64_t value) {
    hl_x64_u8(a, (uint8_t)(0x48 | ((reg >= 8) ? 1 : 0)));
    hl_x64_u8(a, (uint8_t)(0xB8 + (reg & 7)));
    hl_x64_u64(a, value);
}

// lock incq *(uint64_t *)address. Diagnostic-only linked-edge counters emit this after a full spill, when
// RAX and flags are canonical in the CPU image. The same-ISA backend has no persistent code cache, and its
// fork hook discards inherited code, so the process-local absolute address never crosses an image epoch.
static inline void hl_x64_atomic_inc_abs_kind(hl_x64_asm *a, uintptr_t address, uint32_t kind) {
    uint8_t *start = a->cursor;
    hl_x64_mov_imm64(a, HL_X64_RAX, (uint64_t)address);
    hl_x64_u8(a, 0xF0);
    hl_x64_u8(a, 0x48);
    hl_x64_u8(a, 0xFF);
    hl_x64_u8(a, 0x00);
    if (!a->overflow) HL_X64_NOTE_EXTERNAL_ABSOLUTE(start + 2, address, kind);
}

static inline void hl_x64_atomic_inc_abs(hl_x64_asm *a, uintptr_t address) {
    hl_x64_atomic_inc_abs_kind(a, address, 1);
}

#if defined(HL_X64_NOTE_EXTERNAL_ABSOLUTE_DEFAULT)
#undef HL_X64_NOTE_EXTERNAL_ABSOLUTE_DEFAULT
#undef HL_X64_NOTE_EXTERNAL_ABSOLUTE
#endif

// mov (%base), %reg  (64-bit)
static inline void hl_x64_load_ind(hl_x64_asm *a, int reg, int base) {
    hl_x64_u8(a, (uint8_t)(0x48 | ((reg >= 8) ? 4 : 0) | ((base >= 8) ? 1 : 0)));
    hl_x64_u8(a, 0x8B);
    hl_x64_u8(a, (uint8_t)(((reg & 7) << 3) | (base & 7)));
}

// <op> disp32(%base), %reg (64-bit).  The same-ISA TLS lowering uses this only after loading the
// guest's FS base from canonical cpu state; it never borrows the host FS register.
static inline void hl_x64_reg_mem_disp32(hl_x64_asm *a, uint8_t op, int reg, int base, int32_t disp) {
    hl_x64_u8(a, (uint8_t)(0x48 | ((reg >= 8) ? 4 : 0) | ((base >= 8) ? 1 : 0)));
    hl_x64_u8(a, op);
    hl_x64_u8(a, (uint8_t)(0x80 | ((reg & 7) << 3) | (base & 7)));
    if ((base & 7) == 4) hl_x64_u8(a, (uint8_t)(0x20 | (base & 7))); // no index, selected base
    hl_x64_u32(a, (uint32_t)disp);
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

static inline void hl_x64_push_gs(hl_x64_asm *a, int32_t disp) {
    hl_x64_u8(a, 0x65);
    hl_x64_u8(a, 0xFF);
    hl_x64_u8(a, 0x34); // mod=00 /6 rm=100 (SIB)
    hl_x64_u8(a, 0x25); // no base/index, disp32
    hl_x64_u32(a, (uint32_t)disp);
}

static inline void hl_x64_popfq(hl_x64_asm *a) {
    hl_x64_u8(a, 0x9D);
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

// jmp rel32. `target` and the emitted slot must name the same alias of one arena; callers linking JIT
// blocks use RW for both, whose displacement is identical to the RX alias executed by the CPU.
static inline void hl_x64_jmp_rel32(hl_x64_asm *a, const uint8_t *target) {
    hl_x64_u8(a, 0xE9);
    uint8_t *slot = a->cursor;
    int64_t delta = target - (slot + 4);
    if (delta < INT32_MIN || delta > INT32_MAX) {
        a->overflow = 1;
        return;
    }
    hl_x64_u32(a, (uint32_t)(int32_t)delta);
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
