// dd/runtime/translate/x86_64 -- shift/rotate instruction class (group2: C0/C1 imm, D0/D1 by 1, D2/D3 by
// CL -> SHL/SHR/SAR/ROL/ROR/RCL/RCR), lifted VERBATIM out of translate_block's one-byte switch. Emits the
// exact x86 CF/SF/ZF/PF (incl. the count==0 'no flags change' rule and the by-CL masked-count case). A rare
// unhandled form (RCL/RCR by CL) defers to C via report_unimpl. Returns TX_FALL (not a shift), TX_NEXT
// (caller: gpc = next; continue) or TX_BREAK (deferred form -> end the block). #included after the other
// class files (uses emit_rcl_rcr/rm_load/rm_store/report_unimpl above + the e_* shift/flag emitters).
// Dead-flag elision for value-only IMMEDIATE shifts. `shl r,3` (address/index scaling), `shr r,N`
// (bitfield extraction), etc. set CF/SF/ZF/PF (+OF for a 1-bit shift) that in real code are usually
// never read before the next flag producer overwrites them. The shift path always materializes them
// eagerly (e_tst + e_nzcv_save[_setcf] + e_pf_save), unlike the lazy ALU deferral. When the successor
// guest-byte liveness scan (translate/trace.c, the same proven scanner as the cross-block elision)
// proves EVERY written flag is dead before any read, skip the whole synthesis -- the value store is
// unaffected. Conservative + safe: block enders => all live => never elided; AF is left untouched by
// the shift either way (the shift path writes no AF), so eliding cannot perturb it.
// Gate: NOSHIFTFLAGELIDE=1 (independent A/B); also off under NOFLAGELIDE and whenever xblkflags is off.
static int noshiftflagelide(void) {
    static int v = -1;
    if (v < 0) v = (getenv("NOSHIFTFLAGELIDE") || getenv("NOFLAGELIDE")) ? 1 : 0;
    return v;
}

// PROF: SHL/SHR/SAR immediate flag-materializations elided by the successor-dead-flag scan.
static uint64_t g_prof_shflag;

static int translate_shift(struct insn *I, uint64_t gpc, uint64_t next) {
    uint8_t op = I->op;
    int sf = I->opsize == 8;
    // ---- shifts: group2 (C0/C1 imm, D0/D1 by 1, D2/D3 by CL) ----
    if (op == 0xC0 || op == 0xC1 || op == 0xD0 || op == 0xD1 || op == 0xD2 || op == 0xD3) {
        int k = I->reg & 7;
        if (k == 6) k = 4; // SAL == SHL
        int w = (op & 1) ? I->opsize : 1, mem;
        int bycl = (op == 0xD2 || op == 0xD3), by1 = (op == 0xD0 || op == 0xD1);
        // RCL(/2)/RCR(/3) through carry: the constant-count forms (by-1, immediate) are emitted here;
        // the by-CL form still defers (caught by the unimpl check below).
        if ((k == 2 || k == 3) && !bycl) {
            int cmask = (w == 8) ? 63 : 31;
            emit_rcl_rcr(I, next, w, k == 3, by1 ? 1 : ((int)I->imm & cmask));
            return TX_NEXT;
        }
        if (k == 2 || k == 3) {
            // RCL/RCR by CL: the count MOD (width+1) reduction (mod 9/17 for byte/word) is awkward in
            // emitted code, so exit to do_rcl (R_RCL) which does the whole rotate + CF/OF in C. The
            // lazy-flag pre-pass in translate_block has already materialized cpu->nzcv, so do_rcl's
            // CF carry-in (and the following block's flag reload) sees the canonical membank flags.
            uint64_t desc = (uint64_t)w | ((uint64_t)(k == 3) << 8);
            if (I->is_mem) {
                emit_ea(I, next);         // x17 = host effective address
                e_str(17, 28, OFF_X87EA); // stash the EA for do_rcl to dereference
                desc |= (1ull << 9);
            } else {
                int hi8 = (w == 1 && !I->has_rex && I->rm_reg >= 4 && I->rm_reg <= 7);
                int rr = hi8 ? (I->rm_reg - 4) : I->rm_reg; // AH/CH/DH/BH -> base reg RAX/RCX/RDX/RBX
                desc |= ((uint64_t)(hi8 ? 1 : 0) << 10) | ((uint64_t)(rr & 0x1f) << 16);
            }
            e_movconst(16, desc);
            e_str(16, 28, OFF_DIVOP);
            emit_exit_const(next, R_RCL);
            return TX_BREAK; // block ends at the exit (rip = next; do_rcl resumes there)
        } // RCL/RCR by CL -> C helper
        int raw = rm_load(I, next, w, &mem);
        if ((k == 0 || k == 1) && w < 4) {        // 8/16-bit ROL/ROR -- rotate WITHIN the operand width
            int width = 8 * w;                    // (a 64-bit ROR would wrap the wrong bits, e.g. rolw $8)
            e_uxt(16, raw, w);                    // x16 = zero-extended operand (low `width` bits)
            e_bfi(16, 16, width, width, 0);       // replicate v -> [2w-1:w] (16-bit: now 32 bits = v|v)
            if (w == 1) e_bfi(16, 16, 16, 16, 0); // byte: replicate the pair again -> 4 copies fill 32 bits
            if (bycl) {                           // count = CL masked to the operand width
                e_movconst(19, width - 1);
                e_rrr(A_AND, 20, RCX, 19, 0, 0); // x20 = CL & (width-1)
                if (k == 0) {
                    e_movconst(19, width);
                    e_rrr(A_SUB, 20, 19, 20, 0, 0); // ROL by n == ROR by (width-n)
                    e_movconst(19, width - 1);
                    e_rrr(A_AND, 20, 20, 19, 0, 0);
                }
                e_shv(S_RORV, 16, 16, 20, 0); // 32-bit RORV of the replicated value -> low `width` bits correct
            } else {
                int ce = ((((by1 ? 1 : (int)I->imm) % width) + width) % width);
                int rr = (k == 1) ? ce : (width - ce) % width;
                if (rr) e_ror_i(16, 16, rr, 0); // 32-bit ROR; low `width` bits are the answer
            }
            // x86 rotates leave SF/ZF/PF/AF unchanged but DO set CF (and OF for a 1-bit rotate).
            if (bycl) {
                e_rot_flags_cl(16, k, width);
            } else {
                int ce = ((((by1 ? 1 : (int)I->imm) % width) + width) % width);
                if (ce) e_rot_flags_const(16, k, width, ce);
            }
            rm_store(I, w, 16); // stores low w bytes
            return TX_NEXT;
        }
        int ssf = (w >= 4) ? sf : 1; // operate 64-bit on extended byte/word
        // register residency: an IMMEDIATE/by-1 SHL/SHR/SAR whose r/m is a REGISTER at width>=4
        // shifts straight into the guest home (src==dst==raw==I->rm_reg), skipping the raw->x16 copy
        // and the store-back. `direct` is false for mem/byte-word/CL-variable/rotate -> those keep x16.
        int direct = xshiftdirect_on() && !bycl && !mem && w >= 4 && (k == 4 || k == 5 || k == 7);
        // bring the operand into x16, zero/sign-extended for w<4
        if (w < 4) {
            if (k == 5)
                e_uxt(16, raw, w);
            else if (k == 7)
                e_sxt(16, raw, w);
            else
                e_mov_rr(16, raw, 0);
        } else if (!direct && raw != 16)
            e_mov_rr(16, raw, sf);
        int R = direct ? raw : 16; // result/work register: guest home when direct, else scratch x16
        int src = direct ? raw : 16;
        int cnt = by1 ? 1 : (bycl ? -1 : (int)(I->imm & (w == 8 ? 63 : 31)));
        // exact x86 CF (last bit shifted out) for SHL/SHR/SAR immediate at ALL widths (8/16/32/64)
        int want_cf = (!bycl && (k == 4 || k == 5 || k == 7) && cnt >= 1);
        // dead want_cf save: when the WHOLE architectural flag output {CF,SF,ZF,OF,PF} is dead
        // at every successor (the SAME guest-byte scan that elides the materialization below), the
        // `mov x19,src` preserving the operand for the exact-CF path is itself dead -- skip it. Uses
        // the shift flag-elision gate+predicate, so it changes bytes ONLY when the materialization is
        // elided too; NOSHIFTFLAGELIDE=1 forces flags_dead=0 -> the save (and elision) match baseline.
        int flags_dead =
            !bycl && !noshiftflagelide() && xblkflags_on() && !(x86_flags_livein(next, gpc) & (XF_ALL & ~XF_AF));
        if (want_cf && !flags_dead) e_mov_rr(19, src, ssf); // save original operand for CF
        if (bycl) {
            if (k == 0) { // ROL r/m32|64 by CL == ROR by (width - n); leaves SF/ZF unchanged (no flag save)
                int width = ssf ? 64 : 32;
                e_movconst(19, width - 1);
                e_rrr(A_AND, 20, RCX, 19, ssf, 0); // x20 = n = CL & (width-1)
                e_movconst(19, width);
                e_rrr(A_SUB, 20, 19, 20, ssf, 0); // x20 = width - n
                e_movconst(19, width - 1);
                e_rrr(A_AND, 20, 20, 19, ssf, 0); // x20 = (width - n) & (width-1)  -> n==0 maps to rot 0
                e_shv(S_RORV, 16, src, 20, ssf);  // rorv x16, src, x20
                e_rot_flags_cl(16, 0, width);     // ROL CF=LSB(result), OF (1-bit) -- SF/ZF unchanged
                rm_store(I, w, 16);
                return TX_NEXT;
            }
            uint32_t b = k == 4 ? S_LSLV : k == 5 ? S_LSRV : k == 7 ? S_ASRV : S_RORV;
            // SHL/SHR/SAR by CL: stash the ORIGINAL operand (x26) so the exact x86 CF (the last bit
            // shifted out) can be recovered after the destructive shift, below. Use x26 (a
            // trampoline-preserved scratch) -- NOT x17, which holds the EA for a MEMORY destination and
            // must survive to rm_store (a `shr [mem],cl` stashed the operand over the EA -> rm_store
            // wrote the result to a garbage address = the operand value; jemalloc bitmap_init crash).
            if (k == 4 || k == 5 || k == 7) {
                if (w < 4 && k == 4) { // byte/word SHL: mask CF source to operand width (drop high bits)
                    e_movconst(19, (1u << (8 * w)) - 1);
                    e_rrr(A_AND, 26, src, 19, 1, 0);
                } else
                    e_mov_rr(26, src, ssf); // SHR uxt / SAR sxt already width-correct
            }
            // byte/word SHL/SHR/SAR by CL run on the 64-bit-extended operand (ssf=1), but x86
            // masks the count to 5 bits (0x1f) for every size < 64-bit -- so shift as a 32-bit ARM op
            // (count masked by 31, matching x86), NOT 64-bit (mask 63). Without this, `shrw %cl` with
            // CL=32 shifted the extended value by 32 (result 0) instead of x86's count&0x1f==0 (no-op).
            // 32-bit ASRV sign-fills from bit 31, which the sxt-to-64 already set for SAR, so the low
            // `w` bytes stay correct; the exact CF still comes from the 64-bit x26 stash below.
            e_shv(b, 16, src, RCX, (w < 4) ? 0 : ssf);
        } else {
            if (cnt == 0) {
                // x86: a 0 effective count changes NO flags and leaves the value unchanged --
                // but a 32-bit register destination is still written, so bits 63:32 must be
                // zeroed (a 32-bit op zero-extends). w==8/16/8 register dests keep their bits;
                // a memory dest is rewritten unchanged.
                if (mem)
                    e_store(w, raw, 17);
                else if (w == 4)
                    e_mov_rr(raw, raw, 0);
                return TX_NEXT;
            } // no flags change
            if (k == 4)
                e_lsl_i(R, src, cnt, ssf);
            else if (k == 5)
                e_lsr_i(R, src, cnt, ssf);
            else if (k == 7)
                e_asr_i(R, src, cnt, ssf);
            else if (k == 1)
                e_ror_i(16, src, cnt, ssf);
            else /*k==0 ROL*/
                e_ror_i(16, src, (ssf ? 64 : 32) - cnt, ssf);
        }
        if (k == 0 || k == 1) { // ROL/ROR: set only CF/OF; leave SF/ZF/PF/AF (no shift flag path)
            int rwidth = ssf ? 64 : 32;
            if (bycl)
                e_rot_flags_cl(16, k, rwidth);
            else if (cnt != 0)
                e_rot_flags_const(16, k, rwidth, cnt);
            rm_store(I, w, 16);
            return TX_NEXT;
        }
        // Dead-flag elision: an IMMEDIATE SHL/SHR/SAR (cnt is a compile-time constant here; the
        // by-CL/variable path is left to materialize -- its flag effect is count-dependent) whose
        // WHOLE architectural flag output {CF,SF,ZF,OF,PF} is provably dead before any read at every
        // successor entry (guest-byte scan). AF is never written by this path either way, so it is
        // excluded from the mask (eliding leaves it byte-identical to the materialized path). The
        // value in x16 is final; skip the tst + nzcv/PF synthesis entirely and just store.
        if (flags_dead) {      // predicate hoisted above (identical to the old inline condition)
            rm_store(I, w, R); // no-op when direct (R==I->rm_reg); stores x16 otherwise
            g_prof_shflag++;
            return TX_NEXT;
        }
        // SF/ZF from result (byte/word via high-bits); CF exact for immediate SHL/SHR/SAR, else approximate
        if (w < 4) {
            e_lsl_i(21, 16, 8 * (4 - w), 0);
            e_tst(21, 0);
        } else
            e_tst(R, sf); // R==x16 unless direct (then the guest home holds the result)
        if (bycl) {
            // x86 leaves ALL flags unchanged when the runtime count (CL masked to the operand width) is
            // 0 -- exactly when the emitted variable shift was a no-op. Capture the would-be flags, then
            // keep the OLD nzcv (and PF, for the SHL/SHR/SAR forms) if the masked count is zero.
            // width = architectural operand bits (CF/OF positions); cmask = x86 count mask (31 for
            // 8/16/32-bit, 63 for 64-bit) -- NOT width-1, so a byte shift by CL=8 is a real count of 8.
            int width = (w >= 4) ? (ssf ? 64 : 32) : (8 * w);
            int cmask = (w == 8) ? 63 : 31;
            emit32(0xD53B4200u | 20); // mrs x20, nzcv  (new N/Z from result; C/V from the tst)
            e_ldr(24, 28, OFF_NZCV);  // x24 = old nzcv
            e_movconst(19, cmask);
            e_rrr(A_ANDS, 22, RCX, 19, ssf, 0); // x22 = n = CL & cmask; Z = (n == 0)
            if (k == 4 || k == 5 || k == 7) {
                // Exact x86 CF = last bit shifted out of the ORIGINAL operand (x26): SHL -> bit
                // (width - n), SHR/SAR -> bit (n - 1) (n>0 here; the n==0 csel below discards this).
                // ARM tst left C=0; replace the stored borrow C with NOT CF. All ops below are
                // flag-free, so the ANDS's Z survives to the csel.
                if (k == 4) {
                    e_movconst(19, width);
                    e_rrr(A_SUB, 23, 19, 22, ssf, 0); // x23 = width - n
                } else {
                    e_subi(23, 22, 1, ssf); // x23 = n - 1
                }
                e_shv(S_LSRV, 23, 26, 23, ssf); // x23 = orig >> bit
                e_movconst(19, 1);
                e_rrr(A_AND, 23, 23, 19, ssf, 0); // x23 = x86 CF (0/1)
                e_rrr(A_EOR, 23, 23, 19, 0, 0);   // x23 = NOT CF
                e_movconst(19, 1u << 29);
                e_rrr(A_BIC, 20, 20, 19, 1, 0);  // clear stored borrow C (bit 29)
                e_rrr(A_ORR, 20, 20, 23, 1, 29); // stored C = (NOT CF) << 29
            }
            e_csel(20, 24, 20, 0 /*EQ: count==0*/, 1);
            e_str(20, 28, OFF_NZCV);
            if (!g_pfaf_dead && (k == 4 || k == 5 || k == 7)) { // SHL/SHR/SAR set PF; rotates leave it
                e_ldr(25, 28, OFF_PF);                          // old PF (skipped when PF dead)
                e_csel(23, 25, 16, 0, 1); // EQ (count==0) -> keep old PF, else result low byte (x16)
                e_pf_save(23);
            }
            // sync the LIVE ARM NZCV to the just-stored value (x20 is unchanged since the str).
            // SHL/SHR (k==4/5) re-store+msr in the OF block below, but SAR (k==7) never reaches it -- so
            // without this msr the live NZCV stays stale (Z=0 from the count ANDS, C junk). A stitched
            // successor reads the correct membank value, but a CHAINED edge's boundary spill persists the
            // stale LIVE NZCV over cpu->nzcv -> wrong CF/ZF (sarq %cl a=0 cl=1 gave CF=1/ZF=0). Emitted
            // AFTER the PF csel above, which still needs the ANDS's live Z (=count==0) as its condition.
            // The rotate helpers (e_rot_flags_cl/const) already keep this invariant; the shift path must too.
            if (k == 7) emit32(0xD51B4200u | 20); // msr nzcv, x20
            // x86 OF is DEFINED only for a 1-bit shift; for SHL/SHR by CL set V=OF in the stored
            // NZCV only when the masked count is exactly 1 (SAR OF=0 / count!=1 OF undefined -> leave).
            if (k == 4 || k == 5) {
                e_lsr_i(23, 26, width - 1, ssf); // MSB(original x26)
                e_movconst(19, 1);
                e_rrr(A_AND, 23, 23, 19, ssf, 0);
                if (k == 4) { // SHL OF = MSB(result) XOR CF; for count==1 CF = MSB(original)
                    e_lsr_i(22, 16, width - 1, ssf);
                    e_rrr(A_AND, 22, 22, 19, ssf, 0);
                    e_rrr(A_EOR, 23, 23, 22, 0, 0); // x23 = OF
                }
                e_movconst(19, cmask);
                e_rrr(A_AND, 22, RCX, 19, ssf, 0); // x22 = n
                e_subi_s(22, 22, 1, ssf);          // Z = (n == 1)
                e_ldr(20, 28, OFF_NZCV);
                e_movconst(19, 1u << 28);
                e_rrr(A_BIC, 24, 20, 19, 1, 0);  // x24 = nzcv with V cleared
                e_rrr(A_ORR, 24, 24, 23, 1, 28); // x24 = nzcv with V = OF
                e_csel(20, 24, 20, 0 /*EQ: n==1*/, 1);
                e_str(20, 28, OFF_NZCV);
                emit32(0xD51B4200u | 20); // msr nzcv, x20
            }
        } else {
            if (want_cf) {
                // Architectural operand width (8/16/32/64); the shift ran on a 64-bit-extended value
                // for w<4, but CF/OF are defined over the true operand width.
                int width = (w >= 4) ? (ssf ? 64 : 32) : (8 * w);
                // CF = last bit shifted out: SHL -> bit(width-cnt), SHR/SAR -> bit(cnt-1). A count
                // that clears every bit (cnt>width) leaves CF=0 for SHL/SHR; SAR keeps the sign bit.
                int cf_zero = 0, bit;
                if (k == 4) {
                    if (cnt > width) {
                        cf_zero = 1;
                        bit = 0;
                    } else
                        bit = width - cnt;
                } else { // SHR (5) / SAR (7)
                    if (k == 5 && cnt > width) {
                        cf_zero = 1;
                        bit = 0;
                    } else
                        bit = (cnt - 1 > width - 1) ? (width - 1) : (cnt - 1);
                }
                // x86 OF is DEFINED only for a 1-bit shift: SHL OF = MSB(result) XOR CF; SHR OF =
                // MSB(original); SAR OF = 0 (already, since the tst left V=0). For count>1 OF is
                // undefined -> leave it. x19 still holds the ORIGINAL operand here.
                int set_of = (cnt == 1 && (k == 4 || k == 5));
                if (set_of && k == 5) { // SHR: stash MSB(original) in x21 (survives the nzcv save)
                    e_lsr_i(21, 19, width - 1, ssf);
                    e_movconst(23, 1);
                    e_rrr(A_AND, 21, 21, 23, ssf, 0);
                }
                if (cf_zero) {
                    e_movconst(19, 0); // every bit shifted out -> CF=0
                } else {
                    e_lsr_i(19, 19, bit, ssf);
                    e_movconst(23, 1);
                    e_rrr(A_AND, 19, 19, 23, ssf, 0); // x19 = x86 CF bit
                }
                e_nzcv_save_setcf(19);
                if (set_of && k == 4) { // SHL: OF = MSB(result R) XOR CF (x19)
                    e_lsr_i(22, R, width - 1, ssf);
                    e_movconst(23, 1);
                    e_rrr(A_AND, 22, 22, 23, ssf, 0);
                    e_rrr(A_EOR, 22, 22, 19, 0, 0);
                    e_nzcv_set_of(22);
                } else if (set_of) { // SHR: OF was stashed in x21
                    e_nzcv_set_of(21);
                }
            } else
                e_nzcv_save();
            // x86 PF: SHL/SHR/SAR set SF/ZF/PF from the result; rotates (ROL/ROR) leave PF unchanged.
            if (!g_pfaf_dead && (k == 4 || k == 5 || k == 7))
                e_pf_save(R); // result low byte -> PF lane (R holds result; skip when dead)
        }
        rm_store(I, w, R); // no-op when direct (R==I->rm_reg); stores x16 otherwise
        return TX_NEXT;
    }
    return TX_FALL;
}
