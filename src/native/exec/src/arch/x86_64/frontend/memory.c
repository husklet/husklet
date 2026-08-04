#include "private.h"

#include <stddef.h>

#include "../decode.h"
#include "../word.h"
#include "cpu.h"
#include "../../../executor.h"

static void branch(uint32_t *instruction, uint32_t *target, unsigned condition) {
    int64_t distance = target - instruction;
    *instruction = UINT32_C(0x54000000) | ((uint32_t)distance & UINT32_C(0x7ffff)) << 5 | condition;
}

static void test(uint32_t *instruction, uint32_t *target, unsigned bit) {
    int64_t distance = target - instruction;
    *instruction = UINT32_C(0x36000000) | (bit & 0x20u) << 26 | (bit & 31u) << 19 |
                   ((uint32_t)distance & UINT32_C(0x3fff)) << 5 | 17u;
}

#define HL_X86_READ_VIEWS 4u

static void jump(uint32_t *instruction, uint32_t *target) {
    int64_t distance = target - instruction;
    *instruction = UINT32_C(0x14000000) | ((uint32_t)distance & UINT32_C(0x03ffffff));
}

uint32_t hl_x86_read_cache_words(void) {
    return 12u + HL_X86_READ_VIEWS * 14u;
}

void hl_x86_emit_read_cache(uint32_t *words, uint32_t *cursor, unsigned width,
                            unsigned destination, int vector, uint32_t **hits) {
    uint32_t *overflow, *unpublished, *wrong_incarnation, *excess;
    uint32_t *next[HL_X86_READ_VIEWS][4];
    words[(*cursor)++] = UINT32_C(0xb1000000) | width << 10 | 16u << 5 | 18u; /* adds x18,x16,#width */
    words[(*cursor)++] = UINT32_C(0xeb10025f); /* cmp x18,x16 */
    overflow = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, read_token));
    unpublished = &words[(*cursor)++]; /* cbz x17, active */
    words[(*cursor)++] = UINT32_C(0xd50339bf); /* dmb ishld: acquire published views */
    words[(*cursor)++] = load_word(20, offsetof(hl_native_x86_64_cpu, read_incarnation));
    words[(*cursor)++] = UINT32_C(0xeb14023f); /* cmp x17,x20 */
    wrong_incarnation = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, read_count));
    words[(*cursor)++] = UINT32_C(0xf100023f) | HL_X86_READ_VIEWS << 10; /* cmp x17,#capacity */
    excess = &words[(*cursor)++];
    for (unsigned index = 0; index < HL_X86_READ_VIEWS; ++index) {
        size_t base = offsetof(hl_native_x86_64_cpu, read_views) + index * 4u * sizeof(uint64_t);
        words[(*cursor)++] = UINT32_C(0xf100023f) | (index + 1u) << 10; /* cmp x17,#index+1 */
        next[index][0] = &words[(*cursor)++];
        words[(*cursor)++] = load_word(20, base);
        words[(*cursor)++] = UINT32_C(0xeb14021f); /* cmp x16,x20 */
        next[index][1] = &words[(*cursor)++];
        words[(*cursor)++] = load_word(20, base + sizeof(uint64_t));
        words[(*cursor)++] = UINT32_C(0xeb14025f); /* cmp x18,x20 */
        next[index][2] = &words[(*cursor)++];
        words[(*cursor)++] = load_word(17, base + 3u * sizeof(uint64_t));
        next[index][3] = &words[(*cursor)++];
        words[(*cursor)++] = load_word(17, base + 2u * sizeof(uint64_t));
        words[(*cursor)++] = UINT32_C(0x8b110211); /* add x17,x16,x17 */
        words[(*cursor)++] = vector
            ? ((width == 4u ? UINT32_C(0xbd400220) :
                width == 8u ? UINT32_C(0xfd400220) : UINT32_C(0x3dc00220)) | destination)
            : ((width == 1u ? UINT32_C(0x39400220) :
                width == 2u ? UINT32_C(0x79400220) :
                width == 4u ? UINT32_C(0xb9400220) : UINT32_C(0xf9400220)) | destination);
        hits[index] = &words[(*cursor)++];
    }
    uint32_t *active = &words[*cursor];
    /* Token is published last and equals the mapping incarnation. */
    *unpublished = UINT32_C(0xb4000000) |
                   ((uint32_t)(active - unpublished) & UINT32_C(0x7ffff)) << 5 | 17u;
    branch(wrong_incarnation, active, 1u);
    branch(overflow, active, 3u);
    branch(excess, active, 8u);
    for (unsigned index = 0; index < HL_X86_READ_VIEWS; ++index) {
        uint32_t *following = index + 1u < HL_X86_READ_VIEWS ? next[index + 1u][0] - 1 : active;
        branch(next[index][0], active, 3u); /* count below index+1 means no later entry */
        branch(next[index][1], following, 3u); /* address below first */
        branch(next[index][2], following, 8u); /* end above last */
        test(next[index][3], following, 0u);
    }
}

void hl_x86_patch_read_hits(uint32_t **hits, uint32_t *target) {
    for (unsigned index = 0; index < HL_X86_READ_VIEWS; ++index) jump(hits[index], target);
}

void hl_x86_emit_dirty(uint32_t *words, uint32_t *cursor) {
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, dirty_first));
    words[(*cursor)++] = UINT32_C(0xeb11021f); /* cmp x16,x17 */
    words[(*cursor)++] = UINT32_C(0x9a913211); /* csel x17,x16,x17,lo */
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, dirty_first));
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, dirty_last));
    words[(*cursor)++] = UINT32_C(0xeb11025f); /* cmp x18,x17 */
    words[(*cursor)++] = UINT32_C(0x9a918251); /* csel x17,x18,x17,hi */
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, dirty_last));
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_first));
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, dirty_view_first));
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_last));
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, dirty_view_last));
}

uint32_t hl_x86_load_words(const instruction *item) {
    uint32_t access_width = item->load_width != 0u ? item->load_width : item->width;
    uint32_t words = hl_x86_address_words(item) - 1u + 23u + (item->live_chain != 0u ? 19u : 0u) +
                     item->memory_write +
                     constant_words(access_width) + constant_words(item->pc);
    if (item->memory_write == 0u) words += hl_x86_read_cache_words();

    if (item->load_width != 0u && item->signed_extend != 0u && item->width == 2u) ++words;

    return item->conditional != 0u ? words - 1u + hl_x86_cmov_words(item) : words;
}

void hl_x86_emit_load(uint32_t *words, uint32_t *cursor, const instruction *item) {
    uint32_t *below;
    uint32_t *overflow;
    uint32_t *above;
    uint32_t *permission;
    uint32_t *permission_write = NULL;
    uint32_t *skip;
    uint32_t *cache_hits[HL_X86_READ_VIEWS];
    uint32_t access_width = item->load_width != 0u ? item->load_width : item->width;
    hl_x86_emit_address(words, cursor, item);
    --*cursor;
    if (item->memory_write == 0u)
        hl_x86_emit_read_cache(words, cursor, access_width, 18u, 0, cache_hits);
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_first));
    words[(*cursor)++] = UINT32_C(0xeb11021f);
    below = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xb1000000) | access_width << 10 | 16u << 5 | 18u;
    overflow = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_last));
    words[(*cursor)++] = UINT32_C(0xeb11025f);
    above = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
    permission = &words[(*cursor)++];
    if (item->memory_write != 0u) permission_write = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_delta));
    words[(*cursor)++] = UINT32_C(0x8b110211);
    words[(*cursor)++] = access_width == 1u ? UINT32_C(0x39400232) :
                           access_width == 2u ? UINT32_C(0x79400232) :
                           access_width == 4u ? UINT32_C(0xb9400232) : UINT32_C(0xf9400232);
    skip = &words[(*cursor)++];
    uint32_t *miss = &words[*cursor];
    branch(below, miss, 3u);
    branch(overflow, miss, 2u);
    branch(above, miss, 8u);
    test(permission, miss, 0u);
    if (permission_write != NULL) test(permission_write, miss, 1u);
    words[(*cursor)++] = store_word(16, offsetof(hl_native_x86_64_cpu, fault_address));
    words[(*cursor)++] = item->memory_write != 0u ? UINT32_C(0xd2800071) : UINT32_C(0xd2800031);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_access));
    emit_constant(words, cursor, 17, access_width);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_size));
    emit_constant(words, cursor, 17, item->pc);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
    words[(*cursor)++] = UINT32_C(0xd2800071);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
    if (item->live_chain != 0u) {
        hl_x86_finish_chain(words, cursor);
        hl_x86_spill_all(words, cursor);
    }
    words[(*cursor)++] = UINT32_C(0xd65f03c0);
    branch(skip, &words[*cursor], 14u);
    if (item->memory_write == 0u) hl_x86_patch_read_hits(cache_hits, &words[*cursor]);
    if (item->conditional != 0u)
        hl_x86_emit_cmov(words, cursor, item, 18u);
    else if (item->load_width != 0u) {
        unsigned source = 18u;

        if (item->signed_extend != 0u) {
            unsigned target = item->width == 2u ? 17u : item->destination;
            words[(*cursor)++] = (item->width == 8u ? UINT32_C(0x93400000) : UINT32_C(0x13000000)) |
                                   (access_width == 1u ? UINT32_C(0x1c00) :
                                                        access_width == 2u ? UINT32_C(0x3c00) :
                                                                           UINT32_C(0x7c00)) |
                                   source << 5 | target;
            source = target;
        }
        if (item->width == 2u)
            words[(*cursor)++] = UINT32_C(0xb3403c00) | source << 5 | item->destination;
        else if (item->signed_extend == 0u)
            words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) |
                                   source << 16 | item->destination;
    }
    else if (item->width == 1u)
        words[(*cursor)++] = (item->destination_high != 0u ? UINT32_C(0xb3781c00) :
                                                               UINT32_C(0xb3401c00)) |
                             18u << 5 | item->destination;
    else if (item->width == 2u)
        words[(*cursor)++] = UINT32_C(0xb3403c00) | 18u << 5 | item->destination;
    else
        words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) |
                             18u << 16 | item->destination;
}

uint32_t hl_x86_store_words(const instruction *item) {
    return hl_x86_address_words(item) - 1u + (item->source_high == 8u ? 1u : 0u) +
           41u + (item->live_chain != 0u ? 19u : 0u) + constant_words(item->pc) +
           (item->has_immediate != 0u ? constant_words(item->operand_immediate) : 0u);
}

void hl_x86_emit_store(uint32_t *words, uint32_t *cursor, const instruction *item) {
    uint32_t *below;
    uint32_t *overflow;
    uint32_t *above;
    uint32_t *permission;
    uint32_t *skip;
    unsigned source = item->source;
    if (item->has_immediate != 0u) {
        emit_constant(words, cursor, 20u, item->operand_immediate);
        source = 20u;
    }
    hl_x86_emit_address(words, cursor, item);
    --*cursor;
    if (item->source_high == 8u) {
        words[(*cursor)++] = UINT32_C(0x53083c00) | source << 5 | 18u;
        source = 18u;
    }
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_first));
    words[(*cursor)++] = UINT32_C(0xeb11021f);
    below = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xb1000000) | (uint32_t)item->width << 10 | 16u << 5 | 18u;
    overflow = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_last));
    words[(*cursor)++] = UINT32_C(0xeb11025f);
    above = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
    permission = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_delta));
    words[(*cursor)++] = UINT32_C(0x8b110211);
    words[(*cursor)++] = (item->width == 1u ? UINT32_C(0x39000220) :
                           item->width == 2u ? UINT32_C(0x79000220) :
                           item->width == 4u ? UINT32_C(0xb9000220) : UINT32_C(0xf9000220)) |
                          source;
    words[(*cursor)++] = UINT32_C(0xd2800031);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, memory_written));
    hl_x86_emit_dirty(words, cursor);
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
    words[(*cursor)++] = load_word(18, offsetof(hl_native_x86_64_cpu, executable_written));
    words[(*cursor)++] = UINT32_C(0xaa120231); /* orr x17,x17,x18: sticky permission latch */
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, executable_written));
    skip = &words[(*cursor)++];
    uint32_t *miss = &words[*cursor];
    branch(below, miss, 3u);
    branch(overflow, miss, 2u);
    branch(above, miss, 8u);
    test(permission, miss, 1u);
    words[(*cursor)++] = store_word(16, offsetof(hl_native_x86_64_cpu, fault_address));
    words[(*cursor)++] = UINT32_C(0xd2800051);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_access));
    emit_constant(words, cursor, 17, item->width);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_size));
    emit_constant(words, cursor, 17, item->pc);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
    words[(*cursor)++] = UINT32_C(0xd2800071);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
    if (item->live_chain != 0u) {
        hl_x86_finish_chain(words, cursor);
        hl_x86_spill_all(words, cursor);
    }
    words[(*cursor)++] = UINT32_C(0xd65f03c0);
    branch(skip, &words[*cursor], 14u);
}

/* Memory XCHG is implicitly locked.  All fault and alignment decisions precede
 * SWPAL, and publication follows it, so neither fallback nor a guest fault can
 * expose a partial architectural mutation. */
uint32_t hl_x86_xchg_words(const instruction *item) {
    return hl_x86_address_words(item) - 1u + 60u + constant_words(item->width) +
           2u * constant_words(item->pc) + (item->source_high == 8u ? 1u : 0u) +
           (item->live_chain != 0u ? 38u : 0u);
}

void hl_x86_emit_xchg(uint32_t *words, uint32_t *cursor, const instruction *item) {
    uint32_t *below, *overflow, *above, *readable, *writable, *unaligned, *done;
    unsigned source = item->source;
    hl_x86_emit_address(words, cursor, item);
    --*cursor; /* retain the EA in x16 */
    if (item->source_high == 8u) {
        words[(*cursor)++] = UINT32_C(0x53083c00) | source << 5 | 20u; /* ubfx w20,wS,#8,#8 */
        source = 20u;
    }
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_first));
    words[(*cursor)++] = UINT32_C(0xeb11021f); /* cmp x16,x17 */
    below = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xb1000000) | (uint32_t)item->width << 10 | 16u << 5 | 18u;
    overflow = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_last));
    words[(*cursor)++] = UINT32_C(0xeb11025f); /* cmp x18,x17 */
    above = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
    readable = &words[(*cursor)++];
    writable = &words[(*cursor)++];
    if (item->width != 1u) {
        emit_constant(words, cursor, 19u, item->width - 1u);
        words[(*cursor)++] = UINT32_C(0xea00001f) | 19u << 16 | 16u << 5; /* tst x16,x19 */
        unaligned = &words[(*cursor)++];
    } else unaligned = NULL;
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_delta));
    words[(*cursor)++] = UINT32_C(0x8b110211); /* add x17,x16,x17 */
    {
        uint32_t size = item->width == 8u ? UINT32_C(0xc0000000) :
                        item->width == 4u ? UINT32_C(0x80000000) :
                        item->width == 2u ? UINT32_C(0x40000000) : 0u;
        words[(*cursor)++] = (UINT32_C(0xb8e08000) & UINT32_C(0x3fffffff)) | size |
                             source << 16 | 17u << 5 | 18u; /* swpal old,[x17] */
    }
    if (item->width == 1u)
        words[(*cursor)++] = (item->destination_high ? UINT32_C(0xb3781c00) : UINT32_C(0xb3401c00)) |
                             18u << 5 | item->destination;
    else if (item->width == 2u)
        words[(*cursor)++] = UINT32_C(0xb3403c00) | 18u << 5 | item->destination;
    else
        words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) |
                             18u << 16 | item->destination;
    words[(*cursor)++] = UINT32_C(0xd2800031);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, memory_written));
    /* SWPAL returned the pre-image in x18; restore the validated guest end
     * before publishing the exact dirty interval. */
    words[(*cursor)++] = UINT32_C(0x91000000) | (uint32_t)item->width << 10 | 16u << 5 | 18u;
    hl_x86_emit_dirty(words, cursor);
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
    words[(*cursor)++] = load_word(18, offsetof(hl_native_x86_64_cpu, executable_written));
    words[(*cursor)++] = UINT32_C(0xaa120231);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, executable_written));
    done = &words[(*cursor)++];

    uint32_t *fault = &words[*cursor];
    branch(below, fault, 3u); branch(overflow, fault, 2u); branch(above, fault, 8u);
    test(readable, fault, 0u); test(writable, fault, 1u);
    words[(*cursor)++] = store_word(16, offsetof(hl_native_x86_64_cpu, fault_address));
    words[(*cursor)++] = UINT32_C(0xd2800071); /* read|write */
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_access));
    emit_constant(words, cursor, 17u, item->width);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_size));
    emit_constant(words, cursor, 17u, item->pc);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
    words[(*cursor)++] = UINT32_C(0xd2800071); /* fallback */
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
    if (item->live_chain) { hl_x86_finish_chain(words, cursor); hl_x86_spill_all(words, cursor); }
    words[(*cursor)++] = UINT32_C(0xd65f03c0);

    uint32_t *fallback = &words[*cursor];
    if (unaligned != NULL) branch(unaligned, fallback, 1u); /* b.ne */
    emit_constant(words, cursor, 17u, item->pc);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
    emit_constant(words, cursor, 17u, HL_NATIVE_EXIT_FALLBACK);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
    if (item->live_chain) { hl_x86_finish_chain(words, cursor); hl_x86_spill_all(words, cursor); }
    words[(*cursor)++] = UINT32_C(0xd65f03c0);
    branch(done, &words[*cursor], 14u);
}

int hl_x86_decode_xadd(const hl_x86_a64_request *request, decode *block, instruction *item,
                       uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                       uint8_t lock, size_t start, size_t *cursor) {
    uint8_t modrm, raw;
    if (*cursor >= request->guest_size || *cursor - start >= 15u) {
        *cursor = start; block->status = HL_X86_A64_TRUNCATED;
        block->exit = HL_X86_A64_INTERPRETER; return 0;
    }
    modrm = request->guest_bytes[*cursor]; raw = (modrm >> 3) & 7u;
    item->operation = OP_XADD;
    item->source = (uint8_t)(raw | ((rex & 4u) << 1));
    item->width = opcode == 0xc0u ? 1u : (rex & 8u) ? 8u : operand_16 ? 2u : 4u;
    item->condition = lock;
    if (opcode == 0xc0u && rex == 0u && raw >= 4u) {
        item->source = (uint8_t)(raw - 4u); item->source_high = 8u;
    }
    if ((modrm >> 6) == 3u) {
        uint8_t rm = modrm & 7u;
        if (lock) {
            *cursor = start; block->status = HL_X86_A64_UNSUPPORTED;
            block->exit = HL_X86_A64_INTERPRETER; return 0;
        }
        ++*cursor; item->destination = (uint8_t)(rm | ((rex & 1u) << 3));
        if (opcode == 0xc0u && rex == 0u && rm >= 4u) {
            item->destination = (uint8_t)(rm - 4u); item->destination_high = 1u;
        }
        return 1;
    }
    if ((request->flags & HL_X86_A64_LSE) == 0u) {
        *cursor = start; block->status = HL_X86_A64_UNSUPPORTED;
        block->exit = HL_X86_A64_INTERPRETER; return 0;
    }
    if (!hl_x86_decode_address(request, block, item, rex, 0, address_32, start, cursor)) return 0;
    item->operation = OP_XADD; item->memory_operand = 1u; item->memory_write = 1u;
    item->source = (uint8_t)(raw | ((rex & 4u) << 1));
    if (opcode == 0xc0u && rex == 0u && raw >= 4u) {
        item->source = (uint8_t)(raw - 4u); item->source_high = 8u;
    }
    item->width = opcode == 0xc0u ? 1u : (rex & 8u) ? 8u : operand_16 ? 2u : 4u;
    item->condition = lock;
    return 1;
}

void hl_x86_emit_xadd(uint32_t *words, uint32_t *cursor, const instruction *item) {
    instruction add = *item;
    if (!item->memory_operand) {
        words[(*cursor)++] = item->width == 1u ?
            (item->destination_high ? UINT32_C(0xd3483c00) : UINT32_C(0xd3401c00)) |
            item->destination << 5 | 19u :
            (item->width == 8u ? UINT32_C(0xaa0003f3) : UINT32_C(0x2a0003f3)) |
            item->destination << 16;
        words[(*cursor)++] = item->width == 1u ?
            (item->source_high ? UINT32_C(0xd3483c00) : UINT32_C(0xd3401c00)) |
            item->source << 5 | 21u :
            (item->width == 8u ? UINT32_C(0xaa0003f5) : UINT32_C(0x2a0003f5)) |
            item->source << 16;
        words[(*cursor)++] = UINT32_C(0xaa1303f9); /* preserve pre-image in x25 */
        add.destination = 19u; add.source = 21u; add.destination_high = 0u; add.source_high = 0u;
        add.memory_operand = 0u; add.memory_write = 0u; add.alu_kind = 0u; add.flags_only = 1u;
        hl_x86_emit_alu(words, cursor, &add); /* result remains in x18 */
        if (item->width == 1u)
            words[(*cursor)++] = (item->source_high ? UINT32_C(0xb3781c00) : UINT32_C(0xb3401c00)) |
                                 25u << 5 | item->source;
        else if (item->width == 2u)
            words[(*cursor)++] = UINT32_C(0xb3403c00) | 25u << 5 | item->source;
        else
            words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) |
                                 25u << 16 | item->source;
        if (item->width == 1u)
            words[(*cursor)++] = (item->destination_high ? UINT32_C(0xb3781c00) : UINT32_C(0xb3401c00)) |
                                 18u << 5 | item->destination;
        else if (item->width == 2u)
            words[(*cursor)++] = UINT32_C(0xb3403c00) | 18u << 5 | item->destination;
        else
            words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) |
                                 18u << 16 | item->destination;
        return;
    }
    {
        uint32_t *below, *overflow, *above, *readable, *writable, *unaligned, *done;
        unsigned source = item->source;
        hl_x86_emit_address(words, cursor, item); --*cursor;
        if (item->source_high) {
            words[(*cursor)++] = UINT32_C(0x53083c00) | source << 5 | 21u;
            source = 21u;
        } else
            words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003f5) : UINT32_C(0x2a0003f5)) |
                                 source << 16;
        words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_first));
        words[(*cursor)++] = UINT32_C(0xeb11021f); below = &words[(*cursor)++];
        words[(*cursor)++] = UINT32_C(0xb1000000) | (uint32_t)item->width << 10 | 16u << 5 | 18u;
        overflow = &words[(*cursor)++];
        words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_last));
        words[(*cursor)++] = UINT32_C(0xeb11025f); above = &words[(*cursor)++];
        words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
        readable = &words[(*cursor)++]; writable = &words[(*cursor)++];
        if (item->width != 1u) {
            emit_constant(words, cursor, 19u, item->width - 1u);
            words[(*cursor)++] = UINT32_C(0xea00001f) | 19u << 16 | 16u << 5;
            unaligned = &words[(*cursor)++];
        } else unaligned = NULL;
        words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_delta));
        words[(*cursor)++] = UINT32_C(0x8b110211);
        words[(*cursor)++] = UINT32_C(0xaa1103f8); /* preserve admitted host EA across flag lowering */
        {
            uint32_t size = item->width == 8u ? UINT32_C(0xc0000000) :
                            item->width == 4u ? UINT32_C(0x80000000) :
                            item->width == 2u ? UINT32_C(0x40000000) : 0u;
            words[(*cursor)++] = UINT32_C(0x38e00000) | size | source << 16 | 17u << 5 | 19u;
        }
        add.destination = 19u; add.source = 21u; add.destination_high = 0u; add.source_high = 0u;
        add.memory_operand = 0u; add.memory_write = 0u; add.alu_kind = 0u; add.flags_only = 1u;
        hl_x86_emit_alu(words, cursor, &add);
        if (item->width == 1u)
            words[(*cursor)++] = (item->source_high ? UINT32_C(0xb3781c00) : UINT32_C(0xb3401c00)) |
                                 19u << 5 | item->source;
        else if (item->width == 2u)
            words[(*cursor)++] = UINT32_C(0xb3403c00) | 19u << 5 | item->source;
        else
            words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) |
                                 19u << 16 | item->source;
        words[(*cursor)++] = UINT32_C(0xd2800031);
        words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, memory_written));
        words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_delta));
        words[(*cursor)++] = UINT32_C(0xcb110310) | 24u << 5;
        words[(*cursor)++] = UINT32_C(0x91000012) | (uint32_t)item->width << 10 | 16u << 5;
        hl_x86_emit_dirty(words, cursor);
        words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
        words[(*cursor)++] = load_word(18, offsetof(hl_native_x86_64_cpu, executable_written));
        words[(*cursor)++] = UINT32_C(0xaa120231);
        words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, executable_written));
        done = &words[(*cursor)++];
        {
            uint32_t *fault = &words[*cursor];
            branch(below, fault, 3u); branch(overflow, fault, 2u); branch(above, fault, 8u);
            test(readable, fault, 0u); test(writable, fault, 1u);
            words[(*cursor)++] = store_word(16, offsetof(hl_native_x86_64_cpu, fault_address));
            words[(*cursor)++] = UINT32_C(0xd2800071);
            words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_access));
            emit_constant(words, cursor, 17u, item->width);
            words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_size));
            emit_constant(words, cursor, 17u, item->pc);
            words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
            words[(*cursor)++] = UINT32_C(0xd2800071);
            words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
            if (item->live_chain) { hl_x86_finish_chain(words, cursor); hl_x86_spill_all(words, cursor); }
            words[(*cursor)++] = UINT32_C(0xd65f03c0);
        }
        {
            uint32_t *fallback = &words[*cursor];
            if (unaligned) branch(unaligned, fallback, 1u);
            emit_constant(words, cursor, 17u, item->pc);
            words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
            emit_constant(words, cursor, 17u, HL_NATIVE_EXIT_FALLBACK);
            words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
            if (item->live_chain) { hl_x86_finish_chain(words, cursor); hl_x86_spill_all(words, cursor); }
            words[(*cursor)++] = UINT32_C(0xd65f03c0);
        }
        branch(done, &words[*cursor], 14u);
        return;
    }
}

uint32_t hl_x86_xadd_words(const instruction *item) {
    uint32_t scratch[512], cursor = 0;
    hl_x86_emit_xadd(scratch, &cursor, item);
    return cursor;
}

uint32_t hl_x86_cmpxchg_words(const instruction *item) {
    uint32_t scratch[256], cursor = 0;
    hl_x86_emit_cmpxchg(scratch, &cursor, item);
    return cursor;
}

/* Locked memory CMPXCHG.  The complete read/write proof and natural-alignment
 * test happen before CASAL.  CASAL itself is the sole possible mutation; the
 * write publications are reached only when its returned pre-image matched
 * the accumulator. */
void hl_x86_emit_cmpxchg(uint32_t *words, uint32_t *cursor, const instruction *item) {
    uint32_t *below, *overflow, *above, *readable, *writable, *unaligned, *done, *published, *matched;
    unsigned source = item->source;
    instruction compare = *item;

    hl_x86_emit_address(words, cursor, item);
    --*cursor;
    if (item->source_high == 8u) {
        words[(*cursor)++] = UINT32_C(0x53083c00) | source << 5 | 21u;
        source = 21u;
    }
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_first));
    words[(*cursor)++] = UINT32_C(0xeb11021f); below = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xb1000000) | (uint32_t)item->width << 10 | 16u << 5 | 18u;
    overflow = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_last));
    words[(*cursor)++] = UINT32_C(0xeb11025f); above = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
    readable = &words[(*cursor)++]; writable = &words[(*cursor)++];
    if (item->width != 1u) {
        emit_constant(words, cursor, 19u, item->width - 1u);
        words[(*cursor)++] = UINT32_C(0xea00001f) | 19u << 16 | 16u << 5;
        unaligned = &words[(*cursor)++];
    } else unaligned = NULL;
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_delta));
    words[(*cursor)++] = UINT32_C(0x8b110211);
    words[(*cursor)++] = UINT32_C(0xaa1003f8); /* preserve validated guest EA in x24 */
    /* Preserve the replacement before x20 is loaded with the comparand. */
    if (source != 21u)
        words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003f5) : UINT32_C(0x2a0003f5)) |
                             source << 16; /* mov x/w21, source */
    if (item->width == 1u)
        words[(*cursor)++] = UINT32_C(0xd3401c14); /* ubfx x20,x0,#0,#8 */
    else if (item->width == 2u)
        words[(*cursor)++] = UINT32_C(0xd3403c14); /* ubfx x20,x0,#0,#16 */
    else
        words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003f4) : UINT32_C(0x2a0003f4));
    {
        uint32_t base = item->width == 8u ? UINT32_C(0xc8e0fc00) :
                        item->width == 4u ? UINT32_C(0x88e0fc00) :
                        item->width == 2u ? UINT32_C(0x48e0fc00) : UINT32_C(0x08e0fc00);
        words[(*cursor)++] = base | 20u << 16 | 17u << 5 | 21u; /* casal expected,new,[host] */
    }
    words[(*cursor)++] = UINT32_C(0xaa1403f9); /* mov x25,x20: returned old value */
    compare.memory_operand = 0u; compare.memory_write = 0u;
    compare.destination = 0u; compare.destination_high = 0u;
    compare.source = 25u; compare.source_high = 0u;
    compare.has_immediate = 0u; compare.alu_kind = 7u; compare.flags_only = 1u;
    compare.preserve_carry = 0u; compare.preserve_flags = 0u; compare.unary_neg = 0u;
    hl_x86_emit_alu(words, cursor, &compare);
    matched = &words[(*cursor)++]; /* b.eq publication */
    if (item->width == 1u)
        words[(*cursor)++] = UINT32_C(0xb3401f20) | 25u << 5; /* bfi x0,x25,#0,#8 */
    else if (item->width == 2u)
        words[(*cursor)++] = UINT32_C(0xb3403f20) | 25u << 5; /* bfi x0,x25,#0,#16 */
    else
        words[(*cursor)++] = (item->width == 8u ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) |
                             25u << 16;
    done = &words[(*cursor)++];
    branch(matched, &words[*cursor], 0u);
    words[(*cursor)++] = UINT32_C(0xaa1803f0); /* restore guest EA */
    words[(*cursor)++] = UINT32_C(0xd2800031);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, memory_written));
    words[(*cursor)++] = UINT32_C(0x91000000) | (uint32_t)item->width << 10 | 16u << 5 | 18u;
    hl_x86_emit_dirty(words, cursor);
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
    words[(*cursor)++] = load_word(18, offsetof(hl_native_x86_64_cpu, executable_written));
    words[(*cursor)++] = UINT32_C(0xaa120231);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, executable_written));
    published = &words[(*cursor)++];

    uint32_t *fault = &words[*cursor];
    branch(below, fault, 3u); branch(overflow, fault, 2u); branch(above, fault, 8u);
    test(readable, fault, 0u); test(writable, fault, 1u);
    words[(*cursor)++] = store_word(16, offsetof(hl_native_x86_64_cpu, fault_address));
    words[(*cursor)++] = UINT32_C(0xd2800071);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_access));
    emit_constant(words, cursor, 17u, item->width);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_size));
    emit_constant(words, cursor, 17u, item->pc);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
    words[(*cursor)++] = UINT32_C(0xd2800071);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
    if (item->live_chain) { hl_x86_finish_chain(words, cursor); hl_x86_spill_all(words, cursor); }
    words[(*cursor)++] = UINT32_C(0xd65f03c0);
    uint32_t *fallback = &words[*cursor];
    if (unaligned != NULL) branch(unaligned, fallback, 1u);
    emit_constant(words, cursor, 17u, item->pc);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
    emit_constant(words, cursor, 17u, HL_NATIVE_EXIT_FALLBACK);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
    if (item->live_chain) { hl_x86_finish_chain(words, cursor); hl_x86_spill_all(words, cursor); }
    words[(*cursor)++] = UINT32_C(0xd65f03c0);
    branch(done, &words[*cursor], 14u);
    branch(published, &words[*cursor], 14u);
}

static uint32_t vector_operation_words(const instruction *item) {
    switch (item->vector_kind) {
    case VECTOR_BYTE_MASK: return 7u;
    case VECTOR_SCALAR_SQRT_DOUBLE:
    case VECTOR_SCALAR_ADD_DOUBLE:
    case VECTOR_SCALAR_MUL_DOUBLE:
    case VECTOR_SCALAR_SUB_DOUBLE:
    case VECTOR_SCALAR_DIV_DOUBLE:
        return 28u + constant_words(item->pc) + (item->live_chain != 0u ? 18u : 0u);
    case VECTOR_SCALAR_COMPARE_DOUBLE: return 13u;
    case VECTOR_TRUNC_DOUBLE_SIGNED:
        return 4u + constant_words(item->width == 8u ? UINT64_C(0x43e0000000000000) :
                                                      UINT64_C(0x41e0000000000000)) +
               constant_words(item->width == 8u ? UINT64_C(0x8000000000000000) :
                                                  UINT64_C(0x80000000));
    case VECTOR_FLOAT_ARITHMETIC:
        return 28u + constant_words(item->pc) + (item->live_chain != 0u ? 18u : 0u);
    case VECTOR_STRING_EQUAL_EACH: return 96u;
    case VECTOR_INSERT_WORD: return 1u;
    case VECTOR_PACKED_SHIFT: {
        uint32_t words = constant_words((uint64_t)item->vector_lane * 8u) + 6u;
        words += item->variable_count != 0u ? 1u : constant_words(item->vector_immediate);
        return words + (item->vector_subopcode != 6u ? 1u : 0u);
    }
    case VECTOR_BYTE_SHIFT: return 2u;
    default: return 1u;
    }
}

uint32_t hl_x86_vector_words(const instruction *item) {
    uint32_t operation = vector_operation_words(item);
    uint32_t aligned = item->vector_aligned == 0u ? 0u :
                       6u + constant_words(item->pc) + (item->live_chain != 0u ? 19u : 0u);
    if (item->memory_operand == 0u) return operation;
    return hl_x86_address_words(item) - 1u + (item->memory_write != 0u ? 42u : 24u) +
           (item->memory_write == 0u ? hl_x86_read_cache_words() : 0u) +
           (item->live_chain != 0u ? 19u : 0u) +
           constant_words(item->pc) +
           (item->vector_kind != VECTOR_COPY && item->memory_write == 0u ? operation : 0u) + aligned;
}

static void emit_vector_operation(uint32_t *words, uint32_t *cursor, const instruction *item) {
    uint32_t destination = item->destination;
    uint32_t source = item->source;
    uint32_t lane = item->vector_lane;

    if (item->vector_kind == VECTOR_STRING_EQUAL_EACH) {
        uint64_t clear = UINT64_C(1) | UINT64_C(1) << 2 | UINT64_C(1) << 4 |
                         UINT64_C(1) << 6 | UINT64_C(1) << 7 | UINT64_C(1) << 11;
        uint32_t immediate = item->vector_immediate;

        /* IntRes1 for implicit-length byte equal-each.  Scratch vectors are
         * outside the guest XMM bank; operand two has already passed the
         * complete generic memory guard when it names v16. */
        words[(*cursor)++] = UINT32_C(0x6e208c00) | source << 16 | destination << 5 | 18u;
        words[(*cursor)++] = UINT32_C(0x4e209800) | destination << 5 | 19u;
        words[(*cursor)++] = UINT32_C(0x4e209800) | source << 5 | 21u;
#define EMIT_MASK(vector, target) do { \
            words[(*cursor)++] = UINT32_C(0x6f090400) | (vector) << 5 | 17u; \
            words[(*cursor)++] = UINT32_C(0x6f001400) | 25u << 16 | 17u << 5 | 17u; \
            words[(*cursor)++] = UINT32_C(0x6f001400) | 50u << 16 | 17u << 5 | 17u; \
            words[(*cursor)++] = UINT32_C(0x6f001400) | 100u << 16 | 17u << 5 | 17u; \
            words[(*cursor)++] = UINT32_C(0x0e003c00) | 1u << 16 | 17u << 5 | 16u; \
            words[(*cursor)++] = UINT32_C(0x0e003c00) | 17u << 16 | 17u << 5 | (target); \
            words[(*cursor)++] = UINT32_C(0x2a000000) | (target) << 16 | 8u << 10 | 16u << 5 | (target); \
        } while (0)
        EMIT_MASK(18u, 19u);
        EMIT_MASK(19u, 21u);
        EMIT_MASK(21u, 24u);
#undef EMIT_MASK
        emit_constant(words, cursor, 16u, UINT64_C(0x10000));
        words[(*cursor)++] = UINT32_C(0x2a000000) | 16u << 16 | 21u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x5ac00000) | 17u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x5ac01000) | 17u << 5 | 22u;
        words[(*cursor)++] = UINT32_C(0x2a000000) | 16u << 16 | 24u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x5ac00000) | 17u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x5ac01000) | 17u << 5 | 23u;
        emit_constant(words, cursor, 16u, 1u);
        words[(*cursor)++] = UINT32_C(0x1ac02000) | 22u << 16 | 16u << 5 | 25u;
        words[(*cursor)++] = UINT32_C(0x51000400) | 25u << 5 | 25u;
        words[(*cursor)++] = UINT32_C(0x1ac02000) | 23u << 16 | 16u << 5 | 26u;
        words[(*cursor)++] = UINT32_C(0x51000400) | 26u << 5 | 26u;
        words[(*cursor)++] = UINT32_C(0x0a000000) | 25u << 16 | 19u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x0a000000) | 26u << 16 | 17u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x2a000000) | 26u << 16 | 25u << 5 | 16u;
        words[(*cursor)++] = UINT32_C(0x2a200000) | 16u << 16 | 17u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x53003c00) | 17u << 5 | 17u;
        if ((immediate & 0x10u) != 0u) {
            if ((immediate & 0x20u) != 0u) {
                words[(*cursor)++] = UINT32_C(0x4a000000) | 26u << 16 | 17u << 5 | 17u;
            } else {
                emit_constant(words, cursor, 16u, UINT64_C(0xffff));
                words[(*cursor)++] = UINT32_C(0x4a000000) | 16u << 16 | 17u << 5 | 17u;
            }
            words[(*cursor)++] = UINT32_C(0x53003c00) | 17u << 5 | 17u;
        }
        if ((immediate & 0x40u) == 0u) {
            emit_constant(words, cursor, 16u, UINT64_C(0x10000));
            words[(*cursor)++] = UINT32_C(0x2a000000) | 17u << 16 | 16u << 5 | 16u;
            words[(*cursor)++] = UINT32_C(0x5ac00000) | 16u << 5 | 16u;
            words[(*cursor)++] = UINT32_C(0x5ac01000) | 16u << 5 | 1u;
        } else {
            words[(*cursor)++] = UINT32_C(0x5ac01000) | 17u << 5 | 16u;
            emit_constant(words, cursor, 25u, 31u);
            words[(*cursor)++] = UINT32_C(0x4b000000) | 16u << 16 | 25u << 5 | 16u;
            words[(*cursor)++] = UINT32_C(0x7100001f) | 17u << 5;
            emit_constant(words, cursor, 25u, 16u);
            words[(*cursor)++] = UINT32_C(0x1a800000) | 16u << 16 | 0u << 12 | 25u << 5 | 1u;
        }

        /* Publish all six defined flags together after RCX is ready. */
        words[(*cursor)++] = load_word(20u, offsetof(hl_native_x86_64_cpu, flags));
        emit_constant(words, cursor, 25u, clear);
        words[(*cursor)++] = UINT32_C(0x8a200000) | 25u << 16 | 20u << 5 | 20u;
        words[(*cursor)++] = UINT32_C(0x7100001f) | 17u << 5;
        words[(*cursor)++] = UINT32_C(0x1a9f07f9); /* cset w25,ne: CF */
        words[(*cursor)++] = UINT32_C(0x2a000000) | 25u << 16 | 20u << 5 | 20u;
        words[(*cursor)++] = UINT32_C(0x710042df); /* cmp w22,#16 */
        words[(*cursor)++] = UINT32_C(0x1a9f27f9); /* cset w25,lo */
        words[(*cursor)++] = UINT32_C(0x2a000000) | 25u << 16 | 7u << 10 | 20u << 5 | 20u;
        words[(*cursor)++] = UINT32_C(0x710042ff); /* cmp w23,#16 */
        words[(*cursor)++] = UINT32_C(0x1a9f27f9); /* cset w25,lo */
        words[(*cursor)++] = UINT32_C(0x2a000000) | 25u << 16 | 6u << 10 | 20u << 5 | 20u;
        words[(*cursor)++] = UINT32_C(0x12000239); /* and w25,w17,#1 */
        words[(*cursor)++] = UINT32_C(0x2a000000) | 25u << 16 | 11u << 10 | 20u << 5 | 20u;
        words[(*cursor)++] = store_word(20u, offsetof(hl_native_x86_64_cpu, flags));
    } else if (item->vector_kind >= VECTOR_SCALAR_SQRT_DOUBLE &&
        item->vector_kind <= VECTOR_SCALAR_DIV_DOUBLE) {
        uint32_t base = item->vector_kind == VECTOR_SCALAR_SQRT_DOUBLE ? UINT32_C(0x1e61c000) :
                        item->vector_kind == VECTOR_SCALAR_ADD_DOUBLE ? UINT32_C(0x1e602800) :
                        item->vector_kind == VECTOR_SCALAR_MUL_DOUBLE ? UINT32_C(0x1e600800) :
                        item->vector_kind == VECTOR_SCALAR_SUB_DOUBLE ? UINT32_C(0x1e603800) :
                                                                       UINT32_C(0x1e601800);
        if (item->vector_kind == VECTOR_SCALAR_SQRT_DOUBLE)
            words[(*cursor)++] = base | source << 5 | 18u;
        else
            words[(*cursor)++] = base | source << 16 | destination << 5 | 18u;
        words[(*cursor)++] = UINT32_C(0x1e602000) | 18u << 16 | 18u << 5; /* fcmp d18,d18 */
        uint32_t ordered = (*cursor)++;
        emit_constant(words, cursor, 16u, item->pc);
        words[(*cursor)++] = store_word(16u, offsetof(hl_native_x86_64_cpu, program));
        emit_constant(words, cursor, 16u, HL_NATIVE_EXIT_FALLBACK);
        words[(*cursor)++] = store_word(16u, offsetof(hl_native_x86_64_cpu, reason));
        if (item->live_chain != 0u) {
            hl_x86_finish_chain(words, cursor);
            hl_x86_spill_all(words, cursor);
        }
        words[(*cursor)++] = UINT32_C(0xd65f03c0);
        words[ordered] = UINT32_C(0x54000000) |
                         (((*cursor - ordered) & UINT32_C(0x7ffff)) << 5) | 7u; /* b.vc */
        words[(*cursor)++] = UINT32_C(0x6e080400) | 18u << 5 | destination; /* ins vd.d[0],v18.d[0] */
    } else if (item->vector_kind == VECTOR_FLOAT_ARITHMETIC) {
        uint32_t base;
        uint32_t ordered;
        if (item->condition != 0u) {
            base = item->vector_subopcode == 0x58u ? UINT32_C(0x1e202800) :
                   item->vector_subopcode == 0x59u ? UINT32_C(0x1e200800) :
                   item->vector_subopcode == 0x5cu ? UINT32_C(0x1e203800) : UINT32_C(0x1e201800);
            if (item->vector_lane == 8u) base |= UINT32_C(0x00400000);
            words[(*cursor)++] = base | source << 16 | destination << 5 | 18u;
            words[(*cursor)++] = (item->vector_lane == 8u ? UINT32_C(0x1e602000) :
                                                             UINT32_C(0x1e202000)) |
                                 18u << 16 | 18u << 5; /* fcmp result,result */
            ordered = (*cursor)++;
        } else {
            base = item->vector_subopcode == 0x58u ? UINT32_C(0x4e20d400) :
                   item->vector_subopcode == 0x59u ? UINT32_C(0x6e20dc00) :
                   item->vector_subopcode == 0x5cu ? UINT32_C(0x4ea0d400) : UINT32_C(0x6e20fc00);
            if (item->vector_lane == 8u) base |= UINT32_C(0x00400000);
            words[(*cursor)++] = base | source << 16 | destination << 5 | 18u;
            words[(*cursor)++] = (item->vector_lane == 8u ? UINT32_C(0x4e60e400) :
                                                             UINT32_C(0x4e20e400)) |
                                 18u << 16 | 18u << 5 | 19u; /* fcmeq result,result */
            words[(*cursor)++] = UINT32_C(0x6e31a800) | 19u << 5 | 19u; /* uminv b19,v19.16b */
            words[(*cursor)++] = UINT32_C(0x1e260000) | 19u << 5 | 16u; /* fmov w16,s19 */
            ordered = (*cursor)++;
        }
        emit_constant(words, cursor, 16u, item->pc);
        words[(*cursor)++] = store_word(16u, offsetof(hl_native_x86_64_cpu, program));
        emit_constant(words, cursor, 16u, HL_NATIVE_EXIT_FALLBACK);
        words[(*cursor)++] = store_word(16u, offsetof(hl_native_x86_64_cpu, reason));
        if (item->live_chain != 0u) {
            hl_x86_finish_chain(words, cursor);
            hl_x86_spill_all(words, cursor);
        }
        words[(*cursor)++] = UINT32_C(0xd65f03c0);
        words[ordered] = (item->condition != 0u ? UINT32_C(0x54000000) : UINT32_C(0x35000000)) |
                         (((*cursor - ordered) & UINT32_C(0x7ffff)) << 5) |
                         (item->condition != 0u ? 7u : 16u); /* b.vc / cbnz w16 */
        if (item->condition != 0u)
            words[(*cursor)++] = (item->vector_lane == 8u ? UINT32_C(0x6e080400) :
                                                             UINT32_C(0x6e040400)) |
                                 18u << 5 | destination;
        else
            words[(*cursor)++] = UINT32_C(0x4ea01c00) | 18u << 16 | 18u << 5 | destination;
    } else if (item->vector_kind == VECTOR_SCALAR_COMPARE_DOUBLE) {
        uint64_t clear = UINT64_C(1) | UINT64_C(1) << 2 | UINT64_C(1) << 4 |
                         UINT64_C(1) << 6 | UINT64_C(1) << 7 | UINT64_C(1) << 11;
        words[(*cursor)++] = UINT32_C(0x1e602000) | (item->condition != 0u ? UINT32_C(0x10) : 0u) |
                             source << 16 | destination << 5; /* fcmp/fcmpe dd,ds */
        words[(*cursor)++] = UINT32_C(0x9a9f07e0) | (4u ^ 1u) << 12 | 19u; /* cset x19,mi: less */
        words[(*cursor)++] = UINT32_C(0x9a9f07e0) | (0u ^ 1u) << 12 | 20u; /* cset x20,eq */
        words[(*cursor)++] = UINT32_C(0x9a9f07e0) | (6u ^ 1u) << 12 | 21u; /* cset x21,vs: unordered */
        words[(*cursor)++] = load_word(22u, offsetof(hl_native_x86_64_cpu, flags));
        emit_constant(words, cursor, 23u, clear);
        words[(*cursor)++] = UINT32_C(0x8a200000) | 23u << 16 | 22u << 5 | 22u; /* bic flags,flags,mask */
        words[(*cursor)++] = UINT32_C(0xaa000000) | 21u << 16 | 19u << 5 | 19u; /* less | unordered */
        words[(*cursor)++] = UINT32_C(0xaa000000) | 19u << 16 | 22u << 5 | 22u; /* CF */
        words[(*cursor)++] = UINT32_C(0xaa000000) | 21u << 16 | 20u << 5 | 20u; /* equal | unordered */
        words[(*cursor)++] = UINT32_C(0xaa000000) | 20u << 16 | 6u << 10 | 22u << 5 | 22u; /* ZF */
        words[(*cursor)++] = UINT32_C(0xaa000000) | 21u << 16 | 2u << 10 | 22u << 5 | 22u; /* PF */
        words[(*cursor)++] = store_word(22u, offsetof(hl_native_x86_64_cpu, flags));
    } else if (item->vector_kind == VECTOR_TRUNC_DOUBLE_SIGNED) {
        uint64_t threshold = item->width == 8u ? UINT64_C(0x43e0000000000000) :
                                                 UINT64_C(0x41e0000000000000);
        uint64_t indefinite = item->width == 8u ? UINT64_C(0x8000000000000000) :
                                                  UINT64_C(0x80000000);
        words[(*cursor)++] = (item->width == 8u ? UINT32_C(0x9e780000) : UINT32_C(0x1e780000)) |
                             source << 5 | destination; /* fcvtzs x/w destination,dsource */
        emit_constant(words, cursor, 20u, threshold);
        words[(*cursor)++] = UINT32_C(0x9e670000) | 20u << 5 | 19u; /* fmov d19,x20 */
        words[(*cursor)++] = UINT32_C(0x1e602000) | 19u << 16 | source << 5; /* fcmp dsource,d19 */
        emit_constant(words, cursor, 20u, indefinite);
        words[(*cursor)++] = (item->width == 8u ? UINT32_C(0x9a800000) : UINT32_C(0x1a800000)) |
                             destination << 16 | 2u << 12 | 20u << 5 | destination; /* csel result,indef,result,cs */
    } else if (item->vector_kind == VECTOR_FROM_INTEGER)
        words[(*cursor)++] = (item->width == 8u ? UINT32_C(0x9e670000) : UINT32_C(0x1e270000)) |
                               source << 5 | destination;
    else if (item->vector_kind == VECTOR_TO_INTEGER)
        words[(*cursor)++] = (item->width == 8u ? UINT32_C(0x9e660000) : UINT32_C(0x1e260000)) |
                               source << 5 | destination;
    else if (item->vector_kind == VECTOR_ZERO_LOW)
        words[(*cursor)++] = UINT32_C(0x0ea01c00) | source << 16 | source << 5 | destination;
    else if (item->vector_kind == VECTOR_UNPACK_LOW || item->vector_kind == VECTOR_UNPACK_HIGH) {
        uint32_t base = lane == 1u ? UINT32_C(0x4e003800) : lane == 2u ? UINT32_C(0x4e403800) :
                        lane == 4u ? UINT32_C(0x4e803800) : UINT32_C(0x4ec03800);
        if (item->vector_kind == VECTOR_UNPACK_HIGH) base += UINT32_C(0x4000);
        words[(*cursor)++] = base | source << 16 | destination << 5 | destination;
    } else if (item->vector_kind == VECTOR_INSERT_WORD) {
        uint32_t insert_lane = item->vector_immediate & 7u;
        words[(*cursor)++] = UINT32_C(0x4e001c00) | ((insert_lane << 2) | 2u) << 16 |
                             source << 5 | destination;
    } else if (item->vector_kind == VECTOR_COMPARE_EQUAL) {
        uint32_t base = lane == 1u ? UINT32_C(0x6e208c00) : lane == 2u ? UINT32_C(0x6e608c00) :
                                                                        UINT32_C(0x6ea08c00);
        words[(*cursor)++] = base | source << 16 | destination << 5 | destination;
    } else if (item->vector_kind == VECTOR_XOR) {
        words[(*cursor)++] = UINT32_C(0x6e201c00) | source << 16 | destination << 5 | destination;
    } else if (item->vector_kind == VECTOR_AND) {
        words[(*cursor)++] = UINT32_C(0x4e201c00) | source << 16 | destination << 5 | destination;
    } else if (item->vector_kind == VECTOR_OR) {
        words[(*cursor)++] = UINT32_C(0x4ea01c00) | source << 16 | destination << 5 | destination;
    } else if (item->vector_kind == VECTOR_AND_NOT) {
        words[(*cursor)++] = UINT32_C(0x4e601c00) | destination << 16 | source << 5 | destination;
    } else if (item->vector_kind == VECTOR_ADD || item->vector_kind == VECTOR_SUBTRACT) {
        uint32_t base = item->vector_kind == VECTOR_ADD ? UINT32_C(0x4e208400) : UINT32_C(0x6e208400);
        base |= lane == 1u ? 0u : lane == 2u ? UINT32_C(0x00400000) :
                lane == 4u ? UINT32_C(0x00800000) : UINT32_C(0x00c00000);
        words[(*cursor)++] = base | source << 16 | destination << 5 | destination;
    } else if (item->vector_kind == VECTOR_BYTE_MASK) {
        words[(*cursor)++] = UINT32_C(0x6f090400) | source << 5 | 17u; /* ushr v17.16b,source,#7 */
        words[(*cursor)++] = UINT32_C(0x6f001400) | 25u << 16 | 17u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x6f001400) | 50u << 16 | 17u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x6f001400) | 100u << 16 | 17u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x0e013e30); /* umov w16,v17.b[0] */
        words[(*cursor)++] = UINT32_C(0x0e113e20) | destination; /* umov wd,v17.b[8] */
        words[(*cursor)++] = UINT32_C(0x2a000000) | destination << 16 | 8u << 10 | 16u << 5 | destination;
    } else if (item->vector_kind == VECTOR_PACKED_SHIFT) {
        uint32_t bits = lane * 8u;
        uint32_t amount = item->variable_count != 0u ? 0u : item->vector_immediate;
        uint32_t size = lane == 2u ? 1u : lane == 4u ? 2u : 3u;
        uint32_t imm5 = lane;
        if (item->variable_count != 0u)
            words[(*cursor)++] = UINT32_C(0x4e083c00) | source << 5 | 16u; /* umov x16,vs.d[0] */
        else
            emit_constant(words, cursor, 16u, amount);
        emit_constant(words, cursor, 19u, bits);
        words[(*cursor)++] = UINT32_C(0xd53b4216); /* mrs x22,nzcv */
        words[(*cursor)++] = UINT32_C(0xeb13021f); /* cmp x16,x19 */
        words[(*cursor)++] = UINT32_C(0x9a908270); /* csel x16,x19,x16,hi */
        words[(*cursor)++] = UINT32_C(0xd51b4216); /* msr nzcv,x22 */
        if (item->vector_subopcode != 6u)
            words[(*cursor)++] = UINT32_C(0xcb1003f0); /* neg x16 */
        words[(*cursor)++] = UINT32_C(0x4e000c00) | imm5 << 16 | 16u << 5 | 17u; /* dup shift */
        words[(*cursor)++] = (item->vector_subopcode == 4u ? UINT32_C(0x4e204400) :
                                                                  UINT32_C(0x6e204400)) |
                             size << 22 | 17u << 16 | destination << 5 | destination;
    } else if (item->vector_kind == VECTOR_BYTE_SHIFT) {
        uint32_t count = item->vector_immediate;
        if (count >= 16u)
            words[(*cursor)++] = UINT32_C(0x6e201c00) | destination << 16 |
                                 destination << 5 | destination; /* eor vd,vd,vd */
        else if (count == 0u)
            words[(*cursor)++] = UINT32_C(0x4ea01c00) | destination << 16 |
                                 destination << 5 | destination; /* mov vd,vd */
        else {
            words[(*cursor)++] = UINT32_C(0x6e201c00) | 17u << 16 | 17u << 5 | 17u; /* zero v17 */
            if (item->vector_subopcode == 3u)
                words[(*cursor)++] = UINT32_C(0x6e000000) | 17u << 16 | destination << 5 |
                                     count << 11 | destination; /* ext vd,vd,v17,#count */
            else
                words[(*cursor)++] = UINT32_C(0x6e000000) | destination << 16 | 17u << 5 |
                                     (16u - count) << 11 | destination; /* ext vd,v17,vd,#16-count */
        }
    } else
        words[(*cursor)++] = UINT32_C(0x4ea01c00) | source << 16 | source << 5 | destination;
}

void hl_x86_emit_vector(uint32_t *words, uint32_t *cursor, const instruction *item) {
    uint32_t *below;
    uint32_t *overflow;
    uint32_t *above;
    uint32_t *permission;
    uint32_t *skip;
    uint32_t *loaded_operation = NULL;
    uint32_t *cache_hits[HL_X86_READ_VIEWS];
    unsigned required = item->memory_write != 0u ? 2u : 1u;
    uint32_t width = item->vector_memory_width != 0u ? item->vector_memory_width : item->width;
    int scalar_vector = item->vector_kind == VECTOR_INSERT_WORD;

    if (item->memory_operand == 0u) {
        emit_vector_operation(words, cursor, item);
        return;
    }

    hl_x86_emit_address(words, cursor, item);
    --*cursor;
    uint32_t *unaligned = NULL;
    if (item->vector_aligned != 0u) {
        words[(*cursor)++] = UINT32_C(0xf2400e1f); /* tst x16,#15 */
        unaligned = &words[(*cursor)++];
    }
    if (item->memory_write == 0u)
        hl_x86_emit_read_cache(words, cursor, width,
                               item->vector_kind == VECTOR_COPY ? item->destination : 16u,
                               !scalar_vector, cache_hits);
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_first));
    words[(*cursor)++] = UINT32_C(0xeb11021f); /* cmp x16,x17 */
    below = &words[(*cursor)++];
    words[(*cursor)++] = UINT32_C(0xb1000000) | width << 10 | 16u << 5 | 18u;
    overflow = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_last));
    words[(*cursor)++] = UINT32_C(0xeb11025f); /* cmp x18,x17 */
    above = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
    permission = &words[(*cursor)++];
    words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_delta));
    words[(*cursor)++] = UINT32_C(0x8b110211); /* add x17,x16,x17 */
    if (item->memory_write != 0u) {
        words[(*cursor)++] = UINT32_C(0xd5033abf); /* dmb ishst: preserve x86 StoreStore ordering */
        words[(*cursor)++] = (width == 4u ? UINT32_C(0xbd000220) :
                               width == 8u ? UINT32_C(0xfd000220) : UINT32_C(0x3d800220)) |
                              item->source;
        words[(*cursor)++] = UINT32_C(0xd2800031);
        words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, memory_written));
        hl_x86_emit_dirty(words, cursor);
        words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
        words[(*cursor)++] = load_word(18, offsetof(hl_native_x86_64_cpu, executable_written));
        words[(*cursor)++] = UINT32_C(0xaa120231); /* orr x17,x17,x18: sticky permission latch */
        words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, executable_written));
    } else {
        words[(*cursor)++] = (width == 2u ? UINT32_C(0x79400220) :
                               width == 4u ? UINT32_C(0xbd400220) :
                               width == 8u ? UINT32_C(0xfd400220) : UINT32_C(0x3dc00220)) |
                              (item->vector_kind == VECTOR_COPY ? item->destination : 16u);
        words[(*cursor)++] = UINT32_C(0xd50339bf); /* dmb ishld: preserve x86 LoadLoad/LoadStore */
        if (item->vector_kind != VECTOR_COPY) {
            loaded_operation = &words[*cursor];
            emit_vector_operation(words, cursor, item);
        }
    }
    skip = &words[(*cursor)++];

    {
        uint32_t *miss = &words[*cursor];
        branch(below, miss, 3u);
        branch(overflow, miss, 2u);
        branch(above, miss, 8u);
        test(permission, miss, required - 1u);
    }
    words[(*cursor)++] = store_word(16, offsetof(hl_native_x86_64_cpu, fault_address));
    words[(*cursor)++] = required == 2u ? UINT32_C(0xd2800051) : UINT32_C(0xd2800031);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_access));
    emit_constant(words, cursor, 17, width);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, fault_size));
    emit_constant(words, cursor, 17, item->pc);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
    words[(*cursor)++] = UINT32_C(0xd2800071);
    words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
    if (item->live_chain != 0u) {
        hl_x86_finish_chain(words, cursor);
        hl_x86_spill_all(words, cursor);
    }
    words[(*cursor)++] = UINT32_C(0xd65f03c0);
    uint32_t *done = &words[*cursor];
    if (unaligned != NULL) {
        branch(unaligned, done, 1u);
        emit_constant(words, cursor, 17, item->pc);
        words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, program));
        words[(*cursor)++] = UINT32_C(0xd2800071);
        words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, reason));
        if (item->live_chain != 0u) {
            hl_x86_finish_chain(words, cursor);
            hl_x86_spill_all(words, cursor);
        }
        words[(*cursor)++] = UINT32_C(0xd65f03c0);
    }
    branch(skip, &words[*cursor], 14u);
    if (item->memory_write == 0u)
        hl_x86_patch_read_hits(cache_hits,
                               item->vector_kind == VECTOR_COPY ? &words[*cursor] : loaded_operation);
}
