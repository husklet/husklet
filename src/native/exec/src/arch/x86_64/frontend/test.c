#include "private.h"

#include "../decode.h"

int hl_x86_test_opcode(uint8_t opcode) {
    return opcode == 0x84u || opcode == 0x85u || opcode == 0xa8u ||
           opcode == 0xa9u || opcode == 0xf6u || opcode == 0xf7u;
}

static int fail(decode *block, size_t *cursor, size_t start, hl_x86_a64_status status) {
    *cursor = start;
    block->status = status;
    block->exit = HL_X86_A64_INTERPRETER;
    return 0;
}

int hl_x86_decode_test(const hl_x86_a64_request *request, decode *block, instruction *item,
                       uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                       uint8_t lock, size_t start, size_t *cursor) {
    uint8_t modrm = 0;
    uint8_t group = opcode == 0xf6u || opcode == 0xf7u;
    uint8_t extension = 0;

    if (opcode == 0x84u || opcode == 0x85u || group) {
        if (*cursor >= request->guest_size || *cursor - start >= 15u)
            return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
        modrm = request->guest_bytes[*cursor];
        extension = (uint8_t)((modrm >> 3) & 7u);
        if (group && extension > 3u)
            return fail(block, cursor, start, HL_X86_A64_UNSUPPORTED);
        if (lock != 0u && (!group || extension < 2u || (modrm >> 6) == 3u))
            return fail(block, cursor, start, HL_X86_A64_UNSUPPORTED);
        if ((modrm >> 6) == 3u) {
            ++*cursor;
            item->destination = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
        } else {
            if (!hl_x86_decode_address(request, block, item, rex, 0u, address_32, start, cursor)) return 0;
            item->memory_operand = 1u;
        }
    } else {
        if (lock != 0u) return fail(block, cursor, start, HL_X86_A64_UNSUPPORTED);
        item->destination = 0;
    }
    item->operation = OP_ALU;
    item->width = opcode == 0x84u || opcode == 0xa8u || opcode == 0xf6u ? 1u :
                  (rex & 8u) != 0 ? 8u : operand_16 != 0 ? 2u : 4u;
    if (group && extension >= 2u) {
        /* LOCKed memory RMW requires a single atomic host operation.  Do not
         * silently lower it as load/store until that emitter is selected. */
        if (lock != 0u) return fail(block, cursor, start, HL_X86_A64_UNSUPPORTED);
        item->memory_write = item->memory_operand;
        item->alu_kind = extension == 2u ? 6u : 5u;
        item->has_immediate = extension == 2u;
        item->immediate = UINT64_MAX;
        item->operand_immediate = UINT64_MAX;
        item->preserve_flags = extension == 2u;
        item->unary_neg = extension == 3u;
        if (item->width == 1u && item->memory_operand == 0u && rex == 0u && (modrm & 7u) >= 4u) {
            item->destination = (uint8_t)((modrm & 7u) - 4u);
            item->destination_high = 1u;
        }
        return 1;
    }
    item->alu_kind = 4u;
    item->flags_only = 1u;
    if (opcode == 0xf6u && item->memory_operand == 0u && rex == 0u && (modrm & 7u) >= 4u) {
        item->destination = (uint8_t)((modrm & 7u) - 4u);
        item->destination_high = 1u;
    }
    if (opcode == 0x84u || opcode == 0x85u) {
        item->source = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
        if (opcode == 0x84u && rex == 0) {
            if ((modrm & 7u) >= 4u) {
                item->destination = (uint8_t)((modrm & 7u) - 4u);
                item->destination_high = 1u;
            }
            if (((modrm >> 3) & 7u) >= 4u) {
                item->source = (uint8_t)(((modrm >> 3) & 7u) - 4u);
                item->source_high = 8u;
            }
        }
        return 1;
    }
    size_t immediate_size = opcode == 0xa8u || opcode == 0xf6u ? 1u : operand_16 != 0 ? 2u : 4u;
    if (immediate_size > request->guest_size - *cursor || immediate_size > 15u - (*cursor - start))
        return fail(block, cursor, start, HL_X86_A64_TRUNCATED);
    item->has_immediate = 1u;
    item->operand_immediate = item->width == 8u ?
                                  (uint64_t)load_signed(&request->guest_bytes[*cursor], 4u) :
                                  load_unsigned(&request->guest_bytes[*cursor], immediate_size);
    if (item->memory_operand == 0u) item->immediate = item->operand_immediate;
    *cursor += immediate_size;
    return 1;
}
