static void pcache_note_fixed_img(uint64_t base, uint64_t span) {
    if (base > UINT64_MAX - span) return;
    if (base >= PC_INTERP_BASE) {
        g_pc_interp_lo = base;
        g_pc_interp_hi = base + span;
    } else if (base >= PC_IMG_BASE) {
        g_pc_img_lo = base;
        g_pc_img_hi = base + span;
    }
}

static int pc_range_fixed(uint64_t start, uint64_t end) {
    return start < end &&
           ((start >= g_pc_img_lo && end <= g_pc_img_hi) || (start >= g_pc_interp_lo && end <= g_pc_interp_hi));
}

static int pc_range_in_lib(uint64_t start, uint64_t end) {
    if (start >= end) return 0;
    for (int i = 0; i < g_pc_nlib; i++) {
        uint64_t limit = g_pc_libs[i].base + g_pc_libs[i].len;
        if (limit >= g_pc_libs[i].base && start >= g_pc_libs[i].base && end <= limit) return 1;
    }
    return 0;
}

static uint64_t pcache_mmap_hint(uint64_t len) {
    if (!g_pcache || len > UINT64_MAX - UINT64_C(0x1fffff)) return 0;
    uint64_t rounded = (len + UINT64_C(0x1fffff)) & ~UINT64_C(0x1fffff);
    if (rounded > UINT64_MAX - UINT64_C(0x200000)) return 0;
    uint64_t span = rounded + UINT64_C(0x200000);
    uint64_t base = __atomic_fetch_add(&g_pc_lib_next, span, __ATOMIC_RELAXED);
    if (base < PC_LIB_BASE || base > PC_LIB_BASE + PC_LIB_SPAN || span > PC_LIB_BASE + PC_LIB_SPAN - base) return 0;
    return base;
}

static void pcache_note_libmap(uint64_t base, uint64_t len, const hl_host_file_metadata *metadata) {
    if (!g_pcache || !metadata || !len || base > UINT64_MAX - len) return;
    uint64_t id = hl_identity_file(metadata);
    if (!id) return;
    if (!g_pcache_loaded) {
        if (g_threaded) pthread_mutex_lock(&g_jit_lock);
        if (g_pc_nlib < PC_LIB_MAX) g_pc_libs[g_pc_nlib++] = (struct pc_lib){base, len, id};
        if (g_threaded) pthread_mutex_unlock(&g_jit_lock);
        return;
    }
    for (int i = 0; i < g_pc_nlib; i++) {
        if (g_pc_libs[i].base != base) continue;
        if (g_pc_libs[i].len != len || g_pc_libs[i].id != id) return;
        if (g_threaded) pthread_mutex_lock(&g_jit_lock);
        for (uint64_t j = 0; j < g_pc_ndefer; j++) {
            struct pc_mapent *entry = &g_pc_defer[j];
            if (entry->host_off && entry->gpc >= base && entry->gpc < base + len) {
                if (map_put(entry->gpc, entry->guest_start, entry->guest_end, g_cache + entry->host_off,
                            g_cache + entry->body_off) == MAP_PUT_OK)
                    entry->host_off = 0;
            }
        }
        for (uint64_t j = 0; j < g_pc_nprov_defer; j++) {
            struct pc_prov *entry = &g_pc_prov_defer[j];
            if (entry->size && entry->guest >= base && entry->guest < base + len) {
                jit_instruction_map_put((uint64_t)g_cache + entry->host_off,
                                        (uint64_t)g_cache + entry->host_off + entry->size, entry->guest);
                entry->size = 0;
            }
        }
        if (g_threaded) pthread_mutex_unlock(&g_jit_lock);
        return;
    }
}
