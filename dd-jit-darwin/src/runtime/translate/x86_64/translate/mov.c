// dd/runtime/translate/x86_64 -- data-move instruction class, lifted VERBATIM out of translate_block's
// one-byte switch (behavior-preserving move): mov r,imm (B0-BF), mov r/m,imm (C6/C7), mov r/m<->r
// (88-8B), lea (8D), push/pop r (50-5F), movsxd/move (0x63). None of these touch the modeled EFLAGS, so
// there is no lazy-flag interaction. Every handler advances past the insn -> the helper returns TX_FALL
// (not a move -> caller falls through) or TX_NEXT (caller: gpc = next; continue). #included after the
// other class files; uses EA helpers from decode.c + e_* from emit.c + byte_val/byte_wb (all defined above).
static int translate_mov(struct insn *I, uint64_t next) {
    uint8_t op = I->op;
    int sf = I->opsize == 8;
    // ---- mov r8, imm8 (B0+r) ----
    if (op >= 0xB0 && op <= 0xB7) {
        int rnum = (op - 0xB0) | (I->rexB << 3);
        e_movz(16, (uint32_t)(I->imm & 0xff), 0);
        byte_wb(I, rnum, 16);
        return TX_NEXT;
    }
    // ---- mov r, imm (B8+r) ----
    if (op >= 0xB8 && op <= 0xBF) {
        int rd = (op - 0xB8) | (I->rexB << 3);
        uint64_t v = sf ? (uint64_t)I->imm : (uint64_t)(uint32_t)I->imm;
        // bias the ONE baked immediate that materializes V8's embedded-builtins code base
        // (v8_Default_embedded_blob_code_) to the high mapping so V8's InnerPointerToCodeCache range
        // matches where the builtins actually execute (return addresses stay HIGH). Exact-value match
        // on the symbol recorded at load -> inert for every other constant / PIE / non-V8 image.
        if (g_nonpie_blob_code && v == g_nonpie_blob_code) v += g_nonpie_bias;
        e_movconst(rd, v);
        return TX_NEXT;
    }
    // ---- mov r/m, imm (C7 /0, C6 /0) ----
    if (op == 0xC7 || op == 0xC6) {
        int w = op == 0xC6 ? 1 : I->opsize;
        if (I->is_mem) {
            emit_ea(I, next);
            e_movconst(16, (uint64_t)I->imm);
            e_store(w, 16, 17);
        } else {
            uint64_t v = sf ? (uint64_t)I->imm : (uint64_t)(uint32_t)I->imm;
            // the C7 /0 `mov r,imm` form of the V8 embedded-blob code-base load (see the B8-BF
            // note above). Same exact-value rebase; C6 is byte-imm (never a code address) so op==0xC7.
            if (op == 0xC7 && g_nonpie_blob_code && v == g_nonpie_blob_code) v += g_nonpie_bias;
            e_movconst(I->rm_reg, v);
        }
        return TX_NEXT;
    }
    // ---- mov accumulator <-> moffs (A0-A3): the only x86 form with a full direct-ADDRESS immediate ----
    // A0: mov AL,moffs8 ; A1: mov eAX/rAX,moffs ; A2: mov moffs8,AL ; A3: mov moffs,eAX/rAX. moffs is
    // an absolute offset (64-bit; 32-bit zero-extended under a 0x67 addr-size prefix), NOT a ModRM
    // operand. Present it to emit_ea as an absolute-[disp] memory operand so the segment base
    // (fs/gs) and the non-PIE bias-fold are applied exactly as for any other guest memory access.
    // The accumulator is register 0 (RAX); byte forms are AL, opsize picks AX/EAX/RAX.
    if (op == 0xA0 || op == 0xA1 || op == 0xA2 || op == 0xA3) {
        int w = (op & 1) ? I->opsize : 1; // A0/A2 -> byte; A1/A3 -> opsize (2/4/8)
        int load = op < 0xA2;             // A0/A1 load acc<-[moffs]; A2/A3 store [moffs]<-acc
        I->is_mem = 1;                    // route the moffs through the standard EA path
        I->m_hasbase = I->m_hasindex = I->rip_rel = 0;
        I->disp = I->imm; // absolute address (already addr-size sized by the decoder)
        emit_ea(I, next); // x17 = host address (seg base + non-PIE bias applied)
        if (load) {
            if (w == 1) {
                e_load(1, 16, 17);
                byte_wb(I, 0, 16); // AL (preserve bits 63:8)
            } else if (w == 2) {
                e_load(2, 16, 17);
                e_bfi(0, 16, 0, 16, 1); // AX (preserve bits 63:16)
            } else
                e_load(w, 0, 17); // EAX zero-extends to RAX (w==4); RAX (w==8)
        } else {
            int sv = (w == 1) ? byte_val(I, 0, 16) : 0; // byte src = AL; else RAX
            e_store(w, sv, 17);
        }
        return TX_NEXT;
    }
    // ---- mov r/m,r (88/89) and r,r/m (8A/8B) ----
    if (op == 0x88 || op == 0x89 || op == 0x8A || op == 0x8B) {
        int w = (op & 1) ? I->opsize : 1;
        int to_reg = (op & 2); // 8A/8B: dest is reg
        if (I->is_mem) {
            if (to_reg) {     // mov reg, [mem]  -- folded into one ldr when [base+disp]
                if (w == 1) { // byte dest: ah/bh/ch/dh -> bits 8-15; lo8 preserves upper
                    emit_load_mem(I, next, 1, 16);
                    byte_wb(I, I->reg, 16);
                } else
                    emit_load_mem(I, next, w, I->reg);
            } else { // mov [mem], reg  -- folded into one str when [base+disp]
                int rn, off, f = ea_imm_fold(I, w, &rn, &off);
                if (f) {
                    int sv = (w == 1) ? byte_val(I, I->reg, 16) : I->reg;
                    if (f == 1)
                        e_store_uoff(w, sv, rn, (unsigned)off);
                    else
                        e_stur(w, sv, rn, off);
                } else {
                    emit_ea(I, next);                                     // may clobber x16
                    int sv = (w == 1) ? byte_val(I, I->reg, 16) : I->reg; // byte src: ah/bh/ch/dh -> bits 8-15
                    e_store(w, sv, 17);
                }
            }
        } else if (w == 1) {
            // byte reg-to-reg (88/8A, mod=3): copy ONE byte, hi8-aware, preserving the dest's other
            // bits. The full-width e_mov_rr below was wrong here -- `mov bl,cl` copied all of ecx into
            // ebx (and high-byte src/dst like `mov al,dh` were garbage), only masked when the upper
            // bytes happened to be 0. That polluted icu's TinyStr niche math -> dropped 'n' in "en-US".
            int srcreg = to_reg ? I->rm_reg : I->reg;
            int dstreg = to_reg ? I->reg : I->rm_reg;
            int sv = byte_val(I, srcreg, 16);
            byte_wb(I, dstreg, sv);
        } else {
            if (to_reg)
                e_mov_rr(I->reg, I->rm_reg, sf);
            else
                e_mov_rr(I->rm_reg, I->reg, sf);
        }
        return TX_NEXT;
    }
    // ---- lea (8D) ----
    if (op == 0x8D) {
        // Non-PIE pointer identity: a biased ET_EXEC maps HIGH (+g_nonpie_bias), but its baked
        // absolute pointers keep their LOW link addresses -- in .data, in the .tdata TLS template
        // (e.g. glibc's per-thread `tcache` initialized to &__tcache_dummy), and in 32-bit `mov
        // $imm` materializations. A rip-relative lea materializes a pointer VALUE, so it must produce
        // the SAME (low) address a baked pointer holds, or an identity comparison of the two diverges.
        // The concrete failure this fixes: at thread exit __malloc_arena_thread_freeres compares the
        // thread's tcache pointer (the low template value &__tcache_dummy, never overwritten by a
        // never-malloc'ing bare worker thread) against `lea __tcache_dummy(%rip)`; a HIGH lea result
        // != the LOW stored value, so the "is this the dummy sentinel?" guard fails and glibc frees
        // the sentinel -> SIGSEGV (every spawned pthread in a static -no-pie x86 binary crashed). So
        // for a rip-relative lea whose target lands in the low image range, emit the LOW link address.
        // This is the exact analogue of the aarch64 engine's adr/adrp PC un-biasing (translate.c
        // pcrel_base) and generalizes the earlier Go-type-section-only rewrite (which was one instance
        // of the same class). Derefs of the resulting low pointer are folded to the high mapping by
        // ea_bias17 / the rep-string helpers (repstr_g2h) / nonpie_fixup; an indirect call/jmp through
        // it is redirected by the dispatcher (run_guest). Only the 64-bit form (sf); inert for
        // PIE/static-PIE (g_nonpie_lo == 0). NOGUESTFOLD leaves lea biased for A/B bisection.
        if (sf && I->rip_rel && g_nonpie_lo && guestfold_on()) {
            uint64_t lo = (next - g_nonpie_bias) + (uint64_t)I->disp; // low link target
            // EXPERIMENT(diagnosis): for a Go image restrict the low-rewrite to the type
            // section only (the pre-v0.9.40 behavior). Whole-image low-rewrite wrongly caught
            // code-address leas (LEAQ asyncPreempt(SB)) that findfunc needs HIGH -> crash.
            uint64_t rlo = g_nonpie_types_lo ? g_nonpie_types_lo : g_nonpie_lo;
            uint64_t rhi = g_nonpie_types_lo ? g_nonpie_types_hi : g_nonpie_hi;
            if (lo >= rlo && lo < rhi) {
                e_movconst(I->reg, lo);
                return TX_NEXT;
            }
        }
        emit_ea_core(I, next, 0); // lea returns the guest (low) effective ADDRESS -> no bias-fold
        e_mov_rr(I->reg, 17, sf);
        return TX_NEXT;
    }
    // ---- mov r/m16, Sreg (8C) / mov Sreg, r/m16 (8E) ----
    // 64-bit userspace segment model: the six selectors are fixed -- ES/DS/FS/GS = 0, CS = 0x33,
    // SS = 0x2b -- exactly what Linux sets at process start and what the qemu-user oracle reports.
    // The architecturally-visible FS/GS *bases* live in cpu->fs_base/gs_base (set via arch_prctl),
    // NOT the selector, so a selector move touches neither. MOV-from-Sreg materializes the constant
    // selector; MOV-to-Sreg accepts and discards it (loading a userspace selector has no visible
    // effect without a modify_ldt(2) descriptor, which this engine does not model). The ModRM.reg
    // field (low 3 bits) picks ES(0),CS(1),SS(2),DS(3),FS(4),GS(5); 6/7 are reserved (#UD on HW).
    if (op == 0x8C) {
        static const int sel[8] = {0, 0x33, 0x2b, 0, 0, 0, 0, 0};
        int s = sel[I->reg & 7];
        if (I->is_mem) { // selector -> m16 (always a 16-bit store, regardless of opsize)
            emit_ea(I, next);
            e_movconst(16, (uint64_t)s);
            e_store(2, 16, 17);
        } else if (I->opsize == 2) { // r16 dest: write low 16, preserve bits 63:16
            e_movconst(16, (uint64_t)s);
            e_bfi(I->rm_reg, 16, 0, 16, 1);
        } else { // r32/r64 dest: selector zero-extended to the full register
            e_movconst(I->rm_reg, (uint64_t)s);
        }
        return TX_NEXT;
    }
    if (op == 0x8E) { // mov Sreg, r/m16 -- accept + discard (no userspace-visible effect; see above)
        return TX_NEXT;
    }
    // ---- push/pop r (50-5F) ----
    if (op >= 0x50 && op <= 0x57) {
        int r = (op - 0x50) | (I->rexB << 3);
        e_subi(RSP, RSP, 8, 1);
        e_store(8, r, RSP);
        return TX_NEXT;
    } // push (64-bit)
    if (op >= 0x58 && op <= 0x5F) {
        int r = (op - 0x58) | (I->rexB << 3);
        e_load(8, r, RSP);
        e_addi(RSP, RSP, 8, 1);
        return TX_NEXT;
    } // pop
    // ---- movsxd (0x63): operand-size governed. REX.W -> sign-extend r/m32 to r64; no REX.W ->
    // a 32-bit move (zero-extend r/m32 to 64); 0x66 -> a 16-bit move (insert low 16, keep 63:16).
    if (op == 0x63) {
        if (I->opsize == 8) { // REX.W: sign-extend 32 -> 64
            if (I->is_mem) {
                emit_ea(I, next);
                e_ldrs(4, I->reg, 17);
            } else
                e_sxt(I->reg, I->rm_reg, 4);
        } else if (I->opsize == 2) { // 0x66: 16-bit move, preserve bits 63:16
            if (I->is_mem) {
                emit_ea(I, next);
                e_load(2, 16, 17);
                e_bfi(I->reg, 16, 0, 16, 1);
            } else
                e_bfi(I->reg, I->rm_reg, 0, 16, 1);
        } else { // no REX.W: 32-bit move, zero-extend to 64
            if (I->is_mem) {
                emit_ea(I, next);
                e_load(4, I->reg, 17);
            } else
                e_mov_rr(I->reg, I->rm_reg, 0);
        }
        return TX_NEXT;
    }
    return TX_FALL;
}
