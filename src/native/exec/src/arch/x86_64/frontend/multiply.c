#include "private.h"

#include "../decode.h"
#include "../flags.h"
#include "../word.h"
#include "cpu.h"
#include "../../../executor.h"

static int fail(decode *block, size_t *cursor, size_t start, hl_x86_a64_status status) {
    *cursor = start;
    block->status = status;
    block->exit = HL_X86_A64_INTERPRETER;
    return 0;
}

int hl_x86_decode_imul(const hl_x86_a64_request *request, decode *block, instruction *item,
                       uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                       size_t start, size_t *cursor) {
    uint8_t modrm;
    size_t immediate_size = 0;
    if (*cursor >= request->guest_size || *cursor - start >= 15u)
        return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
    modrm = request->guest_bytes[*cursor];
    item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
    item->width = (rex & 8u) != 0u ? 8u : operand_16 != 0u ? 2u : 4u;
    if ((modrm >> 6) == 3u) {
        ++*cursor;
        item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
    } else {
        if (!hl_x86_decode_address(request, block, item, rex, 0, address_32, start, cursor)) return 0;
        item->width = (rex & 8u) != 0u ? 8u : operand_16 != 0u ? 2u : 4u;
        item->source_high = 1u;
    }
    if (opcode == 0x69u) immediate_size = item->width == 2u ? 2u : 4u;
    if (opcode == 0x6bu) immediate_size = 1u;
    if (immediate_size != 0u) {
        if (immediate_size > request->guest_size - *cursor || immediate_size > 15u - (*cursor - start))
            return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
        item->has_immediate = 1u;
        item->operand_immediate = (uint64_t)load_signed(&request->guest_bytes[*cursor], immediate_size);
        *cursor += immediate_size;
    }
    item->operation = OP_IMUL;
    return 1;
}

static uint32_t flags_words(void) {
    return 19u + constant_words(1u) * 2u + constant_words(HL_X86_RFLAGS_NZCV_MASK);
}

static uint32_t lsl(unsigned destination, unsigned source, unsigned shift);

int hl_x86_decode_mul(const hl_x86_a64_request *request, decode *block, instruction *item,
                      uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                      size_t start, size_t *cursor) {
    uint8_t modrm;
    if (*cursor >= request->guest_size || *cursor - start >= 15u)
        return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
    modrm = request->guest_bytes[*cursor];
    if (((modrm >> 3) & 7u) != 4u)
        return fail(block, cursor, start, HL_X86_A64_UNSUPPORTED);
    item->width = opcode == 0xf6u ? 1u : (rex & 8u) != 0u ? 8u : operand_16 != 0u ? 2u : 4u;
    if ((modrm >> 6) == 3u) {
        ++*cursor;
        item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
        if (item->width == 1u && rex == 0u && (modrm & 7u) >= 4u) {
            item->source = (uint8_t)((modrm & 7u) - 4u);
            item->source_high = 8u;
        }
    } else {
        if (!hl_x86_decode_address(request, block, item, rex, 0, address_32, start, cursor)) return 0;
        item->width = opcode == 0xf6u ? 1u : (rex & 8u) != 0u ? 8u : operand_16 != 0u ? 2u : 4u;
        item->memory_operand = 1u;
    }
    item->operation = OP_MUL;
    return 1;
}

int hl_x86_decode_div(const hl_x86_a64_request *request, decode *block, instruction *item,
                      uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                      size_t start, size_t *cursor) {
    uint8_t modrm;
    uint8_t extension;
    if (*cursor >= request->guest_size || *cursor - start >= 15u)
        return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
    modrm = request->guest_bytes[*cursor];
    extension = (modrm >> 3) & 7u;
    if (extension != 6u && extension != 7u)
        return fail(block, cursor, start, HL_X86_A64_UNSUPPORTED);
    item->width = opcode == 0xf6u ? 1u : (rex & 8u) != 0u ? 8u : operand_16 != 0u ? 2u : 4u;
    item->conditional = extension == 7u;
    if ((modrm >> 6) == 3u) {
        ++*cursor;
        item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
        if (item->width == 1u && rex == 0u && (modrm & 7u) >= 4u) {
            item->source = (uint8_t)((modrm & 7u) - 4u);
            item->source_high = 8u;
        }
    } else {
        if (!hl_x86_decode_address(request, block, item, rex, 0, address_32, start, cursor)) return 0;
        item->width = opcode == 0xf6u ? 1u : (rex & 8u) != 0u ? 8u : operand_16 != 0u ? 2u : 4u;
        item->memory_operand = 1u;
    }
    item->operation = OP_DIV;
    return 1;
}

static uint32_t bitfield(unsigned destination, unsigned source, unsigned lsb, unsigned width) {
    unsigned immr = (64u - lsb) & 63u;
    return UINT32_C(0xb3400000) | immr << 16 | (width - 1u) << 10 | source << 5 | destination;
}

static void div_fallback(uint32_t *words, uint32_t *cursor, const instruction *item) {
    emit_constant(words, cursor, 16u, item->pc);
    words[(*cursor)++] = store_word(16u, offsetof(hl_native_x86_64_cpu, program));
    emit_constant(words, cursor, 16u, HL_NATIVE_EXIT_FALLBACK);
    words[(*cursor)++] = store_word(16u, offsetof(hl_native_x86_64_cpu, reason));
    if (item->live_chain != 0u) {
        hl_x86_finish_chain(words, cursor);
        hl_x86_spill_all(words, cursor);
    }
    words[(*cursor)++] = UINT32_C(0xd65f03c0);
}

static void patch_cbnz(uint32_t *site, uint32_t *target, unsigned reg) {
    *site = UINT32_C(0xb5000000) | ((uint32_t)(target - site) & UINT32_C(0x7ffff)) << 5 | reg;
}

static void patch_cbz(uint32_t *site, uint32_t *target, unsigned reg) {
    *site = UINT32_C(0xb4000000) | ((uint32_t)(target - site) & UINT32_C(0x7ffff)) << 5 | reg;
}

static void patch_condition(uint32_t *site, uint32_t *target, unsigned condition) {
    *site = UINT32_C(0x54000000) | ((uint32_t)(target - site) & UINT32_C(0x7ffff)) << 5 | condition;
}

uint32_t hl_x86_div_words(const instruction *item) {
    return (item->memory_operand != 0u ? hl_x86_load_words(item) : 0u) + 160u;
}

void hl_x86_emit_div(uint32_t *words, uint32_t *cursor, const instruction *item) {
    unsigned divisor = item->source;
    int signed_divide = item->conditional != 0u;
    if (item->memory_operand != 0u) {
        instruction load = *item;
        load.operation = OP_LOAD;
        load.destination = 19u;
        load.memory_operand = 0u;
        load.source_high = 0u;
        hl_x86_emit_load(words, cursor, &load);
        divisor = 19u;
    } else if (item->source_high == 8u) {
        words[(*cursor)++] = UINT32_C(0x53083c00) | divisor << 5 | 19u;
        divisor = 19u;
    }
    if (item->width < 8u) {
        uint32_t extension = signed_divide ?
            (item->width == 1u ? UINT32_C(0x93401c00) :
             item->width == 2u ? UINT32_C(0x93403c00) : UINT32_C(0x93407c00)) :
            (item->width == 1u ? UINT32_C(0x12001c00) :
             item->width == 2u ? UINT32_C(0x12003c00) : UINT32_C(0xd3407c00));
        words[(*cursor)++] = extension | divisor << 5 | 19u;
        divisor = 19u;
    } else if (divisor != 19u) {
        words[(*cursor)++] = UINT32_C(0xaa0003f3) | divisor << 16;
        divisor = 19u;
    }

    uint32_t zero = (*cursor)++;
    div_fallback(words, cursor, item);
    patch_cbnz(&words[zero], &words[*cursor], divisor);
    if (item->width == 1u) {
        words[(*cursor)++] = (signed_divide ? UINT32_C(0x93403c14) : UINT32_C(0xd3403c14));
    } else if (item->width == 2u) {
        words[(*cursor)++] = UINT32_C(0xd3403c14); /* uxth x20,x0 */
        words[(*cursor)++] = UINT32_C(0xd3403c55); /* uxth x21,x2 */
        words[(*cursor)++] = UINT32_C(0xaa154294); /* orr x20,x20,x21,lsl #16 */
        if (signed_divide) words[(*cursor)++] = UINT32_C(0x93407e94); /* sxtw x20,w20 */
    } else if (item->width == 4u) {
        words[(*cursor)++] = UINT32_C(0x2a0003f4); /* mov w20,w0 */
        words[(*cursor)++] = UINT32_C(0x2a0203f5); /* mov w21,w2 */
        words[(*cursor)++] = UINT32_C(0xaa158294); /* orr x20,x20,x21,lsl #32 */
    } else if (!signed_divide) {
        words[(*cursor)++] = UINT32_C(0xaa0003f4); /* mov x20,x0 */
        uint32_t high_zero = (*cursor)++;
        div_fallback(words, cursor, item);
        patch_cbz(&words[high_zero], &words[*cursor], 2u);
    } else {
        words[(*cursor)++] = UINT32_C(0xaa0003f4); /* mov x20,x0 */
        words[(*cursor)++] = UINT32_C(0x937ffc11); /* asr x17,x0,#63 */
        words[(*cursor)++] = UINT32_C(0xeb11005f); /* cmp x2,x17 */
        uint32_t high_ok = (*cursor)++;
        div_fallback(words, cursor, item);
        patch_condition(&words[high_ok], &words[*cursor], 0u);
        words[(*cursor)++] = UINT32_C(0x91000671); /* add x17,x19,#1 */
        uint32_t not_minus_one = (*cursor)++;
        div_fallback(words, cursor, item);
        patch_cbnz(&words[not_minus_one], &words[*cursor], 17u);
    }
    words[(*cursor)++] = (signed_divide ? UINT32_C(0x9ac00c00) : UINT32_C(0x9ac00800)) |
                         divisor << 16 | 20u << 5 | 18u;
    words[(*cursor)++] = UINT32_C(0x9b008000) | divisor << 16 | 20u << 10 | 18u << 5 | 21u;
    if (item->width < 8u) {
        if (signed_divide) {
            uint32_t extension = item->width == 1u ? UINT32_C(0x93401c11) :
                                 item->width == 2u ? UINT32_C(0x93403c11) : UINT32_C(0x93407c11);
            words[(*cursor)++] = extension | 18u << 5;
            words[(*cursor)++] = UINT32_C(0xeb12023f); /* cmp x17,x18 */
            uint32_t fits = (*cursor)++;
            div_fallback(words, cursor, item);
            patch_condition(&words[fits], &words[*cursor], 0u);
        } else {
            unsigned bits = item->width * 8u;
            words[(*cursor)++] = UINT32_C(0xd340fc11) | bits << 16 | 18u << 5;
            uint32_t fits = (*cursor)++;
            div_fallback(words, cursor, item);
            patch_cbz(&words[fits], &words[*cursor], 17u);
        }
    }
    if (item->width == 1u) {
        words[(*cursor)++] = bitfield(0u, 18u, 0u, 8u);
        words[(*cursor)++] = bitfield(0u, 21u, 8u, 8u);
    } else if (item->width == 2u) {
        words[(*cursor)++] = bitfield(0u, 18u, 0u, 16u);
        words[(*cursor)++] = bitfield(2u, 21u, 0u, 16u);
    } else if (item->width == 4u) {
        words[(*cursor)++] = UINT32_C(0x2a0003e0) | 18u << 16;
        words[(*cursor)++] = UINT32_C(0x2a0003e2) | 21u << 16;
    } else {
        words[(*cursor)++] = UINT32_C(0xaa0003e0) | 18u << 16;
        words[(*cursor)++] = UINT32_C(0xaa0003e2) | 21u << 16;
    }
}

uint32_t hl_x86_mul_words(const instruction *item) {
    uint32_t words = item->memory_operand != 0u ? hl_x86_load_words(item) : 0u;
    if (item->width == 1u || item->width == 2u) {
        words += 1u; /* truncate the accumulator */
        if (item->memory_operand == 0u) ++words; /* truncate/extract the register source */
        words += item->width == 1u ? 3u : 5u; /* product, high half, and architectural writes */
    } else {
        words += 4u; /* low/high products and architectural writes */
    }
    return words + 2u + flags_words(); /* high-half test and CF/OF materialization */
}

void hl_x86_emit_mul(uint32_t *words, uint32_t *cursor, const instruction *item) {
    unsigned source = item->source;
    if (item->memory_operand != 0u) {
        instruction load = *item;
        load.operation = OP_LOAD;
        load.destination = 18u;
        load.memory_operand = 0u;
        load.source_high = 0u;
        hl_x86_emit_load(words, cursor, &load);
        source = 18u;
    }
    if (item->width == 1u || item->width == 2u) {
        uint32_t mask = item->width == 1u ? UINT32_C(0x53001c00) : UINT32_C(0x53003c00);
        words[(*cursor)++] = mask | 0u << 5 | 20u;
        if (item->memory_operand == 0u) {
            uint32_t extract = item->source_high == 8u ? UINT32_C(0x53083c00) : mask;
            words[(*cursor)++] = extract | source << 5 | 19u;
            source = 19u;
        }
        words[(*cursor)++] = UINT32_C(0x1b007c00) | source << 16 | 20u << 5 | 18u;
        words[(*cursor)++] = (item->width == 1u ? UINT32_C(0x53087c00) : UINT32_C(0x53107c00)) |
                             18u << 5 | 21u;
        if (item->width == 1u) {
            words[(*cursor)++] = UINT32_C(0xb3403c00) | 18u << 5 | 0u;
        } else {
            words[(*cursor)++] = UINT32_C(0xb3403c00) | 18u << 5 | 0u;
            words[(*cursor)++] = UINT32_C(0x53107c00) | 18u << 5 | 18u;
            words[(*cursor)++] = UINT32_C(0xb3403c00) | 18u << 5 | 2u;
        }
    } else if (item->width == 4u) {
        words[(*cursor)++] = UINT32_C(0x9ba07c00) | source << 16 | 0u << 5 | 18u;
        words[(*cursor)++] = UINT32_C(0xd360fc00) | 18u << 5 | 21u;
        words[(*cursor)++] = UINT32_C(0x2a0003e0) | 18u << 16 | 0u;
        words[(*cursor)++] = UINT32_C(0x2a0003e0) | 21u << 16 | 2u;
    } else {
        words[(*cursor)++] = UINT32_C(0x9bc07c00) | source << 16 | 0u << 5 | 21u;
        words[(*cursor)++] = UINT32_C(0x9b007c00) | source << 16 | 0u << 5 | 18u;
        words[(*cursor)++] = UINT32_C(0xaa0003e0) | 18u << 16 | 0u;
        words[(*cursor)++] = UINT32_C(0xaa0003e0) | 21u << 16 | 2u;
    }
    words[(*cursor)++] = UINT32_C(0xeb1f02bf); /* cmp x21,#0 */
    words[(*cursor)++] = UINT32_C(0x9a9f07f5); /* cset x21,ne */
    emit_constant(words, cursor, 23u, 1u);
    words[(*cursor)++] = UINT32_C(0xca1702b6);
    words[(*cursor)++] = lsl(22u, 22u, 29u);
    words[(*cursor)++] = lsl(21u, 21u, 28u);
    words[(*cursor)++] = UINT32_C(0xaa1502d6);
    words[(*cursor)++] = UINT32_C(0xd51b4216);
    hl_x86_emit_nzcv(words, cursor);
}

uint32_t hl_x86_imul_words(const instruction *item) {
    uint32_t words = item->source_high != 0u ? hl_x86_load_words(item) : 0u;
    if (item->has_immediate != 0u) words += constant_words(item->operand_immediate);
    words += item->width == 8u ? 6u : 7u;
    return words + flags_words();
}

static uint32_t move(unsigned destination, unsigned source, int wide) {
    return (wide ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) | source << 16 | destination;
}

static uint32_t lsl(unsigned destination, unsigned source, unsigned shift) {
    return UINT32_C(0xd3400000) | (64u - shift) << 16 | (63u - shift) << 10 |
           source << 5 | destination;
}

void hl_x86_emit_imul(uint32_t *words, uint32_t *cursor, const instruction *item) {
    unsigned source = item->source;
    unsigned left;
    unsigned right;
    if (item->source_high != 0u) {
        instruction load = *item;
        load.operation = OP_LOAD;
        load.destination = 19u;
        load.source_high = 0u;
        hl_x86_emit_load(words, cursor, &load);
        source = 19u;
    }
    if (item->has_immediate != 0u) {
        emit_constant(words, cursor, 20u, item->operand_immediate);
        left = source;
        right = 20u;
    } else {
        left = item->destination;
        right = source;
    }
    if (item->width == 8u) {
        words[(*cursor)++] = UINT32_C(0x9b407c00) | right << 16 | left << 5 | 21u;
        words[(*cursor)++] = UINT32_C(0x9b007c00) | right << 16 | left << 5 | 18u;
        words[(*cursor)++] = UINT32_C(0x937ffc00) | 18u << 5 | 22u;
        words[(*cursor)++] = UINT32_C(0xeb1602bf);
    } else {
        uint32_t extend = item->width == 2u ? UINT32_C(0x93403c00) : UINT32_C(0x93407c00);
        words[(*cursor)++] = extend | left << 5 | 21u;
        words[(*cursor)++] = extend | right << 5 | 22u;
        words[(*cursor)++] = UINT32_C(0x9b007c00) | 22u << 16 | 21u << 5 | 18u;
        words[(*cursor)++] = extend | 18u << 5 | 23u;
        words[(*cursor)++] = UINT32_C(0xeb17025f);
    }
    words[(*cursor)++] = UINT32_C(0x9a9f07f5);
    if (item->width == 2u)
        words[(*cursor)++] = UINT32_C(0xb3403c00) | 18u << 5 | item->destination;
    else
        words[(*cursor)++] = move(item->destination, 18u, item->width == 8u);
    emit_constant(words, cursor, 23u, 1u);
    words[(*cursor)++] = UINT32_C(0xca1702b6);
    words[(*cursor)++] = lsl(22u, 22u, 29u);
    words[(*cursor)++] = lsl(21u, 21u, 28u);
    words[(*cursor)++] = UINT32_C(0xaa1502d6);
    words[(*cursor)++] = UINT32_C(0xd51b4216);
    hl_x86_emit_nzcv(words, cursor);
}
