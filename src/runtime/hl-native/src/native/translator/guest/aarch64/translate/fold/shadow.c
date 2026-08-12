static int region_pure(const uint32_t *code, int n) {
    for (int i = 0; i < n; i++) {
        uint32_t in = code[i];
        // svc -> side effect
        if (in == 0xD4000001u) return 0;
        // any load/store -> not register-determined
        if ((in & 0x0A000000u) == 0x08000000u) return 0;
    }
    // pure: register-to-register computation only
    return 1;
}

// ---- §B shadow-stack return prediction: the validated mechanism (PoC: shadow_stack.c) ----
// At a guest `bl`, record the guest return address. At a guest `ret`, classify the guest's x30:
//   FAST    -> matches the top of the shadow stack: the normal return; take a host `ret` (the
//              hardware RAS predicts it in ~1 insn instead of the ~14-insn ret-IBTC).
//   UNWIND  -> matches a deeper frame (longjmp / multi-frame return): pop to it, still correct.
//   FOREIGN -> not on the shadow (computed/tail return): fall back to the IBTC.
// Conservative: ONLY the FAST path takes the host ret; UNWIND/FOREIGN fall back, so a return can
// never land at the wrong target. The codegen that emits host bl/ret + the x30 steal wires onto
// this (the one subtlety past the PoC is x30's dual role: host return address vs guest-visible
// link value -- handled by keeping guest x30 in cpu->x[30] and validating here).
enum { SS_FAST, SS_UNWIND, SS_FOREIGN };

static inline void shadow_push(struct cpu *c, uint64_t guest_ret, uint64_t host_ret) {
    if (c->ssp < 1024) {
        c->sstk[2 * c->ssp] = guest_ret;
        c->sstk[2 * c->ssp + 1] = host_ret;
        c->ssp++;
    }
}

// matches on guest_ret (even index)
static int shadow_classify(struct cpu *c, uint64_t guest_x30) {
    if (c->ssp > 0 && c->sstk[2 * (c->ssp - 1)] == guest_x30) {
        c->ssp--;
        return SS_FAST;
    }
    for (uint64_t d = 2; d <= c->ssp && d <= 64; d++)
        if (c->sstk[2 * (c->ssp - d)] == guest_x30) {
            c->ssp -= d;
            return SS_UNWIND;
        }
    return SS_FOREIGN;
}

// ---- opt4: greedy superblock / trace formation ----
// Follow unconditional `b` edges INLINE, and lay conditional fall-through successors INLINE
// (inverting the guest condition so the TAKEN side becomes a tiny out-of-line chain-exit).
// A region is bounded to TRACE_MAX_BYTES / TRACE_MAX_BLK; intermediate guest block-starts are
// deliberately NOT registered in g_map -- any edge that later enters mid-region self-heals by
// re-translating a fresh (always-correct) duplicate, wired up through the existing
// add_pend/patch_links_to back-patch path. NOSTITCH=1 -> g_stitch=0 -> exact single-block
// baseline (env read once; set-once + idempotent under the JIT lock).
#define TRACE_MAX_BLK 16
#define TRACE_MAX_BYTES (16 * 1024)
static int g_stitch = -1;

static int seen_has(const uint64_t *seen, int n, uint64_t v) {
    for (int i = 0; i < n; i++)
        if (seen[i] == v) return 1;
    return 0;
}

// Lay a conditional's fall-through inline: `inv` is the branch insn with its condition/op
// already inverted, so when the guest would NOT take it we keep falling through. Emit the
// inverted branch (skips the taken-side exit), the taken chain-exit, then patch the branch to
// jump just past it. The patched offset is always tiny (the taken exit is ~1 insn if chained,
// ~30 if it spills) -> in range even for tbz/tbnz's 14-bit field.
static void stitch_cond(uint32_t inv, uint64_t taken) {
    uint32_t *patch = (uint32_t *)g_cp;
    emit32(0);
    emit_chain_exit(taken);
    *patch = recode_cond(inv, ((uint8_t *)g_cp - (uint8_t *)patch) / 4);
}
