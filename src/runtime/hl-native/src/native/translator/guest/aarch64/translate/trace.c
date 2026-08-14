static int smc_disabled(void) {
    return 0;
}

static void *translate_block(uint64_t gpc);
static uint64_t g_last_guest_start;
static uint64_t g_last_guest_end;

static void smc_queue_line(struct cpu *c, uint64_t address) {
    if (g_prof) g_prof_smc_queued++;
    /*
     * ET_EXEC code is mapped at a high collision-avoidance bias while its
     * architectural pointers remain link-time-low.  Translation-map source
     * intervals use the real executable address, so normalize an ic-ivau
     * operand exactly like instruction dispatch does before classifying it.
     */
    if (g_nonpie_lo && address >= g_nonpie_lo && address < g_nonpie_hi) address += g_nonpie_bias;
    uint64_t start = address & ~UINT64_C(63), end = start + 64;
    for (uint32_t i = 0; i < c->smc_range_count; i++) {
        if (end < c->smc_ranges[i][0] || start > c->smc_ranges[i][1]) continue;
        if (start < c->smc_ranges[i][0]) c->smc_ranges[i][0] = start;
        if (end > c->smc_ranges[i][1]) c->smc_ranges[i][1] = end;
        return;
    }
    if (c->smc_range_count == SMC_RANGE_CAP) {
        c->smc_range_overflow = 1;
        return;
    }
    c->smc_ranges[c->smc_range_count][0] = start;
    c->smc_ranges[c->smc_range_count][1] = end;
    c->smc_range_count++;
}

/*
 * Syscall copy-to-user writes bypass translated store sites.  Queue the
 * guest-visible destination now; logical executable aliases are added by the
 * immutable alias visitor when present, while architectural publication still
 * occurs at the guest's ic-ivau/isb boundary.
 */
static void aarch64_smc_queue_range(uint64_t first, uint64_t last, void *opaque) {
    struct cpu *c = opaque;
    for (uint64_t line = first & ~UINT64_C(63); line < last;) {
        smc_queue_line(c, line);
        if (line > UINT64_MAX - 64) break;
        line += 64;
    }
}

static void aarch64_smc_copyout(uint64_t first, uint64_t last) {
    if (last <= first) return;
    struct cpu *c = pthread_getspecific(g_cpu_key);
    if (c == NULL) return;
    aarch64_smc_queue_range(first, last, c);
    hl_logical_vma_visit_exec_aliases(first, last, aarch64_smc_queue_range, c);
}

/* Return 1 to retry the instruction, 0 for a guest protection fault. */
static int aarch64_soft_tlb_miss(struct cpu *c) {
    // On a host whose VM granule is wider than Linux's 4 KB page, munmap may
    // leave the containing host page physically present to preserve an adjacent live guest page.
    // gna is the architectural accessibility ledger; consult it before the identity fallback so
    // that retained physical backing never makes a logical hole readable.
    if (gna_hit(c->soft_ea, c->soft_bytes ? c->soft_bytes : 1)) return 0;
    void *host = NULL;
    size_t contiguous = 0;
    uint32_t protection = HL_LOGICAL_VMA_READ | HL_LOGICAL_VMA_WRITE | HL_LOGICAL_VMA_EXEC;
    int resolved = hl_logical_vma_resolve_data(c->soft_ea, 1, (uint32_t)c->soft_required, &host, &contiguous);
    if (resolved < 0) return 0;
    uint64_t first, last;
    if (resolved) {
        hl_logical_vma_snapshot *snapshot =
            atomic_load_explicit(hl_logical_vma_global_snapshot_source(), memory_order_acquire);
        const hl_logical_vma_view *view = NULL;
        if (snapshot != NULL)
            for (size_t index = 0; index < snapshot->count; ++index)
                if (c->soft_ea >= snapshot->views[index].guest_first &&
                    c->soft_ea < snapshot->views[index].guest_last) {
                    view = &snapshot->views[index];
                    break;
                }
        if (view == NULL) return 0;
        first = view->guest_first;
        last = view->guest_last;
        protection = view->protection;
    } else {
#if !defined(__APPLE__)
        if (!hl_host_range_mapped((uintptr_t)c->soft_ea, 1)) return 0;
#endif
        first = c->soft_ea & ~UINT64_C(4095);
        last = first + UINT64_C(4096);
    }
    if (c->soft_bytes == 0 || c->soft_bytes > last - first || c->soft_ea > last - c->soft_bytes) {
        c->reason = R_SOFTSPAN;
        return 1;
    }
    c->soft_page = first;
    c->soft_limit = last;
    c->soft_delta = resolved ? (uint64_t)(uintptr_t)host - c->soft_ea : 0;
    c->soft_protection = protection;
    c->reason = R_BRANCH;
    return 1;
}

/*
 * Return 1 when the complete cross-page access has one host delta and can be
 * retried natively, 0 for a protection fault, and -1 when valid adjacent
 * guest pages have discontinuous canonical storage (the architectural split
 * slow path must handle that case).
 */
static int aarch64_soft_span_copy(struct cpu *c, int to_guest, int copy_bytes) {
    uint64_t cursor = c->soft_ea;
    size_t done = 0;
    while (done < c->soft_bytes) {
        void *host = NULL;
        size_t contiguous = 0;
        uint32_t required = to_guest ? HL_LOGICAL_VMA_WRITE : HL_LOGICAL_VMA_READ;
        int resolved = hl_logical_vma_resolve_data(cursor, 1, required, &host, &contiguous);
        if (resolved < 0) return 0;
        if (!resolved) {
            size_t page_left = 4096u - (size_t)(cursor & 4095u);
            if (!hl_host_range_mapped((uintptr_t)cursor, 1)) return 0;
            host = (void *)(uintptr_t)cursor;
            contiguous = page_left;
        }
        size_t take = (size_t)c->soft_bytes - done;
        if (take > contiguous) take = contiguous;
        if (!take) return 0;
        if (copy_bytes) {
            if (to_guest)
                memcpy(host, c->soft_bounce + done, take);
            else
                memcpy(c->soft_bounce + done, host, take);
        }
        cursor += take;
        done += take;
    }
    return 1;
}

static int aarch64_soft_prepare_bounce(struct cpu *c) {
    int ok = 0;
    uint32_t in = a64_fetch_instruction(c->soft_pc, &ok);
    int single = (in & 0x3B000000u) == 0x39000000u || (in & 0x3B200000u) == 0x38000000u;
    int pair = (in & 0x3A000000u) == 0x28000000u;
    int structure = is_advsimd_struct(in);
    int literal = (in & 0xBF000000u) == 0x18000000u || (in & 0xFF000000u) == 0x98000000u ||
                  ((in & 0x3F000000u) == 0x1C000000u && ((in >> 30) & 3) != 3);
    int atomic = (in & 0x3B200C00u) == 0x38200000u || (in & 0x3F000000u) == 0x08000000u || is_casp(in);
    if (!ok || !(single || pair || structure || literal) || atomic || c->soft_bytes == 0 ||
        c->soft_bytes > sizeof(c->soft_bounce))
        return -1;
    int write = (c->soft_required & HL_LOGICAL_VMA_WRITE) != 0;
    if (!aarch64_soft_span_copy(c, write, 0)) return 0; /* validate every span first */
    if (!write && !aarch64_soft_span_copy(c, 0, 1)) return 0;
    /*
     * No asynchronous signal may observe the architectural store after it
     * changed the bounce but before scatter.  This is a cold discontinuous
     * path: block host delivery across the single-instruction retry and
     * restore the exact prior mask in R_SOFTCOMMIT.  Synchronous faults remain
     * deliverable, but the validated aligned bounce access cannot fault.
     */
    sigset_t all;
    sigfillset(&all);
    _Static_assert(sizeof(sigset_t) <= sizeof(c->soft_bounce_host_mask), "soft bounce host-mask storage is too small");
    if (pthread_sigmask(SIG_BLOCK, &all, (sigset_t *)(void *)c->soft_bounce_host_mask) != 0) return 0;
    c->soft_bounce_write = (uint64_t)write;
    c->soft_bounce_pending = 1;
    c->soft_page = c->soft_ea;
    c->soft_limit = c->soft_ea + c->soft_bytes;
    c->soft_delta = (uint64_t)(uintptr_t)c->soft_bounce - c->soft_ea;
    c->soft_protection = c->soft_required;
    c->reason = R_BRANCH;
    if (g_prof) g_prof_soft_bounce_prepare++;
    return 1;
}

static int aarch64_soft_bounce_commit(struct cpu *c) {
    if (!c->soft_bounce_pending) return 1;
    if (g_prof) g_prof_soft_bounce_commit++;
    int ok = !c->soft_bounce_write || aarch64_soft_span_copy(c, 1, 1);
    if (ok && c->soft_bounce_write) aarch64_smc_copyout(c->soft_ea, c->soft_ea + c->soft_bytes);
    c->soft_bounce_pending = 0;
    c->soft_page = UINT64_MAX;
    c->soft_protection = 0;
    (void)pthread_sigmask(SIG_SETMASK, (const sigset_t *)(const void *)c->soft_bounce_host_mask, NULL);
    return ok;
}

static int aarch64_soft_tlb_span(struct cpu *c) {
    if (c->soft_bytes == 0 || c->soft_ea > UINT64_MAX - c->soft_bytes) return 0;
    uint64_t cursor = c->soft_ea, last = cursor + c->soft_bytes, delta = 0;
    int have_delta = 0;
    while (cursor < last) {
        void *host = NULL;
        size_t contiguous = 0;
        int resolved = hl_logical_vma_resolve_data(cursor, 1, (uint32_t)c->soft_required, &host, &contiguous);
        if (resolved < 0) return 0;
        uint64_t part_delta = resolved ? (uint64_t)(uintptr_t)host - cursor : 0;
        if (have_delta && part_delta != delta) return aarch64_soft_prepare_bounce(c);
        delta = part_delta;
        have_delta = 1;
        uint64_t step = UINT64_C(4096) - (cursor & UINT64_C(4095));
        if (resolved && contiguous < step) step = contiguous;
        if (last - cursor < step) step = last - cursor;
        if (step == 0) return 0;
        cursor += step;
    }
    c->soft_page = c->soft_ea;
    c->soft_limit = c->soft_ea + c->soft_bytes;
    c->soft_delta = delta;
    c->soft_protection = c->soft_required;
    c->reason = R_BRANCH;
    return 1;
}

static int smc_commit(struct cpu *c) {
    if (g_prof) g_prof_smc_commit++;
    pthread_mutex_lock(&g_jit_lock);
    txln_activate();                // arm eager line recording; may request a priming wholesale drop
    int force_whole = g_txln_prime; // first SMC after lazy activation: no lines recorded -> can't classify
    g_txln_prime = 0;
    if (!force_whole && !c->smc_range_count && !c->smc_range_overflow) {
        pthread_mutex_unlock(&g_jit_lock);
        return 1;
    }
    /* Freeze peer writers before hashing whole cache lines. Distinct generated-code slots share a
       line: classifying before the rendezvous can record a peer's new bytes before that peer has
       invalidated its stale translation, making its later flush look unchanged. */
    stw_mapping_begin_locked();
    __atomic_store_n(&g_smc_seen, 1, __ATOMIC_RELEASE);
    if (!c->smc_range_overflow && !force_whole) {
        uint32_t retained = 0;
        for (uint32_t i = 0; i < c->smc_range_count; i++) {
            uint64_t dirty_start = UINT64_MAX, dirty_end = 0;
            for (uint64_t line = c->smc_ranges[i][0]; line < c->smc_ranges[i][1]; line += 64) {
                int classification = txln_flush_class(line);
                int warm_source = 0;
                if (classification == 0 && g_pcache_loaded) {
                    for (uint32_t n = 0; n < g_live_map_count; n++) {
                        uint32_t index = g_live_map_indices[n];
                        if (map_live(index) && map_source_overlaps(index, line, line + 64)) {
                            warm_source = 1;
                            break;
                        }
                    }
                }
                if (classification == 1 || warm_source) {
                    if (dirty_start == UINT64_MAX) dirty_start = line;
                    dirty_end = line + 64;
                }
            }
            if (dirty_start != UINT64_MAX) {
                c->smc_ranges[retained][0] = dirty_start;
                c->smc_ranges[retained][1] = dirty_end;
                retained++;
            }
        }
        c->smc_range_count = retained;
        if (!retained) {
            c->smc_range_count = 0;
            c->smc_range_overflow = 0;
            stw_mapping_end();
            return 1;
        }
    }
    /*
     * Do not rewrite live map entries in place here.  Besides leaving several
     * independent ingress paths to the old body (direct chains, shadow
     * returns and per-site ICs), recompiling every overlapping entry can emit
     * an unbounded amount of code during one dispatcher crossing.  Large JITs
     * such as Julia reached the end of the writable alias and the assembler
     * then faulted on the adjacent RX alias.
     *
     * All peers are brought to a dispatcher boundary, so invalidating every
     * lookup/chain is both simpler and coherent.  The old bytes remain mapped
     * and untouched until the ordinary capacity rotation retires them; no
     * executing host PC is invalidated.  Subsequent entries translate the
     * modified guest bytes on demand.
    */
#if HL_ENABLE_LOGGING
    uint32_t removed;
#endif
    if (force_whole || c->smc_range_overflow) {
#if HL_ENABLE_LOGGING
        removed = g_live_map_count;
#endif
        map_clear();
        memset(g_ibtc, 0, sizeof g_ibtc);
        txpg_clear();
    } else {
#if HL_ENABLE_LOGGING
        removed = map_invalidate_source_ranges((const uint64_t (*)[2])c->smc_ranges, c->smc_range_count);
#else
        (void)map_invalidate_source_ranges((const uint64_t (*)[2])c->smc_ranges, c->smc_range_count);
#endif
    }
    pend_reset();
    for (int i = 0; i < STW_MAXTHREAD; i++)
        if (atomic_load_explicit(&g_stw_threads[i].used, memory_order_relaxed) && g_stw_threads[i].cpu)
            g_stw_threads[i].cpu->ssp = 0;
    HL_LOGF(&g_jit_log, HL_LOG_TAG_JIT, "smc invalidate mode=%s ranges=%u removed=%u retained=%u",
            (force_whole || c->smc_range_overflow) ? "whole" : "targeted", c->smc_range_count, removed,
            g_live_map_count);
    g_smc_flushes++;
    stw_mapping_end();
    c->smc_range_count = 0;
    c->smc_range_overflow = 0;
    return 1;
}

// A guest `ic ivau` reached the dispatcher (R_ICFLUSH): the guest is about to execute code it just rewrote,
// so every gpc->host translation may be stale. Drop the whole block map + IBTC + pending chains (mirrors the
// x86 smc_on_write flush). We deliberately do NOT reset g_cp: the just-exited block's host code stays intact
// and is reclaimed by the normal wholesale flush; stale entries are simply re-emitted on demand. The §B
// shadow stack is left alone -- its host_rets point at old code that is still present in g_cp (valid targets).
// g_smc_seen latches so indirect branches stop populating the per-site IC (see G_IBTC_FILL): that literal
// lives in the unmodified CALLER block, which this flush cannot reach.
static void smc_icflush(struct cpu *c, uint64_t va) {
    // The guest issued `ic ivau` -> it generates/patches code -> the per-site monomorphic IC stays disabled
    // (its literal lives in an unmodified caller block this flush can't reach). Latch this unconditionally,
    // even when the precise gate below skips, so a code-modifying guest never trusts the per-site IC.
    __atomic_store_n(&g_smc_seen, 1, __ATOMIC_RELEASE);
    // PRECISE GATE: if the invalidated bytes were never translated, there is nothing stale to drop. A
    // code-generating guest flushes each freshly-written line as it grows its code space -> almost always
    // brand-new bytes -> this turns the catastrophic per-flush wholesale invalidation (which re-translated
    // the entire working set on every `ic ivau`) into a no-op. Gate at CACHE-LINE (64B) granularity -- the
    // exact unit `ic ivau, Xt` invalidates -- NOT at 4KB page granularity. BeamAsm (Erlang/OTP's
    // arm64 JIT) packs many compiled functions per page, so appending a NEW function onto a page that
    // already holds a translated one makes a page-granular gate fire a wholesale drop even though no
    // translated byte changed. Re-translating the whole working set on that spurious drop -- and, before the
    // thread-safety fix below, doing it unlocked -- crashed the heavily-threaded emulator. The line gate
    // makes those same-page appends a no-op (measured: 100% of BeamAsm's page-hit drops are line-misses),
    // while a genuine in-place overwrite (V8 patching a jump) still overlaps a translated line -> real drop.
    // pcache warm-load restores blocks with page info but no line info (see pcache.c), so for a restored
    // arena fall back to the coarse page gate -- conservative (may over-drop) but never misses stale code.
    // CONTENT GATE: classify the flush by whether the invalidated
    // translated line's bytes actually CHANGED. A benign re-flush of unchanged already-translated code
    // (a builtin/trampoline flushed as part of a range each call, or a block flushing its OWN executing
    // source line -- exactly what V8 does thousands of times at startup) must NOT trigger the wholesale
    // drop, or the entire working set re-translates on every flush and translate_block spins forever.
    //   class 0 -> line never translated: nothing stale (fall back to the coarse pcache page gate below).
    //   class 2 -> translated but UNCHANGED: benign icache maintenance -> keep the valid translation, skip.
    //   class 1 -> translated AND (first flush | genuinely rewritten): take the real drop (soak_smc/V8 patch).
    /*
     * Queue only.  smc_commit owns activation, membership/content
     * classification and hash mutation under g_jit_lock.  Besides avoiding a
     * race with translation, this ensures a changed line is classified once
     * rather than immediately looking unchanged on a second observation.
     */
    // ---- a GENUINE in-place modification of already-translated guest code (the line WAS a source line) ----
    // (BeamAsm SIGSEGV) coherence. smc_icflush runs from the dispatcher's post-run reason handler, which
    // has ALREADY released g_jit_lock (engine/dispatch.c: the unlock precedes G_DISPATCH_REASON), so a peer
    // guest thread may be executing translated code concurrently. A wholesale drop memsets g_map/g_ibtc that a
    // peer reads lock-free AND forces a re-translation of the modified bytes -- and there is no way to make
    // that coherent while other threads run. Two approaches were measured and BOTH fault BeamAsm:
    //   * stop-the-world + fresh cache (jit_flush_to_fresh): parked peers resume in the RETIRED cache running
    //     the STALE translation while freshly-dispatched threads run the RE-translated code -> two live
    //     versions of the modified function at once.
    //   * stop-the-world + in-place drop (keep g_cp): the old arena stays mapped, so a resuming peer follows
    //     baked-in direct chains straight into stale old blocks -> same two-version split.
    // The split is what an async/dirty scheduler thread trips over the instant it re-enters a modified region.
    // The coherent choice under live peers is to keep the SINGLE existing translation for EVERY thread and NOT
    // re-translate: g_smc_seen (latched above) already disables the per-site monomorphic IC so a code-
    // modifying guest never trusts a baked body, and the guest re-synchronizes through the shared indirect
    // dispatch. This matches hl's long-standing NOSMC fallback and is exactly what lets Erlang/OTP + Elixir
    // (BeamAsm) run to completion, including external-program ports (os:cmd / open_port {spawn,...}) whose
    // forker relies on the emulator staying alive. Fully coherent re-translation of a multithreaded
    // in-place patch would need precise per-block recompile+redirect with all peers rendezvoused at a
    // safepoint (the tier2_promote bounce, generalized) -- out of scope here; a guest that depends on such a
    // patch keeps running the prior version instead of crashing. The LINE-granular gate above keeps genuine
    // in-place hits rare (a code-generator that merely APPENDS onto a shared page never reaches here), so this
    // fallback is taken only on a true overwrite of executed code.
    // Single-threaded (incl. all peers exited): the wholesale in-place drop IS coherent -- one thread, no
    // split -- so re-translate for correct self-modification (a single-isolate V8, the soak_smc test).
    smc_queue_line(c, va);
}

static void emit_smc_queue(int va_register) {
    assert(g_steal1617);
    if (is_stolen(va_register))
        e_ldr(16, CPUREG, va_register * 8);
    else
        e_movr(16, va_register);
    emit32(0x927AE610u); /* and x16,x16,#-64 */
    e_str(16, CPUREG, OFF_SMCVA);
    e_ldr(16, CPUREG, OFF_SMC_RANGE_COUNT);
    uint32_t *empty = (uint32_t *)g_cp;
    emit32(0); /* cbz x16,append */
    e_subi(16, 16, 1);
    e_addlsl4(17, CPUREG, 16);
    unsigned offset = (unsigned)OFF_SMC_RANGES;
    if (offset >= 4096) {
        emit32(0x91400000u | (((offset >> 12) & 0xfffu) << 10) | (17u << 5) | 17u);
        offset &= 0xfffu;
    }
    if (offset) e_addi(17, 17, offset);
    e_ldr(17, 17, 8);
    e_ldr(16, CPUREG, OFF_SMCVA);
    emit32(0xCB110211u); /* sub x17,x16,x17 */
    uint32_t *not_adjacent = (uint32_t *)g_cp;
    emit32(0); /* cbnz x17,append */
    e_ldr(16, CPUREG, OFF_SMC_RANGE_COUNT);
    e_subi(16, 16, 1);
    e_addlsl4(17, CPUREG, 16);
    offset = (unsigned)OFF_SMC_RANGES;
    if (offset >= 4096) {
        emit32(0x91400000u | (((offset >> 12) & 0xfffu) << 10) | (17u << 5) | 17u);
        offset &= 0xfffu;
    }
    if (offset) e_addi(17, 17, offset);
    e_ldr(16, CPUREG, OFF_SMCVA);
    e_addi(16, 16, 64);
    e_str(16, 17, 8);
    uint32_t *extended = (uint32_t *)g_cp;
    emit32(0); /* b done */

    uint8_t *append = g_cp;
    e_ldr(16, CPUREG, OFF_SMC_RANGE_COUNT);
    e_subi(17, 16, SMC_RANGE_CAP);
    uint32_t *overflow = (uint32_t *)g_cp;
    emit32(0); /* cbz x17,overflow */
    e_addlsl4(17, CPUREG, 16);
    offset = (unsigned)OFF_SMC_RANGES;
    if (offset >= 4096) {
        emit32(0x91400000u | (((offset >> 12) & 0xfffu) << 10) | (17u << 5) | 17u);
        offset &= 0xfffu;
    }
    if (offset) e_addi(17, 17, offset);
    e_ldr(16, CPUREG, OFF_SMCVA);
    e_str(16, 17, 0);
    e_addi(16, 16, 64);
    e_str(16, 17, 8);
    e_ldr(16, CPUREG, OFF_SMC_RANGE_COUNT);
    e_addi(16, 16, 1);
    e_str(16, CPUREG, OFF_SMC_RANGE_COUNT);
    uint32_t *skip = (uint32_t *)g_cp;
    emit32(0); /* b done */
    uint8_t *overflow_body = g_cp;
    e_movconst(16, 1);
    e_str(16, CPUREG, OFF_SMC_RANGE_OVERFLOW);
    uint8_t *done = g_cp;
    *empty = 0xB4000000u | (((uint32_t)((append - (uint8_t *)empty) / 4) & 0x7ffffu) << 5) | 16u;
    *not_adjacent = 0xB5000000u | (((uint32_t)((append - (uint8_t *)not_adjacent) / 4) & 0x7ffffu) << 5) | 17u;
    *extended = 0x14000000u | ((uint32_t)((done - (uint8_t *)extended) / 4) & 0x03ffffffu);
    *overflow = 0xB4000000u | (((uint32_t)((overflow_body - (uint8_t *)overflow) / 4) & 0x7ffffu) << 5) | 17u;
    *skip = 0x14000000u | ((uint32_t)((done - (uint8_t *)skip) / 4) & 0x03ffffffu);
}

// async-interrupt poll: emit a CHEAP flag-free check of cpu->irq at the block body entry (the target
// of every fall-through, direct chain `b body`, self-loop fold, tier-1 back-edge, and IBTC hit). When irq
// is set (a caught async guest signal became pending while spinning in-cache with no syscalls), exit the
// block to the dispatcher at a safe boundary -- all guest regs are live in host regs here, so the standard
// emit_exit_const spill materializes consistent guest state and maybe_deliver_signal builds the sigframe
// exactly as the syscall-boundary path does. Fast path is ldr+cbz (2 insns); cbz never touches NZCV, so a
// self-loop back-edge that lands here keeps the guest condition flags. x16 is engine scratch (dead at body
// entry when x16/x17 are stolen -- the default), so no guest reg is disturbed; the legacy NOSTEAL1617 path
// spills x9 to the red zone instead. `gpc` is the block start = the guest pc to resume at.
// IRQSLIM: when active (g_fwdskip == 8; aarch64 steal-mode default), the poll is emitted as a FIXED
// 2-insn header (ldr + cbnz to an out-of-line exit stub at the end of the block), so a forward direct
// chain can enter at body+8 and skip it -- every cycle still polls through its backward or indirect
// edge (see the g_fwdskip invariant note in cache.c). g_irq_patch carries the cbnz to the end-of-block
// stub emitter (emit_irq_stub). NOIRQSLIM=1 -> the legacy inline poll on every entry, chains to body+0.
static uint32_t *g_irq_patch;

static void emit_irq_check(uint64_t gpc) {
    (void)gpc;
    if (g_fwdskip) {
        e_ldr(16, CPUREG, OFF_IRQ); // ldr x16, [x28, #irq]
        g_irq_patch = (uint32_t *)g_cp;
        emit32(0); // cbnz x16, Lirq (the out-of-line exit stub; patched by emit_irq_stub)
        return;
    }
    if (g_steal1617) {
        e_ldr(16, CPUREG, OFF_IRQ); // ldr x16, [x28, #irq]
        uint32_t *p = (uint32_t *)g_cp;
        emit32(0); // cbz x16, Lcont  (patched below)
        emit_exit_const(gpc, R_BRANCH);
        uint8_t *cont = g_cp;
        *p = 0xB4000000u | (((uint32_t)(((uint8_t *)cont - (uint8_t *)p) / 4) & 0x7FFFF) << 5) | 16;
    } else {
        e_stur(9, 31, -16); // save guest x9 to the red zone
        e_ldr(9, CPUREG, OFF_IRQ);
        uint32_t *p = (uint32_t *)g_cp;
        emit32(0);          // cbz x9, Lcont
        e_ldur(9, 31, -16); // restore guest x9 before the exit (emit_spill saves the real value)
        emit_exit_const(gpc, R_BRANCH);
        uint8_t *cont = g_cp;
        *p = 0xB4000000u | (((uint32_t)(((uint8_t *)cont - (uint8_t *)p) / 4) & 0x7FFFF) << 5) | 9;
        e_ldur(9, 31, -16); // Lcont: restore guest x9 and fall into the body
    }
}

// (SIMD-clean syscall exit): SOUND over-approximation of "this guest instruction WRITES a vector
// (V) register." Over-marks read-only vector ops (vector stores, FCMP), the GPR-destination FP conversions
// (FCVTZS/FMOV-to-GPR), and UMOV/SMOV -> that only ever costs the optimization (a full spill), never
// correctness. It covers every V-writing form: the SIMD&FP data-processing box (scalar FP + AdvSIMD, bits
// [27:25]=111), SIMD&FP loads/stores (the V bit, [26]=1, in the load/store box), and AdvSIMD load/store
// STRUCTURES (LD1..LD4). A block containing any of these must take the full V spill on its syscall exit.
static int insn_touches_vreg(uint32_t in) {
    if ((in & 0x0E000000u) == 0x0E000000u) return 1;                     // SIMD&FP data-processing
    if ((in & 0x0A000000u) == 0x08000000u && ((in >> 26) & 1)) return 1; // SIMD&FP load/store (V=1)
    if ((in & 0xBE000000u) == 0x0C000000u) return 1;                     // AdvSIMD load/store structures
    return 0;
}

static int x28_alu_window_classify(uint32_t in, int *mask_out, int *read_out, int *write_out) {
    static const int shifts[4] = {0, 5, 16, 10};
    static const int mbits[4] = {1, 2, 4, 8};
    int mask = gpr_field_mask(in), read = 0, write = 0;
    uint32_t op = (in >> 25) & 0xFu;
    if (op == 8 || op == 9) {
        if ((in & 0x1F000000u) == 0x10000000u) return 0;
        if ((in & 0x1F800000u) == 0x12800000u) {
            int opc = (in >> 29) & 3;
            if (opc == 1) return 0;
            write = mask & 1;
            if (opc == 3) read = write;
        } else if ((in & 0x1F800000u) == 0x13800000u) {
            read = mask & (2 | 4);
            write = mask & 1;
        } else if ((in & 0x1F000000u) == 0x11000000u || (in & 0x1F800000u) == 0x12000000u ||
                   (in & 0x1F800000u) == 0x13000000u) {
            read = mask & 2;
            write = mask & 1;
        } else {
            return 0;
        }
    } else if ((in & 0x0E000000u) == 0x0A000000u) {
        read = mask & (2 | 4 | 8);
        write = mask & 1;
    } else {
        return 0;
    }
    for (int k = 0; k < 4; k++)
        if ((mask & mbits[k]) && is_stolen((in >> shifts[k]) & 31) && ((in >> shifts[k]) & 31) != 28) return 0;
    *mask_out = mask;
    *read_out = read;
    *write_out = write;
    return 1;
}

static int x28_alu_window_field(uint32_t in, int fields) {
    static const int shifts[4] = {0, 5, 16, 10};
    static const int mbits[4] = {1, 2, 4, 8};
    for (int k = 0; k < 4; k++)
        if ((fields & mbits[k]) && ((in >> shifts[k]) & 31) == 28) return 1;
    return 0;
}

static uint32_t x28_alu_window_rewrite(uint32_t in, int mask) {
    static const int shifts[4] = {0, 5, 16, 10};
    static const int mbits[4] = {1, 2, 4, 8};
    for (int k = 0; k < 4; k++)
        if ((mask & mbits[k]) && ((in >> shifts[k]) & 31) == 28) in = (in & ~(31u << shifts[k])) | (17u << shifts[k]);
    return in;
}

/*
 * Diagnostic ceiling for forwarding the two dominant stolen values through a
 * short straight-line run.  Guest x16 uses the otherwise engine-private host
 * x16 and guest x28 uses host x17; x28 remains CPUREG outside the rewritten
 * instructions.  Writes are published immediately, rather than only at the
 * end of the window, so signal reconstruction and a fault on a later guest
 * instruction always see architecturally committed state in cpu->x[].
 *
 * Deliberately admitted:
 *   - the integer ALU forms audited by x28_alu_window_classify;
 *   - ordinary scalar integer load/store, unsigned-immediate, unscaled,
 *     pre/post-indexed, or register-offset.
 * Deliberately rejected: x17/x18 operands (scratch collision), pairs,
 * atomics/exclusives, SIMD, branches/system instructions, guest-base/folded
 * addressing, tier 2, and active guest-bus/SMC-special translation modes.
 */
