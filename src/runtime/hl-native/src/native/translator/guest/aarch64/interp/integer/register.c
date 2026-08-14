// ShiftReg(). `amount` is already masked to the operand size by the encoding.
static uint64_t interp_shift_operand(uint64_t value, unsigned shift_type, unsigned amount, unsigned sf) {
    if (sf) {
        switch (shift_type) {
        case 0: return amount ? (value << amount) : value;                               // LSL
        case 1: return amount ? (value >> amount) : value;                               // LSR
        case 2: return (uint64_t)(amount ? ((int64_t)value >> amount) : (int64_t)value); // ASR
        default: return interp_ror64(value, amount);                                     // ROR
        }
    }
    uint32_t narrow = (uint32_t)value;
    switch (shift_type) {
    case 0: return (uint64_t)(uint32_t)(amount ? (narrow << amount) : narrow);
    case 1: return (uint64_t)(uint32_t)(amount ? (narrow >> amount) : narrow);
    case 2: return (uint64_t)(uint32_t)(amount ? (uint32_t)((int32_t)narrow >> amount) : narrow);
    default: return (uint64_t)interp_ror32(narrow, amount);
    }
}

// ExtendReg(): Rm sign/zero-extended per `option`, then shifted. UXTX/SXTX read the whole register.
static uint64_t interp_extend_operand(const struct cpu *cpu, int rm, unsigned option, unsigned shift, unsigned sf) {
    uint64_t value = interp_gpr(cpu, rm);
    uint64_t extended;
    switch (option) {
    case 0: extended = (uint8_t)value; break;                    // UXTB
    case 1: extended = (uint16_t)value; break;                   // UXTH
    case 2: extended = (uint32_t)value; break;                   // UXTW
    case 3: extended = value; break;                             // UXTX
    case 4: extended = (uint64_t)(int64_t)(int8_t)value; break;  // SXTB
    case 5: extended = (uint64_t)(int64_t)(int16_t)value; break; // SXTH
    case 6: extended = (uint64_t)(int64_t)(int32_t)value; break; // SXTW
    default: extended = value; break;                            // SXTX
    }
    extended <<= shift;
    return sf ? extended : (uint64_t)(uint32_t)extended;
}

// Add, logical, and multiply register forms.
static int interp_exec_dp_register_arithmetic(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    unsigned sf = (insn >> 31) & 1;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);

    // Add/subtract, shifted and extended register; insn[21] separates them.
    if ((insn & 0x1F000000u) == 0x0B000000u) {
        unsigned op = (insn >> 30) & 1, setflags = (insn >> 29) & 1;
        uint64_t operand;
        int destination_is_sp;
        if (insn & 0x00200000u) { // extended register
            unsigned option = (insn >> 13) & 7, shift = (insn >> 10) & 7;
            if (shift > 4) return interp_undefined(cpu, insn, "data-processing register -- extend shift > 4");
            operand = interp_extend_operand(cpu, rm, option, shift, sf);
            destination_is_sp = 1; // Rn is <Xn|SP>; Rd too unless flag-setting
        } else {                   // shifted register
            unsigned shift_type = (insn >> 22) & 3, amount = (insn >> 10) & 0x3Fu;
            if (shift_type == 3) return interp_undefined(cpu, insn, "data-processing register -- add/sub ROR");
            if (!sf && (amount & 0x20u))
                return interp_undefined(cpu, insn, "data-processing register -- 32-bit add/sub shift > 31");
            operand = interp_shift_operand(interp_gpr(cpu, rm), shift_type, amount, sf);
            destination_is_sp = 0; // this form names no SP; 31 is XZR throughout
        }
        if (sf) {
            uint64_t a = destination_is_sp ? interp_gpr_sp(cpu, rn) : interp_gpr(cpu, rn);
            uint64_t result = op ? interp_add_with_carry64(a, ~operand, 1, cpu, (int)setflags)
                                 : interp_add_with_carry64(a, operand, 0, cpu, (int)setflags);
            if (destination_is_sp && !setflags)
                interp_set_gpr_sp(cpu, rd, result);
            else
                interp_set_gpr(cpu, rd, result);
        } else {
            uint32_t a = (uint32_t)(destination_is_sp ? interp_gpr_sp(cpu, rn) : interp_gpr(cpu, rn));
            uint32_t result = op ? interp_add_with_carry32(a, ~(uint32_t)operand, 1, cpu, (int)setflags)
                                 : interp_add_with_carry32(a, (uint32_t)operand, 0, cpu, (int)setflags);
            if (destination_is_sp && !setflags)
                interp_set_gpr32_sp(cpu, rd, result);
            else
                interp_set_gpr32(cpu, rd, result);
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // Logical (shifted register). opc selects AND/ORR/EOR/ANDS and N inverts Rm.
    if ((insn & 0x1F000000u) == 0x0A000000u) {
        unsigned opc = (insn >> 29) & 3, shift_type = (insn >> 22) & 3, negate = (insn >> 21) & 1;
        unsigned amount = (insn >> 10) & 0x3Fu;
        if (!sf && (amount & 0x20u))
            return interp_undefined(cpu, insn, "data-processing register -- 32-bit logical shift > 31");
        uint64_t operand = interp_shift_operand(interp_gpr(cpu, rm), shift_type, amount, sf);
        if (negate) operand = sf ? ~operand : (uint64_t)(uint32_t)~(uint32_t)operand;
        uint64_t a = interp_gpr(cpu, rn), result;
        switch (opc) {
        case 0: result = a & operand; break;  // AND / BIC
        case 1: result = a | operand; break;  // ORR / ORN  (MOV reg is ORR with Rn == XZR)
        case 2: result = a ^ operand; break;  // EOR / EON
        default: result = a & operand; break; // ANDS / BICS
        }
        if (!sf) result = (uint32_t)result;
        if (opc == 3) interp_set_logical_flags(cpu, result, sf);
        if (sf)
            interp_set_gpr(cpu, rd, result);
        else
            interp_set_gpr32(cpu, rd, (uint32_t)result);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if ((insn & 0x1F000000u) == 0x1B000000u) { // Data-processing (3 source)
        unsigned op31 = (insn >> 21) & 7, o0 = (insn >> 15) & 1;
        int ra = (int)((insn >> 10) & 31);
        uint64_t addend = interp_gpr(cpu, ra);
        switch (op31) {
        case 0: { // MADD / MSUB (MUL / MNEG with Ra == XZR)
            if (sf) {
                uint64_t product = interp_gpr(cpu, rn) * interp_gpr(cpu, rm);
                interp_set_gpr(cpu, rd, o0 ? addend - product : addend + product);
            } else {
                uint32_t product = (uint32_t)interp_gpr(cpu, rn) * (uint32_t)interp_gpr(cpu, rm);
                uint32_t base = (uint32_t)addend;
                interp_set_gpr32(cpu, rd, o0 ? base - product : base + product);
            }
            break;
        }
        case 1: { // SMADDL / SMSUBL (SMULL / SMNEGL with Ra == XZR)
            if (!sf) return interp_undefined(cpu, insn, "data-processing register -- 32-bit widening multiply");
            int64_t product = (int64_t)(int32_t)interp_gpr(cpu, rn) * (int64_t)(int32_t)interp_gpr(cpu, rm);
            interp_set_gpr(cpu, rd, o0 ? addend - (uint64_t)product : addend + (uint64_t)product);
            break;
        }
        case 2: { // SMULH
            if (!sf || o0) return interp_undefined(cpu, insn, "data-processing register -- unallocated SMULH form");
            __int128 product = (__int128)(int64_t)interp_gpr(cpu, rn) * (__int128)(int64_t)interp_gpr(cpu, rm);
            interp_set_gpr(cpu, rd, (uint64_t)(product >> 64));
            break;
        }
        case 5: { // UMADDL / UMSUBL (UMULL / UMNEGL with Ra == XZR)
            if (!sf) return interp_undefined(cpu, insn, "data-processing register -- 32-bit widening multiply");
            uint64_t product = (uint64_t)(uint32_t)interp_gpr(cpu, rn) * (uint64_t)(uint32_t)interp_gpr(cpu, rm);
            interp_set_gpr(cpu, rd, o0 ? addend - product : addend + product);
            break;
        }
        case 6: { // UMULH
            if (!sf || o0) return interp_undefined(cpu, insn, "data-processing register -- unallocated UMULH form");
            unsigned __int128 product = (unsigned __int128)interp_gpr(cpu, rn) * (unsigned __int128)interp_gpr(cpu, rm);
            interp_set_gpr(cpu, rd, (uint64_t)(product >> 64));
            break;
        }
        default: return interp_undefined(cpu, insn, "data-processing register -- unallocated 3-source op31");
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    return interp_undefined(cpu, insn, "data-processing register -- unallocated arithmetic encoding");
}

// Carry and conditional-compare forms, which update NZCV.
static int interp_exec_dp_register_flags(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    unsigned sf = (insn >> 31) & 1;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);

    if ((insn & 0x1FE00000u) == 0x1A000000u) { // ADC / ADCS / SBC / SBCS
        unsigned op = (insn >> 30) & 1, setflags = (insn >> 29) & 1;
        if ((insn & 0x0000FC00u) != 0)
            return interp_undefined(cpu, insn, "data-processing register -- rotate/flag ops");
        unsigned carry = interp_flag_c(cpu);
        if (sf) {
            uint64_t a = interp_gpr(cpu, rn), b = interp_gpr(cpu, rm);
            uint64_t result = op ? interp_add_with_carry64(a, ~b, carry, cpu, (int)setflags)
                                 : interp_add_with_carry64(a, b, carry, cpu, (int)setflags);
            interp_set_gpr(cpu, rd, result);
        } else {
            uint32_t a = (uint32_t)interp_gpr(cpu, rn), b = (uint32_t)interp_gpr(cpu, rm);
            uint32_t result = op ? interp_add_with_carry32(a, ~b, carry, cpu, (int)setflags)
                                 : interp_add_with_carry32(a, b, carry, cpu, (int)setflags);
            interp_set_gpr32(cpu, rd, result);
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if ((insn & 0x1FE00000u) == 0x1A400000u) { // Conditional compare: CCMN / CCMP
        unsigned op = (insn >> 30) & 1, setflags = (insn >> 29) & 1;
        unsigned cond = (insn >> 12) & 0xFu, immediate_form = (insn >> 11) & 1;
        unsigned nzcv = insn & 0xFu;
        if (!setflags || ((insn >> 10) & 1) != 0 || ((insn >> 4) & 1) != 0)
            return interp_undefined(cpu, insn, "data-processing register -- unallocated conditional-compare form");
        if (!interp_cond_holds(cpu, cond)) {
            // Condition failed: NZCV is REPLACED by the encoded nzcv literal, not left alone.
            interp_set_flags(cpu, (nzcv >> 3) & 1, (nzcv >> 2) & 1, (nzcv >> 1) & 1, nzcv & 1);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        uint64_t operand = immediate_form ? (uint64_t)((insn >> 16) & 0x1Fu) : interp_gpr(cpu, rm);
        if (sf) {
            uint64_t a = interp_gpr(cpu, rn);
            if (op)
                (void)interp_add_with_carry64(a, ~operand, 1, cpu, 1); // CCMP
            else
                (void)interp_add_with_carry64(a, operand, 0, cpu, 1); // CCMN
        } else {
            uint32_t a = (uint32_t)interp_gpr(cpu, rn);
            if (op)
                (void)interp_add_with_carry32(a, ~(uint32_t)operand, 1, cpu, 1);
            else
                (void)interp_add_with_carry32(a, (uint32_t)operand, 0, cpu, 1);
        }
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    return interp_undefined(cpu, insn, "data-processing register -- unallocated flag encoding");
}

// Conditional selection and unary/binary register forms.
static int interp_exec_dp_register_select(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    unsigned sf = (insn >> 31) & 1;
    int rd = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);

    if ((insn & 0x1FE00000u) == 0x1A800000u) { // CSEL / CSINC / CSINV / CSNEG
        unsigned op = (insn >> 30) & 1, setflags = (insn >> 29) & 1, op2 = (insn >> 10) & 3;
        if (setflags || (op2 & 2))
            return interp_undefined(cpu, insn, "data-processing register -- unallocated conditional-select form");
        unsigned cond = (insn >> 12) & 0xFu;
        uint64_t result;
        if (interp_cond_holds(cpu, cond)) {
            result = interp_gpr(cpu, rn);
        } else {
            uint64_t other = interp_gpr(cpu, rm);
            if (!op)
                result = op2 ? other + 1 : other; // CSINC : CSEL
            else
                result = op2 ? (uint64_t)0 - other : ~other; // CSNEG : CSINV
        }
        if (sf)
            interp_set_gpr(cpu, rd, result);
        else
            interp_set_gpr32(cpu, rd, (uint32_t)result);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if ((insn & 0x1FE00000u) == 0x1AC00000u) { // Data-processing (1 source) and (2 source)
        if (insn & 0x40000000u) {              // insn[30] == 1: 1 source
            unsigned opcode2 = (insn >> 16) & 0x1Fu, opcode = (insn >> 10) & 0x3Fu;
            if (((insn >> 29) & 1) || opcode2 != 0)
                return interp_undefined(cpu, insn, "data-processing register -- PAC/1-source extension");
            uint64_t value = interp_gpr(cpu, rn), result;
            switch (opcode) {
            case 0: { // RBIT
                uint64_t wide = value;
                wide = ((wide & UINT64_C(0x5555555555555555)) << 1) | ((wide >> 1) & UINT64_C(0x5555555555555555));
                wide = ((wide & UINT64_C(0x3333333333333333)) << 2) | ((wide >> 2) & UINT64_C(0x3333333333333333));
                wide = ((wide & UINT64_C(0x0F0F0F0F0F0F0F0F)) << 4) | ((wide >> 4) & UINT64_C(0x0F0F0F0F0F0F0F0F));
                wide = __builtin_bswap64(wide);
                result = sf ? wide : (uint64_t)(uint32_t)(wide >> 32);
                break;
            }
            case 1: { // REV16: byte-swap within each halfword
                uint64_t wide = value;
                result = ((wide & UINT64_C(0x00FF00FF00FF00FF)) << 8) | ((wide >> 8) & UINT64_C(0x00FF00FF00FF00FF));
                break;
            }
            case 2: // REV (32-bit form) / REV32 (64-bit form)
                if (sf) {
                    uint64_t wide = value;
                    result =
                        ((wide & UINT64_C(0x000000FF000000FF)) << 24) | ((wide & UINT64_C(0x0000FF000000FF00)) << 8) |
                        ((wide >> 8) & UINT64_C(0x0000FF000000FF00)) | ((wide >> 24) & UINT64_C(0x000000FF000000FF));
                } else {
                    result = (uint64_t)__builtin_bswap32((uint32_t)value);
                }
                break;
            case 3: // REV (64-bit form)
                if (!sf) return interp_undefined(cpu, insn, "data-processing register -- 32-bit REV64");
                result = __builtin_bswap64(value);
                break;
            case 4: // CLZ
                if (sf)
                    result = value ? (uint64_t)__builtin_clzll(value) : 64u;
                else
                    result = (uint32_t)value ? (uint64_t)__builtin_clz((uint32_t)value) : 32u;
                break;
            case 5: { // CLS
                // CountLeadingSignBits is CLZ over bits [N-1:1] of (x ^ (x << 1)): the fold must be shifted
                // DOWN first, and the count is one less than a full-width CLZ. All-ones is the catching case.
                if (sf) {
                    uint64_t narrowed = (value ^ (value << 1)) >> 1;
                    result = narrowed ? (uint64_t)__builtin_clzll(narrowed) - 1u : 63u;
                } else {
                    uint32_t narrowed = (uint32_t)((uint32_t)value ^ ((uint32_t)value << 1)) >> 1;
                    result = narrowed ? (uint64_t)__builtin_clz(narrowed) - 1u : 31u;
                }
                break;
            }
            default: return interp_undefined(cpu, insn, "data-processing register -- unallocated 1-source opcode");
            }
            if (sf)
                interp_set_gpr(cpu, rd, result);
            else
                interp_set_gpr32(cpu, rd, (uint32_t)result);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        unsigned opcode = (insn >> 10) & 0x3Fu;
        if ((insn >> 29) & 1) return interp_undefined(cpu, insn, "data-processing register -- flag-setting 2-source");
        uint64_t a = interp_gpr(cpu, rn), b = interp_gpr(cpu, rm), result;
        switch (opcode) {
        case 2: // UDIV: /0 yields 0, it does not trap
            if (sf)
                result = b ? a / b : 0;
            else
                result = (uint32_t)b ? (uint64_t)((uint32_t)a / (uint32_t)b) : 0;
            break;
        case 3: // SDIV: /0 yields 0, INT_MIN / -1 saturates to INT_MIN; neither traps
            if (sf) {
                int64_t x = (int64_t)a, y = (int64_t)b;
                result = y == 0 ? 0 : (y == -1 && x == INT64_MIN ? (uint64_t)x : (uint64_t)(x / y));
            } else {
                int32_t x = (int32_t)a, y = (int32_t)b;
                result = y == 0 ? 0 : (uint64_t)(uint32_t)(y == -1 && x == INT32_MIN ? x : x / y);
            }
            break;
        // The variable shifts mask their amount by the operand size, so LSLV by 64 is a no-op, not zero.
        case 8: result = interp_shift_operand(a, 0, (unsigned)(b & (sf ? 63u : 31u)), sf); break;  // LSLV
        case 9: result = interp_shift_operand(a, 1, (unsigned)(b & (sf ? 63u : 31u)), sf); break;  // LSRV
        case 10: result = interp_shift_operand(a, 2, (unsigned)(b & (sf ? 63u : 31u)), sf); break; // ASRV
        case 11:
            result = interp_shift_operand(a, 3, (unsigned)(b & (sf ? 63u : 31u)), sf);
            break; // RORV
        // CRC32B/H/W/X (10000..10011) and CRC32CB/H/W/X (10100..10111). sf names the DATA operand width
        // only, so it must be 1 for exactly the ..X forms; accumulator and result are always 32-bit.
        case 16:
        case 17:
        case 18:
        case 19:
        case 20:
        case 21:
        case 22:
        case 23: {
            unsigned data_bytes = 1u << (opcode & 3u);
            if ((data_bytes == 8) != (sf != 0))
                return interp_undefined(cpu, insn, "data-processing register -- CRC32 size/sf mismatch");
            result = interp_crc32((uint32_t)a, b, data_bytes, (opcode & 4u) != 0);
            // Always a W register, so not the sf-selected write below.
            interp_set_gpr32(cpu, rd, (uint32_t)result);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        default: return interp_undefined(cpu, insn, "data-processing register -- unallocated 2-source opcode");
        }
        if (sf)
            interp_set_gpr(cpu, rd, result);
        else
            interp_set_gpr32(cpu, rd, (uint32_t)result);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    return interp_undefined(cpu, insn, "data-processing register -- unallocated selection encoding");
}

// Data processing -- register; dispatch sub-classes before executing one bounded encoding family.
static int interp_exec_dp_register(struct cpu *cpu, uint32_t insn) {
    uint32_t group = insn & 0x1FE00000u;
    if ((insn & 0x1F000000u) == 0x0B000000u || (insn & 0x1F000000u) == 0x0A000000u ||
        (insn & 0x1F000000u) == 0x1B000000u)
        return interp_exec_dp_register_arithmetic(cpu, insn);
    if (group == 0x1A000000u || group == 0x1A400000u) return interp_exec_dp_register_flags(cpu, insn);
    if (group == 0x1A800000u || group == 0x1AC00000u) return interp_exec_dp_register_select(cpu, insn);
    return interp_undefined(cpu, insn, "data-processing register -- unallocated encoding");
}
