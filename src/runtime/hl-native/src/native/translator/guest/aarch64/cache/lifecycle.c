static void pcache_poison_check(void) {
    if (g_prof) g_pcache_poison = 1;
}

// ---- guest fork hook (proc.c, both clone/fork sites, in the child, right after jit_after_fork) ----
// The child either KEPT the parent's warm arena (preserved-arena fork: single-threaded parent /
// MAP_JIT fallback) or got a fresh empty one (threaded rebuild); either way its arena from here on is a
// fork-private slice whose inherited g_pc_binid/g_pc_entry identity belongs to the PARENT's complete
// image -- so drop the inherited reloc records and bar this process from saving. Execve may safely LOAD
// an independently-published cache after re-keying, but it must not publish the child epoch: the translated
// arena still descends from the fork snapshot. A later exec may publish only after its complete
// identity/arena/relocation/provenance/library reset in pcache_exec_reload.
static void pcache_after_fork(void) {
    hl_reloc_reset(&g_reloc_table);
    g_pc_nprov = 0;
    // Do not call the allocator in the immediate fork hook. The inherited buffers remain owned by this
    // process and are released at the next exec reset; zero counts make them unreachable in this epoch.
    g_pc_ndefer = 0;
    g_pc_nprov_defer = 0;
    g_pcache_forked = 1;
}

#define PCACHE_FORK_HOOK pcache_after_fork()

// ---- wholesale-flush hook (engine/dispatch.c, after the cache-full in-place or stop-the-world flush) --
// The arena content the records described is gone (bump pointer reset / fresh arena). Everything emitted
// from here on re-records against the new arena, so the "every baked pointer recorded" invariant holds by
// construction after a plain reset. (A restored-then-flushed run is already barred from saving by
// g_pcache_loaded; this keeps the cold-run bookkeeping correct too.)
static void pcache_after_wholesale_flush(void) {
    hl_reloc_reset(&g_reloc_table);
    g_pc_nprov = 0;
    free(g_pc_defer);
    g_pc_defer = NULL;
    g_pc_ndefer = 0;
    free(g_pc_prov_defer);
    g_pc_prov_defer = NULL;
    g_pc_nprov_defer = 0;
}

#define PCACHE_FLUSH_HOOK pcache_after_wholesale_flush()

// ---- guest execve (proc.c case 221) hooks ----
// The go-build fork+execve storm re-loads a toolchain binary (compile/asm/link) IN-PROCESS from a COLD,
// freshly jit_after_fork()'d arena; these let that reload restore the binary's warm arena from the cache.
// Gated behind PCACHE_EXEC_HOOKS so the SHARED proc.c compiles unchanged for the x86 engine.
static void pcache_exec_force_main(void) {
    if (g_pcache) {
        g_force_base = PC_IMG_BASE;
        g_force_base_failed = 0; // fresh image, fresh verdict
        g_pc_img_lo = g_pc_img_hi = g_pc_interp_lo = g_pc_interp_hi = 0;
    }
}

static void pcache_exec_force_interp(void) {
    if (g_pcache) g_force_base = PC_INTERP_BASE;
}

static void pcache_exec_reload(uint64_t program, uint64_t interpreter, const char *argv0, uint64_t jump) {
    if (!g_pcache) return;
    // Execve has flushed the old arena and installed a new fixed-base image. Reset every cache-production
    // datum in lockstep with that identity boundary. The new epoch may publish even when its process was
    // originally forked: no inherited relocation, provenance, map, or library state survives this reset.
    hl_reloc_reset(&g_reloc_table);
    g_pc_nprov = 0;
    g_t2n = 0;    // fresh tier-2 slot set for the new image (no cross-image alias)
    txpg_clear(); // nothing is translated now; the set re-fills (or is restored by the load below)
    txln_clear();
    g_pcache_loaded = 0; // allow a cold-miss save of the NEW binary
    g_pcache_forked = 0;
    free(g_pc_defer);
    g_pc_defer = NULL;
    g_pc_ndefer = 0;
    free(g_pc_prov_defer);
    g_pc_prov_defer = NULL;
    g_pc_nprov_defer = 0;
    g_pc_nlib = 0;
    __atomic_store_n(&g_pc_lib_next, PC_LIB_BASE, __ATOMIC_RELAXED);
    g_pc_binid = pcache_make_id(program, interpreter, argv0);
    g_pc_entry = jump;
    int hit = pcache_load(jump);
    if (g_coldprof) fprintf(stderr, "[pcache] exec %s reloc=%d\n", hit ? "HIT" : "MISS", g_nreloc);
}

#define PCACHE_EXEC_HOOKS 1

#define PCACHE_SAVE_HOOK pcache_save()
