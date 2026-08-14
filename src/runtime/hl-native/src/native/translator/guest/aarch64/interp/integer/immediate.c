// Data processing -- immediate; sub-class from insn[25:23].
static int interp_exec_dp_immediate(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    unsigned group = (insn >> 23) & 7;
    unsigned sf = (insn >> 31) & 1;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31);

    switch (group) {
    case 0:
    case 1: { // PC-relative addressing: ADR / ADRP
        int64_t immediate = interp_sext((((insn >> 5) & 0x7FFFFu) << 2) | ((insn >> 29) & 3u), 21);
        uint64_t value;
        if (insn & 0x80000000u) // ADRP: page base, immediate scaled by 4 KiB
            value = (pcrel_base(gpc) & ~UINT64_C(0xFFF)) + ((uint64_t)immediate << 12);
        else
            value = pcrel_base(gpc) + (uint64_t)immediate;
        interp_set_gpr(cpu, rd, value);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    case 2: { // Add/subtract (immediate)
        unsigned op = (insn >> 30) & 1, setflags = (insn >> 29) & 1, shift = (insn >> 22) & 1;
        uint64_t immediate = (insn >> 10) & 0xFFFu;
        if (shift) immediate <<= 12;
        // Rn is <Xn|SP>; Rd is <Xd|SP> for ADD/SUB but XZR for ADDS/SUBS, so `cmp` discards its result.
        if (sf) {
            uint64_t a = interp_gpr_sp(cpu, rn);
            uint64_t result = op ? interp_add_with_carry64(a, ~immediate, 1, cpu, (int)setflags)
                                 : interp_add_with_carry64(a, immediate, 0, cpu, (int)setflags);
            if (setflags)
                interp_set_gpr(cpu, rd, result);
            else
                interp_set_gpr_sp(cpu, rd, result);
        } else {
            uint32_t a = (uint32_t)interp_gpr_sp(cpu, rn);
            uint32_t result = op ? interp_add_with_carry32(a, ~(uint32_t)immediate, 1, cpu, (int)setflags)
                                 : interp_add_with_carry32(a, (uint32_t)immediate, 0, cpu, (int)setflags);
            if (setflags)
                interp_set_gpr32(cpu, rd, result);
            else
                interp_set_gpr32_sp(cpu, rd, result);
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    case 3: // Add/subtract (immediate, with tags): MTE
        return interp_undefined(cpu, insn, "data-processing immediate -- ADDG/SUBG (memory tagging)");
    case 4: { // Logical (immediate)
        unsigned opc = (insn >> 29) & 3, immn = (insn >> 22) & 1;
        unsigned immr = (insn >> 16) & 0x3Fu, imms = (insn >> 10) & 0x3Fu;
        uint64_t wmask;
        if (!interp_bit_masks(sf, immn, imms, immr, 1, &wmask, NULL))
            return interp_undefined(cpu, insn, "data-processing immediate -- undefined logical-immediate mask");
        uint64_t operand = interp_gpr(cpu, rn), result;
        switch (opc) {
        case 0: result = operand & wmask; break;  // AND
        case 1: result = operand | wmask; break;  // ORR
        case 2: result = operand ^ wmask; break;  // EOR
        default: result = operand & wmask; break; // ANDS
        }
        if (!sf) result = (uint32_t)result;
        if (opc == 3) { // ANDS: flag-setting, so Rd is XZR when 31
            interp_set_logical_flags(cpu, result, sf);
            if (sf)
                interp_set_gpr(cpu, rd, result);
            else
                interp_set_gpr32(cpu, rd, (uint32_t)result);
        } else { // AND/ORR/EOR: Rd is <Xd|SP>
            if (sf)
                interp_set_gpr_sp(cpu, rd, result);
            else
                interp_set_gpr32_sp(cpu, rd, (uint32_t)result);
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    case 5: { // Move wide (immediate): MOVN / MOVZ / MOVK
        unsigned opc = (insn >> 29) & 3, hw = (insn >> 21) & 3;
        uint64_t imm16 = (insn >> 5) & 0xFFFFu;
        if (opc == 1) return interp_undefined(cpu, insn, "data-processing immediate -- unallocated move-wide opc");
        if (!sf && (hw & 2)) return interp_undefined(cpu, insn, "data-processing immediate -- 32-bit move-wide hw>1");
        unsigned shift = hw * 16u;
        uint64_t field = imm16 << shift;
        uint64_t result;
        if (opc == 0)
            result = ~field; // MOVN
        else if (opc == 2)
            result = field; // MOVZ
        else                // MOVK: keep the other halfwords
            result = (interp_gpr(cpu, rd) & ~(UINT64_C(0xFFFF) << shift)) | field;
        if (sf)
            interp_set_gpr(cpu, rd, result);
        else
            interp_set_gpr32(cpu, rd, (uint32_t)result);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    case 6: { // Bitfield: SBFM / BFM / UBFM and every alias
        unsigned opc = (insn >> 29) & 3, immn = (insn >> 22) & 1;
        unsigned immr = (insn >> 16) & 0x3Fu, imms = (insn >> 10) & 0x3Fu;
        uint64_t wmask, tmask;
        if (opc == 3 || immn != sf)
            return interp_undefined(cpu, insn, "data-processing immediate -- unallocated bitfield encoding");
        if (!interp_bit_masks(sf, immn, imms, immr, 0, &wmask, &tmask))
            return interp_undefined(cpu, insn, "data-processing immediate -- undefined bitfield mask");
        uint64_t source = interp_gpr(cpu, rn);
        uint64_t rotated = sf ? interp_ror64(source, immr) : (uint64_t)interp_ror32((uint32_t)source, immr);
        uint64_t result;
        if (opc == 1) { // BFM: keep Rd's bits outside the field
            uint64_t destination = interp_gpr(cpu, rd);
            uint64_t bottom = (destination & ~wmask) | (rotated & wmask);
            result = (destination & ~tmask) | (bottom & tmask);
        } else {
            uint64_t bottom = rotated & wmask;
            // SBFM replicates bit S of the source above the field; UBFM zeroes it.
            uint64_t top = (opc == 0 && ((source >> imms) & 1)) ? UINT64_MAX : UINT64_C(0);
            result = (top & ~tmask) | (bottom & tmask);
        }
        if (sf)
            interp_set_gpr(cpu, rd, result);
        else
            interp_set_gpr32(cpu, rd, (uint32_t)result);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    default: { // 7: Extract -- EXTR (ROR when Rn == Rm)
        unsigned immn = (insn >> 22) & 1, imms = (insn >> 10) & 0x3Fu;
        int rm = (int)((insn >> 16) & 31);
        if (((insn >> 29) & 3) != 0 || ((insn >> 21) & 1) != 0 || immn != sf)
            return interp_undefined(cpu, insn, "data-processing immediate -- unallocated extract encoding");
        if (!sf && (imms & 0x20u))
            return interp_undefined(cpu, insn, "data-processing immediate -- 32-bit EXTR lsb>31");
        uint64_t high = interp_gpr(cpu, rn), low = interp_gpr(cpu, rm);
        // Result is the low datasize bits of (Rn:Rm) >> lsb, so the low half comes from Rm.
        if (sf) {
            uint64_t result = imms ? ((low >> imms) | (high << (64 - imms))) : low;
            interp_set_gpr(cpu, rd, result);
        } else {
            uint32_t result = imms ? (((uint32_t)low >> imms) | ((uint32_t)high << (32 - imms))) : (uint32_t)low;
            interp_set_gpr32(cpu, rd, result);
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }
    }
}
