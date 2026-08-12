// Recognize the compiler's otherwise redundant frame around a direct tail call:
//
//   stp x29,x30,[sp,#-16]!; small register-only setup; ldp x29,x30,[sp],#16; b target
//
// x17 is engine-private in steal mode, so it can carry the architectural LR
// between the real stack store/load.  The stack accesses remain native and at
// their original guest PCs (fault/unwind semantics are unchanged); this only
// avoids repeatedly round-tripping x30 through cpu->x[30] in the generic
// stolen-register mangler.  Keep the recognizer intentionally exact.
static uint64_t scan_tail_x30_carry(uint64_t pc) {
    if (!g_steal1617 || g_noibslim || jit_guest_bus_active()) return 0;
    // The recognizer may inspect the eight setup instructions plus the tail
    // instruction following the restoring LDP.  Refuse to speculate across an
    // unmapped guest boundary.
    if (!hl_host_range_mapped((uintptr_t)pc, 10 * sizeof(uint32_t))) return 0;
    if (a64_fetch_instruction(pc, NULL) != 0xA9BF7BFDu) return 0;
    for (int i = 1; i <= 8; i++) {
        uint32_t in = a64_fetch_instruction(pc + (uint64_t)i * 4, NULL);
        if (in == 0xA8C17BFDu) {
            uint32_t tail = a64_fetch_instruction(pc + (uint64_t)(i + 1) * 4, NULL);
            return (tail & 0xFC000000u) == 0x14000000u ? pc + (uint64_t)i * 4 : 0;
        }
        // No memory, control flow, system, SIMD, PC-relative operation, or
        // stolen guest register may occur while host x17 carries the LR.
        if ((in & 0x0A000000u) == 0x08000000u || (in & 0x7C000000u) == 0x14000000u ||
            (in & 0x1C000000u) == 0x10000000u || (in & 0x0E000000u) == 0x0E000000u || (in & 0xFFC00000u) == 0xD5000000u)
            return 0;
        int mask = gpr_field_mask(in);
        if (uses_x18(in, mask)) return 0;
    }
    return 0;
}

static void *translate_block(uint64_t gpc) {
    /* Observe writes made through another MAP_SHARED alias before decoding
       an executable view backed by an emulated host-page snapshot. */
    uint64_t source_page = gpc & ~UINT64_C(0xfff);
    filemap_refresh_emulated(source_page, source_page + UINT64_C(0x1000));
    // W4E tier-2: read NOTIER2 / TIER2_THRESHOLD once (idempotent) before any self-loop detection.
    tier2_env_init();
    // gpc is mutated by the decode loop; key the cache by START
    uint64_t start = gpc;
    chain_exit_dedup_reset();
    g_bus_stub_patch_count = 0;
    g_soft_stub_patch_count = 0;
    g_soft_legacy_stub_patch_count = 0;
    g_soft_resolver_patch_count = 0;
    uint64_t guest_start = gpc;
    uint64_t guest_end = gpc + 4;
    uint64_t tail_carry_ldp = scan_tail_x30_carry(gpc);
    g_blk_vdirty = 0;     // reset per block; set below when a V-writing insn is emitted
    g_t2_loop_top = NULL; // reset per block; set only in the tier-2 vdirty-hoist path below
    g_t2_irq_patch = NULL;
    void *host = g_cp;
    emit_prologue();
    // Keep the hot chained/IBTC entry stable independently of prologue size.
    // Cold dispatcher entry runs this padding once; hot entries target `body`
    // below and skip it.
    while ((uintptr_t)g_cp & 15)
        emit32(0xD503201Fu);
    // chained jumps land here (regs already live)
    void *body = g_cp;
    // poll cpu->irq at the body entry so a caught async signal reaches a no-syscall guest loop.
    emit_irq_check(start);
    // ldxr/ldaxr..stxr/stlxr exclusive regions must stay in ONE block with no injected
    // memory ops between them, else the monitor clears and stxr retries forever. While
    // inside such a region, conditional branches are emitted inline and their exits are
    // deferred to stubs after the store-exclusive.
    int in_excl = 0;

    struct {
        uint32_t *patch;
        uint64_t target;
        uint32_t in;
    } defer[64];

    int ndefer = 0;
    uint64_t provenance_host = 0;
    uint64_t provenance_guest = 0;
    int provenance_fault_capable = 0;
    // opt4 region state: guest block-starts inlined into this region + a block budget. The
    // region STOPS (falls to the baseline single-block exit) at any dispatcher-mediated edge
    // (indirect br/blr, bl/call, ret, svc/syscall), inside an exclusive monitor region, or on
    // hitting the 16-block / 16 KB bound -- "when unsure, end the region".
    if (g_stitch < 0) g_stitch = 1;
    uint64_t seen[TRACE_MAX_BLK];
    int nseen = 0, trace_blk = 0;
    // opt4 conditional-stitch budget: each conditional fall-through laid inline is a SPECULATION -- the
    // guest may instead take the (chain-exit) branch, leaving the inlined tail dead. Deadness compounds
    // per conditional passed (measured on sqlite: depth-1 fall-throughs 28% never-executed, rising to
    // >85% by the 6th). Unconditional `b` edges follow the guaranteed path and are NOT budgeted, so
    // straight-line/loop-body traces still stitch freely; only chains of hard-to-predict conditionals are
    // cut. Ending a region early is always semantics-preserving: intermediate block-starts are never
    // registered in g_map, so the truncated successor self-heals as an on-demand fresh translation via the
    // ordinary chain-exit path (identical to the NOSTITCH baseline, just re-anchored deeper).
    int ncond = 0;

    struct {
        uint64_t target, resume, retpc, expected_x30;
    } ctx[CTX_INLINE_DEPTH];

    int nctx = 0;
#ifndef STITCH_MAX_COND
#define STITCH_MAX_COND 3
#endif
    // SMC precise gate (line-granular source set): record every 64B guest line this block is actually
    // decoded from, AS WE DECODE, instead of marking the whole contiguous [start,guest_end) hull after the
    // loop (see txpg_mark). For an opt4-stitched superblock the hull also spans the address GAPS between
    // the scattered inlined sub-blocks -- lines that hold no translated code -- so the post-loop hull
    // marked ~15x more lines than the block truly sourced (measured ~29 vs ~2 per block on sqlite), and
    // each txln_put is a cache miss into the 16MB line set. Marking only the decoded lines is a strict
    // subset of the hull yet still a complete superset of the REAL source lines (every translated byte came
    // from a decoded instruction), so txln_flush_class stays correct -- it can never miss a genuinely
    // self-modified source line. Skipped under g_tier2_build (the promoter never marks), matching the
    // post-loop guard.
    uint64_t tx_last_line = ~UINT64_C(0);
#define STITCH_OK                                                                                                      \
    (g_stitch && !smc_seen() && !in_excl && trace_blk < TRACE_MAX_BLK - 1 && ncond < STITCH_MAX_COND &&                \
     (g_cp - (uint8_t *)host) < TRACE_MAX_BYTES)
    for (;;) {
        // A basic block is not necessarily small: generated programs can contain tens of thousands of
        // straight-line instructions before their first control-flow edge.  File-backed BUS guards can
        // expand each guest memory operation into hundreds of host bytes.  Bound normal regions by emitted
        // size so the dispatcher's CACHE_EMIT_HEADROOM admission guarantee remains true.  Splitting at an
        // arbitrary instruction boundary is equivalent to an ordinary chain exit; exclusive sequences are
        // exempt because an injected dispatcher edge would clear the architectural monitor.
        if (!in_excl && ((size_t)(g_cp - (uint8_t *)host) >= CACHE_EMIT_HEADROOM / 2 ||
                         g_bus_stub_patch_count >= BUS_STUB_PATCH_MAX - 4)) {
            emit_chain_exit(gpc);
            break;
        }
        int fetch_ok;
        uint32_t in = a64_fetch_instruction(gpc, &fetch_ok);
        if (!fetch_ok) {
            e_movconst(9, gpc);
            e_str(9, CPUREG, OFF_FAULT_ADDR);
            emit_exit_const(gpc, R_FETCHFAULT);
            break;
        }
        if (stealfast_on() && !g_tier2_build && !in_excl && !guestbase_on() && !jit_guest_bus_active() &&
            !g_nonpie_lo) {
            int mask, read, write, fault;
            if (stolen_forward_classify(in, &mask, &read, &write, &fault) &&
                (stolen_forward_field(in, read, 16) || stolen_forward_field(in, read, 28))) {
                int count = 0, touches = 0, last_touch = -1;
                int need16 = 0, need28 = 0;
                for (; count < 12; count++) {
                    uint32_t win = a64_fetch_instruction(gpc + (uint64_t)count * 4, NULL);
                    int wm, wr, ww, wf;
                    if (!stolen_forward_classify(win, &wm, &wr, &ww, &wf)) break;
                    int r16 = stolen_forward_field(win, wr, 16);
                    int r28 = stolen_forward_field(win, wr, 28);
                    if (r16 || r28) {
                        touches++;
                        last_touch = count;
                        need16 |= r16;
                        need28 |= r28;
                    }
                }
                if (touches >= 3) {
                    int window = last_touch + 1;
                    if (provenance_fault_capable)
                        jit_instruction_map_put(provenance_host, (uint64_t)g_cp, provenance_guest);
                    /* Load x16 first: the second load still needs real x28. */
                    if (need16) e_ldr(16, CPUREG, 16 * 8);
                    if (need28) e_ldr(17, CPUREG, 28 * 8);
                    for (int i = 0; i < window; i++) {
                        uint64_t pc = gpc + (uint64_t)i * 4;
                        uint32_t win = a64_fetch_instruction(pc, NULL);
                        int wm, wr, ww, wf;
                        int ok = stolen_forward_classify(win, &wm, &wr, &ww, &wf);
                        assert(ok);
                        uint64_t hstart = (uint64_t)g_cp;
                        emit32(stolen_forward_rewrite(win, wm));
                        if (stolen_forward_field(win, ww, 16)) e_str(16, CPUREG, 16 * 8);
                        if (stolen_forward_field(win, ww, 28)) e_str(17, CPUREG, 28 * 8);
                        if (wf) jit_instruction_map_put(hstart, (uint64_t)g_cp, pc);
                    }
                    if (g_txln_active) {
                        uint64_t last = (gpc + (uint64_t)window * 4 - 1) >> 6;
                        for (uint64_t line = gpc >> 6; line <= last; line++)
                            txln_put(line);
                        tx_last_line = last;
                    }
                    if (gpc < guest_start) guest_start = gpc;
                    gpc += (uint64_t)window * 4;
                    if (gpc > guest_end) guest_end = gpc;
                    provenance_fault_capable = 0;
                    continue;
                }
            }
        }
        if (stealfast_on() && !g_tier2_build && !in_excl && !guestbase_on() && !jit_guest_bus_active() &&
            !g_nonpie_lo) {
            int mask, read, write;
            if (x28_alu_window_classify(in, &mask, &read, &write) && x28_alu_window_field(in, read)) {
                int count = 0, reads = 0, last_read = -1;
                for (; count < 12; count++) {
                    uint32_t win = a64_fetch_instruction(gpc + (uint64_t)count * 4, NULL);
                    int wm, wr, ww;
                    if (!x28_alu_window_classify(win, &wm, &wr, &ww)) break;
                    if (x28_alu_window_field(win, wr)) {
                        reads++;
                        last_read = count;
                    }
                }
                if (reads >= 3) {
                    int window = last_read + 1;
                    if (provenance_fault_capable)
                        jit_instruction_map_put(provenance_host, (uint64_t)g_cp, provenance_guest);
                    e_ldr(17, CPUREG, 28 * 8);
                    for (int i = 0; i < window; i++) {
                        uint64_t pc = gpc + (uint64_t)i * 4;
                        uint32_t win = a64_fetch_instruction(pc, NULL);
                        int wm, wr, ww;
                        int ok = x28_alu_window_classify(win, &wm, &wr, &ww);
                        assert(ok);
                        emit32(x28_alu_window_rewrite(win, wm));
                        if (x28_alu_window_field(win, ww)) e_str(17, CPUREG, 28 * 8);
                    }
                    if (g_txln_active) {
                        uint64_t last = (gpc + (uint64_t)window * 4 - 1) >> 6;
                        for (uint64_t line = gpc >> 6; line <= last; line++)
                            txln_put(line);
                        tx_last_line = last;
                    }
                    if (gpc < guest_start) guest_start = gpc;
                    gpc += (uint64_t)window * 4;
                    if (gpc > guest_end) guest_end = gpc;
                    provenance_fault_capable = 0;
                    continue;
                }
            }
        }
        if (!g_tier2_build && g_txln_active) {
            uint64_t tx_ln = gpc >> 6;
            if (tx_ln != tx_last_line) {
                txln_put(tx_ln);
                tx_last_line = tx_ln;
            }
        }
        if (gpc < guest_start) guest_start = gpc;
        if (gpc + 4 > guest_end) guest_end = gpc + 4;
        if (provenance_fault_capable) jit_instruction_map_put(provenance_host, (uint64_t)g_cp, provenance_guest);
        provenance_host = (uint64_t)g_cp;
        provenance_guest = gpc;
        uint32_t provenance_major = (in >> 25) & 0xFu;
        provenance_fault_capable =
            ((in & 0x0A000000u) == 0x08000000u) || provenance_major == 0xA || provenance_major == 0xB;
        g_emit_gpc = gpc; // IRQSLIM: tag the current guest PC for the forward/backward edge test in emit_chain_exit
        // at the FIRST vector-touching instruction of the region, store the (nonzero) cpu pointer
        // into cpu->vdirty so a later (possibly chained-to) syscall exit takes the full V spill. Emitted
        // once per region (g_blk_vdirty latch); flag-neutral `str` runs before the vector write. Regions are
        // linear (taken branches exit, only fall-through continues), so the first write dominates all later
        // vector writes -> one store covers every path. Zero cost on vector-free (integer/syscall) blocks.
        if (!g_blk_vdirty && insn_touches_vreg(in)) {
            e_str(CPUREG, CPUREG, (int)OFF_VDIRTY);
            g_blk_vdirty = 1;
            // W4E tier-2 vdirty hoist: only in the promoter recompile, and only when this V-writing
            // insn is the block's FIRST (== the self-loop top). Emit a fresh inline async poll right
            // after the store and record its address so the folded back-edge lands here -- skipping the
            // idempotent store while still polling cpu->irq every iteration (IRQSLIM back-edge invariant).
            // Every block ENTRY still runs the store first: the header poll path (body+0) falls straight
            // in, and a forward chain (body+g_fwdskip) lands exactly on the store. Non-self-loop V-first
            // blocks harmlessly gain one extra entry poll; g_t2_loop_top then goes unused.
            if (g_tier2_build && gpc == start) {
                g_t2_loop_top = g_cp;
                e_ldr(16, CPUREG, OFF_IRQ); // ldr x16, [cpu, #irq]
                g_t2_irq_patch = (uint32_t *)g_cp;
                emit32(0); // cbnz x16, shared out-of-line Lirq
            }
        }

        if (!in_excl) {
            if (tail_carry_ldp && gpc == start) {
                e_ldr(17, CPUREG, 30 * 8);
                emit32((in & ~(31u << 10)) | (17u << 10));
                gpc += 4;
                continue;
            }
            if (tail_carry_ldp && gpc == tail_carry_ldp) {
                emit32((in & ~(31u << 10)) | (17u << 10));
                e_str(17, CPUREG, 30 * 8);
                gpc += 4;
                continue;
            }
            int n = try_lse_atomic(gpc);
            if (n) {
                // try_lse_atomic consumes n bytes (a whole ldxr..stxr sequence) without re-entering the
                // loop top, so mark every source line it spans -- not just gpc's -- keeping the line set a
                // complete superset of the decoded bytes.
                if (!g_tier2_build && g_txln_active)
                    for (uint64_t ll = gpc >> 6; ll <= (gpc + n - 1) >> 6; ll++)
                        txln_put(ll);
                gpc += n;
                continue;
            }
            // ldxr/stxr loop -> LSE
        }
        if (is_i8mm_mmla(in)) {
            emit_i8mm_mmla(in);
            gpc += 4;
            continue;
        }
        if (is_bf16_bfcvt(in)) {
            emit_bf16_bfcvt(in);
            gpc += 4;
            continue;
        }
        if (is_bf16_bfdot(in)) {
            emit_bf16_bfdot(in);
            gpc += 4;
            continue;
        }
        // Load/store-exclusive family is bits[29:24]=001000. The o2 bit (bit23) distinguishes the
        // EXCLUSIVE monitor variants (o2=0: LDXR/LDAXR/STXR/STLXR/LDXP/STXP) from the merely ORDERED
        // load-acquire/store-release (o2=1: LDAR/LDLAR/STLR/STLLR), which are NOT part of an exclusive
        // pair. Masking bit23 in (0x3FC00000) keeps a bare LDAR -- ubiquitous in C++ std::atomic and
        // glibc -- from opening the region and leaving in_excl stuck on. L (bit22) selects load vs store.
        // CASP/CASPA/CASPL/CASPAL share this encoding box (o2=0, and A reuses the L bit), so the plain
        // mask alone reads CASPA/CASPAL as a load-exclusive and CASP/CASPL as a store-exclusive. A CASPA
        // then latches in_excl ON for the rest of the block, which disables the non-PIE bias fold for
        // every following memory op -- a fatal SIGSEGV on the next low image access. Exclude CASP here;
        // it is a single self-contained instruction and opens no monitor region.
        if (!is_casp(in)) {
            if ((in & 0x3FC00000u) == 0x08400000u)
                // load-exclusive (o2=0, L=1)
                in_excl = 1;
            else if (in_excl && (in & 0x3FC00000u) == 0x08000000u)
                // store-exclusive (o2=0, L=0)
                in_excl = 0;
        }
        // Defensive: the deferred-branch table is fixed-size. If a region ever fills it (pathological
        // or mis-decoded -- a real LDXR..STXR pair never holds this many conditional branches), end the
        // region here so the branches below take the normal exit path instead of overflowing defer[].
        if (in_excl && ndefer >= (int)(sizeof defer / sizeof defer[0])) in_excl = 0;

        // svc #0
        if (in == 0xD4000001u) {
            emit_exit_const(gpc, R_SYSCALL);
            break;
        }
        // b
        if ((in & 0xFC000000u) == 0x14000000u) {
            int64_t off = sext(in & 0x3FFFFFF, 26) << 2;
            uint64_t tgt = gpc + off;
            // opt4: follow the unconditional edge INLINE if its target is a fresh block (not the
            // region head, not already inlined, not already translated) -> the inter-block `b`
            // disappears. Otherwise chain normally (existing block / loop back-edge).
            if (STITCH_OK && tgt != start && !seen_has(seen, nseen, tgt) && !map_body(tgt)) {
                seen[nseen++] = tgt;
                trace_blk++;
                gpc = tgt;
                continue;
            }
            emit_chain_exit(tgt);
            break;
        }
        // bl
        if ((in & 0xFC000000u) == 0x94000000u) {
            int64_t off = sext(in & 0x3FFFFFF, 26) << 2;
            // Fuse a direct call to the exact canonical four-insn PLT veneer:
            //   adrp x16,page; ldr x17,[x16,#got]; add x16,x16,#lo; br x17
            // Preserve every architectural effect and the real fault-capable GOT
            // load; only the extra translated-block hop and its entry poll vanish.
            uint64_t plt = gpc + off;
            if (smc_seen()) goto no_bl_plt_fuse;
            if (!hl_host_range_mapped((uintptr_t)plt, 16)) goto no_bl_plt_fuse;
            uint32_t p0 = a64_fetch_instruction(plt, NULL);
            uint32_t p1 = a64_fetch_instruction(plt + 4, NULL);
            uint32_t p2 = a64_fetch_instruction(plt + 8, NULL);
            uint32_t p3 = a64_fetch_instruction(plt + 12, NULL);
            if (!guestbase_on() && !jit_guest_bus_active() && (p0 & 0x9F00001Fu) == 0x90000010u &&
                (p1 & 0xFFC003FFu) == 0xF9400211u && (p2 & 0xFFC003FFu) == 0x91000210u && p3 == 0xD61F0220u) {
                int64_t pimm = sext((((p0 >> 5) & 0x7FFFF) << 2) | ((p0 >> 29) & 3), 21) << 12;
                uint64_t page = (pcrel_base(plt) & ~0xFFFull) + pimm;
                emit_set_x30(pcrel_base(gpc) + 4);
                if (!emit_guest_adrp_page(16, page)) e_movconst(16, page);
                e_str(16, CPUREG, 16 * 8);
                uint64_t load_host = (uint64_t)g_cp;
                emit32(p1);
                pcache_record_provenance(load_host, (uint64_t)g_cp, plt + 4);
                e_str(17, CPUREG, 17 * 8);
                emit32(p2);
                e_str(16, CPUREG, 16 * 8);
                txpg_mark(plt, plt + 16);
                if (g_txln_active)
                    for (uint64_t line = plt >> 6; line <= (plt + 15) >> 6; line++)
                        txln_put(line);
                emit_ibranch_ip2_ready(17, 1);
                break;
            }
        no_bl_plt_fuse:
            // Inline an LSE outline-atomic helper call to a single host atomic op (elides the call +
            // return dispatch, the dominant atomics tax); only fires in the verbatim-safe regime.
            if (try_inline_outline_atomic(gpc, gpc + off)) {
                gpc += 4;
                continue;
            }
            uint64_t ancestors[CTX_INLINE_DEPTH];
            for (int i = 0; i < nctx; i++)
                ancestors[i] = ctx[i].target;
            uint64_t clone_ret;
            int clone_cost;
            /*
             * A BUS-active generation expands every cloned memory operation
             * with a runtime guard. Cloning then duplicates both hot guards
             * and cold stubs, accelerating cache rotation while removing only
             * a call/return pair. Keep ordinary context cloning unchanged, but
             * use the normal RAS call path while BUS observation is active.
             */
            if (!smc_seen() && !jit_guest_bus_active() && nctx < CTX_INLINE_DEPTH &&
                context_clone_candidate(gpc + off, ancestors, nctx, &clone_ret, &clone_cost) &&
                (g_cp - (uint8_t *)host) + clone_cost * 16 < TRACE_MAX_BYTES) {
                emit_set_x30(pcrel_base(gpc) + 4);
                ctx[nctx].target = gpc + off;
                ctx[nctx].resume = gpc + 4;
                ctx[nctx].retpc = clone_ret;
                ctx[nctx].expected_x30 = pcrel_base(gpc) + 4;
                nctx++;
                gpc += off;
                continue;
            }
            emit_bl_ras(gpc, gpc + off);
            // §B: shadow push + host bl (RAS) + Lcont continuation
            break;
        }
        // ret xN
        if ((in & 0xFFFFFC1Fu) == 0xD65F0000u) {
            int rrn = (in >> 5) & 31;
            if (rrn == 30 && nctx > 0 && gpc == ctx[nctx - 1].retpc) {
                e_movr(16, 30);
                e_movconst(17, ctx[nctx - 1].expected_x30);
                emit32(0xCB000000u | (17u << 16) | (16u << 5) | 16u);
                uint32_t *p_zero = (uint32_t *)g_cp;
                emit32(0);
                emit_ibranch(30);
                uint8_t *resume_host = g_cp;
                *p_zero =
                    0xB4000000u | (((uint32_t)(((uint8_t *)resume_host - (uint8_t *)p_zero) / 4) & 0x7FFFF) << 5) | 16;
                gpc = ctx[nctx - 1].resume;
                nctx--;
                continue;
            }
            if (rrn == 30)
                // A3: §B OFF (default) -> bare IBTC return (no shadow-ret preamble); §B ON -> shadow ret
                // (FAST host-ret on guest_ret+guest_sp match, else IBTC fallback).
                shadowgate() == -1 ? emit_ibranch(30) : emit_shadow_ret();
            else
                // ret xN via another reg -> ordinary indirect branch
                emit_ibranch(rrn);
            break;
        }
        // br
        if ((in & 0xFFFFFC1Fu) == 0xD61F0000u) {
            int brn = (in >> 5) & 31;
            if (g_steal1617 && !g_noibslim && !is_stolen(brn) && is_interp_dispatch_br(gpc, brn))
                // IBSLIM: a recognized interpreter-dispatch site (megamorphic by construction) --
                // skip the dead per-site IC, go straight to the shared hash.
                emit_hash_tail(brn);
            else
                emit_ibranch(brn);
            break;
        }
        // blr
        if ((in & 0xFFFFFC1Fu) == 0xD63F0000u) {
            // guest x30 lives in cpu->x[30] (stolen); RAS push needs a host blr. The link value is
            // guest-visible (spilled to the guest stack), so store the UN-BIASED (low) return vaddr
            // for non-PIE; the dispatcher re-biases on the ret. pcrel_base is identity for PIE.
            int blrn = (in >> 5) & 31;
            if (blrn == 30) {
                // BLR x30 reads the old guest x30 as its target, then writes
                // the return address to x30. Preserve that ordering now that
                // guest x30 is resident in cpu->x[30].
                // emit_set_x30 uses x16 as its store scratch, so keep the
                // branch target in the other engine-private IP register.
                e_ldr(17, CPUREG, 30 * 8);
                emit_set_x30(pcrel_base(gpc) + 4);
                emit_ibranch_ip2_ready(17, 1);
            } else {
                emit_set_x30(pcrel_base(gpc) + 4);
                emit_ibranch(blrn);
            }
            //   (Section 3) -- deferred; Stage-B IBTC for the function-ptr return
            break;
        }
        // b.cond
        if ((in & 0xFF000010u) == 0x54000000u) {
            int cond = in & 0xF;
            int64_t off = sext((in >> 5) & 0x7FFFF, 19) << 2;
            uint64_t taken = gpc + off, fall = gpc + 4;
            if (in_excl) {
                defer[ndefer].patch = (uint32_t *)g_cp;
                defer[ndefer].target = taken;
                defer[ndefer].in = in;
                ndefer++;
                emit32(0);
                gpc += 4;
                continue;
            }
            // W4E tier-2: single-block self-loop (taken back-edge == block start). Intercept BEFORE the
            // opt4 stitch so the redundant back-edge trampoline can be folded; non-self-loops (taken !=
            // start) fall through to opt4 unchanged. NOTIER2 -> skipped (exact committed-opt4 baseline).
            if (taken == start && !g_notier2 && !loop_has_rmw_hazard(start, gpc)) {
                int slot = g_tier2_build ? 0 : t2_slot(start);
                if (g_tier2_build || slot >= 0) {
                    emit_selfloop(in, start, fall, body, slot);
                    break;
                }
            }
            // opt4: lay the fall-through inline; invert the condition so TAKEN is the exit. This inverts by
            // flipping cond bit0 (stitch_cond: in ^ 1) -- valid ONLY for a genuinely conditional branch.
            // The condition field 0b111x is special: 0b1110 = AL and 0b1111 = NV BOTH mean "always execute"
            // in A64 (NV is not "never"; it is a reserved alias of AL). So a guest `b.al` is an
            // UNCONDITIONAL branch with a DEAD fall-through, and flipping its bit0 yields NV -- still
            // "always" -- so the "inverted" branch never actually diverts: the stitched superblock would
            // then ALWAYS fall into the (guest-unreachable) fall-through and NEVER reach the guest's real
            // always-taken target. HotSpot's generated interpreter emits `b.al` as a plain jump, so this
            // silently mislowered its dispatch -> an infinite spin (#186). Exclude cond >= 0xE from the fold;
            // an always-branch takes the ordinary b.cond emit below, which chains straight to `taken`.
            if (STITCH_OK && cond < 0xE && fall != start && !seen_has(seen, nseen, fall) && !map_body(fall)) {
                stitch_cond(in ^ 1u, taken);
                seen[nseen++] = fall;
                trace_blk++;
                ncond++;
                gpc = fall;
                continue;
            }
            uint32_t *patch = (uint32_t *)g_cp;
            // b.cond -> taken (backpatched)
            emit32(0);
            emit_chain_exit(fall);
            int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
            *patch = 0x54000000u | ((uint32_t)(d & 0x7FFFF) << 5) | cond;
            emit_chain_exit(taken);
            break;
        }
        // cbz / cbnz
        if ((in & 0x7E000000u) == 0x34000000u) {
            int64_t off = sext((in >> 5) & 0x7FFFF, 19) << 2;
            uint64_t taken = gpc + off, fall = gpc + 4;
            int sf = in >> 31, op = (in >> 24) & 1, rt = in & 31;
            if (in_excl) {
                defer[ndefer].patch = (uint32_t *)g_cp;
                defer[ndefer].target = taken;
                defer[ndefer].in = in;
                ndefer++;
                emit32(0);
                gpc += 4;
                continue;
            }
            // A backward CBZ/CBNZ whose loop body is only polling memory is the canonical AArch64
            // contended-lock wait loop (glibc/musl spin locks and several runtime locks use it).  The
            // translated thread otherwise consumes its entire host timeslice repeatedly loading the
            // held word, which can starve the translated owner that must run to release it.  Preserve
            // the guest-visible semantics but emit the architectural YIELD hint before retrying.  Keep
            // this deliberately narrow: both the instruction immediately before the branch and the
            // branch target must be scalar loads, so compute loops and ordinary backward branches remain
            // byte-for-byte unchanged.
            uint32_t prev = gpc >= start + 4 ? a64_fetch_instruction(gpc - 4, NULL) : 0;
            uint32_t first = taken < gpc ? a64_fetch_instruction(taken, NULL) : 0;
            int prev_load = (prev & 0x0A000000u) == 0x08000000u && ((prev >> 22) & 1u);
            int first_load = (first & 0x0A000000u) == 0x08000000u && ((first >> 22) & 1u);
            if (taken < gpc && prev_load && first_load) emit32(0xD503203Fu); // yield
            // W4E tier-2: single-block self-loop (non-stolen tested reg). Before opt4; NOTIER2 -> skipped.
            if (taken == start && !g_notier2 && !is_stolen(rt) && !loop_has_rmw_hazard(start, gpc)) {
                int slot = g_tier2_build ? 0 : t2_slot(start);
                if (g_tier2_build || slot >= 0) {
                    emit_selfloop(in, start, fall, body, slot);
                    break;
                }
            }
            // opt4: lay the fall-through inline (non-stolen test reg only); invert op (cbz<->cbnz)
            if (STITCH_OK && !is_stolen(rt) && fall != start && !seen_has(seen, nseen, fall) && !map_body(fall)) {
                stitch_cond(in ^ (1u << 24), taken);
                seen[nseen++] = fall;
                trace_blk++;
                ncond++;
                gpc = fall;
                continue;
            }
            // tested reg stolen -> test cpu->x[rt] via a saved scratch
            if (is_stolen(rt)) {
                // stealfast: x16 is engine-dead across both successor edges -> no spill/restore at all
                if (stealfast_on()) {
                    e_ldr(16, CPUREG, rt * 8);
                    uint32_t *patch = (uint32_t *)g_cp;
                    // cbz/cbnz x16 -> taken
                    emit32(0);
                    emit_chain_exit(fall);
                    int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
                    *patch =
                        0x34000000u | ((unsigned)sf << 31) | ((unsigned)op << 24) | ((uint32_t)(d & 0x7FFFF) << 5) | 16;
                    emit_chain_exit(taken);
                    break;
                }
                int S = 0;
                e_str(S, CPUREG, (int)OFF_MSCRATCH);
                e_ldr(S, CPUREG, rt * 8);
                uint32_t *patch = (uint32_t *)g_cp;
                // cbz/cbnz S -> taken
                emit32(0);
                e_ldr(S, CPUREG, (int)OFF_MSCRATCH);
                // fall: restore S
                emit_chain_exit(fall);
                int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
                *patch = 0x34000000u | ((unsigned)sf << 31) | ((unsigned)op << 24) | ((uint32_t)(d & 0x7FFFF) << 5) | S;
                e_ldr(S, CPUREG, (int)OFF_MSCRATCH);
                emit_chain_exit(taken);
                // taken: restore S
                break;
            }
            uint32_t *patch = (uint32_t *)g_cp;
            // cbz/cbnz rt -> taken
            emit32(0);
            emit_chain_exit(fall);
            int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
            *patch = 0x34000000u | ((unsigned)sf << 31) | ((unsigned)op << 24) | ((uint32_t)(d & 0x7FFFF) << 5) | rt;
            emit_chain_exit(taken);
            break;
        }
        // tbz / tbnz
        if ((in & 0x7E000000u) == 0x36000000u) {
            int b40 = (in >> 19) & 0x1F, bit5 = (in >> 31) & 1;
            int64_t off = sext((in >> 5) & 0x3FFF, 14) << 2;
            uint64_t taken = gpc + off, fall = gpc + 4;
            int op = (in >> 24) & 1, rt = in & 31;
            if (in_excl) {
                defer[ndefer].patch = (uint32_t *)g_cp;
                defer[ndefer].target = taken;
                defer[ndefer].in = in;
                ndefer++;
                emit32(0);
                gpc += 4;
                continue;
            }
            // W4E tier-2: single-block self-loop (non-stolen tested reg). Before opt4; NOTIER2 -> skipped.
            if (taken == start && !g_notier2 && !is_stolen(rt) && !loop_has_rmw_hazard(start, gpc)) {
                int slot = g_tier2_build ? 0 : t2_slot(start);
                if (g_tier2_build || slot >= 0) {
                    emit_selfloop(in, start, fall, body, slot);
                    break;
                }
            }
            // opt4: lay the fall-through inline (non-stolen test reg only); invert op (tbz<->tbnz)
            if (STITCH_OK && !is_stolen(rt) && fall != start && !seen_has(seen, nseen, fall) && !map_body(fall)) {
                stitch_cond(in ^ (1u << 24), taken);
                seen[nseen++] = fall;
                trace_blk++;
                ncond++;
                gpc = fall;
                continue;
            }
            // tested reg stolen -> test cpu->x[rt] via a saved scratch
            if (is_stolen(rt)) {
                // stealfast: x16 is engine-dead across both successor edges -> no spill/restore at all
                if (stealfast_on()) {
                    e_ldr(16, CPUREG, rt * 8);
                    uint32_t *patch = (uint32_t *)g_cp;
                    // tbz/tbnz x16,#bit -> taken
                    emit32(0);
                    emit_chain_exit(fall);
                    int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
                    *patch = 0x36000000u | ((unsigned)bit5 << 31) | ((unsigned)op << 24) | ((unsigned)b40 << 19) |
                             ((uint32_t)(d & 0x3FFF) << 5) | 16;
                    emit_chain_exit(taken);
                    break;
                }
                int S = 0;
                e_str(S, CPUREG, (int)OFF_MSCRATCH);
                e_ldr(S, CPUREG, rt * 8);
                uint32_t *patch = (uint32_t *)g_cp;
                // tbz/tbnz S,#bit -> taken
                emit32(0);
                e_ldr(S, CPUREG, (int)OFF_MSCRATCH);
                // fall: restore S
                emit_chain_exit(fall);
                int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
                *patch = 0x36000000u | ((unsigned)bit5 << 31) | ((unsigned)op << 24) | ((unsigned)b40 << 19) |
                         ((uint32_t)(d & 0x3FFF) << 5) | S;
                e_ldr(S, CPUREG, (int)OFF_MSCRATCH);
                emit_chain_exit(taken);
                // taken: restore S
                break;
            }
            uint32_t *patch = (uint32_t *)g_cp;
            // tbz/tbnz rt,#bit -> taken
            emit32(0);
            emit_chain_exit(fall);
            int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
            *patch = 0x36000000u | ((unsigned)bit5 << 31) | ((unsigned)op << 24) | ((unsigned)b40 << 19) |
                     ((uint32_t)(d & 0x3FFF) << 5) | rt;
            emit_chain_exit(taken);
            break;
        }

        // --- TLS: the whole point. mrs/msr tpidr_el0 become a single NATIVE
        //     load/store from cpu->tls. No trap, no Mach round-trip. ---
        // mrs xN, tpidr_el0  (TLS read, hot: CPython reads its thread state through this constantly).
        // stealfast: x28 IS the cpu pointer for the whole block, so the read is ONE ldr (legacy paid a
        // 3-insn TLS-based cpu reload via e_load_cpu, or the full x18_prolog dance for a stolen rd).
        if ((in & 0xFFFFFFE0u) == 0xD53BD040u) {
            int n = in & 31;
            if (is_stolen(n)) {
                if (stealfast_on()) {
                    e_ldr(16, CPUREG, OFF_TLS);
                    e_str(16, CPUREG, n * 8);
                } else {
                    x18_prolog();
                    e_ldr(0, 1, OFF_TLS);
                    e_str(0, 1, n * 8);
                    x18_epilog();
                }
            } else if (stealfast_on()) {
                e_ldr(n, CPUREG, OFF_TLS);
            } else {
                e_load_cpu(n);
                e_ldr(n, n, OFF_TLS);
            }
            gpc += 4;
            continue;
        }
        // msr tpidr_el0, xN  (TLS write, rare)
        if ((in & 0xFFFFFFE0u) == 0xD51BD040u) {
            int n = in & 31, t = (n == 16) ? 15 : 16;
            if (is_stolen(n)) {
                if (stealfast_on()) {
                    e_ldr(16, CPUREG, n * 8);
                    e_str(16, CPUREG, OFF_TLS);
                } else {
                    x18_prolog();
                    e_ldr(0, 1, n * 8);
                    e_str(0, 1, OFF_TLS);
                    x18_epilog();
                }
            } else if (stealfast_on()) {
                e_str(n, CPUREG, OFF_TLS);
            } else {
                // NOSTEALFAST/NOSTEAL1617 only: park scratch `t` in cpu->mscratch[0] (x28=cpu holds), NOT
                // [sp,#-16] -- a below-SP store faults on a shallow guest stack (the 6d38d96c crash class).
                // t (15/16) != n (non-stolen) and != CPUREG, so no operand is clobbered.
                e_str(t, CPUREG, (int)OFF_MSCRATCH);
                e_load_cpu(t);
                e_str(n, t, OFF_TLS);
                e_ldr(t, CPUREG, (int)OFF_MSCRATCH);
            }
            gpc += 4;
            continue;
        }

        // --- SMC prerequisite: mrs Xt, ctr_el0 (cache-type register) ---
        // __clear_cache reads CTR_EL0 to size its dc/ic strides. Reading it from EL0 FAULTS for the JIT'd
        // guest on macOS (SCTLR_EL1.UCT is not enabled for it), so the verbatim mrs crashed every guest that
        // flushes its icache. Materialize a synthetic value describing the DBT's coherence model instead:
        //   IminLine/DminLine = 4 -> 64-byte I/D lines        L1Ip = PIPT
        //   IDC (bit28) = 1 -> "DC clean to PoU not required": TRUE here, the host re-translates the page by
        //                      reading the SAME coherent memory the guest wrote, so __clear_cache skips DC.
        //   DIC (bit29) = 0 -> "IC invalidate IS required": keeps the guest issuing `ic ivau`, our SMC hook.
        if ((in & 0xFFFFFFE0u) == 0xD53B0020u) {
            int rd = in & 31;
            emit_cpu_model_value(rd, g_aarch64_cpu_model.ctr_el0);
            gpc += 4;
            continue;
        }
        if ((in & 0xFFFFFFE0u) == 0xD53B00E0u) {
            emit_cpu_model_value(in & 31, g_aarch64_cpu_model.dczid_el0);
            gpc += 4;
            continue;
        }
        // HWCAP_CPUID is absent: EL1 ID-register families are inaccessible to EL0 and must not leak host IDs.
        if ((in & 0xFFFF0000u) == 0xD5380000u && !g_aarch64_cpu_model.user_id_registers) {
            emit32(0);
            gpc += 4;
            continue;
        }

        /* DC ZVA is defined by the guest's DCZID_EL0, not by the host CPU.
           Apple hosts may use a different zero-block size; copying the opcode
           verbatim then clears bytes outside the 64-byte block advertised to
           the guest.  Managed runtimes place live metadata immediately after
           such blocks, so the mismatch surfaces later as pointer corruption.
           Lower to four exact stores at the guest-model-aligned address. */
        if ((in & 0xFFFFFFE0u) == 0xD50B7420u) {
            int source = (int)(in & 31u);
            if (is_stolen(source))
                e_ldr(16, CPUREG, source * 8);
            else
                e_movr(16, source);
            emit32(0x927AE610u); /* and x16,x16,#-64 */
            if (jit_guest_bus_active()) emit_a64_bus_guard(16, 64, gpc);
            struct a64_soft_guard soft = emit_a64_soft_guard_begin(16, 17, 18, 64, HL_LOGICAL_VMA_WRITE, gpc);
            for (unsigned offset = 0; offset < 64; offset += 16)
                emit32(0xA9000000u | (((offset / 8) & 0x7Fu) << 15) | (31u << 10) | (16u << 5) |
                       31u); /* stp xzr,xzr,[x16,#offset] */
            emit_a64_soft_guard_end(&soft);
            gpc += 4;
            continue;
        }
        // --- SMC: dc cvau, Xt (data-cache clean to PoU) -> nop ---
        // A pure no-op for a DBT: the host never instruction-fetches guest pages, so the guest's data writes
        // need no clean for our re-translation (which is a normal coherent data read). Standard __clear_cache
        // already skips DC via IDC=1 above; this also covers callers that issue it unconditionally and avoids
        // any EL0 trap on the instruction. (NOSMC keeps it -- it is unrelated to the stale-translation A/B.)
        if ((in & 0xFFFFFFE0u) == 0xD50B7B20u) {
            emit32(0xD503201Fu); // nop
            gpc += 4;
            continue;
        }
        // --- SMC: ic ivau, Xt (instruction-cache invalidate by VA to PoU) ---
        // A code-generating guest issues this (the __clear_cache / dc;dsb;ic;dsb;isb dance) before running
        // freshly-written bytes. The host never instruction-fetches guest pages (we execute the TRANSLATED
        // copy), so emitting `ic ivau` verbatim is a no-op for our cache -> the guest would re-run the STALE
        // translation. Instead end the block here and exit R_ICFLUSH: the dispatcher drops the stale gpc->host
        // map + IBTC (smc_icflush) and the modified bytes re-translate. pc resumes PAST the ic ivau. Gated by
        // NOSMC; the dc cvau / isb in the same dance run verbatim (harmless: they touch real data memory).
        if ((in & 0xFFFFFFE0u) == 0xD50B7520u && !smc_disabled()) {
            emit_smc_queue((int)(in & 31));
            gpc += 4;
            continue;
        }
        if (in == 0xD5033FDFu && !smc_disabled()) {
            emit_exit_const(gpc + 4, R_ICCOMMIT);
            break;
        }

        // --- non-branch, PC-relative: rewrite to materialize the (relocated) addr ---
        // adr
        if ((in & 0x9F000000u) == 0x10000000u) {
            int rd = in & 31;
            int64_t imm = sext((((in >> 5) & 0x7FFFF) << 2) | ((in >> 29) & 3), 21);
            uint64_t v = pcrel_base(gpc) + imm;
            if (is_stolen(rd)) {
                // stealfast: host x16 is engine-dead -> movconst + one store (no red-zone stash, no
                // TLS-based cpu reload; x28 = cpu). adrp x16 is the PLT-stub head, so this is HOT.
                if (stealfast_on()) {
                    e_movconst(16, v);
                    e_str(16, CPUREG, rd * 8);
                } else {
                    x18_prolog();
                    e_movconst(0, v);
                    e_str(0, 1, rd * 8);
                    x18_epilog();
                }
            } else
                e_movconst(rd, v);
            gpc += 4;
            continue;
        }
        // adrp
        if ((in & 0x9F000000u) == 0x90000000u) {
            int rd = in & 31;
            int64_t imm = sext((((in >> 5) & 0x7FFFF) << 2) | ((in >> 29) & 3), 21) << 12;
            uint64_t v = (pcrel_base(gpc) & ~0xFFFull) + imm;
            // Exact canonical AArch64 PLT veneer:
            //   adrp x16,page; ldr x17,[x16,#got]; add x16,x16,#lo; br x17
            if (!hl_host_range_mapped((uintptr_t)gpc, 16)) goto no_adrp_plt_fuse;
            uint32_t p1 = a64_fetch_instruction(gpc + 4, NULL);
            uint32_t p2 = a64_fetch_instruction(gpc + 8, NULL);
            uint32_t p3 = a64_fetch_instruction(gpc + 12, NULL);
            if (!guestbase_on() && !jit_guest_bus_active() && (in & 0x9F00001Fu) == 0x90000010u &&
                (p1 & 0xFFC003FFu) == 0xF9400211u && (p2 & 0xFFC003FFu) == 0x91000210u && p3 == 0xD61F0220u) {
                if (!emit_guest_adrp_page(16, v)) e_movconst(16, v);
                e_str(16, CPUREG, 16 * 8);
                uint64_t load_host = (uint64_t)g_cp;
                emit32(p1);
                pcache_record_provenance(load_host, (uint64_t)g_cp, gpc + 4);
                e_str(17, CPUREG, 17 * 8);
                emit32(p2);
                e_str(16, CPUREG, 16 * 8);
                if (!g_tier2_build && g_txln_active) {
                    uint64_t last = (gpc + 12) >> 6;
                    for (uint64_t line = gpc >> 6; line <= last; line++)
                        txln_put(line);
                    tx_last_line = last;
                }
                if (gpc + 16 > guest_end) guest_end = gpc + 16;
                emit_ibranch_ip2_ready(17, 1);
                break;
            }
        no_adrp_plt_fuse:
            if (is_stolen(rd)) {
                if (stealfast_on()) {
                    if (!emit_guest_adrp_page(16, v)) e_movconst(16, v);
                    e_str(16, CPUREG, rd * 8);
                } else {
                    x18_prolog();
                    if (!emit_guest_adrp_page(0, v)) e_movconst(0, v);
                    e_str(0, 1, rd * 8);
                    x18_epilog();
                }
            } else if (!emit_guest_adrp_page(rd, v))
                e_movconst(rd, v);
            gpc += 4;
            continue;
        }
        // ldr (literal) 32/64
        if ((in & 0xBF000000u) == 0x18000000u) {
            int rt = in & 31, is64 = (in >> 30) & 1;
            int64_t off = sext((in >> 5) & 0x7FFFF, 19) << 2;
            if (is_stolen(rt)) {
                e_movconst(16, gpc + off);
                emit_a64_bus_guard(16, is64 ? 8 : 4, gpc);
                struct a64_soft_guard soft =
                    emit_a64_soft_guard_begin(16, 17, 18, is64 ? 8 : 4, HL_LOGICAL_VMA_READ, gpc);
                if (is64)
                    e_ldr(16, 16, 0);
                else
                    emit32(0xB9400000u | (16 << 5) | 16);
                emit_a64_soft_guard_end(&soft);
                e_str(16, CPUREG, rt * 8);
                emit_a64_soft_bounce_commit(gpc + 4);
            } else {
                e_movconst(rt, gpc + off);
                emit_a64_bus_guard(rt, is64 ? 8 : 4, gpc);
                struct a64_soft_guard soft =
                    emit_a64_soft_guard_begin(rt, 16, 17, is64 ? 8 : 4, HL_LOGICAL_VMA_READ, gpc);
                if (is64)
                    e_ldr(rt, rt, 0);
                else
                    emit32(0xB9400000u | (rt << 5) | rt);
                emit_a64_soft_guard_end(&soft);
                emit_a64_soft_bounce_commit(gpc + 4);
            }
            gpc += 4;
            continue;
        }
        // ldrsw (literal): opc=10, V=0 -> top byte 0x98 (unique: bits[29:27]=011, bits[25:24]=00, bit26=0).
        // The integer ldr-literal above masks 0xBF (only bit30), so opc=10 does NOT match it and this
        // sign-extending 32->64 word literal load would fall through to the verbatim emit -- executing
        // PC-relative from the HOST code cache and loading a garbage word (then sign-extended into Xt).
        // Compilers emit LDRSW-literal for switch/jump tables (sign-extended word offsets). Same hazard
        // and same fix as the integer/SIMD forms: materialize the GUEST literal address and LDRSW from it,
        // so the value is correct regardless of host arena placement or a warm pcache load.
        if ((in & 0xFF000000u) == 0x98000000u) {
            int rt = in & 31;
            int64_t off = sext((in >> 5) & 0x7FFFF, 19) << 2;
            if (is_stolen(rt)) {
                e_movconst(16, gpc + off); // x16 = guest literal address
                emit_a64_bus_guard(16, 4, gpc);
                struct a64_soft_guard soft = emit_a64_soft_guard_begin(16, 17, 18, 4, HL_LOGICAL_VMA_READ, gpc);
                emit32(0xB9800000u | (16 << 5) | 16); // ldrsw x16, [x16]
                emit_a64_soft_guard_end(&soft);
                e_str(16, CPUREG, rt * 8);
                emit_a64_soft_bounce_commit(gpc + 4);
            } else {
                e_movconst(rt, gpc + off);
                emit_a64_bus_guard(rt, 4, gpc);
                struct a64_soft_guard soft = emit_a64_soft_guard_begin(rt, 16, 17, 4, HL_LOGICAL_VMA_READ, gpc);
                emit32(0xB9800000u | (rt << 5) | rt); // ldrsw xt, [xt]
                emit_a64_soft_guard_end(&soft);
                emit_a64_soft_bounce_commit(gpc + 4);
            }
            gpc += 4;
            continue;
        }
        // ldr (literal), SIMD&FP: `ldr St/Dt/Qt, [pc, #imm]`. The integer ldr-literal above only matches
        // V=0; the SIMD/FP form (V=1, bit26) would otherwise fall through to the verbatim emit and execute
        // PC-relative from the HOST code cache -- loading garbage instead of the guest literal pool. LuaJIT
        // trace mcode loads its double constants this way (e.g. `ldr d15,[pc,#-N]`), so a verbatim emit
        // corrupts the trace's FP constants (intermittent crashes once a bad value reaches a Lua value).
        // Rewrite it like the integer case: materialize the guest literal ADDRESS into a scratch GPR and
        // load the V register from it. opc[31:30]: 00=S(32b) 01=D(64b) 10=Q(128b); 11 is PRFM (no data reg).
        if ((in & 0x3F000000u) == 0x1C000000u && ((in >> 30) & 3) != 3) {
            int vt = in & 31, sz = (in >> 30) & 3;
            int64_t off = sext((in >> 5) & 0x7FFFF, 19) << 2;
            // ldr (V), [Xn] unsigned-offset #0 base forms, Rn=x0: S=0xBD400000 D=0xFD400000 Q=0x3DC00000
            uint32_t ld = sz == 0 ? 0xBD400000u : (sz == 1 ? 0xFD400000u : 0x3DC00000u);
            e_movconst(16, gpc + off); // x16 = guest literal address
            emit_a64_bus_guard(16, UINT64_C(4) << sz, gpc);
            struct a64_soft_guard soft =
                emit_a64_soft_guard_begin(16, 17, 18, UINT64_C(4) << sz, HL_LOGICAL_VMA_READ, gpc);
            emit32(ld | (16u << 5) | (uint32_t)vt); // ldr St/Dt/Qt, [x16]
            emit_a64_soft_guard_end(&soft);
            emit_a64_soft_bounce_commit(gpc + 4);
            gpc += 4;
            continue;
        }
        // prfm (literal): opc=11, V=0 -> top byte 0xD8. A prefetch HINT that reads its target address
        // PC-relative from the guest literal pool. It has no destination register and never faults, but a
        // verbatim emit would prefetch a host-PC-relative (garbage) address -- useless work, never the
        // intended guest line. Prefetch is architecturally optional, so honoring it as "no prefetch" is
        // always legal: drop it to a nop. This completes the PC-relative literal-load family (0x18/0x58
        // LDR-lit, 0x98 LDRSW-lit, 0x1C/0x5C/0x9C LDR-lit-SIMD, 0xD8 PRFM-lit) -- every form rewritten.
        if ((in & 0xFF000000u) == 0xD8000000u) {
            emit32(0xD503201Fu); // nop
            gpc += 4;
            continue;
        }
        /* Register/immediate PRFM is also an architecturally optional,
           non-faulting hint.  Sending it through the ordinary fold would
           incorrectly require WRITE permission and could synthesize a guest
           fault.  Drop it exactly like PRFM-literal. */
        if (is_prfm_register_or_immediate(in)) {
            emit32(0xD503201Fu);
            gpc += 4;
            continue;
        }

        // pointer authentication (ubuntu 24.04 -mbranch-protection): we don't enforce PAC, and signing
        // x30 on the PAC-capable host would corrupt the §B shadow-stack return match (it expects an
        // UNSIGNED guest x30) -> wild branch to a signed address. Neutralize PAC (hardening, not
        // semantics): paci*/auti* hints -> nop (x30 stays unsigned); retaa/retab -> a plain x30 ret.
        // paciasp/autiasp/paci?z/... -> nop
        if ((in & 0xFFFFFF1Fu) == 0xD503231Fu) {
            emit32(0xD503201Fu);
            gpc += 4;
            continue;
        }
        // retaa/retab -> shadow ret (x30)
        if ((in & 0xFFFFFBFFu) == 0xD65F0BFFu) {
            shadowgate() == -1 ? emit_ibranch(30) : emit_shadow_ret();
            break;
        }

        // guest_base bias-fold: a non-PIE image's LOW absolute load/store -> +bias (the high mapping), no
        // fault. Only outside an exclusive monitor region (the fold spills scratch to memory, which would
        // clear the monitor) and only for a non-SP base (the stack is always high). The AdvSIMD load/store
        // structure family (ld1/st1.., ld1r) has no offset/index so is_foldable_mem omits it -- fold it via
        // its own emitter (else glibc's NEON strlen/memcpy trap once per access on the image). Inert for PIE.
        if ((guestbase_on() || jit_guest_soft_active()) && !in_excl &&
            (jit_guest_soft_active() || ((in >> 5) & 31) != 31)) {
            if (is_foldable_mem(in)) {
                if (jit_guest_bus_active()) emit_a64_bus_guard_instruction(in, gpc);
                emit_fold_mem(in, 0);
                gpc += 4;
                continue;
            }
            if (is_advsimd_struct(in)) {
                emit_fold_advsimd_struct(in);
                gpc += 4;
                continue;
            }
        }
        if (jit_guest_bus_active()) {
            if (!guestbase_on() && !in_excl && is_foldable_mem(in)) emit_a64_bus_guard_instruction(in, gpc);
            if (!guestbase_on() && !in_excl && is_advsimd_struct(in))
                emit_a64_bus_guard_base((in >> 5) & 31, 0, (uint64_t)advsimd_struct_bytes(in), gpc);
            /* Pair pre/post-index forms are deliberately not bias-folded.  Guard
               their architectural access address, then preserve the native
               writeback opcode verbatim below. */
            if ((in & 0x3A000000u) == 0x28000000u) {
                int mode = (in >> 23) & 3;
                if (mode == 1 || mode == 3) {
                    uint64_t total = a64_mem_bytes(in);
                    int64_t imm = sext((in >> 15) & 0x7f, 7) * (int64_t)(total / 2);
                    emit_a64_bus_guard_base((in >> 5) & 31, mode == 3 ? imm : 0, total, gpc);
                }
            }
            /* Guard once at the load-exclusive edge.  No call is injected
               between LDXR/LDXP and STXR/STXP, which would clear the host
               exclusive monitor; activation cannot publish a new BUS range
               until this thread reaches the dispatcher. */
            if ((in & 0x3FC00000u) == 0x08400000u && !is_casp(in)) {
                uint64_t bytes = UINT64_C(1) << ((in >> 30) & 3);
                if ((in >> 21) & 1) bytes *= 2;
                emit_a64_bus_guard_base((in >> 5) & 31, 0, bytes, gpc);
            }
        }
        if (jit_guest_soft_active() && (((in & 0x3F000000u) == 0x08000000u) || is_casp(in))) {
            emit_a64_soft_exclusive(in);
            gpc += 4;
            continue;
        }
        // CASP paired compare-and-swap: the mangle machinery only substitutes NAMED register fields, so a
        // stolen pair partner (Xs+1 / Xt+1) would slip through verbatim. Relocate both pairs when any member
        // is stolen; otherwise (the common case) emit verbatim -- byte-identical to before.
        if (is_casp(in)) {
            if (casp_uses_stolen(in))
                emit_casp_mangled(in, -1);
            else
                emit32(in);
            gpc += 4;
            continue;
        }
        // Exact ORR-alias MOV from a stolen source into a live guest host reg.
        if ((in & 0x7FE0FFE0u) == 0x2A0003E0u) {
            int rd = in & 31, rm = (in >> 16) & 31;
            if (rd != 31 && !is_stolen(rd) && is_stolen(rm)) {
                if (in >> 31)
                    e_ldr(rd, CPUREG, rm * 8);
                else
                    emit32(0xB9400000u | ((unsigned)(rm * 2) << 10) | ((unsigned)CPUREG << 5) | (unsigned)rd);
                gpc += 4;
                continue;
            }
        }
        // Exact MOVZ Wd/Xd,#0,LSL#0.
        if (stealfast_on() && (in & 0x7FFFFFE0u) == 0x52800000u && is_stolen(in & 31)) {
            e_str(31, CPUREG, (int)(in & 31) * 8);
            gpc += 4;
            continue;
        }
        // everything else: verbatim,
        int mask = gpr_field_mask(in);
        if (uses_x18(in, mask))
            // unless it names x18 -> mangle
            emit_mangled_x18(in, mask);
        else
            emit32(in);
        gpc += 4;
    }
    if (provenance_fault_capable) jit_instruction_map_put(provenance_host, (uint64_t)g_cp, provenance_guest);
    // emit the deferred exit stubs for branches taken inside an exclusive region
    for (int i = 0; i < ndefer; i++) {
        int64_t d = ((uint8_t *)g_cp - (uint8_t *)defer[i].patch) / 4;
        *defer[i].patch = recode_cond(defer[i].in, d);
        emit_chain_exit(defer[i].target);
    }
    chain_exit_dedup_finish();
    // IRQSLIM: the out-of-line poll exit stub the body-entry cbnz targets (irq set -> exit to
    // the dispatcher at the block start, exactly like the legacy inline poll).
    if (g_irq_patch || g_t2_irq_patch) {
        int pad_removed_t2_exit = g_t2_irq_patch != NULL;
        uint8_t *stub = g_cp;
        if (g_irq_patch) {
            uint32_t *p = g_irq_patch;
            g_irq_patch = NULL;
            *p = 0xB5000000u | (((uint32_t)((stub - (uint8_t *)p) / 4) & 0x7FFFF) << 5) | 16;
        }
        if (g_t2_irq_patch) {
            uint32_t *p = g_t2_irq_patch;
            g_t2_irq_patch = NULL;
            *p = 0xB5000000u | (((uint32_t)((stub - (uint8_t *)p) / 4) & 0x7FFFF) << 5) | 16;
        }
        uint8_t *exit_begin = g_cp;
        emit_exit_const(start, R_BRANCH);
        size_t exit_bytes = (size_t)(g_cp - exit_begin);
        if (pad_removed_t2_exit)
            for (size_t off = 0; off < exit_bytes; off += 4)
                emit32(0xD503201Fu);
    }
    emit_a64_bus_stub();
    emit_a64_soft_stub();
    size_t emitted_bytes = (size_t)(g_cp - (uint8_t *)host);
    if (emitted_bytes >= (1u << 20))
        HL_LOGF(&g_jit_log, HL_LOG_TAG_JIT,
                "large block guest=%#llx source=%#llx-%#llx bytes=%zu bus_sites=%u soft_sites=%u",
                (unsigned long long)start, (unsigned long long)guest_start, (unsigned long long)guest_end,
                emitted_bytes, g_bus_stub_patch_count, g_soft_stub_patch_count);
    // Only the REGION HEAD (start) is registered; intermediate inlined block-starts are left
    // unregistered so a later mid-region entry self-heals via re-translate + back-patch.
    // W4E tier-2: the promoter (g_tier2_build) recompiles in place and updates the EXISTING map entry
    // itself, so don't insert a duplicate. Expose the body for it.
    g_last_body = body;
    g_last_guest_start = guest_start;
    g_last_guest_end = guest_end;
    if (!g_tier2_build) {
        map_put(start, guest_start, guest_end, host, body);
        // SMC precise gate: record every guest page this block's SOURCE spans, so a later guest `ic ivau`
        // to one of these pages takes the full invalidation while a flush of any never-translated page is
        // skipped. `gpc` is the (exclusive) end of the decoded block here; `start` is its entry.
        txpg_mark(start, guest_end);
    }
    // patch_links_to is MOVED to the dispatcher, AFTER the new block's icache is invalidated:
    // chaining an existing block X -> this new block before its code is icache-coherent on a peer
    // core lets that core fetch stale instructions. Only chain to it once it's visible everywhere.
    return host;
}

#undef STITCH_OK

