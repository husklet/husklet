#include "private.h"

#include "../decode.h"

int hl_x86_immediate_opcode(uint8_t opcode) {
    return (opcode < 0x40u && ((opcode & 7u) == 4u || (opcode & 7u) == 5u)) ||
           opcode == 0x80u || opcode == 0x81u || opcode == 0x83u;
}

int hl_x86_decode_immediate(const hl_x86_a64_request *request, decode *block, instruction *item,
                            uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                            size_t start, size_t *cursor) {
    int accumulator = opcode < 0x40u;
    int byte = accumulator && (opcode & 7u) == 4u;
    size_t immediate_size = opcode == 0x80u || opcode == 0x83u || byte ? 1u :
                            operand_16 != 0 ? 2u : 4u;
    uint8_t kind;

    if (opcode == 0x80u || opcode == 0x81u || opcode == 0x83u) {
        uint8_t modrm;

        if (*cursor >= request->guest_size || *cursor - start >= 15u) {
            *cursor = start;
            block->status = HL_X86_A64_TRUNCATED;
            block->exit = HL_X86_A64_INTERPRETER;
            return 0;
        }
        modrm = request->guest_bytes[*cursor];
        kind = (uint8_t)((modrm >> 3) & 7u);
        if ((modrm >> 6) != 3u) {
            if (!hl_x86_decode_address(request, block, item, rex, 0, address_32,
                                       start, cursor)) return 0;
            item->memory_operand = 1u;
            item->memory_write = kind != 7u;
        } else {
            ++*cursor;
            item->destination = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            if (opcode == 0x80u && rex == 0 && (modrm & 7u) >= 4u) {
                item->destination = (uint8_t)((modrm & 7u) - 4u);
                item->destination_high = 1u;
            }
        }
    } else {
        kind = (uint8_t)((opcode >> 3) & 7u);
        item->destination = 0;
    }
    if (immediate_size > request->guest_size - *cursor || immediate_size > 15u - (*cursor - start)) {
        *cursor = start;
        block->status = HL_X86_A64_TRUNCATED;
        block->exit = HL_X86_A64_INTERPRETER;
        return 0;
    }
    item->operation = OP_ALU;
    item->alu_kind = kind;
    item->width = opcode == 0x80u || byte ? 1u :
                  (rex & 8u) != 0 ? 8u : operand_16 != 0 ? 2u : 4u;
    item->has_immediate = 1u;
    item->operand_immediate = opcode == 0x80u || byte ? load_unsigned(&request->guest_bytes[*cursor], 1u) :
                              immediate_size == 1u ? (uint64_t)load_signed(&request->guest_bytes[*cursor], 1u) :
                              item->width == 8u ? (uint64_t)load_signed(&request->guest_bytes[*cursor], 4u) :
                                                 load_unsigned(&request->guest_bytes[*cursor], immediate_size);
    if (item->memory_operand == 0u) item->immediate = item->operand_immediate;
    item->flags_only = kind == 7u;
    *cursor += immediate_size;
    return 1;
}
