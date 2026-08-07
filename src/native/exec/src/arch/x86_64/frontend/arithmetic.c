#include "private.h"

#include <stddef.h>

#include "../decode.h"
#include "../flags.h"
#include "../word.h"
#include "cpu.h"

uint32_t hl_x86_bit(unsigned destination, unsigned source, unsigned lsb) {
    return UINT32_C(0xd3400000) | lsb << 16 | lsb << 10 | source << 5 | destination;
}

static uint32_t lsl(unsigned destination, unsigned source, unsigned shift) {
    return UINT32_C(0xd3400000) | (64u - shift) << 16 | (63u - shift) << 10 | source << 5 | destination;
}

void hl_x86_emit_rflags(uint32_t *words, uint32_t *cursor) {
    words[(*cursor)++] = load_word(20, offsetof(hl_native_x86_64_cpu, flags));
    words[(*cursor)++] = hl_x86_bit(21, 20, 7);
    words[(*cursor)++] = lsl(21, 21, 31);
    words[(*cursor)++] = hl_x86_bit(22, 20, 6);
    words[(*cursor)++] = lsl(22, 22, 30);
    words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 21u << 5 | 21u;
    words[(*cursor)++] = hl_x86_bit(22, 20, 0);
    emit_constant(words, cursor, 23, 1);
    words[(*cursor)++] = UINT32_C(0xca000000) | 23u << 16 | 22u << 5 | 22u;
    words[(*cursor)++] = lsl(22, 22, 29);
    words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 21u << 5 | 21u;
    words[(*cursor)++] = hl_x86_bit(22, 20, 11);
    words[(*cursor)++] = lsl(22, 22, 28);
    words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 21u << 5 | 21u;
    words[(*cursor)++] = UINT32_C(0xd51b4200) | 21u;
}

void hl_x86_emit_nzcv(uint32_t *words, uint32_t *cursor) {
    words[(*cursor)++] = UINT32_C(0xd53b4200) | 21u;
    words[(*cursor)++] = load_word(20, offsetof(hl_native_x86_64_cpu, flags));
    emit_constant(words, cursor, 23, HL_X86_RFLAGS_NZCV_MASK);
    words[(*cursor)++] = UINT32_C(0x8a200000) | 23u << 16 | 20u << 5 | 20u;
    words[(*cursor)++] = hl_x86_bit(22, 21, 31);
    words[(*cursor)++] = lsl(22, 22, 7);
    words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 20u << 5 | 20u;
    words[(*cursor)++] = hl_x86_bit(22, 21, 30);
    words[(*cursor)++] = lsl(22, 22, 6);
    words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 20u << 5 | 20u;
    words[(*cursor)++] = hl_x86_bit(22, 21, 29);
    emit_constant(words, cursor, 23, 1);
    words[(*cursor)++] = UINT32_C(0xca000000) | 23u << 16 | 22u << 5 | 22u;
    words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 20u << 5 | 20u;
    words[(*cursor)++] = hl_x86_bit(22, 21, 28);
    words[(*cursor)++] = lsl(22, 22, 11);
    words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 20u << 5 | 20u;
    words[(*cursor)++] = store_word(20, offsetof(hl_native_x86_64_cpu, flags));
}

void hl_x86_emit_nzcv_keep_carry(uint32_t *words, uint32_t *cursor) {
    words[(*cursor)++] = load_word(24, offsetof(hl_native_x86_64_cpu, flags));
    words[(*cursor)++] = hl_x86_bit(24, 24, 0);
    hl_x86_emit_nzcv(words, cursor);
    words[(*cursor)++] = load_word(20, offsetof(hl_native_x86_64_cpu, flags));
    words[(*cursor)++] = UINT32_C(0x927ff800) | 20u << 5 | 20u; /* and x20,x20,#~1 */
    words[(*cursor)++] = UINT32_C(0xaa000000) | 24u << 16 | 20u << 5 | 20u;
    words[(*cursor)++] = store_word(20, offsetof(hl_native_x86_64_cpu, flags));
}

static uint32_t move_register(unsigned destination, unsigned source, int wide) {
    return (wide ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) | source << 16 | destination;
}

static uint32_t extract_byte(unsigned destination, unsigned source, unsigned high) {
    unsigned lsb = high ? 8u : 0u;

    return UINT32_C(0xd3400000) | lsb << 16 | (lsb + 7u) << 10 | source << 5 | destination;
}

static void emit_carry(uint32_t *words, uint32_t *cursor, int subtract) {
    words[(*cursor)++] = load_word(19, offsetof(hl_native_x86_64_cpu, flags));
    words[(*cursor)++] = hl_x86_bit(19, 19, 0);
    if (subtract) {
        emit_constant(words, cursor, 23, 1);
        words[(*cursor)++] = UINT32_C(0xca000000) | 23u << 16 | 19u << 5 | 19u;
    }
    words[(*cursor)++] = lsl(19, 19, 29);
    words[(*cursor)++] = UINT32_C(0xd51b4200) | 19u;
}

static void emit_add_flags(uint32_t *words, uint32_t *cursor) {
    words[(*cursor)++] = UINT32_C(0xd53b4200) | 20u;
    emit_constant(words, cursor, 22, UINT64_C(1) << 29);
    words[(*cursor)++] = UINT32_C(0xca000000) | 22u << 16 | 20u << 5 | 20u;
    words[(*cursor)++] = UINT32_C(0xd51b4200) | 20u;
}

static void emit_pfaf(uint32_t *words, uint32_t *cursor, int logical) {
    words[(*cursor)++] = load_word(20, offsetof(hl_native_x86_64_cpu, flags));
    emit_constant(words, cursor, 23, HL_X86_RFLAGS_PF | HL_X86_RFLAGS_AF |
                                      (logical ? HL_X86_RFLAGS_CF | HL_X86_RFLAGS_OF : 0));
    words[(*cursor)++] = UINT32_C(0x8a200000) | 23u << 16 | 20u << 5 | 20u;
    words[(*cursor)++] = move_register(22, 18, 1);
    words[(*cursor)++] = UINT32_C(0xca400000) | 22u << 16 | 4u << 10 | 22u << 5 | 22u;
    words[(*cursor)++] = UINT32_C(0xca400000) | 22u << 16 | 2u << 10 | 22u << 5 | 22u;
    words[(*cursor)++] = UINT32_C(0xca400000) | 22u << 16 | 1u << 10 | 22u << 5 | 22u;
    emit_constant(words, cursor, 23, 1);
    words[(*cursor)++] = UINT32_C(0xca000000) | 23u << 16 | 22u << 5 | 22u;
    words[(*cursor)++] = hl_x86_bit(22, 22, 0);
    words[(*cursor)++] = lsl(22, 22, 2);
    words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 20u << 5 | 20u;
    if (!logical) {
        words[(*cursor)++] = UINT32_C(0xca000000) | 17u << 16 | 16u << 5 | 22u;
        words[(*cursor)++] = UINT32_C(0xca000000) | 18u << 16 | 22u << 5 | 22u;
        words[(*cursor)++] = hl_x86_bit(22, 22, 4);
        words[(*cursor)++] = lsl(22, 22, 4);
        words[(*cursor)++] = UINT32_C(0xaa000000) | 22u << 16 | 20u << 5 | 20u;
    }
    words[(*cursor)++] = store_word(20, offsetof(hl_native_x86_64_cpu, flags));
}

uint32_t hl_x86_alu_words(const instruction *item) {
    if (item->memory_operand != 0u) {
        instruction load = *item;
        instruction alu = *item;
        uint32_t result;

        load.operation = OP_LOAD;
        load.destination = 18u;
        load.memory_operand = 0u;
        load.memory_write = item->memory_write;
        if (item->has_immediate != 0u || item->memory_destination != 0u) {
            alu.destination = 18u;
            alu.destination_high = 0u;
        } else {
            alu.source = 18u;
        }
        alu.memory_operand = 0u;
        alu.immediate = item->operand_immediate;
        result = hl_x86_load_words(&load) + hl_x86_alu_words(&alu);
        if (item->memory_write != 0u) result += 23u;
        return result;
    }
    int logical = item->alu_kind == 1u || item->alu_kind == 4u || item->alu_kind == 6u;
    uint64_t immediate = item->memory_operand != 0u ? item->operand_immediate : item->immediate;
    uint32_t words = 1u + (item->has_immediate ? constant_words(immediate) : 1u) +
                     (logical ? 2u : 1u) +
                     (item->nzcv_dead ? 0u : 18u + (item->preserve_carry ? 6u : 0u)) +
                     (item->pfaf_dead ? 0u : (logical ? 13u : 18u));
    /* A logical op's CF and OF are cleared by the PF/AF tail, so eliding only that half still owes
       them; the NZCV transfer leaves CF set for a logical result. */
    if (logical && item->nzcv_dead == 0u && item->pfaf_dead != 0u)
        words += 3u + constant_words(HL_X86_RFLAGS_CF | HL_X86_RFLAGS_OF);
    if (item->preserve_flags != 0u) words += 2u;

    if (item->alu_kind == 2u) words += 4u;
    if (item->alu_kind == 3u) words += 6u;
    if ((item->alu_kind == 0u || item->alu_kind == 2u) && !item->nzcv_dead) words += 5u;
    if (item->width == 1u || item->width == 2u) words += 5u;
    if ((item->alu_kind == 2u || item->alu_kind == 3u) && item->width < 4u)
        words += 5u + constant_words(item->width == 1u ? UINT64_C(0xffffff) : UINT64_C(0xffff));
    if (!item->flags_only) ++words;
    return words;
}

void hl_x86_emit_alu(uint32_t *words, uint32_t *cursor, const instruction *item) {
    if (item->memory_operand != 0u) {
        instruction load = *item;
        instruction alu = *item;

        load.operation = OP_LOAD;
        load.destination = 18u;
        load.memory_operand = 0u;
        load.memory_write = item->memory_write;
        hl_x86_emit_load(words, cursor, &load);
        if (item->memory_write != 0u)
            words[(*cursor)++] = move_register(24u, 17u, 1);
        if (item->has_immediate != 0u || item->memory_destination != 0u) {
            alu.destination = 18u;
            alu.destination_high = 0u;
        } else {
            alu.source = 18u;
        }
        alu.memory_operand = 0u;
        alu.immediate = item->operand_immediate;
        hl_x86_emit_alu(words, cursor, &alu);
        if (item->memory_write != 0u) {
            words[(*cursor)++] = move_register(25u, 18u, 1);
            words[(*cursor)++] = (item->width == 1u ? UINT32_C(0x39000300) :
                                   item->width == 2u ? UINT32_C(0x79000300) :
                                   item->width == 4u ? UINT32_C(0xb9000300) : UINT32_C(0xf9000300)) |
                                  24u << 5 | 25u;
            words[(*cursor)++] = UINT32_C(0xd2800031);
            words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, memory_written));
            words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_delta));
            words[(*cursor)++] = UINT32_C(0xcb110310); /* sub x16,x24,x17: recover guest address */
            words[(*cursor)++] = UINT32_C(0x91000012) | (uint32_t)item->width << 10 | 16u << 5;
            hl_x86_emit_dirty(words, cursor);
            words[(*cursor)++] = load_word(17, offsetof(hl_native_x86_64_cpu, memory_permissions));
            words[(*cursor)++] = load_word(18, offsetof(hl_native_x86_64_cpu, executable_written));
            words[(*cursor)++] = UINT32_C(0xaa120231); /* orr x17,x17,x18: sticky permission latch */
            words[(*cursor)++] = store_word(17, offsetof(hl_native_x86_64_cpu, executable_written));
        }
        return;
    }
    int wide = item->width == 8u;
    int logical = item->alu_kind == 1u || item->alu_kind == 4u || item->alu_kind == 6u;
    uint32_t base;

    if (item->preserve_flags != 0u)
        words[(*cursor)++] = load_word(25u, offsetof(hl_native_x86_64_cpu, flags));
    words[(*cursor)++] = item->unary_neg != 0u ? move_register(16u, 31u, wide) :
                            item->width == 1u ? extract_byte(16, item->destination, item->destination_high) :
                                               move_register(16, item->destination, wide);
    if (item->has_immediate)
        emit_constant(words, cursor, 17,
                      item->memory_operand != 0u ? item->operand_immediate : item->immediate);
    else if (item->unary_neg != 0u)
        words[(*cursor)++] = item->width == 1u ? extract_byte(17, item->destination, item->destination_high) :
                                                move_register(17, item->destination, wide);
    else
        words[(*cursor)++] = item->width == 1u ?
                                extract_byte(17, item->source, item->source_high != 0) :
                                move_register(17, item->source, wide);
    if (item->width == 1u || item->width == 2u) {
        unsigned shift = item->width == 1u ? 24u : 16u;
        words[(*cursor)++] = lsl(16, 16, shift);
        words[(*cursor)++] = lsl(17, 17, shift);
        if (item->alu_kind == 2u || item->alu_kind == 3u) {
            words[(*cursor)++] = load_word(23, offsetof(hl_native_x86_64_cpu, flags));
            words[(*cursor)++] = hl_x86_bit(23, 23, 0);
            words[(*cursor)++] = UINT32_C(0x4b1703f7); /* neg w23, w23 */
            emit_constant(words, cursor, 22, item->width == 1u ? UINT64_C(0xffffff) : UINT64_C(0xffff));
            words[(*cursor)++] = UINT32_C(0x0a1602f7); /* and w23, w23, w22 */
            words[(*cursor)++] = UINT32_C(0x0b170231); /* add w17, w17, w23 */
        }
    }
    if (item->alu_kind == 2u || item->alu_kind == 3u) emit_carry(words, cursor, item->alu_kind == 3u);
    if (item->alu_kind == 0u) base = UINT32_C(0x2b000000);
    else if (item->alu_kind == 2u) base = UINT32_C(0x3a000000);
    else if (item->alu_kind == 3u) base = UINT32_C(0x7a000000);
    else if (item->alu_kind == 5u || item->alu_kind == 7u) base = UINT32_C(0x6b000000);
    else if (item->alu_kind == 1u) base = UINT32_C(0x2a000000);
    else if (item->alu_kind == 4u) base = UINT32_C(0x0a000000);
    else base = UINT32_C(0x4a000000);
    words[(*cursor)++] = base | (wide ? UINT32_C(0x80000000) : 0) | 17u << 16 | 16u << 5 | 18u;
    if (logical)
        words[(*cursor)++] = UINT32_C(0x6a00001f) | (wide ? UINT32_C(0x80000000) : 0) |
                             18u << 16 | 18u << 5;
    if ((item->alu_kind == 0u || item->alu_kind == 2u) && item->nzcv_dead == 0u)
        emit_add_flags(words, cursor);
    if (item->nzcv_dead == 0u) {
        if (item->preserve_carry)
            hl_x86_emit_nzcv_keep_carry(words, cursor);
        else
            hl_x86_emit_nzcv(words, cursor);
    }
    if (logical && item->nzcv_dead == 0u && item->pfaf_dead != 0u) {
        words[(*cursor)++] = load_word(20, offsetof(hl_native_x86_64_cpu, flags));
        emit_constant(words, cursor, 23, HL_X86_RFLAGS_CF | HL_X86_RFLAGS_OF);
        words[(*cursor)++] = UINT32_C(0x8a200000) | 23u << 16 | 20u << 5 | 20u;
        words[(*cursor)++] = store_word(20, offsetof(hl_native_x86_64_cpu, flags));
    }
    if (item->width == 1u || item->width == 2u) {
        unsigned shift = item->width == 1u ? 24u : 16u;
        words[(*cursor)++] = UINT32_C(0x53007c00) | shift << 16 | 16u << 5 | 16u;
        words[(*cursor)++] = UINT32_C(0x53007c00) | shift << 16 | 17u << 5 | 17u;
        words[(*cursor)++] = UINT32_C(0x53007c00) | shift << 16 | 18u << 5 | 18u;
    }
    if (item->pfaf_dead == 0u) emit_pfaf(words, cursor, logical);
    if (item->preserve_flags != 0u)
        words[(*cursor)++] = store_word(25u, offsetof(hl_native_x86_64_cpu, flags));
    if (!item->flags_only) {
        if (item->width == 1u)
            words[(*cursor)++] = (item->destination_high != 0u ? UINT32_C(0xb3781c00) :
                                                                  UINT32_C(0xb3401c00)) |
                                   18u << 5 | item->destination;
        else if (item->width == 2u)
            words[(*cursor)++] = UINT32_C(0xb3403c00) | 18u << 5 | item->destination;
        else
            words[(*cursor)++] = move_register(item->destination, 18, wide);
    }
}

uint32_t hl_x86_accumulator_words(const instruction *item) {
    if (item->width != 2u) return 1u;
    return item->source_high != 0u ? 3u : 2u;
}

void hl_x86_emit_accumulator(uint32_t *words, uint32_t *cursor, const instruction *item) {
    if (item->source_high == 0u) {
        if (item->width == 2u) {
            words[(*cursor)++] = UINT32_C(0x93401c10); /* sxtb x16,w0 */
            words[(*cursor)++] = UINT32_C(0xb3403e00); /* bfxil x0,x16,#0,#16 */
        } else if (item->width == 4u) {
            words[(*cursor)++] = UINT32_C(0x13003c00); /* sxth w0,w0 */
        } else {
            words[(*cursor)++] = UINT32_C(0x93407c00); /* sxtw x0,w0 */
        }
    } else if (item->width == 2u) {
        words[(*cursor)++] = UINT32_C(0x93403c10); /* sxth x16,w0 */
        words[(*cursor)++] = UINT32_C(0x937ffe10); /* asr x16,x16,#63 */
        words[(*cursor)++] = UINT32_C(0xb3403e02); /* bfxil x2,x16,#0,#16 */
    } else if (item->width == 4u) {
        words[(*cursor)++] = UINT32_C(0x131f7c02); /* asr w2,w0,#31 */
    } else {
        words[(*cursor)++] = UINT32_C(0x937ffc02); /* asr x2,x0,#63 */
    }
}
