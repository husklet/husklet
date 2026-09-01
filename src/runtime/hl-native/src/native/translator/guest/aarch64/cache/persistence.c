static void pcache_directory_close(void) {
    if (g_pc_directory.handle != HL_HOST_HANDLE_INVALID) (void)hl_persist_directory_close(&g_pc_directory);
    g_pc_directory_path[0] = 0;
}

// Re-emit / neutralize every recorded slot for THIS process. Runs inside the jit_wprot() write window,
// against the RW alias (g_cache + off). Offsets/slots were validated by pcache_load before we get here.
static void pcache_relocate(uint64_t saved_rx) {
    for (int i = 0; i < g_nreloc; i++) {
        uint32_t off = g_reloc[i].off, info = g_reloc[i].info;
        int kind = info & 0xff, rd = (info >> 8) & 0xff, slot = (info >> 16) & 0xffff;
        uint32_t *w = (uint32_t *)(g_cache + off);
        uint64_t v;
        switch (kind) {
        case RK_BLOCKRET: v = (uint64_t)block_return; break;
        case RK_IBTC: v = (uint64_t)g_ibtc; break;
        case RK_T2CNT: v = (uint64_t)&g_t2cnt[slot]; break;
        case RK_BUSFAULT: v = (uint64_t)jit_guest_bus_fault; break;
        case RK_BUSRESUME: {
            // Resume target is the instruction immediately after the literal, in the live RX alias.
            uint64_t resume = (uint64_t)J_RX(g_cache) + off + 8;
            memcpy(g_cache + off, &resume, sizeof resume);
            continue;
        }
        case RK_ICSITE:
            // Zero the guard target literal (+0): a zeroed target makes the site's equality guard miss so
            // the dispatcher re-resolves + refills. Under the x16/x17 steal the +8 slot (Lb) is not a stale
            // arena pointer but the hit branch's arena OFFSET (stable across reload) -> keep it so the refill
            // can re-find the branch; the stale direct branch stays unreachable (guard misses) until then.
            // Legacy (non-steal) still stores a stale body pointer at +8 -> zero it.
            *(uint64_t *)(g_cache + off) = 0;
            if (!g_steal1617) *(uint64_t *)(g_cache + off + 8) = 0;
            continue;
        case RK_GUEST_ADRP: {
            // Validation already proved this target remains reachable from the live RX alias.
            uint32_t in = w[0];
            uint32_t imm21 = ((in >> 29) & 3) | (((in >> 5) & 0x7ffff) << 2);
            int64_t pages = (int32_t)(imm21 << 11) >> 11;
            uint64_t saved_pc_page = (saved_rx + off) & ~UINT64_C(0xfff);
            uint64_t target_page = saved_pc_page + (uint64_t)(pages * INT64_C(4096));
            uint64_t live_pc_page = ((uint64_t)J_RX(g_cache) + off) & ~UINT64_C(0xfff);
            int64_t live_pages = (int64_t)(target_page - live_pc_page) >> 12;
            uint32_t live_imm21 = (uint32_t)live_pages & 0x1fffff;
            w[0] = 0x90000000u | ((live_imm21 & 3) << 29) | (((live_imm21 >> 2) & 0x7ffff) << 5) | rd;
            continue;
        }
        default: continue;
        }
        w[0] = 0xD2800000u | (((uint32_t)(v) & 0xffff) << 5) | rd;                    // movz rd, #v[0:16]
        w[1] = 0xF2800000u | (1u << 21) | (((uint32_t)(v >> 16) & 0xffff) << 5) | rd; // movk #16
        w[2] = 0xF2800000u | (2u << 21) | (((uint32_t)(v >> 32) & 0xffff) << 5) | rd; // movk #32
        w[3] = 0xF2800000u | (3u << 21) | (((uint32_t)(v >> 48) & 0xffff) << 5) | rd; // movk #48
    }
}

// Validate one reloc record against the arena bounds. 16 bytes are rewritten for every kind (4 insns or
// the literal pair), so the whole window must be inside the restored arena and naturally aligned.
static int pc_reloc_ok(hl_reloc r, uint64_t arena_used) {
    int kind = r.info & 0xff, slot = (r.info >> 16) & 0xffff;
    // The guard metadata block is instruction-aligned, so the resume literal is only 4-aligned; both
    // sides go through memcpy.
    uint64_t width = kind == RK_GUEST_ADRP ? 4 : kind == RK_BUSRESUME ? 8 : 16;
    if (!pc_window_contains(arena_used, r.off, width, kind == RK_ICSITE ? 8 : 4)) return 0;
    if (kind == RK_ICSITE) return 1;
    // The resume target is off+8 and the stub branches to it, so it must also land inside the arena.
    if (kind == RK_BUSRESUME) return pc_window_contains(arena_used, r.off + 8, 4, 4);
    if (((r.info >> 8) & 0xff) > 30) return 0; // rd must be a real GPR (we never bake into sp/xzr)
    if (kind == RK_T2CNT) return slot < T2_MAX;
    return kind == RK_BLOCKRET || kind == RK_IBTC || kind == RK_BUSFAULT || kind == RK_GUEST_ADRP;
}

// Validate the opcode and prove that an ADRP target recovered relative to the saved RX base remains
// encodable relative to this process's RX base.  Do this before rebuilding any live map state, so a cache
// created with an unusually distant arena base degrades to a clean miss instead of leaving partial state.
static int pc_guest_adrp_ok(hl_reloc r, const uint8_t *arena, uint64_t saved_rx) {
    if ((r.info & 0xff) != RK_GUEST_ADRP) return 1;
    uint32_t in;
    memcpy(&in, arena + r.off, sizeof in);
    int rd = (r.info >> 8) & 0xff;
    if ((in & 0x9f000000u) != 0x90000000u || (in & 31) != (uint32_t)rd) return 0;
    uint32_t imm21 = ((in >> 29) & 3) | (((in >> 5) & 0x7ffff) << 2);
    int64_t pages = (int32_t)(imm21 << 11) >> 11;
    uint64_t saved_pc_page = (saved_rx + r.off) & ~UINT64_C(0xfff);
    uint64_t target_page = saved_pc_page + (uint64_t)(pages * INT64_C(4096));
    uint64_t live_pc_page = ((uint64_t)J_RX(g_cache) + r.off) & ~UINT64_C(0xfff);
    int64_t delta = (int64_t)(target_page - live_pc_page);
    return !(delta & 0xfff) && delta >= -INT64_C(0x100000000) && delta <= INT64_C(0xfffff000);
}

// Returns 1 on HIT (arena + maps restored -> translation of the startup path is skipped). ANY mismatch /
// truncation / checksum failure / out-of-bounds record -> 0 (graceful MISS; the caller translates fresh
// and the exit-time save atomically replaces the bad file).
static int pcache_load(uint64_t entry_jump) {
    if (!g_pcache || hl_identity_digest_empty(&g_pc_binid) || g_force_base_failed) return 0;
    // Every persisted block was translated under an armed ledger, so it carries memory guards; a restored
    // Every persisted block was translated under an armed ledger, so it carries memory guards; a restored
    // arena is only sound in a process whose ledger is armed and latched for good. The launch path does
    // that before it gets here -- refuse rather than restore guarded code into a bus that can still take a
    // 0 -> 1 edge and rotate it away, or a disarmed one whose later arm would have nothing to invalidate.
    if (!jit_guest_bus_active()) return 0;
    uint64_t t0 = g_coldprof ? now_ns() : 0;
    char path[1024];
    if (!pcache_file(path, sizeof path)) return 0;
    void *image = NULL;
    size_t image_size = 0;
    if (!hl_persist_load_at(&g_pc_directory, path, CACHE_SZ + UINT64_C(134217728), &image, &image_size)) return 0;
    hl_persist_cursor cursor = {image, image_size, 0};
    struct pc_hdr h;
    if (!hl_persist_take(&cursor, &h, sizeof h)) {
        free(image);
        return 0;
    }
    if (h.magic != PC_MAGIC || !hl_pcache_compatible(h.version, h.translator_abi, PC_VERSION_EFF, PC_TRANSLATOR_ABI) ||
        h.cpu_sz != sizeof(struct cpu) || h.jit_map_n != JIT_MAP_N || h.ibtc_n != IBTC_N || h.img_base != PC_IMG_BASE ||
        h.interp_base != PC_INTERP_BASE || !hl_identity_digest_equal(&h.bin_id, &g_pc_binid) ||
        h.entry_jump != entry_jump || h.arena_used > CACHE_SZ || (h.arena_used & 3) || h.n_reloc > PC_RELOC_CAP ||
        h.n_mapent > JIT_MAP_N || h.n_pend > (1u << 16) || h.n_t2 > T2_MAX || h.n_txpg > TXPG_N ||
        h.n_prov > PC_PROV_CAP || h.n_lib != 0) {
        free(image);
        return 0;
    }
    hl_reloc *re = h.n_reloc ? malloc(h.n_reloc * sizeof *re) : NULL;
    struct pc_mapent *me = h.n_mapent ? malloc(h.n_mapent * sizeof *me) : NULL;
    struct pc_pend *pe = h.n_pend ? malloc(h.n_pend * sizeof *pe) : NULL;
    struct pc_t2 *te = h.n_t2 ? malloc(h.n_t2 * sizeof *te) : NULL;
    uint64_t *tx = h.n_txpg ? malloc(h.n_txpg * sizeof *tx) : NULL;
    struct pc_prov *pv = h.n_prov ? malloc(h.n_prov * sizeof *pv) : NULL;
    struct pc_lib *libs = h.n_lib ? malloc(h.n_lib * sizeof *libs) : NULL;
    uint8_t *abuf = h.arena_used ? malloc(h.arena_used) : NULL;
    int ok = (h.n_reloc == 0 || re) && (h.n_mapent == 0 || me) && (h.n_pend == 0 || pe) && (h.n_t2 == 0 || te) &&
             (h.n_txpg == 0 || tx) && (h.n_prov == 0 || pv) && (h.n_lib == 0 || libs) && (h.arena_used == 0 || abuf);
#define PC_RD(buf, nbytes) (ok && (ok = hl_persist_take(&cursor, (buf), (size_t)(nbytes))))
    PC_RD(re, h.n_reloc * sizeof *re);
    PC_RD(me, h.n_mapent * sizeof *me);
    PC_RD(pe, h.n_pend * sizeof *pe);
    PC_RD(te, h.n_t2 * sizeof *te);
    PC_RD(tx, h.n_txpg * sizeof *tx);
    PC_RD(pv, h.n_prov * sizeof *pv);
    PC_RD(libs, h.n_lib * sizeof *libs);
    if (ok) ok = hl_persist_take(&cursor, abuf, (size_t)h.arena_used) && cursor.offset == cursor.size;
#undef PC_RD
    free(image);
    // Whole-payload checksum BEFORE trusting any record (bit rot / short file / foreign writer).
    if (ok) {
        hl_digest digest;
        hl_digest_init(&digest, HL_DIGEST_SEED);
        hl_digest_update(&digest, re, h.n_reloc * sizeof *re);
        hl_digest_update(&digest, me, h.n_mapent * sizeof *me);
        hl_digest_update(&digest, pe, h.n_pend * sizeof *pe);
        hl_digest_update(&digest, te, h.n_t2 * sizeof *te);
        hl_digest_update(&digest, tx, h.n_txpg * sizeof *tx);
        hl_digest_update(&digest, pv, h.n_prov * sizeof *pv);
        hl_digest_update(&digest, libs, h.n_lib * sizeof *libs);
        hl_digest_update(&digest, abuf, h.arena_used);
        ok = hl_digest_value(&digest) == h.csum;
    }
    // Per-record bounds: every offset a later pass will WRITE or BRANCH through must be inside the arena.
    for (uint64_t i = 0; ok && i < h.n_reloc; i++)
        ok = pc_reloc_ok(re[i], h.arena_used);
    for (uint64_t i = 0; ok && i < h.n_reloc; i++)
        ok = pc_guest_adrp_ok(re[i], abuf, h.arena_rx_at);
    for (uint64_t i = 0; ok && i < h.n_mapent; i++)
        ok = pc_window_contains(h.arena_used, me[i].host_off, 1, 4) &&
             pc_window_contains(h.arena_used, me[i].body_off, 1, 4) && me[i].guest_start <= me[i].gpc &&
             me[i].gpc < me[i].guest_end;
    for (uint64_t i = 0; ok && i < h.n_pend; i++) {
        ok = pc_window_contains(h.arena_used, pe[i].slot_off, 4, 4) && pe[i].kind <= 2 && pe[i].fwd <= 1;
        if (ok && pe[i].kind == 2) {
            uint32_t in = pe[i].orig;
            int valid = (in & 0xff000010u) == 0x54000000u || (in & 0x7e000000u) == 0x34000000u ||
                        (in & 0x7e000000u) == 0x36000000u;
            ok = valid && !(pe[i].source_gpc & 3) &&
                 pe[i].fwd == (uint32_t)(g_fwdskip && pe[i].target > pe[i].source_gpc);
        }
    }
    for (uint64_t i = 0; ok && i < h.n_prov; i++)
        ok = pv[i].reserved == 0 && pv[i].size != 0 && pc_window_contains(h.arena_used, pv[i].host_off, pv[i].size, 4);
    for (uint64_t i = 0; ok && i < h.n_lib; i++) {
        uint64_t end = libs[i].base + libs[i].len;
        ok = libs[i].id != 0 && libs[i].len != 0 && !(libs[i].base & UINT64_C(0x1fffff)) && end >= libs[i].base &&
             libs[i].base >= PC_LIB_BASE && end <= PC_LIB_BASE + PC_LIB_SPAN;
        for (uint64_t j = 0; ok && j < i; j++) {
            uint64_t other_end = libs[j].base + libs[j].len;
            ok = end <= libs[j].base || libs[i].base >= other_end;
        }
    }
    if (!ok) {
        free(re);
        free(me);
        free(pe);
        free(te);
        free(tx);
        free(pv);
        free(libs);
        free(abuf);
        return 0;
    }

    // Rebuild engine state from the offset-relative records.
    if (!hl_reloc_import(&g_reloc_table, re, (size_t)h.n_reloc)) {
        free(re);
        free(me);
        free(pe);
        free(te);
        free(tx);
        free(pv);
        free(libs);
        free(abuf);
        return 0;
    }
    free(g_pc_defer);
    g_pc_defer = NULL;
    g_pc_ndefer = 0;
    free(g_pc_prov_defer);
    g_pc_prov_defer = NULL;
    g_pc_nprov_defer = 0;
    memcpy(g_pc_libs, libs, (size_t)h.n_lib * sizeof *libs);
    g_pc_nlib = (int)h.n_lib;
    uint64_t map_deferred = 0;
    for (uint64_t i = 0; i < h.n_mapent; i++) {
        if (pc_range_fixed(me[i].guest_start, me[i].guest_end))
            if (map_put(me[i].gpc, me[i].guest_start, me[i].guest_end,
                        g_cache + me[i].host_off, g_cache + me[i].body_off) != MAP_PUT_OK) {
                free(me);
                free(tx);
                free(pv);
                free(libs);
                return 0;
            }
        else if (pc_range_in_lib(me[i].guest_start, me[i].guest_end))
            map_deferred++;
    }
    if (map_deferred) {
        g_pc_defer = malloc((size_t)map_deferred * sizeof *g_pc_defer);
        if (g_pc_defer)
            for (uint64_t i = 0; i < h.n_mapent; i++)
                if (!pc_range_fixed(me[i].guest_start, me[i].guest_end) &&
                    pc_range_in_lib(me[i].guest_start, me[i].guest_end))
                    g_pc_defer[g_pc_ndefer++] = me[i];
    }
    pend_reset();
    for (uint64_t i = 0; i < h.n_pend; i++) {
        uint32_t *slot = (uint32_t *)(g_cache + pe[i].slot_off);
        if (pe[i].kind == 2)
            add_pend_cond(slot, pe[i].target, pe[i].orig, pe[i].source_gpc, (int)pe[i].fwd);
        else
            add_pend3(slot, pe[i].target, (int)pe[i].kind, (int)pe[i].fwd);
    }
    g_t2n = (int)h.n_t2;
    for (uint64_t i = 0; i < h.n_t2; i++) {
        g_t2gpc[i] = te[i].gpc;
        g_t2cnt[i] = te[i].cnt ? te[i].cnt : 1; // 0 = promotion was pending; a 0 counter would wrap, never fire
    }
    // SMC precise gate: re-mark every guest page the restored blocks were translated from, so a warm-run
    // `ic ivau` against restored code still takes the conservative wholesale drop.
    txpg_clear();
    txln_clear(); // restored blocks carry page info only; the line set stays empty ->
                  // smc_icflush's coarse page fallback (g_pcache_loaded) covers restored code
    for (uint64_t i = 0; i < h.n_txpg; i++)
        if (tx[i]) txpg_put(tx[i]);
    g_pc_nprov = 0;
    uint64_t prov_deferred = 0;
    for (uint64_t i = 0; i < h.n_prov; i++) {
        if (pc_range_fixed(pv[i].guest, pv[i].guest + 4)) {
            jit_instruction_map_put((uint64_t)g_cache + pv[i].host_off, (uint64_t)g_cache + pv[i].host_off + pv[i].size,
                                    pv[i].guest);
            g_pc_prov[g_pc_nprov++] = pv[i];
        } else if (pc_range_in_lib(pv[i].guest, pv[i].guest + 4)) {
            prov_deferred++;
        }
    }
    if (prov_deferred) {
        g_pc_prov_defer = malloc((size_t)prov_deferred * sizeof *g_pc_prov_defer);
        if (g_pc_prov_defer)
            for (uint64_t i = 0; i < h.n_prov; i++)
                if (!pc_range_fixed(pv[i].guest, pv[i].guest + 4) && pc_range_in_lib(pv[i].guest, pv[i].guest + 4))
                    g_pc_prov_defer[g_pc_nprov_defer++] = pv[i];
    }
    g_cp = g_cache + h.arena_used;
    free(re);
    free(me);
    free(pe);
    free(te);
    free(tx);
    free(pv);
    free(libs);

    // Commit the arena bytes + re-emit every baked host pointer, then publish to the I-cache.
    if (!jit_wprot(0)) {
        free(abuf);
        return 0;
    }
    memcpy(g_cache, abuf, h.arena_used);
    pcache_relocate(h.arena_rx_at);
    if (!jit_wprot(1) || !jit_publish_code(J_RX(g_cache), h.arena_used)) {
        free(abuf);
        return 0;
    }
    memset(g_ibtc, 0, sizeof g_ibtc); // shared IBTC data table: refills lazily
    free(abuf);
    g_pcache_loaded = 1;
    // Restored blocks carry page info only (their lines were not decoded here); keep the historical
    // page-fallback path for them by arming eager line recording for every block translated from now on,
    // matching the pre-lazy behaviour for a warm-loaded arena.
    g_txln_active = 1;
    /*
     * Restored blocks predate the live line-content table and can contain
     * direct caller ingress.  The first warm-image SMC must therefore perform
     * the normal activation prime before later events become precisely
     * source-targeted.
     */
    g_txln_prime = 1;
    if (g_coldprof)
        fprintf(stderr, "[pcache] load %llu B arena, %llu blocks, %llu reloc in %.3f ms\n",
                (unsigned long long)h.arena_used, (unsigned long long)h.n_mapent, (unsigned long long)h.n_reloc,
                (now_ns() - t0) / 1e6);
    return 1;
}

// Persist the current arena + maps (atomic temp+rename). Refuses after a load (snowball), from a
// fork child (stale bookkeeping), when poisoned (unrecorded baked pointer), after guest SMC
// (non-file code in the arena), or when a fixed-VA map fell back (mixed-base arena). The snapshot is
// taken under g_jit_lock so a live peer thread (threaded exit_group) can never tear it.
static void pcache_save(void) {
    if (!g_pcache || hl_identity_digest_empty(&g_pc_binid) || g_cp == g_cache) return;
    if (g_pcache_poison || g_pcache_loaded || g_pcache_forked || g_force_base_failed || smc_seen()) return;
    if (!jit_guest_bus_active()) return; // unguarded blocks must never reach a file that outlives this process
    uint64_t t0 = g_coldprof ? now_ns() : 0;
    char path[1024];
    if (!pcache_file(path, sizeof path)) return;
    // ---- consistent snapshot under the translation lock (peers may still be running/translating) ----
    pthread_mutex_lock(&g_jit_lock);
    uint64_t nmap = 0;
    for (uint32_t i = 0; i < map_capacity(); i++)
        if (map_live(i) && (pc_range_fixed(g_map_metadata[i].guest_start, g_map_metadata[i].guest_end) ||
                            pc_range_in_lib(g_map_metadata[i].guest_start, g_map_metadata[i].guest_end)))
            nmap++;
    uint64_t ntxpg = 0;
    for (uint32_t i = 0; i < TXPG_N; i++)
        if (g_txpg[i] && (pc_range_fixed(g_txpg[i], g_txpg[i] + UINT64_C(4096)) ||
                          pc_range_in_lib(g_txpg[i], g_txpg[i] + UINT64_C(4096))))
            ntxpg++;
    uint64_t nprov = 0;
    for (uint32_t i = 0; i < g_pc_nprov; i++)
        if (pc_range_fixed(g_pc_prov[i].guest, g_pc_prov[i].guest + 4) ||
            pc_range_in_lib(g_pc_prov[i].guest, g_pc_prov[i].guest + 4))
            nprov++;
    uint64_t arena_used = (uint64_t)(g_cp - g_cache);
    struct pc_hdr h;
    memset(&h, 0, sizeof h);
    h.magic = PC_MAGIC;
    h.version = PC_VERSION_EFF;
    h.translator_abi = PC_TRANSLATOR_ABI;
    h.cpu_sz = sizeof(struct cpu);
    h.jit_map_n = JIT_MAP_N;
    h.ibtc_n = IBTC_N;
    h.img_base = PC_IMG_BASE;
    h.interp_base = PC_INTERP_BASE;
    h.bin_id = g_pc_binid;
    h.entry_jump = g_pc_entry;
    h.arena_used = arena_used;
    h.n_reloc = (uint64_t)g_nreloc;
    h.n_mapent = nmap;
    h.n_pend = (uint64_t)g_npend;
    h.n_t2 = (uint64_t)g_t2n;
    h.n_txpg = ntxpg;
    h.n_prov = nprov;
    h.n_lib = (uint64_t)g_pc_nlib;
    h.block_return_at = (uint64_t)block_return;
    h.ibtc_at = (uint64_t)g_ibtc;
    h.arena_rx_at = (uint64_t)J_RX(g_cache);
    // Build the whole image in one heap buffer -> one write() (per-record writes dominated the save cost).
    size_t total = sizeof h + (size_t)g_nreloc * sizeof(hl_reloc) + (size_t)nmap * sizeof(struct pc_mapent) +
                   (size_t)g_npend * sizeof(struct pc_pend) + (size_t)g_t2n * sizeof(struct pc_t2) +
                   (size_t)ntxpg * sizeof(uint64_t) + (size_t)nprov * sizeof(struct pc_prov) +
                   (size_t)g_pc_nlib * sizeof(struct pc_lib) + arena_used;
    uint8_t *buf = malloc(total);
    int ok = buf != NULL;
    if (ok) {
        uint8_t *w = buf + sizeof h; // header written last (its csum covers everything after it)
        memcpy(w, g_reloc, (size_t)g_nreloc * sizeof(hl_reloc));
        w += (size_t)g_nreloc * sizeof(hl_reloc);
        for (uint32_t i = 0; i < map_capacity(); i++) {
            if (!map_live(i) || !(pc_range_fixed(g_map_metadata[i].guest_start, g_map_metadata[i].guest_end) ||
                                  pc_range_in_lib(g_map_metadata[i].guest_start, g_map_metadata[i].guest_end)))
                continue;
            // The hot map entry stays 32 bytes; cold source bounds live in parallel arrays and are persisted
            // so a warm-loaded block remains individually invalidatable after guest code rewrites.
            struct pc_mapent e = {g_map[i].gpc, g_map_metadata[i].guest_start, g_map_metadata[i].guest_end,
                                  (uint64_t)((uint8_t *)g_map[i].host - g_cache),
                                  (uint64_t)((uint8_t *)g_map[i].body - g_cache)};
            memcpy(w, &e, sizeof e);
            w += sizeof e;
        }
        for (int i = 0; i < g_npend; i++) {
            struct pc_pend e = {(uint64_t)((uint8_t *)g_pend[i].slot - g_cache),
                                g_pend[i].target,
                                g_pend[i].source_gpc,
                                (uint32_t)g_pend[i].is_bl,
                                (uint32_t)g_pend[i].fwd,
                                g_pend[i].orig,
                                0};
            memcpy(w, &e, sizeof e);
            w += sizeof e;
        }
        for (int i = 0; i < g_t2n; i++) {
            struct pc_t2 e = {g_t2gpc[i], g_t2cnt[i]};
            memcpy(w, &e, sizeof e);
            w += sizeof e;
        }
        for (uint32_t i = 0; i < TXPG_N; i++)
            if (g_txpg[i] && (pc_range_fixed(g_txpg[i], g_txpg[i] + UINT64_C(4096)) ||
                              pc_range_in_lib(g_txpg[i], g_txpg[i] + UINT64_C(4096)))) {
                memcpy(w, &g_txpg[i], 8);
                w += 8;
            }
        for (uint32_t i = 0; i < g_pc_nprov; i++)
            if (pc_range_fixed(g_pc_prov[i].guest, g_pc_prov[i].guest + 4) ||
                pc_range_in_lib(g_pc_prov[i].guest, g_pc_prov[i].guest + 4)) {
                memcpy(w, &g_pc_prov[i], sizeof g_pc_prov[i]);
                w += sizeof g_pc_prov[i];
            }
        memcpy(w, g_pc_libs, (size_t)g_pc_nlib * sizeof(struct pc_lib));
        w += (size_t)g_pc_nlib * sizeof(struct pc_lib);
        memcpy(w, g_cache, arena_used); // read from the RW alias is always permitted
        h.csum = hl_digest_bytes(HL_DIGEST_SEED, buf + sizeof h, total - sizeof h);
        memcpy(buf, &h, sizeof h);
    }
    pthread_mutex_unlock(&g_jit_lock);
    if (ok) ok = hl_persist_store_at(&g_pc_directory, path, buf, total);
    free(buf);
    if (g_coldprof)
        fprintf(stderr, "[pcache] save %s (%llu B arena, %llu blocks, %d reloc) in %.3f ms\n", ok ? "ok" : "FAILED",
                (unsigned long long)arena_used, (unsigned long long)nmap, g_nreloc, (now_ns() - t0) / 1e6);
}

// Poison the cache if a non-default codegen mode that bakes an UNRECORDED host pointer is active (their
// counters/logs are emitted via raw e_movconst/adrp of BSS addresses with no reloc record). Called once
// at engine init, after the mode flags are read.
