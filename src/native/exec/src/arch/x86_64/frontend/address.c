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

uint32_t hl_x86_address_words(const instruction *item) {
    uint64_t base = item->rip_relative ? item->pc + item->length : 0u;
    uint32_t words = item->address_base == UINT8_MAX ? constant_words(base) : 1u;

    if (item->address_index != UINT8_MAX) ++words;
    if (item->immediate != 0u) words += constant_words(item->immediate) + 1u;
    return words + 1u + (item->segment != 0u ? 2u : 0u);
}

void hl_x86_emit_address(uint32_t *words, uint32_t *cursor, const instruction *item) {
    uint64_t base = item->rip_relative ? item->pc + item->length : 0u;

    if (item->address_base == UINT8_MAX)
        emit_constant(words, cursor, 16, base);
    else
        words[(*cursor)++] = move_register(16, item->address_base, item->address_32 == 0u);
    if (item->address_index != UINT8_MAX)
        words[(*cursor)++] = (item->address_32 != 0u ? UINT32_C(0x0b000000) : UINT32_C(0x8b000000)) |
                             (uint32_t)item->address_index << 16 |
                             (uint32_t)item->address_scale << 10 | 16u << 5 | 16u;
    if (item->immediate != 0u) {
        emit_constant(words, cursor, 17, item->immediate);
        words[(*cursor)++] = (item->address_32 != 0u ? UINT32_C(0x0b000000) : UINT32_C(0x8b000000)) |
                             17u << 16 | 16u << 5 | 16u;
    }
    if (item->segment != 0u) {
        words[(*cursor)++] = load_word(17, item->segment == 1u ?
                                      offsetof(hl_native_x86_64_cpu, fs) :
                                      offsetof(hl_native_x86_64_cpu, gs));
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
