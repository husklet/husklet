static void emit_atomic_part(uint32_t in, int mask, int is_mem) {
    if (is_mem && (guestbase_on() || jit_guest_soft_active()) && (jit_guest_soft_active() || ((in >> 5) & 31) != 31)) {
        if (jit_guest_bus_active()) emit_a64_bus_guard_instruction(in, g_emit_gpc);
        emit_fold_mem(in, 0);
    } else if (uses_x18(in, mask))
        emit_mangled_x18(in, mask);
    else
        emit32(in);
}

// The rewritten LSE op replaces a whole ldxr/stxr RETRY LOOP, and that loop only ever falls out of
// `cbnz Ws, loop` with the store-exclusive status register Ws == 0. A single LSE instruction never
// touches Ws, so without this the guest keeps the stale pre-loop value of Ws -- an architectural
// divergence the differential ISA fuzzer (tests/fuzz/isa/aarch64) caught directly. Emit it LAST: in
// the original loop the stxr writes Ws after every other operand, so a Ws that aliases Wt/Ws2/Wm
// must also end up zero.
static void emit_lse_status_zero(int Ws) {
    if (Ws == 31) return;                               /* stxr wzr: the status is architecturally discarded */
    emit_atomic_part(0x2A1F03E0u | (uint32_t)Ws, 1, 0); /* mov Ws, wzr */
}

// Returns bytes consumed (12 or 16) if a known atomic loop at gpc was rewritten, else 0.
static int try_lse_atomic(uint64_t gpc) {
    uint32_t i0 = a64_fetch_instruction(gpc, NULL);
    // load-exclusive?
    if ((i0 & 0x3F400000u) != 0x08400000u) return 0;
    int sz = (i0 >> 30) & 3;
    // word/dword only
    if (sz < 2) return 0;
    // non-pair
    if (((i0 >> 16) & 0x1F) != 0x1F || ((i0 >> 10) & 0x1F) != 0x1F) return 0;
    int Wt = i0 & 31, Xn = (i0 >> 5) & 31;
    uint32_t i1 = a64_fetch_instruction(gpc + 4, NULL);

    // SWP:  ldxr Wt,[Xn]; stxr Ws,Wv,[Xn]; cbnz Ws,loop
    if ((i1 & 0x3F400000u) == 0x08000000u && ((i1 >> 30) & 3) == sz && ((i1 >> 10) & 0x1F) == 0x1F &&
        ((i1 >> 5) & 31) == Xn) {
        int Ws = (i1 >> 16) & 31, Wv = i1 & 31;
        uint32_t i2 = a64_fetch_instruction(gpc + 8, NULL);
        if ((i2 & 0xFF000000u) == 0x35000000u && (i2 & 31) == Ws &&
            (gpc + 8 + (uint64_t)(sext((i2 >> 5) & 0x7FFFF, 19) << 2)) == gpc) {
            // A bare `swpal` in place of this swap loop is a deterministic lost-wakeup for multithreaded
            // musl: node's V8 workers park forever in __unlock's `a_swap(l,0)==2 && __wake` because the
            // swp'd old value doesn't drive the wake (node:alpine hung >400s; the exclusive pair completes
            // in 0.28s, matching the docker oracle). The `ldadd*`/`casal` idioms below are unaffected. So
            // upgrade ONLY when the exclusive-pair fallback is UNUSABLE -- i.e. when translating it verbatim
            // would inject a monitor-clearing ldr/str between the ldxr and stxr (a stolen operand needs a
            // cpu-slot mangle, or a non-PIE low base needs a bias-fold), which would spin the stxr forever.
            // The common clean-PIE case (no stolen operand, no fold) keeps the proven exclusive pair.
            if (guestbase_on() || is_stolen(Wt) || is_stolen(Xn) || is_stolen(Ws) || is_stolen(Wv)) {
                // swpal Wv, Wt, [Xn] (a single LSE op; emit_atomic_part folds/mangles the corner cases).
                emit_atomic_part(0xB8E08000u | (sz == 3 ? 0x40000000u : 0) | (Wv << 16) | (Xn << 5) | Wt, 1 | 2 | 4, 1);
                emit_lse_status_zero(Ws);
                g_lse_n++;
                return 12;
            }
        }
    }
    // LDADD/LDSET/LDEOR/LDCLR/LDADD-neg:  ldxr Wt,[Xn]; <op> Ws2,Wt,Wm; stxr Ws,Ws2,[Xn]; cbnz Ws,loop
    // 0 add 1 orr 2 eor 3 and 4 sub
    int op = -1;
    if ((i1 & 0xFFE0FC00u) == (sz == 3 ? 0x8B000000u : 0x0B000000u))
        op = 0;
    else if ((i1 & 0xFFE0FC00u) == (sz == 3 ? 0xAA000000u : 0x2A000000u))
        op = 1;
    else if ((i1 & 0xFFE0FC00u) == (sz == 3 ? 0xCA000000u : 0x4A000000u))
        op = 2;
    else if ((i1 & 0xFFE0FC00u) == (sz == 3 ? 0x8A000000u : 0x0A000000u))
        op = 3;
    else if ((i1 & 0xFFE0FC00u) == (sz == 3 ? 0xCB000000u : 0x4B000000u))
        op = 4;
    if (op >= 0) {
        int Ws2 = i1 & 31, n = (i1 >> 5) & 31, m = (i1 >> 16) & 31, Wm = -1;
        if (op == 4) {
            if (n == Wt) Wm = m;
            // sub: not commutative, Rn must be Wt
        } else {
            if (n == Wt)
                Wm = m;
            else if (m == Wt)
                Wm = n;
        }
        uint32_t i2 = a64_fetch_instruction(gpc + 8, NULL), i3 = a64_fetch_instruction(gpc + 12, NULL);
        if (Wm >= 0 && (i2 & 0x3F400000u) == 0x08000000u && ((i2 >> 30) & 3) == sz && (i2 & 31) == Ws2 &&
            ((i2 >> 5) & 31) == Xn && ((i2 >> 10) & 0x1F) == 0x1F) {
            int Ws = (i2 >> 16) & 31;
            if ((i3 & 0xFF000000u) == 0x35000000u && (i3 & 31) == Ws &&
                (gpc + 12 + (uint64_t)(sext((i3 >> 5) & 0x7FFFF, 19) << 2)) == gpc) {
                // op>=3 borrows Ws as a scratch holding ~Wm / -Wm across two ops -> it must not alias Wm.
                if (op >= 3 && Wm == Ws) return 0;
                uint32_t szb = sz == 3 ? 0x40000000u : 0, szd = sz == 3 ? 0x80000000u : 0;
                if (op <= 2) {
                    uint32_t lse = op == 0 ? 0xB8E00000u : op == 1 ? 0xB8E03000u : 0xB8E02000u;
                    // ldaddal/ldsetal/ldeoral Wm, Wt, [Xn]
                    emit_atomic_part(lse | szb | (Wm << 16) | (Xn << 5) | Wt, 1 | 2 | 4, 1);
                } else if (op == 3) {
                    // fetch_and: *Xn &= Wm  ==  ldclr ~Wm:  mvn Ws,Wm (orn Ws,wzr,Wm); ldclral Ws, Wt, [Xn]
                    emit_atomic_part(0x2A200000u | szd | (Wm << 16) | (31 << 5) | Ws, 1 | 4, 0);
                    emit_atomic_part(0xB8E01000u | szb | (Ws << 16) | (Xn << 5) | Wt, 1 | 2 | 4, 1);
                } else {
                    // fetch_sub: *Xn -= Wm  ==  ldadd -Wm:  neg Ws,Wm (sub Ws,wzr,Wm); ldaddal Ws, Wt, [Xn]
                    emit_atomic_part(0x4B000000u | szd | (Wm << 16) | (31 << 5) | Ws, 1 | 4, 0);
                    emit_atomic_part(0xB8E00000u | szb | (Ws << 16) | (Xn << 5) | Wt, 1 | 2 | 4, 1);
                }
                // reconstruct the new value (re-emit the original op) for any following guest code
                emit_atomic_part(i1, gpr_field_mask(i1), 0);
                emit_lse_status_zero(Ws);
                g_lse_n++;
                return 16;
            }
        }
    }
    // LDADD immediate (fetch_add of a constant -- the headline refcount/counter case):
    //   ldxr Wt,[Xn]; add Ws2,Wt,#imm (sh=0); stxr Ws,Ws2,[Xn]; cbnz Ws,loop
    uint32_t addib = sz == 3 ? 0x91000000u : 0x11000000u;
    if ((i1 & 0xFFC00000u) == addib && ((i1 >> 5) & 31) == Wt) {
        int Ws2 = i1 & 31;
        unsigned imm = (i1 >> 10) & 0xFFF;
        uint32_t i2 = a64_fetch_instruction(gpc + 8, NULL), i3 = a64_fetch_instruction(gpc + 12, NULL);
        if ((i2 & 0x3F400000u) == 0x08000000u && ((i2 >> 30) & 3) == sz && (i2 & 31) == Ws2 && ((i2 >> 5) & 31) == Xn &&
            ((i2 >> 10) & 0x1F) == 0x1F) {
            int Ws = (i2 >> 16) & 31;
            if ((i3 & 0xFF000000u) == 0x35000000u && (i3 & 31) == Ws &&
                (gpc + 12 + (uint64_t)(sext((i3 >> 5) & 0x7FFFF, 19) << 2)) == gpc) {
                uint32_t szb = sz == 3 ? 0x40000000u : 0;
                // Ws (dead status reg) = imm  (movz Ws, #imm; e_movz always uses the 64-bit form)
                emit_atomic_part(0xD2800000u | ((imm & 0xFFFFu) << 5) | Ws, 1, 0);
                // ldaddal Ws, Wt, [Xn]
                emit_atomic_part(0xB8E00000u | szb | (Ws << 16) | (Xn << 5) | Wt, 1 | 2 | 4, 1);
                // re-emit add Ws2, Wt, #imm (reconstruct the new value)
                emit_atomic_part(i1, gpr_field_mask(i1), 0);
                emit_lse_status_zero(Ws);
                g_lse_n++;
                return 16;
            }
        }
    }
    // CAS:  ldxr Wt,[Xn]; cmp Wt,Wexp; b.ne out; stxr Ws,Wnew,[Xn]; cbnz Ws,loop; out:
    // subs wzr, Wt, Wexp (cmp)
    uint32_t subsb = sz == 3 ? 0xEB00001Fu : 0x6B00001Fu;
    if ((i1 & 0xFFE0FC1Fu) == subsb && ((i1 >> 5) & 31) == Wt) {
        int Wexp = (i1 >> 16) & 31;
        uint32_t i2 = a64_fetch_instruction(gpc + 8, NULL), i3 = a64_fetch_instruction(gpc + 12, NULL);
        uint32_t i4 = a64_fetch_instruction(gpc + 16, NULL);
        // b.ne
        if ((i2 & 0xFF00001Fu) == 0x54000001u && (i3 & 0x3F400000u) == 0x08000000u && ((i3 >> 30) & 3) == sz &&
            ((i3 >> 10) & 0x1F) == 0x1F && ((i3 >> 5) & 31) == Xn && (i4 & 0xFF000000u) == 0x35000000u &&
            (i4 & 31) == ((i3 >> 16) & 31) &&
            // cbnz -> loop
            (gpc + 16 + (uint64_t)(sext((i4 >> 5) & 0x7FFFF, 19) << 2)) == gpc
            // b.ne -> out
            && (gpc + 8 + (uint64_t)(sext((i2 >> 5) & 0x7FFFF, 19) << 2)) == gpc + 20) {
            int Wnew = i3 & 31;
            // casal carries the compare/old value in Wt, so Wt must differ from Wexp (a stolen Wt flows
            // through its cpu slot across the three ops). The bare ldxr/stxr fallback would spin on a stolen
            // operand / low non-PIE [Xn], so route every part through emit_atomic_part.
            if (Wt == Wexp) return 0;
            uint32_t szd = sz == 3 ? 0x80000000u : 0;
            // mov Wt, Wexp (orr Wt, wzr, Wexp): Rd=Wt[0], Rm=Wexp[16]
            emit_atomic_part(0x2A000000u | szd | (Wexp << 16) | (31 << 5) | Wt, 1 | 4, 0);
            // casal Wt, Wnew, [Xn]; Wt = old:  Rs=Wt[16], Rn=Xn[5], Rt=Wnew[0]
            emit_atomic_part((sz == 3 ? 0xC8E0FC00u : 0x88E0FC00u) | (Wt << 16) | (Xn << 5) | Wnew, 1 | 2 | 4 | 8, 1);
            // cmp Wt, Wexp (reproduce NZCV): subs wzr, Wt, Wexp -> Rn=Wt[5], Rm=Wexp[16]
            emit_atomic_part(0x6B00001Fu | szd | (Wexp << 16) | (Wt << 5), 2 | 4, 0);
            // The guest loop reaches `stxr Ws` only when the compare matched; on the b.ne-out path Ws
            // keeps its pre-loop value. Reproduce both with a csel off the NZCV just recomputed.
            {
                int Ws = (i3 >> 16) & 31;
                // 64-bit csel: the not-taken path must preserve the FULL guest register (a 32-bit csel
                // would zero the top half of an untouched Ws); the taken path selects xzr, and stxr's
                // W-sized status write is zero-extending anyway, so 0 is right for it too.
                if (Ws != 31)
                    emit_atomic_part(0x9A800000u | ((uint32_t)Ws << 16) | (31u << 5) | (uint32_t)Ws, 1 | 2 | 4, 0);
            }
            g_lse_n++;
            return 20;
        }
    }
    return 0;
}

// ---- LSE outline-atomic call inlining ----
// GCC/LLVM emit every C atomic as a `bl __aarch64_<op><sz>_<order>` outline helper (the distro/musl and
// -mno-outline-atomics-ignored toolchains still do). The helper is a fixed 5-insn leaf:
//     adrp x16,#page ; ldrb w16,[x16,#off] ; cbz w16, Lfallback ; <host LSE op> ; ret   Lfallback: ldxr/stxr..
// The gated byte is __aarch64_have_lse_atomics -- ALWAYS 1 here (we advertise HWCAP_ATOMICS and the host
// has FEAT_LSE), so the fast-path single LSE op IS the architectural effect of the call. Inline that one
// op at the call site: elide the bl + adrp/ldrb/cbz + ret AND the block-split/return dispatch (the call
// idiom is ~2 helper round-trips per atomic in tight code -- the dominant hl-vs-native atomics tax, since
// the LSE op itself already lowers 1:1). The op reads/writes guest memory with its native [Xn] base, so
// inline ONLY when an in-stream copy of that op would be emitted verbatim too: guestbase off (PIE/static-
// PIE) and BUS inactive. Otherwise fall through to the normal call (the helper still runs correctly).
// Returns 1 if it inlined (caller advances past the bl and keeps the block going), else 0.
static int try_inline_outline_atomic(uint64_t gpc, uint64_t target) {
    /*
     * This optimization embeds an instruction read from an out-of-line helper
     * in the caller translation.  The initial SMC prime removes all such
     * pre-SMC callers; do not create new hidden source dependencies once map
     * entries are individually invalidatable.
     */
    if (smc_seen() || guestbase_on() || jit_guest_bus_active()) return 0;
    uint32_t i0 = a64_fetch_instruction(target, NULL), i1 = a64_fetch_instruction(target + 4, NULL);
    uint32_t i2 = a64_fetch_instruction(target + 8, NULL), i3 = a64_fetch_instruction(target + 12, NULL);
    uint32_t i4 = a64_fetch_instruction(target + 16, NULL);
    // adrp x16, #page
    if ((i0 & 0x9F00001Fu) != 0x90000010u) return 0;
    // ldrb w16, [x16, #imm12]
    if ((i1 & 0xFFC003FFu) != 0x39400210u) return 0;
    // cbz w16, Lfallback  (byte==0 -> ldxr/stxr fallback; byte!=0 falls through to the LSE op i3)
    if ((i2 & 0xFF00001Fu) != 0x34000010u) return 0;
    // the cbz must jump PAST i3 (forward, skipping the fast-path op) so the fall-through reaches i3
    if ((int64_t)(sext((i2 >> 5) & 0x7FFFF, 19) << 2) < 8) return 0;
    // i4 = ret x30 (the helper is a leaf; x30 is preserved across it)
    if (i4 != 0xD65F03C0u) return 0;
    // i3 must be a single-[Xn]-base LSE atomic memory op (LDADD/SWP/LDSET/...) or a CAS (single).
    int is_lse = (i3 & 0x3F200C00u) == 0x38200000u;
    int is_cas = (i3 & 0x3FA07C00u) == 0x08A07C00u;
    if (!is_lse && !is_cas) return 0;
    // no stolen operand (Rs[20:16], Rn[9:5], Rt[4:0]) -> the op is safe to copy verbatim
    if (is_stolen(i3 & 31) || is_stolen((i3 >> 5) & 31) || is_stolen((i3 >> 16) & 31)) return 0;
    // architectural x30 after the (elided) bl+ret is the return address; set it so a later reader / signal
    // / unwinder sees exactly what the real call would have left. (Un-biased low vaddr for non-PIE; identity
    // for PIE -- but guestbase is off here anyway.)
    emit_set_x30(pcrel_base(gpc) + 4);
    emit32(i3);
    g_lse_n++;
    return 1;
}

// ---- tier-2 substrate: the purity gate (the analyze() of trace_pipeline.c) ----
// Given a formed trace's instructions, return 1 only if it is safe to MEMOIZE:
// no syscall (svc) and no memory access at all -- so the result is fully determined
// by the input registers and there are no side effects. Conservative by construction:
// any load/store or syscall -> impure -> emit unoptimized (side effects must run).
// This is the gate that refuses the impure region in the pipeline (a wrong gate here
// is a miscompile). Linear in trace length, run once on promotion. Verified by
// TIER2_SELFTEST; wired into specialization when trace formation (the "form trace"
// step) lands -- the remaining substrate brick.
