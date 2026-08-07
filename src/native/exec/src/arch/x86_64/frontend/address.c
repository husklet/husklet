#include "private.h"

#include <stddef.h>

#include "../decode.h"
#include "../word.h"
#include "cpu.h"

static int fail(decode *block, size_t *cursor, size_t start, hl_x86_a64_status status) {
    *cursor = start;
    block->status = status;
    block->exit = HL_X86_A64_INTERPRETER;
    return 0;
}

int hl_x86_decode_address(const hl_x86_a64_request *request, decode *block, instruction *item,
                          uint8_t rex, uint8_t operand_16, uint8_t address_32, size_t start, size_t *cursor) {
    uint8_t modrm;
    uint8_t mod;
    uint8_t rm;
    size_t displacement_size = 0;

    if (operand_16 != 0)
        return fail(block, cursor, start, HL_X86_A64_UNSUPPORTED);
    if (*cursor >= request->guest_size || *cursor - start >= 15u)
        return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
    modrm = request->guest_bytes[(*cursor)++];
    mod = modrm >> 6;
    rm = modrm & 7u;
    if (mod == 3u) return fail(block, cursor, start, HL_X86_A64_UNSUPPORTED);
    item->operation = OP_ADDRESS;
    item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
    item->width = (rex & 8u) != 0 ? 8u : 4u;
    item->address_base = UINT8_MAX;
    item->address_index = UINT8_MAX;
    item->address_32 = address_32;
    if (rm == 4u) {
        uint8_t sib;
        uint8_t base;
        uint8_t index;

        if (*cursor >= request->guest_size || *cursor - start >= 15u)
            return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
        sib = request->guest_bytes[(*cursor)++];
        base = sib & 7u;
        index = (sib >> 3) & 7u;
        item->address_scale = sib >> 6;
        if (index != 4u || (rex & 2u) != 0)
            item->address_index = (uint8_t)(index | ((rex & 2u) << 2));
        if (mod == 0u && base == 5u)
            displacement_size = 4u;
        else
            item->address_base = (uint8_t)(base | ((rex & 1u) << 3));
    } else if (mod == 0u && rm == 5u) {
        item->rip_relative = address_32 == 0u;
        displacement_size = 4u;
    } else {
        item->address_base = (uint8_t)(rm | ((rex & 1u) << 3));
    }
    if (mod == 1u) displacement_size = 1u;
    if (mod == 2u) displacement_size = 4u;
    if (displacement_size > request->guest_size - *cursor || displacement_size > 15u - (*cursor - start))
        return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
    item->immediate = displacement_size == 0u ? 0u :
                      (uint64_t)load_signed(&request->guest_bytes[*cursor], displacement_size);
    *cursor += displacement_size;
    return 1;
}

static uint32_t move_register(unsigned destination, unsigned source, int wide) {
    return (wide ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) | source << 16 | destination;
}

/* ARM add/sub-immediate reaches any twelve-bit magnitude, optionally shifted by
 * twelve, so a displacement folds into one or two words that also absorb the
 * base move, instead of materialising a constant and adding it. */
static uint32_t fold_displacement(uint64_t immediate, uint32_t *low, uint32_t *high, int *negative) {
    int64_t value = (int64_t)immediate;
    uint64_t magnitude;

    *negative = value < 0;
    magnitude = *negative ? ~immediate + 1u : immediate;
    if (magnitude > UINT64_C(0xffffff)) return 0;
    *low = (uint32_t)(magnitude & UINT64_C(0xfff));
    *high = (uint32_t)(magnitude >> 12);
    return (*low != 0u ? 1u : 0u) + (*high != 0u ? 1u : 0u);
}

static uint32_t add_immediate(unsigned destination, unsigned source, uint32_t value,
                              int shift, int negative, int wide) {
    return (negative ? (wide ? UINT32_C(0xd1000000) : UINT32_C(0x51000000))
                     : (wide ? UINT32_C(0x91000000) : UINT32_C(0x11000000))) |
           (shift != 0 ? UINT32_C(0x400000) : 0u) | value << 10 | source << 5 | destination;
}

/* A displacement with no base register folds into the materialised constant. */
static uint64_t address_constant(const instruction *item) {
    uint64_t value = (item->rip_relative ? item->pc + item->length : 0u) + item->immediate;

    return item->address_32 != 0u ? (value & UINT32_C(0xffffffff)) : value;
}

uint32_t hl_x86_address_words(const instruction *item) {
    uint32_t low;
    uint32_t high;
    int negative;
    uint32_t folded = item->address_base != UINT8_MAX && item->immediate != 0u
                          ? fold_displacement(item->immediate, &low, &high, &negative)
                          : 0u;
    uint32_t words;

    if (item->address_base == UINT8_MAX)
        words = constant_words(address_constant(item));
    else if (folded != 0u)
        words = folded;
    else
        words = 1u + (item->immediate != 0u ? constant_words(item->immediate) + 1u : 0u);

    if (item->address_index != UINT8_MAX) ++words;
    if (item->bit_memory_offset != 0u)
        words += item->bit_immediate != 0u ? constant_words(item->operand_immediate >> 3) + 1u :
                 3u; /* extend/move, asr, add - three words at every operand width */
    return words + 1u + (item->segment != 0u ? 2u : 0u);
}

void hl_x86_emit_address(uint32_t *words, uint32_t *cursor, const instruction *item) {
    int wide = item->address_32 == 0u;
    uint32_t low;
    uint32_t high;
    int negative;
    uint32_t folded = item->address_base != UINT8_MAX && item->immediate != 0u
                          ? fold_displacement(item->immediate, &low, &high, &negative)
                          : 0u;

    if (item->address_base == UINT8_MAX) {
        emit_constant(words, cursor, 16, address_constant(item));
    } else if (folded != 0u) {
        unsigned source = item->address_base;

        if (high != 0u) {
            words[(*cursor)++] = add_immediate(16, source, high, 1, negative, wide);
            source = 16u;
        }
        if (low != 0u) words[(*cursor)++] = add_immediate(16, source, low, 0, negative, wide);
    } else {
        words[(*cursor)++] = move_register(16, item->address_base, wide);
        if (item->immediate != 0u) {
            emit_constant(words, cursor, 17, item->immediate);
            words[(*cursor)++] = (wide ? UINT32_C(0x8b000000) : UINT32_C(0x0b000000)) |
                                 17u << 16 | 16u << 5 | 16u;
        }
    }
    if (item->address_index != UINT8_MAX)
        words[(*cursor)++] = (item->address_32 != 0u ? UINT32_C(0x0b000000) : UINT32_C(0x8b000000)) |
                             (uint32_t)item->address_index << 16 |
                             (uint32_t)item->address_scale << 10 | 16u << 5 | 16u;
    if (item->segment != 0u) {
        words[(*cursor)++] = load_word(17, item->segment == 1u ?
                                      offsetof(hl_native_x86_64_cpu, fs) :
                                      offsetof(hl_native_x86_64_cpu, gs));
        words[(*cursor)++] = UINT32_C(0x8b110210); /* add x16,x16,x17 */
    }
    if (item->bit_memory_offset != 0u) {
        if (item->bit_immediate != 0u) {
            emit_constant(words, cursor, 17u, item->operand_immediate >> 3);
        } else {
            if (item->bit_operand_width == 2u)
                words[(*cursor)++] = UINT32_C(0x93403c00) | (uint32_t)item->bit_index << 5 | 17u; /* sxth */
            else if (item->bit_operand_width == 4u)
                words[(*cursor)++] = UINT32_C(0x93407c00) | (uint32_t)item->bit_index << 5 | 17u; /* sxtw */
            else
                words[(*cursor)++] = UINT32_C(0xaa0003e0) | (uint32_t)item->bit_index << 16 | 17u;
            words[(*cursor)++] = UINT32_C(0x9343fc00) | 17u << 5 | 17u; /* asr x17,x17,#3 */
        }
        words[(*cursor)++] = UINT32_C(0x8b110210); /* add x16,x16,x17 */
    }
    words[(*cursor)++] = move_register(item->destination, 16, item->width == 8u);
}

int hl_x86_decode_store(const hl_x86_a64_request *request, decode *block, instruction *item,
                        uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                        size_t start, size_t *cursor) {
    uint8_t modrm;
    uint8_t raw;
    if (*cursor >= request->guest_size) return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
    modrm = request->guest_bytes[*cursor];
    if ((modrm >> 6) == 3u) return fail(block, cursor, start, HL_X86_A64_UNSUPPORTED);
    if (!hl_x86_decode_address(request, block, item, rex, 0, address_32, start, cursor)) return 0;
    raw = (modrm >> 3) & 7u;
    item->operation = OP_STORE;
    item->source = (uint8_t)(raw | ((rex & 4u) << 1));
    item->width = opcode == 0x88u ? 1u : (rex & 8u) != 0 ? 8u : operand_16 != 0 ? 2u : 4u;
    if (opcode == 0x88u && rex == 0 && raw >= 4u) {
        item->source = (uint8_t)(raw - 4u);
        item->source_high = 8u;
    }
    return 1;
}

int hl_x86_decode_load(const hl_x86_a64_request *request, decode *block, instruction *item,
                       uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                       size_t start, size_t *cursor) {
    uint8_t modrm;
    uint8_t raw;
    if (*cursor >= request->guest_size) return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
    modrm = request->guest_bytes[*cursor];
    if ((modrm >> 6) == 3u) return fail(block, cursor, start, HL_X86_A64_UNSUPPORTED);
    if (!hl_x86_decode_address(request, block, item, rex, 0, address_32, start, cursor)) return 0;
    raw = (modrm >> 3) & 7u;
    item->operation = OP_LOAD;
    item->destination = (uint8_t)(raw | ((rex & 4u) << 1));
    item->width = opcode == 0x8au ? 1u : (rex & 8u) != 0 ? 8u : operand_16 != 0 ? 2u : 4u;
    if (opcode == 0x8au && rex == 0 && raw >= 4u) {
        item->destination = (uint8_t)(raw - 4u);
        item->destination_high = 1u;
    }
    return 1;
}
