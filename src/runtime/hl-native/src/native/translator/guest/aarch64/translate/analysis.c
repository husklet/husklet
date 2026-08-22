static int stolen_forward_classify(uint32_t in, int *mask_out, int *read_out, int *write_out, int *fault_out) {
    int mask, read, write;
    if (x28_alu_window_classify(in, &mask, &read, &write)) {
        *mask_out = mask;
        *read_out = read;
        *write_out = write;
        *fault_out = 0;
        return 1;
    }

    /* Same audited integer-ALU set, but admit guest x16 as well as x28. */
    uint32_t op = (in >> 25) & 0xFu;
    mask = gpr_field_mask(in);
    if (op == 8 || op == 9) {
        if ((in & 0x1F000000u) == 0x10000000u) return 0;
        if ((in & 0x1F800000u) == 0x12800000u) {
            int opc = (in >> 29) & 3;
            if (opc == 1) return 0;
            write = mask & 1;
            read = opc == 3 ? write : 0;
        } else if ((in & 0x1F800000u) == 0x13800000u) {
            read = mask & (2 | 4);
            write = mask & 1;
        } else if ((in & 0x1F000000u) == 0x11000000u || (in & 0x1F800000u) == 0x12000000u ||
                   (in & 0x1F800000u) == 0x13000000u) {
            read = mask & 2;
            write = mask & 1;
        } else {
            goto try_memory;
        }
    } else if ((in & 0x0E000000u) == 0x0A000000u) {
        read = mask & (2 | 4 | 8);
        write = mask & 1;
    } else {
        goto try_memory;
    }
    {
        static const int shifts[4] = {0, 5, 16, 10};
        static const int mbits[4] = {1, 2, 4, 8};
        for (int k = 0; k < 4; k++) {
            if (!(mask & mbits[k])) continue;
            int r = (in >> shifts[k]) & 31;
            if (is_stolen(r) && r != 16 && r != 28) return 0;
        }
    }
    *mask_out = mask;
    *read_out = read;
    *write_out = write;
    *fault_out = 0;
    return 1;

try_memory:
    /* Scalar ordinary single-register loads/stores only. */
    if (in & 0x04000000u) return 0;
    int unsigned_imm = (in & 0x3B000000u) == 0x39000000u;
    int unscaled = (in & 0x3B200000u) == 0x38000000u;
    int regoff = (in & 0x3B200C00u) == 0x38200800u;
    if (!unsigned_imm && !unscaled && !regoff) return 0;
    if (unscaled) {
        int mode = (in >> 10) & 3;
        if (mode == 2) return 0; /* unprivileged: keep uncommon form baseline */
    }

    mask = gpr_field_mask(in);
    int opc = (in >> 22) & 3;
    int size = (in >> 30) & 3;
    if (size == 3 && opc == 2) return 0; /* PRFM */
    int writeback = unscaled && (((in >> 10) & 3) == 1 || ((in >> 10) & 3) == 3);
    int base_index = mask & (2 | 4);
    if (opc == 0) {
        read = mask & (1 | 2 | 4);
        write = writeback ? (mask & 2) : 0;
    } else {
        read = base_index;
        write = (mask & 1) | (writeback ? (mask & 2) : 0);
    }

    static const int shifts[4] = {0, 5, 16, 10};
    static const int mbits[4] = {1, 2, 4, 8};
    for (int k = 0; k < 4; k++) {
        if (!(mask & mbits[k])) continue;
        int r = (in >> shifts[k]) & 31;
        if (is_stolen(r) && r != 16 && r != 28) return 0;
    }
    *mask_out = mask;
    *read_out = read;
    *write_out = write;
    *fault_out = 1;
    return 1;
}

static int stolen_forward_field(uint32_t in, int fields, int reg) {
    static const int shifts[4] = {0, 5, 16, 10};
    static const int mbits[4] = {1, 2, 4, 8};
    for (int k = 0; k < 4; k++)
        if ((fields & mbits[k]) && (int)((in >> shifts[k]) & 31) == reg) return 1;
    return 0;
}

static uint32_t stolen_forward_rewrite(uint32_t in, int mask) {
    static const int shifts[4] = {0, 5, 16, 10};
    static const int mbits[4] = {1, 2, 4, 8};
    for (int k = 0; k < 4; k++)
        if ((mask & mbits[k]) && ((in >> shifts[k]) & 31) == 28) in = (in & ~(31u << shifts[k])) | (17u << shifts[k]);
    return in;
}

static void emit_cpu_model_value(int rd, uint64_t value) {
    if (is_stolen(rd)) {
        if (stealfast_on()) {
            e_movconst(16, value);
            e_str(16, CPUREG, rd * 8);
        } else {
            x18_prolog();
            e_movconst(0, value);
            e_str(0, 1, rd * 8);
            x18_epilog();
        }
    } else {
        e_movconst(rd, value);
    }
}
