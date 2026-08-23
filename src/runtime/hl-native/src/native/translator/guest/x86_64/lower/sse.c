#include "sse.h"

#include "primitives.h"
#include "../cpu.h"
#include "../encoding.h"
#include "../glue.h"

static uint32_t sse_register_field(int register_index, unsigned shift) {
    return (uint32_t)register_index << shift;
}

static uint32_t sse_packed_binary_opcode(uint8_t opcode) {
    switch (opcode) {
    case 0xEF: return 0x6E201C00u; // PXOR
    case 0xDB: return 0x4E201C00u; // PAND
    case 0xEB: return 0x4EA01C00u; // POR
    case 0xDF: return 0x4E601C00u; // PANDN (operands reversed below)
    case 0x74: return 0x6E208C00u; // PCMPEQB
    case 0x75: return 0x6E608C00u; // PCMPEQW
    case 0x76: return 0x6EA08C00u; // PCMPEQD
    case 0x64: return 0x4E203400u; // PCMPGTB
    case 0x65: return 0x4E603400u; // PCMPGTW
    case 0x66: return 0x4EA03400u; // PCMPGTD
    case 0xDE: return 0x6E206400u; // PMAXUB
    case 0xDA: return 0x6E206C00u; // PMINUB
    case 0xEE: return 0x4E606400u; // PMAXSW
    case 0xEA: return 0x4E606C00u; // PMINSW
    case 0xFC: return 0x4E208400u; // PADDB
    case 0xFD: return 0x4E608400u; // PADDW
    case 0xFE: return 0x4EA08400u; // PADDD
    case 0xD4: return 0x4EE08400u; // PADDQ
    case 0xF8: return 0x6E208400u; // PSUBB
    case 0xF9: return 0x6E608400u; // PSUBW
    case 0xFA: return 0x6EA08400u; // PSUBD
    case 0xFB: return 0x6EE08400u; // PSUBQ
    case 0xDC: return 0x6E200C00u; // PADDUSB
    case 0xDD: return 0x6E600C00u; // PADDUSW
    case 0xEC: return 0x4E200C00u; // PADDSB
    case 0xED: return 0x4E600C00u; // PADDSW
    case 0xD8: return 0x6E202C00u; // PSUBUSB
    case 0xD9: return 0x6E602C00u; // PSUBUSW
    case 0xE8: return 0x4E202C00u; // PSUBSB
    case 0xE9: return 0x4E602C00u; // PSUBSW
    case 0xE0: return 0x6E201400u; // PAVGB
    case 0xE3: return 0x6E601400u; // PAVGW
    case 0xD5: return 0x4E609C00u; // PMULLW
    default: return 0;
    }
}

int lower_sse_packed_binary(struct insn *instruction, uint64_t next, int vd, int vm, int mmx) {
    uint32_t encoding = sse_packed_binary_opcode(instruction->op);
    if (!encoding) return TX_FALL;
    int source = instruction->is_mem ? 16 : vm;
    if (instruction->is_mem) g_ldr_vec_ea(16, instruction, next, mmx);
    if (instruction->op == 0xDF)
        e_v3(encoding, vd, source, vd);
    else
        e_v3(encoding, vd, vd, source);
    return TX_NEXT;
}

int lower_sse_widening_multiply(struct insn *instruction, uint64_t next, int vd, int vm, int mmx) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xE5 && opcode != 0xE4 && opcode != 0xF5) return TX_FALL;
    int source = instruction->is_mem ? 16 : vm;
    if (instruction->is_mem) g_ldr_vec_ea(16, instruction, next, mmx);
    if (opcode == 0xF5) { // PMADDWD: signed products followed by adjacent pair addition.
        emit32(0x0E60C000u | sse_register_field(source, 16) | sse_register_field(vd, 5) | 18u);
        int high = mmx ? 18 : 19;
        if (!mmx) emit32(0x4E60C000u | sse_register_field(source, 16) | sse_register_field(vd, 5) | 19u);
        emit32(0x4EA0BC00u | sse_register_field(high, 16) | sse_register_field(18, 5) | sse_register_field(vd, 0));
        return TX_NEXT;
    }
    // PMULHW/PMULHUW widen each half independently; UZP2 selects the upper
    // word of each 32-bit product. MMX has only the low four input words.
    uint32_t low = opcode == 0xE5 ? 0x0E60C000u : 0x2E60C000u;
    uint32_t high = opcode == 0xE5 ? 0x4E60C000u : 0x6E60C000u;
    emit32(low | sse_register_field(source, 16) | sse_register_field(vd, 5) | 18u);
    if (mmx) {
        emit32(0x0F108400u | sse_register_field(18, 5) | sse_register_field(vd, 0));
    } else {
        emit32(high | sse_register_field(source, 16) | sse_register_field(vd, 5) | 19u);
        emit32(0x4E405800u | sse_register_field(19, 16) | sse_register_field(18, 5) | sse_register_field(vd, 0));
    }
    return TX_NEXT;
}

int lower_sse_shuffle(struct insn *instruction, uint64_t next, int vd, int vm, int mmx) {
    if (instruction->op != 0x70) return TX_FALL;
    int source = instruction->is_mem ? 16 : vm;
    if (instruction->is_mem) g_ldr_vec_ea(16, instruction, next, mmx);
    unsigned immediate = (unsigned)instruction->imm & 0xff;
    if (instruction->p66) {
        if (immediate == 0xE4) {
            if (vd != source) e_vmov(vd, source);
        } else if (immediate == 0x4E) {
            e_ext(vd, source, source, 8);
        } else if (immediate == 0xB1) {
            emit32(0x4EA00800u | sse_register_field(source, 5) | sse_register_field(vd, 0));
        } else if (immediate == 0x00 || immediate == 0x55 || immediate == 0xAA || immediate == 0xFF) {
            hl_x86_emit_vector_broadcast32(vd, source, (int)(immediate & 3));
        } else {
            int output = vd == source ? 17 : vd;
            for (int lane = 0; lane < 4; lane++)
                e_ins_s(output, lane, source, (immediate >> (2 * lane)) & 3);
            if (output != vd) e_vmov(vd, output);
        }
        return TX_NEXT;
    }
    if (instruction->rep || instruction->repne) {
        int high = instruction->rep;
        e_vmov(17, source);
        for (int lane = 0; lane < 4; lane++) {
            int destination_lane = high ? 4 + lane : lane;
            int source_lane = (high ? 4 : 0) + (int)((immediate >> (2 * lane)) & 3);
            emit32(0x6E000400u | ((((unsigned)destination_lane << 2) | 2u) << 16) |
                   (((unsigned)source_lane << 1) << 11) | sse_register_field(source, 5) | 17u);
        }
        e_vmov(vd, 17);
        return TX_NEXT;
    }
    return TX_FALL;
}

int lower_sse_sign_mask(struct insn *instruction, int vm, int mmx) {
    if (instruction->op == 0x50) {
        if (instruction->p66) {
            e_vshr_imm(17, vm, 64, 63, 0);
            emit32(0x4E003C00u | ((0u * 16 + 8) << 16) | sse_register_field(17, 5) |
                   sse_register_field(instruction->reg, 0));
            emit32(0x4E003C00u | ((1u * 16 + 8) << 16) | (17 << 5) | 19);
            e_rrr(A_ORR, instruction->reg, instruction->reg, 19, 1, 1);
        } else {
            e_vshr_imm(17, vm, 32, 31, 0);
            emit32(0x0E003C00u | ((0u * 8 + 4) << 16) | sse_register_field(17, 5) |
                   sse_register_field(instruction->reg, 0));
            for (int lane = 1; lane < 4; lane++) {
                emit32(0x0E003C00u | (((unsigned)lane * 8 + 4) << 16) | (17 << 5) | 19);
                e_rrr(A_ORR, instruction->reg, instruction->reg, 19, 0, lane);
            }
        }
        return TX_NEXT;
    }
    if (instruction->op != 0xD7) return TX_FALL;
    int source = vm;
    if (mmx) {
        e_vmov8(18, vm);
        source = 18;
    }
    g_pmovmskb_n++;
    e_vshr_imm(17, source, 8, 7, 0);
    emit32(0x6F001400u | (25u << 16) | (17 << 5) | 17);
    emit32(0x6F001400u | (50u << 16) | (17 << 5) | 17);
    emit32(0x6F001400u | (100u << 16) | (17 << 5) | 17);
    emit32(0x0E003C00u | (1u << 16) | (17 << 5) | 16);
    emit32(0x0E003C00u | (17u << 16) | sse_register_field(17, 5) | sse_register_field(instruction->reg, 0));
    e_rrr(A_ORR, instruction->reg, 16, instruction->reg, 0, 8);
    return TX_NEXT;
}

int lower_mmx_fp_conversion(struct insn *instruction, uint64_t next, int vd, int vm) {
    uint8_t opcode = instruction->op;
    if ((opcode != 0x2A && opcode != 0x2C && opcode != 0x2D) || instruction->rep || instruction->repne) return TX_FALL;
    int truncate = opcode == 0x2C;
    if (opcode == 0x2A) {
        int source = vm & 7;
        if (instruction->is_mem) {
            g_ldr_d_ea(16, instruction, next);
            source = 16;
        }
        if (instruction->p66) {
            emit32(0x0F20A400u | sse_register_field(source, 5) | 18u);
            emit32(0x4E61D800u | (18 << 5) | 18);
            e_vmov(vd, 18);
        } else {
            emit32(0x0E21D800u | sse_register_field(source, 5) | 18u);
            e_ins_d(vd, 0, 18, 0);
        }
        return TX_NEXT;
    }
    if (instruction->p66) {
        int source = vm;
        if (instruction->is_mem) {
            g_ldr_q_ea(16, instruction, next);
            source = 16;
        }
        e_movconst(16, 0x41E0000000000000ull);
        emit32(0x4E080C00u | (16 << 5) | 19);
        e_movconst(16, 0xC1E0000000000000ull);
        emit32(0x4E080C00u | (16 << 5) | 20);
        emit_pd2i32_pieces(24, 22, source, truncate, 19, 20, 23, 21);
        emit32(0x0EA12800u | (24 << 5) | 24);
        emit32(0x0EA12800u | (22 << 5) | 22);
        e_movconst(16, 0x80000000ull);
        emit32(0x0E040C00u | (16 << 5) | 18);
        emit32(0x2E601C00u | (24 << 16) | (18 << 5) | 22);
        e_vmov8(vd & 7, 22);
        return TX_NEXT;
    }
    int source = vm;
    if (instruction->is_mem) {
        g_ldr_d_ea(16, instruction, next);
        source = 16;
    }
    if (truncate) {
        emit32(0x0EA1B800u | sse_register_field(source, 5) | 21u);
    } else {
        emit32(0x2E219800u | sse_register_field(source, 5) | 21u);
        emit32(0x0EA1B800u | (21 << 5) | 21);
    }
    e_movconst(16, 0x4F000000ull);
    emit32(0x0E040C00u | (16 << 5) | 17);
    emit32(0x2E20E400u | sse_register_field(17, 16) | sse_register_field(source, 5) | 19u);
    emit32(0x0E20E400u | sse_register_field(source, 16) | sse_register_field(source, 5) | 20u);
    emit32(0x2E205800u | (20 << 5) | 20);
    e_v3(0x0EA01C00u, 19, 19, 20);
    e_movconst(16, 0x80000000ull);
    emit32(0x0E040C00u | (16 << 5) | 18);
    emit32(0x2E601C00u | (21 << 16) | (18 << 5) | 19);
    e_vmov8(vd & 7, 19);
    return TX_NEXT;
}

int lower_sse_packed_shift(struct insn *instruction, uint64_t guest_pc, uint64_t next, int vd, int vm, int mmx,
                           int *writeback, hl_x86_crypto_state *crypto_state) {
    uint8_t opcode = instruction->op;
    if (opcode == 0x71 || opcode == 0x72 || opcode == 0x73) {
        int operation = instruction->reg & 7;
        int element_bits = opcode == 0x71 ? 16 : opcode == 0x72 ? 32 : 64;
        int shift = (int)(instruction->imm & 0xff);
        int destination = vm;
        *writeback = mmx ? destination : -1;
        if (operation == 2)
            e_vshr_imm(destination, destination, element_bits, shift, 0);
        else if (operation == 4)
            e_vshr_imm(destination, destination, element_bits, shift, 1);
        else if (operation == 6)
            e_vshl_imm(destination, destination, element_bits, shift);
        else if (opcode == 0x73 && (operation == 3 || operation == 7) && !mmx) {
            if (shift > 15) {
                e_v3(0x6E201C00u, destination, destination, destination);
            } else if (shift) {
                if (!crypto_state->zero_ready) e_v3(0x6E201C00u, 26, 26, 26);
                crypto_state->zero_ready = 1;
                if (operation == 3)
                    e_ext(destination, destination, 26, shift);
                else
                    e_ext(destination, 26, destination, 16 - shift);
            }
        } else {
            report_unimpl(guest_pc, instruction);
            return TX_BREAK;
        }
        return TX_NEXT;
    }
    if (opcode != 0xF1 && opcode != 0xF2 && opcode != 0xF3 && opcode != 0xD1 && opcode != 0xD2 && opcode != 0xD3 &&
        opcode != 0xE1 && opcode != 0xE2)
        return TX_FALL;
    int source = instruction->is_mem ? 16 : vm;
    if (instruction->is_mem) g_ldr_vec_ea(16, instruction, next, mmx);
    int left = opcode == 0xF1 || opcode == 0xF2 || opcode == 0xF3;
    int arithmetic = opcode == 0xE1 || opcode == 0xE2;
    int element_bits = (opcode == 0xF1 || opcode == 0xD1 || opcode == 0xE1)   ? 16
                       : (opcode == 0xF2 || opcode == 0xD2 || opcode == 0xE2) ? 32
                                                                              : 64;
    e_sse_var_shift(vd, vd, source, element_bits, left, arithmetic);
    return TX_NEXT;
}

int lower_sse_packed_conversion(struct insn I, uint64_t next, int vd, int vm) {
    if (I.op != 0x5B) return TX_FALL;
    int s = vm;
    if (I.is_mem) {
        g_ldr_q_ea(16, &I, next);
        s = 16;
    }
    if (I.rep || I.p66) {
        // Same emit as the VEX form: 66 cvtps2dq used to emit FCVTNS unconditionally and so
        // ignored MXCSR.RC, while vcvtps2dq honoured it. emit_ps2dq_128 builds the
        // "make-indefinite" mask from the SOURCE floats before converting, which the
        // in-place `cvttps2dq %xmm7,%xmm7` form requires: reading it back from the integer
        // result would see an all-ones lane (-1) as a NaN, and the indefinite (== -0.0f) as
        // ordered.
        e_movconst(19, 0x4F000000u);
        emit32(0x4E040C00u | (19 << 5) | 25); // v25.4s = 2^31 (f32)
        e_movconst(19, 0x80000000u);
        emit32(0x4E040C00u | (19 << 5) | 26); // v26.4s = integer indefinite
        emit_ps2dq_128(17, s, I.rep != 0, 25, 26, 27, 28);
        e_vmov(vd, 17);
    } else {
        emit32(0x4E21D800u | sse_register_field(s, 5) |
               sse_register_field(vd, 0)); // NP: cvtdq2ps -> SCVTF .4S (s32->f32)
    }
    return TX_NEXT;
}

int lower_sse_nontemporal_store(struct insn *instruction, uint64_t guest_pc, uint64_t next, int vd, int vm) {
    uint8_t opcode = instruction->op;
    if (opcode == 0xE7 && instruction->p66) { // movntdq: non-temporal store xmm -> m128
        g_str_q_ea(vd, instruction, next);
        return TX_NEXT;
    }
    if (opcode == 0x2B && instruction->is_mem) { // movntps/movntpd: non-temporal store xmm -> m128
        g_str_q_ea(vd, instruction, next);
        return TX_NEXT;
    }
    if (opcode != 0xF7 || !instruction->p66) return TX_FALL;

    // maskmovdqu: each mask byte's MSB selects a byte from vd for the implicit [RDI]
    // destination. Blend with the existing bytes so unselected locations remain unchanged.
    e_vshr_imm(18, vm, 8, 7, 1);
    e_mov_rr(17, RDI, 1);
    emit_memory_guard(17, 16, guest_pc, X86_SOFT_READ | X86_SOFT_WRITE);
    g_ldr_q(16, 17, 0);
    e_v3(0x6E601C00u, 18, vd, 16);
    g_str_q(18, 17, 0);
    if (emit_soft_memory_active()) emit_soft_store_commit(16);
    return TX_NEXT;
}

int lower_sse_word_lane(struct insn *instruction, uint64_t guest_pc, uint64_t next, int vd, int vm, int mmx,
                        int *mmx_writeback) {
    if (instruction->op != 0xC4 && instruction->op != 0xC5) return TX_FALL;
    int lane = (int)instruction->imm & (mmx ? 3 : 7);
    if (instruction->op == 0xC5) { // pextrw: extract H lane to r32, zero-extended
        *mmx_writeback = -1;
        emit32(0x0E003C00u | ((((unsigned)lane << 2) | 2u) << 16) | sse_register_field(vm, 5) |
               sse_register_field(instruction->reg, 0));
        return TX_NEXT;
    }

    // pinsrw: insert the low 16 bits of r/m16 into the selected H lane.
    int source;
    if (instruction->is_mem) {
        emit_ea(instruction, next);
        if (emit_soft_memory_active()) emit_memory_guard(17, 2, guest_pc, X86_SOFT_READ);
        e_load(2, 16, 17);
        source = 16;
    } else {
        source = instruction->rm_reg;
    }
    emit32(0x4E001C00u | ((((unsigned)lane << 2) | 2u) << 16) | sse_register_field(source, 5) |
           sse_register_field(vd, 0));
    return TX_NEXT;
}

int lower_sse_widening_integer(struct insn *instruction, uint64_t next, int vd, int vm, int mmx) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xF4 && opcode != 0xF6) return TX_FALL;
    int source = instruction->is_mem ? 16 : vm;
    if (instruction->is_mem) g_ldr_vec_ea(16, instruction, next, mmx);
    if (opcode == 0xF4) { // pmuludq: multiply even unsigned 32-bit lanes into 64-bit products
        emit32(0x4E801800u | sse_register_field(vd, 16) | sse_register_field(vd, 5) | 17u);
        emit32(0x4E801800u | sse_register_field(source, 16) | sse_register_field(source, 5) | 18u);
        emit32(0x2EA0C000u | sse_register_field(18, 16) | sse_register_field(17, 5) | sse_register_field(vd, 0));
        return TX_NEXT;
    }

    // psadbw: widen and pairwise-add absolute byte differences into each 64-bit half.
    emit32(0x6E207400u | sse_register_field(source, 16) | sse_register_field(vd, 5) | 17u);
    emit32(0x6E202800u | (17 << 5) | 17);
    emit32(0x6E602800u | (17 << 5) | 17);
    emit32(0x6EA02800u | (17 << 5) | 17);
    e_vmov(vd, 17);
    return TX_NEXT;
}

int lower_sse_saturating_pack(struct insn *instruction, uint64_t next, int vd, int vm, int mmx) {
    uint8_t opcode = instruction->op;
    if (opcode != 0x67 && opcode != 0x63 && opcode != 0x6B) return TX_FALL;
    int source = instruction->is_mem ? 16 : vm;
    if (instruction->is_mem) g_ldr_vec_ea(16, instruction, next, mmx);
    uint32_t element_size = opcode == 0x6B ? 1u : 0u;
    uint32_t narrow_low = opcode == 0x67 ? 0x2E212800u : 0x0E214800u;
    uint32_t narrow_high = opcode == 0x67 ? 0x6E212800u : 0x4E214800u;
    if (mmx) {
        // Concatenate both 64-bit operands before narrowing; MMX has no architectural high lanes.
        emit32(0x4EC03800u | sse_register_field(source, 16) | sse_register_field(vd, 5) | 17u);
        emit32(narrow_low | ((uint32_t)element_size << 22) | sse_register_field(17, 5) | sse_register_field(vd, 0));
    } else {
        emit32(narrow_low | ((uint32_t)element_size << 22) | sse_register_field(vd, 5) | 17u);
        emit32(narrow_high | ((uint32_t)element_size << 22) | sse_register_field(source, 5) | 17u);
        e_vmov(vd, 17);
    }
    return TX_NEXT;
}

int lower_sse_unpack(struct insn *instruction, uint64_t next, int vd, int vm, int mmx) {
    uint8_t opcode = instruction->op;
    if (opcode != 0x60 && opcode != 0x61 && opcode != 0x62 && opcode != 0x6C && opcode != 0x68 && opcode != 0x69 &&
        opcode != 0x6A && opcode != 0x6D)
        return TX_FALL;
    int source = instruction->is_mem ? 16 : vm;
    if (instruction->is_mem) g_ldr_vec_ea(16, instruction, next, mmx);
    int high = opcode == 0x68 || opcode == 0x69 || opcode == 0x6A || opcode == 0x6D;
    int element_size = (opcode == 0x60 || opcode == 0x68)   ? 0
                       : (opcode == 0x61 || opcode == 0x69) ? 1
                       : (opcode == 0x62 || opcode == 0x6A) ? 2
                                                            : 3;
    if (mmx && element_size == 3) return TX_FALL; // 0F 6C/6D have no MMX form.
    uint32_t encoding = (high ? 0x4E007800u : 0x4E003800u) | ((uint32_t)element_size << 22);
    // MMX operates on 64-bit halves, so Q=0 selects the architectural lanes for both ZIP variants.
    if (mmx) encoding &= ~0x40000000u;
    e_v3(encoding, vd, vd, source);
    return TX_NEXT;
}

int lower_sse_float_unpack(struct insn *instruction, uint64_t next, int vd, int vm, int mmx) {
    if (instruction->op != 0x14 && instruction->op != 0x15) return TX_FALL;
    int source = instruction->is_mem ? 16 : vm;
    if (instruction->is_mem) g_ldr_vec_ea(16, instruction, next, mmx);
    int high = instruction->op == 0x15;
    int element_size = instruction->p66 ? 3 : 2;
    uint32_t encoding = (high ? 0x4E007800u : 0x4E003800u) | ((uint32_t)element_size << 22);
    e_v3(encoding, vd, vd, source);
    return TX_NEXT;
}

int lower_sse_two_source_shuffle(struct insn *instruction, uint64_t next, int vd, int vm, int mmx) {
    if (instruction->op != 0xC6) return TX_FALL;
    int source = instruction->is_mem ? 16 : vm;
    if (instruction->is_mem) g_ldr_vec_ea(16, instruction, next, mmx);
    unsigned immediate = (unsigned)instruction->imm;
    e_vmov(18, vd);
    if (instruction->p66) {
        e_ins_d(17, 0, 18, immediate & 1);
        e_ins_d(17, 1, source, (immediate >> 1) & 1);
    } else {
        e_ins_s(17, 0, 18, immediate & 3);
        e_ins_s(17, 1, 18, (immediate >> 2) & 3);
        e_ins_s(17, 2, source, (immediate >> 4) & 3);
        e_ins_s(17, 3, source, (immediate >> 6) & 3);
    }
    e_vmov(vd, 17);
    return TX_NEXT;
}

int lower_sse_move_lane(struct insn *instruction, uint64_t next, int vd, int vm) {
    uint8_t opcode = instruction->op;
    if (opcode != 0x12 && opcode != 0x13 && opcode != 0x16 && opcode != 0x17) return TX_FALL;
    if ((opcode == 0x12 || opcode == 0x16) && instruction->rep) {
        int source = vm;
        if (instruction->is_mem) {
            g_ldr_q_ea(16, instruction, next);
            source = 16;
        }
        e_v3(opcode == 0x12 ? 0x4E802800u : 0x4E806800u, vd, source, source);
        return TX_NEXT;
    }
    if (opcode == 0x12 && instruction->repne) {
        int source = vm;
        if (instruction->is_mem) {
            g_ldr_d_ea(16, instruction, next);
            source = 16;
        }
        emit32(0x4E080400u | sse_register_field(source, 5) | sse_register_field(vd, 0));
        return TX_NEXT;
    }
    if (opcode == 0x12 || opcode == 0x16) {
        int lane = opcode == 0x16 ? 1 : 0;
        if (instruction->is_mem) {
            g_ldr_d_ea(16, instruction, next);
            e_ins_d(vd, lane, 16, 0);
        } else {
            e_ins_d(vd, lane, vm, opcode == 0x12 ? 1 : 0);
        }
        return TX_NEXT;
    }
    int lane = opcode == 0x17 ? 1 : 0;
    e_ins_d(16, 0, vd, lane);
    g_str_d_ea(16, instruction, next);
    return TX_NEXT;
}

int lower_sse_bitwise(struct insn *instruction, uint64_t next, int vd, int vm, int mmx) {
    uint8_t opcode = instruction->op;
    if (opcode != 0x54 && opcode != 0x55 && opcode != 0x56 && opcode != 0x57) return TX_FALL;
    int source = instruction->is_mem ? 16 : vm;
    if (instruction->is_mem) g_ldr_vec_ea(16, instruction, next, mmx);
    if (opcode == 0x54)
        e_v3(0x4E201C00u, vd, vd, source);
    else if (opcode == 0x55)
        e_v3(0x4E601C00u, vd, source, vd);
    else if (opcode == 0x56)
        e_v3(0x4EA01C00u, vd, vd, source);
    else
        e_v3(0x6E201C00u, vd, vd, source);
    return TX_NEXT;
}

int lower_sse_packed_double_integer(struct insn *instruction, uint64_t next, int vd, int vm) {
    if (instruction->op != 0xE6 || (!instruction->rep && !instruction->p66 && !instruction->repne)) return TX_FALL;
    int source = vm;
    if (instruction->rep) { // cvtdq2pd: low two packed s32 -> two packed f64
        if (instruction->is_mem) {
            g_ldr_d_ea(16, instruction, next);
            source = 16;
        }
        emit32(0x0F20A400u | sse_register_field(source, 5) | 16u);
        emit32(0x4E61D800u | sse_register_field(16, 5) | sse_register_field(vd, 0));
        return TX_NEXT;
    }
    if (instruction->is_mem) {
        g_ldr_q_ea(16, instruction, next);
        source = 16;
    }
    int truncate = instruction->p66 != 0;
    e_movconst(19, 0x41E0000000000000ull);
    emit32(0x4E080C00u | (19 << 5) | 25);
    e_movconst(19, 0xC1E0000000000000ull);
    emit32(0x4E080C00u | (19 << 5) | 26);
    e_movconst(19, 0x80000000ull);
    emit32(0x0E040C00u | (19 << 5) | 27);
    emit_pd2i32_pieces(22, 18, source, truncate, 25, 26, 28, 21);
    emit32(0x0EA12800u | (22 << 5) | 24);
    emit32(0x0EA12800u | (18 << 5) | 25);
    emit32(0x2E601C00u | (24 << 16) | (27 << 5) | 25);
    e_vmov(vd, 25);
    return TX_NEXT;
}

int lower_sse_flag_compare(struct insn I, uint64_t guest_pc, uint64_t next, int vd, int vm) {
    uint8_t op = I.op;
    if (op != 0x2E && op != 0x2F) return TX_FALL;
    int s = vm;
    if (I.is_mem) {
        emit_ea(&I, next);
        if (emit_soft_memory_active()) emit_memory_guard(17, I.p66 ? 8u : 4u, guest_pc, X86_SOFT_READ);
        if (I.p66)
            g_ldr_d(16, 17);
        else
            g_ldr_s(16, 17);
        s = 16;
    }
    // COMISS/COMISD (0x2F) is the SIGNALING ordered compare: it raises Invalid (IE)
    // on ANY NaN operand, including qNaN. UCOMISS/UCOMISD (0x2E) is quiet: IE only for
    // sNaN. Map 0x2F -> FCMPE (bit4 set) and 0x2E -> FCMP. EFLAGS result is identical
    // for both (unordered -> N0 Z0 C1 V1), so the fixup below is unchanged.
    emit32((I.p66 ? 0x1E602000u : 0x1E202000u) | (op == 0x2F ? 0x10u : 0u) | sse_register_field(s, 16) |
           sse_register_field(vd, 5)); // FCMP/FCMPE Dvd, Ds  (Rd=0)
    e_nzcv_save_fcmp();                // unordered fixup: x86 ZF=PF=CF=1, SF=0 (ARM FCMP gives N0 Z0 C1 V1)
    return TX_NEXT;
}

int lower_sse_compare(struct insn I, uint64_t guest_pc, uint64_t next, int vd, int vm) {
    if (I.op != 0xC2) return TX_FALL;
    int packed = !I.repne && !I.rep;
    int s = vm;
    if (I.is_mem) {
        if (packed) {
            g_ldr_q_ea(16, &I, next);
        } else {
            emit_ea(&I, next);
            if (emit_soft_memory_active()) emit_memory_guard(17, I.repne ? 8u : 4u, guest_pc, X86_SOFT_READ);
            if (I.repne)
                g_ldr_d(16, 17);
            else
                g_ldr_s(16, 17);
        }
        s = 16;
    }
    int pred = (int)I.imm & 7;
    // sz bit (bit22): packed 66 / scalar F2 -> double, else single
    uint32_t szb = (packed ? I.p66 : I.repne) ? 0x00400000u : 0;
    uint32_t EQ = (packed ? 0x4E20E400u : 0x5E20E400u) | szb; // FCMEQ
    uint32_t GE = (packed ? 0x6E20E400u : 0x7E20E400u) | szb; // FCMGE
    uint32_t GT = (packed ? 0x6EA0E400u : 0x7EA0E400u) | szb; // FCMGT
    uint32_t ANDb = packed ? 0x4E201C00u : 0x0E201C00u;       // AND Vd.16b/8b
    uint32_t NOTb = packed ? 0x6E205800u : 0x2E205800u;       // NOT (MVN) Vd.16b/8b
    // CMPSS/CMPSD write ONLY the low element and preserve the rest of the
    // destination, but the ARM scalar FCMxx/NOT forms zero everything above the
    // element. So scalar results are built in v18 and inserted back into lane 0.
    int res = packed ? vd : 18;
    if (pred == 3 || pred == 7) {                                                  // UNORD/ORD: ordered(a)&ordered(b)
        emit32(EQ | sse_register_field(vd, 16) | sse_register_field(vd, 5) | 17u); // v17 = a==a
        emit32(EQ | sse_register_field(s, 16) | sse_register_field(s, 5) | sse_register_field(res, 0)); // res = b==b
        emit32(ANDb | sse_register_field(17, 16) | sse_register_field(res, 5) |
               sse_register_field(res, 0));                                                    // res = ORD
        if (pred == 3) emit32(NOTb | sse_register_field(res, 5) | sse_register_field(res, 0)); // UNORD = ~ORD
    } else {
        // predicates handled here: 0 EQ, 1 LT, 2 LE, 4 NEQ, 5 NLT, 6 NLE.
        // LT/LE/NLT/NLE build the ordered comparison a<b / a<=b via the swapped GT/GE (a<b ==
        // b>a); NEQ/NLT/NLE then invert. x86's N-forms are UNORDERED: they return all-ones when
        // an operand is NaN. ARM FCMGT/FCMGE give 0 on NaN, so inverting the ordered result (NOT)
        // yields the correct NaN->true mask for NLT/NLE (H12) exactly as it already did for NEQ.
        int lt_like = (pred == 1 || pred == 2 || pred == 5 || pred == 6);
        int use_ge = (pred == 2 || pred == 6);           // LE/NLE -> GE ; LT/NLT -> GT
        int neg = (pred == 4 || pred == 5 || pred == 6); // NEQ/NLT/NLE invert (NaN -> true)
        int n = lt_like ? s : vd, m = lt_like ? vd : s;
        uint32_t fc = (pred == 0 || pred == 4) ? EQ : use_ge ? GE : GT;
        emit32(fc | sse_register_field(m, 16) | sse_register_field(n, 5) |
               sse_register_field(res, 0)); // FCMxx res, n, m
        if (neg)
            emit32(NOTb | sse_register_field(res, 5) |
                   sse_register_field(res, 0)); // invert -> NaN lane becomes all-ones
    }
    if (!packed) { // merge the scalar lane back
        if (I.repne)
            e_ins_d(vd, 0, res, 0); // cmpsd: bits 63:0 only
        else
            e_ins_s(vd, 0, res, 0); // cmpss: bits 31:0 only
    }
    return TX_NEXT;
}

int lower_sse_scalar_to_integer(struct insn *instruction, uint64_t guest_pc, uint64_t next, int vm) {
    uint8_t opcode = instruction->op;
    if (opcode != 0x2C && opcode != 0x2D) return TX_FALL;
    int source = vm;
    if (instruction->is_mem) {
        emit_ea(instruction, next);
        if (emit_soft_memory_active()) emit_memory_guard(17, instruction->repne ? 8u : 4u, guest_pc, X86_SOFT_READ);
        if (instruction->repne)
            g_ldr_d(16, 17);
        else
            g_ldr_s(16, 17);
        source = 16;
    }
    int original_source = source;
    if (opcode == 0x2D) {
        uint32_t round_integral = instruction->repne ? 0x1E67C000u : 0x1E27C000u;
        emit32(round_integral | sse_register_field(source, 5) | 18u);
        source = 18;
    }
    emit32(0x1E380000u | (instruction->rexW ? 0x80000000u : 0) | (instruction->repne ? 0x00400000u : 0) |
           sse_register_field(source, 5) | sse_register_field(instruction->reg, 0));

    // x86 returns integer-indefinite for positive overflow and NaN while ARM saturates.
    int sf = instruction->rexW ? 1 : 0;
    uint64_t threshold = instruction->repne ? (sf ? 0x43E0000000000000ull : 0x41E0000000000000ull)
                                            : (sf ? 0x5F000000ull : 0x4F000000ull);
    e_movconst(20, threshold);
    if (instruction->repne)
        e_fmov_to_d(19, 20);
    else
        e_fmov_to_s(19, 20);
    uint32_t compare = instruction->repne ? 0x1E602000u : 0x1E202000u;
    emit32(0xD53B4200u | 21);
    emit32(compare | sse_register_field(19, 16) | sse_register_field(source, 5));
    if (opcode == 0x2D) {
        // Raise precision only for in-range inexact rounding. Mask both overflow directions
        // before FRINTX so out-of-range inputs retain x86's invalid-only exception behavior.
        emit32(0xDA9F33E0u | 22);
        emit32((instruction->repne ? 0x1E614000u : 0x1E214000u) | (19 << 5) | 20);
        emit32(compare | sse_register_field(20, 16) | sse_register_field(source, 5));
        emit32(0xDA9F53E0u | 23);
        e_rrr(A_ORR, 22, 22, 23, 1, 0);
        e_fmov_to_d(20, 22);
        e_v3(0x0E601C00u, 20, original_source, 20);
        emit32((instruction->repne ? 0x1E674000u : 0x1E274000u) | (20 << 5) | 20);
        emit32(compare | sse_register_field(19, 16) | sse_register_field(source, 5));
    }
    e_movconst(20, sf ? 0x8000000000000000ull : 0x80000000ull);
    e_csel(instruction->reg, 20, instruction->reg, 2, sf);
    emit32(0xD51B4200u | 21);
    return TX_NEXT;
}

int lower_sse_integer_to_scalar(struct insn *instruction, uint64_t guest_pc, uint64_t next, int vd) {
    if (instruction->op != 0x2A) return TX_FALL;
    int source;
    if (instruction->is_mem) {
        emit_ea(instruction, next);
        if (emit_soft_memory_active()) emit_memory_guard(17, instruction->rexW ? 8u : 4u, guest_pc, X86_SOFT_READ);
        e_load(instruction->rexW ? 8 : 4, 16, 17);
        source = 16;
    } else {
        source = instruction->rm_reg;
    }
    // Convert into scratch and merge lane zero: CVTSI2SS/SD preserve all upper destination bits.
    emit32(0x1E220000u | (instruction->rexW ? 0x80000000u : 0) | (instruction->repne ? 0x00400000u : 0) |
           sse_register_field(source, 5) | 18u);
    if (instruction->repne)
        e_ins_d(vd, 0, 18, 0);
    else
        e_ins_s(vd, 0, 18, 0);
    return TX_NEXT;
}

int lower_sse_minmax(struct insn *instruction, uint64_t guest_pc, uint64_t next, int vd, int vm) {
    uint8_t opcode = instruction->op;
    if (opcode != 0x5D && opcode != 0x5F) return TX_FALL;
    // x86 selects the r/m source for NaN and equal operands, including opposite signed zero.
    // ARM FMIN/FMAX differ there, so use an ordered greater-than mask and a byte select.
    int packed = !instruction->repne && !instruction->rep;
    int source = vm;
    if (instruction->is_mem) {
        if (packed) {
            g_ldr_q_ea(16, instruction, next);
        } else {
            emit_ea(instruction, next);
            if (emit_soft_memory_active()) emit_memory_guard(17, instruction->repne ? 8u : 4u, guest_pc, X86_SOFT_READ);
            if (instruction->repne)
                g_ldr_d(16, 17);
            else
                g_ldr_s(16, 17);
        }
        source = 16;
    }
    uint32_t size_bit = (packed ? instruction->p66 : instruction->repne) ? 0x00400000u : 0;
    uint32_t greater_than = (packed ? 0x6EA0E400u : 0x7EA0E400u) | size_bit;
    if (opcode == 0x5D)
        emit32(greater_than | sse_register_field(vd, 16) | sse_register_field(source, 5) | 17u);
    else
        emit32(greater_than | sse_register_field(source, 16) | sse_register_field(vd, 5) | 17u);
    if (packed) {
        e_v3(0x6E601C00u, 17, vd, source);
        e_vmov(vd, 17);
    } else {
        e_v3(0x2E601C00u, 17, vd, source);
        if (instruction->repne)
            e_ins_d(vd, 0, 17, 0);
        else
            e_ins_s(vd, 0, 17, 0);
    }
    return TX_NEXT;
}
