static uint64_t interp_call_return_pc(uint64_t pc) {
    return pc;
}

static uint64_t interp_ea(const struct cpu *cpu, const struct insn *insn, uint64_t next);

// A non-PIE rip-relative LEA must yield the LOW link address: it MATERIALISES a pointer compared against
// the image's baked LOW pointers, and a HIGH value silently disagrees -- glibc's __malloc_fork_lock_parent
// then self-deadlocks on main_arena.mutex. Un-biases materialisation only; ACCESSES stay rebiased by
// hl_x86_guest_pointer. Guards match lower/mov.c: 64-bit opsize (32-bit
// truncates to the low value anyway); rip-relative; target inside the link range.
static uint64_t interp_lea_value(const struct cpu *cpu, const struct insn *insn, uint64_t next) {
    if (insn->opsize == 8 && insn->rip_rel && g_nonpie_lo) {
        uint64_t link_target = next + (uint64_t)insn->disp;
        if (link_target >= g_nonpie_lo && link_target < g_nonpie_hi) return link_target;
    }
    return interp_ea(cpu, insn, next);
}

// ---- The flag substrate: x86 EFLAGS on ARM NZCV in cpu->nzcv plus side lanes, fixed by the checkpoint
// format and signal.c's converters.
//   bit 31 N = SF, bit 30 Z = ZF, bit 28 V = OF, bit 29 C = NOT x86 CF (ARM's borrow convention)
//   cpu->pf      a BYTE whose EVEN PARITY is x86 PF; ops store the low byte of the result
//   cpu->af      (a ^ b ^ result) of the last add/sub-shaped op; x86 AF is bit 4. Logical ops store 0.
//   cpu->df      x86 DF, 0 = forward. Runtime state, so a cross-block `std` is honoured.
//   cpu->idflag  x86 RFLAGS.ID (bit 21), the bit 32-bit CPUID probes flip.
// The C inversion is the classic way an x86 interpreter passes its own tests and fails on hardware.

#define NZ_N (UINT64_C(1) << 31)
#define NZ_Z (UINT64_C(1) << 30)
#define NZ_C (UINT64_C(1) << 29)
#define NZ_V (UINT64_C(1) << 28)

static uint64_t interp_mask(int width) {
    return width == 8 ? UINT64_MAX : ((UINT64_C(1) << (8 * width)) - 1);
}

static unsigned interp_msb(uint64_t value, int width) {
    return (unsigned)((value >> (8 * width - 1)) & 1);
}

static void interp_flags_nzcv(struct cpu *cpu, unsigned sf, unsigned zf, unsigned x86_cf, unsigned of) {
    uint64_t nzcv = 0;
    if (sf) nzcv |= NZ_N;
    if (zf) nzcv |= NZ_Z;
    if (!x86_cf) nzcv |= NZ_C; // stored C is the INVERSE of x86 CF
    if (of) nzcv |= NZ_V;
    cpu->nzcv = nzcv;
}

static unsigned interp_cf(const struct cpu *cpu) {
    return (unsigned)(((cpu->nzcv >> 29) & 1) ^ 1);
}

static void interp_set_cf(struct cpu *cpu, unsigned x86_cf) {
    if (x86_cf)
        cpu->nzcv &= ~NZ_C;
    else
        cpu->nzcv |= NZ_C;
}

static unsigned interp_pf(const struct cpu *cpu) {
    return (unsigned)(__builtin_parity((unsigned)(cpu->pf & 0xff)) ^ 1); // even parity -> PF=1
}

static uint64_t interp_alu_add(struct cpu *cpu, uint64_t a, uint64_t b, unsigned carry_in, int width) {
    uint64_t m = interp_mask(width);
    unsigned bits = (unsigned)(8 * width);
    unsigned __int128 wide = (unsigned __int128)(a & m) + (b & m) + carry_in;
    uint64_t result = (uint64_t)wide & m;
    unsigned cf = (unsigned)((wide >> bits) & 1);
    // OF: both inputs agree in sign and the result disagrees with them.
    unsigned of = (unsigned)((((a ^ result) & (b ^ result)) >> (bits - 1)) & 1);
    interp_flags_nzcv(cpu, interp_msb(result, width), result == 0, cf, of);
    cpu->pf = result & 0xff;
    cpu->af = a ^ b ^ result;
    return result;
}

static uint64_t interp_alu_sub(struct cpu *cpu, uint64_t a, uint64_t b, unsigned borrow_in, int width) {
    uint64_t m = interp_mask(width);
    unsigned bits = (unsigned)(8 * width);
    unsigned __int128 wide = (unsigned __int128)(a & m) - (b & m) - borrow_in;
    uint64_t result = (uint64_t)wide & m;
    unsigned cf = (unsigned)((wide >> bits) & 1); // the subtraction went negative -> borrow
    // OF: the inputs disagree in sign and the result disagrees with the minuend.
    unsigned of = (unsigned)((((a ^ b) & (a ^ result)) >> (bits - 1)) & 1);
    interp_flags_nzcv(cpu, interp_msb(result, width), result == 0, cf, of);
    cpu->pf = result & 0xff;
    cpu->af = a ^ b ^ result;
    return result;
}

// AND/OR/XOR/TEST: CF and OF clear, AF undefined (0, matching the JIT).
static void interp_flags_logic(struct cpu *cpu, uint64_t result, int width) {
    interp_flags_nzcv(cpu, interp_msb(result, width), result == 0, 0, 0);
    cpu->pf = result & 0xff;
    cpu->af = 0;
}

// ADD/SUB of 1 except that CF is left UNTOUCHED.
static uint64_t interp_alu_incdec(struct cpu *cpu, uint64_t a, int decrement, int width) {
    unsigned cf = interp_cf(cpu);
    uint64_t result = decrement ? interp_alu_sub(cpu, a, 1, 0, width) : interp_alu_add(cpu, a, 1, 0, width);
    interp_set_cf(cpu, cf);
    return result;
}

// Indexed by the low nibble of a Jcc/SETcc/CMOVcc opcode.
static int interp_cond(const struct cpu *cpu, int code) {
    unsigned cf = interp_cf(cpu);
    unsigned zf = (unsigned)((cpu->nzcv >> 30) & 1);
    unsigned sf = (unsigned)((cpu->nzcv >> 31) & 1);
    unsigned of = (unsigned)((cpu->nzcv >> 28) & 1);
    switch (code & 0xf) {
    case 0x0: return (int)of;                 // O
    case 0x1: return (int)!of;                // NO
    case 0x2: return (int)cf;                 // B / NAE / C
    case 0x3: return (int)!cf;                // AE / NB / NC
    case 0x4: return (int)zf;                 // E / Z
    case 0x5: return (int)!zf;                // NE / NZ
    case 0x6: return (int)(cf || zf);         // BE / NA
    case 0x7: return (int)(!cf && !zf);       // A / NBE
    case 0x8: return (int)sf;                 // S
    case 0x9: return (int)!sf;                // NS
    case 0xa: return (int)interp_pf(cpu);     // P / PE
    case 0xb: return (int)!interp_pf(cpu);    // NP / PO
    case 0xc: return (int)(sf != of);         // L / NGE
    case 0xd: return (int)(sf == of);         // GE / NL
    case 0xe: return (int)(zf || (sf != of)); // LE / NG
    default: return (int)(!zf && (sf == of)); // G / NLE
    }
}

// IF reads 1 -- a guest always observes interrupts enabled. Mirrors the JIT's pushfq lowering bit for bit.
static uint64_t interp_read_rflags(const struct cpu *cpu) {
    uint64_t flags = hl_x86_signal_nzcv_to_eflags(cpu->nzcv);
    flags |= UINT64_C(1) << 9;                     // IF
    flags |= (cpu->df & 1) << 10;                  // DF
    if (interp_pf(cpu)) flags |= UINT64_C(1) << 2; // PF
    flags |= ((cpu->af >> 4) & 1) << 4;            // AF
    flags |= (cpu->idflag & 1) << 21;              // ID
    return flags;
}

static void interp_write_rflags(struct cpu *cpu, uint64_t flags) {
    cpu->nzcv = hl_x86_signal_eflags_to_nzcv(flags);
    cpu->df = (flags >> 10) & 1;
    cpu->idflag = (flags >> 21) & 1;
    cpu->af = ((flags >> 4) & 1) << 4;
    // 0 has even parity (PF=1), 1 has odd (PF=0).
    cpu->pf = ((flags >> 2) & 1) ^ 1u;
}

// ---- Register and r/m operand access.

// WITHOUT REX, byte register numbers 4..7 name AH/CH/DH/BH, not the low byte of rSP/rBP/rSI/rDI.
static int interp_hi8(const struct insn *insn, int number, int width) {
    return width == 1 && !insn->has_rex && number >= 4 && number <= 7;
}

static uint64_t interp_reg_read(const struct cpu *cpu, const struct insn *insn, int number, int width) {
    if (interp_hi8(insn, number, width)) return (cpu->r[number - 4] >> 8) & 0xff;
    return cpu->r[number] & interp_mask(width);
}

static void interp_reg_write(struct cpu *cpu, const struct insn *insn, int number, int width, uint64_t value) {
    if (interp_hi8(insn, number, width)) {
        cpu->r[number - 4] = (cpu->r[number - 4] & ~UINT64_C(0xff00)) | ((value & 0xff) << 8);
        return;
    }
    switch (width) {
    // A byte or word write MERGES into the surrounding bits.
    case 1: cpu->r[number] = (cpu->r[number] & ~UINT64_C(0xff)) | (value & 0xff); break;
    case 2: cpu->r[number] = (cpu->r[number] & ~UINT64_C(0xffff)) | (value & 0xffff); break;
    // A 32-bit write ZERO-EXTENDS; stale high bits surface later as a wild pointer.
    case 4: cpu->r[number] = value & UINT64_C(0xffffffff); break;
    default: cpu->r[number] = value; break;
    }
}

// In GUEST coordinates: LEA must yield what the guest computes; the rebias happens at dereference.
static uint64_t interp_ea(const struct cpu *cpu, const struct insn *insn, uint64_t next) {
    uint64_t address;
    if (insn->rip_rel) {
        // Measured from the END of the instruction, hence `next`.
        address = next + (uint64_t)insn->disp;
    } else {
        address = 0;
        if (insn->m_hasbase) address += cpu->r[insn->m_base];
        if (insn->m_hasindex) address += cpu->r[insn->m_index] << insn->m_scale;
        address += (uint64_t)insn->disp;
    }
    // 0x67: modulo 2^32, so one mask at the end suffices.
    if (insn->addr32) address &= UINT64_C(0xffffffff);
    if (insn->seg == 1)
        address += cpu->fs_base; // %fs: TLS (arch_prctl SET_FS)
    else if (insn->seg == 2)
        address += cpu->gs_base;
    return address;
}

// The implicit-operand forms (XLATB's RBX+AL, MASKMOVDQU's RDI) name a base register with no ModRM, so
// interp_ea cannot serve them. Prefixes apply the same way: 0x67 wraps the guest-computed address at 32
// bits, and the segment base is added after that wrap.
static uint64_t interp_implicit_address(const struct cpu *cpu, const struct insn *insn, uint64_t address) {
    if (insn->addr32) address &= UINT64_C(0xffffffff);
    if (insn->seg == 1)
        address += cpu->fs_base;
    else if (insn->seg == 2)
        address += cpu->gs_base;
    return address;
}

typedef struct interp_operand {
    int is_memory;
    uint64_t address; // valid when is_memory
    int number;       // register number when !is_memory
} interp_operand;

static interp_operand interp_rm(const struct cpu *cpu, const struct insn *insn, uint64_t next) {
    interp_operand operand;
    operand.is_memory = insn->is_mem;
    operand.address = insn->is_mem ? interp_ea(cpu, insn, next) : 0;
    operand.number = insn->is_mem ? 0 : insn->rm_reg;
    return operand;
}

static uint64_t interp_rm_read(const struct cpu *cpu, const struct insn *insn, const interp_operand *operand,
                               int width) {
    if (operand->is_memory) return interp_load(operand->address, width);
    return interp_reg_read(cpu, insn, operand->number, width);
}

static void interp_rm_write(struct cpu *cpu, const struct insn *insn, const interp_operand *operand, int width,
                            uint64_t value) {
    if (operand->is_memory)
        interp_store(operand->address, width, value);
    else
        interp_reg_write(cpu, insn, operand->number, width, value);
}

// ---- Atomic read-modify-write for LOCK-prefixed instructions (and memory XCHG, implicitly locked). The
// unaligned split-lock case, which x86 permits and ARM's LSE atomics refuse, falls back to cmpxchg.c's
// hashed spinlock.

#define INTERP_SPLIT_LOCKS 256
// Acquired and released with the C11 <stdatomic.h> spelling rather than the __atomic_* builtins the
// rest of this file uses on PLAIN objects. The distinction is the object, not the host: these words
// are _Atomic-qualified, and a compiler is entitled to reject an __atomic_* builtin applied to an
// _Atomic lvalue (clang 22 does, with "address argument to atomic operation must be a pointer to
// integer or pointer"). The generated code is identical; only the type check differs.
static _Atomic unsigned g_interp_split_lock[INTERP_SPLIT_LOCKS];

enum interp_rmw_kind {
    RMW_ADD,
    RMW_OR,
    RMW_ADC,
    RMW_SBB,
    RMW_AND,
    RMW_SUB,
    RMW_XOR,
    RMW_CMP, // LOCK CMP is legal and read-only
    RMW_NOT,
    RMW_NEG,
    RMW_INC,
    RMW_DEC,
    RMW_XCHG,
    RMW_BTS,
    RMW_BTR,
    RMW_BTC
};

static uint64_t interp_rmw_apply(enum interp_rmw_kind kind, uint64_t old, uint64_t operand, unsigned carry_in,
                                 int width) {
    uint64_t m = interp_mask(width);
    switch (kind) {
    case RMW_ADD: return (old + operand) & m;
    case RMW_OR: return (old | operand) & m;
    case RMW_ADC: return (old + operand + carry_in) & m;
    case RMW_SBB: return (old - operand - carry_in) & m;
    case RMW_AND: return (old & operand) & m;
    case RMW_SUB: return (old - operand) & m;
    case RMW_XOR: return (old ^ operand) & m;
    case RMW_CMP: return old & m;
    case RMW_NOT: return (~old) & m;
    case RMW_NEG: return (UINT64_C(0) - old) & m;
    case RMW_INC: return (old + 1) & m;
    case RMW_DEC: return (old - 1) & m;
    case RMW_BTS: return (old | operand) & m;
    case RMW_BTR: return (old & ~operand) & m;
    case RMW_BTC: return (old ^ operand) & m;
    default: return operand & m; // RMW_XCHG
    }
}

// Returns the PRE-image, so flags match what a non-locked path would have seen.
static uint64_t interp_locked_rmw(uint64_t guest_address, int width, enum interp_rmw_kind kind, uint64_t operand,
                                  unsigned carry_in) {
    uint64_t host_address = hl_x86_guest_pointer(guest_address);
    void *pointer = (void *)(uintptr_t)host_address;
    uint64_t old = 0;
    if ((host_address & (uint64_t)(width - 1)) == 0) {
        interp_access_begin(guest_address, (uint64_t)width);
        switch (width) {
        case 1: {
            unsigned char *p = pointer;
            unsigned char expected = __atomic_load_n(p, __ATOMIC_SEQ_CST), desired;
            do {
                desired = (unsigned char)interp_rmw_apply(kind, expected, operand, carry_in, width);
            } while (!__atomic_compare_exchange_n(p, &expected, desired, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST));
            old = expected;
            break;
        }
        case 2: {
            unsigned short *p = pointer;
            unsigned short expected = __atomic_load_n(p, __ATOMIC_SEQ_CST), desired;
            do {
                desired = (unsigned short)interp_rmw_apply(kind, expected, operand, carry_in, width);
            } while (!__atomic_compare_exchange_n(p, &expected, desired, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST));
            old = expected;
            break;
        }
        case 4: {
            uint32_t *p = pointer;
            uint32_t expected = __atomic_load_n(p, __ATOMIC_SEQ_CST), desired;
            do {
                desired = (uint32_t)interp_rmw_apply(kind, expected, operand, carry_in, width);
            } while (!__atomic_compare_exchange_n(p, &expected, desired, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST));
            old = expected;
            break;
        }
        default: {
            uint64_t *p = pointer;
            uint64_t expected = __atomic_load_n(p, __ATOMIC_SEQ_CST), desired;
            do {
                desired = interp_rmw_apply(kind, expected, operand, carry_in, width);
            } while (!__atomic_compare_exchange_n(p, &expected, desired, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST));
            old = expected;
            break;
        }
        }
        interp_access_end();
    } else {
        // Hashed: the same bytes serialise, unrelated sites do not contend.
        unsigned hash = (unsigned)((host_address >> 3) & (INTERP_SPLIT_LOCKS - 1));
        _Atomic unsigned *lock = &g_interp_split_lock[hash];
        uint64_t next_value;
        while (atomic_exchange_explicit(lock, 1u, memory_order_acquire))
            ; // an exchange always makes forward progress
        interp_access_begin(guest_address, (uint64_t)width);
        interp_copy_indivisible(&old, pointer, (unsigned)width);
        next_value = interp_rmw_apply(kind, old, operand, carry_in, width);
        interp_copy_indivisible(pointer, &next_value, (unsigned)width);
        interp_access_end();
        atomic_store_explicit(lock, 0u, memory_order_release);
    }
    if (kind != RMW_CMP && jit86_store_alias_observation_active())
        jit86_store_alias_changed(guest_address, (uint64_t)width);
    return old & interp_mask(width);
}

// ---- The block descriptor.

// One descriptor per translated guest PC, holding no decoded instructions: run_block re-decodes from guest
// memory every execution, which makes self-modifying guest code coherent by construction. Bump-allocated
// from the shared CODE ARENA (g_cp), not malloc'd, because the flush, the stop-the-world generation
// rotation, jit_publish_code, arena reclamation and jit_resolve_rw_code all reason about arena membership.
// The magic word and gpc make a stale pointer -- from a reclaimed arena, or host code out of a JIT-written
// persistent cache -- fail loudly at the first execution.
#define INTERP_BLOCK_MAGIC UINT64_C(0x496e74657270426b) // "InterpBk"

// host_entry_off is the whole of the transliterator's intrusion on this backend: 0 means "interpret this
// block", anything else is the offset to same-ISA host code emitted straight after the header. Both kinds
// live in one cache and the dispatcher cannot tell them apart, which is what makes the second backend
// strictly additive (translit.inc).
struct interp_block {
    uint64_t magic;
    uint64_t gpc;
    uint64_t generation; // diagnostic
    // Conservative source dependency hull. Ordinarily this is the byte range copied into host code. A
    // same-page immutable link expands it to the whole linked page, so invalidating any byte that could
    // stale the target also removes every source descriptor that can bypass the translation map to it.
    uint64_t guest_start;
    uint64_t guest_end;
    uint32_t host_entry_off;
    uint32_t host_len;
#if defined(HL_NATIVE_TEST_HOOKS)
    uint8_t profile_jcc_fall_stitches;
    uint8_t profile_fallback_kind;
    uint16_t profile_insns;
    uint64_t profile_fallback_form;
#endif
};

static void *translate_block(hl_x86_hot_context *context, uint64_t gpc);
#if defined(HL_NATIVE_TEST_HOOKS)
static _Atomic int translit_test_commit_gap;
static _Atomic int translit_test_gap_registered;
static _Atomic int translit_test_gap_payload;
static _Atomic int translit_test_gap_early;
static pthread_t translit_test_gap_thread;

static void *translit_test_gap_writer(void *unused) {
    (void)unused;
    int begun = hl_guest_fetch_authority_test_global_begin_observed(&translit_test_gap_registered);
    atomic_store_explicit(&translit_test_gap_payload, 1, memory_order_release);
    hl_guest_fetch_authority_end(begun);
    return NULL;
}
#endif

#include "../translit.inc"

// Must return a distinct non-NULL pointer per guest PC: non-NULL from map_host() suppresses re-translation.
static void *translate_block(hl_x86_hot_context *context, uint64_t gpc) {
    // Pick up writes made through another MAP_SHARED alias before reading an emulated executable view.
    uint64_t source_page = gpc & ~UINT64_C(0xfff);
    filemap_refresh_emulated(source_page, source_page + UINT64_C(0x1000));
    HL_LOGF(&g_jit_log, HL_LOG_TAG_TRANSLATE, "isa=x86_64 interp guest_pc=%#llx", (unsigned long long)gpc);
    // The compact unresolved-JCC path is optional. If its generation-owned
    // shared stub cannot be established, translit_build keeps using the
    // ordinary dispatcher path; this function must still return its unique
    // interpreter descriptor.
    if (translit_enabled()) {
        (void)translit_jcc_ibtc_stub_init();
        (void)translit_direct_jmp_ibtc_stub_init();
    }
    while ((uintptr_t)g_cp & 15)
        g_cp++;
    struct interp_block *block = (struct interp_block *)g_cp;
    g_cp += sizeof *block;
    block->magic = INTERP_BLOCK_MAGIC;
    block->gpc = gpc;
    block->generation = g_cache_gen;
    block->guest_start = gpc;
    block->guest_end = gpc + 1;
    block->host_entry_off = 0;
    block->host_len = 0;
#if defined(HL_NATIVE_TEST_HOOKS)
    block->profile_insns = 0;
    block->profile_jcc_fall_stitches = 0;
    block->profile_fallback_kind = HL_BACKEND_SHAPE_I_OTHER;
    block->profile_fallback_form = 0;
#endif
    uint64_t jcc_ibtc_sites = 0;
    (void)hl_x86_decode_transaction_begin(context);
    (void)translit_build(context, block, gpc, &jcc_ibtc_sites);
    if (hl_x86_decode_transaction_rejected(context)) {
        /* Nothing was published: body bytes remain behind g_cp, owner and
           instruction maps are untouched, and this descriptor executes by
           re-decoding through the ordinary validated interpreter path. */
        hl_x86_decode_transaction_abort(context);
    }
#if defined(HL_NATIVE_TEST_HOOKS)
    int gap_test = atomic_exchange_explicit(&translit_test_commit_gap, 0, memory_order_acq_rel);
    if (gap_test) {
        atomic_store_explicit(&translit_test_gap_registered, 0, memory_order_relaxed);
        atomic_store_explicit(&translit_test_gap_payload, 0, memory_order_relaxed);
        atomic_store_explicit(&translit_test_gap_early, 0, memory_order_relaxed);
        if (pthread_create(&translit_test_gap_thread, NULL, translit_test_gap_writer, NULL) != 0)
            atomic_store_explicit(&translit_test_gap_payload, -1, memory_order_release);
        else
            while (!atomic_load_explicit(&translit_test_gap_registered, memory_order_acquire)) sched_yield();
    }
#endif
    // host == body (no prologue to skip). SOURCE range [gpc, guest_end) so SMC invalidation finds it by
    // address -- a transliterated block caches guest BYTES and so owns the range it copied, where an
    // interpreted one re-decodes and needs only its entry.
    map_put(gpc, block->guest_start, block->guest_end, block, block);
#if defined(HL_NATIVE_TEST_HOOKS)
    if (gap_test && atomic_load_explicit(&translit_test_gap_payload, memory_order_acquire) != 0)
        atomic_store_explicit(&translit_test_gap_early, 1, memory_order_release);
#endif
    translit_jcc_ibtc_count(TL_JCC_IBTC_COUNT_EMITTED, jcc_ibtc_sites);
    hl_x86_decode_transaction_release(context);
#if defined(HL_NATIVE_TEST_HOOKS)
    if (gap_test && atomic_load_explicit(&translit_test_gap_payload, memory_order_acquire) != -1)
        (void)pthread_join(translit_test_gap_thread, NULL);
#endif
    return block;
}

// No emitted back-edge to fold and no in-cache counter, so R_TIER2 is unreachable (interp_dispatch.h
// normalizes it to R_BRANCH). core/dispatch.c calls this unconditionally.
static void tier2_promote(uint64_t gpc) {
    (void)gpc;
}

// An ENGINE GAP, not a guest fault: a class this backend cannot execute stops the run loudly with reason 99
// (interp_dispatch.h -> exit 70), whereas a guest-caused #UD is a SIGILL delivered by interp_guest_trap --
// never route one here. cpu->rip stays EXACT (the JIT writes a 0xDEAD marker); bytes come through the guest
// fetch path, so reporting next to an unmapped page cannot itself fault.
static int interp_undefined(struct cpu *cpu, const struct insn *insn, uint64_t pc, const char *class_name) {
    uint8_t bytes[16] = {0};
    char text[96] = {0};
    char message[384];
    int length = (insn->len > 0 && insn->len <= 15) ? insn->len : 8;
    int used = 0;
    const char *map = insn->vex         ? (insn->evex ? "EVEX" : "VEX")
                      : insn->map3 == 2 ? "0F38"
                      : insn->map3 == 3 ? "0F3A"
                      : insn->two       ? "0F"
                                        : "1B";
    if (x86_guest_fetch_exec(pc, bytes, (size_t)length) != 0) length = 0;
    for (int index = 0; index < length && used < (int)sizeof text - 4; index++)
        used += snprintf(text + used, sizeof text - (size_t)used, "%02x ", bytes[index]);
    if (used > 0) text[used - 1] = 0;
    int written =
        snprintf(message, sizeof message,
                 "x86 interpreter unsupported class=%s pc=%#llx bytes=%s map=%s op=%#x modrm=%#x", class_name,
                 (unsigned long long)pc, text, map, (unsigned)insn->op, (unsigned)(insn->has_modrm ? insn->modrm : 0));
    if (written < 0) written = 0;
    if ((size_t)written >= sizeof message) written = (int)sizeof message - 1;
    (void)jit_fail(HL_STATUS_NOT_SUPPORTED, message, (size_t)written);
    cpu->rip = pc;
    cpu->reason = R_BRANCH;
    return 1; // STEP_END
}

// ---- The interpreter.

enum { STEP_NEXT = 0, STEP_END = 1 };

// Guest trap signal, as the JIT's emit_guest_signal: divop = (signo | si_code<<8), rip = the handler's PC.
static int interp_guest_trap(struct cpu *cpu, uint64_t rip, int signo, int si_code) {
    cpu->divop = (uint64_t)((signo & 0xff) | ((si_code & 0xff) << 8));
    cpu->rip = rip;
    cpu->reason = R_TRAP;
    return STEP_END;
}

static int interp_exit(struct cpu *cpu, uint64_t rip, uint64_t reason) {
    cpu->rip = rip;
    cpu->reason = reason;
    return STEP_END;
}

// Guest-access fault for an operand a helper touches AFTER the block returns -- the FXSAVE/FXRSTOR pair,
// whose 512 bytes are written by x87state.c from the dispatch loop, outside the pad above. Same R_SOFTMISS
// protocol the emitted memory guard uses (emit.c): the dispatcher either resolves a logical mapping and
// retries or delivers the guest SIGSEGV with the right si_addr. rip stays on the instruction.
static int interp_softmiss(struct cpu *cpu, uint64_t rip, uint64_t address, uint64_t width, uint32_t required) {
    cpu->bus_ea = address;
    cpu->soft_guest_ea = address;
    cpu->soft_width = width;
    cpu->soft_required = required;
    cpu->rip = rip;
    cpu->reason = R_SOFTMISS;
    return STEP_END;
}

// Push/pop default to 64-bit in long mode; insn->opsize follows REX.W, so ask here instead. 66 narrows the
// stack operand to 16 bits, but REX.W WINS over 66 (SDM vol 2 table 3-4): measured on silicon, `66 48 50`
// moves RSP by 8 and stores 8 bytes, where this moved it by 2.
static int interp_stack_width(const struct insn *insn) {
    return (insn->p66 && !insn->rexW) ? 2 : 8;
}

static void interp_push(struct cpu *cpu, uint64_t value, int width) {
    cpu->r[RSP] -= (uint64_t)width;
    interp_store(cpu->r[RSP], width, value);
}

static uint64_t interp_pop(struct cpu *cpu, int width) {
    uint64_t value = interp_load(cpu->r[RSP], width);
    cpu->r[RSP] += (uint64_t)width;
    return value;
}

// Flag traps: a zero effective count changes NO flags; the count masks to 5 bits (6 at 64-bit), so
// `shlb $8` shifts by 8 and yields 0; ROL/ROR touch only CF (plus OF at count==1), and write CF whenever
// the MASKED count is nonzero, even if count%width == 0.
static uint64_t interp_shift(struct cpu *cpu, int kind, uint64_t value, unsigned count_raw, int width) {
    uint64_t m = interp_mask(width);
    unsigned bits = (unsigned)(8 * width);
    unsigned count = count_raw & (width == 8 ? 63u : 31u);
    uint64_t original = value & m;
    uint64_t result = original;
    if (kind == 0 || kind == 1) { // ROL / ROR
        unsigned rotate = count % bits;
        if (rotate != 0)
            result = (kind == 0 ? ((original << rotate) | (original >> (bits - rotate)))
                                : ((original >> rotate) | (original << (bits - rotate)))) &
                     m;
        if (count != 0) {
            unsigned cf = kind == 0 ? (unsigned)(result & 1) : interp_msb(result, width);
            interp_set_cf(cpu, cf);
            if (count == 1) {
                unsigned of = kind == 0 ? (interp_msb(result, width) ^ cf)
                                        : (interp_msb(result, width) ^ (unsigned)((result >> (bits - 2)) & 1));
                cpu->nzcv = (cpu->nzcv & ~NZ_V) | ((uint64_t)of << 28);
            }
        }
        return result;
    }
    if (count == 0) return original; // no flags change
    unsigned cf, of = 0;
    if (kind == 4) { // SHL / SAL
        result = (count < bits ? (original << count) : 0) & m;
        cf = count <= bits ? (unsigned)((original >> (bits - count)) & 1) : 0u;
        of = interp_msb(result, width) ^ cf;
    } else if (kind == 5) { // SHR
        result = count < bits ? (original >> count) : 0;
        cf = count <= bits ? (unsigned)((original >> (count - 1)) & 1) : 0u;
        of = interp_msb(original, width);
    } else { // SAR (kind 7)
        int64_t signed_value = (int64_t)(original << (64 - bits)) >> (64 - bits);
        unsigned shift = count < bits ? count : bits - 1;
        result = (uint64_t)(signed_value >> shift) & m;
        cf = (unsigned)((signed_value >> (count > bits ? bits - 1 : count - 1)) & 1);
        of = 0; // SAR can never overflow
    }
    // Flags x86 leaves UNDEFINED are written exactly as the JIT writes them, everywhere in this file.
    // For shifts that means AF untouched.
    interp_flags_nzcv(cpu, interp_msb(result, width), result == 0, cf, of);
    cpu->pf = result & 0xff;
    return result;
}

// SHLD/SHRD: `fill` supplies the shifted-in bits.
static uint64_t interp_double_shift(struct cpu *cpu, int right, uint64_t value, uint64_t fill, unsigned count_raw,
                                    int width) {
    uint64_t m = interp_mask(width);
    unsigned bits = (unsigned)(8 * width);
    unsigned count = count_raw & (width == 8 ? 63u : 31u);
    uint64_t result, cf;
    if (count == 0 || count > bits) return value & m; // count > width: x86 leaves the result undefined
    if (right) {
        cf = (value >> (count - 1)) & 1;
        result = count == bits ? (fill & m) : ((((value & m) >> count) | ((fill & m) << (bits - count))) & m);
    } else {
        cf = (value >> (bits - count)) & 1;
        result = count == bits ? (fill & m) : ((((value & m) << count) | ((fill & m) >> (bits - count))) & m);
    }
    interp_flags_nzcv(cpu, interp_msb(result, width), result == 0, (unsigned)cf,
                      count == 1 ? (interp_msb(result, width) ^ interp_msb(value, width)) : 0u);
    cpu->pf = result & 0xff;
    return result;
}

// DIV / IDIV. #DE for a zero divisor and for a too-wide quotient both exit divop == 0, R_DIV/R_IDIV, at
// the FAULTING PC -- the JIT's convention. The 64-bit form is left to the dispatcher's 128/64 division.
static int interp_divide(struct cpu *cpu, uint64_t divisor, int width, int is_signed, uint64_t pc, uint64_t next) {
    uint64_t reason = is_signed ? R_IDIV : R_DIV;
    if (divisor == 0) {
        cpu->divop = 0;
        return interp_exit(cpu, pc, reason);
    }
    if (width == 8) {
        // Quotient overflow is decided HERE, not by the dispatcher's own overflow arm, for two reasons.
        // Linux reports FPE_INTDIV for the #DE trap whatever raised it -- verified against hardware, and
        // it is already what every narrower width below produces -- while that arm queues FPE_INTOVF. And
        // its signed check divides first, so RDX:RAX == INT128_MIN over -1 would trap the ENGINE's own
        // idiv before it could rule. divop == 0 is the #DE marker; the fault reports the DIV's own pc.
        int overflow;
        if (!is_signed) {
            overflow = cpu->r[RDX] >= divisor;
        } else if ((int64_t)divisor == -1) {
            __int128 numerator = ((__int128)(int64_t)cpu->r[RDX] << 64) | cpu->r[RAX];
            overflow = numerator < -(__int128)INT64_MAX || numerator > -(__int128)INT64_MIN;
        } else {
            __int128 numerator = ((__int128)(int64_t)cpu->r[RDX] << 64) | cpu->r[RAX];
            __int128 quotient = numerator / (int64_t)divisor;
            overflow = (__int128)(int64_t)quotient != quotient;
        }
        if (overflow) {
            cpu->divop = 0;
            return interp_exit(cpu, pc, reason);
        }
        cpu->divop = divisor;
        return interp_exit(cpu, next, reason); // the 128/64 division itself stays in the dispatcher
    }
    unsigned bits = (unsigned)(8 * width);
    uint64_t m = interp_mask(width);
    if (!is_signed) {
        // At byte width the dividend is AX alone, not DX:AX.
        uint64_t dividend = width == 1 ? (cpu->r[RAX] & 0xffff) : (((cpu->r[RDX] & m) << bits) | (cpu->r[RAX] & m));
        uint64_t quotient = dividend / (divisor & m);
        uint64_t remainder = dividend % (divisor & m);
        if (quotient > m) {
            cpu->divop = 0;
            return interp_exit(cpu, pc, reason);
        }
        if (width == 1) {
            cpu->r[RAX] = (cpu->r[RAX] & ~UINT64_C(0xffff)) | (quotient & 0xff) | ((remainder & 0xff) << 8);
        } else {
            uint64_t q = quotient & m, r = remainder & m;
            cpu->r[RAX] = width == 4 ? q : ((cpu->r[RAX] & ~m) | q);
            cpu->r[RDX] = width == 4 ? r : ((cpu->r[RDX] & ~m) | r);
        }
        cpu->rip = next;
        return STEP_NEXT;
    }
    int64_t signed_divisor = (int64_t)((divisor & m) << (64 - bits)) >> (64 - bits);
    int64_t dividend;
    if (width == 1) {
        dividend = (int64_t)(int16_t)(uint16_t)(cpu->r[RAX] & 0xffff);
    } else if (width == 2) {
        dividend = (int64_t)(int32_t)(uint32_t)(((cpu->r[RDX] & 0xffff) << 16) | (cpu->r[RAX] & 0xffff));
    } else {
        dividend = (int64_t)(((cpu->r[RDX] & 0xffffffff) << 32) | (cpu->r[RAX] & 0xffffffff));
    }
    if (signed_divisor == -1 && dividend == INT64_MIN) { // cannot happen for width<8, but be explicit
        cpu->divop = 0;
        return interp_exit(cpu, pc, reason);
    }
    int64_t quotient = dividend / signed_divisor;
    int64_t remainder = dividend % signed_divisor;
    int64_t low = (int64_t)(((uint64_t)quotient & m) << (64 - bits)) >> (64 - bits);
    if (low != quotient) { // quotient too wide -> #DE
        cpu->divop = 0;
        return interp_exit(cpu, pc, reason);
    }
    if (width == 1) {
        cpu->r[RAX] =
            (cpu->r[RAX] & ~UINT64_C(0xffff)) | ((uint64_t)quotient & 0xff) | (((uint64_t)remainder & 0xff) << 8);
    } else {
        uint64_t q = (uint64_t)quotient & m, r = (uint64_t)remainder & m;
        cpu->r[RAX] = width == 4 ? q : ((cpu->r[RAX] & ~m) | q);
        cpu->r[RDX] = width == 4 ? r : ((cpu->r[RDX] & ~m) | r);
    }
    cpu->rip = next;
    return STEP_NEXT;
}

// MUL / IMUL (widening). CF=OF when the high half is significant; N=Z=0, as the JIT's e_mul_set_oc.
static void interp_widening_multiply(struct cpu *cpu, const struct insn *insn, uint64_t source, int width,
                                     int is_signed) {
    uint64_t m = interp_mask(width);
    uint64_t low, high;
    unsigned overflow;
    if (is_signed) {
        __int128 a = (__int128)(int64_t)((cpu->r[RAX] & m) << (64 - 8 * width)) >> (64 - 8 * width);
        __int128 b = (__int128)(int64_t)((source & m) << (64 - 8 * width)) >> (64 - 8 * width);
        __int128 product = a * b;
        low = (uint64_t)product & m;
        high = (uint64_t)((unsigned __int128)product >> (8 * width)) & m;
        int64_t sign_extended_low = (int64_t)(low << (64 - 8 * width)) >> (64 - 8 * width);
        overflow = (unsigned)((__int128)sign_extended_low != product);
    } else {
        unsigned __int128 product = (unsigned __int128)(cpu->r[RAX] & m) * (source & m);
        low = (uint64_t)product & m;
        high = (uint64_t)(product >> (8 * width)) & m;
        overflow = high != 0;
    }
    if (width == 1) {
        // MUL r/m8 lands the whole 16-bit product in AX.
        cpu->r[RAX] = (cpu->r[RAX] & ~UINT64_C(0xffff)) | (low & 0xff) | ((high & 0xff) << 8);
    } else {
        interp_reg_write(cpu, insn, RAX, width, low);
        interp_reg_write(cpu, insn, RDX, width, high);
    }
    interp_flags_nzcv(cpu, 0, 0, overflow, overflow);
}

// Two/three-operand IMUL: CF=OF report that the untruncated product did not fit; SF/ZF from the result.
// PF/AF stay preserved, matching the translated path's deterministic undefined-flag convention.
static uint64_t interp_imul_truncating(struct cpu *cpu, uint64_t a, uint64_t b, int width) {
    uint64_t m = interp_mask(width);
    unsigned bits = (unsigned)(8 * width);
    __int128 sa = (__int128)(int64_t)((a & m) << (64 - bits)) >> (64 - bits);
    __int128 sb = (__int128)(int64_t)((b & m) << (64 - bits)) >> (64 - bits);
    __int128 product = sa * sb;
    uint64_t result = (uint64_t)product & m;
    int64_t sign_extended = (int64_t)(result << (64 - bits)) >> (64 - bits);
    unsigned overflow = (unsigned)((__int128)sign_extended != product);
    interp_flags_nzcv(cpu, interp_msb(result, width), result == 0, overflow, overflow);
    return result;
}

static int interp_step_one_byte(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next);
static int interp_step_two_byte(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next);
// x87 (D8..DF) is implemented below, with the other FP.
static int interp_step_x87(struct cpu *cpu, struct insn *insn, uint64_t pc, uint64_t next);
