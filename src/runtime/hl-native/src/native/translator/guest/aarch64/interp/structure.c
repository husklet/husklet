/*
 * AdvSIMD multiple- and single-structure load/store decoding.
 *
 * Included by vector.c so the decoder and shared vector helpers remain
 * translation-unit private.
 */
static int interp_exec_load_store_structures(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    int rt = (int)(insn & 31), rn = (int)((insn >> 5) & 31);
    int rt2 = (int)((insn >> 10) & 31), rm = (int)((insn >> 16) & 31);
    unsigned vector = (insn >> 26) & 1;
    unsigned q = (insn >> 30) & 1;

    // AdvSIMD load/store multiple structures. `opcode` names both the register count and whether they
    // INTERLEAVE: LD1 x4 is four whole-register loads, LD4 walks memory one element at a time across four.
    if ((insn & 0xBF200000u) == 0x0C000000u) {
        unsigned load = (insn >> 22) & 1u, opcode = (insn >> 12) & 0xFu, esize_code = (insn >> 10) & 3u;
        int post_index = (insn & 0x00800000u) != 0;
        unsigned registers, interleaved = 1;
        switch (opcode) {
        case 0x0: registers = 4; break; // LD4/ST4
        case 0x2:
            registers = 4;
            interleaved = 0;
            break;                      // LD1/ST1, four registers
        case 0x4: registers = 3; break; // LD3/ST3
        case 0x6:
            registers = 3;
            interleaved = 0;
            break; // LD1/ST1, three registers
        case 0x7:
            registers = 1;
            interleaved = 0;
            break;                      // LD1/ST1, one register
        case 0x8: registers = 2; break; // LD2/ST2
        case 0xA:
            registers = 2;
            interleaved = 0;
            break; // LD1/ST1, two registers
        default: return interp_undefined(cpu, insn, "AdvSIMD load/store -- unallocated multi-structure opcode");
        }
        unsigned bytes = q ? 16u : 8u;
        uint64_t base = interp_gpr_sp(cpu, rn);
        uint64_t address = base;
        // The 1D arrangement exists only for the one-register LD1/ST1 form.
        if (esize_code == 3 && !q && registers > 1)
            return interp_undefined(cpu, insn, "AdvSIMD load/store -- 1D arrangement with several registers");
        if (interleaved) {
            unsigned lanes = interp_vec_lanes(esize_code, q), element_bytes = 1u << esize_code;
            for (unsigned lane = 0; lane < lanes; lane++)
                for (unsigned index = 0; index < registers; index++) {
                    int reg = (rt + (int)index) % 32; // the register list wraps at V31
                    if (load) {
                        uint64_t element = interp_load_bits(address, element_bytes);
                        // Lane 0 starts from zero: unwritten lanes must end up zero, not stale.
                        interp_vec value;
                        if (lane == 0)
                            memset(value.byte, 0, sizeof value.byte);
                        else
                            value = interp_vec_read(cpu, reg);
                        interp_vec_set_element(&value, esize_code, lane, element);
                        interp_vec_write(cpu, reg, value, 1);
                    } else {
                        interp_vec value = interp_vec_read(cpu, reg);
                        interp_store_bits(address, interp_vec_element(&value, esize_code, lane), element_bytes);
                    }
                    address += element_bytes;
                }
        } else {
            for (unsigned index = 0; index < registers; index++) {
                int reg = (rt + (int)index) % 32;
                if (load) {
                    interp_vec value;
                    memset(value.byte, 0, sizeof value.byte);
                    for (unsigned offset = 0; offset < bytes; offset += 8) {
                        uint64_t chunk = interp_load_bits(address + offset, 8);
                        memcpy(value.byte + offset, &chunk, 8);
                    }
                    interp_vec_write(cpu, reg, value, q);
                } else {
                    interp_vec value = interp_vec_read(cpu, reg);
                    for (unsigned offset = 0; offset < bytes; offset += 8) {
                        uint64_t chunk;
                        memcpy(&chunk, value.byte + offset, 8);
                        interp_store_bits(address + offset, chunk, 8);
                    }
                }
                address += bytes;
            }
        }
        if (post_index) {
            // Rm == 31: the increment is the whole transfer size.
            uint64_t increment = rm == 31 ? (uint64_t)registers * bytes : interp_gpr(cpu, rm);
            interp_set_gpr_sp(cpu, rn, base + increment);
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // AdvSIMD load/store SINGLE structure, and the LD1R..LD4R replicating loads. Register count is
    // (R ? 2 : 1) + (opcode<0> ? 2 : 0); the lane INDEX is Q:S:size for 8-bit, Q:S:size<1> for 16-bit, Q:S
    // for 32-bit, Q alone for 64-bit -- `size` is NOT a lane count. The mask covers bits[29:24] only, since
    // bit23 is post-index and bit21 is R and either one would reject every LD2R/LD4R.
    if ((insn & 0xBF000000u) == 0x0D000000u) {
        unsigned load = (insn >> 22) & 1u, replicate_group = (insn >> 21) & 1u;
        unsigned opcode = (insn >> 13) & 7u, selector = (insn >> 12) & 1u, size_field = (insn >> 10) & 3u;
        int post_index = (insn & 0x00800000u) != 0;
        unsigned registers = (replicate_group ? 2u : 1u) + ((opcode & 1u) ? 2u : 0u);
        uint64_t base = interp_gpr_sp(cpu, rn), address = base;
        unsigned element_size, index;
        if ((opcode >> 1) == 3u) { // LD1R / LD2R / LD3R / LD4R
            if (!load || selector) return interp_undefined(cpu, insn, "AdvSIMD load single -- unallocated replicate");
            element_size = size_field;
            unsigned bytes = 1u << element_size;
            for (unsigned entry = 0; entry < registers; entry++) {
                uint64_t element = interp_load_bits(address, bytes);
                interp_vec value;
                memset(value.byte, 0, sizeof value.byte);
                for (unsigned lane = 0; lane < interp_vec_lanes(element_size, q); lane++)
                    interp_vec_set_element(&value, element_size, lane, element);
                interp_vec_write(cpu, (rt + (int)entry) % 32, value, q);
                address += bytes;
            }
        } else {
            switch (opcode >> 1) {
            case 0: // 8-bit: the index uses Q, S and both size bits
                element_size = 0;
                index = (q << 3) | (selector << 2) | size_field;
                break;
            case 1: // 16-bit: size<0> is RES0 and does not participate
                if (size_field & 1u) return interp_undefined(cpu, insn, "AdvSIMD load single -- 16-bit size<0> set");
                element_size = 1;
                index = (q << 2) | (selector << 1) | (size_field >> 1);
                break;
            default: // 32-bit when size == 00, 64-bit when size == 01 (S must then be 0)
                if (size_field == 0) {
                    element_size = 2;
                    index = (q << 1) | selector;
                } else if (size_field == 1 && selector == 0) {
                    element_size = 3;
                    index = q;
                } else {
                    return interp_undefined(cpu, insn, "AdvSIMD load single -- unallocated 32/64-bit form");
                }
                break;
            }
            unsigned bytes = 1u << element_size;
            for (unsigned entry = 0; entry < registers; entry++) {
                int reg = (rt + (int)entry) % 32;
                if (load) {
                    uint64_t element = interp_load_bits(address, bytes);
                    // A single-lane LOAD leaves every other lane unchanged, [127:64] included.
                    interp_vec value = interp_vec_read(cpu, reg);
                    interp_vec_set_element(&value, element_size, index, element);
                    interp_vec_write(cpu, reg, value, 1);
                } else {
                    interp_vec value = interp_vec_read(cpu, reg);
                    interp_store_bits(address, interp_vec_element(&value, element_size, index), bytes);
                }
                address += bytes;
            }
        }
        if (post_index) {
            uint64_t increment = rm == 31 ? (address - base) : interp_gpr(cpu, rm);
            interp_set_gpr_sp(cpu, rn, base + increment);
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    return interp_undefined(cpu, insn, "loads and stores -- unallocated structure encoding");
}
