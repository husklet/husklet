#include "frontend.h"

#include <stddef.h>
#include <string.h>

#include "cpu.h"
#include "decode.h"
#include "frontend/private.h"
#include "word.h"

/* The only register-only vector lowerings that can leave the block are the double and packed float
   arithmetic forms, which bail to the interpreter on an unordered result. */
static int vector_may_exit(const instruction *item) {
    return (item->vector_kind >= VECTOR_SCALAR_SQRT_DOUBLE &&
            item->vector_kind <= VECTOR_SCALAR_DIV_DOUBLE) ||
           item->vector_kind == VECTOR_FLOAT_ARITHMETIC;
}

static int may_fallback(const instruction *item) {
    return item->memory_operand != 0u || item->operation == OP_LOAD || item->operation == OP_STORE ||
           item->operation == OP_STRING ||
           item->operation == OP_MUL || item->operation == OP_DIV ||
           (item->operation == OP_IMUL && item->source_high != 0u) ||
           item->operation == OP_CALL ||
           (item->operation == OP_JUMP && item->source_high != 0u) || item->operation == OP_RETURN ||
           item->operation == OP_LEAVE || item->operation == OP_PUSH || item->operation == OP_POP ||
           (item->operation == OP_VECTOR && vector_may_exit(item));
}

/* 1 iff the item redefines every flag the NZCV transfer publishes while reading none. adc/sbb read CF,
   inc/dec preserve it, and a memory operand can fault before the redefinition. imul qualifies: it
   redefines CF and OF, and this lowering stores SF and ZF too rather than leaving them undefined. */
static int kills_nzcv(const instruction *item) {
    if (item->operation == OP_IMUL) return item->source_high == 0u && item->memory_operand == 0u;
    return item->operation == OP_ALU && item->memory_operand == 0u && item->preserve_carry == 0u &&
           item->preserve_flags == 0u && item->alu_kind != 2u && item->alu_kind != 3u;
}

/* 1 iff the item redefines both PF and AF while reading neither, which adc/sbb and inc/dec also do
   even though they read or preserve CF. imul is absent because this lowering leaves PF and AF alone. */
static int kills_pfaf(const instruction *item) {
    return item->operation == OP_ALU && item->memory_operand == 0u && item->preserve_flags == 0u;
}

/* A register ALU, shift or rotate op publishes nothing observable besides cpu->flags, so a successor
   redefining a half it wrote lets it skip that half's materialization. The two halves are tracked
   apart because their killer sets differ, and each producer consumes the ones it writes. */
static void mark_dead_flags(decode *block) {
    uint32_t index;

    for (index = 0; index < block->count; ++index) {
        instruction *item = &block->instructions[index];

        item->nzcv_dead = 0u;
        item->pfaf_dead = 0u;
        if (item->operation != OP_ALU && item->operation != OP_SHIFT && item->operation != OP_ROTATE)
            continue;
        if (item->memory_operand != 0u || item->preserve_flags != 0u) continue;
        if (index + 1u >= block->count) continue;
        item->nzcv_dead = (uint8_t)kills_nzcv(&block->instructions[index + 1u]);
        item->pfaf_dead = (uint8_t)kills_pfaf(&block->instructions[index + 1u]);
    }
}

static int vector_register_write(const instruction *item) {
    return item->operation == OP_VECTOR && item->memory_write == 0u &&
           item->vector_kind != VECTOR_TO_INTEGER && item->vector_kind != VECTOR_BYTE_MASK &&
           item->vector_kind != VECTOR_SCALAR_COMPARE_DOUBLE &&
           item->vector_kind != VECTOR_TRUNC_DOUBLE_SIGNED &&
           item->vector_kind != VECTOR_STRING_EQUAL_EACH;
}

void hl_x86_prepare_vector_immediate(instruction *item, uint8_t modrm, uint8_t rex,
                                     uint8_t immediate, uint8_t destructive_rm) {
    item->vector_immediate_form = destructive_rm != 0u ? VECTOR_IMMEDIATE_RM_DESTRUCTIVE :
                                                         VECTOR_IMMEDIATE_REG_DESTINATION;
    item->vector_subopcode = (uint8_t)((modrm >> 3) & 7u);
    item->vector_immediate = immediate;
    item->destination = destructive_rm != 0u ?
                            (uint8_t)((modrm & 7u) | ((rex & 1u) << 3)) :
                            (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
}

static void decode_block(const hl_x86_a64_request *request, decode *block) {
    size_t cursor = 0;
    memset(block, 0, sizeof *block);
    block->next_pc = request->guest_pc;
    block->exit = HL_X86_A64_FALLTHROUGH;
    block->status = HL_X86_A64_OK;
    while (cursor < request->guest_size && block->count < request->max_instructions) {
        size_t start = cursor;
        uint8_t operand_16 = 0;
        uint8_t address_32 = 0;
        uint8_t overlong = 0;
        uint8_t semantic_prefix = 0;
        uint8_t segment = 0;
        uint8_t rex = 0;
        uint8_t vex = 0;
        uint8_t vex_pp = 0;
        uint8_t vex_l = 0;
        uint8_t vex_vvvv = 0;
        uint8_t vex_map = 0;
        uint8_t vex_w = 0;
        uint8_t opcode;
        instruction *item = &block->instructions[block->count];
        for (;;) {
            uint8_t prefix;
            if (cursor - start >= 15u) {
                overlong = 1;
                break;
            }
            prefix = request->guest_bytes[cursor];

            if (prefix == 0x66u) operand_16 = 1;
            else if (prefix == 0x67u) address_32 = 1;
            else if (prefix == 0x64u) segment = 1;
            else if (prefix == 0x65u) segment = 2;
            else if (prefix == 0xf0u || prefix == 0xf2u || prefix == 0xf3u) semantic_prefix = prefix;
            else if (prefix != 0xf0u && prefix != 0xf2u && prefix != 0xf3u &&
                     prefix != 0x2eu && prefix != 0x36u && prefix != 0x3eu && prefix != 0x26u &&
                     prefix != 0x64u && prefix != 0x65u)
                break;
            if (++cursor == request->guest_size) break;
        }
        if (overlong != 0) {
            cursor = start;
            block->status = HL_X86_A64_UNSUPPORTED;
            block->exit = HL_X86_A64_INTERPRETER;
            break;
        }
        if (cursor >= request->guest_size) {
            cursor = start;
            block->status = HL_X86_A64_TRUNCATED;
            block->exit = HL_X86_A64_INTERPRETER;
            break;
        }
        if ((request->guest_bytes[cursor] & 0xf0u) == 0x40u) rex = request->guest_bytes[cursor++];
        if (cursor >= request->guest_size) {
            cursor = start;
            block->status = HL_X86_A64_TRUNCATED;
            block->exit = HL_X86_A64_INTERPRETER;
            break;
        }
        opcode = request->guest_bytes[cursor++];
        if ((opcode == 0xc4u || opcode == 0xc5u) && rex == 0u && operand_16 == 0u &&
            semantic_prefix == 0u && address_32 == 0u && segment == 0u) {
            uint8_t vex_two;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            vex_two = request->guest_bytes[cursor++];
            if (opcode == 0xc5u) {
                vex = 1u; vex_map = 1u;
                rex = (uint8_t)(((~vex_two >> 7) & 1u) << 2);
                vex_vvvv = (uint8_t)((~vex_two >> 3) & 15u);
                vex_l = (uint8_t)((vex_two >> 2) & 1u); vex_pp = (uint8_t)(vex_two & 3u);
            } else {
                uint8_t vex_three;
                vex_map = (uint8_t)(vex_two & 31u);
                rex = (uint8_t)((((~vex_two >> 7) & 1u) << 2) |
                                (((~vex_two >> 6) & 1u) << 1) | ((~vex_two >> 5) & 1u));
                if (cursor >= request->guest_size || cursor - start >= 15u) {
                    cursor = start; block->status = HL_X86_A64_TRUNCATED;
                    block->exit = HL_X86_A64_INTERPRETER; break;
                }
                vex_three = request->guest_bytes[cursor++]; vex = 1u;
                vex_w = (uint8_t)((vex_three >> 7) & 1u);
                vex_vvvv = (uint8_t)((~vex_three >> 3) & 15u);
                vex_l = (uint8_t)((vex_three >> 2) & 1u); vex_pp = (uint8_t)(vex_three & 3u);
            }
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            opcode = request->guest_bytes[cursor++];
        }
        if (cursor - start > 15u) {
            cursor = start;
            block->status = HL_X86_A64_UNSUPPORTED;
            block->exit = HL_X86_A64_INTERPRETER;
            break;
        }
        if (vex == 0u && semantic_prefix == 0xf0u && opcode != 0xf6u && opcode != 0xf7u &&
            opcode != 0x86u && opcode != 0x87u &&
            !(opcode == 0x0fu && cursor < request->guest_size &&
              (request->guest_bytes[cursor] == 0xb0u || request->guest_bytes[cursor] == 0xb1u ||
               request->guest_bytes[cursor] == 0xc0u || request->guest_bytes[cursor] == 0xc1u))) {
            cursor = start;
            block->status = HL_X86_A64_UNSUPPORTED;
            block->exit = HL_X86_A64_INTERPRETER;
            break;
        }
        if (vex == 0u && semantic_prefix != 0 && semantic_prefix != 0xf0u &&
            !((semantic_prefix == 0xf2u || semantic_prefix == 0xf3u) &&
              (opcode == 0xa4u || opcode == 0xa5u || opcode == 0xaau || opcode == 0xabu ||
               opcode == 0xacu || opcode == 0xadu)) &&
            !(semantic_prefix == 0xf3u && opcode == 0x0fu && cursor < request->guest_size &&
              (request->guest_bytes[cursor] == 0x6fu || request->guest_bytes[cursor] == 0x7eu ||
               request->guest_bytes[cursor] == 0x7fu || request->guest_bytes[cursor] == 0x70u ||
               request->guest_bytes[cursor] == 0x58u ||
               request->guest_bytes[cursor] == 0x59u || request->guest_bytes[cursor] == 0x5cu ||
               request->guest_bytes[cursor] == 0x5eu ||
               request->guest_bytes[cursor] == 0x5bu ||
               request->guest_bytes[cursor] == 0xbcu || request->guest_bytes[cursor] == 0xbdu ||
               (cursor + 1u < request->guest_size && request->guest_bytes[cursor] == 0x1eu &&
                (request->guest_bytes[cursor + 1u] == 0xfau || request->guest_bytes[cursor + 1u] == 0xfbu ||
                 ((request->guest_bytes[cursor + 1u] >> 6) == 3u &&
                  ((request->guest_bytes[cursor + 1u] >> 3) & 7u) == 1u))))) &&
            !(semantic_prefix == 0xf2u && opcode == 0x0fu && cursor < request->guest_size &&
              (request->guest_bytes[cursor] == 0x51u || request->guest_bytes[cursor] == 0x58u ||
               request->guest_bytes[cursor] == 0x59u || request->guest_bytes[cursor] == 0x5cu ||
               request->guest_bytes[cursor] == 0x5eu || request->guest_bytes[cursor] == 0x2cu ||
               request->guest_bytes[cursor] == 0x70u)) &&
            !((semantic_prefix == 0xf2u || semantic_prefix == 0xf3u) && opcode == 0x0fu &&
              cursor < request->guest_size && request->guest_bytes[cursor] == 0xc3u)) {
            cursor = start;
            block->status = HL_X86_A64_UNSUPPORTED;
            block->exit = HL_X86_A64_INTERPRETER;
            break;
        }
        item->pc = block->next_pc;
        item->segment = segment;
        item->live_chain = (request->flags & HL_X86_A64_LIVE_CHAIN) != 0u;
        if (vex != 0u && vex_map == 1u && opcode == 0x5bu && vex_pp <= 2u &&
            vex_vvvv == 0u) {
            uint8_t modrm;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            modrm = request->guest_bytes[cursor];
            item->operation = OP_VECTOR; item->vector_vex = 1u;
            item->width = vex_l != 0u ? 32u : 16u;
            item->vector_memory_width = item->width;
            item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->vector_source_one = item->source;
            item->vector_kind = vex_pp == 0u ? VECTOR_SIGNED_DWORD_TO_FLOAT :
                                vex_pp == 1u ? VECTOR_FLOAT_TO_SIGNED_DWORD :
                                              VECTOR_TRUNC_FLOAT_TO_SIGNED_DWORD;
            if ((modrm >> 6) == 3u) ++cursor;
            else {
                if (!hl_x86_decode_address(request, block, item, rex, 0u, 0u, start, &cursor)) break;
                item->operation = OP_VECTOR; item->memory_operand = 1u; item->source = 16u;
            }
        } else if (vex != 0u && vex_map == 1u &&
            ((opcode == 0x70u && vex_pp >= 1u) ||
             (opcode == 0xc6u && vex_pp <= 1u))) {
            uint8_t modrm;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            if (opcode == 0x70u && vex_vvvv != 0u) {
                cursor = start; block->status = HL_X86_A64_UNSUPPORTED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            modrm = request->guest_bytes[cursor];
            item->operation = OP_VECTOR; item->vector_vex = 1u;
            item->width = vex_l != 0u ? 32u : 16u;
            item->vector_memory_width = item->width;
            item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->vector_source_one = opcode == 0xc6u ? vex_vvvv : item->source;
            item->vector_kind = opcode == 0xc6u ?
                                    (vex_pp == 1u ? VECTOR_SHUFFLE_DOUBLE : VECTOR_SHUFFLE_FLOAT) :
                                vex_pp == 1u ? VECTOR_SHUFFLE_DWORD : VECTOR_SHUFFLE_WORD;
            item->condition = vex_pp == 3u;
            if ((modrm >> 6) == 3u) ++cursor;
            else {
                if (!hl_x86_decode_address(request, block, item, rex, 0u, 0u, start, &cursor)) break;
                item->operation = OP_VECTOR; item->memory_operand = 1u; item->source = 16u;
            }
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            item->vector_immediate = request->guest_bytes[cursor++];
        } else if (vex != 0u && vex_map == 2u && vex_pp == 1u &&
                   ((opcode >= 0x08u && opcode <= 0x0au) ||
                    (opcode >= 0x1cu && opcode <= 0x1eu && vex_vvvv == 0u))) {
            uint8_t modrm;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            modrm = request->guest_bytes[cursor];
            item->operation = OP_VECTOR; item->vector_vex = 1u;
            item->width = vex_l != 0u ? 32u : 16u;
            item->vector_memory_width = item->width;
            item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->vector_source_one = vex_vvvv;
            item->vector_kind = opcode < 0x1cu ? VECTOR_SIGN : VECTOR_ABSOLUTE;
            item->vector_lane = (uint8_t)(1u << (opcode - (opcode < 0x1cu ? 0x08u : 0x1cu)));
            if ((modrm >> 6) == 3u) ++cursor;
            else {
                if (!hl_x86_decode_address(request, block, item, rex, 0u, 0u, start, &cursor)) break;
                item->operation = OP_VECTOR; item->memory_operand = 1u; item->source = 16u;
            }
        } else if (vex != 0u && vex_map == 3u && vex_pp == 1u && vex_w == 0u &&
                   (opcode == 0x02u || (opcode >= 0x0cu && opcode <= 0x0eu) ||
                    (opcode >= 0x4au && opcode <= 0x4cu))) {
            uint8_t modrm;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            modrm = request->guest_bytes[cursor];
            item->operation = OP_VECTOR; item->vector_vex = 1u;
            item->width = vex_l != 0u ? 32u : 16u;
            item->vector_memory_width = item->width;
            item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->vector_source_one = vex_vvvv;
            item->vector_kind = opcode >= 0x4au ? VECTOR_BLEND_VARIABLE : VECTOR_BLEND_IMMEDIATE;
            item->vector_lane = opcode == 0x0eu ? 2u :
                                (opcode == 0x0du || opcode == 0x4bu) ? 8u :
                                opcode == 0x4cu ? 1u : 4u;
            item->condition = opcode == 0x0eu;
            if ((modrm >> 6) == 3u) ++cursor;
            else {
                if (!hl_x86_decode_address(request, block, item, rex, 0u, 0u, start, &cursor)) break;
                item->operation = OP_VECTOR; item->memory_operand = 1u; item->source = 16u;
                item->width = item->vector_memory_width;
            }
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            item->vector_immediate = request->guest_bytes[cursor++];
            if (item->vector_kind == VECTOR_BLEND_VARIABLE)
                item->vector_subopcode = (uint8_t)((item->vector_immediate >> 4) & 15u);
        } else if (vex != 0u && vex_pp == 1u &&
                   ((vex_map == 1u && (opcode == 0x63u || opcode == 0x67u || opcode == 0x6bu)) ||
                    (vex_map == 2u && opcode == 0x2bu))) {
            uint8_t modrm;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            modrm = request->guest_bytes[cursor];
            item->operation = OP_VECTOR; item->vector_vex = 1u;
            item->width = vex_l != 0u ? 32u : 16u;
            item->vector_memory_width = item->width;
            item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->vector_source_one = vex_vvvv;
            item->vector_kind = opcode == 0x67u || opcode == 0x2bu ?
                                    VECTOR_PACK_UNSIGNED : VECTOR_PACK_SIGNED;
            item->vector_lane = opcode == 0x6bu || opcode == 0x2bu ? 4u : 2u;
            if ((modrm >> 6) == 3u) ++cursor;
            else {
                if (!hl_x86_decode_address(request, block, item, rex, 0u, 0u, start, &cursor)) break;
                item->operation = OP_VECTOR; item->memory_operand = 1u; item->source = 16u;
            }
        } else if (vex != 0u && vex_map == 1u && vex_pp == 1u &&
                   (opcode == 0xd4u || opcode == 0xd5u || opcode == 0xe0u ||
                    opcode == 0xe3u || opcode == 0xe4u || opcode == 0xe5u ||
                    opcode == 0xf4u || opcode == 0xf6u ||
                    (opcode >= 0xf8u && opcode <= 0xfeu))) {
            uint8_t modrm;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            modrm = request->guest_bytes[cursor];
            item->operation = OP_VECTOR; item->vector_vex = 1u;
            item->width = vex_l != 0u ? 32u : 16u;
            item->vector_memory_width = item->width;
            item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->vector_source_one = vex_vvvv;
            if (opcode == 0xd4u || opcode >= 0xfcu) {
                item->vector_kind = VECTOR_ADD;
                item->vector_lane = opcode == 0xfcu ? 1u : opcode == 0xfdu ? 2u :
                                    opcode == 0xfeu ? 4u : 8u;
            } else if (opcode >= 0xf8u) {
                item->vector_kind = VECTOR_SUBTRACT;
                item->vector_lane = (uint8_t)(1u << (opcode - 0xf8u));
            } else if (opcode == 0xe0u || opcode == 0xe3u) {
                item->vector_kind = VECTOR_AVERAGE_UNSIGNED;
                item->vector_lane = opcode == 0xe0u ? 1u : 2u;
            } else if (opcode == 0xf6u) {
                item->vector_kind = VECTOR_SUM_ABSOLUTE_DIFFERENCES_BYTE;
            } else if (opcode == 0xd5u) item->vector_kind = VECTOR_MULTIPLY_LOW_WORD;
            else if (opcode == 0xe4u || opcode == 0xe5u) {
                item->vector_kind = VECTOR_MULTIPLY_HIGH_WORD;
                item->condition = opcode == 0xe5u;
            } else item->vector_kind = VECTOR_MULTIPLY_EVEN_DWORD;
            if ((modrm >> 6) == 3u) ++cursor;
            else {
                if (!hl_x86_decode_address(request, block, item, rex, 0u, 0u, start, &cursor)) break;
                item->operation = OP_VECTOR; item->memory_operand = 1u; item->source = 16u;
            }
        } else if (vex != 0u && vex_map == 1u && vex_pp == 1u &&
                   ((opcode >= 0x60u && opcode <= 0x62u) ||
                    (opcode >= 0x68u && opcode <= 0x6au) ||
                    opcode == 0x6cu || opcode == 0x6du)) {
            uint8_t modrm;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            modrm = request->guest_bytes[cursor];
            item->operation = OP_VECTOR; item->vector_vex = 1u;
            item->width = vex_l != 0u ? 32u : 16u;
            item->vector_memory_width = item->width;
            item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->vector_source_one = vex_vvvv;
            item->vector_kind = (opcode >= 0x68u && opcode <= 0x6au) || opcode == 0x6du
                                    ? VECTOR_UNPACK_HIGH : VECTOR_UNPACK_LOW;
            item->vector_lane = opcode == 0x60u || opcode == 0x68u ? 1u :
                                opcode == 0x61u || opcode == 0x69u ? 2u :
                                opcode == 0x62u || opcode == 0x6au ? 4u : 8u;
            if ((modrm >> 6) == 3u) ++cursor;
            else {
                if (!hl_x86_decode_address(request, block, item, rex, 0u, 0u, start, &cursor)) break;
                item->operation = OP_VECTOR; item->memory_operand = 1u; item->source = 16u;
            }
        } else if (vex != 0u && vex_map == 2u && vex_pp == 1u &&
                   (opcode == 0x01u || opcode == 0x02u || opcode == 0x03u ||
                    opcode == 0x05u || opcode == 0x06u || opcode == 0x07u)) {
            uint8_t modrm;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            modrm = request->guest_bytes[cursor];
            item->operation = OP_VECTOR; item->vector_vex = 1u;
            item->width = vex_l != 0u ? 32u : 16u; item->vector_memory_width = item->width;
            item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->vector_source_one = vex_vvvv;
            item->vector_lane = opcode == 0x02u || opcode == 0x06u ? 4u : 2u;
            item->condition = opcode >= 0x05u;
            item->vector_kind = opcode == 0x03u || opcode == 0x07u ? VECTOR_HORIZONTAL_SATURATING :
                                opcode >= 0x05u ? VECTOR_HORIZONTAL_SUBTRACT : VECTOR_HORIZONTAL_ADD;
            if ((modrm >> 6) == 3u) ++cursor;
            else {
                if (!hl_x86_decode_address(request, block, item, rex, 0u, 0u, start, &cursor)) break;
                item->operation = OP_VECTOR; item->memory_operand = 1u; item->source = 16u;
            }
        } else if (vex != 0u && vex_pp == 1u &&
                   ((vex_map == 1u &&
                     (opcode == 0xdau || opcode == 0xdeu || opcode == 0xeau || opcode == 0xeeu)) ||
                    (vex_map == 2u && opcode >= 0x38u && opcode <= 0x3fu))) {
            uint8_t modrm;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            modrm = request->guest_bytes[cursor];
            item->operation = OP_VECTOR; item->vector_vex = 1u;
            item->width = vex_l != 0u ? 32u : 16u; item->vector_memory_width = item->width;
            item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->vector_source_one = vex_vvvv;
            item->vector_lane = vex_map == 1u ? (opcode >= 0xeau ? 2u : 1u) :
                                ((opcode & 3u) == 0u ? 1u : (opcode & 3u) == 2u ? 2u : 4u);
            {
                uint8_t maximum = vex_map == 1u ? (opcode == 0xdeu || opcode == 0xeeu) : opcode >= 0x3cu;
                uint8_t unsigned_kind = vex_map == 1u ? opcode < 0xe0u :
                    (opcode == 0x3au || opcode == 0x3bu || opcode == 0x3eu || opcode == 0x3fu);
                item->vector_kind = maximum != 0u ?
                    (unsigned_kind != 0u ? VECTOR_MAXIMUM_UNSIGNED : VECTOR_MAXIMUM_SIGNED) :
                    (unsigned_kind != 0u ? VECTOR_MINIMUM_UNSIGNED : VECTOR_MINIMUM_SIGNED);
            }
            if ((modrm >> 6) == 3u) ++cursor;
            else {
                if (!hl_x86_decode_address(request, block, item, rex, 0u, 0u, start, &cursor)) break;
                item->operation = OP_VECTOR; item->memory_operand = 1u; item->source = 16u;
            }
        } else if (vex != 0u && vex_pp == 1u &&
                   ((vex_map == 1u &&
                     ((opcode >= 0x64u && opcode <= 0x66u) ||
                      (opcode >= 0x74u && opcode <= 0x76u))) ||
                    (vex_map == 2u && (opcode == 0x28u || opcode == 0x29u ||
                                      opcode == 0x37u || opcode == 0x40u)))) {
            uint8_t modrm;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            modrm = request->guest_bytes[cursor];
            item->operation = OP_VECTOR; item->vector_vex = 1u;
            item->width = vex_l != 0u ? 32u : 16u;
            item->vector_memory_width = item->width;
            item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->vector_source_one = vex_vvvv;
            item->vector_kind = opcode == 0x28u ? VECTOR_MULTIPLY_EVEN_SIGNED_DWORD :
                                opcode == 0x40u ? VECTOR_MULTIPLY_LOW_DWORD :
                                opcode == 0x64u || opcode == 0x65u || opcode == 0x66u ||
                                opcode == 0x37u ? VECTOR_COMPARE_GREATER_SIGNED :
                                                  VECTOR_COMPARE_EQUAL;
            item->vector_lane = opcode == 0x28u || opcode == 0x40u ? 4u :
                                vex_map == 2u ? 8u :
                                opcode == 0x64u || opcode == 0x74u ? 1u :
                                opcode == 0x65u || opcode == 0x75u ? 2u : 4u;
            if ((modrm >> 6) == 3u) ++cursor;
            else {
                if (!hl_x86_decode_address(request, block, item, rex, 0u, 0u, start, &cursor)) break;
                item->operation = OP_VECTOR; item->memory_operand = 1u; item->source = 16u;
            }
        } else if (vex != 0u) {
            cursor = start; block->status = HL_X86_A64_UNSUPPORTED;
            block->exit = HL_X86_A64_INTERPRETER; break;
        } else if ((semantic_prefix == 0u || semantic_prefix == 0xf2u || semantic_prefix == 0xf3u) &&
            (opcode == 0xa4u || opcode == 0xa5u || opcode == 0xaau || opcode == 0xabu ||
             opcode == 0xacu || opcode == 0xadu)) {
            uint64_t next;
            if (!add_pc(block->next_pc, cursor - start, &next)) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            item->operation = OP_STRING;
            item->condition = opcode <= 0xa5u ? 0u : opcode <= 0xabu ? 1u : 2u;
            item->conditional = semantic_prefix == 0xf2u || semantic_prefix == 0xf3u;
            item->width = (opcode & 1u) == 0u ? 1u :
                          (rex & 8u) != 0u ? 8u : operand_16 != 0u ? 2u : 4u;
            item->address_32 = address_32; item->memory_operand = 1u;
            item->length = (uint8_t)(cursor - start); block->next_pc = next;
            block->exit = HL_X86_A64_DYNAMIC_BRANCH; ++block->count; break;
        } else if (semantic_prefix == 0xf3u && opcode == 0x0fu && cursor + 1u < request->guest_size &&
            request->guest_bytes[cursor] == 0x1eu &&
            (request->guest_bytes[cursor + 1u] >> 6) == 3u &&
            ((request->guest_bytes[cursor + 1u] >> 3) & 7u) == 1u) {
            cursor += 2u;
            /* RDSSPD/RDSSPQ preserve the destination when the guest shadow
             * stack facility is disabled. Retained C exposes no guest CET
             * shadow stack, so this is an architectural conditional no-op. */
            item->operation = OP_NOP;
        } else if (semantic_prefix == 0xf3u && opcode == 0x0fu && cursor + 1u < request->guest_size &&
            request->guest_bytes[cursor] == 0x1eu &&
            (request->guest_bytes[cursor + 1u] == 0xfau || request->guest_bytes[cursor + 1u] == 0xfbu)) {
            cursor += 2u;
            item->operation = OP_NOP;
        } else if (opcode >= 0x50u && opcode <= 0x5fu) {
            item->width = operand_16 != 0u ? 2u : 8u;
            if (opcode < 0x58u) {
                item->operation = OP_PUSH;
                item->source = (uint8_t)((opcode & 7u) | ((rex & 1u) << 3));
            } else {
                item->operation = OP_POP;
                item->destination = (uint8_t)((opcode & 7u) | ((rex & 1u) << 3));
            }
        } else if (opcode == 0x68u || opcode == 0x6au) {
            size_t immediate_size = opcode == 0x6au ? 1u : operand_16 != 0u ? 2u : 4u;
            if (immediate_size > request->guest_size - cursor || immediate_size > 15u - (cursor - start)) {
                cursor = start;
                block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            item->operation = OP_PUSH;
            item->width = operand_16 != 0u ? 2u : 8u;
            item->has_immediate = 1u;
            item->immediate = (uint64_t)load_signed(&request->guest_bytes[cursor], immediate_size);
            cursor += immediate_size;
        } else if (operand_16 != 0u && semantic_prefix == 0u && opcode == 0x0fu &&
                   cursor + 2u < request->guest_size && request->guest_bytes[cursor] == 0x3au &&
                   request->guest_bytes[cursor + 1u] == 0x63u) {
            uint8_t modrm;
            cursor += 2u;
            modrm = request->guest_bytes[cursor];
            item->operation = OP_VECTOR;
            item->width = 16u;
            item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->vector_kind = VECTOR_STRING_EQUAL_EACH;
            if ((modrm >> 6) == 3u) {
                ++cursor;
            } else {
                if (!hl_x86_decode_address(request, block, item, rex, 0, address_32,
                                           start, &cursor)) break;
                item->operation = OP_VECTOR;
                item->width = 16u;
                item->memory_operand = 1u;
                item->source = 16u;
            }
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start;
                block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            item->vector_immediate = request->guest_bytes[cursor++];
            if ((item->vector_immediate & 0x0du) != 0x08u) {
                cursor = start;
                block->status = HL_X86_A64_UNSUPPORTED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
        } else if (opcode == 0x0fu && cursor < request->guest_size &&
                   request->guest_bytes[cursor] == 0x5bu &&
                   (semantic_prefix == 0u || semantic_prefix == 0xf3u)) {
            uint8_t modrm;
            ++cursor;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            modrm = request->guest_bytes[cursor];
            item->operation = OP_VECTOR;
            item->width = 16u;
            item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            /* No prefix is cvtdq2ps, 66 is cvtps2dq, F3 is cvttps2dq. */
            item->vector_kind = semantic_prefix == 0xf3u ? VECTOR_TRUNC_FLOAT_TO_SIGNED_DWORD :
                                operand_16 != 0u ? VECTOR_FLOAT_TO_SIGNED_DWORD :
                                                   VECTOR_SIGNED_DWORD_TO_FLOAT;
            if ((modrm >> 6) == 3u) {
                ++cursor;
            } else {
                if (!hl_x86_decode_address(request, block, item, rex, 0, address_32, start, &cursor)) break;
                item->operation = OP_VECTOR;
                item->width = 16u;
                item->memory_operand = 1u;
                item->source = 16u;
            }
        } else if (opcode == 0x0fu && cursor < request->guest_size &&
                   (request->guest_bytes[cursor] == 0x58u || request->guest_bytes[cursor] == 0x59u ||
                    request->guest_bytes[cursor] == 0x5cu || request->guest_bytes[cursor] == 0x5eu) &&
                   (semantic_prefix == 0xf2u || semantic_prefix == 0u || semantic_prefix == 0xf3u)) {
            uint8_t extension = request->guest_bytes[cursor++];
            uint8_t modrm;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            modrm = request->guest_bytes[cursor];
            item->operation = OP_VECTOR;
            item->condition = semantic_prefix == 0xf2u || semantic_prefix == 0xf3u;
            item->vector_lane = semantic_prefix == 0xf2u ||
                                (semantic_prefix == 0u && operand_16 != 0u) ? 8u : 4u;
            item->width = item->condition != 0u ? item->vector_lane : 16u;
            item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->vector_kind = VECTOR_FLOAT_ARITHMETIC;
            item->vector_subopcode = extension;
            if ((modrm >> 6) == 3u) {
                ++cursor;
            } else {
                if (!hl_x86_decode_address(request, block, item, rex, 0, address_32, start, &cursor)) break;
                item->operation = OP_VECTOR;
                item->width = item->condition != 0u ? item->vector_lane : 16u;
                item->memory_operand = 1u;
                item->source = 16u;
            }
        } else if (semantic_prefix == 0xf2u && opcode == 0x0fu && cursor < request->guest_size &&
                   request->guest_bytes[cursor] == 0x51u) {
            uint8_t modrm;
            ++cursor;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            modrm = request->guest_bytes[cursor];
            item->operation = OP_VECTOR;
            item->width = 8u;
            item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->vector_kind = VECTOR_SCALAR_SQRT_DOUBLE;
            if ((modrm >> 6) == 3u) {
                ++cursor;
            } else {
                if (!hl_x86_decode_address(request, block, item, rex, 0, address_32, start, &cursor)) break;
                item->operation = OP_VECTOR;
                item->width = 8u;
                item->memory_operand = 1u;
                item->source = 16u;
            }
        } else if (operand_16 != 0u && opcode == 0x0fu && cursor < request->guest_size &&
                   (request->guest_bytes[cursor] == 0x2eu || request->guest_bytes[cursor] == 0x2fu)) {
            uint8_t extension = request->guest_bytes[cursor++];
            uint8_t modrm;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            modrm = request->guest_bytes[cursor];
            item->operation = OP_VECTOR;
            item->width = 8u;
            item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->vector_kind = VECTOR_SCALAR_COMPARE_DOUBLE;
            item->condition = extension == 0x2fu;
            if ((modrm >> 6) == 3u) {
                ++cursor;
            } else {
                if (!hl_x86_decode_address(request, block, item, rex, 0, address_32, start, &cursor)) break;
                item->operation = OP_VECTOR;
                item->width = 8u;
                item->memory_operand = 1u;
                item->source = 16u;
            }
        } else if (semantic_prefix == 0xf2u && opcode == 0x0fu && cursor < request->guest_size &&
                   request->guest_bytes[cursor] == 0x2cu) {
            uint8_t modrm;
            ++cursor;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            modrm = request->guest_bytes[cursor];
            item->operation = OP_VECTOR;
            item->width = (rex & 8u) != 0u ? 8u : 4u;
            item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->vector_kind = VECTOR_TRUNC_DOUBLE_SIGNED;
            if ((modrm >> 6) == 3u) {
                ++cursor;
            } else {
                if (!hl_x86_decode_address(request, block, item, rex, 0, address_32, start, &cursor)) break;
                item->operation = OP_VECTOR;
                item->width = (rex & 8u) != 0u ? 8u : 4u;
                item->vector_memory_width = 8u;
                item->memory_operand = 1u;
                item->source = 16u;
            }
        } else if (opcode == 0x0fu && cursor < request->guest_size &&
                   ((request->guest_bytes[cursor] == 0x70u &&
                     (operand_16 != 0u || semantic_prefix == 0xf2u || semantic_prefix == 0xf3u)) ||
                    (request->guest_bytes[cursor] == 0xc6u &&
                     semantic_prefix == 0u))) {
            uint8_t extension = request->guest_bytes[cursor++];
            uint8_t modrm;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            modrm = request->guest_bytes[cursor];
            item->operation = OP_VECTOR;
            item->width = 16u;
            item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->vector_kind = extension == 0xc6u ?
                                    (operand_16 != 0u ? VECTOR_SHUFFLE_DOUBLE : VECTOR_SHUFFLE_FLOAT) :
                                operand_16 != 0u ? VECTOR_SHUFFLE_DWORD : VECTOR_SHUFFLE_WORD;
            item->condition = semantic_prefix == 0xf3u;
            if ((modrm >> 6) == 3u) {
                ++cursor;
            } else {
                if (!hl_x86_decode_address(request, block, item, rex, 0, address_32,
                                           start, &cursor)) break;
                item->operation = OP_VECTOR;
                item->width = 16u;
                item->memory_operand = 1u;
                item->source = 16u;
            }
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            item->vector_immediate = request->guest_bytes[cursor++];
        } else if (opcode == 0x0fu && cursor < request->guest_size &&
                   request->guest_bytes[cursor] == 0x1fu) {
            ++cursor;
            if (!hl_x86_decode_nop(request, block, item, start, &cursor)) break;
        } else if (opcode == 0x0fu && cursor < request->guest_size &&
                   ((semantic_prefix == 0u &&
                     (request->guest_bytes[cursor] == 0x28u || request->guest_bytes[cursor] == 0x29u)) ||
                    (operand_16 != 0u &&
                     ((request->guest_bytes[cursor] >= 0x60u && request->guest_bytes[cursor] <= 0x62u) ||
                      (request->guest_bytes[cursor] >= 0x68u && request->guest_bytes[cursor] <= 0x6au) ||
                      request->guest_bytes[cursor] == 0x6cu || request->guest_bytes[cursor] == 0x6du ||
                      request->guest_bytes[cursor] == 0x6fu ||
                      (request->guest_bytes[cursor] >= 0x74u && request->guest_bytes[cursor] <= 0x76u) ||
                      request->guest_bytes[cursor] == 0x7fu || request->guest_bytes[cursor] == 0xd7u ||
                      request->guest_bytes[cursor] == 0xd4u ||
                      request->guest_bytes[cursor] == 0xd5u ||
                      request->guest_bytes[cursor] == 0xdbu || request->guest_bytes[cursor] == 0xdfu ||
                      request->guest_bytes[cursor] == 0xdau || request->guest_bytes[cursor] == 0xdeu ||
                      request->guest_bytes[cursor] == 0xeau || request->guest_bytes[cursor] == 0xeeu ||
                      request->guest_bytes[cursor] == 0xe4u || request->guest_bytes[cursor] == 0xe5u ||
                      request->guest_bytes[cursor] == 0xebu || request->guest_bytes[cursor] == 0xefu ||
                      request->guest_bytes[cursor] == 0xf4u ||
                      (request->guest_bytes[cursor] >= 0xf8u && request->guest_bytes[cursor] <= 0xfeu))) ||
                    (semantic_prefix == 0xf3u &&
                     (request->guest_bytes[cursor] == 0x6fu || request->guest_bytes[cursor] == 0x7fu)))) {
            uint8_t extension = request->guest_bytes[cursor++];
            uint8_t modrm;
            uint8_t reg;
            uint8_t rm;
            int store = extension == 0x29u || extension == 0x7fu;

            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start;
                block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            modrm = request->guest_bytes[cursor];
            reg = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            rm = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->operation = OP_VECTOR;
            item->width = 16u;
            item->destination = reg;
            item->source = rm;
            item->vector_aligned = extension == 0x28u || extension == 0x29u ||
                                   (operand_16 != 0u && (extension == 0x6fu || extension == 0x7fu));
            if (extension == 0x28u || extension == 0x29u || extension == 0x6fu || extension == 0x7fu) {
                item->vector_kind = VECTOR_COPY;
                if (store) {
                    item->destination = rm;
                    item->source = reg;
                }
            } else if (extension == 0xd7u) {
                if ((modrm >> 6) != 3u) {
                    cursor = start;
                    block->status = HL_X86_A64_UNSUPPORTED;
                    block->exit = HL_X86_A64_INTERPRETER;
                    break;
                }
                item->vector_kind = VECTOR_BYTE_MASK;
            } else if (extension >= 0x74u && extension <= 0x76u) {
                item->vector_kind = VECTOR_COMPARE_EQUAL;
                item->vector_lane = (uint8_t)(1u << (extension - 0x74u));
            } else if (extension == 0xdau || extension == 0xdeu ||
                       extension == 0xeau || extension == 0xeeu) {
                /* Byte forms (0xda/0xde) are unsigned, word forms (0xea/0xee) signed. */
                uint8_t maximum = extension == 0xdeu || extension == 0xeeu;
                uint8_t unsigned_kind = extension < 0xe0u;
                item->vector_kind = maximum != 0u ?
                    (unsigned_kind != 0u ? VECTOR_MAXIMUM_UNSIGNED : VECTOR_MAXIMUM_SIGNED) :
                    (unsigned_kind != 0u ? VECTOR_MINIMUM_UNSIGNED : VECTOR_MINIMUM_SIGNED);
                item->vector_lane = unsigned_kind != 0u ? 1u : 2u;
            } else if (extension == 0xdbu) {
                item->vector_kind = VECTOR_AND;
            } else if (extension == 0xdfu) {
                item->vector_kind = VECTOR_AND_NOT;
            } else if (extension == 0xebu) {
                item->vector_kind = VECTOR_OR;
            } else if (extension == 0xefu) {
                item->vector_kind = VECTOR_XOR;
            } else if (extension == 0xd4u || extension >= 0xfcu) {
                item->vector_kind = VECTOR_ADD;
                item->vector_lane = extension == 0xfcu ? 1u : extension == 0xfdu ? 2u :
                                    extension == 0xfeu ? 4u : 8u;
            } else if (extension >= 0xf8u && extension <= 0xfbu) {
                item->vector_kind = VECTOR_SUBTRACT;
                item->vector_lane = (uint8_t)(1u << (extension - 0xf8u));
            } else if (extension == 0xd5u) {
                item->vector_kind = VECTOR_MULTIPLY_LOW_WORD;
            } else if (extension == 0xe4u || extension == 0xe5u) {
                item->vector_kind = VECTOR_MULTIPLY_HIGH_WORD;
                item->condition = extension == 0xe5u;
            } else if (extension == 0xf4u) {
                item->vector_kind = VECTOR_MULTIPLY_EVEN_DWORD;
            } else {
                item->vector_kind = (extension >= 0x68u && extension <= 0x6au) || extension == 0x6du
                                        ? VECTOR_UNPACK_HIGH : VECTOR_UNPACK_LOW;
                item->vector_lane = extension == 0x60u || extension == 0x68u ? 1u :
                                    extension == 0x61u || extension == 0x69u ? 2u :
                                    extension == 0x62u || extension == 0x6au ? 4u : 8u;
            }
            if ((modrm >> 6) == 3u) {
                ++cursor;
            } else {
                if (extension == 0xd7u ||
                    !hl_x86_decode_address(request, block, item, rex, 0, address_32,
                                           start, &cursor)) break;
                item->operation = OP_VECTOR;
                item->width = 16u;
                item->memory_operand = 1u;
                item->memory_write = (uint8_t)store;
                if (!store && item->vector_kind != VECTOR_COPY) item->source = 16u;
            }
        } else if (opcode == 0x0fu && cursor < request->guest_size &&
                   ((semantic_prefix == 0u &&
                     (request->guest_bytes[cursor] == 0x10u || request->guest_bytes[cursor] == 0x11u ||
                      (operand_16 != 0u && (request->guest_bytes[cursor] == 0x6eu ||
                                           request->guest_bytes[cursor] == 0x7eu ||
                                           request->guest_bytes[cursor] == 0xd6u)))) ||
                    (semantic_prefix == 0xf3u && request->guest_bytes[cursor] == 0x7eu))) {
            uint8_t extension = request->guest_bytes[cursor++];
            uint8_t modrm;
            uint8_t reg;
            uint8_t rm;

            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start;
                block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            modrm = request->guest_bytes[cursor];
            reg = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            rm = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->operation = OP_VECTOR;
            item->width = extension == 0x10u || extension == 0x11u ? 16u :
                          semantic_prefix == 0xf3u || (rex & 8u) != 0u ? 8u : 4u;
            if (extension == 0x10u) {
                item->destination = reg;
                item->source = rm;
                item->vector_kind = VECTOR_COPY;
            } else if (extension == 0x11u) {
                item->destination = rm;
                item->source = reg;
                item->vector_kind = VECTOR_COPY;
            } else if (extension == 0x6eu) {
                item->destination = reg;
                item->source = rm;
                item->vector_kind = VECTOR_FROM_INTEGER;
            } else if (semantic_prefix == 0xf3u) {
                item->destination = reg;
                item->source = rm;
                item->vector_kind = VECTOR_ZERO_LOW;
            } else if (extension == 0xd6u) {
                item->destination = rm;
                item->source = reg;
                item->width = 8u;
                item->vector_kind = VECTOR_ZERO_LOW;
            } else {
                item->destination = rm;
                item->source = reg;
                item->vector_kind = VECTOR_TO_INTEGER;
            }
            if ((modrm >> 6) == 3u) {
                ++cursor;
            } else {
                uint8_t vector = reg;
                if (!hl_x86_decode_address(request, block, item, rex, 0, address_32,
                                           start, &cursor)) break;
                item->operation = OP_VECTOR;
                item->width = extension == 0x10u || extension == 0x11u ? 16u :
                              extension == 0xd6u ? 8u :
                              semantic_prefix == 0xf3u || (rex & 8u) != 0u ? 8u : 4u;
                item->memory_operand = 1u;
                item->memory_write = extension == 0x11u ||
                                     (extension == 0x7eu && semantic_prefix == 0u) || extension == 0xd6u;
                item->destination = vector;
                item->source = vector;
                item->vector_kind = item->memory_write != 0u && extension == 0x7eu ? VECTOR_TO_INTEGER :
                                    item->memory_write != 0u && extension == 0xd6u ? VECTOR_ZERO_LOW : VECTOR_COPY;
            }
        } else if ((opcode == 0xf6u || opcode == 0xf7u) && cursor < request->guest_size &&
                   ((request->guest_bytes[cursor] >> 3) & 7u) == 4u) {
            if (!hl_x86_decode_mul(request, block, item, opcode, rex, operand_16,
                                   address_32, start, &cursor)) break;
        } else if ((opcode == 0xf6u || opcode == 0xf7u) && cursor < request->guest_size &&
                   ((request->guest_bytes[cursor] >> 3) & 7u) >= 6u) {
            if (!hl_x86_decode_div(request, block, item, opcode, rex, operand_16,
                                   address_32, start, &cursor)) break;
        } else if (opcode == 0x69u || opcode == 0x6bu ||
                   (opcode == 0x0fu && cursor < request->guest_size &&
                    request->guest_bytes[cursor] == 0xafu)) {
            uint8_t extension = opcode;
            if (opcode == 0x0fu) { extension = request->guest_bytes[cursor]; ++cursor; }
            if (!hl_x86_decode_imul(request, block, item, extension, rex, operand_16,
                                    address_32, start, &cursor)) break;
        } else if (opcode == 0x90u && rex == 0) {
            item->operation = OP_NOP;
        } else if ((opcode == 0x98u || opcode == 0x99u) && semantic_prefix == 0u) {
            item->operation = OP_ACCUMULATOR;
            item->width = (rex & 8u) != 0u ? 8u : operand_16 != 0u ? 2u : 4u;
            item->source_high = opcode == 0x99u;
        } else if (opcode == 0x0fu && cursor < request->guest_size && cursor - start < 15u &&
                   request->guest_bytes[cursor] >= 0x40u && request->guest_bytes[cursor] <= 0x4fu) {
            uint8_t extension = request->guest_bytes[cursor++];
            uint8_t modrm;

            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start;
                block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            modrm = request->guest_bytes[cursor];
            item->condition = extension & 0x0fu;
            item->conditional = 1u;
            item->width = (rex & 8u) != 0 ? 8u : operand_16 != 0 ? 2u : 4u;
            if ((modrm >> 6) == 3u) {
                ++cursor;
                item->operation = OP_CMOV;
                item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
                item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            } else {
                if (!hl_x86_decode_address(request, block, item, rex, 0, address_32, start, &cursor)) break;
                item->operation = OP_LOAD;
                item->conditional = 1u;
                item->condition = extension & 0x0fu;
                item->width = (rex & 8u) != 0 ? 8u : operand_16 != 0 ? 2u : 4u;
            }
        } else if (semantic_prefix == 0u && opcode == 0x0fu && cursor < request->guest_size && cursor - start < 15u &&
                   request->guest_bytes[cursor] >= 0x90u && request->guest_bytes[cursor] <= 0x9fu) {
            uint8_t extension = request->guest_bytes[cursor++];
            uint8_t modrm;
            uint8_t raw;

            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start;
                block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            modrm = request->guest_bytes[cursor];
            raw = modrm & 7u;
            item->operation = OP_SET;
            item->condition = extension & 0x0fu;
            item->width = 1u;
            item->destination = (uint8_t)(raw | ((rex & 1u) << 3));
            if ((modrm >> 6) == 3u) {
                ++cursor;
                if (rex == 0u && raw >= 4u) {
                    item->destination = (uint8_t)(raw - 4u);
                    item->destination_high = 1u;
                }
            } else {
                if (!hl_x86_decode_address(request, block, item, rex, 0, address_32,
                                           start, &cursor)) break;
                item->operation = OP_SET;
                item->condition = extension & 0x0fu;
                item->width = 1u;
                item->memory_operand = 1u;
                item->memory_write = 1u;
            }
        } else if (opcode >= 0xb8u && opcode <= 0xbfu) {
            size_t immediate_size = (rex & 8u) != 0 ? 8u : operand_16 != 0 ? 2u : 4u;

            if (immediate_size > request->guest_size - cursor || immediate_size > 15u - (cursor - start)) {
                cursor = start;
                block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            item->operation = OP_MOV_IMMEDIATE;
            item->destination = (uint8_t)((opcode - 0xb8u) | ((rex & 1u) << 3));
            item->immediate = load_unsigned(&request->guest_bytes[cursor], immediate_size);
            item->width = (uint8_t)immediate_size;
            cursor += immediate_size;
        } else if (opcode == 0x0fu && cursor < request->guest_size && cursor - start < 15u &&
                   request->guest_bytes[cursor] >= 0xc8u && request->guest_bytes[cursor] <= 0xcfu) {
            uint8_t extension = request->guest_bytes[cursor++];

            item->operation = OP_BSWAP;
            item->destination = (uint8_t)((extension & 7u) | ((rex & 1u) << 3));
            item->width = (rex & 8u) != 0 ? 8u : 4u;
        } else if (semantic_prefix == 0u && opcode == 0x0fu && cursor < request->guest_size &&
                   (request->guest_bytes[cursor] == 0xa3u || request->guest_bytes[cursor] == 0xabu ||
                    request->guest_bytes[cursor] == 0xb3u || request->guest_bytes[cursor] == 0xbbu ||
                    request->guest_bytes[cursor] == 0xbau)) {
            uint8_t extension = request->guest_bytes[cursor++];
            if (!hl_x86_decode_bit(request, block, item, extension, rex, operand_16,
                                   address_32, start, &cursor)) break;
        } else if ((semantic_prefix == 0u || semantic_prefix == 0xf3u) &&
                   opcode == 0x0fu && cursor < request->guest_size && cursor - start < 15u &&
                   (request->guest_bytes[cursor] == 0xbcu || request->guest_bytes[cursor] == 0xbdu)) {
            uint8_t extension = request->guest_bytes[cursor++];
            uint8_t modrm;
            uint8_t destination;
            uint8_t width = (rex & 8u) != 0u ? 8u : operand_16 != 0u ? 2u : 4u;

            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start;
                block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            modrm = request->guest_bytes[cursor];
            destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->operation = OP_BITSCAN;
            item->destination = destination;
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->width = width;
            item->condition = extension == 0xbdu;
            item->conditional = semantic_prefix == 0xf3u;
            if ((modrm >> 6) == 3u) {
                ++cursor;
            } else {
                if (!hl_x86_decode_address(request, block, item, rex, 0, address_32,
                                           start, &cursor)) break;
                item->operation = OP_BITSCAN;
                item->destination = destination;
                item->width = width;
                item->load_width = width;
                item->condition = extension == 0xbdu;
                item->conditional = semantic_prefix == 0xf3u;
                item->memory_operand = 1u;
            }
        } else if (opcode == 0x63u ||
                   (opcode == 0x0fu && cursor < request->guest_size && cursor - start < 15u &&
                    (request->guest_bytes[cursor] == 0xb6u || request->guest_bytes[cursor] == 0xb7u ||
                     request->guest_bytes[cursor] == 0xbeu || request->guest_bytes[cursor] == 0xbfu))) {
            uint8_t extension = opcode == 0x0fu ? request->guest_bytes[cursor++] : 0x63u;
            uint8_t modrm;

            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start;
                block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            modrm = request->guest_bytes[cursor];
            item->destination = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->width = (rex & 8u) != 0 ? 8u : operand_16 != 0 ? 2u : 4u;
            if (extension == 0x63u) {
                item->source_high = 4u;
                item->signed_extend = item->width == 8u;
            } else {
                item->source_high = (extension & 1u) != 0 ? 2u : 1u;
                item->signed_extend = extension >= 0xbeu;
                if (item->source_high == 1u && rex == 0 && (modrm & 7u) >= 4u) {
                    item->source = (uint8_t)((modrm & 7u) - 4u);
                    item->source_high = 8u;
                }
            }
            if ((modrm >> 6) != 3u) {
                if (!hl_x86_decode_address(request, block, item, rex, 0, address_32,
                                           start, &cursor)) break;
                item->operation = OP_LOAD;
                item->width = (rex & 8u) != 0 ? 8u : operand_16 != 0 ? 2u : 4u;
                item->load_width = item->source_high == 8u ? 1u : item->source_high;
            } else {
                ++cursor;
                item->operation = OP_EXTEND;
            }
        } else if (opcode >= 0xb0u && opcode <= 0xb7u) {
            uint8_t raw = (uint8_t)(opcode & 7u);

            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start;
                block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            item->operation = OP_BYTE;
            item->has_immediate = 1u;
            item->immediate = request->guest_bytes[cursor++];
            item->destination = (uint8_t)(raw | ((rex & 1u) << 3));
            if (rex == 0 && raw >= 4u) {
                item->destination = (uint8_t)(raw - 4u);
                item->destination_high = 1u;
            }
        } else if ((semantic_prefix == 0u || semantic_prefix == 0xf0u) && opcode == 0x0fu &&
                   cursor < request->guest_size && cursor - start < 15u &&
                   (request->guest_bytes[cursor] == 0xc0u || request->guest_bytes[cursor] == 0xc1u)) {
            uint8_t extension = request->guest_bytes[cursor++];
            if (!hl_x86_decode_xadd(request, block, item, extension, rex, operand_16,
                                    address_32, semantic_prefix == 0xf0u, start, &cursor)) break;
        } else if ((semantic_prefix == 0u || semantic_prefix == 0xf0u) &&
                   (request->flags & HL_X86_A64_LSE) != 0u && opcode == 0x0fu &&
                   cursor < request->guest_size && cursor - start < 15u &&
                   (request->guest_bytes[cursor] == 0xb0u || request->guest_bytes[cursor] == 0xb1u)) {
            uint8_t extension = request->guest_bytes[cursor++];
            uint8_t modrm;
            uint8_t raw;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            modrm = request->guest_bytes[cursor];
            if ((modrm >> 6) == 3u) {
                cursor = start; block->status = HL_X86_A64_UNSUPPORTED;
                block->exit = HL_X86_A64_INTERPRETER; break;
            }
            raw = (modrm >> 3) & 7u;
            if (!hl_x86_decode_address(request, block, item, rex, 0, address_32, start, &cursor)) break;
            item->operation = OP_CMPXCHG;
            item->memory_operand = 1u;
            item->memory_write = 1u;
            item->source = (uint8_t)(raw | ((rex & 4u) << 1));
            item->width = extension == 0xb0u ? 1u :
                          (rex & 8u) != 0u ? 8u : operand_16 != 0u ? 2u : 4u;
            if (extension == 0xb0u && rex == 0u && raw >= 4u) {
                item->source = (uint8_t)(raw - 4u);
                item->source_high = 8u;
            }
        } else if ((request->flags & HL_X86_A64_LSE) != 0u &&
                   (opcode == 0x86u || opcode == 0x87u) && cursor < request->guest_size &&
                   (request->guest_bytes[cursor] >> 6) != 3u) {
            uint8_t modrm = request->guest_bytes[cursor];
            uint8_t raw = (modrm >> 3) & 7u;
            if (!hl_x86_decode_address(request, block, item, rex, 0, address_32, start, &cursor)) break;
            item->operation = OP_XCHG;
            item->memory_operand = 1u;
            item->memory_write = 1u;
            item->source = (uint8_t)(raw | ((rex & 4u) << 1));
            item->destination = item->source;
            item->width = opcode == 0x86u ? 1u :
                          (rex & 8u) != 0u ? 8u : operand_16 != 0u ? 2u : 4u;
            if (opcode == 0x86u && rex == 0u && raw >= 4u) {
                item->source = (uint8_t)(raw - 4u);
                item->destination = item->source;
                item->source_high = 8u;
                item->destination_high = 1u;
            }
        } else if ((opcode == 0x86u || opcode == 0x87u) && semantic_prefix == 0xf0u) {
            cursor = start; block->status = HL_X86_A64_UNSUPPORTED;
            block->exit = HL_X86_A64_INTERPRETER; break;
        } else if ((semantic_prefix == 0u || semantic_prefix == 0xf2u || semantic_prefix == 0xf3u) &&
                   opcode == 0x0fu && cursor < request->guest_size &&
                   request->guest_bytes[cursor] == 0xc3u) {
            ++cursor;
            if (!hl_x86_decode_store(request, block, item, 0x89u, rex, operand_16,
                                     address_32, start, &cursor)) break;
        } else if ((opcode == 0x88u || opcode == 0x89u) && cursor < request->guest_size &&
                   (request->guest_bytes[cursor] >> 6) != 3u) {
            if (!hl_x86_decode_store(request, block, item, opcode, rex, operand_16,
                                     address_32, start, &cursor)) break;
        } else if ((opcode == 0x8au || opcode == 0x8bu) && cursor < request->guest_size &&
                   (request->guest_bytes[cursor] >> 6) != 3u) {
            if (!hl_x86_decode_load(request, block, item, opcode, rex, operand_16,
                                    address_32, start, &cursor)) break;
        } else if ((opcode == 0xc6u || opcode == 0xc7u) && cursor < request->guest_size &&
                   (request->guest_bytes[cursor] >> 6) != 3u) {
            uint8_t modrm = request->guest_bytes[cursor];
            size_t immediate_size = opcode == 0xc6u ? 1u : operand_16 != 0u ? 2u : 4u;
            if (((modrm >> 3) & 7u) != 0u) {
                cursor = start;
                block->status = HL_X86_A64_UNSUPPORTED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            if (!hl_x86_decode_address(request, block, item, rex, 0, address_32, start, &cursor)) break;
            if (immediate_size > request->guest_size - cursor || immediate_size > 15u - (cursor - start)) {
                cursor = start;
                block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            item->operation = OP_STORE;
            item->width = opcode == 0xc6u ? 1u : (rex & 8u) != 0u ? 8u : operand_16 != 0u ? 2u : 4u;
            item->has_immediate = 1u;
            item->operand_immediate = opcode == 0xc6u ? request->guest_bytes[cursor] :
                                      item->width == 8u ? (uint64_t)load_signed(&request->guest_bytes[cursor], 4u) :
                                                         load_unsigned(&request->guest_bytes[cursor], immediate_size);
            cursor += immediate_size;
        } else if (opcode == 0x88u || opcode == 0x8au || opcode == 0xc6u) {
            uint8_t modrm;
            uint8_t reg;
            uint8_t rm;

            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start;
                block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            modrm = request->guest_bytes[cursor++];
            if ((modrm >> 6) != 3u || (opcode == 0xc6u && ((modrm >> 3) & 7u) != 0u)) {
                cursor = start;
                block->status = HL_X86_A64_UNSUPPORTED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            reg = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            rm = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            item->operation = OP_BYTE;
            if (opcode == 0xc6u) {
                if (cursor >= request->guest_size || cursor - start >= 15u) {
                    cursor = start;
                    block->status = HL_X86_A64_TRUNCATED;
                    block->exit = HL_X86_A64_INTERPRETER;
                    break;
                }
                item->destination = rm;
                item->has_immediate = 1u;
                item->immediate = request->guest_bytes[cursor++];
            } else {
                item->destination = opcode == 0x8au ? reg : rm;
                item->source = opcode == 0x8au ? rm : reg;
            }
            if (rex == 0) {
                uint8_t destination_raw = opcode == 0x8au ? (uint8_t)((modrm >> 3) & 7u) :
                                                            (uint8_t)(modrm & 7u);
                uint8_t source_raw = opcode == 0x8au ? (uint8_t)(modrm & 7u) :
                                                       (uint8_t)((modrm >> 3) & 7u);

                if (destination_raw >= 4u) {
                    item->destination = (uint8_t)(destination_raw - 4u);
                    item->destination_high = 1u;
                }
                if (opcode != 0xc6u && source_raw >= 4u) {
                    item->source = (uint8_t)(source_raw - 4u);
                    item->source_high = 8u;
                }
            }
        } else if (opcode < 0x40u && (opcode & 7u) <= 3u) {
            uint8_t modrm;
            uint8_t byte;

            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start;
                block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            modrm = request->guest_bytes[cursor++];
            byte = (uint8_t)((opcode & 1u) == 0u);
            if ((modrm >> 6) != 3u && (byte != 0u || operand_16 == 0u)) {
                uint8_t reg = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
                uint8_t raw_reg = (uint8_t)((modrm >> 3) & 7u);
                --cursor;
                if (!hl_x86_decode_address(request, block, item, rex, 0,
                                           address_32, start, &cursor)) break;
                item->operation = OP_ALU;
                item->alu_kind = (uint8_t)((opcode >> 3) & 7u);
                item->width = byte != 0u ? 1u : (rex & 8u) != 0 ? 8u : 4u;
                item->memory_operand = 1u;
                item->memory_destination = (opcode & 2u) == 0u;
                item->memory_write = (opcode & 2u) == 0u && item->alu_kind != 7u;
                item->flags_only = item->alu_kind == 7u;
                if ((opcode & 2u) == 0u) {
                    item->source = reg;
                    if (byte != 0u && rex == 0u && raw_reg >= 4u) {
                        item->source = (uint8_t)(raw_reg - 4u);
                        item->source_high = 8u;
                    }
                } else {
                    item->destination = reg;
                    if (byte != 0u && rex == 0u && raw_reg >= 4u) {
                        item->destination = (uint8_t)(raw_reg - 4u);
                        item->destination_high = 1u;
                    }
                }
            } else if ((modrm >> 6) != 3u || (!byte && operand_16 != 0)) {
                cursor = start;
                block->status = (modrm >> 6) == 3u ? HL_X86_A64_FLAGS_ABI_REQUIRED : HL_X86_A64_UNSUPPORTED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            } else {
                item->operation = OP_ALU;
                item->alu_kind = (uint8_t)((opcode >> 3) & 7u);
                item->width = byte != 0u ? 1u : (rex & 8u) != 0 ? 8u : 4u;
                item->destination = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
                item->source = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
                if ((opcode & 2u) != 0) {
                    uint8_t swap = item->destination;
                    item->destination = item->source;
                    item->source = swap;
                }
                if (byte != 0u && rex == 0u) {
                    uint8_t destination_raw = (opcode & 2u) != 0u ?
                                                  (uint8_t)((modrm >> 3) & 7u) :
                                                  (uint8_t)(modrm & 7u);
                    uint8_t source_raw = (opcode & 2u) != 0u ?
                                             (uint8_t)(modrm & 7u) :
                                             (uint8_t)((modrm >> 3) & 7u);
                    if (destination_raw >= 4u) {
                        item->destination = (uint8_t)(destination_raw - 4u);
                        item->destination_high = 1u;
                    }
                    if (source_raw >= 4u) {
                        item->source = (uint8_t)(source_raw - 4u);
                        item->source_high = 8u;
                    }
                }
                item->flags_only = item->alu_kind == 7u;
            }
        } else if (semantic_prefix == 0u && opcode == 0x0fu && cursor < request->guest_size &&
                   (request->guest_bytes[cursor] == 0xa4u || request->guest_bytes[cursor] == 0xa5u ||
                    request->guest_bytes[cursor] == 0xacu || request->guest_bytes[cursor] == 0xadu)) {
            uint8_t extension = request->guest_bytes[cursor++];
            if (!hl_x86_decode_double_shift(request, block, item, extension, rex, operand_16,
                                            address_32, start, &cursor)) break;
        } else if (hl_x86_test_opcode(opcode)) {
            if (!hl_x86_decode_test(request, block, item, opcode, rex, operand_16, address_32,
                                    semantic_prefix == 0xf0u, start, &cursor)) break;
        } else if (hl_x86_shift_opcode(opcode)) {
            if (!hl_x86_decode_shift(request, block, item, opcode, rex, operand_16,
                                     address_32, start, &cursor)) break;
        } else if (hl_x86_immediate_opcode(opcode)) {
            if (!hl_x86_decode_immediate(request, block, item, opcode, rex, operand_16,
                                         address_32, start, &cursor)) break;
        } else if (opcode == 0x8du) {
            if (!hl_x86_decode_address(request, block, item, rex, operand_16, address_32, start, &cursor)) break;
        } else if (opcode == 0x89u || opcode == 0x8bu || opcode == 0xc7u) {
            uint8_t modrm;
            uint8_t reg;
            uint8_t rm;
            size_t immediate_size;

            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start;
                block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            modrm = request->guest_bytes[cursor++];
            reg = (uint8_t)(((modrm >> 3) & 7u) | ((rex & 4u) << 1));
            rm = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
            if ((modrm >> 6) != 3u || (opcode == 0xc7u && ((modrm >> 3) & 7u) != 0u)) {
                cursor = start;
                block->status = HL_X86_A64_UNSUPPORTED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            item->width = (rex & 8u) != 0 ? 8u : operand_16 != 0 ? 2u : 4u;
            immediate_size = item->width == 2u ? 2u : 4u;
            if (opcode == 0xc7u) {
                if (immediate_size > request->guest_size - cursor || immediate_size > 15u - (cursor - start)) {
                    cursor = start;
                    block->status = HL_X86_A64_TRUNCATED;
                    block->exit = HL_X86_A64_INTERPRETER;
                    break;
                }
                item->operation = OP_MOV_IMMEDIATE;
                item->destination = rm;
                item->immediate = load_unsigned(&request->guest_bytes[cursor], immediate_size);
                if (item->width == 8u && (item->immediate & UINT64_C(0x80000000)) != 0)
                    item->immediate |= UINT64_C(0xffffffff00000000);
                cursor += immediate_size;
            } else {
                item->operation = OP_MOV_REGISTER;
                item->destination = opcode == 0x8bu ? reg : rm;
                item->source = opcode == 0x8bu ? rm : reg;
            }
        } else if (opcode == 0xe8u) {
            uint64_t next;
            if (4u > request->guest_size - cursor || 4u > 15u - (cursor - start) ||
                !add_pc(block->next_pc, (cursor - start) + 4u, &next) ||
                !branch_target(next, load_signed(&request->guest_bytes[cursor], 4u), &block->target)) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED; block->exit = HL_X86_A64_INTERPRETER; break;
            }
            cursor += 4u; block->next_pc = next; block->exit = HL_X86_A64_DIRECT_CALL;
            item->operation = OP_CALL; item->has_immediate = 1u; item->immediate = block->target;
            item->length = (uint8_t)(cursor - start); ++block->count; break;
        } else if (opcode == 0xc9u && operand_16 == 0u && address_32 == 0u && semantic_prefix == 0u) {
            item->operation = OP_LEAVE;
            item->width = 8u;
            item->address_base = 5u;
            item->address_index = UINT8_MAX;
        } else if (opcode == 0xc3u || opcode == 0xc2u) {
            uint64_t next;
            if (opcode == 0xc2u && (2u > request->guest_size - cursor || 2u > 15u - (cursor - start))) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED; block->exit = HL_X86_A64_INTERPRETER; break;
            }
            if (opcode == 0xc2u) { item->immediate = load_unsigned(&request->guest_bytes[cursor], 2u); cursor += 2u; }
            if (!add_pc(block->next_pc, cursor - start, &next)) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED; block->exit = HL_X86_A64_INTERPRETER; break;
            }
            block->next_pc = next; block->exit = HL_X86_A64_DYNAMIC_BRANCH; item->operation = OP_RETURN;
            item->length = (uint8_t)(cursor - start); ++block->count; break;
        } else if (opcode == 0xfeu || opcode == 0xffu) {
            uint8_t modrm;
            if (cursor >= request->guest_size || cursor - start >= 15u) {
                cursor = start; block->status = HL_X86_A64_TRUNCATED; block->exit = HL_X86_A64_INTERPRETER; break;
            }
            modrm = request->guest_bytes[cursor];
            uint8_t sub = (modrm >> 3) & 7u;
            if (sub == 0u || sub == 1u) {
                if ((modrm >> 6) != 3u) {
                    cursor = start;
                    block->status = HL_X86_A64_UNSUPPORTED;
                    block->exit = HL_X86_A64_INTERPRETER;
                    break;
                }
                item->operation = OP_ALU;
                item->alu_kind = sub == 0u ? 0u : 5u;
                item->width = opcode == 0xfeu ? 1u :
                              (rex & 8u) != 0u ? 8u : operand_16 != 0u ? 2u : 4u;
                item->has_immediate = 1u;
                item->immediate = 1u;
                item->operand_immediate = 1u;
                item->preserve_carry = 1u;
                ++cursor;
                item->destination = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
                if (opcode == 0xfeu && rex == 0u && (modrm & 7u) >= 4u) {
                    item->destination = (uint8_t)((modrm & 7u) - 4u);
                    item->destination_high = 1u;
                }
            } else if (opcode != 0xffu || (sub != 2u && sub != 4u) || operand_16 != 0u) {
                cursor = start; block->status = HL_X86_A64_UNSUPPORTED; block->exit = HL_X86_A64_INTERPRETER; break;
            } else {
                item->operation = sub == 2u ? OP_CALL : OP_JUMP; item->width = 8u;
                if ((modrm >> 6) == 3u) {
                    ++cursor; item->source = (uint8_t)((modrm & 7u) | ((rex & 1u) << 3));
                } else {
                    if (!hl_x86_decode_address(request, block, item, rex, 0, address_32, start, &cursor)) break;
                    item->operation = sub == 2u ? OP_CALL : OP_JUMP; item->source_high = 1u;
                }
                if (!add_pc(block->next_pc, cursor - start, &block->next_pc)) {
                    cursor = start; block->status = HL_X86_A64_TRUNCATED; block->exit = HL_X86_A64_INTERPRETER; break;
                }
                block->exit = HL_X86_A64_DYNAMIC_BRANCH; item->length = (uint8_t)(cursor - start); ++block->count; break;
            }
        } else if ((opcode >= 0x70u && opcode <= 0x7fu) ||
                   (opcode == 0x0fu && cursor < request->guest_size && cursor - start < 15u &&
                    request->guest_bytes[cursor] >= 0x80u && request->guest_bytes[cursor] <= 0x8fu)) {
            size_t immediate_size = opcode == 0x0fu ? 4u : 1u;
            uint8_t condition = opcode == 0x0fu ? (uint8_t)(request->guest_bytes[cursor++] & 0x0fu) :
                                                  (uint8_t)(opcode & 0x0fu);
            uint64_t next;

            if (immediate_size > request->guest_size - cursor || immediate_size > 15u - (cursor - start) ||
                !add_pc(block->next_pc, (cursor - start) + immediate_size, &next) ||
                !branch_target(next, load_signed(&request->guest_bytes[cursor], immediate_size), &block->target)) {
                block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            cursor += immediate_size;
            block->next_pc = next;
            block->exit = HL_X86_A64_CONDITIONAL_BRANCH;
            item->operation = OP_CONTROL;
            item->condition = condition;
            item->length = (uint8_t)(cursor - start);
            ++block->count;
            break;
        } else if (opcode == 0xebu || opcode == 0xe9u) {
            size_t immediate_size = opcode == 0xebu ? 1u : 4u;
            uint64_t next;

            if (immediate_size > request->guest_size - cursor || immediate_size > 15u - (cursor - start) ||
                !add_pc(block->next_pc, (cursor - start) + immediate_size, &next) ||
                !branch_target(next, load_signed(&request->guest_bytes[cursor], immediate_size), &block->target)) {
                block->status = HL_X86_A64_TRUNCATED;
                block->exit = HL_X86_A64_INTERPRETER;
                break;
            }
            block->next_pc = next;
            block->exit = HL_X86_A64_DIRECT_BRANCH;
            cursor += immediate_size;
            item->operation = OP_CONTROL;
            item->length = (uint8_t)(cursor - start);
            ++block->count;
            break;
        } else if (opcode == 0x0fu && cursor < request->guest_size && cursor - start < 15u &&
                   request->guest_bytes[cursor] == 0x05u) {
            ++cursor;
            if (!add_pc(block->next_pc, cursor - start, &block->next_pc)) block->status = HL_X86_A64_TRUNCATED;
            block->exit = block->status == HL_X86_A64_OK ? HL_X86_A64_SYSCALL : HL_X86_A64_INTERPRETER;
            item->operation = OP_CONTROL;
            item->length = (uint8_t)(cursor - start);
            ++block->count;
            break;
        } else {
            cursor = start;
            block->status = HL_X86_A64_UNSUPPORTED;
            block->exit = HL_X86_A64_INTERPRETER;
            break;
        }
        item->length = (uint8_t)(cursor - start);
        if (!add_pc(block->next_pc, item->length, &block->next_pc)) {
            block->status = HL_X86_A64_TRUNCATED;
            block->exit = HL_X86_A64_INTERPRETER;
            break;
        }
        ++block->count;
    }
    if (block->count != 0 && block->status == HL_X86_A64_UNSUPPORTED)
        block->status = HL_X86_A64_OK;
}

hl_x86_a64_status hl_x86_a64_emit(const hl_x86_a64_request *request, hl_x86_a64_result *result) {
    decode block;
    uint32_t dirty = 0;
    int vector_marked = 0;
    uint32_t words = 0;
    uint32_t index;
    uint32_t accounted = 0;
    int checkpoints = (request->flags & HL_X86_A64_CHECKPOINTS) != 0u;
    int live_chain = (request->flags & HL_X86_A64_LIVE_CHAIN) != 0u;

    if (!hl_x86_request_valid(request, result)) return HL_X86_A64_ARGUMENT;
    decode_block(request, &block);
    if (block.count > request->provenance_capacity) return HL_X86_A64_CAPACITY;
    mark_dead_flags(&block);
    for (index = 0; index < block.count; ++index) {
        if (!vector_marked && (request->flags & HL_X86_A64_DIAGNOSTICS) != 0u &&
            vector_register_write(&block.instructions[index])) {
            words += 2u;
            vector_marked = 1;
        }
        if (checkpoints && may_fallback(&block.instructions[index])) {
            words += (live_chain ? 1u : bit_count(dirty) + 3u);
            dirty = 0;
        }
        if (block.instructions[index].operation == OP_MOV_IMMEDIATE) {
            words += constant_words(block.instructions[index].immediate) +
                     (block.instructions[index].width == 2u ? 1u : 0u);
            dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_MOV_REGISTER) {
            ++words;
            dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_BSWAP) {
            ++words;
            dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_BITSCAN) {
            words += hl_x86_bitscan_words(&block.instructions[index]);
            dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_BIT) {
            words += hl_x86_bit_words(&block.instructions[index]);
            if (block.instructions[index].memory_operand == 0u &&
                block.instructions[index].condition != 0u)
                dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_STRING) {
            words += hl_x86_string_words(&block.instructions[index]);
            dirty |= UINT32_C(1) << 7;
            if (block.instructions[index].condition == 0u) dirty |= UINT32_C(1) << 6;
            if (block.instructions[index].condition == 2u) dirty |= UINT32_C(1);
            if (block.instructions[index].conditional != 0u) dirty |= UINT32_C(1) << 1;
        } else if (block.instructions[index].operation == OP_EXTEND) {
            words += 1u + (block.instructions[index].source_high == 8u ? 1u : 0u) +
                     (block.instructions[index].width == 2u ? 1u : 0u);
            dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_BYTE) {
            words += 1u + (block.instructions[index].source_high == 8u ? 1u : 0u) +
                     (block.instructions[index].has_immediate != 0u ? 1u : 0u);
            dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_ALU) {
            words += hl_x86_alu_words(&block.instructions[index]);
            if (!block.instructions[index].flags_only &&
                (block.instructions[index].memory_operand == 0u || block.instructions[index].memory_write == 0u))
                dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_IMUL) {
            words += hl_x86_imul_words(&block.instructions[index]);
            dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_MUL) {
            words += hl_x86_mul_words(&block.instructions[index]);
            dirty |= UINT32_C(1) << 0;
            if (block.instructions[index].width != 1u) dirty |= UINT32_C(1) << 2;
        } else if (block.instructions[index].operation == OP_DIV) {
            words += hl_x86_div_words(&block.instructions[index]);
            dirty |= UINT32_C(1) << 0;
            if (block.instructions[index].width != 1u) dirty |= UINT32_C(1) << 2;
        } else if (block.instructions[index].operation == OP_SHIFT) {
            words += hl_x86_shift_words(&block.instructions[index]);
            dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_DOUBLE_SHIFT) {
            words += hl_x86_double_shift_words(&block.instructions[index]);
            if (block.instructions[index].memory_operand == 0u)
                dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_ROTATE) {
            words += hl_x86_rotate_words(&block.instructions[index]);
            if (block.instructions[index].memory_operand == 0u)
                dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_ADDRESS) {
            words += hl_x86_address_words(&block.instructions[index]);
            dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_LOAD) {
            words += hl_x86_load_words(&block.instructions[index]);
            dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_CMOV) {
            words += hl_x86_cmov_words(&block.instructions[index]);
            dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_SET) {
            words += hl_x86_set_words(&block.instructions[index]);
            if (block.instructions[index].memory_operand == 0u)
                dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_STORE) {
            words += hl_x86_store_words(&block.instructions[index]);
        } else if (block.instructions[index].operation == OP_XCHG) {
            words += hl_x86_xchg_words(&block.instructions[index]);
            dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_XADD) {
            words += hl_x86_xadd_words(&block.instructions[index]);
            dirty |= UINT32_C(1) << block.instructions[index].source;
            if (block.instructions[index].memory_operand == 0u)
                dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_CMPXCHG) {
            words += hl_x86_cmpxchg_words(&block.instructions[index]);
            dirty |= UINT32_C(1) << 0;
        } else if (block.instructions[index].operation == OP_VECTOR) {
            words += hl_x86_vector_words(&block.instructions[index]);
            if (block.instructions[index].vector_kind == VECTOR_STRING_EQUAL_EACH)
                dirty |= UINT32_C(1) << 1;
            else if (block.instructions[index].vector_kind == VECTOR_TRUNC_DOUBLE_SIGNED)
                dirty |= UINT32_C(1) << block.instructions[index].destination;
            else if (block.instructions[index].memory_operand == 0u &&
                (block.instructions[index].vector_kind == VECTOR_TO_INTEGER ||
                 block.instructions[index].vector_kind == VECTOR_BYTE_MASK))
                dirty |= UINT32_C(1) << block.instructions[index].destination;
        } else if (block.instructions[index].operation == OP_ACCUMULATOR) {
            words += hl_x86_accumulator_words(&block.instructions[index]);
            dirty |= UINT32_C(1) << 0;
            if (block.instructions[index].source_high != 0u) dirty |= UINT32_C(1) << 2;
        } else if (block.instructions[index].operation == OP_CALL ||
                   block.instructions[index].operation == OP_JUMP ||
                   block.instructions[index].operation == OP_RETURN ||
                   block.instructions[index].operation == OP_LEAVE ||
                   block.instructions[index].operation == OP_PUSH ||
                   block.instructions[index].operation == OP_POP) {
            words += hl_x86_control_words(&block.instructions[index]);
            if (block.instructions[index].operation == OP_LEAVE)
                dirty |= UINT32_C(1) << 4 | UINT32_C(1) << 5;
            if (block.instructions[index].operation == OP_PUSH)
                dirty |= UINT32_C(1) << 4;
            if (block.instructions[index].operation == OP_POP)
                dirty |= UINT32_C(1) << 4 | UINT32_C(1) << block.instructions[index].destination;
        }
    }
    words += (live_chain ? 0u : bit_count(dirty)) + (checkpoints ? (live_chain ? 1u : 3u) : 0u);
    if (block.exit == HL_X86_A64_CONDITIONAL_BRANCH &&
        (request->flags & HL_X86_A64_CONDITIONAL_SELF_LOOP) != 0u && block.target == request->guest_pc)
        words += (live_chain ? 64u : 26u) + constant_words(block.next_pc);
    else if (block.exit == HL_X86_A64_CONDITIONAL_BRANCH)
        words += (block.instructions[block.count - 1u].condition == 0xau ||
                  block.instructions[block.count - 1u].condition == 0xbu ? 5u : 35u) +
                 constant_words(block.target) + constant_words(block.next_pc);
    else
        words += constant_words(block.exit == HL_X86_A64_DIRECT_BRANCH ? block.target : block.next_pc) + 1u;
    if (words > request->host_capacity) return HL_X86_A64_CAPACITY;

    words = 0;
    dirty = 0;
    vector_marked = 0;
    accounted = 0;
    for (index = 0; index < block.count; ++index) {
        const instruction *item = &block.instructions[index];
        hl_x86_a64_provenance *provenance = &request->provenance[index];

        if (checkpoints && may_fallback(item)) {
            if (!live_chain) hl_x86_spill(request->host_words, &words, dirty);
            dirty = 0;
            hl_x86_checkpoint(request->host_words, &words, index - accounted, live_chain);
            accounted = index;
        }
        provenance->guest_pc = item->pc;
        provenance->guest_size = item->length;
        provenance->word_start = words;
        provenance->reserved = 0;
        if (!vector_marked && (request->flags & HL_X86_A64_DIAGNOSTICS) != 0u &&
            vector_register_write(item)) {
            request->host_words[words++] = UINT32_C(0xd2800031); /* mov x17,#1 */
            request->host_words[words++] =
                store_word(17, offsetof(hl_native_x86_64_cpu, vector_dirty));
            vector_marked = 1;
        }
        if (item->operation == OP_MOV_IMMEDIATE) {
            unsigned target = item->width == 2u ? 16u : item->destination;

            emit_constant(request->host_words, &words, target, item->immediate);
            if (item->width == 2u)
                request->host_words[words++] = UINT32_C(0xb3403c00) | 16u << 5 | item->destination;
            dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_MOV_REGISTER) {
            if (item->width == 2u)
                request->host_words[words++] = UINT32_C(0xb3403c00) | (uint32_t)item->source << 5 |
                                               item->destination;
            else
                request->host_words[words++] = (item->width == 8u ? UINT32_C(0xaa0003e0) : UINT32_C(0x2a0003e0)) |
                                               (uint32_t)item->source << 16 | item->destination;
            dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_BSWAP) {
            request->host_words[words++] = (item->width == 8u ? UINT32_C(0xdac00c00) : UINT32_C(0x5ac00800)) |
                                           (uint32_t)item->destination << 5 | item->destination;
            dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_BITSCAN) {
            hl_x86_emit_bitscan(request->host_words, &words, item);
            dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_BIT) {
            hl_x86_emit_bit(request->host_words, &words, item);
            if (item->memory_operand == 0u && item->condition != 0u)
                dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_STRING) {
            hl_x86_emit_string(request->host_words, &words, item);
            dirty |= UINT32_C(1) << 7;
            if (item->condition == 0u) dirty |= UINT32_C(1) << 6;
            if (item->condition == 2u) dirty |= UINT32_C(1);
            if (item->conditional != 0u) dirty |= UINT32_C(1) << 1;
        } else if (item->operation == OP_EXTEND) {
            unsigned source = item->source;
            unsigned source_bytes = item->source_high == 8u ? 1u : item->source_high;
            unsigned target = item->width == 2u ? 16u : item->destination;

            if (item->source_high == 8u) {
                request->host_words[words++] = UINT32_C(0x53083c00) | source << 5 | 16u;
                source = 16u;
            }
            if (item->source_high == 4u && item->signed_extend != 0)
                request->host_words[words++] = UINT32_C(0x93407c00) | source << 5 | target;
            else if (item->source_high == 4u)
                request->host_words[words++] = UINT32_C(0x2a0003e0) | source << 16 | target;
            else if (item->signed_extend != 0)
                request->host_words[words++] = (item->width == 8u ? UINT32_C(0x93400000) : UINT32_C(0x13000000)) |
                                               (source_bytes == 1u ? UINT32_C(0x1c00) : UINT32_C(0x3c00)) |
                                               source << 5 | target;
            else
                request->host_words[words++] = (source_bytes == 1u ? UINT32_C(0x12001c00) :
                                                                      UINT32_C(0x12003c00)) |
                                               source << 5 | target;
            if (item->width == 2u)
                request->host_words[words++] = UINT32_C(0xb3403c00) | 16u << 5 | item->destination;
            dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_BYTE) {
            unsigned source = item->source;

            if (item->has_immediate != 0u) {
                emit_constant(request->host_words, &words, 16, item->immediate);
                source = 16u;
            } else if (item->source_high == 8u) {
                request->host_words[words++] = UINT32_C(0x53083c00) | source << 5 | 16u;
                source = 16u;
            }
            request->host_words[words++] = (item->destination_high != 0 ? UINT32_C(0xb3781c00) :
                                                                            UINT32_C(0xb3401c00)) |
                                           source << 5 | item->destination;
            dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_ALU) {
            hl_x86_emit_alu(request->host_words, &words, item);
            if (!item->flags_only && (item->memory_operand == 0u || item->memory_write == 0u))
                dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_IMUL) {
            hl_x86_emit_imul(request->host_words, &words, item);
            dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_MUL) {
            hl_x86_emit_mul(request->host_words, &words, item);
            dirty |= UINT32_C(1) << 0;
            if (item->width != 1u) dirty |= UINT32_C(1) << 2;
        } else if (item->operation == OP_DIV) {
            hl_x86_emit_div(request->host_words, &words, item);
            dirty |= UINT32_C(1) << 0;
            if (item->width != 1u) dirty |= UINT32_C(1) << 2;
        } else if (item->operation == OP_SHIFT) {
            hl_x86_emit_shift(request->host_words, &words, item);
            dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_DOUBLE_SHIFT) {
            hl_x86_emit_double_shift(request->host_words, &words, item);
            if (item->memory_operand == 0u) dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_ROTATE) {
            hl_x86_emit_rotate(request->host_words, &words, item);
            if (item->memory_operand == 0u) dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_ADDRESS) {
            hl_x86_emit_address(request->host_words, &words, item);
            dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_LOAD) {
            hl_x86_emit_load(request->host_words, &words, item);
            dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_CMOV) {
            hl_x86_emit_cmov(request->host_words, &words, item, item->source);
            dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_SET) {
            hl_x86_emit_set(request->host_words, &words, item);
            if (item->memory_operand == 0u) dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_STORE) {
            hl_x86_emit_store(request->host_words, &words, item);
        } else if (item->operation == OP_XCHG) {
            hl_x86_emit_xchg(request->host_words, &words, item);
            dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_XADD) {
            hl_x86_emit_xadd(request->host_words, &words, item);
            dirty |= UINT32_C(1) << item->source;
            if (item->memory_operand == 0u) dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_CMPXCHG) {
            hl_x86_emit_cmpxchg(request->host_words, &words, item);
            dirty |= UINT32_C(1) << 0;
        } else if (item->operation == OP_VECTOR) {
            hl_x86_emit_vector(request->host_words, &words, item);
            if (item->vector_kind == VECTOR_STRING_EQUAL_EACH)
                dirty |= UINT32_C(1) << 1;
            else if (item->vector_kind == VECTOR_TRUNC_DOUBLE_SIGNED)
                dirty |= UINT32_C(1) << item->destination;
            else if (item->memory_operand == 0u &&
                (item->vector_kind == VECTOR_TO_INTEGER || item->vector_kind == VECTOR_BYTE_MASK))
                dirty |= UINT32_C(1) << item->destination;
        } else if (item->operation == OP_ACCUMULATOR) {
            hl_x86_emit_accumulator(request->host_words, &words, item);
            dirty |= UINT32_C(1) << 0;
            if (item->source_high != 0u) dirty |= UINT32_C(1) << 2;
        } else if (item->operation == OP_CALL || item->operation == OP_JUMP ||
                   item->operation == OP_RETURN || item->operation == OP_LEAVE ||
                   item->operation == OP_PUSH || item->operation == OP_POP) {
            hl_x86_emit_control(request->host_words, &words, item);
            if (item->operation == OP_LEAVE)
                dirty |= UINT32_C(1) << 4 | UINT32_C(1) << 5;
            if (item->operation == OP_PUSH) dirty |= UINT32_C(1) << 4;
            if (item->operation == OP_POP)
                dirty |= UINT32_C(1) << 4 | UINT32_C(1) << item->destination;
        }
        provenance->word_end = words;
    }
    if (!live_chain) hl_x86_spill(request->host_words, &words, dirty);
    if (checkpoints) {
        hl_x86_checkpoint(request->host_words, &words, block.count - accounted, live_chain);
    }
    hl_x86_emit_exit(request, &block, &words);

    return hl_x86_result(request, &block, result, words);
}
