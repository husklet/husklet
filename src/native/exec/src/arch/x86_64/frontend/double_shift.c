#include "private.h"

#include <stddef.h>

#include "../decode.h"
#include "../flags.h"
#include "../word.h"
#include "cpu.h"

static int fail(decode *block, size_t *cursor, size_t start,
                hl_x86_a64_status status) {
    *cursor = start;
    block->status = status;
    block->exit = HL_X86_A64_INTERPRETER;
    return 0;
}

int hl_x86_decode_double_shift(const hl_x86_a64_request *request, decode *block,
                               instruction *item, uint8_t opcode, uint8_t rex,
                               uint8_t operand_16, uint8_t address_32,
                               size_t start, size_t *cursor) {
    uint8_t modrm;

    if (*cursor >= request->guest_size || *cursor - start >= 15u)
        return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
    modrm = request->guest_bytes[*cursor];
    item->operation = OP_DOUBLE_SHIFT;
    item->width = (rex & 8u) != 0u ? 8u : operand_16 != 0u ? 2u : 4u;
    item->source = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
    item->shift_right = opcode == 0xacu || opcode == 0xadu;
    item->variable_count = opcode == 0xa5u || opcode == 0xadu;
    if ((modrm >> 6) == 3u) {
        ++*cursor;
        item->destination = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
    } else {
        if (!hl_x86_decode_address(request, block, item, rex, 0u, address_32,
                                   start, cursor)) return 0;
        item->operation = OP_DOUBLE_SHIFT;
        item->width = (rex & 8u) != 0u ? 8u : operand_16 != 0u ? 2u : 4u;
        item->memory_operand = 1u;
    }
    if (item->variable_count == 0u) {
        if (*cursor >= request->guest_size || *cursor - start >= 15u)
            return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
        item->shift_count = request->guest_bytes[(*cursor)++];
        item->shift_count &= item->width == 8u ? 63u : 31u;
    }
    return 1;
}

static uint32_t move(unsigned destination, unsigned source, int wide) {
    return (wide ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) |
           source << 16 | destination;
}

static uint32_t lsl(unsigned destination, unsigned source, unsigned shift) {
    return UINT32_C(0xd3400000) | (64u - shift) << 16 |
           (63u - shift) << 10 | source << 5 | destination;
}

static uint32_t lsr(unsigned destination, unsigned source, unsigned shift) {
    return UINT32_C(0xd340fc00) | shift << 16 | source << 5 | destination;
}

static void compare_zero(uint32_t *instruction, uint32_t *target, unsigned reg) {
    int64_t distance = target - instruction;
    *instruction = UINT32_C(0x34000000) |
                   ((uint32_t)distance & UINT32_C(0x7ffff)) << 5 | reg;
}

static void branch(uint32_t *instruction, uint32_t *target, unsigned condition) {
    int64_t distance = target - instruction;
    *instruction = UINT32_C(0x54000000) |
                   ((uint32_t)distance & UINT32_C(0x7ffff)) << 5 | condition;
}

static void emit_parity(uint32_t *words, uint32_t *cursor) {
    words[(*cursor)++] = move(22u, 18u, 1);
    words[(*cursor)++] = UINT32_C(0xca400000) | 22u << 16 | 4u << 10 | 22u << 5 | 22u;
    words[(*cursor)++] = UINT32_C(0xca400000) | 22u << 16 | 2u << 10 | 22u << 5 | 22u;
    words[(*cursor)++] = UINT32_C(0xca400000) | 22u << 16 | 1u << 10 | 22u << 5 | 22u;
    emit_constant(words, cursor, 23u, 1u);
    words[(*cursor)++] = UINT32_C(0xca1702d6); /* eor x22,x22,x23 */
    words[(*cursor)++] = hl_x86_bit(22u, 22u, 0u);
    words[(*cursor)++] = lsl(22u, 22u, 2u);
    words[(*cursor)++] = UINT32_C(0xaa160294); /* orr x20,x20,x22 */
}

void hl_x86_emit_double_shift(uint32_t *words, uint32_t *cursor,
                              const instruction *item) {
    unsigned bits = item->width * 8u;
    uint64_t width_mask = item->width == 8u ? UINT64_MAX :
                          (UINT64_C(1) << bits) - 1u;
    uint32_t *preserved;
    uint32_t *out_of_range = NULL;
    uint32_t *loop;
    uint32_t *again;
    uint32_t *not_one;

    if (item->memory_operand != 0u) {
        instruction load = *item;
        load.operation = OP_LOAD;
        load.destination = 18u;
        load.memory_operand = 0u;
        load.memory_write = 1u;
        hl_x86_emit_load(words, cursor, &load);
    } else if (item->width == 2u) {
        words[(*cursor)++] = UINT32_C(0xd3403c00) |
                             (uint32_t)item->destination << 5 | 18u;
    } else {
        words[(*cursor)++] = move(18u, item->destination, item->width == 8u);
    }
    if (item->width == 2u)
        words[(*cursor)++] = UINT32_C(0xd3403c00) |
                             (uint32_t)item->source << 5 | 19u;
    else
        words[(*cursor)++] = move(19u, item->source, item->width == 8u);
    if (item->variable_count == 0u &&
        (item->shift_count == 0u || (item->width == 2u && item->shift_count > 16u))) {
        if (item->memory_operand != 0u) {
            instruction store = *item;
            words[(*cursor)++] = move(24u, 18u, 1);
            store.operation = OP_STORE;
            store.source = 24u;
            store.memory_operand = 0u;
            hl_x86_emit_store(words, cursor, &store);
        }
        return;
    }
    if (item->variable_count != 0u) {
        words[(*cursor)++] = move(17u, 1u, 0);
        emit_constant(words, cursor, 23u, item->width == 8u ? 63u : 31u);
        words[(*cursor)++] = UINT32_C(0x0a170231); /* and w17,w17,w23 */
    } else {
        emit_constant(words, cursor, 17u, item->shift_count);
    }
    words[(*cursor)++] = move(24u, 17u, 0);
    preserved = item->variable_count != 0u ? &words[(*cursor)++] : NULL;
    if (item->variable_count != 0u && item->width == 2u) {
        emit_constant(words, cursor, 23u, 16u);
        words[(*cursor)++] = UINT32_C(0x6b17023f); /* cmp w17,w23 */
        out_of_range = &words[(*cursor)++];
    }
    words[(*cursor)++] = hl_x86_bit(25u, 18u, bits - 1u);
    loop = &words[*cursor];
    if (item->shift_right != 0u) {
        words[(*cursor)++] = hl_x86_bit(21u, 18u, 0u);
        words[(*cursor)++] = lsr(18u, 18u, 1u);
        words[(*cursor)++] = hl_x86_bit(22u, 19u, 0u);
        words[(*cursor)++] = lsr(19u, 19u, 1u);
        words[(*cursor)++] = lsl(22u, 22u, bits - 1u);
        words[(*cursor)++] = UINT32_C(0xaa160252); /* orr x18,x18,x22 */
    } else {
        words[(*cursor)++] = hl_x86_bit(21u, 18u, bits - 1u);
        words[(*cursor)++] = lsl(18u, 18u, 1u);
        words[(*cursor)++] = hl_x86_bit(22u, 19u, bits - 1u);
        words[(*cursor)++] = lsl(19u, 19u, 1u);
        words[(*cursor)++] = UINT32_C(0xaa160252); /* orr x18,x18,x22 */
    }
    if (item->width < 8u) {
        emit_constant(words, cursor, 23u, width_mask);
        words[(*cursor)++] = UINT32_C(0x8a170252); /* and x18,x18,x23 */
        words[(*cursor)++] = UINT32_C(0x8a170273); /* and x19,x19,x23 */
    }
    words[(*cursor)++] = UINT32_C(0x71000631); /* subs w17,w17,#1 */
    again = &words[(*cursor)++];
    compare_zero(again, loop, 17u); /* replace CBZ with CBNZ */
    *again |= UINT32_C(0x01000000);

    words[(*cursor)++] = load_word(20u, offsetof(hl_native_x86_64_cpu, flags));
    emit_constant(words, cursor, 23u, HL_X86_RFLAGS_CF | HL_X86_RFLAGS_PF |
                                      HL_X86_RFLAGS_ZF | HL_X86_RFLAGS_SF |
                                      HL_X86_RFLAGS_OF);
    words[(*cursor)++] = UINT32_C(0x8a370294); /* bic x20,x20,x23 */
    words[(*cursor)++] = UINT32_C(0xf100025f); /* cmp x18,#0 */
    words[(*cursor)++] = UINT32_C(0x9a9f17f6); /* cset x22,eq */
    words[(*cursor)++] = lsl(22u, 22u, 6u);
    words[(*cursor)++] = UINT32_C(0xaa160294);
    words[(*cursor)++] = hl_x86_bit(22u, 18u, bits - 1u);
    words[(*cursor)++] = lsl(22u, 22u, 7u);
    words[(*cursor)++] = UINT32_C(0xaa160294);
    words[(*cursor)++] = move(21u, 21u, 1);
    words[(*cursor)++] = UINT32_C(0xaa150294);
    emit_parity(words, cursor);
    words[(*cursor)++] = UINT32_C(0x7100071f); /* cmp w24,#1 */
    not_one = &words[(*cursor)++];
    words[(*cursor)++] = hl_x86_bit(22u, 18u, bits - 1u);
    if (item->shift_right == 0u)
        words[(*cursor)++] = UINT32_C(0xca1502d6); /* eor x22,x22,x21 */
    else
        words[(*cursor)++] = UINT32_C(0xca1902d6); /* eor x22,x22,x25 */
    words[(*cursor)++] = lsl(22u, 22u, 11u);
    words[(*cursor)++] = UINT32_C(0xaa160294);
    branch(not_one, &words[*cursor], 1u);

    if (item->memory_operand != 0u) {
        instruction store = *item;
        words[(*cursor)++] = move(25u, 20u, 1);
        words[(*cursor)++] = move(24u, 18u, 1);
        store.operation = OP_STORE;
        store.source = 24u;
        store.memory_operand = 0u;
        hl_x86_emit_store(words, cursor, &store);
        words[(*cursor)++] = store_word(25u, offsetof(hl_native_x86_64_cpu, flags));
    } else {
        words[(*cursor)++] = store_word(20u, offsetof(hl_native_x86_64_cpu, flags));
        if (item->width == 2u)
            words[(*cursor)++] = UINT32_C(0xb3403c00) | 18u << 5 | item->destination;
        else
            words[(*cursor)++] = move(item->destination, 18u, item->width == 8u);
    }
    if (preserved != NULL || out_of_range != NULL) {
        uint32_t *done = &words[*cursor];
        if (item->memory_operand != 0u) {
            instruction store = *item;
            uint32_t *skip_preserved = &words[(*cursor)++];
            done = &words[*cursor];
            words[(*cursor)++] = move(24u, 18u, 1);
            store.operation = OP_STORE;
            store.source = 24u;
            store.memory_operand = 0u;
            hl_x86_emit_store(words, cursor, &store);
            branch(skip_preserved, &words[*cursor], 14u);
        }
        if (preserved != NULL) compare_zero(preserved, done, 17u);
        if (out_of_range != NULL) branch(out_of_range, done, 8u);
    }
}

uint32_t hl_x86_double_shift_words(const instruction *item) {
    uint32_t scratch[512];
    uint32_t cursor = 0u;
    hl_x86_emit_double_shift(scratch, &cursor, item);
    return cursor;
}
