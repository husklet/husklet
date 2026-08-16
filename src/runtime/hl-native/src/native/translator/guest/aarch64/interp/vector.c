// The vector register file. cpu->v[] holds V0..V31 as {low, high} uint64 pairs -- the layout
// guest/aarch64/signal.c memcpy's into the sigframe's fpsimd_context, so guest-visible ABI.
typedef struct {
    uint8_t byte[16];
} interp_vec;

static interp_vec interp_vec_read(const struct cpu *cpu, int reg) {
    interp_vec value;
    memcpy(value.byte, &cpu->v[2 * reg], 16);
    return value;
}

// THE RULE THAT IS EASY TO GET WRONG: a D-form (Q == 0) write must ZERO the upper 64 bits of the
// destination, for every AdvSIMD and scalar-FP write; keeping the old half is invisible until it is read.
static void interp_vec_write(struct cpu *cpu, int reg, interp_vec value, unsigned q) {
    if (!q) memset(value.byte + 8, 0, 8);
    memcpy(&cpu->v[2 * reg], value.byte, 16);
    // Nothing to spill here, but vdirty is in the checkpoint image the JIT shares; keep it truthful.
    cpu->vdirty = (uint64_t)(uintptr_t)cpu;
}

// `size` is the architecture's log2 element width: 0 = B, 1 = H, 2 = S, 3 = D.
static uint64_t interp_vec_element(const interp_vec *value, unsigned size, unsigned index) {
    uint64_t element = 0;
    memcpy(&element, value->byte + (index << size), (size_t)1u << size);
    return element;
}

static void interp_vec_set_element(interp_vec *value, unsigned size, unsigned index, uint64_t element) {
    memcpy(value->byte + (index << size), &element, (size_t)1u << size);
}

static unsigned interp_vec_lanes(unsigned size, unsigned q) {
    return (q ? 16u : 8u) >> size;
}

static uint64_t interp_element_mask(unsigned size) {
    return size >= 3 ? UINT64_MAX : ((UINT64_C(1) << (8u << size)) - 1u);
}

static uint64_t interp_element_sext(uint64_t element, unsigned size) {
    return (uint64_t)interp_sext(element, 8u << size);
}

// The byte dot product shared by FEAT_DotProd (SDOT/UDOT) and FEAT_I8MM (USDOT, SMMLA/UMMLA/USMMLA): four
// byte products summed modulo 2^32, exactly as the ARM ARM writes them. MMLA calls it twice per lane for its
// eight-element rows. Signedness is per SOURCE, which is what the mixed US/SU forms need.
static uint32_t interp_dot4(const interp_vec *left, const interp_vec *right, unsigned left_base, unsigned right_base,
                            int left_signed, int right_signed) {
    uint32_t sum = 0;
    for (unsigned i = 0; i < 4u; i++) {
        uint8_t a = left->byte[left_base + i], b = right->byte[right_base + i];
        int32_t x = left_signed ? (int32_t)(int8_t)a : (int32_t)a;
        int32_t y = right_signed ? (int32_t)(int8_t)b : (int32_t)b;
        sum += (uint32_t)(x * y);
    }
    return sum;
}

// Maps the three-same-extra opcode + U to how each source's bytes are read; 0 means "not a dot/MMLA form".
// 0010/0100 are the same-signedness pairs (S with U=0, U with U=1); 0011/0101 are the mixed unsigned-by-signed
// forms, for which U=1 is unallocated.
static int interp_dot_signedness(unsigned opcode, unsigned u, int *left_signed, int *right_signed) {
    if (opcode == 2u || opcode == 4u) {
        *left_signed = *right_signed = !u;
        return 1;
    }
    if ((opcode == 3u || opcode == 5u) && !u) {
        *left_signed = 0;
        *right_signed = 1;
        return 1;
    }
    return 0;
}

// AdvSIMDExpandImm(), for MOVI/MVNI and immediate ORR/BIC. Returns 0 for a reserved cmode/op.
static int interp_advsimd_expand_imm(unsigned op, unsigned cmode, unsigned o2, unsigned q, uint64_t imm8,
                                     uint64_t *out) {
    unsigned selector = (cmode >> 1) & 7, low = cmode & 1;
    uint64_t imm64;
    if (selector <= 3 && !low) { // 32-bit element, imm8 shifted by 0/8/16/24
        uint32_t narrow = (uint32_t)(imm8 << (8u * selector));
        imm64 = ((uint64_t)narrow << 32) | narrow;
    } else if (selector <= 3) { // the same shifts, but the ORR/BIC-immediate spelling
        uint32_t narrow = (uint32_t)(imm8 << (8u * selector));
        imm64 = ((uint64_t)narrow << 32) | narrow;
    } else if (selector == 4 || selector == 5) { // 16-bit element, imm8 shifted by 0 or 8
        uint16_t narrow = (uint16_t)(imm8 << (8u * (selector & 1u)));
        uint32_t doubled = ((uint32_t)narrow << 16) | narrow;
        imm64 = ((uint64_t)doubled << 32) | doubled;
    } else if (selector == 6) { // 32-bit element with a "moving ones" low field (MSL)
        uint32_t narrow = low ? (uint32_t)((imm8 << 16) | 0xFFFFu) : (uint32_t)((imm8 << 8) | 0xFFu);
        imm64 = ((uint64_t)narrow << 32) | narrow;
    } else if (!low && !op) { // 8-bit element replicated: MOVI Vd.8B/16B, #imm8
        if (o2) return 0;
        imm64 = imm8 * UINT64_C(0x0101010101010101);
    } else if (!low) {
        // cmode == 1110 with op == 1: each BIT of imm8 becomes a whole BYTE of the element (imm8<0> lowest),
        // so `movi v0.2d, #0xffffffff` is 0x00000000ffffffff, not the op == 0 arm's byte replication.
        if (o2) return 0;
        imm64 = 0;
        for (unsigned byte = 0; byte < 8u; byte++)
            if ((imm8 >> byte) & 1u) imm64 |= UINT64_C(0xFF) << (8u * byte);
    } else if (!op && o2) { // half-precision float expansion, replicated (FMOV Vd.4H/8H, #imm)
        uint32_t sign = (uint32_t)((imm8 >> 7) & 1), exponent = (uint32_t)((imm8 >> 4) & 7);
        uint32_t fraction = (uint32_t)(imm8 & 0xFu);
        uint32_t narrow =
            (sign << 15) | ((exponent & 4u) ? 0x3000u : 0x4000u) | ((exponent & 3u) << 10) | (fraction << 6);
        imm64 = (uint64_t)narrow * UINT64_C(0x0001000100010001);
    } else if (!op) { // single-precision float expansion, replicated
        uint32_t sign = (uint32_t)((imm8 >> 7) & 1), exponent = (uint32_t)((imm8 >> 4) & 7);
        uint32_t fraction = (uint32_t)(imm8 & 0xFu);
        uint32_t narrow =
            (sign << 31) | ((exponent & 4u) ? 0x3E000000u : 0x40000000u) | ((exponent & 3u) << 23) | (fraction << 19);
        imm64 = ((uint64_t)narrow << 32) | narrow;
    } else { // double-precision float expansion (Q must be 1)
        if (!q || o2) return 0;
        uint64_t sign = (imm8 >> 7) & 1, exponent = (imm8 >> 4) & 7, fraction = imm8 & 0xFu;
        imm64 = (sign << 63) | ((exponent & 4u) ? UINT64_C(0x3FC0000000000000) : UINT64_C(0x4000000000000000)) |
                (exponent & 3u) << 52 | (fraction << 48);
    }
    *out = imm64;
    return 1;
}

// imm5 encodes the element size in its lowest set bit and the lane index above it; none set is reserved.
static int interp_imm5_element(unsigned imm5, unsigned *size_out, unsigned *index_out) {
    if (imm5 & 1u) {
        *size_out = 0;
        *index_out = (imm5 >> 1) & 0xFu;
    } else if (imm5 & 2u) {
        *size_out = 1;
        *index_out = (imm5 >> 2) & 7u;
    } else if (imm5 & 4u) {
        *size_out = 2;
        *index_out = (imm5 >> 3) & 3u;
    } else if (imm5 & 8u) {
        *size_out = 3;
        *index_out = (imm5 >> 4) & 1u;
    } else {
        return 0;
    }
    return 1;
}

// The by-element operand of the "vector x indexed element" box, keyed on the ELEMENT size (log2), not on the
// encoding's `size` field -- half-precision FP spells H as size 00 but indexes like any 16-bit element.
// THE PART THAT IS SILENT WHEN WRONG: the index field GROWS as the element shrinks, at Rm's expense.
//   16-bit: index = H:L:M, so Rm is 4 bits -- only V0..V15 are addressable.
//   32-bit: index = H:L,   Rm = M:Rm.
//   64-bit: index = H,     Rm = M:Rm, and L must be 0.
// Reading H:L for a 16-bit element takes the right value from the wrong lane, which most tests never see.
static int interp_elem_index(uint32_t decode, unsigned size, unsigned *index_out, int *reg_out) {
    unsigned l = (decode >> 21) & 1u, m = (decode >> 20) & 1u, h = (decode >> 11) & 1u;
    unsigned low = (decode >> 16) & 0xFu;
    if (size == 1u) {
        *index_out = (h << 2) | (l << 1) | m;
        *reg_out = (int)low;
        return 1;
    }
    *reg_out = (int)(low | (m << 4));
    if (size == 2u) {
        *index_out = (h << 1) | l;
        return 1;
    }
    if (size != 3u || l) return 0;
    *index_out = h;
    return 1;
}

static void interp_vec_load(struct cpu *cpu, int reg, uint64_t address, unsigned bytes) {
    interp_vec value;
    memset(value.byte, 0, sizeof value.byte);
    if (bytes <= 8) {
        uint64_t chunk = interp_load_bits(address, bytes);
        memcpy(value.byte, &chunk, bytes);
    } else {
        uint64_t low = interp_load_bits(address, 8), high = interp_load_bits(address + 8, 8);
        memcpy(value.byte, &low, 8);
        memcpy(value.byte + 8, &high, 8);
    }
    interp_vec_write(cpu, reg, value, bytes > 8);
}

static void interp_vec_store(struct cpu *cpu, int reg, uint64_t address, unsigned bytes) {
    interp_vec value = interp_vec_read(cpu, reg);
    uint64_t low, high;
    memcpy(&low, value.byte, 8);
    memcpy(&high, value.byte + 8, 8);
    if (bytes <= 8) {
        interp_store_bits(address, low, bytes);
    } else {
        interp_store_bits(address, low, 8);
        interp_store_bits(address + 8, high, 8);
    }
}

// SIMD&FP access width, spelled opc<1>:size and NOT plain size. 0 means unallocated.
static unsigned interp_simd_access_bytes(unsigned size, unsigned opc) {
    if (opc & 2u) return size == 0 ? 16u : 0u;
    return 1u << size;
}

// Loads and stores. Three rules, each enforced in one place:
//   * Rn == 31 IS SP, NOT XZR, in every addressing mode here (interp_gpr_sp / interp_set_gpr_sp). Rt/Rt2/Rm
//     keep the ordinary meaning where 31 is XZR.
//   * Every guest access goes through interp_load_bits / interp_store_bits, which memcpy, so unaligned
//     accesses work. The atomic/exclusive family REQUIRES natural alignment instead.
//   * Read sources into locals, then access, then write the base back: the fault path has nothing to undo.

#include "exclusive.c"

#define DEFINE_INTERP_LSE_MINMAX(name, type, signed_type)                                                           \
    static uint64_t name(void *pointer, uint64_t operand, int want_max, int is_signed) {                            \
        type *slot = (type *)pointer;                                                                               \
        type argument = (type)operand;                                                                              \
        type current = __atomic_load_n(slot, __ATOMIC_SEQ_CST);                                                     \
        for (;;) {                                                                                                  \
            int argument_greater =                                                                                  \
                is_signed ? ((signed_type)argument > (signed_type)current) : (argument > current);                  \
            type chosen = (argument_greater == (want_max != 0)) ? argument : current;                              \
            if (chosen == current) break;                                                                           \
            if (__atomic_compare_exchange_n(slot, &current, chosen, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) break; \
        }                                                                                                           \
        return (uint64_t)current;                                                                                   \
    }

DEFINE_INTERP_LSE_MINMAX(interp_lse_minmax_u8, uint8_t, int8_t)
DEFINE_INTERP_LSE_MINMAX(interp_lse_minmax_u16, uint16_t, int16_t)
DEFINE_INTERP_LSE_MINMAX(interp_lse_minmax_u32, uint32_t, int32_t)
DEFINE_INTERP_LSE_MINMAX(interp_lse_minmax_u64, uint64_t, int64_t)

#undef DEFINE_INTERP_LSE_MINMAX

#include "structure.c"

static int interp_exec_load_store_literal_pair(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    int rt = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rt2 = (int)((insn >> 10) & 31);
    unsigned vector = (insn >> 26) & 1;

    if ((insn & 0x3B000000u) == 0x18000000u) {
        unsigned opc = (insn >> 30) & 3;
        int64_t offset = interp_sext((insn >> 5) & 0x7FFFFu, 19) << 2;
        // pcrel_base, not the raw PC: a non-PIE image's architectural PC is its low link address.
        uint64_t address = pcrel_base(gpc) + (uint64_t)offset;
        if (vector) { // LDR St/Dt/Qt, literal
            if (opc == 3) return interp_undefined(cpu, insn, "loads and stores -- unallocated SIMD literal size");
            interp_vec_load(cpu, rt, address, 4u << opc);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (opc == 3) { // PRFM (literal): a hint
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (opc == 2) // LDRSW
            interp_set_gpr(cpu, rt, (uint64_t)interp_sext(interp_load_bits(address, 4), 32));
        else if (opc == 1) // LDR Xt
            interp_set_gpr(cpu, rt, interp_load_bits(address, 8));
        else // LDR Wt
            interp_set_gpr32(cpu, rt, (uint32_t)interp_load_bits(address, 4));
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    if ((insn & 0x3A000000u) == 0x28000000u) {
        unsigned opc = (insn >> 30) & 3, load = (insn >> 22) & 1, mode = (insn >> 23) & 3;
        if (vector) { // STP/LDP of two S, D or Q registers
            if (opc == 3) return interp_undefined(cpu, insn, "loads and stores -- unallocated SIMD pair opc");
            unsigned element = 4u << opc; // opc 0/1/2 -> 4, 8, 16 bytes per register
            int64_t vector_offset = interp_sext((insn >> 15) & 0x7Fu, 7) * (int64_t)element;
            uint64_t vector_base = interp_gpr_sp(cpu, rn);
            int vector_writeback = mode == 1 || mode == 3;
            uint64_t vector_address = mode == 1 ? vector_base : vector_base + (uint64_t)vector_offset;
            if (load) {
                interp_vec_load(cpu, rt, vector_address, element);
                interp_vec_load(cpu, rt2, vector_address + element, element);
            } else {
                interp_vec_store(cpu, rt, vector_address, element);
                interp_vec_store(cpu, rt2, vector_address + element, element);
            }
            if (vector_writeback) interp_set_gpr_sp(cpu, rn, vector_base + (uint64_t)vector_offset);
            cpu->pc = gpc + 4;
            return INTERP_NEXT;
        }
        if (opc == 3) return interp_undefined(cpu, insn, "loads and stores -- unallocated pair opc");
        if (opc == 1 && !load) return interp_undefined(cpu, insn, "loads and stores -- STGP (memory tagging)");
        unsigned bytes = opc == 2 ? 8u : 4u;
        unsigned scale = opc == 2 ? 3u : 2u;
        int64_t offset = interp_sext((insn >> 15) & 0x7Fu, 7) << scale;
        uint64_t base = interp_gpr_sp(cpu, rn);
        // mode 0 = LDNP/STNP, 1 = post-index, 2 = signed offset, 3 = pre-index. Only 1 uses the OLD base.
        int writeback = mode == 1 || mode == 3;
        uint64_t address = mode == 1 ? base : base + (uint64_t)offset;
        if (load) {
            uint64_t first = interp_load_bits(address, bytes);
            uint64_t second = interp_load_bits(address + bytes, bytes);
            if (opc == 1) { // LDPSW: two 32-bit loads, sign-extended
                interp_set_gpr(cpu, rt, (uint64_t)interp_sext(first, 32));
                interp_set_gpr(cpu, rt2, (uint64_t)interp_sext(second, 32));
            } else if (bytes == 8) {
                interp_set_gpr(cpu, rt, first);
                interp_set_gpr(cpu, rt2, second);
            } else {
                interp_set_gpr32(cpu, rt, (uint32_t)first);
                interp_set_gpr32(cpu, rt2, (uint32_t)second);
            }
        } else {
            uint64_t first = interp_gpr(cpu, rt), second = interp_gpr(cpu, rt2);
            interp_store_bits(address, first, bytes);
            interp_store_bits(address + bytes, second, bytes);
        }
        // Writeback LAST: Rn as a transfer register too is CONSTRAINED UNPREDICTABLE; last is what cores do.
        if (writeback) interp_set_gpr_sp(cpu, rn, base + (uint64_t)offset);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    return interp_undefined(cpu, insn, "loads and stores -- unallocated literal/pair encoding");
}

static int interp_exec_load_store_atomic(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    int rt = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
    unsigned vector = (insn >> 26) & 1;

    // LDAPR (FEAT_LRCPC) sits inside the LSE atomic box below, so it must be recognised BEFORE that
    // decode. RCpc and RCsc come out the same here: SEQ_CST is stronger than either.
    if ((insn & 0x3FFFFC00u) == 0x38BFC000u && !vector) {
        unsigned bytes = 1u << ((insn >> 30) & 3);
        uint64_t value = interp_load_bits(interp_gpr_sp(cpu, rn), bytes);
        __atomic_thread_fence(__ATOMIC_SEQ_CST);
        if (bytes == 8)
            interp_set_gpr(cpu, rt, value);
        else
            interp_set_gpr32(cpu, rt, (uint32_t)value);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    // Atomic memory operations (LSE), sharing the register-offset box; bits[11:10] == 00 selects them.
    if ((insn & 0x3B200C00u) == 0x38200000u) {
        unsigned size = (insn >> 30) & 3, opc = (insn >> 12) & 7, o3 = (insn >> 15) & 1;
        int rs = rm;
        unsigned bytes = 1u << size;
        if (vector) return interp_undefined(cpu, insn, "loads and stores -- SIMD/FP atomic");
        uint64_t address = interp_gpr_sp(cpu, rn);
        void *pointer = interp_atomic_pointer(address, bytes);
        if (pointer == NULL) return interp_alignment_fault(cpu, address);
        uint64_t operand = interp_gpr(cpu, rs), old = 0;
        // Real host read-modify-writes, not load-then-store: an interleaved peer would lose an update.
        interp_access_begin(address, bytes, 1);
#define INTERP_LSE_RMW(type, expression)                                                                               \
    do {                                                                                                               \
        type *slot = (type *)pointer;                                                                                  \
        type argument = (type)operand;                                                                                 \
        (void)argument;                                                                                                \
        old = (uint64_t)(expression);                                                                                  \
    } while (0)
#define INTERP_LSE_WIDTHS(expression8, expression16, expression32, expression64)                                       \
    do {                                                                                                               \
        switch (bytes) {                                                                                               \
        case 1: INTERP_LSE_RMW(uint8_t, expression8); break;                                                           \
        case 2: INTERP_LSE_RMW(uint16_t, expression16); break;                                                         \
        case 4: INTERP_LSE_RMW(uint32_t, expression32); break;                                                         \
        default: INTERP_LSE_RMW(uint64_t, expression64); break;                                                        \
        }                                                                                                              \
    } while (0)
        if (o3) { // SWP
            if (opc != 0) {
                interp_access_end();
                return interp_undefined(cpu, insn, "loads and stores -- unallocated LSE swap/op3 encoding");
            }
            INTERP_LSE_WIDTHS(__atomic_exchange_n(slot, argument, __ATOMIC_SEQ_CST),
                              __atomic_exchange_n(slot, argument, __ATOMIC_SEQ_CST),
                              __atomic_exchange_n(slot, argument, __ATOMIC_SEQ_CST),
                              __atomic_exchange_n(slot, argument, __ATOMIC_SEQ_CST));
        } else {
            switch (opc) {
            case 0: // LDADD
                INTERP_LSE_WIDTHS(__atomic_fetch_add(slot, argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_add(slot, argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_add(slot, argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_add(slot, argument, __ATOMIC_SEQ_CST));
                break;
            case 1: // LDCLR: bit CLEAR, so the operand is complemented
                INTERP_LSE_WIDTHS(__atomic_fetch_and(slot, (uint8_t)~argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_and(slot, (uint16_t)~argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_and(slot, (uint32_t)~argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_and(slot, (uint64_t)~argument, __ATOMIC_SEQ_CST));
                break;
            case 2: // LDEOR
                INTERP_LSE_WIDTHS(__atomic_fetch_xor(slot, argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_xor(slot, argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_xor(slot, argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_xor(slot, argument, __ATOMIC_SEQ_CST));
                break;
            case 3: // LDSET
                INTERP_LSE_WIDTHS(__atomic_fetch_or(slot, argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_or(slot, argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_or(slot, argument, __ATOMIC_SEQ_CST),
                                  __atomic_fetch_or(slot, argument, __ATOMIC_SEQ_CST));
                break;
            case 4:   // LDSMAX
            case 5:   // LDSMIN
            case 6:   // LDUMAX
            case 7: { // LDUMIN
                // No __atomic_fetch_max, so these are a CAS retry loop: a load-compare-store would let a
                // peer's update land in between. Comparison is at the ACCESS width and signedness.
                unsigned want_max = opc == 4 || opc == 6;
                unsigned is_signed = opc < 6;
                switch (bytes) {
                case 1: old = interp_lse_minmax_u8(pointer, operand, want_max, is_signed); break;
                case 2: old = interp_lse_minmax_u16(pointer, operand, want_max, is_signed); break;
                case 4: old = interp_lse_minmax_u32(pointer, operand, want_max, is_signed); break;
                default: old = interp_lse_minmax_u64(pointer, operand, want_max, is_signed); break;
                }
                break;
            }
            default:
                interp_access_end();
                return interp_undefined(cpu, insn, "loads and stores -- unallocated LSE atomic opcode");
            }
        }
#undef INTERP_LSE_WIDTHS
#undef INTERP_LSE_RMW
        interp_access_end();
        // Rt receives the PRE-operation value; Rt == 31 is the ST<op> alias, which discards it.
        if (bytes == 8)
            interp_set_gpr(cpu, rt, old);
        else
            interp_set_gpr32(cpu, rt, (uint32_t)old);
        cpu->pc = gpc + 4;
        return INTERP_NEXT;
    }

    return interp_undefined(cpu, insn, "loads and stores -- unallocated atomic encoding");
}

static int interp_exec_load_store_single(struct cpu *cpu, uint32_t insn) {
    uint64_t gpc = cpu->pc;
    int rt = (int)(insn & 31), rn = (int)((insn >> 5) & 31), rm = (int)((insn >> 16) & 31);
    unsigned vector = (insn >> 26) & 1;

    // The three single-register integer addressing modes, sharing one size/opc layout:
    //   opc == 0  store of (1 << size) bytes        1  zero-extending load
    //   opc == 2  sign-extending load into Xt       3  sign-extending load into Wt   (2 with size 3 is PRFM)
    unsigned size = (insn >> 30) & 3;
    unsigned opc = (insn >> 22) & 3;
    int scaled = (insn & 0x3B000000u) == 0x39000000u;
    int register_offset = (insn & 0x3B200C00u) == 0x38200800u;
    int unscaled = (insn & 0x3B200000u) == 0x38000000u;
    if (!scaled && !register_offset && !unscaled)
        return interp_undefined(cpu, insn, "loads and stores -- AdvSIMD structure or unallocated encoding");
    if (!vector && ((insn & 0x3B200C00u) == 0x38200400u || (insn & 0x3B200C00u) == 0x38200C00u))
        return interp_undefined(cpu, insn, "loads and stores -- LDRAA/LDRAB (pointer authentication)");

    unsigned bytes = vector ? interp_simd_access_bytes(size, opc) : (1u << size);
    if (vector && bytes == 0) return interp_undefined(cpu, insn, "loads and stores -- unallocated SIMD/FP access size");
    unsigned scale = vector ? (opc & 2u ? 4u : size) : size;
    uint64_t base = interp_gpr_sp(cpu, rn);
    uint64_t address;
    int writeback = 0;
    uint64_t writeback_value = 0;
    if (scaled) {
        address = base + (((uint64_t)((insn >> 10) & 0xFFFu)) << scale);
    } else if (register_offset) {
        unsigned option = (insn >> 13) & 7, s = (insn >> 12) & 1;
        // S scales the index by the access size; only options 010/011/110/111 are allocated here.
        if ((option & 3u) < 2u)
            return interp_undefined(cpu, insn, "loads and stores -- unallocated register-offset extend option");
        address = base + interp_extend_operand(cpu, rm, option, s ? scale : 0u, 1);
    } else {
        unsigned mode = (insn >> 10) & 3;
        int64_t offset = interp_sext((insn >> 12) & 0x1FFu, 9);
        // mode 0 = LDUR/STUR, 1 = post-index, 2 = LDTR/STTR (at EL0 the same as 0), 3 = pre-index.
        writeback = mode == 1 || mode == 3;
        writeback_value = base + (uint64_t)offset;
        address = mode == 1 ? base : base + (uint64_t)offset;
    }

    if (vector) {
        if (opc & 1u)
            interp_vec_load(cpu, rt, address, bytes);
        else
            interp_vec_store(cpu, rt, address, bytes);
    } else if (opc == 0) {                    // store
        uint64_t value = interp_gpr(cpu, rt); // source read before the access
        interp_store_bits(address, value, bytes);
    } else if (opc == 2 && size == 3) { // PRFM / PRFUM: a hint
        (void)0;
    } else if (opc == 1) { // zero-extending load
        uint64_t value = interp_load_bits(address, bytes);
        if (size == 3)
            interp_set_gpr(cpu, rt, value);
        else
            interp_set_gpr32(cpu, rt, (uint32_t)value); // a 32-bit destination zero-extends to 64 anyway
    } else {                                            // sign-extending load: LDRSB / LDRSH / LDRSW
        if (size == 3 || (size == 2 && opc == 3))
            return interp_undefined(cpu, insn, "loads and stores -- unallocated sign-extending load size");
        uint64_t value = (uint64_t)interp_sext(interp_load_bits(address, bytes), bytes * 8u);
        if (opc == 2) // 64-bit destination
            interp_set_gpr(cpu, rt, value);
        else // 32-bit destination
            interp_set_gpr32(cpu, rt, (uint32_t)value);
    }
    if (writeback) interp_set_gpr_sp(cpu, rn, writeback_value);
    cpu->pc = gpc + 4;
    return INTERP_NEXT;
}

static int interp_exec_load_store(struct cpu *cpu, uint32_t insn) {
    if ((insn & 0xBF200000u) == 0x0C000000u || (insn & 0xBF000000u) == 0x0D000000u)
        return interp_exec_load_store_structures(cpu, insn);
    if ((insn & 0x3B000000u) == 0x18000000u || (insn & 0x3A000000u) == 0x28000000u)
        return interp_exec_load_store_literal_pair(cpu, insn);
    if ((insn & 0x3F000000u) == 0x08000000u) return interp_exec_load_store_exclusive(cpu, insn);
    if ((insn & 0x3FFFFC00u) == 0x38BFC000u || (insn & 0x3B200C00u) == 0x38200000u)
        return interp_exec_load_store_atomic(cpu, insn);
    return interp_exec_load_store_single(cpu, insn);
}
