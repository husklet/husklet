// W4E tier-2: promote a hot self-loop (its in-cache counter hit the threshold and exited R_TIER2 with
// pc == gpc). Recompile the block with the folded back-edge (no counter), then SWAP it in under live
// execution: emit+icache-flush the tier-2 code, repoint the live map entry, repoint any still-pending
// chains, and drop a stale IBTC entry so the indirect path refills to tier-2. The old tier-1 code is left
// in place (harmless dead bytes). Single-threaded only -- promotion mutates the cache outside the threaded
// translate-lock discipline, so it's skipped once a guest thread exists (loop keeps running tier-1, still
// correct). Caller is the dispatcher between block runs, so guest state is settled.
static void tier2_promote(uint64_t gpc) {
    /*
     * Promotion leaves predecessor bounces and a redirected tier-1 body.  The
     * first SMC wholesale drop makes older promotions unreachable; do not
     * create new cross-block ingress while translations are individually
     * invalidatable.
     */
    if (g_threaded || g_notier2 || smc_seen()) return;
    // Promotion emits a fresh (folded) block at g_cp, but it runs from the dispatcher's post-run reason
    // handling -- OUTSIDE the cache-full check, which only fires on a translate MISS (dispatch.c). A run of
    // hot loops that all reach threshold between two misses promotes back-to-back with NO intervening
    // headroom test, so near a nearly-full cache the emit runs past g_cache+CACHE_SZ and scribbles over
    // whatever the kernel mapped after the arena (guest heap/image) -> a corrupted guest pointer surfaces
    // much later as a wild store (window-gated, common only when the cache churns/flushes often).
    // Demand the same headroom a normal translate does; if it's not there, skip promotion. That is always
    // safe: the loop keeps running its correct tier-1 body (the spent down-counter simply wraps past 0 and
    // stops re-raising R_TIER2), and it can promote later once a flush has reset the arena.
    if (g_cp + CACHE_EMIT_HEADROOM > g_cache + CACHE_SZ) return;
    int mi = map_idx(gpc);
    if (mi < 0) return;
    if (!jit_wprot(0)) return;
    g_emit_start = g_cp;
    g_tier2_build = 1;
    void *nh = translate_block(gpc); // folded recompile; no counter, no map_put
    void *nb = g_last_body;
    g_tier2_build = 0;
    if (0) {
        fprintf(stderr, "[t2dump] gpc=%llx body+%ld:", (unsigned long long)gpc, (long)((uint8_t *)nb - (uint8_t *)nh));
        for (uint32_t *p = (uint32_t *)nb; (uint8_t *)p < g_cp; p++)
            fprintf(stderr, " %08x", *p);
        fprintf(stderr, "\n");
    }
    // make the tier-2 code coherent on all cores BEFORE anything can branch into it
    if (!jit_publish_code(g_emit_start, (size_t)(g_cp - g_emit_start))) {
        (void)jit_wprot(1);
        return;
    }
    // Redirect the OLD tier-1 body to tier-2: overwrite its first instruction with `b nb`. Chains from
    // predecessors were resolved to the old body when they were translated (patch_links_to only fixes
    // still-PENDING ones), so without this an outer loop re-entering this inner loop would keep hitting
    // the spent counter stub. The bounce costs one branch per loop ENTRY (negligible vs the loop body).
    void *old_body = g_map[mi].body;
    int64_t bd = ((uint8_t *)nb - (uint8_t *)old_body) / 4;
    *(uint32_t *)old_body = 0x14000000u | ((uint32_t)bd & 0x3FFFFFFu);
    // IRQSLIM: forward chains enter at body+8 (past the 2-insn poll) and would miss the body+0
    // bounce -- give the poll-skipping entry its own bounce to nb+8 (tier-2 has the same layout).
    if (g_fwdskip) {
        int64_t bd8 = (((uint8_t *)nb + 8) - ((uint8_t *)old_body + 8)) / 4;
        ((uint32_t *)old_body)[2] = 0x14000000u | ((uint32_t)bd8 & 0x3FFFFFFu);
    }
    if (!jit_publish_code(old_body, 4 + (g_fwdskip ? 8 : 0))) {
        (void)jit_wprot(1);
        return;
    }
    // swap the live map entry: future dispatcher lookups + IBTC fills resolve to tier-2 directly
    g_map[mi].host = nh;
    g_map[mi].body = nb;
    // repoint any still-unresolved chains to this gpc straight at the tier-2 body
    patch_links_to(gpc, nb);
    // drop a stale IBTC entry (if this block is an indirect-branch target) so it refills to tier-2
    uint32_t h = (uint32_t)((gpc >> 2) & (IBTC_N - 1));
    if (g_ibtc[h].target == gpc) {
        g_ibtc[h].target = 0;
        g_ibtc[h].body = NULL;
    }
    if (!jit_wprot(1)) return;
    g_prof_t2++;
}
