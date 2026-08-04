#include "private.h"

#include <stddef.h>

#include "../decode.h"
#include "../word.h"
#include "cpu.h"

uint32_t hl_x86_cmov_words(const instruction *item) {
    uint32_t condition = item->condition == 0xau || item->condition == 0xbu ? 3u : 15u;

    return condition + (item->width == 2u ? 2u : 1u);
}

void hl_x86_emit_cmov(uint32_t *words, uint32_t *cursor, const instruction *item, unsigned source) {
    unsigned condition;

    if (item->condition == 0xau || item->condition == 0xbu) {
        words[(*cursor)++] = load_word(20, offsetof(hl_native_x86_64_cpu, flags));
        words[(*cursor)++] = hl_x86_bit(20, 20, 2);
        words[(*cursor)++] = UINT32_C(0xf100001f) | 20u << 5;
        condition = item->condition == 0xau ? 1u : 0u;
    } else {
        hl_x86_emit_rflags(words, cursor);
        condition = arm_condition(item->condition);
    }
    if (item->width == 2u) {
        words[(*cursor)++] = UINT32_C(0x1a800000) | (uint32_t)item->destination << 16 |
                             condition << 12 | source << 5 | 21u;
        words[(*cursor)++] = UINT32_C(0xb3403c00) | 21u << 5 | item->destination;
    } else {
        words[(*cursor)++] = (item->width == 8u ? UINT32_C(0x9a800000) : UINT32_C(0x1a800000)) |
                             (uint32_t)item->destination << 16 | condition << 12 |
                             source << 5 | item->destination;
    }
}

uint32_t hl_x86_set_words(const instruction *item) {
    uint32_t condition = item->condition == 0xau || item->condition == 0xbu ? 4u : 16u;

    if (item->memory_operand != 0u) {
        instruction store = *item;
        store.operation = OP_STORE;
        store.source = 20u;
        store.width = 1u;
        return condition + hl_x86_store_words(&store);
    }
    return condition + 1u;
}

void hl_x86_emit_set(uint32_t *words, uint32_t *cursor, const instruction *item) {
    unsigned condition;

    if (item->condition == 0xau || item->condition == 0xbu) {
        words[(*cursor)++] = load_word(20, offsetof(hl_native_x86_64_cpu, flags));
        words[(*cursor)++] = hl_x86_bit(20, 20, 2);
        words[(*cursor)++] = UINT32_C(0xf100001f) | 20u << 5;
        condition = item->condition == 0xau ? 1u : 0u;
    } else {
        hl_x86_emit_rflags(words, cursor);
        condition = arm_condition(item->condition);
    }
    words[(*cursor)++] = UINT32_C(0x1a9f07e0) | ((condition ^ 1u) & 15u) << 12 | 20u;
    if (item->memory_operand != 0u) {
        instruction store = *item;
        store.operation = OP_STORE;
        store.source = 20u;
        store.width = 1u;
        hl_x86_emit_store(words, cursor, &store);
    } else {
        words[(*cursor)++] = (item->destination_high != 0u ? UINT32_C(0xb3781c00) :
                                                                  UINT32_C(0xb3401c00)) |
                               20u << 5 | item->destination;
    }
}

uint32_t hl_x86_bitscan_words(const instruction *item) {
    if (item->memory_operand == 0u) return 48u;
    instruction load = *item;
    load.operation = OP_LOAD;
    load.destination = 16u;
    load.width = item->width;
    load.load_width = item->width;
    return 48u + hl_x86_load_words(&load);
}

void hl_x86_emit_bitscan(uint32_t *words, uint32_t *cursor, const instruction *item) {
    unsigned source = item->source;
    unsigned wide = item->width == 8u;
    unsigned target = item->width == 2u ? 21u : item->destination;
    unsigned scan = item->conditional != 0u ? target : 22u;

    if (item->memory_operand != 0u) {
        instruction load = *item;
        load.operation = OP_LOAD;
        load.destination = 16u;
        load.width = item->width;
        load.load_width = item->width;
        load.conditional = 0u;
        hl_x86_emit_load(words, cursor, &load);
        source = 16u;
    } else if (source == item->destination) {
        words[(*cursor)++] = (wide ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) |
                               source << 16 | 23u;
        source = 23u;
    }
    if (item->width == 2u) {
        words[(*cursor)++] = UINT32_C(0x12003c00) | source << 5 | 23u;
        source = 23u;
    }
    if (item->condition == 0u) {
        words[(*cursor)++] = (wide ? UINT32_C(0xdac00000) : UINT32_C(0x5ac00000)) | source << 5 | scan;
        words[(*cursor)++] = (wide ? UINT32_C(0xdac01000) : UINT32_C(0x5ac01000)) | scan << 5 | scan;
    } else if (item->conditional != 0u) {
        words[(*cursor)++] = (wide ? UINT32_C(0xdac01000) : UINT32_C(0x5ac01000)) | source << 5 | scan;
    } else {
        words[(*cursor)++] = (wide ? UINT32_C(0xdac01000) : UINT32_C(0x5ac01000)) | source << 5 | 20u;
        emit_constant(words, cursor, 19u, wide ? 63u : item->width == 2u ? 15u : 31u);
        words[(*cursor)++] = (wide ? UINT32_C(0xcb000000) : UINT32_C(0x4b000000)) |
                               20u << 16 | 19u << 5 | scan;
    }
    if (item->conditional != 0u && item->width == 2u) {
        if (item->condition != 0u) {
            words[(*cursor)++] = UINT32_C(0x51004000) | target << 5 | target;
        } else {
            emit_constant(words, cursor, 19u, 16u);
            words[(*cursor)++] = UINT32_C(0x6b13001f) | target << 5;
            words[(*cursor)++] = UINT32_C(0x1a800000) | 19u << 16 | 9u << 12 |
                                   target << 5 | target;
        }
    }
    words[(*cursor)++] = (wide ? UINT32_C(0xea00001f) : UINT32_C(0x6a00001f)) |
                           source << 16 | source << 5;
    if (item->conditional != 0u) {
        words[(*cursor)++] = UINT32_C(0x1a9f17f3); /* cset w19,eq: count-form CF */
        words[(*cursor)++] = (wide ? UINT32_C(0xea00001f) : UINT32_C(0x6a00001f)) |
                               target << 16 | target << 5;
        if (item->width == 2u)
            words[(*cursor)++] = UINT32_C(0xb3403c00) | target << 5 | item->destination;
        hl_x86_emit_nzcv(words, cursor);
        words[(*cursor)++] = load_word(20u, offsetof(hl_native_x86_64_cpu, flags));
        words[(*cursor)++] = UINT32_C(0x927ff800) | 20u << 5 | 20u;
        words[(*cursor)++] = UINT32_C(0xaa000000) | 19u << 16 | 20u << 5 | 20u;
        words[(*cursor)++] = store_word(20u, offsetof(hl_native_x86_64_cpu, flags));
    } else {
        words[(*cursor)++] = (wide ? UINT32_C(0x9a800000) : UINT32_C(0x1a800000)) |
                               scan << 16 | item->destination << 5 | target;
        if (item->width == 2u)
            words[(*cursor)++] = UINT32_C(0xb3403c00) | target << 5 | item->destination;
        hl_x86_emit_nzcv(words, cursor);
    }
}

static instruction string_memory(const instruction *item, unsigned operation, unsigned base,
                                 unsigned reg, unsigned segment) {
    instruction memory = {0};
    memory.pc = item->pc; memory.length = item->length; memory.operation = operation;
    memory.destination = reg; memory.source = reg; memory.width = item->width;
    memory.load_width = operation == OP_LOAD ? item->width : 0u;
    memory.address_32 = item->address_32; memory.address_base = base;
    memory.address_index = UINT8_MAX; memory.segment = segment; memory.live_chain = item->live_chain;
    return memory;
}

uint32_t hl_x86_string_words(const instruction *item) {
    uint32_t count = 40u;
    if (item->condition != 1u) {
        instruction load = string_memory(item, OP_LOAD, 6u, item->condition == 2u ? 0u : 18u, item->segment);
        count += hl_x86_load_words(&load);
        if (item->condition == 0u) ++count;
    }
    if (item->condition != 2u) {
        instruction store = string_memory(item, OP_STORE, 7u, item->condition == 1u ? 0u : 19u, 0u);
        count += hl_x86_store_words(&store);
    }
    return count;
}

static void string_cbz(uint32_t *site, uint32_t *target, int narrow) {
    *site = (narrow ? UINT32_C(0x34000000) : UINT32_C(0xb4000000)) |
            ((uint32_t)(target - site) & UINT32_C(0x7ffff)) << 5 | 1u;
}

void hl_x86_emit_string(uint32_t *words, uint32_t *cursor, const instruction *item) {
    uint32_t *empty = item->conditional != 0u ? &words[(*cursor)++] : NULL;
    if (item->condition != 1u) {
        instruction load = string_memory(item, OP_LOAD, 6u, item->condition == 2u ? 0u : 18u, item->segment);
        hl_x86_emit_load(words, cursor, &load);
        if (item->condition == 0u)
            words[(*cursor)++] = UINT32_C(0xaa1203f3); /* mov x19,x18: preserve across store guards */
    }
    if (item->condition != 2u) {
        instruction store = string_memory(item, OP_STORE, 7u, item->condition == 1u ? 0u : 19u, 0u);
        hl_x86_emit_store(words, cursor, &store);
    }
    words[(*cursor)++] = load_word(17u, offsetof(hl_native_x86_64_cpu, flags));
    uint32_t *forward = &words[(*cursor)++];
    uint32_t sub = item->address_32 != 0u ? UINT32_C(0x51000000) : UINT32_C(0xd1000000);
    uint32_t add = item->address_32 != 0u ? UINT32_C(0x11000000) : UINT32_C(0x91000000);
    if (item->condition != 1u) words[(*cursor)++] = sub | (uint32_t)item->width << 10 | 6u << 5 | 6u;
    if (item->condition != 2u) words[(*cursor)++] = sub | (uint32_t)item->width << 10 | 7u << 5 | 7u;
    uint32_t *advanced = &words[(*cursor)++];
    uint32_t forward_index = *cursor;
    if (item->condition != 1u) words[(*cursor)++] = add | (uint32_t)item->width << 10 | 6u << 5 | 6u;
    if (item->condition != 2u) words[(*cursor)++] = add | (uint32_t)item->width << 10 | 7u << 5 | 7u;
    uint32_t advanced_index = *cursor;
    *forward = UINT32_C(0x36000000) | 10u << 19 |
               ((forward_index - (uint32_t)(forward - words)) & UINT32_C(0x3fff)) << 5 | 17u;
    *advanced = UINT32_C(0x14000000) |
                ((advanced_index - (uint32_t)(advanced - words)) & UINT32_C(0x03ffffff));
    uint32_t *remaining = NULL;
    if (item->conditional != 0u) {
        words[(*cursor)++] = (item->address_32 != 0u ? UINT32_C(0x51000421) : UINT32_C(0xd1000421));
        remaining = &words[(*cursor)++];
        emit_constant(words, cursor, 21u, item->pc);
        uint32_t *finish = &words[(*cursor)++];
        uint32_t complete = *cursor;
        emit_constant(words, cursor, 21u, item->pc + item->length);
        uint32_t done = *cursor;
        string_cbz(empty, &words[complete], item->address_32 != 0u);
        string_cbz(remaining, &words[complete], item->address_32 != 0u);
        *finish = UINT32_C(0x14000000) |
                  ((done - (uint32_t)(finish - words)) & UINT32_C(0x03ffffff));
    } else emit_constant(words, cursor, 21u, item->pc + item->length);
    words[(*cursor)++] = store_word(21u, offsetof(hl_native_x86_64_cpu, program));
}
