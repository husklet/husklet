static void pcache_record_provenance(uint64_t host, uint64_t end, uint64_t guest) {
    jit_instruction_map_put(host, end, guest);
    if (!g_pcache || host < (uint64_t)g_cache || end < host || end - host > UINT32_MAX || g_pc_nprov >= PC_PROV_CAP) {
        if (g_pcache && g_pc_nprov >= PC_PROV_CAP) g_pcache_poison = 1;
        return;
    }
    g_pc_prov[g_pc_nprov++] = (struct pc_prov){host - (uint64_t)g_cache, guest, (uint32_t)(end - host), 0};
}

#if defined(__GNUC__) && !defined(__clang__) && defined(__aarch64__)
extern void block_return(void) __attribute__((visibility("hidden")));
#else
static void block_return(void);
#endif

static void pc_reloc_add(uint32_t off, uint8_t kind, uint8_t rd, uint16_t slot) {
    if (!hl_reloc_add(&g_reloc_table, off, (uint32_t)kind | ((uint32_t)rd << 8) | ((uint32_t)slot << 16))) {
        g_pcache_poison = 1; // table full -> a baked pointer would go unrecorded; refuse to persist
    }
}

// Fixed 4-insn absolute materialization (movz + 3*movk). Fixed length so a reload can re-emit into the
// SAME reserved bytes regardless of the value's high-lane sparsity (unlike e_movconst, which is variable).
static void emit_hostptr48(int rd, uint64_t v) {
    e_movz(rd, (uint32_t)(v & 0xffff), 0);
    e_movk(rd, (uint32_t)((v >> 16) & 0xffff), 1);
    e_movk(rd, (uint32_t)((v >> 32) & 0xffff), 2);
    e_movk(rd, (uint32_t)((v >> 48) & 0xffff), 3);
}

// Recorded emitters. When the cache is OFF they fall back to the exact original emitter (matrix stays
// byte-identical); when ON they use the fixed 4-insn form + record the slot for relocation on load.
static void emit_blockret(int rd) {
    if (g_pcache) {
        pc_reloc_add((uint32_t)(g_cp - g_cache), RK_BLOCKRET, (uint8_t)rd, 0);
        emit_hostptr48(rd, (uint64_t)block_return);
    } else {
        e_movconst(rd, (uint64_t)block_return);
    }
}

static void emit_ibtcptr(int rd) {
    if (g_pcache) {
        pc_reloc_add((uint32_t)(g_cp - g_cache), RK_IBTC, (uint8_t)rd, 0);
        emit_hostptr48(rd, (uint64_t)g_ibtc);
    } else {
        e_adrp_add(rd, (uint64_t)g_ibtc);
    }
}

static void emit_t2cntptr(int rd, int slot) {
    if (g_pcache) {
        pc_reloc_add((uint32_t)(g_cp - g_cache), RK_T2CNT, (uint8_t)rd, (uint16_t)slot);
        emit_hostptr48(rd, (uint64_t)&g_t2cnt[slot]);
    } else {
        e_adrp_add(rd, (uint64_t)&g_t2cnt[slot]);
    }
}

static void emit_busfaultptr(int rd) {
    if (g_pcache) {
        pc_reloc_add((uint32_t)(g_cp - g_cache), RK_BUSFAULT, (uint8_t)rd, 0);
        emit_hostptr48(rd, (uint64_t)jit_guest_bus_fault);
    } else {
        e_movconst(rd, (uint64_t)jit_guest_bus_fault);
    }
}

// Materialize a page-aligned GUEST address with one host ADRP when it is reachable from the RX alias.
// The instruction PC is the executable alias, not g_cp's writable alias.  Persistent-cache reload may
// put that alias at a different VA, so record the instruction: the loader recovers its fixed guest target
// from the saved instruction + saved RX base and re-encodes it relative to the live RX base.
//
// Returns 1 after emitting, or 0 if the target is outside ADRP's signed 21-page (±4 GiB) range.  Callers
// must use their existing e_movconst fallback on 0.
static int emit_guest_adrp_page(int rd, uint64_t target) {
    uint64_t target_page = target & ~UINT64_C(0xfff);
    uint64_t pc_page = (uint64_t)J_RX(g_cp) & ~UINT64_C(0xfff);
    int64_t delta = (int64_t)(target_page - pc_page);
    if ((delta & 0xfff) || delta < -INT64_C(0x100000000) || delta > INT64_C(0xfffff000)) return 0;
    int64_t pages = delta >> 12;
    uint32_t imm21 = (uint32_t)pages & 0x1fffff;
    if (g_pcache) pc_reloc_add((uint32_t)(g_cp - g_cache), RK_GUEST_ADRP, (uint8_t)rd, 0);
    emit32(0x90000000u | ((imm21 & 3) << 29) | (((imm21 >> 2) & 0x7ffff) << 5) | (uint32_t)rd);
    return 1;
}

// Record a per-site IC's 16-byte cached {target,body} literal pair so a reload can zero it (the cached
// body pointer is an arena address that would be stale in a fresh process; a zeroed guard never matches
// -> the site harmlessly re-resolves through the dispatcher, which rewrites both literals).
static void pc_record_icsite(uint8_t *lt) {
    if (g_pcache) pc_reloc_add((uint32_t)(lt - g_cache), RK_ICSITE, 0, 0);
}

// ---- persisted layout ----
// [pc_hdr][reloc][map][pend][t2][txpg][provenance][library manifest][arena bytes]
struct pc_hdr {
    uint64_t magic, version;
    uint64_t translator_abi;
    uint64_t cpu_sz, jit_map_n, ibtc_n;
    uint64_t img_base, interp_base;
    hl_identity_digest bin_id;
    uint64_t entry_jump;
    uint64_t arena_used;
    uint64_t n_reloc, n_mapent, n_pend, n_t2, n_txpg, n_prov, n_lib;
    uint64_t csum;                     // FNV-1a over every byte after this header
    uint64_t block_return_at, ibtc_at; // diagnostics only (we re-emit from live symbols)
    uint64_t arena_rx_at;              // v6: RX base used to encode RK_GUEST_ADRP instructions
};

struct pc_mapent {
    uint64_t gpc, guest_start, guest_end, host_off, body_off;
};

struct pc_pend {
    uint64_t slot_off, target, source_gpc;
    uint32_t kind, fwd, orig, reserved;
};

struct pc_t2 {
    uint64_t gpc, cnt;
};

// Only fixed images and identity-validated, deterministically placed file mappings are revivable.
