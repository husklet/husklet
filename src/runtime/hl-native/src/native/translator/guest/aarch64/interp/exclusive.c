// The local exclusive monitor for LDXR/STXR: it records the address AND the value LDXR observed, and STXR
// compare-and-swaps against it. ABA is NOT reproduced -- more permissive, invisible to lock/refcount code.
static __thread int g_interp_monitor_valid;
static __thread uint64_t g_interp_monitor_address;
static __thread unsigned g_interp_monitor_bytes;
static __thread uint64_t g_interp_monitor_value;
static __thread uint64_t g_interp_monitor_value2; // second register of an LDXP

static void interp_monitor_clear(void) {
    g_interp_monitor_valid = 0;
}

// A misaligned atomic: an alignment fault, reported through the JIT's soft-TLB-probe reason so signal.c
// raises it as an ordinary synchronous SIGBUS.
static int interp_alignment_fault(struct cpu *cpu, uint64_t address) {
    cpu->fault_addr = address;
    cpu->bus_ea = address;
    cpu->reason = R_BUS;
    return INTERP_END;
}

static int interp_exec_compare_exchange_pair(struct cpu *cpu, uint32_t insn, uint64_t gpc, uint64_t address, int rt,
                                             int rt2, int rs) {
    if (rt2 != 31) return interp_undefined(cpu, insn, "loads and stores -- unallocated CASP encoding");
    // Rs and Rt each name the first register of a pair, so both must be even.
    if ((rs & 1) || (rt & 1)) return interp_undefined(cpu, insn, "loads and stores -- CASP with an odd register pair");
    // Bit 30 selects a 32-bit or 64-bit pair.
    unsigned element = ((insn >> 30) & 1u) ? 8u : 4u;
    unsigned total = element * 2u;
    void *pointer = interp_atomic_pointer(address, total);
    if (pointer == NULL) return interp_alignment_fault(cpu, address);
    uint64_t compare_low = interp_gpr(cpu, rs), compare_high = interp_gpr(cpu, rs + 1);
    uint64_t swap_low = interp_gpr(cpu, rt), swap_high = interp_gpr(cpu, rt + 1);
    uint64_t observed_low, observed_high;
    interp_access_begin(address, total, 1);
    if (element == 4) {
        // A 32-bit pair is one aligned 64-bit location, with the low register first.
        uint64_t expected = (compare_low & 0xFFFFFFFFu) | ((compare_high & 0xFFFFFFFFu) << 32);
        uint64_t replacement = (swap_low & 0xFFFFFFFFu) | ((swap_high & 0xFFFFFFFFu) << 32);
        __atomic_compare_exchange_n((uint64_t *)pointer, &expected, replacement, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
        observed_low = expected & 0xFFFFFFFFu;
        observed_high = expected >> 32;
    } else {
        unsigned __int128 expected = (unsigned __int128)compare_low | ((unsigned __int128)compare_high << 64);
        unsigned __int128 replacement = (unsigned __int128)swap_low | ((unsigned __int128)swap_high << 64);
        __atomic_compare_exchange_n((unsigned __int128 *)pointer, &expected, replacement, 0, __ATOMIC_SEQ_CST,
                                    __ATOMIC_SEQ_CST);
        observed_low = (uint64_t)expected;
        observed_high = (uint64_t)(expected >> 64);
    }
    interp_access_end();
    // CASP returns the pre-existing pair whether or not the swap happened.
    if (element == 8) {
        interp_set_gpr(cpu, rs, observed_low);
        interp_set_gpr(cpu, rs + 1, observed_high);
    } else {
        interp_set_gpr32(cpu, rs, (uint32_t)observed_low);
        interp_set_gpr32(cpu, rs + 1, (uint32_t)observed_high);
    }
    cpu->pc = gpc + 4;
    return INTERP_NEXT;
}

// Load/store exclusive, plus the ordered (LDAR/STLR) and CAS members of the box.
static int interp_exec_load_store_exclusive(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    int rt = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rt2 = (int)((insn >> 10) & 31);
    unsigned vector = (insn >> 26) & 1;

    if ((insn & 0x3F000000u) == 0x08000000u) {
        unsigned size = (insn >> 30) & 3, o2 = (insn >> 23) & 1, load = (insn >> 22) & 1;
        unsigned o1 = (insn >> 21) & 1, o0 = (insn >> 15) & 1;
        int rs = (int)((insn >> 16) & 31);
        unsigned bytes = 1u << size;

        if (o2 && o1) { // CAS / CASA / CASL / CASAL (Rt2 is 11111)
            if (rt2 != 31) return interp_undefined(cpu, insn, "loads and stores -- unallocated CAS encoding");
            uint64_t address = interp_gpr_sp(cpu, rn);
            void *pointer = interp_atomic_pointer(address, bytes);
            if (pointer == NULL) return interp_alignment_fault(cpu, address);
            uint64_t compare = interp_gpr(cpu, rs), swap = interp_gpr(cpu, rt);
            // Comparand and returned value are the ACCESS width, not the register width.
            uint64_t mask = bytes == 8 ? UINT64_MAX : ((UINT64_C(1) << (bytes * 8)) - 1u);
            uint64_t expected = compare & mask, observed;
            interp_access_begin(address, bytes, 1);
            switch (bytes) {
            case 1: {
                uint8_t narrow = (uint8_t)expected;
                __atomic_compare_exchange_n((uint8_t *)pointer, &narrow, (uint8_t)swap, 0, __ATOMIC_SEQ_CST,
                                            __ATOMIC_SEQ_CST);
                observed = narrow;
                break;
            }
            case 2: {
                uint16_t narrow = (uint16_t)expected;
                __atomic_compare_exchange_n((uint16_t *)pointer, &narrow, (uint16_t)swap, 0, __ATOMIC_SEQ_CST,
                                            __ATOMIC_SEQ_CST);
                observed = narrow;
                break;
            }
            case 4: {
                uint32_t narrow = (uint32_t)expected;
                __atomic_compare_exchange_n((uint32_t *)pointer, &narrow, (uint32_t)swap, 0, __ATOMIC_SEQ_CST,
                                            __ATOMIC_SEQ_CST);
                observed = narrow;
                break;
            }
            default: {
                uint64_t wide = expected;
                __atomic_compare_exchange_n((uint64_t *)pointer, &wide, swap, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
                observed = wide;
                break;
            }
            }
            interp_access_end();
            // CAS returns the PRE-EXISTING value in Rs, whether or not the swap happened.
            if (bytes == 8)
                interp_set_gpr(cpu, rs, observed);
            else
                interp_set_gpr32(cpu, rs, (uint32_t)observed);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        if (o2) { // LDAR / LDLAR / STLR / STLLR: ordered access, no monitor
            if (o1 || rs != 31 || rt2 != 31)
                return interp_undefined(cpu, insn, "loads and stores -- unallocated ordered-access encoding");
            uint64_t address = interp_gpr_sp(cpu, rn);
            // Free on x86-TSO, but this backend is not x86-only and the compiler must be stopped anyway.
            if (load) {
                uint64_t value = interp_load_bits(address, bytes);
                __atomic_thread_fence(__ATOMIC_ACQUIRE);
                if (bytes == 8)
                    interp_set_gpr(cpu, rt, value);
                else
                    interp_set_gpr32(cpu, rt, (uint32_t)value);
            } else {
                uint64_t value = interp_gpr(cpu, rt);
                __atomic_thread_fence(__ATOMIC_RELEASE);
                interp_store_bits(address, value, bytes);
            }
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        uint64_t address = interp_gpr_sp(cpu, rn);

        // CASP shares o1 == 1 with LDXP/STXP and is separated by BIT 31, not by size: `size < 2` alone
        // rejects every CASP as an unallocated pair size.
        if (o1 && !(insn & 0x80000000u)) return interp_exec_compare_exchange_pair(cpu, insn, gpc, address, rt, rt2, rs);

        if (o1 && size < 2) return interp_undefined(cpu, insn, "loads and stores -- unallocated exclusive-pair size");
        unsigned access_bytes = o1 ? bytes * 2u : bytes;
        if (load) { // LDXR / LDAXR / LDXP / LDAXP
            if (rs != 31 || (!o1 && rt2 != 31))
                return interp_undefined(cpu, insn, "loads and stores -- unallocated load-exclusive encoding");
            if (interp_atomic_pointer(address, bytes) == NULL) return interp_alignment_fault(cpu, address);
            uint64_t first = interp_load_bits(address, bytes);
            uint64_t second = o1 ? interp_load_bits(address + bytes, bytes) : 0;
            if (o0) __atomic_thread_fence(__ATOMIC_ACQUIRE); // LDAXR/LDAXP
            g_interp_monitor_address = address;
            g_interp_monitor_bytes = access_bytes;
            g_interp_monitor_value = first;
            g_interp_monitor_value2 = second;
            g_interp_monitor_valid = 1;
            if (bytes == 8) {
                interp_set_gpr(cpu, rt, first);
                if (o1) interp_set_gpr(cpu, rt2, second);
            } else {
                interp_set_gpr32(cpu, rt, (uint32_t)first);
                if (o1) interp_set_gpr32(cpu, rt2, (uint32_t)second);
            }
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }

        // STXR / STLXR / STXP / STLXP: Rs receives 0 on success, 1 on failure.
        if (!o1 && rt2 != 31)
            return interp_undefined(cpu, insn, "loads and stores -- unallocated store-exclusive encoding");
        void *pointer = interp_atomic_pointer(address, bytes);
        if (pointer == NULL) return interp_alignment_fault(cpu, address);
        unsigned failed = 1;
        if (g_interp_monitor_valid && g_interp_monitor_address == address && g_interp_monitor_bytes == access_bytes) {
            uint64_t desired = interp_gpr(cpu, rt);
            if (o0) __atomic_thread_fence(__ATOMIC_RELEASE); // STLXR/STLXP
            interp_access_begin(address, access_bytes, 1);
            if (!o1) {
                switch (bytes) {
                case 1: {
                    uint8_t expected = (uint8_t)g_interp_monitor_value;
                    failed = !__atomic_compare_exchange_n((uint8_t *)pointer, &expected, (uint8_t)desired, 0,
                                                          __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
                    break;
                }
                case 2: {
                    uint16_t expected = (uint16_t)g_interp_monitor_value;
                    failed = !__atomic_compare_exchange_n((uint16_t *)pointer, &expected, (uint16_t)desired, 0,
                                                          __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
                    break;
                }
                case 4: {
                    uint32_t expected = (uint32_t)g_interp_monitor_value;
                    failed = !__atomic_compare_exchange_n((uint32_t *)pointer, &expected, (uint32_t)desired, 0,
                                                          __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
                    break;
                }
                default: {
                    uint64_t expected = g_interp_monitor_value;
                    failed = !__atomic_compare_exchange_n((uint64_t *)pointer, &expected, desired, 0, __ATOMIC_SEQ_CST,
                                                          __ATOMIC_SEQ_CST);
                    break;
                }
                }
            } else {
                // STXP: the pair must commit indivisibly -- a 64-bit pair is one 128-bit CAS, a 32-bit
                // pair one 64-bit CAS. __atomic on 16 bytes may lower to a libatomic lock, consistent only
                // because every access to the location goes through this code.
                uint64_t desired2 = interp_gpr(cpu, rt2);
                if (bytes == 4) {
                    uint64_t expected =
                        (g_interp_monitor_value & 0xFFFFFFFFu) | ((g_interp_monitor_value2 & 0xFFFFFFFFu) << 32);
                    uint64_t replacement = (desired & 0xFFFFFFFFu) | ((desired2 & 0xFFFFFFFFu) << 32);
                    failed = !__atomic_compare_exchange_n((uint64_t *)pointer, &expected, replacement, 0,
                                                          __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
                } else {
                    unsigned __int128 expected =
                        (unsigned __int128)g_interp_monitor_value | ((unsigned __int128)g_interp_monitor_value2 << 64);
                    unsigned __int128 replacement = (unsigned __int128)desired | ((unsigned __int128)desired2 << 64);
                    failed = !__atomic_compare_exchange_n((unsigned __int128 *)pointer, &expected, replacement, 0,
                                                          __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
                }
            }
            interp_access_end();
        }
        // ANY store-exclusive clears the monitor, or a retry loop could succeed without re-reading.
        interp_monitor_clear();
        interp_set_gpr32(cpu, rs, failed);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    return interp_undefined(cpu, insn, "loads and stores -- unallocated exclusive encoding");
}
