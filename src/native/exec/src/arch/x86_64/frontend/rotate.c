#include "private.h"

#include <stddef.h>

#include "../decode.h"
#include "../word.h"
#include "cpu.h"

static uint32_t move(unsigned destination, unsigned source, int wide) {
    return (wide ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) | source << 16 | destination;
}

static uint32_t lsl(unsigned destination, unsigned source, unsigned shift) {
    return UINT32_C(0xd3400000) | (64u - shift) << 16 | (63u - shift) << 10 |
           source << 5 | destination;
}

static uint32_t lsr(unsigned destination, unsigned source, unsigned shift) {
    return UINT32_C(0xd340fc00) | shift << 16 | source << 5 | destination;
}

static void branch(uint32_t *instruction, uint32_t *target, unsigned condition) {
    int64_t distance = target - instruction;
    *instruction = UINT32_C(0x54000000) | ((uint32_t)distance & UINT32_C(0x7ffff)) << 5 | condition;
}

static void compare_zero(uint32_t *instruction, uint32_t *target, unsigned reg, int nonzero) {
    int64_t distance = target - instruction;
    *instruction = (nonzero ? UINT32_C(0x35000000) : UINT32_C(0x34000000)) |
                   ((uint32_t)distance & UINT32_C(0x7ffff)) << 5 | reg;
}

void hl_x86_emit_rotate(uint32_t *words, uint32_t *cursor, const instruction *item) {
    unsigned bits = item->width * 8u;
    uint64_t mask = item->width == 8u ? UINT64_MAX : (UINT64_C(1) << bits) - 1u;
    uint32_t *zero;
    uint32_t *loop;
    uint32_t *again;
    uint32_t *other_count;

    if (item->memory_operand != 0u) {
        instruction load = *item;
        load.operation = OP_LOAD;
        load.destination = 18u;
        load.memory_operand = 0u;
        load.memory_write = 1u;
        load.source_high = 0u;
        hl_x86_emit_load(words, cursor, &load);
    } else if (item->width == 1u) {
        unsigned low = item->destination_high != 0u ? 8u : 0u;
        words[(*cursor)++] = UINT32_C(0xd3400000) | low << 16 | (low + 7u) << 10 |
                             (uint32_t)item->destination << 5 | 18u;
    } else if (item->width == 2u) {
        words[(*cursor)++] = UINT32_C(0xd3403c00) | (uint32_t)item->destination << 5 | 18u;
    } else {
        words[(*cursor)++] = move(18u, item->destination, item->width == 8u);
    }
    if (item->variable_count != 0u) {
        words[(*cursor)++] = move(17u, 1u, 0);
        emit_constant(words, cursor, 23u, item->width == 8u ? 63u : 31u);
        words[(*cursor)++] = UINT32_C(0x0a170231); /* and w17, w17, w23 */
    } else {
        emit_constant(words, cursor, 17u, item->shift_count);
    }
    words[(*cursor)++] = move(24u, 17u, 0);
    if ((item->shift_kind == 2u || item->shift_kind == 3u) && item->width < 4u) {
        emit_constant(words, cursor, 23u, bits + 1u);
        words[(*cursor)++] = UINT32_C(0x1ad70a36); /* udiv w22, w17, w23 */
        words[(*cursor)++] = UINT32_C(0x1b17c6d1); /* msub w17, w22, w23, w17 */
    }
    words[(*cursor)++] = load_word(20u, offsetof(hl_native_x86_64_cpu, flags));
    words[(*cursor)++] = hl_x86_bit(21u, 20u, 0u);
    zero = &words[(*cursor)++];
    loop = &words[*cursor];
    if (item->shift_kind == 0u || item->shift_kind == 2u) {
        words[(*cursor)++] = hl_x86_bit(22u, 18u, bits - 1u);
        words[(*cursor)++] = lsl(18u, 18u, 1u);
        words[(*cursor)++] = UINT32_C(0xaa000252) |
                             (uint32_t)(item->shift_kind == 0u ? 22u : 21u) << 16;
        if (item->width < 8u) {
            emit_constant(words, cursor, 23u, mask);
            words[(*cursor)++] = UINT32_C(0x8a170252); /* and x18, x18, x23 */
        }
        words[(*cursor)++] = move(21u, 22u, 1);
    } else {
        words[(*cursor)++] = hl_x86_bit(22u, 18u, 0u);
        words[(*cursor)++] = lsr(18u, 18u, 1u);
        words[(*cursor)++] = lsl(23u, item->shift_kind == 1u ? 22u : 21u, bits - 1u);
        words[(*cursor)++] = UINT32_C(0xaa170252); /* orr x18, x18, x23 */
        words[(*cursor)++] = move(21u, 22u, 1);
    }
    words[(*cursor)++] = UINT32_C(0x71000631); /* subs w17, w17, #1 */
    again = &words[(*cursor)++];
    compare_zero(again, loop, 17u, 1);
    emit_constant(words, cursor, 23u, 1u);
    words[(*cursor)++] = UINT32_C(0x8a370294); /* bic x20, x20, x23 */
    words[(*cursor)++] = UINT32_C(0xaa150294); /* orr x20, x20, x21 */
    words[(*cursor)++] = UINT32_C(0x7100071f); /* cmp w24, #1 */
    other_count = &words[(*cursor)++];
    emit_constant(words, cursor, 23u, UINT64_C(1) << 11);
    words[(*cursor)++] = UINT32_C(0x8a370294); /* bic x20, x20, x23 */
    words[(*cursor)++] = hl_x86_bit(22u, 18u, bits - 1u);
    if (item->shift_kind == 0u || item->shift_kind == 2u)
        words[(*cursor)++] = UINT32_C(0xca1502d6); /* eor x22, x22, x21 */
    else {
        words[(*cursor)++] = hl_x86_bit(23u, 18u, bits - 2u);
        words[(*cursor)++] = UINT32_C(0xca1702d6); /* eor x22, x22, x23 */
    }
    words[(*cursor)++] = lsl(22u, 22u, 11u);
    words[(*cursor)++] = UINT32_C(0xaa160294); /* orr x20, x20, x22 */
    branch(other_count, &words[*cursor], 1u);
    words[(*cursor)++] = store_word(20u, offsetof(hl_native_x86_64_cpu, flags));
    if (item->memory_operand != 0u) {
        instruction store = *item;
        words[(*cursor)++] = move(25u, 18u, 1);
        store.operation = OP_STORE;
        store.source = 25u;
        store.source_high = 0u;
        store.memory_operand = 0u;
        hl_x86_emit_store(words, cursor, &store);
    } else if (item->width == 1u) {
        words[(*cursor)++] = (item->destination_high != 0u ? UINT32_C(0xb3781c00) :
                                                              UINT32_C(0xb3401c00)) |
                             18u << 5 | item->destination;
    } else if (item->width == 2u) {
        words[(*cursor)++] = UINT32_C(0xb3403c00) | 18u << 5 | item->destination;
    } else {
        words[(*cursor)++] = move(item->destination, 18u, item->width == 8u);
    }
    compare_zero(zero, &words[*cursor], 17u, 0);
}

uint32_t hl_x86_rotate_words(const instruction *item) {
    uint32_t scratch[256];
    uint32_t cursor = 0;
    hl_x86_emit_rotate(scratch, &cursor, item);
    return cursor;
}
