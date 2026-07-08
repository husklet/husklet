// ---------------- opt5: rep movs/stos idiom upgrade ----------------
// Generalizes the LSE idiom-upgrade lever to the x86 string ops: a `rep movs`/`rep stos`
// (the idiomatic memcpy/memset of every musl/glibc x86 guest) is lowered to ONE optimized
// host libc call instead of the per-element `ldr;str;sub;cbnz` host loop. Bit-exact with
// that scalar loop for all lengths (incl. 0), alignments, and the forward-overlap smear.
// Kill-switch: NOREP=1 (or any non-"0" value) -> fall back to the original element loop.
static int norep_disabled(void) {
    static int v = -1; // env read once, then cached
    if (v < 0) {
        const char *s = getenv("NOREP");
        v = (s && *s && *s != '0') ? 1 : 0;
    }
    return v;
}

// Host helper for `rep movs`: copy `nbytes` forward, x86 element-by-element semantics.
// rep movs always copies LOW->HIGH; a plain memcpy/memmove is correct only when the
// regions are disjoint or dst precedes src. When src < dst < src+nbytes the forward copy
// SMEARS (each element is re-read after a previous element overwrote it) -- memmove would
// be WRONG here -- so we replay it element-by-element at the guest element width `w`,
// which reproduces the scalar loop's bytes exactly (byte-wise smear differs from a
// w>1 element smear at sub-element overlap offsets).
// W6A item 1 (non-PIE): a biased ET_EXEC's guest pointer may still carry its low link address (e.g. a
// rip-relative lea into the type/rodata section); rebase it to the real high mapping so these bulk C string
// helpers touch the mapped bytes (the single-access fault path nonpie_fixup cannot serve a libc memcpy).
// Inert for PIE/static-PIE (g_nonpie_lo == 0).
static inline uint64_t repstr_g2h(uint64_t a) {
    return (g_nonpie_lo && a >= g_nonpie_lo && a < g_nonpie_hi) ? a + g_nonpie_bias : a;
}

static void dd_rep_movs(uint8_t *dst, const uint8_t *src, uint64_t nbytes, int w, int df) {
    g_repmovs_n++;
    dst = (uint8_t *)repstr_g2h((uint64_t)dst);
    src = (const uint8_t *)repstr_g2h((uint64_t)src);
    if (nbytes == 0) return;
    if (df) { // DF=1 backward: dst/src point at the HIGHEST element; copy high->low, element-granular (the
        // x86 `std; rep movs` used by memmove for dst>src overlap). Element-by-element replays the scalar
        // loop's exact bytes for every overlap/width; byte-identical to the -w element loop below.
        uint64_t n = nbytes / (unsigned)w;
        for (uint64_t i = 0; i < n; i++) {
            uint64_t o = i * (uint64_t)w;
            memcpy(dst - o, src - o, (size_t)w); // one whole w-wide element per step
        }
        return;
    }
    if (dst <= src || dst >= src + nbytes) { // disjoint, or forward-safe (dst before src)
        memcpy(dst, src, nbytes);
        return;
    }
    switch (w) { // forward-overlap smear, element-granular (matches per-element rep movs)
    case 2: {
        uint16_t *d = (uint16_t *)dst;
        const uint16_t *s = (const uint16_t *)src;
        for (uint64_t i = 0, n = nbytes >> 1; i < n; i++)
            d[i] = s[i];
        return;
    }
    case 4: {
        uint32_t *d = (uint32_t *)dst;
        const uint32_t *s = (const uint32_t *)src;
        for (uint64_t i = 0, n = nbytes >> 2; i < n; i++)
            d[i] = s[i];
        return;
    }
    case 8: {
        uint64_t *d = (uint64_t *)dst;
        const uint64_t *s = (const uint64_t *)src;
        for (uint64_t i = 0, n = nbytes >> 3; i < n; i++)
            d[i] = s[i];
        return;
    }
    default:
        for (uint64_t i = 0; i < nbytes; i++)
            dst[i] = src[i];
        return;
    }
}

// Host helper for `rep stos`: fill `n` elements of width `w` with `val` (AL/AX/EAX/RAX).
static void dd_rep_stos(uint8_t *dst, uint64_t val, uint64_t n, int w, int df) {
    g_repstos_n++;
    dst = (uint8_t *)repstr_g2h((uint64_t)dst);
    if (df) { // DF=1 backward: dst points at the highest element; write val at dst, dst-w, dst-2w, ...
        uint8_t *p = dst;
        for (uint64_t i = 0; i < n; i++, p -= (unsigned)w)
            memcpy(p, &val, (size_t)w); // low w bytes of RAX, little-endian (== AL/AX/EAX/RAX)
        return;
    }
    switch (w) {
    case 2: {
        uint16_t *p = (uint16_t *)dst, v = (uint16_t)val;
        for (uint64_t i = 0; i < n; i++)
            p[i] = v;
        return;
    }
    case 4: {
        uint32_t *p = (uint32_t *)dst, v = (uint32_t)val;
        for (uint64_t i = 0; i < n; i++)
            p[i] = v;
        return;
    }
    case 8: {
        uint64_t *p = (uint64_t *)dst, v = val;
        for (uint64_t i = 0; i < n; i++)
            p[i] = v;
        return;
    }
    default: memset(dst, (int)(val & 0xff), n); return;
    }
}

// Reload guest state (flags + xmm0..15 + r0..15) from the membank into host regs, WITHOUT
// re-pinning x28 (== &cpu, callee-saved across the host call). Mirrors emit_prologue minus
// the `mov x28,x0`.
static void emit_reload(void) {
    e_nzcv_load();
    for (int t = 0; t < 16; t += 2)
        e_ldp_q(t, t + 1, 28, OFF_V + t * 16);
    for (int r = 1; r <= 15; r++)
        e_ldr(r, 28, R_OFF(r));
    e_ldr(0, 28, 0); // rax last
}

// Codegen for the idiom: spill guest state, marshal args, blr the host helper, then fix up
// RDI/RSI (+= count*w) and RCX (->0) in the membank snapshot, and reload. Guest GPRs live in
// host x0..x15 (caller-saved) so the spill/reload around the call is mandatory; x28 (cpu) is
// callee-saved and survives; the host SP is untouched (guest RSP is x4), so ABI alignment holds.
// df_static: the block-static DF (DF_FWD/DF_BWD/DF_DYN). The helper takes the runtime direction as its 5th
// arg (x4); on DF_DYN we load cpu->df and the pointer fixup negates the delta at runtime (backward => -nbytes).
static void emit_rep_string(int movs, int w, int shift, int df_static) {
    // emit_spill (below) clears cpu->vdirty and republishes cpu->V, and emit_reload restores host
    // v0..v15 FROM cpu->V, so this leaves cpu->V current -> no vdirty mark needed (a later syscall may slim).
    emit_spill();                          // x0..x15 + xmm0..15 + flags -> cpu (membank)
    e_ldr(0, 28, R_OFF(RDI));              // x0 = dst (rdi)
    e_ldr(1, 28, R_OFF(movs ? RSI : RAX)); // x1 = src (rsi) / fill value (rax)
    e_ldr(2, 28, R_OFF(RCX));              // x2 = element count (rcx)
    e_movconst(3, (uint64_t)w);            // x3 = element width
    if (df_static == DF_DYN)
        e_ldr(4, 28, OFF_DF); // x4 = cpu->df (0 fwd / 1 bwd) -- runtime direction
    else
        e_movconst(4, (uint64_t)(df_static == DF_BWD)); // x4 = statically-known direction
    if (movs) {
        if (shift) e_lsl_i(2, 2, shift, 1); // x2 = nbytes = count << shift
        emit_host_ptr(16, (uint64_t)(uintptr_t)&dd_rep_movs, PRELOC_HOSTGLOBAL);
    } else {
        emit_host_ptr(16, (uint64_t)(uintptr_t)&dd_rep_stos, PRELOC_HOSTGLOBAL);
    }
    emit32(0xD63F0000u | (16 << 5)); // blr x16
    // membank still holds the pre-call rcx/rdi/rsi (the helper takes its args by value):
    e_ldr(17, 28, R_OFF(RCX)); // x17 = original element count
    if (shift)
        e_lsl_i(16, 17, shift, 1); // x16 = nbytes = count << shift
    else
        e_mov_rr(16, 17, 1);
    // signed pointer delta: forward => +nbytes, backward => -nbytes.
    if (df_static == DF_BWD) {
        e_rrr(A_SUB, 16, 31, 16, 1, 0); // x16 = -nbytes
    } else if (df_static == DF_DYN) {
        e_ldr(20, 28, OFF_DF);               // x20 = cpu->df
        emit32(0x34000000u | (2 << 5) | 20); // cbz x20, .+8  (df==0 -> keep +nbytes)
        e_rrr(A_SUB, 16, 31, 16, 1, 0);      // df==1 -> x16 = -nbytes
    }
    e_ldr(19, 28, R_OFF(RDI));
    e_rrr(A_ADD, 19, 19, 16, 1, 0); // rdi += delta
    e_str(19, 28, R_OFF(RDI));
    if (movs) {
        e_ldr(19, 28, R_OFF(RSI));
        e_rrr(A_ADD, 19, 19, 16, 1, 0); // rsi += delta
        e_str(19, 28, R_OFF(RSI));
    }
    e_str(31, 28, R_OFF(RCX)); // rcx = 0 (str xzr); EFLAGS unchanged by movs/stos
    emit_reload();
    // the emit_spill above cleared cpu->vdirty at RUNTIME, so the once-per-trace mark latch must
    // reset -- a later xmm write in this same region has to re-mark or a following slim syscall exit would
    // skip the xmm save with host v0..v15 newer than cpu->V.
    g_vmark_done = 0;
}

// translate_block control code: a per-class translate helper cannot continue/break the caller's for(;;)
// translate loop, so it returns how the caller should steer it. (Shared by the instruction-class splits.)
enum { TX_FALL = 0, TX_NEXT = 1, TX_BREAK = 2 };

// ---- string ops dispatch: stos/movs/lods (AA/AB/A4/A5/AC/AD), cmps/scas (A6/A7/AE/AF), cld/std (FC/FD).
// Lifted VERBATIM out of translate_block's one-byte switch (behavior-preserving move). DF assumed 0 (fwd)
// for stos/movs/lods unless std set g_df. Returns TX_FALL if `op` is not a string op (caller falls through
// to the next handler), TX_NEXT (caller: `gpc = next; continue;`), or TX_BREAK (block ends; caller: break).
static int translate_string(struct insn *I, uint64_t next) {
    uint8_t op = I->op;
    if (op == 0xAA || op == 0xAB || op == 0xA4 || op == 0xA5 || op == 0xAC || op == 0xAD) {
        int w = (op & 1) ? I->opsize : 1;
        int movs = (op == 0xA4 || op == 0xA5), lods = (op == 0xAC || op == 0xAD);
        // opt5: `rep movs`/`rep stos` -> one optimized host memcpy/memset call (bit-exact with the scalar
        // loop below). The host helper is now direction-aware (takes cpu->df / the static DF as its 5th arg),
        // so this fast path serves BOTH forward and backward. Fall back to the scalar loop only for NOREP=1,
        // `lods` (result is RAX, not a bulk move), or a segment override / 32-bit address size (the scalar
        // loop ignores both too).
        if (I->rep && !lods && !I->seg && !I->addr32 && (w == 1 || w == 2 || w == 4 || w == 8) &&
            !norep_disabled()) {
            int shift = w == 1 ? 0 : w == 2 ? 1 : w == 4 ? 2 : 3;
            emit_rep_string(movs, w, shift, g_df);
            return TX_NEXT;
        }
        // Scalar element loop. DF stride: forward +w, backward -w. When DF is statically known (DF_FWD/DF_BWD)
        // use an immediate add/sub; when DF_DYN (block entry / after popfq) compute a runtime stride reg (x17)
        // from cpu->df so a cross-block `std`/popfq direction is honored.
        int dyn = (g_df == DF_DYN);
        if (dyn) {
            e_movconst(17, (uint64_t)w);         // x17 = +w
            e_ldr(16, 28, OFF_DF);               // x16 = cpu->df
            emit32(0xB4000000u | (2 << 5) | 16); // cbz x16, .+8   (df==0 -> keep +w)
            e_rrr(A_SUB, 17, 31, 17, 1, 0);      // df==1 -> x17 = -w
        }
#define REP_STEP(reg)                                                                                                  \
    do {                                                                                                               \
        if (dyn)                                                                                                       \
            e_rrr(A_ADD, (reg), (reg), 17, 1, 0);                                                                      \
        else if (g_df == DF_BWD)                                                                                       \
            e_subi((reg), (reg), w, 1);                                                                                \
        else                                                                                                          \
            e_addi((reg), (reg), w, 1);                                                                                \
    } while (0)
        uint32_t *cbz = NULL, *top = NULL;
        if (I->rep) {
            top = (uint32_t *)g_cp;
            cbz = (uint32_t *)g_cp;
            emit32(0);
        } // cbz RCX,done
        if (movs) {
            e_load(w, 16, RSI);
            e_store(w, 16, RDI);
            REP_STEP(RSI);
            REP_STEP(RDI);
        } else if (lods) {
            e_load(w, RAX, RSI);
            REP_STEP(RSI);
        } else {
            e_store(w, RAX, RDI);
            REP_STEP(RDI);
        } // stos
#undef REP_STEP
        if (I->rep) {
            e_subi(RCX, RCX, 1, 1);
            int64_t back = (int64_t)(top - (uint32_t *)g_cp);
            emit32(0x14000000u | ((uint32_t)back & 0x3FFFFFFu)); // b top
            int64_t d = ((uint32_t *)g_cp - cbz);
            *cbz = 0xB4000000u | (((uint32_t)d & 0x7FFFF) << 5) | RCX; // cbz x_rcx,done
        }
        return TX_NEXT;
    }
    // cmps (A6/A7) / scas (AE/AF): the whole (possibly REP/REPE/REPNE) compare+scan is done in ONE C round-
    // trip (like cpuid/div): bit-exact RCX/RSI/RDI + ZF/SF/CF/OF end-state, fast host memcmp/memchr inside on
    // the forward path (gate NOREPCMP for the naive per-element oracle loop; DF=1 uses that loop with a
    // decrementing stride). Descriptor (width | isscas | isrepne | isrep | df) -> cpu->divop.
    if (op == 0xA6 || op == 0xA7 || op == 0xAE || op == 0xAF) {
        int w = (op & 1) ? I->opsize : 1;
        int isscas = (op == 0xAE || op == 0xAF);
        int isrep = (I->rep || I->repne);
        uint64_t desc = (uint64_t)w | ((uint64_t)isscas << 8) | ((uint64_t)(I->repne ? 1 : 0) << 9) |
                        ((uint64_t)isrep << 10) | ((uint64_t)(g_df == DF_BWD ? 1 : 0) << 11);
        e_movconst(16, desc);
        if (g_df == DF_DYN) {              // DF unknown statically -> OR in the runtime direction bit
            e_ldr(18, 28, OFF_DF);         // x18 = cpu->df (0/1)
            e_rrr(A_ORR, 16, 16, 18, 1, 11); // desc |= df << 11
        }
        e_str(16, 28, OFF_DIVOP);
        emit_exit_const(next, R_REPSTR); // spills regs+flags; do_repstr() resumes at `next`
        return TX_BREAK;                 // block ends here (helper runs, dispatcher continues)
    }
    if (op == 0xFC || op == 0xFD) { // cld (FC) / std (FD): update BOTH the runtime bit and the static shadow
        e_movconst(16, (uint64_t)(op == 0xFD));
        e_str(16, 28, OFF_DF);            // cpu->df = 0 (fwd) / 1 (bwd) -- persists across blocks
        g_df = (op == 0xFD) ? DF_BWD : DF_FWD; // statically known for the rest of THIS block (fast stride)
        return TX_NEXT;
    }
    return TX_FALL;
}
