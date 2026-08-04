#include "private.h"

#include <stddef.h>

#include "../decode.h"
#include "../word.h"
#include "cpu.h"

static void branch(uint32_t *instruction, uint32_t *target, unsigned condition) {
    int64_t distance = target - instruction;
    *instruction = UINT32_C(0x54000000) | ((uint32_t)distance & UINT32_C(0x7ffff)) << 5 | condition;
}

static void test(uint32_t *instruction, uint32_t *target, unsigned bit) {
    int64_t distance = target - instruction;
    *instruction = UINT32_C(0x36000000) | (bit & 0x20u) << 26 | (bit & 31u) << 19 |
                   ((uint32_t)distance & UINT32_C(0x3fff)) << 5 | 17u;
}

/* Checked load with no architectural mutation before all guards pass. */
static void load_target(uint32_t *words, uint32_t *cursor, const instruction *item, int effective) {
    uint32_t *below, *overflow, *above, *permission, *skip;
    uint32_t *cache_hits[4];
    if (effective) {
        hl_x86_emit_address(words, cursor, item);
        --*cursor;
    } else {
        words[(*cursor)++] = UINT32_C(0xaa0003f0) | 4u << 16; /* mov x16,x4 */
    }
    hl_x86_emit_read_cache(words, cursor, 8u, 21u, 0, cache_hits);
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_first));
    words[(*cursor)++] = UINT32_C(0xeb11021f); below = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xb1002212); overflow = &words[(*cursor)++]; /* adds x18,x16,#8 */
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_last));
    words[(*cursor)++] = UINT32_C(0xeb11025f); above = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
    permission = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_delta));
    words[(*cursor)++] = UINT32_C(0x8b110211); /* add x17,x16,x17 */
    words[(*cursor)++] = UINT32_C(0xf9400235); /* ldr x21,[x17] */
    skip = &words[(*cursor)++];
    uint32_t *miss = &words[*cursor];
    branch(below, miss, 3u); branch(overflow, miss, 2u); branch(above, miss, 8u); test(permission, miss, 0u);
    words[(*cursor)++] = store_word(16, offsetof(hl_native_x86_64_cpu, fault_address));
    words[(*cursor)++] = UINT32_C(0xd2800031); /* mov x17,#1 read */
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_access));
    words[(*cursor)++] = UINT32_C(0xd2800111); /* mov x17,#8 */
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_size));
    emit_constant(words, cursor, 17, item->pc);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
    words[(*cursor)++] = UINT32_C(0xd2800071); /* fallback */
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
    if (item->live_chain != 0u) {
        hl_x86_finish_chain(words, cursor);
        hl_x86_spill_all(words, cursor);
    }
    words[(*cursor)++] = UINT32_C(0xd65f03c0);
    branch(skip, &words[*cursor], 14u);
    hl_x86_patch_read_hits(cache_hits, &words[*cursor]);
}

uint32_t hl_x86_control_words(const instruction *item) {
    if (item->operation == OP_PUSH) {
        instruction store = {.pc = item->pc, .immediate = (uint64_t)(-(int64_t)item->width),
                             .operation = OP_STORE, .source = item->has_immediate ? 20u : item->source,
                             .width = item->width, .address_base = 4u, .address_index = UINT8_MAX,
                             .live_chain = item->live_chain};
        return hl_x86_store_words(&store) + (item->has_immediate ? constant_words(item->immediate) : 0u) + 2u;
    }
    if (item->operation == OP_POP) {
        instruction load = {.pc = item->pc, .operation = OP_LOAD, .destination = 21u,
                            .width = item->width, .address_base = 4u, .address_index = UINT8_MAX,
                            .live_chain = item->live_chain};
        return hl_x86_load_words(&load) + 2u;
    }
    return 96u + hl_x86_read_cache_words() + (item->live_chain != 0u ? 19u : 0u) +
           (item->source_high != 0u ? hl_x86_address_words(item) : 0u) +
           constant_words(item->pc) + constant_words(item->pc + item->length) + constant_words(item->immediate);
}

void hl_x86_emit_control(uint32_t *words, uint32_t *cursor, const instruction *item) {
    if (item->operation == OP_PUSH) {
        unsigned source = item->source;
        if (item->has_immediate != 0u) {
            emit_constant(words, cursor, 20u, item->immediate);
            source = 20u;
        }
        instruction store = {.pc = item->pc, .immediate = (uint64_t)(-(int64_t)item->width),
                             .operation = OP_STORE, .source = (uint8_t)source, .width = item->width,
                             .address_base = 4u, .address_index = UINT8_MAX,
                             .live_chain = item->live_chain};
        hl_x86_emit_store(words, cursor, &store);
        words[(*cursor)++] = UINT32_C(0xd1000084) | (uint32_t)item->width << 10; /* sub x4,x4,#width */
        return;
    }
    if (item->operation == OP_POP) {
        instruction load = {.pc = item->pc, .operation = OP_LOAD, .destination = 21u,
                            .width = item->width, .address_base = 4u, .address_index = UINT8_MAX,
                            .live_chain = item->live_chain};
        hl_x86_emit_load(words, cursor, &load);
        words[(*cursor)++] = UINT32_C(0x91000084) | (uint32_t)item->width << 10; /* add x4,x4,#width */
        if (item->width == 2u) {
            words[(*cursor)++] = UINT32_C(0xb3403c00) | 21u << 5 | item->destination;
        } else {
            words[(*cursor)++] = UINT32_C(0xaa0003e0) | 21u << 16 | item->destination;
        }
        return;
    }
    if (item->operation == OP_LEAVE) {
        /* Validate [rbp] before changing either RSP or RBP. */
        load_target(words, cursor, item, 1);
        words[(*cursor)++] = UINT32_C(0x910020a4); /* add x4,x5,#8 */
        words[(*cursor)++] = UINT32_C(0xaa1503e5); /* mov x5,x21 */
        return;
    }
    if (item->operation == OP_RETURN) {
        load_target(words, cursor, item, 0);
        emit_constant(words, cursor, 18, 8u + item->immediate);
        words[(*cursor)++] = UINT32_C(0x8b120084); /* add x4,x4,x18 */
        words[(*cursor)++] = store_word(4, offsetof(hl_native_x86_64_cpu, registers) + 4u * sizeof(uint64_t));
        emit_constant(words, cursor, 18, item->pc);
        words[(*cursor)++] = store_word(18, offsetof(hl_native_x86_64_cpu, indirect_site));
        words[(*cursor)++] = store_word(21, offsetof(hl_native_x86_64_cpu, program));
        return;
    }

    if (item->has_immediate != 0u)
        emit_constant(words, cursor, 21, item->immediate);
    else if (item->source_high != 0u)
        load_target(words, cursor, item, 1);
    else
        words[(*cursor)++] = UINT32_C(0xaa0003f5) | (uint32_t)item->source << 16; /* mov x21,xN */

    if (item->operation == OP_JUMP) {
        emit_constant(words, cursor, 18, item->pc);
        words[(*cursor)++] = store_word(18, offsetof(hl_native_x86_64_cpu, indirect_site));
        words[(*cursor)++] = store_word(21, offsetof(hl_native_x86_64_cpu, program));
        return;
    }

    /* Validate and perform [rsp-8] before committing architectural RSP. */
    emit_constant(words, cursor, 20, item->pc + item->length);
    instruction push = {.pc = item->pc, .immediate = UINT64_MAX - 7u, .operation = OP_STORE,
                        .source = 20u, .width = 8u, .address_base = 4u,
                        .address_index = UINT8_MAX, .live_chain = item->live_chain};
    hl_x86_emit_store(words, cursor, &push);
    words[(*cursor)++] = UINT32_C(0xd1002084); /* sub x4,x4,#8 */
    words[(*cursor)++] = store_word(4, offsetof(hl_native_x86_64_cpu, registers) + 4u * sizeof(uint64_t));
    emit_constant(words, cursor, 18, item->has_immediate != 0u ? 0u : item->pc);
    words[(*cursor)++] = store_word(18, offsetof(hl_native_x86_64_cpu, indirect_site));
    words[(*cursor)++] = store_word(21, offsetof(hl_native_x86_64_cpu, program));
}
