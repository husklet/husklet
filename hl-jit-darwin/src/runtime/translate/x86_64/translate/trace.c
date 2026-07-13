// ---- W3-A: trace / superblock formation (gate NOSTITCH=1; default ON) ----
// Port of the aarch64 opt4 PoC to the x86 engine. Greedily lay successor blocks inline:
//  - unconditional `jmp rel` (E9/EB): if the target is a fresh (untranslated, not-yet-inlined)
//    block, continue translating it inline -- the inter-block `b body` disappears.
//  - conditional `jcc` fall-through: lay the not-taken (`next`) successor inline, branchless.
//    The ARM condition is INVERTED so the emitted b.cond jumps over the (tiny, out-of-line)
//    taken-side exit; the taken successor becomes the out-of-line exit.
// Intermediate blocks are deliberately NOT registered in g_map, so any mid-region entry simply
// re-translates a (correct) duplicate and self-heals via the existing add_pend/patch_links_to
// back-patch path. Region bounded to TRACE_MAX_BLK blocks / TRACE_MAX_BYTES host bytes.
//
// x86 lazy-flag interplay (opt3, the critical correctness point): a width-4/8 sub/cmp/add/logic
// producer DEFERS its NZCV materialization, leaving its result flags live in ARM NZCV with
// g_fl_pending naming the finalizer that would spill them to cpu->nzcv. A chained/inlined entry
// reaches a successor's post-prologue body (no NZCV reload), so cpu->nzcv MUST be canonical at
// every stitched boundary:
//  - `jmp` stitch is flag-clean for free: the top-of-loop already materializes g_fl_pending
//    before any non-Jcc instruction (the jmp itself), so g_fl_pending == FL_NONE and the membank
//    is current when the jmp handler runs -- inline continuation needs nothing extra.
//  - `jcc` fall-through stitch runs AFTER the existing fast-path flags_materialize() (the
//    producer's exact spill, which also msr's the value back so live NZCV stays canonical). So
//    the out-of-line taken exit AND the inline fall-through both see a correct cpu->nzcv, and
//    g_fl_pending == FL_NONE for the inlined successor.
static int g_stitch = -1; // -1 uninit; resolved lazily from env (NOSTITCH=1 disables)
#define TRACE_MAX_BLK 16
#define TRACE_MAX_BYTES (16u * 1024u)

static int seen_has(const uint64_t *s, int n, uint64_t v) {
    for (int i = 0; i < n; i++)
        if (s[i] == v) return 1;
    return 0;
}

// A successor whose first byte is a trap (hlt / int3 / ud2) is a dynamically-dead guard arm --
// e.g. musl's alloca size check `cmp size,0xfff; jbe ok; hlt`, where the hlt fall-through is the
// never-taken oversize trap. Do NOT eagerly inline it: leave it as a normal out-of-line exit so
// it is only ever translated if actually reached (it isn't), avoiding wasted code + report_unimpl.
static int trap_head(uint64_t a) {
    const uint8_t *p = (const uint8_t *)a;
    return p[0] == 0xF4 || p[0] == 0xCC || (p[0] == 0x0F && p[1] == 0x0B);
}

// ==================== W5B adaptive tier-2 (x86 engine) ====================
// emit the in-cache back-edge hotness counter for a hot-candidate self-loop (tier-1 build only). Runs on
// the TAKEN (loop) edge. x16/x17 are host scratch here (NOT guest regs on the x86 engine: guest GPRs are
// x0..x15), so no spill is needed — simpler than the aarch64 port. The counter is flag-free (movconst /
// ldr / sub-imm(D1) / str / cbnz never touch NZCV), so the guest's condition flags survive the back-edge.
// Counts DOWN from g_t2thresh; on reaching zero it exits R_TIER2 (rip = loop start) so the dispatcher
// promotes the block, after which this stub is dead.
static void emit_t2_counter_x86(int slot, uint64_t start, void *body) {
    emit_host_ptr(16, (uint64_t)&g_t2cnt[slot], PRELOC_HOSTGLOBAL); // x16 = &g_t2cnt[slot]  (plain RW data)
    e_ldr(17, 16, 0);                                               // x17 = count
    e_subi(17, 17, 1, 1);                                           // --count (sub-imm: flag-free)
    e_str(17, 16, 0);
    uint32_t *p_cbnz = (uint32_t *)g_cp;
    emit32(0); // cbnz x17, Lcont (still counting -> keep looping; flag-free)
    // reached 0 -> exit to the dispatcher to promote (rip = loop start; counter dead afterwards)
    emit_exit_const(start, R_TIER2);
    uint8_t *Lcont = g_cp;
    *p_cbnz = 0xB5000000u | (((uint32_t)(((uint8_t *)Lcont - (uint8_t *)p_cbnz) / 4) & 0x7FFFF) << 5) | 17;
    int64_t d = ((uint8_t *)body - (uint8_t *)g_cp) / 4; // b body (the loop back-edge, in-cache)
    emit32(0x14000000u | ((uint32_t)d & 0x3FFFFFFu));
}

// store-to-load-forwarding hazard guard (ported verbatim from W4E): folding the back-edge tightens the
// loop enough that a store immediately followed by a load of the SAME address starts hitting an
// Apple-Silicon store-forwarding replay. Scan the EMITTED host code [body,end) for a load that reuses a
// stored (size,base,offset); if found, leave the loop on tier-1 (no counter, no fold). Pure-store /
// load-only / distinct-address loops are NOT flagged and still tier up.
static int loop_has_rmw_hazard(uint64_t body, uint64_t end) {
    uint64_t stores[32];
    int ns = 0;
    for (uint64_t p = body; p < end; p += 4) {
        uint32_t in = *(uint32_t *)p;
        uint64_t key = 0;
        int opc = -1;
        if ((in & 0x3B000000u) == 0x39000000u) { // load/store unsigned imm12
            opc = (in >> 22) & 3;
            key = ((uint64_t)((in >> 30) & 3) << 24) | (((in >> 5) & 31) << 12) | ((in >> 10) & 0xFFF);
        } else if ((in & 0x3B200C00u) == 0x38000000u) { // STUR/LDUR unscaled imm9
            opc = (in >> 22) & 3;
            key = (1ull << 40) | ((uint64_t)((in >> 30) & 3) << 24) | (((in >> 5) & 31) << 12) | ((in >> 12) & 0x1FF);
        }
        if (opc == 0) {
            if (ns < 32) stores[ns++] = key;
        } else if (opc > 0) {
            for (int i = 0; i < ns; i++)
                if (stores[i] == key) return 1;
        }
    }
    return 0;
}

// flag-liveness class of a guest x86 insn (for the dead-flag-save elision proof):
//   2 = full NZCV producer (writes all flags, reads none): add/or/and/sub/xor/cmp/test/neg
//   1 = flag CONSUMER (reads flags): adc/sbb/jcc/setcc/cmovcc
//   0 = flag-transparent and reads no flags: mov/lea/push/pop/movzx/movsx/inc/dec/not/nop
//  -1 = anything else -> treat conservatively as flag-live (keeps the save). Default-unknown is SAFE.
static int x86_flag_class(struct insn *I) {
    uint8_t op = I->op;
    if (I->two) {
        if ((op & 0xF0) == 0x80) return 1;                                  // jcc rel32
        if ((op & 0xF0) == 0x90) return 1;                                  // setcc
        if ((op & 0xF0) == 0x40) return 1;                                  // cmovcc
        if (op == 0xB6 || op == 0xB7 || op == 0xBE || op == 0xBF) return 0; // movzx/movsx
        return -1;                                                          // unknown 2-byte
    }
    if (op <= 0x3D && (op & 7) <= 5) { // primary ALU 00..3D
        int k = (op >> 3) & 7;
        return (k == 2 || k == 3) ? 1 : 2; // adc/sbb read CF; rest full producers
    }
    if (op == 0x80 || op == 0x81 || op == 0x83) { // group1 ALU r/m,imm
        int k = I->reg & 7;
        return (k == 2 || k == 3) ? 1 : 2;
    }
    if (op == 0x84 || op == 0x85 || op == 0xA8 || op == 0xA9) return 2; // test
    if (op == 0xF6 || op == 0xF7) {                                     // group3
        int k = I->reg & 7;
        if (k == 0 || k == 1 || k == 3) return 2; // test imm / neg
        if (k == 2) return 0;                     // not (no flags)
        return -1;                                // mul/imul/div/idiv
    }
    if (op == 0xFE) {
        int k = I->reg & 7;
        return (k == 0 || k == 1) ? 0 : -1;
    } // inc/dec byte
    if (op == 0xFF) {
        int k = I->reg & 7;
        return (k == 0 || k == 1) ? 0 : -1;
    } // inc/dec (call/jmp/push -> -1)
    if ((op >= 0x88 && op <= 0x8B) || op == 0x8D) return 0;               // mov r/m,r ; r,r/m ; lea
    if (op >= 0xA0 && op <= 0xA3) return 0;                               // mov acc <-> moffs (touches no flags)
    if ((op >= 0xB0 && op <= 0xBF) || op == 0xC6 || op == 0xC7) return 0; // mov imm
    if ((op >= 0x50 && op <= 0x5F) || op == 0x68 || op == 0x6A) return 0; // push/pop
    if (op == 0x90) return 0;                                             // nop
    return -1;                                                            // conservative
}

// Decode the self-loop body from `start` and decide whether the guest's NZCV is DEAD at loop top, i.e.
// the first flag-touching insn is a full producer (so on the back-edge the loop overwrites flags before
// any read). Returns 1 -> the per-iteration NZCV save can be elided onto the loop-exit edge. Returns 0
// (keep the save) on any consumer-before-producer or any unrecognized/uncertain insn. Conservative + safe.
static int loop_flags_dead(uint64_t start) {
    uint64_t gpc = start;
    int produced = 0;
    for (int n = 0; n < 256; n++) {
        struct insn I;
        decode(gpc, &I);
        if (I.len == 0) return 0;
        uint8_t op = I.op;
        // terminator of a self-loop block: the loop jcc (reads flags produced THIS iter iff produced)
        if ((!I.two && op >= 0x70 && op <= 0x7F) || (I.two && (op & 0xF0) == 0x80)) return produced;
        // any other control-flow ends the scan too
        if (!I.two && (op == 0xE9 || op == 0xEB || op == 0xE8 || op == 0xC3 || op == 0xC2 || op == 0xE3 ||
                       (op == 0xFF && (I.reg & 7) >= 2)))
            return produced;
        int cl = x86_flag_class(&I);
        if (cl == 1) {
            if (!produced) return 0; // consumer reads flags from a prior iteration -> live at top
        } else if (cl == 2) {
            produced = 1;
        } else if (cl != 0) {
            return 0; // unknown -> conservative
        }
        gpc += I.len;
    }
    return 0;
}

// ==================== x86-xflags: cross-block dead-flag elimination ====================
// Extends the intra-block dead-flag elision (opt3 NZCV, PF/AF) ACROSS direct block edges. The
// per-instruction elisions stop at block boundaries today: every block exit materializes the full
// flag state because the successor's needs are unknown. This pass computes, at translate time, a
// conservative FLAG LIVE-IN set for a direct successor by scanning ITS OWN GUEST BYTES (depth-1:
// anything past the successor's first control transfer is unknown -> live), and elides the
// materialization on edges whose successor provably overwrites the flag before any read.
//
// Soundness rules (each is load-bearing):
//  * The summary is derived from GUEST BYTES only -- never from another block's translation -- so
//    translation order, tier-2 retranslation and chain re-patching cannot make it stale. Guest code
//    MODIFICATION invalidates it exactly as it invalidates stitched-in successor bytes: smc_on_write
//    drops the whole g_map, so every block that baked an assumption is retranslated from the new
//    bytes before it can be re-entered through the map (baked `b body` chains into orphaned old code
//    share the pre-existing stitch/chain staleness window, no wider).
//  * The live ARM NZCV stays CANONICAL (borrow convention) at every block boundary even when the
//    membank store is elided: FL_SUB's live flags already are canonical, FL_ADD/FL_LOGIC get the
//    msr-only fixup (e_nzcv_fix_ci/_c1). Every exit to the dispatcher spills the LIVE flags
//    (emit_spill -> e_nzcv_save), so the successor's irq-poll exit -- the only place an ASYNC
//    signal can observe RFLAGS -- persists the true flag bytes before maybe_deliver_signal reads
//    cpu->nzcv. The membank may go stale only ACROSS a chained edge whose successor provably
//    overwrites CF/ZF/SF/OF before any read (same observable class as the existing intra-block
//    deferral between a producer and its consumer).
//  * The scan never dereferences a byte outside (a) the 4KB page(s) of the branch instruction doing
//    the asking (bytes the translator is already decoding) or (b) pages with a live translation
//    (txpg_has -> mapped, executable guest code). A speculative jcc target on a never-mapped page is
//    simply "all flags live".
//  * Classification is a sound under-approximation of WRITES and over-approximation of READS with
//    respect to dd's OWN emitters (not just the x86 architecture): e.g. rotates and by-CL shifts
//    load-modify-store cpu->nzcv (membank readers!) -> unknown; inc/dec merge the stored C (which
//    they can never kill, so a live CF always blocks the elision before it matters); imm shifts
//    store a full fresh NZCV word + PF but never AF.
// Gate: NOXBLOCKFLAGS=1 disables ONLY this cross-block pass; it is also off under the pre-existing
// broader switches NOFLAGELIDE (all flag-save elision) and NOLAZY (whole lazy-flag model).
#define XF_CF 0x01
#define XF_PF 0x02
#define XF_AF 0x04
#define XF_ZF 0x08
#define XF_SF 0x10
#define XF_OF 0x20
#define XF_ALL 0x3F
#define XF_NZCV (XF_CF | XF_ZF | XF_SF | XF_OF)

static int xblkflags_on(void) {
    static int v = -1;
    if (v < 0) {
        const char *s = getenv("NOXBLOCKFLAGS");
        v = (lazyflags_on() && !(s && *s && *s != '0') && !getenv("NOFLAGELIDE")) ? 1 : 0;
    }
    return v;
}

// x86-xflags kill-switch (A/B): NOXALUFLAGELIDE=1 disables the block-scan mid-block NZCV / PF-AF dead-flag
// elision on ALU producers (falls back to the 1-insn insn_is_flagkill / insn_kills_pfaf peepholes).
// Piggybacks on the same xblkflags_on() master switch (NOXBLOCKFLAGS/NOFLAGELIDE turn it off too).
static int xblkalu_elide_on(void) {
    static int v = -1;
    if (v < 0) {
        const char *s = getenv("NOXALUFLAGELIDE");
        v = (xblkflags_on() && !(s && *s && *s != '0')) ? 1 : 0;
    }
    return v;
}

// x86 condition code (opcode low nibble) -> the RFLAGS bits the condition READS.
static const uint8_t xf_cond_rd[16] = {
    XF_OF,
    XF_OF, // o / no
    XF_CF,
    XF_CF, // b / ae
    XF_ZF,
    XF_ZF, // e / ne
    XF_CF | XF_ZF,
    XF_CF | XF_ZF, // be / a
    XF_SF,
    XF_SF, // s / ns
    XF_PF,
    XF_PF, // p / np
    XF_SF | XF_OF,
    XF_SF | XF_OF, // l / ge
    XF_ZF | XF_SF | XF_OF,
    XF_ZF | XF_SF | XF_OF, // le / g
};

// Per-insn flag read/write classification for the liveness scan. *rd = flags possibly read,
// *wr = flags DEFINITELY overwritten (with a fresh, input-independent storage write by dd's
// emitter). Returns the scan step kind.
enum { XRW_OK, XRW_JMP, XRW_END, XRW_UNK };

static int x86_flag_rw(const struct insn *I, int *rd, int *wr) {
    uint8_t op = I->op;
    *rd = 0;
    *wr = 0;
    if (I->vex || I->map3) return XRW_UNK; // AVX / 0F38-3A: C-emulated, may touch flags -> unknown
    if (I->two) {
        if ((op & 0xF0) == 0x80) {
            *rd = xf_cond_rd[op & 0xF];
            return XRW_END;
        } // jcc rel32
        if ((op & 0xF0) == 0x90) {
            *rd = xf_cond_rd[op & 0xF];
            return XRW_OK;
        } // setcc
        if ((op & 0xF0) == 0x40) {
            *rd = xf_cond_rd[op & 0xF];
            return XRW_OK;
        } // cmovcc
        if (op == 0xB6 || op == 0xB7 || op == 0xBE || op == 0xBF) return XRW_OK; // movzx/movsx
        if (op == 0x1E || op == 0x1F) return XRW_OK; // endbr64 / long-nop (reserved hint-nop space)
        if (op == 0xAF) {
            *wr = XF_NZCV;
            return XRW_OK;
        } // imul r,r/m: e_mul_set_oc builds a fresh
          // full NZCV word; PF/AF untouched
        return XRW_UNK;
    }
    if (op < 0x40 && alu_kind_primary(op) >= 0) { // primary ALU add/or/adc/sbb/and/sub/xor/cmp
        int k = alu_kind_primary(op);
        if (k == 2 || k == 3) *rd = XF_CF; // adc/sbb read CF
        *wr = XF_ALL;
        return XRW_OK;
    }
    if (op == 0x80 || op == 0x81 || op == 0x83) { // group1 ALU r/m, imm
        int k = I->reg & 7;
        if (k == 2 || k == 3) *rd = XF_CF;
        *wr = XF_ALL;
        return XRW_OK;
    }
    if (op == 0x84 || op == 0x85 || op == 0xA8 || op == 0xA9) {
        *wr = XF_ALL;
        return XRW_OK;
    } // test
    if (op == 0xF6 || op == 0xF7) { // group3
        int k = I->reg & 7;
        if (k == 0 || k == 3) {
            *wr = XF_ALL;
            return XRW_OK;
        } // test imm / neg
        if (k == 2) return XRW_OK; // not: no flags
        return XRW_UNK;            // mul/imul/div/idiv (div block-exits)
    }
    if (op == 0xFE || op == 0xFF) { // group4/5
        int k = I->reg & 7;
        if (k == 0 || k == 1) {
            *wr = XF_PF | XF_AF | XF_ZF | XF_SF | XF_OF;
            return XRW_OK;
        } // inc/dec keep CF
        if (op == 0xFF && k == 6) return XRW_OK;              // push r/m
        if (op == 0xFF && (k == 2 || k == 4)) return XRW_END; // call/jmp indirect
        return XRW_UNK;
    }
    if ((op >= 0x88 && op <= 0x8B) || op == 0x8D || op == 0x63) return XRW_OK; // mov / lea / movsxd
    if (op >= 0xA0 && op <= 0xA3) return XRW_OK;                               // mov acc <-> moffs (no flags)
    if (op >= 0xB0 && op <= 0xBF) return XRW_OK;                               // mov r, imm
    if ((op == 0xC6 || op == 0xC7) && (I->reg & 7) == 0) return XRW_OK;        // mov r/m, imm (/0 only)
    if ((op >= 0x50 && op <= 0x5F) || op == 0x68 || op == 0x6A) return XRW_OK; // push/pop/push imm
    if (op == 0x8F && (I->reg & 7) == 0) return XRW_OK;                        // pop r/m
    if (op >= 0x90 && op <= 0x97) return XRW_OK;                               // nop / xchg acc
    if (op == 0x86 || op == 0x87) return XRW_OK;                               // xchg r/m
    if (op == 0x98 || op == 0x99) return XRW_OK;                               // cwde/cdq
    if (op == 0x9E) {
        *wr = XF_CF | XF_PF | XF_AF | XF_ZF | XF_SF;
        return XRW_OK;
    } // sahf (not OF)
    if (op == 0x9F) {
        *rd = XF_CF | XF_PF | XF_AF | XF_ZF | XF_SF;
        return XRW_OK;
    } // lahf
    if (op == 0x9C) {
        *rd = XF_ALL;
        return XRW_OK;
    } // pushfq
    if (op == 0x9D) {
        *wr = XF_ALL;
        return XRW_OK;
    } // popfq rewrites all lanes
    if (op == 0xF5) {
        *rd = XF_CF;
        *wr = XF_CF;
        return XRW_OK;
    } // cmc
    if (op == 0xF8 || op == 0xF9) {
        *wr = XF_CF;
        return XRW_OK;
    } // clc/stc
    if (op == 0xFC || op == 0xFD) return XRW_OK;                // cld/std (DF only)
    if (op == 0xC0 || op == 0xC1 || op == 0xD0 || op == 0xD1) { // group2, constant count
        int k = I->reg & 7;
        if (k == 6) k = 4;
        if (k == 4 || k == 5 || k == 7) { // SHL/SHR/SAR: fresh full NZCV word + PF; AF untouched
            int w = (op & 1) ? I->opsize : 1;
            int cnt = (op == 0xD0 || op == 0xD1) ? 1 : (int)(I->imm & (w == 8 ? 63 : 31));
            if (cnt == 0) return XRW_OK; // count 0: no flag change at all
            *wr = XF_NZCV | XF_PF;
            return XRW_OK;
        }
        return XRW_UNK; // rol/ror/rcl/rcr: load-modify-store cpu->nzcv (membank readers)
    }
    if (op >= 0x70 && op <= 0x7F) {
        *rd = xf_cond_rd[op & 0xF];
        return XRW_END;
    } // jcc rel8
    if (op == 0xE9 || op == 0xEB) return XRW_JMP;               // jmp rel: follow
    if (op == 0xE8 || op == 0xC3 || op == 0xC2) return XRW_END; // call / ret
    if (op == 0xE0 || op == 0xE1) {
        *rd = XF_ZF;
        return XRW_END;
    } // loope/loopne
    if (op == 0xE2 || op == 0xE3) return XRW_END; // loop / jrcxz
    return XRW_UNK;
}

// Is it safe to READ guest bytes [a, a+15] at translate time? Only when every touched 4KB page is
// the anchor's own page (the branch insn being translated -- provably mapped) or a page some live
// translation was decoded from (txpg_has). Never fault on a speculative target.
static int xf_scan_ok(uint64_t a, uint64_t anchor) {
    uint64_t apg = anchor & ~0xFFFull;
    uint64_t pg = a & ~0xFFFull, pg2 = (a + 15) & ~0xFFFull;
    if (pg != apg && !txpg_has(a)) return 0;
    if (pg2 != pg && pg2 != apg && !txpg_has(a + 15)) return 0;
    return 1;
}

// Flag live-in mask at guest pc: the {CF,PF,AF,ZF,SF,OF} that MAY be read before being definitely
// overwritten, scanning straight-line successor code (following unconditional jmps) up to a budget.
// Anything unproven is live: unknown insn, control transfer (depth-1), undecodable byte, unsafe page,
// budget exhaustion.
#define XSCAN_INSNS 32

static int x86_flags_livein(uint64_t pc, uint64_t anchor) {
    int killed = 0, live = 0;
    g_prof_xflag_scan++;
    for (int n = 0; n < XSCAN_INSNS; n++) {
        if (!xf_scan_ok(pc, anchor)) break;
        struct insn I;
        decode(pc, &I);
        if (I.len <= 0) break;
        int rd, wr;
        int kind = x86_flag_rw(&I, &rd, &wr);
        if (kind == XRW_UNK) break;
        live |= rd & ~killed;
        if (kind == XRW_END) return live | (XF_ALL & ~killed); // successors unknown -> rest live
        if (kind == XRW_JMP) {
            pc = pc + (uint64_t)I.len + (uint64_t)I.imm;
            continue;
        }
        killed |= wr;
        if ((killed | live) == XF_ALL) return live; // every flag decided
        pc += (uint64_t)I.len;
    }
    return live | (XF_ALL & ~killed);
}

// Deferred-flag emission for a DIRECT unconditional edge to `target` (jmp rel / call rel):
// materialize the pending producer unless the successor provably overwrites CF/ZF/SF/OF first.
//  - FL_SUB: the live NZCV is already canonical -> elide = emit NOTHING.
//  - FL_ADD/FL_LOGIC: the msr fixup must still run (live NZCV must be canonical at every boundary:
//    x86cc_to_arm consumers and every exit's LIVE-flag spill assume it) -> elide only the str.
static void flags_edge(uint64_t target, uint64_t anchor) {
    if (g_fl_pending == FL_NONE) return;
    if (xblkflags_on() && !(x86_flags_livein(target, anchor) & XF_NZCV)) {
        if (g_fl_pending == FL_ADD)
            e_nzcv_fix_ci();
        else if (g_fl_pending == FL_LOGIC)
            e_nzcv_fix_c1();
        // FL_SUB: nothing -- live NZCV already holds the canonical borrow-convention flags
        g_fl_pending = FL_NONE;
        g_prof_xflag++;
        return;
    }
    flags_materialize();
}

// Flag emission for a block-ending Jcc with edges (taken, fall) -- generalizes the tier-2 self-loop
// elide (emit_selfloop_x86, FL_SUB save-on-exit-edge-only) to every conditional edge pair:
//  - FL_SUB: branch off the live NZCV (already canonical); the producer's spill (e_nzcv_save == its
//    exact finalizer bytes) is pushed onto only the edge(s) whose successor may read cpu->nzcv --
//    reported via *save_taken/*save_fall, emitted by the caller inside the per-edge stubs. When the
//    fall side is being stitched inline (stitch_fall), g_fl_pending is KEPT: the continuation is the
//    same host block and its own consumers handle the deferral exactly as intra-block code.
//  - FL_ADD/FL_LOGIC: the msr fixup must precede the b.cond either way; only the membank str can be
//    elided, and only when BOTH edges are provably dead. Otherwise materialize (pre-change bytes).
//  - FL_NONE: reload the canonical membank flags (unchanged consumer path).
static void jcc_edge_flags(uint64_t taken, uint64_t fall, uint64_t anchor, int stitch_fall, int *save_taken,
                           int *save_fall) {
    *save_taken = 0;
    *save_fall = 0;
    if (!g_fl_pending) {
        e_nzcv_load();
        return;
    }
    if (!xblkflags_on()) {
        flags_materialize();
        return;
    }
    if (g_fl_pending == FL_SUB) {
        *save_taken = (x86_flags_livein(taken, anchor) & XF_NZCV) != 0;
        if (!*save_taken) g_prof_xflag++;
        if (stitch_fall) return; // keep g_fl_pending live for the inline continuation
        *save_fall = (x86_flags_livein(fall, anchor) & XF_NZCV) != 0;
        if (!*save_fall) g_prof_xflag++;
        g_fl_pending = FL_NONE;
        return;
    }
    if (!(x86_flags_livein(taken, anchor) & XF_NZCV) && !(x86_flags_livein(fall, anchor) & XF_NZCV)) {
        if (g_fl_pending == FL_ADD)
            e_nzcv_fix_ci();
        else
            e_nzcv_fix_c1();
        g_fl_pending = FL_NONE;
        g_prof_xflag += 2;
        return;
    }
    flags_materialize();
}

// cross-block PF/AF: NI (the insn after a PF/AF producer) is a direct control transfer -> the
// producer's PF/AF are dead iff provably overwritten-before-read at EVERY successor entry. jp/jnp
// read PF themselves -> never dead. Indirect branches / ret / syscall -> unknown -> live.
static int pfaf_dead_thru(const struct insn *NI, uint64_t ni_pc, uint64_t anchor) {
    uint64_t n2 = ni_pc + (uint64_t)NI->len;
    uint8_t op = NI->op;
    if (!NI->two && (op == 0xE9 || op == 0xEB || op == 0xE8))
        return !(x86_flags_livein(n2 + (uint64_t)NI->imm, anchor) & (XF_PF | XF_AF));
    int lo = -1;
    if (!NI->two && op >= 0x70 && op <= 0x7F) lo = op & 0xF;
    if (NI->two && (op & 0xF0) == 0x80) lo = op & 0xF;
    if (lo >= 0 && lo != 0xA && lo != 0xB) // any jcc except jp/jnp (those READ PF)
        return !(x86_flags_livein(n2 + (uint64_t)NI->imm, anchor) & (XF_PF | XF_AF)) &&
               !(x86_flags_livein(n2, anchor) & (XF_PF | XF_AF));
    return 0;
}

// Emit a single-block self-loop terminating jcc (taken target == block start). `cc` is the ARM cond.
// TIER-1 (with counter): flag handling byte-identical to the baseline jcc handler (opt3 lazy flags:
//   if a producer's flags are deferred, flags_materialize() spills them to cpu->nzcv AND leaves the
//   live ARM NZCV canonical; otherwise e_nzcv_load() reloads cpu->nzcv) -- only the back-edge differs:
//   it routes through emit_t2_counter_x86 (which promotes when hot) instead of a plain `b body`.
// TIER-2 (g_tier2_build): FOLD the trampoline to a single `b.cond body`; additionally, when the deferred
//   flags are a *sub/cmp* producer (g_fl_pending == FL_SUB) and provably dead at loop top, ELIDE the
//   per-iteration `mrs;str` save onto the loop-exit (fall) edge only. The elision is restricted to
//   FL_SUB because its finalizer (e_nzcv_save) leaves the live ARM NZCV already in the canonical borrow
//   convention x86cc_to_arm() assumes -- so the back-edge branch can read the live NZCV directly and the
//   save is pure spill-for-successor. FL_ADD/FL_LOGIC finalizers msr a *corrected* value into the live
//   NZCV, so they MUST materialize before the branch (fold-only). Bit-identical guest-visible control
//   flow + cpu->nzcv in every case.
static void emit_selfloop_x86(int cc, uint64_t start, uint64_t fall, void *body, int slot) {
    if (g_tier2_build) {
        int dead = (g_fl_pending == FL_SUB) && !getenv("NOFLAGELIDE") && loop_flags_dead(start);
        if (g_fl_pending == FL_SUB) {
            if (dead) {
                // FOLD + ELIDE: branch off the live NZCV (still holds the subs result); save only on the
                // loop-exit (fall) path. e_nzcv_save here == the FL_SUB finalizer flags_materialize would
                // emit, so the exit successor reads byte-identical cpu->nzcv.
                uint32_t *patch = (uint32_t *)g_cp;
                emit32(0);     // b.cond -> body (filled below)
                e_nzcv_save(); // loop-exit: materialize for the successor block's prologue
                g_fl_pending = FL_NONE;
                emit_chain_exit(fall);
                int64_t d = ((uint8_t *)body - (uint8_t *)patch) / 4;
                *patch = 0x54000000u | (((uint32_t)d & 0x7FFFF) << 5) | (cc & 0xF);
                g_prof_t2fold++;
                return;
            }
            flags_materialize(); // FOLD only: spill before the branch (FL_SUB -> e_nzcv_save, == tier-1)
        } else if (g_fl_pending) {
            flags_materialize(); // FL_ADD/FL_LOGIC: materialize (msr's corrected NZCV) before the branch
        } else {
            e_nzcv_load();
        }
        uint32_t *patch = (uint32_t *)g_cp;
        emit32(0); // b.cond -> body
        emit_chain_exit(fall);
        int64_t d = ((uint8_t *)body - (uint8_t *)patch) / 4;
        *patch = 0x54000000u | (((uint32_t)d & 0x7FFFF) << 5) | (cc & 0xF);
        return;
    }
    // TIER-1: flag handling byte-identical to baseline; only the back-edge differs (counter -> b body).
    if (g_fl_pending)
        flags_materialize();
    else
        e_nzcv_load();
    uint32_t *patch = (uint32_t *)g_cp;
    emit32(0);             // b.cond -> Lcnt (counter)
    emit_chain_exit(fall); // fall = loop exit
    int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
    *patch = 0x54000000u | (((uint32_t)d & 0x7FFFF) << 5) | (cc & 0xF);
    emit_t2_counter_x86(slot, start, body);
}
