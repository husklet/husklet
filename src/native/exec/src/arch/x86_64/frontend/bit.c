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

int hl_x86_decode_bit(const hl_x86_a64_request *request, decode *block, instruction *item,
                      uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                      size_t start, size_t *cursor) {
    uint8_t modrm, operation, width;
    int immediate = opcode == 0xbau;

    if (*cursor >= request->guest_size || *cursor - start >= 15u)
        return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
    modrm = request->guest_bytes[*cursor];
    operation = immediate ? (uint8_t)((modrm >> 3) & 7u) :
                            (uint8_t)(((opcode >> 3) & 3u) + 4u);
    if (operation < 4u) return fail(block, cursor, start, HL_X86_A64_UNSUPPORTED);
    width = (rex & 8u) != 0u ? 8u : operand_16 != 0u ? 2u : 4u;
    item->operation = OP_BIT;
    item->condition = (uint8_t)(operation - 4u);
    item->width = width;
    item->bit_operand_width = width;
    item->destination = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
    item->bit_index = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
    item->has_immediate = (uint8_t)immediate;
    item->bit_immediate = (uint8_t)immediate;
    if ((modrm >> 6) == 3u) {
        ++*cursor;
    } else {
        if (!hl_x86_decode_address(request, block, item, rex, 0u, address_32, start, cursor)) return 0;
        item->operation = OP_BIT;
        item->condition = (uint8_t)(operation - 4u);
        item->width = width;
        item->bit_operand_width = width;
        item->bit_index = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
        item->has_immediate = (uint8_t)immediate;
        item->bit_immediate = (uint8_t)immediate;
        item->memory_operand = 1u;
        item->memory_write = operation != 4u;
        item->bit_memory_offset = 1u;
    }
    if (immediate) {
        if (*cursor >= request->guest_size || *cursor - start >= 15u)
            return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
        item->operand_immediate = request->guest_bytes[(*cursor)++] & (uint8_t)(width * 8u - 1u);
    }
    return 1;
}

static void publish_carry(uint32_t *words, uint32_t *cursor) {
    words[(*cursor)++] = load_word(20u, offsetof(hl_native_x86_64_cpu, flags));
    words[(*cursor)++] = UINT32_C(0xb3400020) | 21u << 5 | 20u; /* bfi x20,x21,#0,#1 */
    words[(*cursor)++] = store_word(20u, offsetof(hl_native_x86_64_cpu, flags));
}

void hl_x86_emit_bit(uint32_t *words, uint32_t *cursor, const instruction *item) {
    unsigned wide = item->width == 8u;
    unsigned old = item->memory_operand != 0u ? 18u : item->destination;
    instruction memory;

    /* The memory form addresses the containing byte, so only the register form keeps the
       decoder's already width-masked bit index. */
    if (item->bit_immediate != 0u)
        emit_constant(words, cursor, 19u,
                      item->memory_operand != 0u ? (item->operand_immediate & 7u) :
                                                   item->operand_immediate);
    else {
        emit_constant(words, cursor, 19u, item->memory_operand != 0u ? 7u : item->width * 8u - 1u);
        words[(*cursor)++] = (wide ? UINT32_C(0x8a000000) : UINT32_C(0x0a000000)) |
                             19u << 16 | (uint32_t)item->bit_index << 5 | 19u;
    }
    if (item->memory_operand != 0u) {
        memory = *item;
        memory.operation = OP_LOAD;
        memory.destination = 18u;
        memory.load_width = 1u;
        hl_x86_emit_load(words, cursor, &memory);
    } else if (item->width == 2u) {
        words[(*cursor)++] = UINT32_C(0x53003c00) | old << 5 | 18u;
        old = 18u;
    }
    words[(*cursor)++] = (wide ? UINT32_C(0x9ac02400) : UINT32_C(0x1ac02400)) |
                         19u << 16 | old << 5 | 21u; /* lsrv */
    emit_constant(words, cursor, 22u, 1u);
    words[(*cursor)++] = UINT32_C(0x8a1602b5); /* and x21,x21,x22 */
    if (item->condition != 0u) {
        words[(*cursor)++] = (wide ? UINT32_C(0x9ac02000) : UINT32_C(0x1ac02000)) |
                             19u << 16 | 22u << 5 | 22u; /* lslv mask */
        if (item->condition == 1u)
            words[(*cursor)++] = (wide ? UINT32_C(0xaa000000) : UINT32_C(0x2a000000)) |
                                 22u << 16 | old << 5 | 20u;
        else if (item->condition == 2u)
            words[(*cursor)++] = (wide ? UINT32_C(0x8a200000) : UINT32_C(0x0a200000)) |
                                 22u << 16 | old << 5 | 20u;
        else
            words[(*cursor)++] = (wide ? UINT32_C(0xca000000) : UINT32_C(0x4a000000)) |
                                 22u << 16 | old << 5 | 20u;
        if (item->memory_operand != 0u) {
            memory = *item;
            memory.operation = OP_STORE;
            memory.source = 20u;
            memory.width = 1u;
            memory.has_immediate = 0u;
            hl_x86_emit_store(words, cursor, &memory);
        } else if (item->width == 2u) {
            words[(*cursor)++] = UINT32_C(0xb3403c00) | 20u << 5 | item->destination;
        } else {
            words[(*cursor)++] = (wide ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) |
                                 20u << 16 | item->destination;
        }
    }
    publish_carry(words, cursor);
}

uint32_t hl_x86_bit_words(const instruction *item) {
    uint32_t scratch[512];
    uint32_t cursor = 0;
    hl_x86_emit_bit(scratch, &cursor, item);
    return cursor;
}
