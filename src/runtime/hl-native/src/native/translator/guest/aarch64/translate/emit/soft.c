struct a64_soft_guard {
    uint32_t *miss[6];
    uint32_t *direct[2];
    int miss_bit[6]; /* -1 = CBNZ; otherwise TBZ bit */
    int miss_reg[6];
    unsigned nmiss;
    unsigned ndirect;
    int ea;
    int tmp;
    int tmp2;
    uint64_t bytes;
    uint32_t required;
    uint64_t pc;
    uint8_t *native;
    uint8_t *metadata;
    int shared;
    int active;
    int profile_sample;
    int restore_reg[4];
    int restore_offset[4];
    unsigned nrestore;
};

#define SOFT_STUB_PATCH_MAX 65536
static uint32_t *g_soft_stub_patches[SOFT_STUB_PATCH_MAX];
static uint32_t g_soft_stub_patch_count;
static uint32_t *g_soft_legacy_stub_patches[SOFT_STUB_PATCH_MAX];
static uint32_t g_soft_legacy_stub_patch_count;
static uint32_t *g_soft_resolver_patches[SOFT_STUB_PATCH_MAX];
static uint32_t g_soft_resolver_bytes[SOFT_STUB_PATCH_MAX];
static uint32_t g_soft_resolver_required[SOFT_STUB_PATCH_MAX];
static uint32_t g_soft_resolver_patch_count;

static void emit_a64_bus_guard(int, uint64_t, uint64_t);
static void patch_adr(uint32_t *, uint8_t *, unsigned);
static int shadowgate(void);
static void emit_prof_bump(void *);

static int soft_profile_sample(uint64_t pc) {
    return g_prof && ((((pc >> 2) * UINT64_C(0x9e3779b97f4a7c15)) >> 58) == 0);
}

static uint32_t a64_cbnz_x(int reg, int64_t words) {
    return 0xB5000000u | (((uint32_t)words & 0x7ffffu) << 5) | (unsigned)reg;
}

static uint32_t a64_tbz_x(int reg, unsigned bit, int64_t words) {
    return 0x36000000u | ((bit & 0x20u) << 26) | ((bit & 0x1fu) << 19) | (((uint32_t)words & 0x3fffu) << 5) |
           (unsigned)reg;
}

static struct a64_soft_guard emit_a64_soft_guard_begin(int ea, int tmp, int tmp2, uint64_t bytes, uint32_t required,
                                                       uint64_t pc) {
    struct a64_soft_guard guard = {.ea = ea, .tmp = tmp, .tmp2 = tmp2, .bytes = bytes, .required = required, .pc = pc};
    int resume_ea = ea;
    if (!jit_guest_soft_active()) return guard;
    guard.active = 1;
    guard.profile_sample = soft_profile_sample(pc);
    if (guard.profile_sample) g_prof_soft_sites_sampled++;
    assert(ea != tmp && ea != tmp2 && tmp != tmp2);
    assert(bytes != 0 && bytes <= 4096);
    /*
     * With the shadow-RAS disabled x30 carries no live engine return link.
     * Use it as the resolver's per-site continuation, normalize every EA in
     * x16, and share the complete interval/permission check once per block.
     * Shadow-enabled builds retain the proven inline guard below.
     */
    guard.shared = shadowgate() < 0 && !g_tier2_build && !guard.profile_sample;
    if (guard.shared) {
        if (ea != 16) e_movr(16, ea);
        guard.ea = 16;
        guard.tmp = 17;
        guard.tmp2 = 18;
        ea = 16;
        tmp = 17;
        tmp2 = 18;
    }

    /*
     * Most accesses in a process with one sparse 4 KiB alias still target
     * ordinary identity-mapped stack/heap pages. Reject those against the
     * conservative logical-VMA hull before consulting the per-page software
     * TLB. All arithmetic and branches are flag-free, preserving guest NZCV
     * and host exclusive-monitor state.
     *
     * Below-hull: first - (ea+bytes) has bit 63 clear when the access ends at
     * or below first. Above-hull: ea-last has bit 63 clear when ea >= last.
     * Equality is intentionally classified direct; overlap remains on the
     * guarded path.
     */
    e_ldr(tmp, CPUREG, OFF_SOFT_FILTER_FIRST);
    e_movconst(tmp2, bytes);
    emit32(0x8B000000u | ((unsigned)tmp2 << 16) | ((unsigned)ea << 5) | (unsigned)tmp2);
    emit32(0xCB000000u | ((unsigned)tmp2 << 16) | ((unsigned)tmp << 5) | (unsigned)tmp2);
    emit32(0xD37FFC00u | ((unsigned)tmp2 << 5) | (unsigned)tmp2);
    guard.direct[guard.ndirect++] = (uint32_t *)g_cp;
    emit32(0); /* tbz tmp2,#0,direct */

    e_ldr(tmp, CPUREG, OFF_SOFT_FILTER_LAST);
    emit32(0xCB000000u | ((unsigned)tmp << 16) | ((unsigned)ea << 5) | (unsigned)tmp2);
    emit32(0xD37FFC00u | ((unsigned)tmp2 << 5) | (unsigned)tmp2);
    guard.direct[guard.ndirect++] = (uint32_t *)g_cp;
    emit32(0); /* tbz tmp2,#0,direct */

    if (guard.shared) {
        /*
         * x17 points at fixed metadata and a plain branch enters the shared
         * resolver. The native continuation immediately follows metadata:
         *   [pc:u64, miss_delta:i32, pad:u32]
         */
        uint32_t *metadata_address = (uint32_t *)g_cp;
        emit32(0); /* adr x17,metadata */
        if (g_soft_resolver_patch_count >= SOFT_STUB_PATCH_MAX) {
            static const char message[] = "too many shared soft-memory guards in one translated block";
            (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
            _exit(70);
        }
        uint32_t resolver_index = g_soft_resolver_patch_count++;
        g_soft_resolver_patches[resolver_index] = (uint32_t *)g_cp;
        g_soft_resolver_bytes[resolver_index] = (uint32_t)bytes;
        g_soft_resolver_required[resolver_index] = required;
        emit32(0); /* b shared_soft_resolver */
        guard.metadata = g_cp;
        patch_adr(metadata_address, guard.metadata, 17);
        memcpy(g_cp, &pc, sizeof pc);
        g_cp += sizeof pc;
        memset(g_cp, 0, 4); /* miss displacement, filled by guard end */
        g_cp += 4;
        uint16_t narrow_bytes = (uint16_t)bytes;
        memcpy(g_cp, &narrow_bytes, sizeof narrow_bytes);
        g_cp += sizeof narrow_bytes;
        *g_cp++ = (uint8_t)required;
        *g_cp++ = 0;
        guard.native = g_cp;
        if (resume_ea != 16) e_movr(resume_ea, 16);
        return guard;
    }

    /* Width-independent cached interval hit, using sign bits of non-setting
       subtracts. Linux userspace canonical addresses are below 2^63, so an
       unsigned underflow is exactly the high-bit test here. */
    e_ldr(tmp, CPUREG, OFF_SOFT_PAGE); /* inclusive first */
    emit32(0xCB000000u | ((unsigned)tmp << 16) | ((unsigned)ea << 5) | (unsigned)tmp2);
    emit32(0xD37FFC00u | ((unsigned)tmp2 << 5) | (unsigned)tmp2); /* lsr tmp2,tmp2,#63 */
    guard.miss[guard.nmiss++] = (uint32_t *)g_cp;
    guard.miss_bit[guard.nmiss - 1] = -1;
    guard.miss_reg[guard.nmiss - 1] = tmp2;
    emit32(0);

    e_ldr(tmp, CPUREG, OFF_SOFT_LIMIT); /* exclusive end */
    e_movconst(tmp2, bytes);
    emit32(0x8B000000u | ((unsigned)tmp2 << 16) | ((unsigned)ea << 5) | (unsigned)tmp2);
    emit32(0xCB000000u | ((unsigned)tmp2 << 16) | ((unsigned)tmp << 5) | (unsigned)tmp2);
    emit32(0xD37FFC00u | ((unsigned)tmp2 << 5) | (unsigned)tmp2);
    guard.miss[guard.nmiss++] = (uint32_t *)g_cp;
    guard.miss_bit[guard.nmiss - 1] = -1;
    guard.miss_reg[guard.nmiss - 1] = tmp2;
    emit32(0);

    e_ldr(tmp, CPUREG, OFF_SOFT_PROTECTION);
    if (required & HL_LOGICAL_VMA_READ) {
        guard.miss[guard.nmiss++] = (uint32_t *)g_cp;
        guard.miss_bit[guard.nmiss - 1] = 0;
        guard.miss_reg[guard.nmiss - 1] = tmp;
        emit32(0); /* tbz tmp,#0,miss */
    }
    if (required & HL_LOGICAL_VMA_WRITE) {
        guard.miss[guard.nmiss++] = (uint32_t *)g_cp;
        guard.miss_bit[guard.nmiss - 1] = 1;
        guard.miss_reg[guard.nmiss - 1] = tmp;
        emit32(0); /* tbz tmp,#1,miss */
    }
    e_ldr(tmp, CPUREG, OFF_SOFT_DELTA);
    emit32(0x8B000000u | ((unsigned)tmp << 16) | ((unsigned)ea << 5) | (unsigned)ea); /* add ea,ea,tmp */
    guard.native = g_cp;
    return guard;
}

static void a64_soft_guard_restore(struct a64_soft_guard *guard, int reg, int offset) {
    assert(guard->nrestore < 4);
    guard->restore_reg[guard->nrestore] = reg;
    guard->restore_offset[guard->nrestore] = offset;
    guard->nrestore++;
}

/*
 * A soft-TLB miss is cold, but the old lowering put a complete architectural
 * spill and block-return sequence at every guest memory instruction.  A
 * memory-heavy straight-line region consequently spent hundreds of bytes per
 * access on identical code which almost never ran.
 *
 * Keep only the site-specific work inline: preserve the guest EA, restore
 * translator scratch registers, and point x17 at immutable metadata adjacent
 * to the site.  All miss sites in the translated block branch to one shared
 * spill/exit stub.  x16/x17 are engine-owned in this ABI and emit_spill()
 * deliberately preserves them, exactly as for the shared BUS stub.
 */
static void emit_a64_soft_exit_site(const struct a64_soft_guard *guard) {
    assert(g_steal1617);
    e_str(guard->ea, CPUREG, OFF_SOFT_EA);
    for (unsigned index = 0; index < guard->nrestore; ++index)
        e_ldr(guard->restore_reg[index], CPUREG, guard->restore_offset[index]);
    uint32_t *metadata_address = (uint32_t *)g_cp;
    emit32(0); /* adr x17,immutable_site_metadata */
    if (g_soft_legacy_stub_patch_count >= SOFT_STUB_PATCH_MAX) {
        static const char message[] = "too many soft-memory guards in one translated block";
        (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
        _exit(70);
    }
    g_soft_legacy_stub_patches[g_soft_legacy_stub_patch_count++] = (uint32_t *)g_cp;
    emit32(0); /* b shared_soft_stub */
    uint8_t *metadata = g_cp;
    patch_adr(metadata_address, metadata, 17);
    memcpy(g_cp, &guard->bytes, sizeof(guard->bytes));
    g_cp += sizeof(guard->bytes);
    uint64_t required = guard->required;
    memcpy(g_cp, &required, sizeof(required));
    g_cp += sizeof(required);
    memcpy(g_cp, &guard->pc, sizeof(guard->pc));
    g_cp += sizeof(guard->pc);
}

static void emit_a64_soft_guard_end(struct a64_soft_guard *guard) {
    if (!guard->active) return;
    if (guard->shared) {
        uint32_t *skip = (uint32_t *)g_cp;
        emit32(0); /* b resume */
        uint8_t *miss = g_cp;
        for (unsigned index = 0; index < guard->nrestore; ++index)
            e_ldr(guard->restore_reg[index], CPUREG, guard->restore_offset[index]);
        if (g_soft_stub_patch_count >= SOFT_STUB_PATCH_MAX) {
            static const char message[] = "too many soft-memory restore stubs in one translated block";
            (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
            _exit(70);
        }
        g_soft_stub_patches[g_soft_stub_patch_count++] = (uint32_t *)g_cp;
        emit32(0); /* b shared_soft_exit */
        uint8_t *resume = g_cp;
        *skip = 0x14000000u | ((uint32_t)((resume - (uint8_t *)skip) / 4) & 0x03ffffffu);
        int64_t miss_delta = miss - guard->native;
        if (miss_delta < INT32_MIN || miss_delta > INT32_MAX) {
            static const char message[] = "soft-memory site miss displacement out of range";
            (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
            _exit(70);
        }
        int32_t narrow_delta = (int32_t)miss_delta;
        memcpy(guard->metadata + 8, &narrow_delta, sizeof narrow_delta);
        for (unsigned i = 0; i < guard->ndirect; ++i)
            *guard->direct[i] = a64_tbz_x(guard->tmp2, 0, (guard->native - (uint8_t *)guard->direct[i]) / 4);
        return;
    }
    uint32_t *skip = (uint32_t *)g_cp;
    emit32(0); /* b resume */
    uint8_t *miss = g_cp;
    emit_a64_soft_exit_site(guard);
    uint8_t *resume = g_cp;
    *skip = 0x14000000u | ((uint32_t)((resume - (uint8_t *)skip) / 4) & 0x03ffffffu);
    for (unsigned i = 0; i < guard->nmiss; ++i) {
        if (guard->miss_bit[i] < 0)
            *guard->miss[i] = a64_cbnz_x(guard->miss_reg[i], (miss - (uint8_t *)guard->miss[i]) / 4);
        else
            *guard->miss[i] =
                a64_tbz_x(guard->miss_reg[i], (unsigned)guard->miss_bit[i], (miss - (uint8_t *)guard->miss[i]) / 4);
    }
    // Profiling must not insert register-using code into this live-EA path.
    uint8_t *direct = guard->native;
    for (unsigned i = 0; i < guard->ndirect; ++i)
        *guard->direct[i] = a64_tbz_x(guard->tmp2, 0, (direct - (uint8_t *)guard->direct[i]) / 4);
}

static void aarch64_soft_filter_refresh(struct cpu *c) {
    uint64_t first = UINT64_MAX, last = 0;
    hl_logical_vma_snapshot *snapshot =
        atomic_load_explicit(hl_logical_vma_global_snapshot_source(), memory_order_acquire);
    if (snapshot != NULL && snapshot->count != 0) {
        first = snapshot->views[0].guest_first;
        last = snapshot->views[snapshot->count - 1].guest_last;
    }
    c->soft_filter_first = first;
    c->soft_filter_last = last;
}

static void emit_a64_soft_stub(void) {
    if (!g_soft_stub_patch_count && !g_soft_resolver_patch_count && !g_soft_legacy_stub_patch_count) return;
    if (g_soft_resolver_patch_count) {
        uint32_t *cold_miss_patches[1024];
        int cold_miss_bits[1024]; /* -1 = CBNZ x18, otherwise TBZ x18,bit */
        unsigned cold_miss_count = 0;
        for (;;) {
            uint32_t first = 0;
            while (first < g_soft_resolver_patch_count && g_soft_resolver_patches[first] == NULL)
                ++first;
            if (first == g_soft_resolver_patch_count) break;
            uint32_t bytes = g_soft_resolver_bytes[first];
            uint32_t required = g_soft_resolver_required[first];
            uint8_t *resolver = g_cp;
            for (uint32_t i = first; i < g_soft_resolver_patch_count; ++i) {
                if (g_soft_resolver_patches[i] == NULL || g_soft_resolver_bytes[i] != bytes ||
                    g_soft_resolver_required[i] != required)
                    continue;
                int64_t displacement = (resolver - (uint8_t *)g_soft_resolver_patches[i]) / 4;
                if (displacement < -(INT64_C(1) << 25) || displacement >= (INT64_C(1) << 25)) {
                    static const char message[] = "soft-memory resolver branch out of range";
                    (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
                    _exit(70);
                }
                *g_soft_resolver_patches[i] = 0x14000000u | ((uint32_t)displacement & 0x03ffffffu);
                g_soft_resolver_patches[i] = NULL;
            }

            /* x16 = guest EA, x17 = immutable site metadata. Only x18 is
               scratch; x30 remains untouched for precise host-link state. */
            e_ldr(18, CPUREG, OFF_SOFT_PAGE);
            emit32(0xCB000000u | (18u << 16) | (16u << 5) | 18u);
            emit32(0xD37FFC00u | (18u << 5) | 18u);
            assert(cold_miss_count < sizeof cold_miss_patches / sizeof cold_miss_patches[0]);
            cold_miss_patches[cold_miss_count] = (uint32_t *)g_cp;
            cold_miss_bits[cold_miss_count++] = -1;
            emit32(0);

            e_ldr(18, CPUREG, OFF_SOFT_LIMIT);
            if (bytes == 4096)
                emit32(0xD1400000u | (1u << 10) | (18u << 5) | 18u);
            else
                e_subi(18, 18, bytes);
            emit32(0xCB000000u | (16u << 16) | (18u << 5) | 18u);
            emit32(0xD37FFC00u | (18u << 5) | 18u);
            assert(cold_miss_count < sizeof cold_miss_patches / sizeof cold_miss_patches[0]);
            cold_miss_patches[cold_miss_count] = (uint32_t *)g_cp;
            cold_miss_bits[cold_miss_count++] = -1;
            emit32(0);

            e_ldr(18, CPUREG, OFF_SOFT_PROTECTION);
            if (required & HL_LOGICAL_VMA_READ) {
                assert(cold_miss_count < sizeof cold_miss_patches / sizeof cold_miss_patches[0]);
                cold_miss_patches[cold_miss_count] = (uint32_t *)g_cp;
                cold_miss_bits[cold_miss_count++] = 0;
                emit32(0); /* tbz x18,#READ,miss */
            }
            if (required & HL_LOGICAL_VMA_WRITE) {
                assert(cold_miss_count < sizeof cold_miss_patches / sizeof cold_miss_patches[0]);
                cold_miss_patches[cold_miss_count] = (uint32_t *)g_cp;
                cold_miss_bits[cold_miss_count++] = 1;
                emit32(0); /* tbz x18,#WRITE,miss */
            }
            e_ldr(18, CPUREG, OFF_SOFT_DELTA);
            emit32(0x8B000000u | (18u << 16) | (16u << 5) | 16u);
            e_addi(17, 17, 16);
            e_br(17);
        }
        uint8_t *resolver_miss = g_cp;
        for (unsigned i = 0; i < cold_miss_count; ++i) {
            uint32_t *patch = cold_miss_patches[i];
            int64_t displacement = (resolver_miss - (uint8_t *)patch) / 4;
            *patch = cold_miss_bits[i] < 0 ? a64_cbnz_x(18, displacement)
                                           : a64_tbz_x(18, (unsigned)cold_miss_bits[i], displacement);
        }
        e_str(16, CPUREG, OFF_SOFT_EA);
        emit32(0x79400000u | (6u << 10) | (17u << 5) | 18u); /* ldrh w18,[meta,#12] */
        e_str(18, CPUREG, OFF_SOFT_BYTES);
        emit32(0x39400000u | (14u << 10) | (17u << 5) | 18u); /* ldrb w18,[meta,#14] */
        e_str(18, CPUREG, OFF_SOFT_REQUIRED);
        e_ldr(18, 17, 0);
        e_str(18, CPUREG, OFF_SOFT_PC);
        e_str(18, CPUREG, OFF_PC);
        emit32(0xB9800000u | (2u << 10) | (17u << 5) | 18u); /* ldrsw x18,[meta,#8] */
        e_addi(17, 17, 16);
        emit32(0x8B000000u | (18u << 16) | (17u << 5) | 18u);
        e_br(18);
    }

    if (g_soft_stub_patch_count) {
        uint8_t *stub = g_cp;
        for (uint32_t i = 0; i < g_soft_stub_patch_count; ++i) {
            int64_t displacement = (stub - (uint8_t *)g_soft_stub_patches[i]) / 4;
            if (displacement < -(INT64_C(1) << 25) || displacement >= (INT64_C(1) << 25)) {
                static const char message[] = "soft-memory stub branch out of range";
                (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
                _exit(70);
            }
            *g_soft_stub_patches[i] = 0x14000000u | ((uint32_t)displacement & 0x03ffffffu);
        }
        emit_spill();
        e_movconst(9, R_SOFTMISS);
        e_str(9, 0, OFF_RSN);
        emit_blockret(9);
        e_br(9);
    }
    if (g_soft_legacy_stub_patch_count) {
        uint8_t *stub = g_cp;
        for (uint32_t i = 0; i < g_soft_legacy_stub_patch_count; ++i) {
            int64_t displacement = (stub - (uint8_t *)g_soft_legacy_stub_patches[i]) / 4;
            if (displacement < -(INT64_C(1) << 25) || displacement >= (INT64_C(1) << 25)) {
                static const char message[] = "legacy soft-memory stub branch out of range";
                (void)jit_fail(HL_STATUS_OUT_OF_MEMORY, message, sizeof message - 1u);
                _exit(70);
            }
            *g_soft_legacy_stub_patches[i] = 0x14000000u | ((uint32_t)displacement & 0x03ffffffu);
        }
        emit_spill();
        e_ldr(9, 17, 0);
        e_str(9, 0, OFF_SOFT_BYTES);
        e_ldr(9, 17, 8);
        e_str(9, 0, OFF_SOFT_REQUIRED);
        e_ldr(9, 17, 16);
        e_str(9, 0, OFF_SOFT_PC);
        e_str(9, 0, OFF_PC);
        e_movconst(9, R_SOFTMISS);
        e_str(9, 0, OFF_RSN);
        emit_blockret(9);
        e_br(9);
    }
}

/* A discontinuous-view retry executes against cpu->soft_bounce.  Force one
   cold dispatcher crossing after the architectural operation so stores can
   be scattered before the following guest instruction observes them. */
static void emit_a64_soft_bounce_commit(uint64_t next_pc) {
    if (!jit_guest_soft_active()) return;
    e_ldr(16, CPUREG, OFF_SOFT_BOUNCE_PENDING);
    uint32_t *clear = (uint32_t *)g_cp;
    emit32(0); /* cbz x16,resume */
    emit_exit_const(next_pc, R_SOFTCOMMIT);
    uint8_t *resume = g_cp;
    *clear = 0xB4000000u | (((uint32_t)((resume - (uint8_t *)clear) / 4) & 0x7ffffu) << 5) | 16u;
}

static void emit_a64_soft_exclusive(uint32_t in) {
    int base = (int)((in >> 5) & 31u);
    if (base == 31)
        e_mov_from_sp(16);
    else if (is_stolen(base))
        e_ldr(16, CPUREG, base * 8);
    else
        e_movr(16, base);
    emit_a64_bus_guard(16, a64_mem_bytes(in), g_emit_gpc);

    int mask = gpr_field_mask(in);
    unsigned used = 0;
    static const int shifts[4] = {0, 5, 16, 10}, mbits[4] = {1, 2, 4, 8};
    for (int k = 0; k < 4; ++k)
        if (mask & mbits[k]) used |= 1u << ((in >> shifts[k]) & 31u);
    if (is_casp(in)) {
        used |= 1u << ((((in >> 16) & 31u) + 1u) & 31u);
        used |= 1u << (((in & 31u) + 1u) & 31u);
    }
    int ea = 0;
    while ((used & (1u << ea)) || is_stolen(ea))
        ++ea;
    e_str(ea, CPUREG, (int)OFF_MSCRATCH + 32);
    e_movr(ea, 16);
    struct a64_soft_guard soft =
        emit_a64_soft_guard_begin(ea, 17, 18, a64_mem_bytes(in), a64_mem_required(in), g_emit_gpc);
    a64_soft_guard_restore(&soft, ea, (int)OFF_MSCRATCH + 32);
    if (is_casp(in)) {
        emit_casp_mangled(in, ea);
    } else {
        uint32_t rebased = (in & ~(31u << 5)) | ((uint32_t)ea << 5);
        mask &= ~2;
        if (uses_x18(in, mask))
            emit_mangled_x18(rebased, mask);
        else
            emit32(rebased);
    }
    emit_a64_soft_guard_end(&soft);
    e_ldr(ea, CPUREG, (int)OFF_MSCRATCH + 32);
}
