#include "private.h"

#include <stddef.h>

#include "../decode.h"
#include "../flags.h"
#include "../word.h"
#include "cpu.h"

int hl_x86_shift_opcode(uint8_t opcode) {
    return opcode == 0xc0u || opcode == 0xc1u || opcode == 0xd0u ||
           opcode == 0xd1u || opcode == 0xd2u || opcode == 0xd3u;
}

static int fail(decode *block, size_t *cursor, size_t start, hl_x86_a64_status status) {
    *cursor = start;
    block->status = status;
    block->exit = HL_X86_A64_INTERPRETER;
    return 0;
}

int hl_x86_decode_shift(const hl_x86_a64_request *request, decode *block, instruction *item,
                        uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                        size_t start, size_t *cursor) {
    uint8_t modrm;
    uint8_t kind;

    if (*cursor >= request->guest_size || *cursor - start >= 15u)
        return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
    modrm = request->guest_bytes[*cursor];
    kind = (uint8_t)((modrm >> 3) & 7u);
    if (kind == 6u) kind = 4u;
    if (kind > 3u && kind != 4u && kind != 5u && kind != 7u)
        return fail(block, cursor, start, HL_X86_A64_UNSUPPORTED);
    if (kind > 3u && (operand_16 != 0u || opcode == 0xc0u || opcode == 0xd0u || opcode == 0xd2u ||
                      (modrm >> 6) != 3u))
        return fail(block, cursor, start, operand_16 != 0u ? HL_X86_A64_FLAGS_ABI_REQUIRED :
                                                            HL_X86_A64_UNSUPPORTED);
    item->width = opcode == 0xc0u || opcode == 0xd0u || opcode == 0xd2u ? 1u :
                  (rex & 8u) != 0u ? 8u : operand_16 != 0u ? 2u : 4u;
    if ((modrm >> 6) == 3u) {
        ++*cursor;
        item->destination = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
        if (item->width == 1u && rex == 0u && (modrm & 7u) >= 4u) {
            item->destination = (uint8_t)((modrm & 7u) - 4u);
            item->destination_high = 1u;
        }
    } else {
        if (!hl_x86_decode_address(request, block, item, rex, 0, address_32, start, cursor)) return 0;
        item->memory_operand = 1u;
        item->width = opcode == 0xc0u || opcode == 0xd0u || opcode == 0xd2u ? 1u :
                      (rex & 8u) != 0u ? 8u : operand_16 != 0u ? 2u : 4u;
    }
    item->shift_count = 1u;
    item->variable_count = opcode == 0xd2u || opcode == 0xd3u;
    if (opcode == 0xc0u || opcode == 0xc1u) {
        if (*cursor >= request->guest_size || *cursor - start >= 15u)
            return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
        item->shift_count = request->guest_bytes[(*cursor)++];
    }
    item->operation = kind <= 3u ? OP_ROTATE : OP_SHIFT;
    item->shift_kind = kind;
    item->shift_count &= item->width == 8u ? 63u : 31u;
    return 1;
}

static uint32_t move_register(unsigned destination, unsigned source, int wide) {
    return (wide ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) | source << 16 | destination;
}

static uint32_t lsl(unsigned destination, unsigned source, unsigned shift) {
    return UINT32_C(0xd3400000) | (64u - shift) << 16 | (63u - shift) << 10 | source << 5 | destination;
}

static uint32_t shift_word(unsigned destination, unsigned source, unsigned count, unsigned kind, int wide) {
    unsigned bits = wide ? 64u : 32u;
    uint32_t base = kind == 7u ? (wide ? UINT32_C(0x93400000) : UINT32_C(0x13000000)) :
                                (wide ? UINT32_C(0xd3400000) : UINT32_C(0x53000000));
    unsigned rotate = kind == 4u ? bits - count : count;
    unsigned mask = kind == 4u ? bits - 1u - count : bits - 1u;

    return base | rotate << 16 | mask << 10 | source << 5 | destination;
}

/* Words for the shifted value alone, i.e. what remains once the flag word is elided. */
static uint32_t value_words(const instruction *item) {
    if (item->variable_count) return 5u + constant_words(item->width == 8u ? 63u : 31u);
    return item->shift_count == 0u ? 1u : 3u;
}

uint32_t hl_x86_shift_words(const instruction *item) {
    if (item->flags_dead) return value_words(item);
    if (item->variable_count) return item->shift_kind == 4u ? 46u : 43u;
    if (item->shift_count == 0u) return 1u;
    if (item->shift_count != 1u) return 27u;
    return item->shift_kind == 4u ? 32u : 30u;
}

static uint32_t variable_shift(unsigned destination, unsigned source, unsigned count, unsigned kind, int wide) {
    uint32_t base = kind == 4u ? UINT32_C(0x1ac02000) : kind == 5u ? UINT32_C(0x1ac02400) :
                                                                  UINT32_C(0x1ac02800);
    return base | (wide ? UINT32_C(0x80000000) : 0) | count << 16 | source << 5 | destination;
}

static void emit_parity(uint32_t *words, uint32_t *cursor) {
    words[(*cursor)++] = move_register(22, 18, 1);
    words[(*cursor)++] = UINT32_C(0xca400000) | 22u << 16 | 4u << 10 | 22u << 5 | 22u;
    words[(*cursor)++] = UINT32_C(0xca400000) | 22u << 16 | 2u << 10 | 22u << 5 | 22u;
    words[(*cursor)++] = UINT32_C(0xca400000) | 22u << 16 | 1u << 10 | 22u << 5 | 22u;
    emit_constant(words, cursor, 23, 1);
    words[(*cursor)++] = UINT32_C(0xca000000) | 23u << 16 | 22u << 5 | 22u;
    words[(*cursor)++] = hl_x86_bit(22, 22, 0);
    words[(*cursor)++] = lsl(22, 22, 2);
    words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 20u << 5 | 20u;
}

static void emit_variable(uint32_t *words, uint32_t *cursor, const instruction *item) {
    unsigned bits = item->width == 8u ? 64u : 32u;
    int wide = item->width == 8u;

    words[(*cursor)++] = move_register(16, item->destination, wide);
    words[(*cursor)++] = move_register(17, 1, wide);
    emit_constant(words, cursor, 23, bits - 1u);
    words[(*cursor)++] = UINT32_C(0x8a000000) | 23u << 16 | 17u << 5 | 17u;
    words[(*cursor)++] = variable_shift(18, 16, 17, item->shift_kind, wide);
    words[(*cursor)++] = load_word(24, offsetof(hl_native_x86_64_cpu, flags));
    words[(*cursor)++] = move_register(20, 24, 1);
    emit_constant(words, cursor, 23, HL_X86_RFLAGS_CF | HL_X86_RFLAGS_PF |
                                      HL_X86_RFLAGS_ZF | HL_X86_RFLAGS_SF);
    words[(*cursor)++] = UINT32_C(0x8a200000) | 23u << 16 | 20u << 5 | 20u;
    words[(*cursor)++] = UINT32_C(0x6a00001f) | (wide ? UINT32_C(0x80000000) : 0) |
                         18u << 16 | 18u << 5;
    words[(*cursor)++] = UINT32_C(0xd53b4200) | 21u;
    words[(*cursor)++] = hl_x86_bit(22, 21, 30);
    words[(*cursor)++] = lsl(22, 22, 6);
    words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 20u << 5 | 20u;
    words[(*cursor)++] = hl_x86_bit(22, 18, bits - 1u);
    words[(*cursor)++] = lsl(22, 22, 7);
    words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 20u << 5 | 20u;
    if (item->shift_kind == 4u) {
        emit_constant(words, cursor, 23, bits);
        words[(*cursor)++] = UINT32_C(0xcb000000) | 17u << 16 | 23u << 5 | 22u;
    } else {
        words[(*cursor)++] = UINT32_C(0xd1000000) | 1u << 10 | 17u << 5 | 22u;
    }
    words[(*cursor)++] = variable_shift(22, 16, 22, 5u, 1);
    emit_constant(words, cursor, 23, 1);
    words[(*cursor)++] = UINT32_C(0x8a000000) | 23u << 16 | 22u << 5 | 22u;
    words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 20u << 5 | 20u;
    emit_parity(words, cursor);
    if (item->shift_kind == 4u) {
        words[(*cursor)++] = hl_x86_bit(22, 18, bits - 1u);
        words[(*cursor)++] = hl_x86_bit(23, 16, bits - 1u);
        words[(*cursor)++] = UINT32_C(0xca000000) | 23u << 16 | 22u << 5 | 22u;
    } else if (item->shift_kind == 5u) {
        words[(*cursor)++] = hl_x86_bit(22, 16, bits - 1u);
    } else {
        emit_constant(words, cursor, 22, 0);
    }
    words[(*cursor)++] = move_register(25, 20, 1);
    emit_constant(words, cursor, 23, HL_X86_RFLAGS_OF);
    words[(*cursor)++] = UINT32_C(0x8a200000) | 23u << 16 | 25u << 5 | 25u;
    words[(*cursor)++] = lsl(22, 22, 11);
    words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 25u << 5 | 25u;
    words[(*cursor)++] = UINT32_C(0xf100001f) | 1u << 10 | 17u << 5;
    words[(*cursor)++] = UINT32_C(0x9a800000) | 20u << 16 | 25u << 5 | 20u;
    words[(*cursor)++] = UINT32_C(0xea00001f) | 17u << 16 | 17u << 5;
    words[(*cursor)++] = UINT32_C(0x9a800000) | 20u << 16 | 24u << 5 | 20u;
    words[(*cursor)++] = store_word(20, offsetof(hl_native_x86_64_cpu, flags));
    words[(*cursor)++] = move_register(item->destination, 18, wide);
}

/* The value half of a shift, emitting the same instructions the full form does for the result. */
static void emit_value_only(uint32_t *words, uint32_t *cursor, const instruction *item) {
    unsigned bits = item->width == 8u ? 64u : 32u;
    int wide = item->width == 8u;

    if (!item->variable_count) {
        if (item->shift_count == 0u) {
            words[(*cursor)++] = move_register(item->destination, item->destination, wide);
            return;
        }
        words[(*cursor)++] = move_register(16, item->destination, wide);
        words[(*cursor)++] = shift_word(18, 16, item->shift_count, item->shift_kind, wide);
        words[(*cursor)++] = move_register(item->destination, 18, wide);
        return;
    }
    words[(*cursor)++] = move_register(16, item->destination, wide);
    words[(*cursor)++] = move_register(17, 1, wide);
    emit_constant(words, cursor, 23, bits - 1u);
    words[(*cursor)++] = UINT32_C(0x8a000000) | 23u << 16 | 17u << 5 | 17u;
    words[(*cursor)++] = variable_shift(18, 16, 17, item->shift_kind, wide);
    words[(*cursor)++] = move_register(item->destination, 18, wide);
}

void hl_x86_emit_shift(uint32_t *words, uint32_t *cursor, const instruction *item) {
    unsigned bits = item->width == 8u ? 64u : 32u;
    unsigned count = item->shift_count;
    int wide = item->width == 8u;
    uint64_t clear;

    if (item->flags_dead) {
        emit_value_only(words, cursor, item);
        return;
    }
    if (item->variable_count) {
        emit_variable(words, cursor, item);
        return;
    }
    if (count == 0u) {
        words[(*cursor)++] = move_register(item->destination, item->destination, wide);
        return;
    }
    words[(*cursor)++] = move_register(16, item->destination, wide);
    words[(*cursor)++] = shift_word(18, 16, count, item->shift_kind, wide);
    words[(*cursor)++] = load_word(20, offsetof(hl_native_x86_64_cpu, flags));
    clear = HL_X86_RFLAGS_CF | HL_X86_RFLAGS_PF | HL_X86_RFLAGS_ZF | HL_X86_RFLAGS_SF |
            (count == 1u ? HL_X86_RFLAGS_OF : 0);
    emit_constant(words, cursor, 23, clear);
    words[(*cursor)++] = UINT32_C(0x8a200000) | 23u << 16 | 20u << 5 | 20u;
    words[(*cursor)++] = UINT32_C(0x6a00001f) | (wide ? UINT32_C(0x80000000) : 0) |
                         18u << 16 | 18u << 5;
    words[(*cursor)++] = UINT32_C(0xd53b4200) | 21u;
    words[(*cursor)++] = hl_x86_bit(22, 21, 30);
    words[(*cursor)++] = lsl(22, 22, 6);
    words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 20u << 5 | 20u;
    words[(*cursor)++] = hl_x86_bit(22, 18, bits - 1u);
    words[(*cursor)++] = lsl(22, 22, 7);
    words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 20u << 5 | 20u;
    words[(*cursor)++] = hl_x86_bit(22, 16, item->shift_kind == 4u ? bits - count : count - 1u);
    words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 20u << 5 | 20u;
    words[(*cursor)++] = move_register(22, 18, 1);
    words[(*cursor)++] = UINT32_C(0xca400000) | 22u << 16 | 4u << 10 | 22u << 5 | 22u;
    words[(*cursor)++] = UINT32_C(0xca400000) | 22u << 16 | 2u << 10 | 22u << 5 | 22u;
    words[(*cursor)++] = UINT32_C(0xca400000) | 22u << 16 | 1u << 10 | 22u << 5 | 22u;
    emit_constant(words, cursor, 23, 1);
    words[(*cursor)++] = UINT32_C(0xca000000) | 23u << 16 | 22u << 5 | 22u;
    words[(*cursor)++] = hl_x86_bit(22, 22, 0);
    words[(*cursor)++] = lsl(22, 22, 2);
    words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 20u << 5 | 20u;
    if (count == 1u) {
        if (item->shift_kind == 4u) {
            words[(*cursor)++] = hl_x86_bit(22, 18, bits - 1u);
            words[(*cursor)++] = hl_x86_bit(23, 16, bits - 1u);
            words[(*cursor)++] = UINT32_C(0xca000000) | 23u << 16 | 22u << 5 | 22u;
        } else if (item->shift_kind == 5u) {
            words[(*cursor)++] = hl_x86_bit(22, 16, bits - 1u);
        } else {
            emit_constant(words, cursor, 22, 0);
        }
        words[(*cursor)++] = lsl(22, 22, 11);
        words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 20u << 5 | 20u;
    }
    words[(*cursor)++] = store_word(20, offsetof(hl_native_x86_64_cpu, flags));
    words[(*cursor)++] = move_register(item->destination, 18, wide);
}
