// dd/runtime/frontend/x86_64 -- the x86-64 -> arm64 translator (flag synthesis, SSE/x87 lowering, the
// big translate_block) + host entry trampolines.

// ---------------- the translator ----------------
static void report_unimpl(uint64_t pc, struct insn *I);

// MUL/IMUL (group3 F6/F7 /4,/5) set x86 CF=OF when the high half of the product is significant
// (MUL: high half != 0; IMUL: high half != sign-extension of the low half); SF/ZF/AF/PF are
// x86-undefined. cfreg holds the computed CF/OF as 0/1. Write the stored NZCV using the engine's
// borrow convention (stored C = NOT x86 CF at bit 29, OF = V at bit 28) with N=Z=0; scratch x20/x23.
static void e_mul_set_oc(int cfreg) {
    e_movconst(23, 1);
    e_rrr(A_EOR, 23, cfreg, 23, 0, 0); // x23 = NOT cf (cf is 0/1)
    e_movconst(20, 0);
    e_rrr(A_ORR, 20, 20, 23, 1, 29);    // stored C (bit 29) = NOT x86 CF
    e_rrr(A_ORR, 20, 20, cfreg, 1, 28); // V (bit 28) = OF = cf
    e_str(20, 28, OFF_NZCV);
    emit32(0xD51B4200u | 20); // msr nzcv, x20 (sync live flags)
}

// imul reg<-a*b (two-/three-operand forms 0F AF, 69, 6B): truncated product into dst, and x86
// CF=OF = (the full signed product differs from the sign-extension of the truncated result).
// Scratch x21..x25 (x21 carries the 0/1 CF into e_mul_set_oc); callers must not pass a/b in those.
// x86-xflags: when `co_live`==0 the caller proved the WHOLE NZCV word imul defines (dd sets N=Z=0,
// C=NOT CF, V=OF) is dead before any read -> skip the entire overflow/flag synthesis (incl. the extra
// smulh, a real multiply that contends with the product mul on a dependent chain) and emit product-only.
static void e_imul2(int dst, int a, int b, int w, int co_live) {
    if (!co_live) { // product only; imul's CF/OF/SF/ZF are all dead
        if (w == 8) {
            e_mul(dst, a, b, 1); // low 64 bits
        } else if (w == 4) {
            e_mul(dst, a, b, 0); // 32-bit mul zero-extends bits 63:32
        } else {                 // 16-bit: insert low 16, preserve upper
            e_mul(22, a, b, 0);
            e_bfi(dst, 22, 0, 16, 1);
        }
        return;
    }
    if (w == 8) {
        e_smulh(24, a, b);               // x24 = signed high 64 bits of the product
        e_mul(dst, a, b, 1);             // dst = low 64 (a,b already consumed by smulh)
        e_asr_i(25, dst, 63, 1);         // x25 = sign-extension of the low half
        e_rrr(A_SUBS, 22, 24, 25, 1, 0); // overflow iff high != sign(low)
        e_cset(21, 1 /*NE*/, 1);
    } else { // 32- or 16-bit: full signed product, overflow iff it != sxt of the truncated result
        e_sxt(24, a, w);
        e_sxt(25, b, w);
        e_mul(22, 24, 25, 1); // x22 = full signed product (operands fit in 32, so 64 is exact)
        e_sxt(23, 22, w);     // x23 = sign-extension of the low w bytes
        e_rrr(A_SUBS, 25, 22, 23, 1, 0);
        e_cset(21, 1 /*NE*/, 1);
        if (w == 4)
            e_mov_rr(dst, 22, 0); // 32-bit dest: low 32, zero-extended
        else
            e_bfi(dst, 22, 0, 16, 1); // 16-bit dest: insert low 16, preserve upper bits
    }
    e_mul_set_oc(21);
}

// 8/16-bit one-operand MUL/IMUL (F6/F7 /4,/5) CF=OF: MUL -> the high half is nonzero; IMUL -> the result
// doesn't fit the low half (full signed product != sign-extension of the low `w` bytes). SF/ZF/AF/PF are
// x86-undefined. `prod` holds the product (2*w bytes) in a 32-bit reg; k==4 MUL / k==5 IMUL. Scratch
// x22/x23 (+ e_mul_set_oc's x20/x23); leaves `prod` intact.
static void e_mul_oc_narrow(int prod, int k, int w) {
    if (k == 4) { // MUL: CF=OF = (high half != 0)
        e_lsr_i(22, prod, 8 * w, 0);
        e_subi_s(23, 22, 0, 0);
    } else { // IMUL: CF=OF = (sxt(low half) != full product)
        e_sxt(22, prod, w);
        e_rrr(A_SUBS, 23, prod, 22, 0, 0);
    }
    e_cset(22, 1 /*NE*/, 0);
    e_mul_set_oc(22);
}

// x86 ROL/ROR affect ONLY CF and OF; SF/ZF/PF/AF are left untouched. CF gets the bit that wrapped to the
// other end: ROR -> CF = MSB of the result (bit width-1); ROL -> CF = LSB (bit 0). OF is x86-DEFINED only
// for a 1-bit rotate: ROL -> OF = MSB(result) XOR CF; ROR -> OF = MSB XOR (bit width-2). For any other
// count OF is undefined and left unchanged. `res` holds the rotated value in its low `width` bits. We
// rewrite only stored-C (bit29 = NOT CF, the borrow convention) and V (bit28 = OF), preserving N/Z and the
// PF/AF lanes. `cnt` is the (already masked, nonzero) immediate count -> OF written iff cnt==1. Scratch x18..x23.
static void e_rot_flags_const(int res, int k, int width, int cnt) {
    int wsf = width == 64;
    e_ldr(18, 28, OFF_NZCV);
    e_lsr_i(20, res, k == 1 ? width - 1 : 0, wsf);
    e_movconst(21, 1);
    e_rrr(A_AND, 20, 20, 21, 0, 0); // x20 = x86 CF (0/1)
    e_movconst(21, 1u << 29);
    e_rrr(A_BIC, 18, 18, 21, 1, 0); // clear stored C
    e_movconst(21, 1);
    e_rrr(A_EOR, 22, 20, 21, 0, 0);  // x22 = NOT CF
    e_rrr(A_ORR, 18, 18, 22, 1, 29); // stored C = (NOT CF) << 29
    if (cnt == 1) {
        e_lsr_i(22, res, width - 1, wsf);
        e_movconst(21, 1);
        e_rrr(A_AND, 22, 22, 21, 0, 0); // x22 = MSB(result)
        if (k == 1) {
            e_lsr_i(23, res, width - 2, wsf);
            e_rrr(A_AND, 23, 23, 21, 0, 0); // x23 = bit width-2
        } else
            e_mov_rr(23, 20, 0);        // x23 = CF
        e_rrr(A_EOR, 22, 22, 23, 0, 0); // x22 = OF
        e_movconst(21, 1u << 28);
        e_rrr(A_BIC, 18, 18, 21, 1, 0);  // clear V
        e_rrr(A_ORR, 18, 18, 22, 1, 28); // V = OF
    }
    e_str(18, 28, OFF_NZCV);
    emit32(0xD51B4200u | 18); // msr nzcv, x18 (sync live flags)
}

// ROL/ROR by CL: like e_rot_flags_const but the count is runtime (n = CL & (width-1)). When n==0 x86
// changes NO flags, so keep the old NZCV; otherwise set CF (and OF via the 1-bit formula -- for n>1 OF is
// x86-undefined, so emitting that legal value is fine). Reads CL (RCX); scratch x18..x25.
static void e_rot_flags_cl(int res, int k, int width) {
    int wsf = width == 64;
    // "flags affected?" is decided by the 5-bit (0x1f) / 6-bit (0x3f, REX.W) masked count -- NOT the
    // rotate amount (count MOD width). For 8/16-bit rotates these differ: e.g. `rolb %cl` with CL=8 rotates
    // by 8%8==0 (value unchanged) but (CL&0x1f)==8!=0 so x86 DOES set CF = LSB(result). Masking by width-1
    // here (7 for a byte) wrongly took the count==0 keep-old path and left stale CF. Use the true x86 cmask;
    // for width 32/64 this is width-1 (unchanged), so only byte/word behavior moves.
    e_movconst(19, (width == 64) ? 63 : 31);
    e_rrr(A_ANDS, 24, RCX, 19, wsf, 0); // x24 = n = CL & cmask (x86 5/6-bit); Z = (n == 0) -> flags unchanged
    e_ldr(18, 28, OFF_NZCV);            // old NZCV (kept when n == 0)
    e_lsr_i(20, res, k == 1 ? width - 1 : 0, wsf);
    e_movconst(21, 1);
    e_rrr(A_AND, 20, 20, 21, 0, 0); // x20 = CF
    e_mov_rr(25, 18, 1);            // candidate = copy of old NZCV
    e_movconst(21, 1u << 29);
    e_rrr(A_BIC, 25, 25, 21, 1, 0); // clear stored C
    e_movconst(21, 1);
    e_rrr(A_EOR, 22, 20, 21, 0, 0);  // NOT CF
    e_rrr(A_ORR, 25, 25, 22, 1, 29); // stored C = (NOT CF) << 29
    e_lsr_i(22, res, width - 1, wsf);
    e_movconst(21, 1);
    e_rrr(A_AND, 22, 22, 21, 0, 0); // MSB(result)
    if (k == 1) {
        e_lsr_i(23, res, width - 2, wsf);
        e_rrr(A_AND, 23, 23, 21, 0, 0); // bit width-2
    } else
        e_mov_rr(23, 20, 0);        // CF
    e_rrr(A_EOR, 22, 22, 23, 0, 0); // OF
    e_movconst(21, 1u << 28);
    e_rrr(A_BIC, 25, 25, 21, 1, 0);  // clear V
    e_rrr(A_ORR, 25, 25, 22, 1, 28); // V = OF
    // all ops since the ANDS are flag-free, so its Z survives: n==0 -> keep old (x18), else candidate (x25).
    e_csel(18, 18, 25, 0 /*EQ*/, 1);
    e_str(18, 28, OFF_NZCV);
    emit32(0xD51B4200u | 18); // msr nzcv, x18 (sync live flags)
}

// Set x86 OF (= ARM V, bit28) of the stored NZCV to the 0/1 in `ofreg` (read-modify-write; the prior flag
// save left V=0). Used by the 1-bit SHL/SHR paths where OF is x86-defined. `ofreg` must not be x20/x23.
static void e_nzcv_set_of(int ofreg) {
    e_ldr(20, 28, OFF_NZCV);
    e_movconst(23, 1u << 28);
    e_rrr(A_BIC, 20, 20, 23, 1, 0);     // clear V
    e_rrr(A_ORR, 20, 20, ofreg, 1, 28); // V = OF
    e_str(20, 28, OFF_NZCV);
    emit32(0xD51B4200u | 20); // msr nzcv, x20 (sync live flags)
}

// ALU operation selector from the primary opcode group (00..3D) or group1 /digit.
// returns: 0 ADD 1 OR 2 ADC 3 SBB 4 AND 5 SUB 6 XOR 7 CMP, or -1.
static int alu_kind_primary(uint8_t op) {
    int k = (op >> 3) & 7;
    return ((op & 7) <= 5) ? k : -1;
}

// 32/64-bit core ALU into `out`, rn<op>rm, setting ARM flags. out=31 -> discard (cmp/test).
static void alu_core(int kind, int out, int rn, int rm, int sf) {
    switch (kind) {
    case 0: e_rrr(A_ADDS, out, rn, rm, sf, 0); break; // add
    case 4: e_rrr(A_ANDS, out, rn, rm, sf, 0); break; // and / test
    case 5: e_rrr(A_SUBS, out, rn, rm, sf, 0); break; // sub / cmp
    case 1:
        e_rrr(A_ORR, out, rn, rm, sf, 0); // or
        emit32((sf ? 0xEA00001Fu : 0x6A00001Fu) | (out << 16) | (out << 5));
        break; // tst
    case 6:
        e_rrr(A_EOR, out, rn, rm, sf, 0); // xor
        emit32((sf ? 0xEA00001Fu : 0x6A00001Fu) | (out << 16) | (out << 5));
        break;
    default: break;
    }
}

// Byte-register operands: without REX, encodings 4..7 are the HIGH bytes ah/ch/dh/bh
// (bits[15:8] of the first 4 regs); with any REX they're the low bytes spl/bpl/sil/dil.
static int is_hi8(struct insn *I, int regnum) {
    return !I->has_rex && regnum >= 4 && regnum < 8;
}

// value of an 8-bit register operand, in the LOW 8 bits of the returned reg (rest is
// don't-care -- do_alu's <<24 trick keeps only the low byte). hi8 -> extract via >>8.
static int byte_val(struct insn *I, int regnum, int scratch) {
    if (is_hi8(I, regnum)) {
        e_lsr_i(scratch, regnum - 4, 8, 1);
        return scratch;
    }
    return regnum;
}

// write the low byte of `val` into an 8-bit register operand (preserving other bits).
static void byte_wb(struct insn *I, int regnum, int val) {
    if (is_hi8(I, regnum))
        e_bfi(regnum - 4, val, 8, 8, 1);
    else
        e_bfi(regnum, val, 0, 8, 1);
}

// W6A item 1 (non-PIE): the link range + bias of a biased ET_EXEC image (defined in os/linux/container/vfs.c,
// set in elf.c's load_elf -- both later in the unity TU), plus the Go type section [md.types, md.etypes) in
// low link coords (set by elf.c's go_rebase_nonpie; the range whose lea-materialized *_type pointers are made
// low so they match the image's baked-absolute type pointers and Go's type identity holds). Forward tentative
// declarations so the rip-relative `lea` rewrite below can see them. All zero for PIE/static-PIE / non-Go
// images -> the rewrite is inert.
static uint64_t g_nonpie_lo, g_nonpie_hi, g_nonpie_bias, g_nonpie_types_lo, g_nonpie_types_hi;
// V8's embedded-builtins CODE base (symbol v8_Default_embedded_blob_code_) -- a baked LOW.text
// address the binary loads via `mov r32,imm32`. dd runs the code at the HIGH mapping (+bias), so V8's
// InnerPointerToCodeCache range check compares its LOW registered base against a HIGH frame return address
// and MISSES -> V8_Fatal "maybe_code.has_value()" (node:20 `new Error().stack` / mongosh). We record the
// symbol's LOW link value at load (0 if absent / PIE / non-V8) and bias just THIS one materialization to the
// high mapping (translate/mov.c), so V8's code range lives where the code executes -- the same idea as
// go_rebase_nonpie for Go's moduledata code range. Exact-value match -> no other constant is touched, and
// return addresses stay HIGH (Go's gentraceback/findfunc keep working). 0 for PIE / non-V8 / stripped.
static uint64_t g_nonpie_blob_code;

// r/m operand: mem -> EA to x17, load value to x16 (returns 16); reg -> value reg.
static void emit_ea(struct insn *I, uint64_t next_rip);
// unimplemented-insn diagnostic (defined below translate_block); fwd-declared so the instruction-class
// helpers in translate/<class>.c (#included above translate_block) can defer a rare unhandled form.
static void report_unimpl(uint64_t pc, struct insn *I);

static int rm_load(struct insn *I, uint64_t next, int w, int *mem) {
    if (I->is_mem) {
        emit_ea(I, next);
        e_load(w, 16, 17);
        *mem = 1;
        return 16;
    }
    *mem = 0;
    if (w == 1) return byte_val(I, I->rm_reg, 23); // handle ah/ch/dh/bh
    return I->rm_reg;
}

static void rm_store(struct insn *I, int w, int val) { // val -> r/m (EA already in x17 if mem)
    if (I->is_mem) {
        e_store(w, val, 17);
        return;
    }
    if (w == 1) {
        byte_wb(I, I->rm_reg, val);
        return;
    }
    if (val != I->rm_reg) {
        if (w >= 4)
            e_mov_rr(I->rm_reg, val, w == 8);
        else
            e_bfi(I->rm_reg, val, 0, 8 * w, 1);
    }
}

// RCL/RCR (group2 /2,/3): rotate the r/m operand THROUGH the x86 carry flag by a CONSTANT count -- the
// by-1 (D0/D1) and immediate (C0/C1) forms; the by-CL form is left to defer (report_unimpl). The operand
// and CF together form a (W+1)-bit value rotated by `ec`; only CF and -- for a 1-bit rotate -- OF are
// affected, with SF/ZF/PF preserved. Carry-in is taken from cpu->nzcv (stored ARM C = NOT x86 CF; the
// lazy-flag pre-pass has already materialized any pending producer), the result and the new CF/OF are
// emitted with compile-time-constant shifts, and CF/OF are written back to cpu->nzcv. Scratch x19..x24.
static void emit_rcl_rcr(struct insn *I, uint64_t next, int w, int rcr, int cnt_raw) {
    int ssf = (w >= 4) ? (w == 8) : 1; // operate 64-bit for byte/word (operand is zero-extended)
    int W = 8 * w, bw = ssf ? 64 : 32;
    int ec = (w < 4) ? (cnt_raw % (W + 1)) : cnt_raw; // effective rotate through the (W+1)-bit value
    int count1 = (cnt_raw == 1);                      // OF defined only for a single-bit rotate
    int mem;
    int raw = rm_load(I, next, w, &mem);
    if (ec == 0) { // a 0-count rotate is a no-op and affects no flags
        if (mem)
            e_store(w, raw, 17);
        else if (w == 4)
            e_mov_rr(raw, raw, 0); // 32-bit register dest: value unchanged but bits 63:32 must be zeroed
        return;
    }
    if (w < 4)
        e_uxt(19, raw, w); // x19 = zero-extended operand
    else
        e_mov_rr(19, raw, ssf);
    // x24 = carry-in (x86 CF) = NOT(stored ARM C, nzcv bit 29)
    e_ldr(20, 28, OFF_NZCV);
    e_lsr_i(20, 20, 29, 1);
    e_movconst(23, 1);
    e_rrr(A_AND, 20, 20, 23, 0, 0);
    e_rrr(A_EOR, 24, 20, 23, 0, 0);
    // x21 = new x86 CF: RCR -> bit (ec-1) of operand, RCL -> bit (W-ec) of operand
    e_lsr_i(21, 19, rcr ? ec - 1 : W - ec, ssf);
    e_rrr(A_AND, 21, 21, 23, 0, 0);
    // x16 = result (low W bits valid). Terms emitted only when non-trivial -> no out-of-range shifts.
    if (rcr) {
        if (ec < W)
            e_lsr_i(16, 19, ec, ssf); // operand bits that fall straight down
        else
            e_movconst(16, 0);
        if (W - ec == 0) // carry-in lands at result bit (W-ec)
            e_rrr(A_ORR, 16, 16, 24, ssf, 0);
        else {
            e_lsl_i(20, 24, W - ec, ssf);
            e_rrr(A_ORR, 16, 16, 20, ssf, 0);
        }
        if (ec >= 2) { // operand bits below carry wrap to the top: (operand & ((1<<(ec-1))-1)) << (W-ec+1)
            e_lsl_i(20, 19, bw - (ec - 1), ssf);
            e_lsr_i(20, 20, bw - (ec - 1), ssf);
            e_lsl_i(20, 20, W - ec + 1, ssf);
            e_rrr(A_ORR, 16, 16, 20, ssf, 0);
        }
    } else { // RCL
        if (ec < W)
            e_lsl_i(16, 19, ec, ssf);
        else
            e_movconst(16, 0);
        if (ec - 1 == 0) // carry-in lands at result bit (ec-1)
            e_rrr(A_ORR, 16, 16, 24, ssf, 0);
        else {
            e_lsl_i(20, 24, ec - 1, ssf);
            e_rrr(A_ORR, 16, 16, 20, ssf, 0);
        }
        if (ec >= 2) { // top operand bits wrap to the bottom: (operand >> (W+1-ec)) keeping low (ec-1) bits
            e_lsr_i(20, 19, W + 1 - ec, ssf);
            e_lsl_i(20, 20, bw - (ec - 1), ssf);
            e_lsr_i(20, 20, bw - (ec - 1), ssf);
            e_rrr(A_ORR, 16, 16, 20, ssf, 0);
        }
    }
    // OF (single-bit rotate only): RCL -> newCF ^ result_MSB ; RCR -> result top two bits XORed.
    if (count1) {
        e_lsr_i(20, 16, W - 1, ssf); // x20 = result MSB
        if (rcr) {
            e_lsr_i(19, 16, W - 2, ssf);
            e_rrr(A_EOR, 22, 20, 19, ssf, 0);
        } else
            e_rrr(A_EOR, 22, 21, 20, ssf, 0);
        e_rrr(A_AND, 22, 22, 23, 0, 0); // x22 = OF (0/1)  (x23 still == 1)
    }
    rm_store(I, w, 16);
    // Write back CF (stored C = NOT newCF) and, for a 1-bit rotate, OF (V); preserve N/Z (and V otherwise).
    e_ldr(20, 28, OFF_NZCV);
    e_movconst(19, 1u << 29);
    e_rrr(A_BIC, 20, 20, 19, 1, 0);  // clear stored C
    e_rrr(A_EOR, 19, 21, 23, 0, 0);  // x19 = NOT newCF
    e_rrr(A_ORR, 20, 20, 19, 1, 29); // stored C = (NOT newCF) << 29
    if (count1) {
        e_movconst(19, 1u << 28);
        e_rrr(A_BIC, 20, 20, 19, 1, 0);  // clear V
        e_rrr(A_ORR, 20, 20, 22, 1, 28); // V = OF
    }
    e_str(20, 28, OFF_NZCV);
    emit32(0xD51B4200u | 20); // sync live ARM nzcv
}

// Lazy flags (x86-perf PR1 + opt3): pending-finalizer record. Translate-time only -- no guest
// state, never exists at runtime. A width-4/8 do_alu producer *defers* its NZCV materialization:
// the LIVE ARM NZCV currently holds that op's result flags, and g_fl_pending names which finalizer
// would spill them to cpu->nzcv in the canonical borrow convention (== exactly the bytes the inline
// finalizer would have emitted, and what x86cc_to_arm() assumes). Consumed live only by an
// *immediately following* Jcc; any other instruction or block boundary materializes it to membank
// and clears it -- so the cross-block cpu->nzcv ABI is byte-identical. Reset per block.
//   FL_SUB   -> e_nzcv_save     (sub/cmp: ARM SUBS already canonical; PR1 baseline path)
//   FL_ADD   -> e_nzcv_save_ci  (x86 add: invert ARM add-carry)
//   FL_LOGIC -> e_nzcv_save_c1  (and/or/xor/test: x86 CF=0,OF=0)
enum { FL_NONE, FL_SUB, FL_ADD, FL_LOGIC };

static int g_fl_pending;

// PF/AF dead-flag elimination: 1 iff the CURRENT instruction's x86 PF (parity) and AF (aux-carry)
// substrate is provably DEAD -- the immediately-following instruction fully overwrites BOTH PF and AF
// while reading neither, so no consumer (lahf/pushfq/jp/jnp/setp/setnp/cmovp/fcmovu/sahf/popfq) can ever
// observe this op's PF/AF before it is clobbered. Set once per instruction in the translate loop from a
// one-step lookahead (mirrors the NZCV insn_is_flagkill scheme); the PF/AF emitters (e_pf_save /
// e_af_addsub, and the gated e_af_save call sites) no-op when it is set. Unlike NZCV there is nothing to
// "materialize later": when NOT dead we emit eagerly right here, so a stale-true value must never leak to
// a non-producer -- it is reset to 0 every iteration and only raised for a genuine PF/AF producer.
// Reset per iteration; only the PF/AF-producing families raise it. Never consulted by the sahf/popfq
// materializers (they call e_af_save directly, ungated). No EFLAGS snapshot in C (sigframe
// nzcv_to_eflags, ptrace, core) reads cpu->pf/cpu->af, so block boundaries are not PF/AF consumers.
static int g_pfaf_dead;

// x86 direction flag (DF). The AUTHORITATIVE copy is now the RUNTIME bit cpu->df (OFF_DF), maintained by
// cld/std/popfq and read at runtime by pushfq and the string-op lowering -- so a `std` (or popfq-set DF)
// whose `rep movs/stos/scas` lands in a LATER block honors the backward direction (previously it silently
// ran forward). g_df additionally tracks the STATICALLY-known value within the current block for codegen:
// DF_FWD/DF_BWD emit a constant +w/-w stride (fast, no cpu->df load); DF_DYN means "unknown at translate
// time" (block entry, or after popfq) so the lowering loads cpu->df and picks the stride at runtime.
enum { DF_FWD = 0, DF_BWD = 1, DF_DYN = 2 };
static int g_df; // one of DF_FWD/DF_BWD/DF_DYN; the runtime truth is cpu->df

// opt3 kill-switch: NOLAZY=1 (any non-"0") reverts to the PR1 partial scheme (only sub/cmp defers;
// add/logical materialize inline; no dead-flag elimination). Read once, cached.
static int lazyflags_on(void) {
    static int v = -1;
    if (v < 0) {
        const char *s = getenv("NOLAZY");
        v = (s && *s && *s != '0') ? 0 : 1;
    }
    return v;
}

// Direct-write ALU dst: when an ALU (or group1) instruction's r/m operand is a REGISTER (not memory)
// at width>=4, compute the result straight into the guest reg's host home instead of into scratch x16
// followed by a store-back `mov guest,x16`. do_alu already writes any dst (including a guest x0..x15,
// as the dst==reg forms do) and computes PF/AF from the pristine a,b BEFORE overwriting `out`, so
// out==a is byte-identical to out==x16 + rm_store — one fewer instruction on the dependent chain.
// Gate NOXALUDIRECT=1 for A/B (elide-on default). Independent of the flag levers.
static int xaludirect_on(void) {
    static int v = -1;
    if (v < 0) {
        const char *s = getenv("NOXALUDIRECT");
        v = (s && *s && *s != '0') ? 0 : 1;
    }
    return v;
}

// Direct-write SHIFT dst (follow-on to the ALU residency above): when an IMMEDIATE/by-1
// SHL/SHR/SAR's r/m operand is a REGISTER at width>=4, shift straight into the guest reg's host home
// (raw == I->rm_reg from rm_load) instead of copying raw->x16, shifting x16, then storing x16 back.
// The want_cf save (`mov x19,src`) still runs BEFORE the in-place shift when CF/OF are materialized,
// so the exact-CF path sees the pristine operand; every flag read of the result switches x16->the
// guest home. rm_store(...,rmreg) is already a no-op (val==I->rm_reg), so gate-OFF is byte-identical.
// Memory / byte / word / CL-variable / rotate / RCL-RCR keep the x16+store-back path untouched.
// Gate NOXSHIFTDIRECT=1 for A/B (elide-on default). Independent of the flag-elision lever.
static int xshiftdirect_on(void) {
    static int v = -1;
    if (v < 0) {
        const char *s = getenv("NOXSHIFTDIRECT");
        v = (s && *s && *s != '0') ? 0 : 1;
    }
    return v;
}

// Spill the deferred flags to cpu->nzcv with the producer-correct finalizer (byte-identical to the
// old inline finalizer) and clear the pending state. Every finalizer also msr's the corrected value
// back, so the live ARM NZCV is left canonical for an immediately-following Jcc to branch off.
static void flags_materialize(void) {
    switch (g_fl_pending) {
    case FL_SUB: e_nzcv_save(); break;
    case FL_ADD: e_nzcv_save_ci(); break;
    case FL_LOGIC: e_nzcv_save_c1(); break;
    default: break;
    }
    g_fl_pending = FL_NONE;
}

// PUSHFQ/POPFQ flag shuffling: OR the single bit at position `sp` of x[src] into x[dst] at
// position `dp`, via a scratch reg `tmp`. (ubfx wtmp,wsrc,#sp,#1 ; orr xdst,xdst,xtmp,lsl #dp)
static void e_bit_move(int dst, int src, int sp, int dp, int tmp) {
    emit32(0x53000000u | (sp << 16) | (sp << 10) | (src << 5) | tmp); // ubfx wtmp,wsrc,#sp,#1
    e_rrr(A_ORR, dst, dst, tmp, 0, dp);                               // orr xdst,xdst,xtmp,lsl #dp
}

// opt3 dead-flag elimination: 1 iff I's handler provably writes the FULL NZCV while reading no
// flags -- so a pending producer's flags are dead (overwritten before any read) and need not be
// materialized at all. Conservative whitelist: add/or/and/sub/xor/cmp/test/neg only. EXCLUDES
// adc/sbb (read CF), inc/dec (preserve CF), shifts, mul/div, not (flags untouched) -> default 0.
static int insn_is_flagkill(const struct insn *I) {
    if (I->two) return 0;
    uint8_t op = I->op;
    // primary ALU 00..3D (reg/rm + AL/imm forms): kinds add/or/and/sub/xor/cmp, not adc(2)/sbb(3)
    if (op < 0x40 && alu_kind_primary(op) >= 0) {
        int k = alu_kind_primary(op);
        return (k != 2 && k != 3);
    }
    // group1 (80/81/83): ALU r/m, imm
    if (op == 0x80 || op == 0x81 || op == 0x83) {
        int k = I->reg & 7;
        return (k != 2 && k != 3);
    }
    if (op == 0x84 || op == 0x85 || op == 0xA8 || op == 0xA9) return 1; // test
    if (op == 0xF6 || op == 0xF7) {                                     // group3
        int k = I->reg & 7;
        return (k == 0 || k == 3); // /0 test, /3 neg (full NZCV overwrite, read nothing)
    }
    return 0;
}

// opt3 carry-value consumer (adc/sbb): 1 iff I reaches do_alu kind 2/3 with width>=4 -- the forms that
// can pull their x86 CF carry-in straight from an immediately-preceding deferred producer's LIVE NZCV
// (so the main loop must NOT eagerly materialize the pending flags before it; do_alu consumes them).
// Byte/word adc/sbb (report_unimpl) and every non-adc/sbb op return 0 -> normal materialize path.
static int insn_is_carry_consumer(const struct insn *I) {
    if (I->two) return 0;
    uint8_t op = I->op;
    // primary reg/rm forms 10/11/12/13 (adc) 18/19/1A/1B (sbb): width>=4 needs (op&1) && opsize>=4
    if (op < 0x40 && (op & 7) <= 3 && alu_kind_primary(op) >= 0) {
        int k = alu_kind_primary(op);
        return (k == 2 || k == 3) && (op & 1) && I->opsize >= 4;
    }
    // imm-to-acc 15 (adc eax,imm) 1D (sbb eax,imm): (op&7)==5 is the word/dword form
    if (op < 0x40 && (op & 7) == 5 && alu_kind_primary(op) >= 0) {
        int k = alu_kind_primary(op);
        return (k == 2 || k == 3) && I->opsize >= 4;
    }
    // group1 81/83 (/2 adc, /3 sbb); 80 is byte-only -> not a carry consumer here
    if (op == 0x81 || op == 0x83) {
        int k = I->reg & 7;
        return (k == 2 || k == 3) && I->opsize >= 4;
    }
    return 0;
}

// kill-switch: NOPFAFELIM=1 (any non-"0") disables PF/AF dead-flag elimination -> revert to the
// always-eager PF/AF substrate (every ALU op materializes cpu->pf/cpu->af). Read once, cached.
static int pfaf_elim_on(void) {
    static int v = -1;
    if (v < 0) {
        const char *s = getenv("NOPFAFELIM");
        v = (s && *s && *s != '0') ? 0 : 1;
    }
    return v;
}

// 1 iff I's handler EMITS the PF/AF substrate (so the translate loop knows a lookahead is worth
// doing -- and, crucially, that I falls through to a real successor at `next`, making the lookahead
// decode memory-safe). PF/AF producers: primary ALU 00..3D (incl adc/sbb), group1 80/81/83, test,
// inc/dec (FE/FF /0/1), group3 test/neg (F6/F7 /0/3), and shifts C0/C1/D0..D3 (which set PF). mul/div
// (F6/F7 /4..7) leave PF/AF x86-undefined and store nothing -> excluded.
static int insn_writes_pfaf(const struct insn *I) {
    if (I->two) return 0;
    uint8_t op = I->op;
    if (op < 0x40 && alu_kind_primary(op) >= 0) return 1;
    if (op == 0x80 || op == 0x81 || op == 0x83) return 1;
    if (op == 0x84 || op == 0x85 || op == 0xA8 || op == 0xA9) return 1; // test
    if (op == 0xFE || op == 0xFF) {
        int k = I->reg & 7;
        return (k == 0 || k == 1);
    }
    if (op == 0xF6 || op == 0xF7) {
        int k = I->reg & 7;
        return (k == 0 || k == 3);
    }
    if (op == 0xC0 || op == 0xC1 || (op >= 0xD0 && op <= 0xD3)) return 1; // shifts set PF
    return 0;
}

// 1 iff I DEFINITELY overwrites BOTH x86 PF and AF and reads NEITHER -- so a preceding producer's
// PF/AF are dead (clobbered before any consumer could read them). Sound under-approximation: any op not
// on this list (readers jp/jnp/setp/lahf/pushfq/cmovp/fcmovu; mul/div & shifts which leave AF/PF x86-
// undefined; `not`/mov/branch/call/string ops; every two-byte op; unknown opcodes) returns 0 -> the
// producer materializes (the always-correct direction). The set is the ALU/inc-dec/neg/test family:
// every one writes PF and AF as defined outputs, and none reads PF or AF (adc/sbb read CF, not PF/AF).
static int insn_kills_pfaf(const struct insn *I) {
    if (I->two) return 0;
    uint8_t op = I->op;
    if (op < 0x40 && alu_kind_primary(op) >= 0) return 1;               // add/or/adc/sbb/and/sub/xor/cmp
    if (op == 0x80 || op == 0x81 || op == 0x83) return 1;               // group1 ALU r/m,imm (all 8 forms)
    if (op == 0x84 || op == 0x85 || op == 0xA8 || op == 0xA9) return 1; // test
    if (op == 0xFE || op == 0xFF) {
        int k = I->reg & 7;
        return (k == 0 || k == 1);
    } // inc/dec
    if (op == 0xF6 || op == 0xF7) {
        int k = I->reg & 7;
        return (k == 0 || k == 3);
    } // group3 test /0, neg /3
    return 0; // NOT mul/div (undefined), NOT shifts (AF undefined), NOT `not` (untouched), NOT readers
}

// opt3 carry-flow: adjust ONLY the C bit of the LIVE ARM NZCV in place (no cpu->nzcv round-trip), so an
// adc/sbb can read its x86 CF carry-in directly from a deferred producer's live flags. `alu_base` selects
// the bit op on bit 29 (C): A_EOR flips it, A_BIC clears it, A_ORR sets it. Scratch x20/x22 match the
// e_nzcv_* convention (callee-saved, never an x86 guest reg x0..x15 nor a do_alu operand reg).
static void e_nzcv_C_op(uint32_t alu_base) {
    emit32(0xD53B4200u | 20);          // mrs x20, nzcv
    e_movconst(22, 1u << 29);          // C is bit 29 of nzcv
    e_rrr(alu_base, 20, 20, 22, 1, 0); // x20 = x20 <op> (1<<29)   (EOR=flip / BIC=clear / ORR=set)
    emit32(0xD51B4200u | 20);          // msr nzcv, x20
}

// Stash the x86 PF source: the low byte of an integer op's result (the consumer computes even-parity).
// A non-flag str -> leaves the live ARM NZCV untouched (safe to interleave with the lazy-flag path).
static void e_pf_save(int reg) {
    if (g_pfaf_dead) return; // PF dead (next insn overwrites it) -- skip the store entirely
    e_str(reg, 28, OFF_PF);
}

// x86 AF (auxiliary carry) substrate. `reg` must hold a value whose BIT 4 is the carry out of bit 3:
// for add/sub/adc/sbb/cmp that is (a ^ b ^ result); for inc/dec, (a ^ result) (the +/-1 operand only
// flips bit 0, never bit 4). Logical ops store xzr (AF=0, matching qemu's CC_OP_LOGIC). The consumers
// lahf/pushfq extract bit 4; popfq/sahf restore it.
static void e_af_save(int reg) {
    e_str(reg, 28, OFF_AF);
}

// Compute x86 AF for an add/sub-class op: store (a ^ b ^ result) -- its bit 4 is the carry out of bit 3.
// `tmp` is a scratch reg (clobbered). Read a/b/res before they may be reused (they are value regs).
static void e_af_addsub(int a, int b, int res, int tmp) {
    if (g_pfaf_dead) return; // AF dead (next insn overwrites it) -- skip the compute+store entirely
    e_rrr(A_EOR, tmp, a, b, 0, 0);
    e_rrr(A_EOR, tmp, tmp, res, 0, 0);
    e_af_save(tmp);
}

// Width-correct ALU: dst = a <kind> b, set cpu->nzcv.  dst<0 => cmp/test (no write).
// 4/8-byte: direct ARM op. 1/2-byte: operate in the HIGH bits (<<sh) so ARM NZCV matches
// x86 byte/word flags exactly, then merge the low w bytes back (preserving upper bits).
static void do_alu(int kind, int dst, int a, int b, int w) {
    int sf = w == 8, out = dst < 0 ? 31 : dst;
    int ak = kind == 7 ? 5 : kind; // cmp == sub(discard); test == and(discard)
    if (kind == 7) ak = 5;
    if (kind == 2 || kind == 3) { // ADC / SBB -- carry-VALUE consumers (opt3 lazy carry-flow)
        // ARM ADCS computes a+b+C, SBCS computes a-b-(NOT C). x86 ADC/SBB use x86 CF directly.
        // Borrow convention: cpu->nzcv stores ARM C = NOT x86 CF. Hence the required LIVE ARM C is:
        //   ADC -> C = x86 CF        SBB -> C = NOT x86 CF (so SBCS' -(NOT C) = -CF).
        // The op's OWN result is itself deferrable: after ADCS, live C = x86 carry-out, so the canonical
        // spill is the FL_ADD finalizer (e_nzcv_save_ci, flip-C); after SBCS, live C is already the borrow
        // convention, so it is the FL_SUB finalizer (e_nzcv_save). FL_ADC/FL_SBB therefore FOLD into
        // FL_ADD/FL_SUB with bit-identical finalizer bytes, and every downstream Jcc/boundary/SETcc
        // consumer handles them unchanged.
        int adc = (kind == 2);
        uint32_t opc = adc ? 0x3A000000u : 0x7A000000u; // adcs / sbcs
        if (lazyflags_on() && g_fl_pending != FL_NONE) {
            // Carry-in is derivable from the deferred producer's LIVE NZCV with a single C-bit fixup --
            // no cpu->nzcv load/store. Producer live ARM C: FL_SUB -> NOT CF, FL_ADD -> CF, FL_LOGIC ->
            // (x86 CF forced to 0, since AND/OR/XOR/TEST clear CF). An adc;adc;… / sbb;sbb;… bignum chain
            // thus stays in registers with the host carry flowing, never touching cpu->nzcv per step.
            switch (g_fl_pending) {
            case FL_SUB:
                if (adc) e_nzcv_C_op(A_EOR); /* NOT CF -> CF; SBB needs NOT CF already */
                break;
            case FL_ADD:
                if (!adc) e_nzcv_C_op(A_EOR); /* CF ok for ADC; SBB needs NOT CF */
                break;
            case FL_LOGIC:
                e_nzcv_C_op(adc ? A_BIC : A_ORR); /* x86 CF=0: ADC C=0, SBB C=1 */
                break;
            default: break;
            }
            e_rrr(A_EOR, 23, a, b, 0, 0);         // x23 = a ^ b, captured BEFORE the op (out aliases a;
            e_rrr(opc, out, a, b, sf, 0);         //   x23 is never an operand reg, unlike x19=imm)
            e_pf_save(out);                       // x86 PF source = result low byte (incl. carry)
            e_rrr(A_EOR, 23, 23, out, 0, 0);      // x23 = a ^ b ^ result -> bit 4 is x86 AF
            if (!g_pfaf_dead) e_af_save(23);      // skip when AF dead
            g_fl_pending = adc ? FL_ADD : FL_SUB; // defer own flags (FL_ADC==FL_ADD, FL_SBB==FL_SUB)
            return;
        }
        // No live producer (FL_NONE) under lazy, OR NOLAZY: carry-in from cpu->nzcv (membank).
        if (adc)
            e_nzcv_load_ci(); // live ARM C = x86 CF
        else
            e_nzcv_load();               // live ARM C = stored borrow (= NOT x86 CF)
        e_rrr(A_EOR, 23, a, b, 0, 0);    // x23 = a ^ b, captured BEFORE the op (out aliases a; x23 is never
        e_rrr(opc, out, a, b, sf, 0);    //   an operand reg, unlike x19=imm)
        e_pf_save(out);                  // x86 PF source = result low byte (incl. carry)
        e_rrr(A_EOR, 23, 23, out, 0, 0); // x23 = a ^ b ^ result -> bit 4 is x86 AF
        if (!g_pfaf_dead) e_af_save(23); // skip when AF dead
        if (lazyflags_on())
            g_fl_pending = adc ? FL_ADD : FL_SUB; // keep the chain alive: defer (same finalizer bytes)
        else if (adc)
            e_nzcv_save_ci(); // NOLAZY: exact pre-opt3 inline path (spill to membank)
        else
            e_nzcv_save();
        return;
    }
    int logical = (kind == 1 || kind == 4 || kind == 6); // or/and/xor (and test): x86 clears CF
    // x86 PF: stash the result's low byte (computed from pristine a,b before alu_core may overwrite `out`).
    // PF depends only on the low 8 bits, so a non-flag, non-width-extended op gives the right source byte.
    // when g_pfaf_dead the whole PF+AF substrate (parity source op, AF xors, both stores) is skipped.
    if (!g_pfaf_dead) {
        uint32_t pfop = (kind == 0) ? A_ADD : (kind == 1) ? A_ORR : (kind == 6) ? A_EOR : (kind == 4) ? A_AND : A_SUB;
        e_rrr(pfop, 25, a, b, 0, 0);
        e_pf_save(25);
        // x86 AF: add/sub/cmp -> bit 4 of (a ^ b ^ result); logical (and/or/xor/test) leave AF
        // undefined, store 0 (matches qemu CC_OP_LOGIC). x25 already holds the (low) result.
        if (logical) {
            if (!g_pfaf_dead) e_af_save(31); // skip when AF dead (logical AF=0)
        } else
            e_af_addsub(a, b, 25, 26);
    }
    if (w >= 4) {
        alu_core(ak, out, a, b, sf);
        // opt3: defer the NZCV materialization (record which finalizer would spill it). The live ARM
        // NZCV holds the result flags; an immediately-following Jcc branches off them directly and any
        // other consumer/boundary calls flags_materialize() -- emitting the exact same finalizer bytes.
        // Sub/cmp always defers (the PR1 baseline path). Under NOLAZY, add/logical materialize inline
        // (exactly the pre-opt3 behavior) so only sub/cmp stays deferred.
        int lazy = lazyflags_on();
        if (kind == 0) {
            if (lazy)
                g_fl_pending = FL_ADD;
            else
                e_nzcv_save_ci();
        } else if (logical) {
            if (lazy)
                g_fl_pending = FL_LOGIC;
            else
                e_nzcv_save_c1();
        } else {
            g_fl_pending = FL_SUB;
        }
        return;
    }
    int sh = 8 * (4 - w);                       // 24 for byte, 16 for word
    e_lsl_i(21, a, sh, 0);                      // x21 = a << sh
    e_lsl_i(22, b, sh, 0);                      // x22 = b << sh
    alu_core(ak, dst < 0 ? 31 : 21, 21, 22, 0); // op in high bits -> correct NZCV
    if (kind == 0)
        e_nzcv_save_ci();
    else if (logical)
        e_nzcv_save_c1();
    else
        e_nzcv_save();
    if (dst >= 0) {
        e_lsr_i(21, 21, sh, 0);
        e_bfi(dst, 21, 0, 8 * w, 1);
    } // merge low w bytes
}

// Byte/word ADC/SBB. do_alu only handles width>=4 (ARM ADCS/SBCS); ARM has no narrow add-with-carry, and
// the high-bit trick can't inject the carry at the byte's LSB. So compute the masked result + the EXACT
// x86 CF/OF/SF/ZF explicitly, then store the borrow-convention NZCV via e_nzcv_save_setcf. `dst`>=0 gets
// the low w bytes merged (bfi); a/b are value regs. Scratch x19..x27 (callee-saved host regs the
// trampoline preserves; never a guest x0..x15, the value x16, or the EA x17 -- so a mem dest still works).
static void narrow_adcsbb(int adc, int dst, int a, int b, int w) {
    int bits = 8 * w;
    e_uxt(21, a, w); // x21 = a & mask  (read operands FIRST -- a/b may alias scratch like x19/x16)
    e_uxt(22, b, w); // x22 = b & mask
    e_movconst(25, 1);
    // x19 = x86 CF (0/1): stored nzcv C (bit29) is the BORROW (= NOT x86 CF), so x86CF = NOT bit29.
    e_ldr(19, 28, OFF_NZCV);
    e_lsr_i(19, 19, 29, 1);
    e_rrr(A_AND, 19, 19, 25, 0, 0);
    e_rrr(A_EOR, 19, 19, 25, 0, 0);
    if (adc) {
        e_rrr(A_ADD, 23, 21, 22, 0, 0);
        e_rrr(A_ADD, 23, 23, 19, 0, 0); // x23 = a8 + b8 + cf
    } else {
        e_rrr(A_SUB, 23, 21, 22, 0, 0);
        e_rrr(A_SUB, 23, 23, 19, 0, 0); // x23 = a8 - b8 - cf (negative -> bits>=`bits` set = borrow)
    }
    e_uxt(24, 23, w);         // x24 = result (low w bytes)
    e_lsr_i(20, 23, bits, 0); // new x86 CF / borrow = bit `bits` of the wide result
    e_rrr(A_AND, 20, 20, 25, 0, 0);
    // OF: add = ((a^res)&(b^res))msb ; sub = ((a^b)&(a^res))msb
    if (adc) {
        e_rrr(A_EOR, 26, 21, 24, 0, 0);
        e_rrr(A_EOR, 27, 22, 24, 0, 0);
    } else {
        e_rrr(A_EOR, 26, 21, 22, 0, 0);
        e_rrr(A_EOR, 27, 21, 24, 0, 0);
    }
    e_rrr(A_AND, 26, 26, 27, 0, 0);
    e_lsr_i(26, 26, bits - 1, 0);
    e_rrr(A_AND, 26, 26, 25, 0, 0); // x26 = OF (0/1)
    e_lsr_i(27, 24, bits - 1, 0);
    e_rrr(A_AND, 27, 27, 25, 0, 0); // x27 = SF (0/1)
    e_rrr(A_SUBS, 31, 24, 31, 0, 0);
    e_cset(23, 0 /*EQ*/, 0); // x23 = ZF
    e_lsl_i(27, 27, 31, 1);
    e_lsl_i(23, 23, 30, 1);
    e_lsl_i(26, 26, 28, 1);
    e_rrr(A_ORR, 27, 27, 23, 1, 0);
    e_rrr(A_ORR, 27, 27, 26, 1, 0);
    emit32(0xD51B4200u | 27);                 // msr nzcv, x27  (live N/Z/V)
    e_pf_save(24);                            // x86 PF source = result low byte
    e_af_addsub(21, 22, 24, 19);              // x86 AF = bit 4 of (a ^ b ^ result)  (x19 free here)
    if (dst >= 0) e_bfi(dst, 24, 0, bits, 1); // merge low w bytes into dst
    // CF store LAST: e_nzcv_save_setcf clobbers x20/x22/x23, so it must run after AF reads b (x22) and
    // after the result merge; the carry-VALUE lives in x20 and is captured up front by the helper.
    e_nzcv_save_setcf(20); // store N/Z/V, set stored C = NOT new-CF
}

// LOCK-prefixed read-modify-write to a memory operand, done ATOMICALLY via an LSE op (x17 = EA already
// computed). `rs` is the operand value register. `k` is the alu kind (0 add, 1 or, 4 and, 5 sub, 6 xor).
// x86 flags are set from (old OP operand); x19/x20 are scratch. Returns 1 if it emitted an atomic, 0 if
// `k` has no atomic form here (caller falls back to the non-atomic load-op-store).
static int lock_rmw(int k, int w, int rs) {
    int sf = (w == 8);
    uint32_t lse;
    int rsu = rs;
    switch (k) {
    case 0: lse = LSE_LDADD; break;
    case 5:
        e_rrr(A_SUB, 20, 31, rs, sf, 0);
        rsu = 20;
        lse = LSE_LDADD;
        break;                      // sub: atomic add(-v)
    case 1: lse = LSE_LDSET; break; // or
    case 6: lse = LSE_LDEOR; break; // xor
    case 4:
        e_rrr(A_ORN, 20, 31, rs, sf, 0);
        rsu = 20;
        lse = LSE_LDCLR;
        break; // and: clear ~v
    default: return 0;
    }
    e_lse(lse, w, rsu, 19, 17); // x19 = old; [x17] op= rsu  (acquire-release)
    do_alu(k, -1, 19, rs, w);   // x86 flags from (old OP original-operand)
    return 1;
}

// x86 condition (opcode low nibble) -> ARM cond, or -1 if unsupported (parity).
static int x86cc_to_arm(int cc) {
    // Parity (idx 10/11, jp/jnp/setp/.../cmovp/...) is NOT routed here -- it reads the real PF lane
    // (cpu->pf) via e_pf_compute, so its slots below (mapping onto ARM V) are dead. Everything else is
    // a direct NZCV condition.
    static const int t[16] = {6, 7, 3, 2, 0, 1, 9, 8, 4, 5, 6, 7, 11, 10, 13, 12};
    return t[cc & 0xF];
}

// jp/jnp (parity jcc): spill any deferred flags to membank (this is a block boundary for the
// successor blocks), then compute the real x86 PF lane into the live ARM Z flag and return the ARM
// condition the branch machinery should test. Mirrors setp/setnp + cmovp/cmovnp, which already read
// cpu->pf instead of the stale ARM V flag. `lo` is the opcode low nibble (0xA=jp, 0xB=jnp).
// NOTE (parity-edge fix): the SUBS below CLOBBERS the live ARM NZCV with parity scratch. The jcc
// handlers MUST restore the canonical flags (e_nzcv_load from the just-materialized membank) on
// EVERY outgoing edge after the b.cond -- otherwise the edge's exit spill (emit_spill ->
// e_nzcv_save) persists the scratch NZCV over cpu->nzcv and the successor blocks read corrupted
// CF/ZF/SF/OF (caught by the comp-x86-misc/parity-edge differential: cmp; jp; jb diverged).
static int emit_parity_jcc_cond(int lo) {
    if (g_fl_pending) flags_materialize(); // spill the deferred producer to membank (boundary)
    e_pf_compute(19);                      // x19 = x86 PF in {0,1} (scratch x16; x17/EA preserved)
    e_rrr(A_SUBS, 31, 19, 31, 0, 0);       // live ARM Z = (PF == 0)
    return (lo == 0xA) ? 1 /*NE: PF==1*/ : 0 /*EQ: PF==0*/;
}

#include "translate/trace.c"

#include "translate/repstr.c"

#include "translate/x87.c"

#include "translate/alu.c"

#include "translate/mov.c"

#include "translate/shift.c"

#include "translate/crypto.c"

#include "translate/sse4x.c"

// SSE2 variable-count packed shift (PSLLW/D/Q, PSRLW/D/Q, PSRAW/D by xmm/m): shift every
// `esize`-bit lane of `vn` by the SCALAR count held in the low 64 bits of `vs`, result -> `vd`.
// x86 saturates the count: any count >= esize yields 0 (logical) or the sign bit replicated
// (arithmetic right). NEON USHL/SSHL take a per-lane signed amount from the low byte of each
// lane, so we clamp the (unsigned) count to esize -- which is < 128, keeping the signed byte
// valid -- and DUP it across all lanes (negated for a right shift).
static void e_sse_var_shift(int vd, int vn, int vs, int esize, int left, int arith) {
    uint32_t sz = esize == 16 ? 1u : esize == 32 ? 2u : 3u; // NEON element size field
    uint32_t imm5 = esize == 16 ? 2u : esize == 32 ? 4u : 8u;
    emit32(0x4E083C00u | (vs << 5) | 16); // umov x16, vs.d[0]   (the 64-bit count)
    e_movconst(19, esize);
    e_rrr(A_SUBS, 31, 16, 19, 1, 0);                                 // cmp x16, esize
    e_csel(16, 19, 16, 8 /*HI*/, 1);                                 // x16 = (count u> esize) ? esize : count
    if (!left) e_rrr(A_SUB, 16, 31, 16, 1, 0);                       // right shift -> negative NEON amount (neg x16)
    emit32(0x4E000C00u | (imm5 << 16) | (16 << 5) | 17);             // dup v17.<T>, w16/x16
    uint32_t shl = (arith ? 0x4E204400u : 0x6E204400u) | (sz << 22); // SSHL (arith) / USHL
    emit32(shl | (17 << 16) | (vn << 5) | vd);                       // [s|u]shl vd, vn, v17
}

// x86 default/indefinite-NaN sign fixup for the inline SSE FP arithmetic (add/sub/mul/div/sqrt).
// When such an op GENERATES a NaN with NO NaN input (0/0, inf/inf, 0*inf, inf-inf, sqrt(-1)), x86
// yields the QNaN floating-point INDEFINITE whose SIGN BIT IS SET: single 0xFFC00000, double
// 0xFFF8000000000000. ARM's FDIV/FADD/FSUB/FMUL/FSQRT instead produce the DEFAULT NaN with sign CLEAR
// (0x7FC00000 / 0x7FF8000000000000) -- identical payload, opposite sign. A NaN PROPAGATED from an input
// keeps that input's sign on BOTH ISAs, so we must fix up ONLY generated default-NaNs, identified as
// "result is NaN AND no input is NaN". Branchless: v20/v21 scratch, per-lane so scalar and packed share
// one path (scalar upper lanes are 0.0 in the result -> never flagged). Set NOXFPDNAN to disable (A/B).
static int g_fpdnan = -1;
static int fpdnan_on(void) {
    if (g_fpdnan < 0) g_fpdnan = (getenv("NOXFPDNAN") == NULL);
    return g_fpdnan;
}
// PRE (emit BEFORE the arithmetic, while vd still holds src1): v20 <- "no input is NaN" lane mask.
// two_in: 1 for add/sub/mul/div (src1=vd, src2=s); 0 for sqrt (single operand s, vd is not an input).
static void emit_dnan_pre(int vd, int s, int two_in, int dbl) {
    uint32_t EQ = dbl ? 0x4E60E400u : 0x4E20E400u; // FCMEQ Vd.2d/.4s (all-ones per lane where NOT NaN)
    if (two_in) {
        emit32(EQ | (vd << 16) | (vd << 5) | 20); // v20 = (src1 == src1)
        emit32(EQ | (s << 16) | (s << 5) | 21);   // v21 = (src2 == src2)
        e_v3(0x4E201C00u, 20, 20, 21);            // v20 = in_notnan = src1nn & src2nn  (AND.16b)
    } else {
        emit32(EQ | (s << 16) | (s << 5) | 20); // v20 = (src == src)
    }
}
// POST (emit AFTER the arithmetic; vd = result): OR the x86 indefinite sign into lanes that are a
// GENERATED default NaN (result is NaN AND no input was NaN). Payload already matches x86, so only the
// sign bit must be set.
static void emit_dnan_post(int vd, int dbl) {
    uint32_t EQ = dbl ? 0x4E60E400u : 0x4E20E400u;
    emit32(EQ | (vd << 16) | (vd << 5) | 21); // v21 = (res == res)  (all-ones where result NOT NaN)
    e_v3(0x4E601C00u, 20, 20, 21);            // v20 = gen = in_notnan & ~res_notnan  (BIC.16b)
    if (dbl) {
        e_movconst(16, 0x8000000000000000ull);
        emit32(0x4E080C00u | (16 << 5) | 21); // DUP v21.2d, x16   (per-lane sign const)
    } else {
        e_movconst(16, 0x80000000ull);
        emit32(0x4E040C00u | (16 << 5) | 21); // DUP v21.4s, w16
    }
    e_v3(0x4E201C00u, 21, 21, 20); // v21 = signbits & gen  (AND.16b)
    e_v3(0x4EA01C00u, vd, vd, 21); // vd |= signbits        (ORR.16b)
}

// Deliver a guest trap SIGNAL (int3 -> SIGTRAP, UD2 -> SIGILL) by EXITING the block to the dispatcher with
// R_TRAP, rather than emitting a host BRK/UDF. On Apple Silicon a JIT'd BRK/UDF raises a Mach exception the
// x86 engine does not catch, so the host BSD SIGTRAP/SIGILL never reaches jit86_syncguard and the process
// dies (exit 133/132) instead of running the guest handler. Routing through the dispatcher (raise_guest_trap)
// is the same C-delivery path #DE already uses (raise_guest_de) and is host-trap-independent. lsig/code are
// packed into cpu->divop; emit_exit_const spills guest GPR+xmm and sets cpu->rip = the architectural PC.
static void emit_guest_signal(uint64_t rip, int lsig, int code) {
    if (g_fl_pending) flags_materialize();
    if (g_fp_known) fp_drop();
    e_movconst(16, (uint64_t)((lsig & 0xff) | ((code & 0xff) << 8)));
    e_str(16, 28, OFF_DIVOP); // (linux_signo | si_code<<8) -> cpu->divop for raise_guest_trap
    emit_exit_const(rip, R_TRAP);
}

// MXCSR sticky exception flags <-> ARM FPSR cumulative flags. x86 MXCSR bits 0..5 are IE/DE/ZE/OE/UE/PE
// (invalid/denormal/divide-by-zero/overflow/underflow/precision). ARM FPSR cumulative bits are IOC(0)/
// DZC(1)/OFC(2)/UFC(3)/IXC(4)/IDC(7). The per-bit map (MXCSR bit i <- FPSR bit fpsr_src[i]) is:
//   IE<-IOC(0)  DE<-IDC(7)  ZE<-DZC(1)  OE<-OFC(2)  UE<-UFC(3)  PE<-IXC(4)
// SSE ops execute as host NEON, so the host FPSR already accumulates the real exceptions; stmxcsr/fxsave
// just need to project them into MXCSR bits 0..5 (previously hard-zeroed), and ldmxcsr/fxrstor project a
// loaded MXCSR back so a guest that CLEARS the sticky flags (feclearexcept) actually clears the host FPSR.
static const int g_mxcsr_fpsr_bit[6] = {0, 7, 1, 2, 3, 4};
static void emit_fpsr_to_mxcsr(int dst) { // OR the host FPSR sticky flags into `dst` at MXCSR bits 0..5
    emit32(0xD53B4420u | 22);             // mrs x22, fpsr
    e_movconst(21, 0);                    // accumulator
    e_movconst(19, 1);
    for (int i = 0; i < 6; i++) {
        e_lsr_i(20, 22, g_mxcsr_fpsr_bit[i], 0);
        e_rrr(A_AND, 20, 20, 19, 0, 0);
        e_rrr(A_ORR, 21, 21, 20, 0, i); // x21 |= bit << i
    }
    e_rrr(A_ORR, dst, dst, 21, 0, 0);
}
static void emit_mxcsr_to_fpsr(int src) { // set the host FPSR sticky flags from `src` (MXCSR) bits 0..5
    emit32(0xD53B4420u | 22);             // mrs x22, fpsr
    e_movconst(19, 0x9f);                 // ARM cumulative-flag mask: IOC|DZC|OFC|UFC|IXC|IDC (bits 0-4,7)
    e_rrr(A_BIC, 22, 22, 19, 0, 0);       // clear the existing sticky flags
    e_movconst(19, 1);
    for (int i = 0; i < 6; i++) {
        e_lsr_i(20, src, i, 0);
        e_rrr(A_AND, 20, 20, 19, 0, 0);
        e_rrr(A_ORR, 22, 22, 20, 0, g_mxcsr_fpsr_bit[i]); // FPSR bit |= MXCSR bit i
    }
    emit32(0xD51B4420u | 22); // msr fpsr, x22
}

// x87 fist/fistp round ST0 (already in d16) to an integral double using the CURRENT x87 rounding control
// (cpu->fpcw bits[11:10]), so the caller's FCVTZS then converts it exactly. x86 x87 defaults to round-to-
// NEAREST-even (not toward-zero) and honors fldcw's RC, but the old code emitted a bare FCVTZS (truncate) --
// so fistp(2.7) gave 2 instead of 3, and a round-up/down control word had no effect. x87 has its OWN rounding
// domain, SEPARATE from SSE MXCSR (both share ARM FPCR.RMode), so round under a SAVED/RESTORED FPCR: set
// FPCR.RMode from the x87 RC (same two-bit swap as ldmxcsr), FRINTI, then restore FPCR so SSE rounding is
// untouched. Scratch x20/x21/x22/x23; x19 (the store EA at every caller) is left intact.
static void emit_x87_round_st0(void) {
    e_ldr(20, 28, OFF_FPCW);                                          // w20 = cpu->fpcw
    emit32(0x53000000u | (10u << 16) | (11u << 10) | (20 << 5) | 20); // ubfx w20,w20,#10,#2 -> RC (0..3)
    e_movconst(21, 1);
    e_rrr(A_AND, 22, 20, 21, 0, 0);       // w22 = RC & 1
    e_lsr_i(21, 20, 1, 0);                // w21 = RC >> 1
    e_rrr(A_ORR, 22, 21, 22, 0, 1);       // w22 = (RC>>1) | (RC&1)<<1 = ARM RMode (x87 RC bits swapped)
    emit32(0xD53B4400u | 23);             // mrs x23, fpcr  (save the live -- SSE -- rounding mode)
    e_movconst(21, 3u << 22);             // RMode mask
    e_rrr(A_BIC, 20, 23, 21, 1, 0);       // x20 = fpcr & ~RMode
    e_rrr(A_ORR, 20, 20, 22, 1, 22);      // x20 = | (ARM RMode << 22)
    emit32(0xD51B4400u | 20);             // msr fpcr, x20  (x87 rounding mode)
    emit32(0x1E67C000u | (16 << 5) | 16); // frinti d16, d16 (round to integral per FPCR.RMode)
    emit32(0xD51B4400u | 23);             // msr fpcr, x23  (restore SSE rounding mode)
}

// A JIT guest unmapped / remapped an executable VA range: any block translations we cached for guest PCs in
// that range are now STALE -- the same VA can be re-mapped with DIFFERENT code (JITs, trampolines, dlopen VA
// reuse), and the dispatcher keys cached host code by guest PC, so it would jump to the OLD host code for the
// new bytes. Called from the guest munmap / MAP_FIXED / mremap(MREMAP_FIXED) paths. This is the SAME wholesale
// map/IBTC drop the SMC write-fault path uses (a currently-running block's host code stays intact; orphaned
// translations are reclaimed by the next wholesale flush) -- but ONLY fired when the range actually overlaps a
// write-protected code page (g_smc_pg), so ordinary data munmap/mmap churn pays nothing and re-translates
// nothing. Inert unless a JIT guest is present (g_rwx_guest) -> the normal (non-JIT) matrix is byte-exact.
static void jit86_drop_range_translations(uint64_t lo, uint64_t hi) {
    if (!g_rwx_guest || g_smc_n == 0 || hi <= lo) return;
    uint64_t plo = lo & ~0x3FFFull, phi = (hi + 0x3FFFull) & ~0x3FFFull;
    int hit = 0;
    for (int i = 0; i < g_smc_n;) {
        if (g_smc_pg[i] >= plo && g_smc_pg[i] < phi) { // a translated code page lived in the range
            hit = 1;
            g_smc_pg[i] = g_smc_pg[--g_smc_n]; // forget it -> re-protected when the fresh mapping is translated
        } else {
            i++;
        }
    }
    if (!hit) return; // no translated code in the range -> nothing to invalidate (the common data-munmap case)
    memset(g_map, 0, sizeof g_map);
    memset(g_ibtc, 0, sizeof g_ibtc);
    memset(g_xibtc, 0, sizeof g_xibtc);
    g_npend = 0;
}

// Integer DIV/IDIV by zero raises #DE (SIGFPE) on x86, but ARM sdiv/udiv quietly return 0 -- a guest
// #DE would be silently swallowed. Guard the inline (8/16/32-bit) divides: when the (width-extended)
// divisor in divreg is zero, route to the C div path (R_DIV/R_IDIV in dispatch.c), which reports the
// #DE -- the same exit the 64-bit DIV already uses. The non-zero path falls straight through to the
// inline divide, so normal division is unaffected.
static void emit_div_zero_check(int divreg, uint64_t next, int idiv) {
    uint32_t *patch = (uint32_t *)g_cp;
    emit32(0);                    // cbnz divreg, ok  (divisor != 0): offset patched below
    e_str(divreg, 28, OFF_DIVOP); // divisor (== 0) -> cpu->divop for the C #DE path
    emit_exit_const(next, idiv ? R_IDIV : R_DIV);
    int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
    *patch = 0xB5000000u | (((uint32_t)d & 0x7FFFF) << 5) | (uint32_t)divreg; // cbnz x[divreg], ok
}

// x86 DIV/IDIV also raise #DE when the quotient does not fit the RESULT width (e.g. DIV 0x1FF/1 with an
// 8-bit result, or IDIV INT_MIN/-1). ARM UDIV/SDIV silently truncate, so after an inline (width w<8) divide
// computes the quotient in qreg, trap the overflow: branch over the trap when the quotient is in range,
// else route to the C #DE path (divop=0 -> raise_guest_de delivers SIGFPE/FPE_INTDIV, si_addr = the div
// insn). Cheap: two insns on the in-range fast path (compare/shift + a taken-forward branch).
static void emit_div_ovf_check(int qreg, int tmp, int w, int is_signed, uint64_t gpc, int idiv) {
    uint32_t *br;
    if (is_signed) {
        // The 8/16-bit inline divides use a 32-bit ARM SDIV (sf=0) whose quotient is zero-extended into
        // the upper 32 bits, so normalize to a true 64-bit signed value first; the 32-bit divide is sf=1
        // (already 64-bit signed). Then the quotient fits the result width w iff sxt(q,w) == q.
        if (w == 4)
            e_mov_rr(tmp, qreg, 1);
        else
            e_sxt(tmp, qreg, 4);          // sxtw: 32-bit quotient -> 64-bit signed
        e_sxt(16, tmp, w);                // sign-extend the low w bytes
        e_rrr(A_SUBS, 31, 16, tmp, 1, 0); // cmp: in range iff equal
        br = (uint32_t *)g_cp;
        emit32(0); // b.eq skip  (patched below)
    } else {
        e_lsr_i(tmp, qreg, 8 * w, 1); // any bits above the width -> quotient overflows
        br = (uint32_t *)g_cp;
        emit32(0); // cbz tmp, skip  (patched below)
    }
    e_movconst(16, 0);
    e_str(16, 28, OFF_DIVOP); // divop = 0 -> the C R_DIV/R_IDIV path raises #DE
    emit_exit_const(gpc, idiv ? R_IDIV : R_DIV);
    int64_t d = ((uint8_t *)g_cp - (uint8_t *)br) / 4; // offset from the branch to the skip target
    if (is_signed)
        *br = 0x54000000u | (((uint32_t)d & 0x7FFFF) << 5); // b.eq skip (cond EQ = 0)
    else
        *br = 0xB4000000u | (((uint32_t)d & 0x7FFFF) << 5) | (uint32_t)tmp; // cbz x[tmp], skip
}

// 64-bit DIV/IDIV. ARM has no 128/64 divide, but the compiler-emitted common case is a 64/64 divide
// (`xor edx,edx; div r` or `cqo; idiv r`) whose dividend fits 64 bits -- DIV: RDX==0; IDIV: RDX==
// sign_ext(RAX). Fast-path those with a single hardware UDIV/SDIV (+ MSUB for the remainder), guarded
// by the shared zero-check (divisor==0 -> #DE). The rare true 128/64 case, and IDIV by -1 (which can
// overflow: INT_MIN/-1), route to the C R_DIV/R_IDIV helper, which does the exact 128/64 division and
// raises #DE on quotient overflow. On the fast path we resume inline (no block exit); the slow path
// exits to the dispatcher, which resumes at `next` after computing the division.
static void emit_div64_fast(uint64_t next, uint64_t gpc, int idiv, int rmv) {
    e_mov_rr(23, rmv, 1);               // snapshot divisor (may alias RAX/RDX, which we overwrite below)
    emit_div_zero_check(23, gpc, idiv); // divisor==0 -> #DE(rip=gpc); else fall through (divisor != 0)
    uint32_t *b_slow1, *b_slow2 = 0;
    if (!idiv) { // DIV: fast when RDX==0 (dividend==RAX, quotient always fits)
        b_slow1 = (uint32_t *)g_cp;
        emit32(0);                        // cbnz RDX, Lslow  (RDX!=0 -> true 128/64 in C)
        e_udiv(20, RAX, 23, 1);           // q   = RAX / divisor
        e_msub(21, 20, 23, RAX, 1);       // rem = RAX - q*divisor
    } else {                              // IDIV: fast when RDX==sign_ext(RAX) AND divisor != -1
        e_asr_i(22, RAX, 63, 1);          // x22 = sign extension of RAX
        e_rrr(A_SUBS, 31, RDX, 22, 1, 0); // cmp RDX, x22
        b_slow1 = (uint32_t *)g_cp;
        emit32(0);            // b.ne Lslow  (RDX != sign_ext(RAX): 128-bit dividend)
        e_addi(21, 23, 1, 1); // x21 = divisor + 1
        b_slow2 = (uint32_t *)g_cp;
        emit32(0);                  // cbz x21, Lslow  (divisor == -1: INT_MIN/-1 may overflow)
        e_sdiv(20, RAX, 23, 1);     // q   = RAX / divisor
        e_msub(21, 20, 23, RAX, 1); // rem = RAX - q*divisor
    }
    e_mov_rr(RAX, 20, 1); // RAX = quotient
    e_mov_rr(RDX, 21, 1); // RDX = remainder
    uint32_t *b_done = (uint32_t *)g_cp;
    emit32(0); // b Ldone  (skip the slow exit)
    // ---- Lslow: divisor is nonzero here; C helper does 128/64 exact + quotient-overflow #DE ----
    int64_t d1 = ((uint8_t *)g_cp - (uint8_t *)b_slow1) / 4;
    if (!idiv)
        *b_slow1 = 0xB5000000u | (((uint32_t)d1 & 0x7FFFF) << 5) | (uint32_t)RDX; // cbnz RDX, Lslow
    else
        *b_slow1 = 0x54000000u | (((uint32_t)d1 & 0x7FFFF) << 5) | 0x1; // b.ne Lslow (cond NE = 1)
    if (b_slow2) {
        int64_t d2 = ((uint8_t *)g_cp - (uint8_t *)b_slow2) / 4;
        *b_slow2 = 0xB4000000u | (((uint32_t)d2 & 0x7FFFF) << 5) | 21; // cbz x21, Lslow
    }
    e_str(23, 28, OFF_DIVOP);                     // divisor -> cpu->divop
    emit_exit_const(next, idiv ? R_IDIV : R_DIV); // -> dispatcher (resumes at next after the division)
    // ---- Ldone ----
    int64_t dd = ((uint8_t *)g_cp - (uint8_t *)b_done) / 4;
    *b_done = 0x14000000u | ((uint32_t)dd & 0x3FFFFFF); // b Ldone
}

// 0F 0B (UD2): an explicitly-undefined opcode that real software (e.g. ruby's unreachable/trap paths,
// libc CPU-feature probes) uses as a deliberate trap. On x86 it raises #UD -> SIGILL; with a guest
// handler that runs, otherwise the process dies with status 128+SIGILL = 132. Emit a host UDF so the
// SIGILL guard delivers it to the guest handler (or default-terminates), instead of the old always-
// terminate hack. This is distinct from report_unimpl's "engine aborted" path (status 70), which would
// mislabel a legitimate guest fault as an unimplemented-opcode bug of ours.
static void emit_sigill(uint64_t pc) {
    // Quiet by default: UD2 frequently sits on never-taken paths (compiler trap/unreachable slots) that get
    // translated as block fall-through but never run; an unconditional message would falsely imply delivery.
    if (getenv("CRASHDBG")) fprintf(stderr, "[dd] #UD ud2 at rip=%llx -> SIGILL\n", (unsigned long long)pc);
    emit_guest_signal(pc, 4, 2); // ud2 -> SIGILL (si_code ILL_ILLOPN), rip = the faulting insn
}

// async-interrupt poll: emit a CHEAP flag-free check of cpu->irq at the block body entry (the target
// of every fall-through, direct chain `b body`, self-loop fold, and IBTC hit). When irq is set (a caught
// async guest signal became pending while the guest spins in-cache making no syscalls), exit to the
// dispatcher at a safe boundary -- all guest regs are live in host regs here, so emit_exit_const's spill
// materializes consistent guest state and maybe_deliver_signal builds the sigframe as the syscall path
// does. Fast path is ldr+cbz (2 insns); cbz never touches NZCV, so a self-loop back-edge that lands here
// keeps the guest flags (incl. x86 lazy flags live in NZCV). x16 is engine scratch (dead at body entry),
// so no guest reg is disturbed. `rip` is the block start = the guest pc to resume at.
// IRQSLIM: when active (g_fwdskip == 8, the default) the poll is a FIXED 2-insn header (ldr + cbnz
// to an out-of-line exit stub emitted at the end of the block), so a forward direct chain can land
// at body+8 and skip it -- every in-cache cycle still polls through its backward or indirect edge
// (invariant note in engine/cache.c). NOIRQSLIM=1 -> the legacy inline poll, chains to body+0.
static uint32_t *g_irq_patch;

static void emit_irq_check(uint64_t rip) {
    // NOIRQCHECK=1: MEASUREMENT-ONLY kill switch (quantifies the per-block-entry poll cost).
    // Unsafe for async signal delivery into syscall-free loops -- never default.
    static int noirq = -1;
    if (noirq < 0) noirq = getenv("NOIRQCHECK") != NULL;
    if (noirq) return;
    if (g_fwdskip) {
        e_ldr(16, 28, (int)OFF_IRQ); // ldr x16, [x28(cpu), #irq]
        g_irq_patch = (uint32_t *)g_cp;
        emit32(0); // cbnz x16, Lirq (out-of-line exit stub; patched at end of translate_block)
        return;
    }
    e_ldr(16, 28, (int)OFF_IRQ); // ldr x16, [x28(cpu), #irq]
    uint32_t *p = (uint32_t *)g_cp;
    emit32(0); // cbz x16, Lcont  (patched below)
    emit_exit_const(rip, R_BRANCH);
    uint8_t *cont = g_cp;
    *p = 0xB4000000u | (((uint32_t)(((uint8_t *)cont - (uint8_t *)p) / 4) & 0x7FFFF) << 5) | 16;
}

// Translate the basic block at guest address gpc; returns host entry pointer.
static void *translate_block(uint64_t gpc) {
    uint64_t start = gpc;
    void *host = g_cp;
    emit_prologue();
    void *body = g_cp;
    // poll cpu->irq at the body entry so a caught async signal reaches a no-syscall guest loop.
    emit_irq_check(start);
    g_fl_pending = FL_NONE; // lazy flags: nothing deferred at block entry
    g_v26z = g_v27m = 0;    // crypto constant hoist: no v26==0 / v27==0x8f claim survives a block entry
    g_df = DF_DYN; // DF unknown at block entry (a prior block's std/popfq may have left it set) -> string
    g_fp_known = 0;         // x87: top unknown at block entry until a finit anchors it
    g_fp_dirty = 0;
    g_vmark_done = 0; // fresh region -> first xmm write must re-mark cpu->vdirty
    g_prof_xlate++;   // PROF (measurement-only): translate_block calls
    if (g_stitch < 0) g_stitch = (getenv("NOSTITCH") == NULL);
    // W3-A superblock state: guest block-starts already laid in this region + region budget.
    uint64_t seen[TRACE_MAX_BLK];
    int nseen = 0, trace_blk = 0;
    seen[nseen++] = start;
#define STITCH_OK                                                                                                      \
    (g_stitch && !g_nochain && !g_trace && !g_itrace && trace_blk < TRACE_MAX_BLK - 1 &&                               \
     (size_t)((uint8_t *)g_cp - (uint8_t *)host) < TRACE_MAX_BYTES)
    for (;;) {
        if (g_itrace && gpc != start) {
            if (g_fl_pending) flags_materialize(); // materialize before boundary
            fp_drop();                             // x87: spill the shadow top before the boundary
            emit_chain_exit(gpc);
            break;
        } // 1 insn/block: per-instruction register dump
        struct insn I;
        g_emit_gpc = gpc; // IRQSLIM: tag chain emission with the current branch's rip
        decode(gpc, &I);
        uint64_t next = gpc + I.len;
        uint8_t op = I.op;
        int sf = I.opsize == 8;
        // VEX/EVEX (AVX/AVX2/AVX-512): not lowered to NEON. Exit the block and emulate this single insn in C
        // (do_avx), which owns the full v[]/vhi/vz/vx/kreg register file + memory. rip := gpc so do_avx
        // decodes the insn at rip and advances past it. Done BEFORE the lazy-flag classifier (which only
        // knows legacy opcodes) -- AVX touches no EFLAGS we model, so just spill any pending flags first.
        if (I.vex) {
            // An EVEX prefix (0x62) whose map field mm==0 is a RESERVED/invalid encoding -> x86 raises #UD.
            // (0x62 is also the legacy BOUND opcode, invalid in 64-bit mode -> #UD.) Deliver SIGILL to the
            // guest instead of aborting the engine as "UNIMPLEMENTED EVEX".
            if (I.evex && I.vex_map == 0) {
                emit_guest_signal(gpc, 4, 2); // #UD -> SIGILL (si_code ILL_ILLOPN), rip = the faulting insn
                break;
            }
            if (g_fl_pending) flags_materialize();
            emit_exit_const(gpc, R_AVX);
            break;
        }
        // Legacy (non-VEX) 0F38/0F3A SSSE3/SSE4/AES/SHA/PCLMUL/CRC32/MOVBE: emulate this single insn in C
        // (do_sse3b) -- correctness-first, mirroring the AVX path. These touch no EFLAGS we lazily defer
        // (cmp-string sets flags but writes them through cpu->nzcv in C), so just spill any pending flags.
        if (I.map3) {
            if (g_fl_pending) flags_materialize();
            // map the AES-NI / PCLMULQDQ / SHA-NI (SHA-1 + SHA-256, via the hardware ARM SHA
            // extension) crypto opcodes to inline ARM crypto (near-native); everything else
            // (SSSE3/SSE4/CRC32/MOVBE/aeskeygenassist) still exits to do_sse3b.
            if (translate_crypto(&I, next) == TX_NEXT) {
                // (missed in v0.9.19-as-shipped): the inline crypto/shuffle glue WRITES guest xmm
                // (pshufb->TBL, palignr->EXT, AESENC, PCLMUL, ...) without passing the SSE region's mark, so
                // a following slim R_SYSCALL exit could skip the xmm save with cpu->V stale. Mark here.
                // (Marking after emission is fine: the latch/mark are translate-time; any exit emitted
                // before this point ends the region, and translate_crypto emits no exits on TX_NEXT.)
                mark_vdirty();
                gpc = next;
                continue;
            }
            // perf wave 2: MOVBE/CRC32 + PINSR/PEXTR/INSERTPS + AESKEYGENASSIST lowered inline
            // (translate/sse4x.c) -- the residual per-block exits in openssl's stitched CTR loop.
            if (translate_sse4x(&I, next) == TX_NEXT) {
                gpc = next;
                continue;
            }
            emit_exit_const(gpc, R_SSE3B);
            break;
        }
        if (g_trace)
            fprintf(stderr, "[dec] %llx %s%02x len=%d mod%d rm%d reg%d mem%d base%d idx%d disp=%lld imm=%lld\n",
                    (unsigned long long)gpc, I.two ? "0F " : "", op, I.len, I.mod, I.rm_reg, I.reg, I.is_mem,
                    I.m_hasbase ? I.m_base : -1, I.m_hasindex ? I.m_index : -1, (long long)I.disp, (long long)I.imm);

        // Lazy flags: a pending width-4/8 producer left its result flags live in NZCV instead of
        // spilling to cpu->nzcv. The ONLY consumer allowed to read them live is an immediately-
        // following Jcc (rel8 70-7F / rel32 0F 80-8F). For every other instruction:
        //   - opt3 dead-flag elimination: if this instruction fully overwrites NZCV and reads no
        //     flags (insn_is_flagkill), the pending flags are dead -- drop them, emitting nothing;
        //   - otherwise (a non-Jcc flag reader/consumer or a block-ender): materialize to membank
        //     NOW, before it emits anything, with the exact finalizer the producer would have used.
        // Both keep the cross-block cpu->nzcv ABI byte-identical (intra-block only). NOLAZY disables
        // dead-flag elimination, so the pending sub/cmp always materializes (PR1 behavior).
        int is_jcc = (!I.two && op >= 0x70 && op <= 0x7F) || (I.two && (op & 0xF0) == 0x80);
        // x86-xflags: jmp rel (E9/EB) and call rel (E8) are flag-transparent block enders whose
        // handlers do the edge-aware flag handling themselves (stitch keeps the deferral alive;
        // a chained edge consults the successor's live-in set via flags_edge). Everything else
        // keeps the eager top-of-loop materialization below.
        int is_xedge = xblkflags_on() && !I.two && (op == 0xE9 || op == 0xEB || op == 0xE8);
        if (g_fl_pending && !is_jcc && !is_xedge) {
            int lazy = lazyflags_on();
            if (lazy && insn_is_carry_consumer(&I)) {
                // opt3 carry-flow: leave the producer's flags LIVE; do_alu's adc/sbb pulls its x86 CF
                // carry-in straight from them (FL_SUB/FL_ADD/FL_LOGIC) -- no eager materialize.
            } else if (lazy && insn_is_flagkill(&I))
                g_fl_pending = FL_NONE; // dead: next op fully overwrites the flags before any read
            // x86-xflags: the 1-insn insn_is_flagkill peephole only catches an *immediately* following
            // full-NZCV writer. In real integer chains the producer is separated from the next flag op by
            // value-only movs/immediate-shifts (e.g. an LCG mix: add; mov; shr; xor; ...). Generalize with
            // the same guest-byte liveness scan the shift/edge paths already trust: if this producer's NZCV
            // is provably overwritten before any read across the block (following unconditional jmps), it is
            // dead -- drop it, emitting NOTHING (same as the flagkill path; the live ARM NZCV need not be
            // canonicalized because no consumer observes it). PF/AF are handled independently below.
            else if (lazy && xblkalu_elide_on() && !(x86_flags_livein(gpc, gpc) & XF_NZCV))
                g_fl_pending = FL_NONE;
            else
                flags_materialize();
        }

        // PF/AF dead-flag elimination: decide, one step ahead, whether THIS instruction's PF/AF
        // substrate is dead. It is dead iff I is a PF/AF producer AND the immediately-following insn at
        // `next` fully overwrites both PF and AF while reading neither (insn_kills_pfaf) -- then no
        // consumer can observe I's PF/AF. Gating on insn_writes_pfaf(&I) both scopes the work to
        // producers and guarantees `next` is a real fall-through (producers never terminate a block), so
        // the lookahead decode reads only bytes the successor iteration would decode anyway. Reset every
        // iteration so a stale value never reaches a non-producer; the e_pf_save/e_af_addsub emitters and
        // the do_alu e_af_save sites no-op when it is set. VEX/0F38-3A/x87 already `continue`d above.
        g_pfaf_dead = 0;
        if (pfaf_elim_on() && insn_writes_pfaf(&I)) {
            struct insn NI;
            decode(next, &NI);
            g_pfaf_dead = insn_kills_pfaf(&NI);
            // x86-xflags: the producer is the LAST flag op before a direct branch (cmp/test + jcc is
            // THE hot pattern) -> its PF/AF are still dead if EVERY successor entry provably
            // overwrites both before any read (guest-byte liveness scan, translate/trace.c).
            if (!g_pfaf_dead && xblkflags_on() && NI.len > 0) g_pfaf_dead = pfaf_dead_thru(&NI, next, gpc);
            // x86-xflags: generalize past the 1-insn / direct-branch cases -- in real integer chains the
            // next PF/AF writer sits a few value-only movs/immediate-shifts downstream. The same block
            // liveness scan proves both PF and AF overwritten-before-read from `next`; if so, drop the
            // whole PF/AF substrate for I (e_pf_save/e_af_save no-op on g_pfaf_dead).
            if (!g_pfaf_dead && xblkalu_elide_on()) g_pfaf_dead = !(x86_flags_livein(next, gpc) & (XF_PF | XF_AF));
        }

        // x87 static-top tracking ends at any non-x87 instruction: spill the shadow top to
        // cpu->fptop and drop to the runtime-top model (the run only spans consecutive x87 ops, so
        // no top assumption ever crosses a non-x87 op, a branch target, or a block boundary).
        if (g_fp_known && !(!I.two && op >= 0xD8 && op <= 0xDF)) fp_drop();

        if (!I.two) {
            // ---- data-move class (mov B0-BF/C6/C7/88-8B, lea 8D, push/pop 50-5F, movsxd 63) ----
            // Lowered in translate/mov.c translate_mov(); no EFLAGS interaction.
            {
                int s = translate_mov(&I, next);
                if (s == TX_NEXT) {
                    gpc = next;
                    continue;
                }
            }
            // ---- integer ALU class (primary 00..3D, acc imm, group1 80/81/83, test 84/85 A8/A9) ----
            // Lowered in translate/alu.c translate_alu(); flag deferral is unchanged (do_alu drives it).
            {
                int s = translate_alu(&I, next);
                if (s == TX_NEXT) {
                    gpc = next;
                    continue;
                }
            }
            // ---- shift/rotate class (group2: C0/C1/D0/D1/D2/D3 -> SHL/SHR/SAR/ROL/ROR/RCL/RCR) ----
            // Lowered in translate/shift.c translate_shift().
            {
                int s = translate_shift(&I, gpc, next);
                if (s == TX_NEXT) {
                    gpc = next;
                    continue;
                }
                if (s == TX_BREAK) break;
            }
            // ---- group3 (F6/F7): /0 test /2 not /3 neg /4 mul /5 imul /6 div /7 idiv ----
            if (op == 0xF6 || op == 0xF7) {
                int k = I.reg & 7, w = op == 0xF6 ? 1 : I.opsize, mem;
                if (k == 0) {
                    int rmv = rm_load(&I, next, w, &mem);
                    e_movconst(19, (uint64_t)I.imm);
                    do_alu(4, -1, rmv, 19, w);
                    gpc = next;
                    continue;
                } // test r/m, imm
                if (k == 2) {
                    int rmv = rm_load(&I, next, w, &mem); // not -> x16, then rm_store
                    emit32(0xAA2003E0u | (rmv << 16) | 16);
                    rm_store(&I, w, 16);
                    gpc = next;
                    continue;
                }
                if (k == 3) {
                    // neg r/m == 0 - r/m. For byte/word, a raw 32-bit SUBS got OF wrong for negb 0x80 /
                    // negw 0x8000 (the INT_MIN overflow case) -- e.g. icu_locid's Option-niche PartialEq does
                    // `negb; jno` on the 0x80 None marker. do_alu shifts byte/word operands into the ARM high
                    // bits so N/Z/V are width-correct. For 32/64-bit the direct SUBS flags are already exact,
                    // and do_alu's deferred (FL_SUB) path would let flags_materialize clobber the x16 result
                    // before rm_store (it broke `neg ebx`), so keep the original inline path there.
                    int rmv = rm_load(&I, next, w, &mem); // neg -> x16
                    if (w < 4) {
                        do_alu(5, 16, 31, rmv, w); // x16 = 0 - rmv, x86 SUB flags at width w (sets PF too)
                    } else {
                        e_rrr(A_SUBS, 16, 31, rmv, w == 8, 0);
                        e_nzcv_save();
                        e_pf_save(16);                // x86 PF source = result low byte (do_alu handles the w<4 path)
                        e_af_addsub(31, rmv, 16, 19); // x86 AF = bit 4 of (0 ^ rmv ^ result)
                    }
                    rm_store(&I, w, 16);
                    gpc = next;
                    continue;
                }
                if (w == 1 && k >= 4) { // 8-bit mul/div (0xF6 /4../7) -- were UNIMPL (glibc inet_ntoa aborts)
                    int rmv = rm_load(&I, next, 1, &mem);
                    if (k == 4 || k == 5) { // mul/imul r/m8: AX = AL * r/m8 (16-bit result)
                        if (k == 4) {
                            e_uxt(19, RAX, 1);
                            e_uxt(20, rmv, 1);
                        } // zero-extend AL + src (uxtb)
                        else {
                            e_sxt(19, RAX, 1);
                            e_sxt(20, rmv, 1);
                        } // sign-extend (sxtb)
                        e_mul(21, 19, 20, 0);
                        e_mul_oc_narrow(21, k, 1); // CF=OF: AX doesn't fit AL (uses the full product in x21)
                        e_bfi(RAX, 21, 0, 16, 1);  // write AX (low 16), preserve upper RAX (x86 8/16-bit semantics)
                    } else {                       // div/idiv r/m8: AL = AX / r/m8, AH = AX % r/m8
                        if (k == 6) {
                            e_uxt(19, RAX, 2);
                            e_uxt(20, rmv, 1);
                            emit_div_zero_check(20, gpc, 0); // #DE on /0 (rip = the div insn)
                            e_udiv(21, 19, 20, 0);
                        } // AX uxth, src uxtb
                        else {
                            e_sxt(19, RAX, 2);
                            e_sxt(20, rmv, 1);
                            emit_div_zero_check(20, gpc, 1);
                            e_sdiv(21, 19, 20, 0);
                        } // AX sxth, src sxtb
                        e_msub(22, 21, 20, 19, 0);                          // rem = dividend - quot*divisor
                        emit_div_ovf_check(21, 23, 1, k == 7, gpc, k == 7); // AL overflow -> #DE
                        e_bfi(RAX, 21, 0, 8, 1);                            // AL = quotient
                        e_bfi(RAX, 22, 8, 8, 1);                            // AH = remainder
                    }
                    gpc = next;
                    continue;
                }
                if (w == 2 && k >= 4) { // 16-bit mul/div (66 F7 /4../7) -- e.g. uutils `date` does `div si`
                    int rmv = rm_load(&I, next, 2, &mem);
                    if (k == 4 || k == 5) { // mul/imul r/m16: DX:AX = AX * r/m16 (32-bit product)
                        if (k == 4) {
                            e_uxt(19, RAX, 2);
                            e_uxt(20, rmv, 2);
                        } // AX + src zero-extended (uxth)
                        else {
                            e_sxt(19, RAX, 2);
                            e_sxt(20, rmv, 2);
                        } // sign-extended (sxth)
                        e_mul(21, 19, 20, 0);
                        e_mul_oc_narrow(21, k, 2); // CF=OF: product doesn't fit AX (uses full product in x21)
                        e_bfi(RAX, 21, 0, 16, 1);  // AX = low 16
                        e_lsr_i(21, 21, 16, 0);
                        e_bfi(RDX, 21, 0, 16, 1); // DX = high 16
                    } else {                      // div/idiv r/m16: AX = (DX:AX)/r/m16, DX = remainder
                        e_uxt(19, RAX, 2);
                        e_bfi(19, RDX, 16, 16, 0); // x19 = (DX<<16)|AX -- the 32-bit dividend
                        if (k == 6) {
                            e_uxt(20, rmv, 2);
                            emit_div_zero_check(20, gpc, 0); // #DE on /0
                            e_udiv(21, 19, 20, 0);
                        } // unsigned: divisor uxth
                        else {
                            e_sxt(20, rmv, 2);
                            emit_div_zero_check(20, gpc, 1);
                            e_sdiv(21, 19, 20, 0);
                        } // signed: x19 already the 32-bit pattern
                        e_msub(22, 21, 20, 19, 0);                          // rem = dividend - quot*divisor
                        emit_div_ovf_check(21, 23, 2, k == 7, gpc, k == 7); // AX overflow -> #DE
                        e_bfi(RAX, 21, 0, 16, 1);                           // AX = quotient
                        e_bfi(RDX, 22, 0, 16, 1);                           // DX = remainder
                    }
                    gpc = next;
                    continue;
                }
                if (w == 4 || w == 8) {
                    if (k == 4 || k == 5) { // mul / imul (rdx:rax = rax * r/m)
                        int rmv = rm_load(&I, next, w, &mem);
                        if (w == 4) {
                            // 32-bit: the operands are EAX and the 32-bit r/m. Mask (mul) or sign-extend
                            // (imul) them to 32 bits FIRST so dirty upper halves of the host regs can't
                            // corrupt the product; the 64-bit result lands in edx:eax. (Before this, the
                            // full 64-bit regs were multiplied -> wrong product on a dirty upper half, and
                            // the imul high half came out unsigned.)
                            if (k == 4) {
                                e_uxt(20, RAX, 4);
                                e_uxt(21, rmv, 4);
                            } else {
                                e_sxt(20, RAX, 4);
                                e_sxt(21, rmv, 4);
                            }
                            e_mul(19, 20, 21, 1);    // x19 = 64-bit product
                            e_lsr_i(RDX, 19, 32, 1); // edx = product[63:32]
                            e_mov_rr(RAX, 19, 0);    // eax = product[31:0]
                            if (k == 4) {            // MUL: CF=OF = (high half != 0)
                                e_lsr_i(22, 19, 32, 1);
                                e_subi_s(23, 22, 0, 1);
                                e_cset(21, 1 /*NE*/, 1);
                            } else { // IMUL: CF=OF = (full product != sxt of low 32)
                                e_sxt(22, 19, 4);
                                e_rrr(A_SUBS, 23, 19, 22, 1, 0);
                                e_cset(21, 1 /*NE*/, 1);
                            }
                            e_mul_set_oc(21);
                            gpc = next;
                            continue;
                        }
                        // w == 8: the operands are the full 64-bit registers, lo->rax hi->rdx.
                        e_mul(19, RAX, rmv, 1);
                        if (k == 4)
                            e_umulh(RDX, RAX, rmv);
                        else
                            e_smulh(RDX, RAX, rmv);
                        e_mov_rr(RAX, 19, 1);
                        // x86 CF=OF: high half significant? (jc/jo/setc/seto consume these; e.g. glibc's
                        // divide-by-constant idioms after a widening multiply). x19=full lo product, RDX=hi.
                        if (k == 4) { // MUL: CF=OF = (high half != 0)
                            e_mov_rr(22, RDX, 1);
                            e_subi_s(23, 22, 0, 1);
                            e_cset(21, 1 /*NE*/, 1);
                        } else {                              // IMUL: CF=OF = (high half != sign-extension of low half)
                            e_asr_i(22, 19, 63, 1);           // x22 = sign bits of low half
                            e_rrr(A_SUBS, 23, RDX, 22, 1, 0); // cmp smulh(hi), sign(lo)
                            e_cset(21, 1 /*NE*/, 1);
                        }
                        e_mul_set_oc(21);
                        gpc = next;
                        continue;
                    }
                    if (k == 6 || k == 7) { // div / idiv
                        int rmv = rm_load(&I, next, w, &mem);
                        if (w == 8) { // 64-bit: fast inline UDIV/SDIV for 64/64; C helper for true 128/64
                            emit_div64_fast(next, gpc, k == 7, rmv);
                            gpc = next;
                            continue;
                        }
                        // 32-bit: dividend = edx:eax (64-bit), 32-bit divisor (zero/sign-extend), 32-bit quotient
                        e_lsl_i(19, RDX, 32, 1);
                        e_bfi(19, RAX, 0, 32, 1); // x19 = (edx<<32)|eax
                        if (k == 6) {
                            e_uxt(22, rmv, 4);
                            emit_div_zero_check(22, gpc, 0); // #DE on /0
                            e_udiv(20, 19, 22, 1);
                        } // unsigned: zero-extend divisor
                        else {
                            e_sxt(22, rmv, 4);
                            emit_div_zero_check(22, gpc, 1);
                            e_sdiv(20, 19, 22, 1);
                        } // signed: sign-extend divisor (edx:eax already 64-bit signed)
                        e_msub(21, 20, 22, 19, 1);                          // rem = x19 - q*divisor
                        emit_div_ovf_check(20, 23, 4, k == 7, gpc, k == 7); // EAX overflow -> #DE
                        e_mov_rr(RAX, 20, 0);
                        e_mov_rr(RDX, 21, 0); // eax=quot, edx=rem (32-bit)
                        gpc = next;
                        continue;
                    }
                }
                report_unimpl(gpc, &I);
                break;
            }
            // ---- group4/5 (FE/FF): inc/dec, and FF: call/jmp/push (indirect) ----
            if (op == 0xFE || op == 0xFF) {
                int k = I.reg & 7, w = op == 0xFE ? 1 : I.opsize, mem;
                if (k == 0 || k == 1) {                   // inc / dec: set N/Z/V (OF correct), PRESERVE CF
                    int rmv = rm_load(&I, next, w, &mem); // mem -> x16 (val), x17 (EA)
                    if (I.lock && mem) {
                        // LOCK inc/dec [mem] -> atomic RMW (e.g. glibc's spinlock `lock decl`): a plain
                        // load-op-store races under contention and strands the lock with no owner -> hang.
                        // LDADDAL of +1/-1 updates memory indivisibly and yields the old value for flags.
                        e_movconst(19, k == 0 ? 1 : (uint64_t)-1); // delta (LSE size truncates to width w)
                        e_lse(LSE_LDADD, w, 19, 20, 17);           // x20 = old; [x17] += delta (acq-rel)
                        if (w >= 4) {
                            if (k == 0)
                                e_addi_s(21, 20, 1, sf);
                            else
                                e_subi_s(21, 20, 1, sf);
                            e_af_addsub(20, 21, 31, 19); // x86 AF = bit 4 of (old ^ result) -- x20=old
                            e_nzcv_save_keepC();         // (must precede keepC, which clobbers x20)
                            e_pf_save(21);
                        } else { // byte/word: flags from the high bits (mirror the non-atomic path)
                            int sh = 8 * (4 - w);
                            e_mov_rr(26, 20, 0); // save old before keepC clobbers x20
                            e_lsl_i(21, 20, sh, 0);
                            e_movconst(19, 1u << sh);
                            if (k == 0)
                                e_rrr(A_ADDS, 21, 21, 19, 0, 0);
                            else
                                e_rrr(A_SUBS, 21, 21, 19, 0, 0);
                            e_nzcv_save_keepC();
                            e_lsr_i(21, 21, sh, 0);
                            e_pf_save(21);
                            e_af_addsub(26, 21, 31, 19); // x86 AF = bit 4 of (old ^ result)
                        }
                        gpc = next;
                        continue;
                    }
                    int o = mem ? 16 : I.rm_reg;
                    if (w >= 4) {
                        e_mov_rr(26, rmv, sf); // save old (o aliases rmv for a register dest) for AF
                        if (k == 0)
                            e_addi_s(o, rmv, 1, sf);
                        else
                            e_subi_s(o, rmv, 1, sf);
                        e_nzcv_save_keepC();
                        e_pf_save(o);               // x86 PF source = result low byte
                        e_af_addsub(26, o, 31, 19); // x86 AF = bit 4 of (old ^ result)
                        rm_store(&I, w, o);
                    } else { // byte/word: flags from the high bits
                        int sh = 8 * (4 - w);
                        e_lsl_i(21, rmv, sh, 0);
                        e_movconst(19, 1u << sh);
                        if (k == 0)
                            e_rrr(A_ADDS, 21, 21, 19, 0, 0);
                        else
                            e_rrr(A_SUBS, 21, 21, 19, 0, 0);
                        e_nzcv_save_keepC();
                        e_lsr_i(21, 21, sh, 0);
                        e_pf_save(21);                // x86 PF source = result low byte
                        e_af_addsub(rmv, 21, 31, 19); // x86 AF = bit 4 of (old ^ result)
                        rm_store(&I, w, 21);
                    }
                    gpc = next;
                    continue;
                }
                if (op == 0xFF && (k == 4 || k == 2)) { // jmp / call r/m (indirect)
                    int mem2;
                    int tgt = rm_load(&I, next, 8, &mem2);
                    if (tgt != 16) e_mov_rr(16, tgt, 1); // target -> x16
                    e_movconst(19, gpc);
                    e_str(19, 28, OFF_IBSRC); // debug
                    if (k == 2) {
                        e_subi(RSP, RSP, 8, 1);
                        e_movconst(19, next);
                        e_store(8, 19, RSP);
                    } // call: push ret
                    emit_ibranch();
                    break; // IBTC inline probe (target in x16)
                }
                if (op == 0xFF && k == 6) { // push r/m
                    int mem2;
                    int v = rm_load(&I, next, 8, &mem2);
                    if (v != 16) e_mov_rr(16, v, 1);
                    e_subi(RSP, RSP, 8, 1);
                    e_store(8, 16, RSP);
                    gpc = next;
                    continue;
                }
            }
            // ---- xchg (86/87) ----
            if (op == 0x86 || op == 0x87) {
                int w = (op & 1) ? I.opsize : 1, mem;
                if (I.is_mem) {
                    emit_ea(&I, next);
                    int sv = (w == 1) ? byte_val(&I, I.reg, 19) : I.reg; // reg->mem: handle ah/bh/ch/dh
                    // xchg with memory is IMPLICITLY atomic on x86 (no LOCK needed) -> SWP, not load+store.
                    // glibc's mutex fast-path acquires the lock with xchg, so this must be a real atomic.
                    e_lse(LSE_SWP, w, sv, 16, 17); // x16 = old [mem]; [mem] = sv (atomic swap)
                    if (w >= 4)
                        e_mov_rr(I.reg, 16, w == 8);
                    else if (w == 1)
                        byte_wb(&I, I.reg, 16);
                    else
                        e_bfi(I.reg, 16, 0, 8 * w, 1);
                } else if (w == 1) {
                    // byte xchg of two registers, either of which may be a high-byte (ah/bh/ch/dh) or even
                    // the SAME underlying register (xchg %ch,%cl): materialize BOTH bytes into independent
                    // scratch before writing either back, or the first write corrupts the second's source.
                    // The full-register e_mov_rr swap below ignores the byte/hi8 lanes -> base64 -d
                    // byte-scramble (busybox decode_base64 repacks via `mov %esi,%ecx; xchg %ch,%cl`).
                    int a = byte_val(&I, I.reg, 16);
                    int b = byte_val(&I, I.rm_reg, 17);
                    e_mov_rr(19, a, 0);
                    e_mov_rr(23, b, 0);
                    byte_wb(&I, I.reg, 23);
                    byte_wb(&I, I.rm_reg, 19);
                } else {
                    e_mov_rr(19, I.rm_reg, sf);
                    e_mov_rr(I.rm_reg, I.reg, sf);
                    e_mov_rr(I.reg, 19, sf);
                }
                (void)mem;
                gpc = next;
                continue;
            }
            // ---- push imm (68 iz, 6A ib) ----
            if (op == 0x68 || op == 0x6A) {
                e_movconst(16, (uint64_t)I.imm);
                e_subi(RSP, RSP, 8, 1);
                e_store(8, 16, RSP);
                gpc = next;
                continue;
            }
            // ---- pop r/m (8F /0) ----
            if (op == 0x8F) {
                if (I.is_mem) {
                    // Pop into x19 (callee-saved) FIRST: emit_ea uses x16 as a scratch, so the popped
                    // value can't live in x16 across the address computation. x86 also computes the
                    // destination EA AFTER RSP is incremented (matters for an RSP-based destination).
                    e_load(8, 19, RSP);
                    e_addi(RSP, RSP, 8, 1);
                    emit_ea(&I, next);
                    e_store(8, 19, 17);
                } else {
                    e_load(8, 16, RSP);
                    e_addi(RSP, RSP, 8, 1);
                    e_mov_rr(I.rm_reg, 16, 1);
                }
                gpc = next;
                continue;
            }
            // ---- imul reg, r/m, imm (69 iz, 6B ib) ----
            if (op == 0x69 || op == 0x6B) {
                int mem;
                int rmv = rm_load(&I, next, I.opsize, &mem);
                e_movconst(19, (uint64_t)I.imm);
                int imm_co_live = !xblkalu_elide_on() || (x86_flags_livein(next, gpc) & XF_NZCV);
                e_imul2(I.reg, rmv, 19, I.opsize, imm_co_live); // dst = r/m * imm, sets x86 CF/OF on overflow
                gpc = next;
                continue;
            }
            // ---- string ops: stos/movs/lods (AA/AB/A4/A5/AC/AD), cmps/scas (A6/A7/AE/AF), cld/std (FC/FD).
            // Lifted into translate/repstr.c translate_string(); it returns how to steer this translate loop
            // (TX_NEXT -> gpc=next;continue, TX_BREAK -> end the block, TX_FALL -> not a string op).
            {
                int s = translate_string(&I, next);
                if (s == TX_NEXT) {
                    // A string idiom was emitted; its ERMS funnel may `blr` a host helper, which can
                    // clobber ALL of v16..v31 -- drop the crypto constant-hoist claims (v26/v27).
                    g_v26z = g_v27m = 0;
                    gpc = next;
                    continue;
                }
                if (s == TX_BREAK) break;
            }
            // ---- jmp rel (E9/EB) ----
            if (op == 0xE9 || op == 0xEB) {
                uint64_t tgt = next + (uint64_t)I.imm;
                // STITCH: follow the unconditional edge inline. Under x86-xflags the top-of-loop
                // did NOT materialize a deferred producer for this jmp: the inlined continuation is
                // the same host block, so g_fl_pending simply stays live across the (vanished) edge
                // and the continuation's own consumers handle it exactly as intra-block code.
                // (Without x86-xflags, g_fl_pending is FL_NONE here as before.) Skip if the target
                // is the region head, already laid in this region, an already-registered block, or
                // a dead trap arm.
                if (STITCH_OK && tgt != start && !seen_has(seen, nseen, tgt) && !map_body(tgt) && !trap_head(tgt)) {
                    seen[nseen++] = tgt;
                    trace_blk++;
                    gpc = tgt;
                    continue;
                }
                // x86-xflags: chained/exit edge -- materialize unless the successor provably kills
                // the flags first (no-op when nothing is pending).
                flags_edge(tgt, gpc);
                emit_chain_exit(tgt);
                break;
            }
            // ---- call rel32 (E8) ----
            if (op == 0xE8) {
                e_subi(RSP, RSP, 8, 1); // flag-free push of the return address
                e_movconst(16, next);
                e_store(8, 16, RSP);
                // x86-xflags: consult the CALLEE's flag live-in (function prologues kill flags fast).
                flags_edge(next + (uint64_t)I.imm, gpc);
                emit_chain_exit(next + (uint64_t)I.imm);
                break;
            }
            // ---- jrcxz/jecxz rel8 (E3): jump if rCX == 0 (no decrement, no flag effect) ----
            // 0x67 selects the 32-bit counter (ECX): test the W-form so a nonzero upper half of RCX
            // doesn't suppress the branch; without it the 64-bit X-form tests all of RCX.
            if (op == 0xE3) {
                uint64_t taken = next + (uint64_t)I.imm;
                uint32_t cbz = I.addr32 ? 0x34000000u : 0xB4000000u; // cbz w_rcx / x_rcx
                uint32_t *patch = (uint32_t *)g_cp;
                emit32(0);             // cbz rcx -> taken
                emit_chain_exit(next); // rCX != 0: fall through
                int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
                *patch = cbz | (((uint32_t)d & 0x7FFFF) << 5) | RCX; // cbz rcx, taken
                emit_chain_exit(taken);
                break;
            }
            // ---- loop/loope/loopne rel8 (E2/E1/E0): --rCX; branch if rCX != 0 [and ZF cond] ----
            // None of these touch RFLAGS; loope/loopne only READ ZF. 0x67 makes the counter ECX (sf=0,
            // which also zero-extends so the X-form cbz/cbnz still tests the right value); else RCX.
            if (op == 0xE0 || op == 0xE1 || op == 0xE2) {
                uint64_t taken = next + (uint64_t)I.imm;
                int sf2 = I.addr32 ? 0 : 1; // counter width: ECX vs RCX
                uint32_t cbz = I.addr32 ? 0x34000000u : 0xB4000000u;
                uint32_t cbnz = I.addr32 ? 0x35000000u : 0xB5000000u;
                e_subi(RCX, RCX, 1, sf2); // --rCX (no flag effect: plain SUB, not SUBS)
                if (op == 0xE2) {
                    // plain LOOP: taken iff rCX != 0.
                    uint32_t *patch = (uint32_t *)g_cp;
                    emit32(0);             // cbnz rcx -> taken
                    emit_chain_exit(next); // rCX == 0: fall through
                    int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
                    *patch = cbnz | (((uint32_t)d & 0x7FFFF) << 5) | RCX;
                    emit_chain_exit(taken);
                    break;
                }
                // loope (E1): taken iff rCX != 0 && ZF==1; loopne (E0): rCX != 0 && ZF==0.
                // Branch to the fall-through exit when EITHER test fails; fall through to the taken exit.
                // Flags are already materialized to membank by the top-of-loop (E0/E1 aren't flagkill),
                // but reload cpu->nzcv to be robust even when no producer was pending this block.
                e_nzcv_load(); // ZF (ARM Z) canonical in live NZCV; SUB above left it untouched
                int fail_cc = (op == 0xE1) ? 1 /*NE: ZF==0 fails loope*/ : 0 /*EQ: ZF==1 fails loopne*/;
                uint32_t *p1 = (uint32_t *)g_cp;
                emit32(0); // cbz rcx -> fall
                uint32_t *p2 = (uint32_t *)g_cp;
                emit32(0);              // b.<fail_cc> -> fall
                emit_chain_exit(taken); // both tests passed
                int64_t d1 = ((uint8_t *)g_cp - (uint8_t *)p1) / 4;
                *p1 = cbz | (((uint32_t)d1 & 0x7FFFF) << 5) | RCX; // cbz rcx, fall
                int64_t d2 = ((uint8_t *)g_cp - (uint8_t *)p2) / 4;
                *p2 = 0x54000000u | (((uint32_t)d2 & 0x7FFFF) << 5) | (uint32_t)fail_cc; // b.cc, fall
                emit_chain_exit(next);
                break;
            }
            // ---- jcc rel8 (70-7F) ----
            if (op >= 0x70 && op <= 0x7F) {
                int lo = op & 0xF, parity = (lo == 0xA || lo == 0xB);
                int cc;
                if (parity) {
                    cc = emit_parity_jcc_cond(lo); // jp/jnp: PF lane -> live ARM Z, branch off it
                } else {
                    cc = x86cc_to_arm(lo);
                    if (cc < 0) {
                        if (g_fl_pending) flags_materialize(); // materialize before boundary
                        report_unimpl(gpc, &I);
                        break;
                    }
                }
                uint64_t taken = next + (uint64_t)I.imm;
                // W5B tier-2: single-block self-loop (taken back-edge == block start). Detected BEFORE the
                // flag handling / superblock stitch below so the self-loop owns the back-edge; emit the
                // hotness counter (tier-1) or the folded back-edge (tier-2). g_fl_pending is still pending
                // here -- emit_selfloop_x86 does the flag handling itself. Parity already set the live Z
                // (and spilled any pending producer) above, so it skips this purely-NZCV-flag path.
                if (!parity && taken == start && !notier2x() && !loop_has_rmw_hazard((uint64_t)body, (uint64_t)g_cp)) {
                    int slot = g_tier2_build ? 0 : t2_slot(start);
                    if (g_tier2_build || slot >= 0) {
                        emit_selfloop_x86(cc, start, next, body, slot);
                        break;
                    }
                }
                uint64_t fall = next;
                int stitch_fall =
                    (STITCH_OK && fall != start && !seen_has(seen, nseen, fall) && !map_body(fall) && !trap_head(fall));
                int save_taken = 0, save_fall = 0;
                if (parity) {
                    // live ARM Z already holds (PF==0) from emit_parity_jcc_cond; flags spilled there.
                } else {
                    // Fast path: live NZCV still holds the immediately-preceding width-4/8 producer's
                    // flags, so branch straight off them. jcc_edge_flags (x86-xflags, trace.c) spills
                    // the deferred producer exactly as flags_materialize() did -- EXCEPT on edges whose
                    // successor provably overwrites the flags before reading: FL_SUB pushes its spill
                    // onto only the flag-live edge(s) (save_taken/save_fall, emitted below), a stitched
                    // fall keeps g_fl_pending for the inline continuation, and FL_ADD/FL_LOGIC drop the
                    // dead store after the mandatory msr fixup. FL_NONE reloads membank as before.
                    jcc_edge_flags(taken, fall, gpc, stitch_fall, &save_taken, &save_fall);
                }
                // STITCH: lay the fall-through (`next`) inline; the taken side becomes a tiny
                // out-of-line exit reached by the INVERTED condition. Both arms see canonical live
                // flags; the taken stub spills cpu->nzcv iff its successor may read it. A parity jcc
                // clobbered the live NZCV with its PF scratch -> restore the canonical membank flags
                // on EVERY outgoing edge (parity-edge fix; see emit_parity_jcc_cond).
                if (stitch_fall) {
                    int inv = (cc ^ 1) & 0xF; // not-taken -> branch over the taken exit (x86cc_to_arm is 0..13)
                    uint32_t *patch = (uint32_t *)g_cp;
                    emit32(0);                     // b.inv -> fall (inline)
                    if (parity) e_nzcv_load();     // taken edge: restore canonical live NZCV
                    if (save_taken) e_nzcv_save(); // FL_SUB spill on the (flag-live) taken edge only
                    emit_chain_exit(taken);
                    int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
                    *patch = 0x54000000u | (((uint32_t)d & 0x7FFFF) << 5) | (uint32_t)inv;
                    if (parity) e_nzcv_load(); // inline fall: restore before continuing
                    seen[nseen++] = fall;
                    trace_blk++;
                    gpc = fall;
                    continue;
                }
                uint32_t *patch = (uint32_t *)g_cp;
                emit32(0);                    // b.cond -> taken
                if (parity) e_nzcv_load();    // fall edge: restore canonical live NZCV
                if (save_fall) e_nzcv_save(); // FL_SUB spill for a flag-live fall successor
                emit_chain_exit(next);
                int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
                *patch = 0x54000000u | (((uint32_t)d & 0x7FFFF) << 5) | (cc & 0xF);
                if (parity) e_nzcv_load();     // taken edge: restore canonical live NZCV
                if (save_taken) e_nzcv_save(); // FL_SUB spill for a flag-live taken successor
                emit_chain_exit(taken);
                break;
            }
            // ---- ret (C3) / ret imm16 (C2) ----
            if (op == 0xC3 || op == 0xC2) {
                e_load(8, 16, RSP);
                e_addi(RSP, RSP, 8, 1);
                if (op == 0xC2) {
                    e_movconst(19, (uint64_t)(uint16_t)I.imm);
                    e_rrr(A_ADD, RSP, RSP, 19, 1, 0);
                }
                e_movconst(19, gpc);
                e_str(19, 28, OFF_IBSRC); // debug
                emit_ibranch();
                break; // IBTC inline probe (target in x16)
            }
            // ---- leave (C9) ----
            if (op == 0xC9) {
                e_mov_rr(RSP, RBP, 1);
                e_load(8, RBP, RSP);
                e_addi(RSP, RSP, 8, 1);
                gpc = next;
                continue;
            }
            // ---- nop (90) / xchg rAX, rN (91-97) ----
            if (op == 0x90 && !I.rep) {
                // `90` is XCHG eAX,rN — only a NOP when N==rAX. With REX.B it targets r8 (`49 90` =
                // xchg rax,r8), a REAL swap; dropping it (stale r8) is the busybox `sort` SIGSEGV (the
                // `call malloc; xchg %rax,%r8` allocator idiom). Mirror the 0x91-0x97 sibling.
                if (I.rexB) {
                    int r = I.rexB << 3; // r8
                    e_mov_rr(19, RAX, sf);
                    e_mov_rr(RAX, r, sf);
                    e_mov_rr(r, 19, sf);
                }
                gpc = next;
                continue;
            } // (F3 90 = pause -> also nop)
            if (op == 0x9B) {
                gpc = next;
                continue;
            } // fwait/wait -> nop (FPU sync)
            // sahf (9E): AH -> flags. We map SF=AH.7, ZF=AH.6, CF=AH.0 into cpu->nzcv (N/Z/C) and
            // restore PF=AH.2 into the PF lane (the consumer takes even-parity, so store NOT(PF) as the
            // source byte: a 0 byte -> even popcount -> PF=1, a 1 byte -> odd -> PF=0).
            if (op == 0x9E) {
                emit32(0x53083C00u | (RAX << 5) | 16); // ubfx w16, w_rax, #8, #8  (AH)
                emit32(0x53000000u | (16 << 5) | 17);  // ubfx w17, w16, #0, #1  (CF)
                e_movconst(20, 1);
                e_rrr(A_EOR, 17, 17, 20, 0, 0); // stored borrow-C = NOT x86 CF
                e_lsl_i(17, 17, 29, 0);
                emit32(0x53061800u | (16 << 5) | 18); // ubfx w18, w16, #6, #1  (ZF)
                e_lsl_i(18, 18, 30, 0);
                e_rrr(A_ORR, 17, 17, 18, 0, 0);
                emit32(0x53071C00u | (16 << 5) | 18); // ubfx w18, w16, #7, #1  (SF)
                e_lsl_i(18, 18, 31, 0);
                e_rrr(A_ORR, 17, 17, 18, 0, 0);
                e_str(17, 28, OFF_NZCV);
                emit32(0xD51B4200u | 17);             // msr nzcv, x17 (sync live flags)
                emit32(0x53020800u | (16 << 5) | 19); // ubfx w19, w16, #2, #1  (PF)
                e_rrr(A_EOR, 19, 19, 20, 0, 0);       // PF source byte = NOT PF (parity-even <-> PF=1)
                e_str(19, 28, OFF_PF);
                e_af_save(16);          // AF from AH bit4 (cpu->af consumer extracts bit 4)
                g_fl_pending = FL_NONE; // flags now live in cpu->nzcv (don't let a stale defer overwrite)
                gpc = next;
                continue;
            }
            // lahf (9F): flags -> AH (SF,ZF,0,AF,0,PF,1,CF). Fill SF/ZF/CF/AF/PF + the always-1 bit.
            if (op == 0x9F) {
                if (g_fl_pending) flags_materialize(); // make cpu->nzcv current first
                e_ldr(16, 28, OFF_NZCV);
                emit32(0x53000000u | (31 << 16) | (31 << 10) | (16 << 5) | 17); // ubfx w17,w16,#31,#1 (N->SF)
                e_lsl_i(17, 17, 7, 0);
                emit32(0x53000000u | (30 << 16) | (30 << 10) | (16 << 5) | 18); // ubfx w18,w16,#30,#1 (Z->ZF)
                e_lsl_i(18, 18, 6, 0);
                e_rrr(A_ORR, 17, 17, 18, 0, 0);
                emit32(0x53000000u | (29 << 16) | (29 << 10) | (16 << 5) | 18); // ubfx w18,w16,#29,#1 (stored borrow-C)
                e_movconst(19, 1);
                e_rrr(A_EOR, 18, 18, 19, 0, 0); // x86 CF = NOT stored borrow-C
                e_rrr(A_ORR, 17, 17, 18, 0, 0); // -> bit0
                e_movconst(18, 2);
                e_rrr(A_ORR, 17, 17, 18, 0, 0); // bit1 reads as 1
                e_pf_compute(18);               // x18 = x86 PF (0/1); clobbers x16
                e_rrr(A_ORR, 17, 17, 18, 0, 2); // PF -> bit2
                e_ldr(18, 28, OFF_AF);
                emit32(0x53000000u | (4 << 16) | (4 << 10) | (18 << 5) | 18); // ubfx w18,w18,#4,#1 (AF)
                e_rrr(A_ORR, 17, 17, 18, 0, 4);                               // AF -> bit4
                e_bfi(RAX, 17, 8, 8, 1);
                gpc = next;
                continue; // AH = w17
            }
            // clc/stc/cmc (F8/F9/F5): set/clear/complement x86 CF only (other flags untouched). Borrow
            // convention -> stored C(bit29) = NOT x86 CF, so clc(CF=0)=set, stc(CF=1)=clear, cmc=toggle.
            if (op == 0xF8 || op == 0xF9 || op == 0xF5) {
                e_nzcv_setcf_op(op == 0xF8 ? A_ORR : op == 0xF9 ? A_BIC : A_EOR);
                gpc = next;
                continue;
            }
            // pushfq (9C): materialize x86 RFLAGS from the flag substrate and push it. Bits assembled in
            // x17: reserved bit1=1, IF(bit9)=1 (userspace), DF(bit10)=cpu->df (runtime), then CF/PF/ZF/
            // SF/OF from cpu->nzcv + the PF lane, and AF(bit4) from the cpu->af lane.
            if (op == 0x9C) {
                if (g_fl_pending) flags_materialize(); // make cpu->nzcv current
                e_ldr(16, 28, OFF_NZCV);               // x16 = ARM NZCV substrate
                e_movconst(17, 0x202u);                // reserved bit1=1, IF(bit9)=1
                e_ldr(18, 28, OFF_DF);                 // DF(bit10) from the runtime cpu->df (0/1)
                e_rrr(A_ORR, 17, 17, 18, 0, 10);
                emit32(0x53000000u | (29 << 16) | (29 << 10) | (16 << 5) | 18); // ubfx w18,w16,#29,#1 (borrow C)
                e_movconst(19, 1);
                e_rrr(A_EOR, 18, 18, 19, 0, 0); // x86 CF = NOT stored-C (borrow convention)
                e_rrr(A_ORR, 17, 17, 18, 0, 0); // -> bit0
                e_bit_move(17, 16, 30, 6, 18);  // ZF: NZCV.Z(30) -> bit6
                e_bit_move(17, 16, 31, 7, 18);  // SF: NZCV.N(31) -> bit7
                e_bit_move(17, 16, 28, 11, 18); // OF: NZCV.V(28) -> bit11
                e_pf_compute(18);               // x18 = x86 PF (0/1); clobbers x16
                e_rrr(A_ORR, 17, 17, 18, 0, 2); // -> bit2
                e_ldr(18, 28, OFF_AF);
                emit32(0x53000000u | (4 << 16) | (4 << 10) | (18 << 5) | 18); // ubfx w18,w18,#4,#1 (AF)
                e_rrr(A_ORR, 17, 17, 18, 0, 4);                               // AF -> bit4
                e_ldr(18, 28, OFF_ID);
                e_rrr(A_ORR, 17, 17, 18, 0, 21); // ID(bit21) <- cpu->idflag (0/1) -- round-trips a CPUID probe
                e_subi(RSP, RSP, 8, 1);
                e_store(8, 17, RSP);
                gpc = next;
                continue;
            }
            // popfq (9D): pop RFLAGS and distribute back into the flag substrate (cpu->nzcv + PF/AF/ID/DF).
            // DF (bit10) IS now restored to the runtime cpu->df (formerly a documented M-gap): the direction
            // flag persists across block boundaries and a `popfq` of a value with bit10=1 followed by a
            // later-block `rep movs/stos/scas` copies BACKWARD correctly. g_df drops to DF_DYN because the
            // restored value is runtime (the string-op lowering will load cpu->df).
            if (op == 0x9D) {
                e_load(8, 16, RSP); // x16 = popped RFLAGS
                e_addi(RSP, RSP, 8, 1);
                e_movconst(17, 0);
                e_bit_move(17, 16, 6, 30, 18);                                // ZF(bit6) -> NZCV.Z(30)
                e_bit_move(17, 16, 7, 31, 18);                                // SF(bit7) -> NZCV.N(31)
                e_bit_move(17, 16, 11, 28, 18);                               // OF(bit11) -> NZCV.V(28)
                emit32(0x53000000u | (0 << 16) | (0 << 10) | (16 << 5) | 18); // ubfx w18,w16,#0,#1 (CF)
                e_movconst(19, 1);
                e_rrr(A_EOR, 18, 18, 19, 0, 0);  // stored borrow-C = NOT x86 CF
                e_rrr(A_ORR, 17, 17, 18, 0, 29); // -> NZCV.C(29)
                e_str(17, 28, OFF_NZCV);
                emit32(0xD51B4200u | 17);                                     // msr nzcv, x17  (sync live ARM flags)
                emit32(0x53000000u | (2 << 16) | (2 << 10) | (16 << 5) | 18); // ubfx w18,w16,#2,#1 (PF)
                e_movconst(19, 1);
                e_rrr(A_EOR, 18, 18, 19, 0, 0); // PF lane source byte = NOT PF (consumer takes even-parity)
                e_str(18, 28, OFF_PF);
                e_af_save(16); // AF from popped RFLAGS bit4 (cpu->af consumer extracts bit 4)
                emit32(0x53000000u | (21 << 16) | (21 << 10) | (16 << 5) | 18); // ubfx w18,w16,#21,#1 (ID)
                e_str(18, 28, OFF_ID);  // stash RFLAGS.ID so a later pushfq observes the toggle (CPUID probe)
                emit32(0x53000000u | (10 << 16) | (10 << 10) | (16 << 5) | 18); // ubfx w18,w16,#10,#1 (DF)
                e_str(18, 28, OFF_DF);  // restore the runtime direction flag (was a documented M-gap)
                g_df = DF_DYN;          // DF value is now runtime (popped) -> not statically known
                g_fl_pending = FL_NONE; // flags now materialized directly into cpu->nzcv
                gpc = next;
                continue;
            }
            // ===== x87 FPU (D8-DF): double-precision stack emulation =====
            if (op >= 0xD8 && op <= 0xDF) {
                mark_vdirty(); // x87 lowering touches the vector file -> mark cpu->V dirty
                int reg = I.reg & 7, rm = I.rm_reg & 7;
#define FAd(d, n, m) emit32(0x1E602800u | ((m) << 16) | ((n) << 5) | (d)) /* fadd d */
#define FSd(d, n, m) emit32(0x1E603800u | ((m) << 16) | ((n) << 5) | (d)) /* fsub d */
#define FMd(d, n, m) emit32(0x1E600800u | ((m) << 16) | ((n) << 5) | (d)) /* fmul d */
#define FDd(d, n, m) emit32(0x1E601800u | ((m) << 16) | ((n) << 5) | (d)) /* fdiv d */
// fucomi/fcomi/fucomip/fcomip set integer EFLAGS exactly like COMISD (ZF/PF/CF, unordered -> all 1), so
// use the same unordered+PF fixup (this also writes the real PF lane the setp/setnp consumers read).
#define FCMPd(n, m)                                                                                                    \
    do {                                                                                                               \
        emit32(0x1E602000u | ((m) << 16) | ((n) << 5));                                                                \
        e_nzcv_save_fcmp();                                                                                            \
    } while (0)
                if (I.is_mem) {
                    // x87 mem forms do a faulting guest load/store and the m80 forms exit to a C
                    // helper -- both can escape, so make cpu->fptop reflect the (pre-op) shadow first.
                    fp_materialize();
                    emit_ea(&I, next);
                    e_mov_rr(19, 17, 1); // x19 = EA (helpers clobber x17)
                    if (op == 0xD9) {    // f32 mem
                        if (reg == 0) {
                            e_ldr_s(16, 19);
                            emit32(0x1E22C000u | (16 << 5) | 16);
                            fp_push(16);
                        } // fld m32
                        else if (reg == 2 || reg == 3) {
                            fp_ld(16, 0);
                            emit32(0x1E624000u | (16 << 5) | 16);
                            e_str_s(16, 19);
                            if (reg == 3) fp_settop(1);
                        } // fst/fstp
                        else if (reg == 5) { // fldcw m16: load the x87 control word (RC/PC/exception masks)
                            emit32(0x79400000u | (19 << 5) | 16); // ldrh w16, [x19]
                            e_str(16, 28, OFF_FPCW);              // cpu->fpcw = CW (honored by fist rounding)
                        } else if (reg == 7) {                    // fnstcw m16: store the live x87 control word
                            e_ldr(16, 28, OFF_FPCW);
                            emit32(0x79000000u | (19 << 5) | 16); // strh w16, [x19]
                        } // fnstcw
                        else if (reg == 6) {
                            // FNSTENV m28: store the 28-byte 32-bit protected-mode x87 environment --
                            // FCW@0, FSW@4 (TOP in bits 11:13), FTW@8, then FIP/FCS/FOO/FOS @12/16/20/24.
                            // The engine keeps no per-reg tags, so emit the live FCW (cpu->fpcw, mirrors fnstcw)
                            // and FTW=0xffff (all-empty); the instruction/data pointers are zeroed. OpenBLAS (R
                            // startup, the only caller) just saves/restores FCW/FSW/FTW.
                            // x19 = EA. emit_fpsw_with_top() materializes cpu->fptop and yields FSW in x16.
                            e_ldr(16, 28, OFF_FPCW);
                            emit32(0x79000000u | (0u << 10) | (19 << 5) | 16); // strh FCW, [x19,#0]
                            emit_fpsw_with_top();                              // x16 = FSW | (TOP<<11)
                            emit32(0x79000000u | (2u << 10) | (19 << 5) | 16); // strh FSW, [x19,#4]
                            e_movconst(16, 0xffff);
                            emit32(0x79000000u | (4u << 10) | (19 << 5) | 16); // strh FTW, [x19,#8]
                            emit32(0xB9000000u | (3u << 10) | (19 << 5) | 31); // str  wzr, [x19,#12] FIP
                            emit32(0xB9000000u | (4u << 10) | (19 << 5) | 31); // str  wzr, [x19,#16] FCS
                            emit32(0xB9000000u | (5u << 10) | (19 << 5) | 31); // str  wzr, [x19,#20] FOO
                            emit32(0xB9000000u | (6u << 10) | (19 << 5) | 31); // str  wzr, [x19,#24] FOS
                            // FNSTENV then masks all FP exceptions in FCW; the engine's FCW is the fixed
                            // default 0x037f (exception-mask bits 0-5 already set), so nothing to update.
                        } // fnstenv m28
                        else if (reg == 4) {
                            // FLDENV m28: reload the x87 environment (inverse of FNSTENV). Restore the FCW
                            // (so a later fist rounds per the reloaded RC) and the FSW condition codes + TOP;
                            // FTW (no per-reg tags) is ignored. The reloaded TOP is unknown at translate
                            // time, so leave the static-top model -> runtime-top addressing afterwards.
                            fp_drop();
                            emit32(0x79400000u | (0u << 10) | (19 << 5) | 16); // ldrh w16, [x19,#0]  (FCW)
                            e_str(16, 28, OFF_FPCW);                           // cpu->fpcw = FCW
                            emit32(0x79400000u | (2u << 10) | (19 << 5) | 16); // ldrh w16, [x19,#4]  (FSW)
                            e_str(16, 28, OFF_FPSW);                           // cpu->fpsw  = FSW
                            emit32(0x53000000u | (11u << 16) | (13u << 10) | (16 << 5) | 17); // ubfx w17,w16,#11,#3
                            e_str(17, 28, OFF_FPTOP);                                         // cpu->fptop = TOP
                        } // fldenv m28
                        else {
                            report_unimpl(gpc, &I);
                            break;
                        }
                    } else if (op == 0xDD) { // f64 mem
                        if (reg == 0) {
                            e_ldr_d(16, 19);
                            fp_push(16);
                        } // fld m64
                        else if (reg == 2 || reg == 3) {
                            fp_ld(16, 0);
                            e_str_d(16, 19);
                            if (reg == 3) fp_settop(1);
                        } // fst/fstp
                        else if (reg == 7) {
                            emit_fpsw_with_top();
                            emit32(0x79000000u | (19 << 5) | 16);
                        } // fnstsw m16
                        else {
                            report_unimpl(gpc, &I);
                            break;
                        }
                    } else if (op == 0xDB) { // i32 mem / m80
                        if (reg == 0) {
                            emit32(0xB9400000u | (19 << 5) | 16);
                            emit32(0x1E620000u | (16 << 5) | 16);
                            fp_push(16);
                        } // fild m32
                        else if (reg == 2 || reg == 3) {
                            fp_ld(16, 0);
                            emit_x87_round_st0();                 // round per x87 control word (default: nearest)
                            emit32(0x1E780000u | (16 << 5) | 16); // FCVTZS w16,d16 (exact: d16 already integral)
                            emit32(0xB9000000u | (19 << 5) | 16);
                            if (reg == 3) fp_settop(1);
                        } // fist/fistp m32
                        else if (reg == 5) {
                            e_str(19, 28, OFF_X87EA);
                            emit_exit_const(next, R_X87FLD);
                            break;
                        } // fld m80 -> C
                        else if (reg == 7) {
                            e_str(19, 28, OFF_X87EA);
                            emit_exit_const(next, R_X87FSTP);
                            break;
                        } // fstp m80 -> C
                        else {
                            report_unimpl(gpc, &I);
                            break;
                        }
                    } else if (op == 0xDF) { // i16/i64 mem
                        if (reg == 0) {
                            emit32(0x79C00000u | (19 << 5) | 16);
                            emit32(0x1E620000u | (16 << 5) | 16);
                            fp_push(16);
                        } // fild m16 (ldrsh)
                        else if (reg == 3) {
                            fp_ld(16, 0);
                            emit_x87_round_st0();                 // round per x87 control word (default: nearest)
                            emit32(0x1E780000u | (16 << 5) | 16); // FCVTZS w16,d16 (exact: d16 already integral)
                            emit32(0x79000000u | (19 << 5) | 16);
                            fp_settop(1);
                        } // fistp m16
                        else if (reg == 5) {
                            e_ldr(16, 19, 0);
                            emit32(0x9E620000u | (16 << 5) | 16);
                            fp_push(16);
                        } // fild m64
                        else if (reg == 7) {
                            fp_ld(16, 0);
                            emit_x87_round_st0();                 // round per x87 control word (default: nearest)
                            emit32(0x9E780000u | (16 << 5) | 16); // FCVTZS x16,d16 (exact: d16 already integral)
                            e_str(16, 19, 0);
                            fp_settop(1);
                        } // fistp m64
                        else {
                            report_unimpl(gpc, &I);
                            break;
                        }
                    } else { // D8/DC/DA/DE arith with ST0: load the operand into d16 honoring its
                        // declared memory type -- m32/m64 float (D8/DC) or a SIGNED 32/16-bit integer
                        // (DA/DE: the fiadd/fimul/ficom/fisub/fidiv group) -- then share the reg-field
                        // arith dispatch below (identical fadd(0)/fmul(1)/fcom(2)/fcomp(3)/fsub(4)/
                        // fsubr(5)/fdiv(6)/fdivr(7) encoding for all four opcodes).
                        if (op == 0xD8) { // m32 float
                            e_ldr_s(16, 19);
                            emit32(0x1E22C000u | (16 << 5) | 16); // fcvt d16, s16
                        } else if (op == 0xDA) {                  // m32 signed integer
                            emit32(0xB9400000u | (19 << 5) | 16); // ldr   w16, [x19]
                            emit32(0x1E620000u | (16 << 5) | 16); // scvtf d16, w16
                        } else if (op == 0xDE) {                  // m16 signed integer
                            emit32(0x79C00000u | (19 << 5) | 16); // ldrsh w16, [x19]
                            emit32(0x1E620000u | (16 << 5) | 16); // scvtf d16, w16
                        } else                                    // 0xDC: m64 float
                            e_ldr_d(16, 19);
                        if (reg == 2 || reg == 3) {
                            fp_ld(18, 0);
                            e_fcom_setfpsw(18, 16);
                            if (reg == 3) fp_settop(1);
                            gpc = next;
                            continue;
                        } // fcom/fcomp
                        fp_ld(18, 0);
                        if (reg == 0)
                            FAd(18, 18, 16);
                        else if (reg == 1)
                            FMd(18, 18, 16);
                        else if (reg == 4)
                            FSd(18, 18, 16);
                        else if (reg == 5)
                            FSd(18, 16, 18);
                        else if (reg == 6)
                            FDd(18, 18, 16);
                        else if (reg == 7)
                            FDd(18, 16, 18);
                        else {
                            report_unimpl(gpc, &I);
                            break;
                        }
                        fp_st(18, 0);
                    }
                    gpc = next;
                    continue;
                }
                // ---- register forms (mod=3) ----
                if (op == 0xD9) {
                    if (reg == 0) {
                        fp_ld(16, rm);
                        fp_push(16);
                    } // fld ST(i)
                    else if (reg == 1) {
                        fp_ld(16, 0);
                        fp_ld(18, rm);
                        fp_st(18, 0);
                        fp_st(16, rm);
                    } // fxch
                    else if (reg == 4 && rm == 0) {
                        fp_ld(16, 0);
                        emit32(0x1E614000u | (16 << 5) | 16);
                        fp_st(16, 0);
                    } // fchs
                    else if (reg == 4 && rm == 1) {
                        fp_ld(16, 0);
                        emit32(0x1E60C000u | (16 << 5) | 16);
                        fp_st(16, 0);
                    } // fabs
                    else if (reg == 5) { // fld const
                        static const uint64_t k[8] = {0x3FF0000000000000ull /*1*/,
                                                      0x400A934F0979A371ull /*l2t*/,
                                                      0x3FF71547652B82FEull /*l2e*/,
                                                      0x400921FB54442D18ull /*pi*/,
                                                      0x3FD34413509F79FFull /*lg2*/,
                                                      0x3FE62E42FEFA39EFull /*ln2*/,
                                                      0x0ull /*0*/,
                                                      0x0ull};
                        e_movconst(16, k[rm]);
                        e_fmov_to_d(16, 16);
                        fp_push(16);
                    } else if (reg == 7 && rm == 2) {
                        fp_ld(16, 0);
                        emit32(0x1E61C000u | (16 << 5) | 16);
                        fp_st(16, 0);
                    } // fsqrt
                    else if (reg == 2 && rm == 0) { /* fnop */
                    } else if (reg == 4 && rm == 4) {
                        emit_ftst();
                    } // ftst
                    else if (reg == 4 && rm == 5) {
                        emit_fxam();
                    } // fxam
                    else if (reg == 6 && rm == 0) {
                        emit_x87func(X87_F2XM1, next);
                        break;
                    } // f2xm1
                    else if (reg == 6 && rm == 1) {
                        emit_x87func(X87_FYL2X, next);
                        break;
                    } // fyl2x
                    else if (reg == 6 && rm == 2) {
                        emit_x87func(X87_FPTAN, next);
                        break;
                    } // fptan
                    else if (reg == 6 && rm == 3) {
                        emit_x87func(X87_FPATAN, next);
                        break;
                    } // fpatan
                    else if (reg == 6 && rm == 4) {
                        emit_fxtract();
                    } // fxtract
                    else if (reg == 6 && rm == 5) {
                        emit_fprem(1);
                    } // fprem1
                    else if (reg == 6 && rm == 6) {
                        fp_settop(-1);
                    } // fdecstp
                    else if (reg == 6 && rm == 7) {
                        fp_settop(1);
                    } // fincstp
                    else if (reg == 7 && rm == 0) {
                        emit_fprem(0);
                    } // fprem
                    else if (reg == 7 && rm == 1) {
                        emit_x87func(X87_FYL2XP1, next);
                        break;
                    } // fyl2xp1
                    else if (reg == 7 && rm == 3) {
                        emit_x87func(X87_FSINCOS, next);
                        break;
                    } // fsincos
                    else if (reg == 7 && rm == 4) {
                        emit_frndint();
                    } // frndint
                    else if (reg == 7 && rm == 5) {
                        emit_fscale();
                    } // fscale
                    else if (reg == 7 && rm == 6) {
                        emit_x87func(X87_FSIN, next);
                        break;
                    } // fsin
                    else if (reg == 7 && rm == 7) {
                        emit_x87func(X87_FCOS, next);
                        break;
                    } // fcos
                    else {
                        report_unimpl(gpc, &I);
                        break;
                    }
                } else if (op == 0xD8 || op == 0xDC || op == 0xDE) { // arith ST0/ST(i) [+pop for DE]
                    fp_ld(18, 0);
                    fp_ld(16, rm);                     // v18=ST0, v16=ST(rm)
                    int dst_i = (op == 0xD8) ? 0 : rm; // D8 -> ST0; DC/DE -> ST(i)
                    if (reg == 2 || reg == 3) {
                        e_fcom_setfpsw(18, 16);
                        if (op == 0xDE && rm == 1) fp_settop(1);
                        if (reg == 3) fp_settop(1);
                        gpc = next;
                        continue;
                    } // fcom[p]/fcompp
                    int a = 18, b = 16;
                    if (op != 0xD8) {
                        a = 16;
                        b = 18;
                    } // DC/DE: dst=ST(i)=v16, other=ST0=v18
                    if (reg == 0)
                        FAd(a, a, b);
                    else if (reg == 1)
                        FMd(a, a, b);
                    else if (reg == 4) {
                        if (op == 0xD8)
                            FSd(a, a, b);
                        else
                            FSd(a, b, a);
                    } // DC/DE reverse sub
                    else if (reg == 5) {
                        if (op == 0xD8)
                            FSd(a, b, a);
                        else
                            FSd(a, a, b);
                    } else if (reg == 6) {
                        if (op == 0xD8)
                            FDd(a, a, b);
                        else
                            FDd(a, b, a);
                    } else if (reg == 7) {
                        if (op == 0xD8)
                            FDd(a, b, a);
                        else
                            FDd(a, a, b);
                    } else {
                        report_unimpl(gpc, &I);
                        break;
                    }
                    fp_st(a, dst_i);
                    if (op == 0xDE) fp_settop(1); // pop
                } else if (op == 0xDD) {
                    if (reg == 0) { /* ffree: no tag tracking -> nop */
                    } else if (reg == 2) {
                        fp_ld(16, 0);
                        fp_st(16, rm);
                    } // fst ST(i)
                    else if (reg == 3) {
                        fp_ld(16, 0);
                        fp_st(16, rm);
                        fp_settop(1);
                    } // fstp ST(i)
                    else if (reg == 4 || reg == 5) {
                        fp_ld(18, 0);
                        fp_ld(16, rm);
                        e_fcom_setfpsw(18, 16);
                        if (reg == 5) fp_settop(1);
                    } // fucom[p]
                    else {
                        report_unimpl(gpc, &I);
                        break;
                    }
                } else if (op == 0xDB) {
                    if (reg == 4 && rm == 3) {
                        e_movconst(16, 0);
                        e_str(16, 28, OFF_FPTOP);
                        if (x87opt_on()) { // anchor the translate-time shadow: top is now statically 0
                            g_fp_known = 1;
                            g_fp_top = 0;
                            g_fp_dirty = 0; // memory just written, shadow == cpu->fptop
                        }
                    } // finit -> top=0
                    else if (reg == 4) { /* fclex/etc */
                    } else if (reg == 5 || reg == 6) {
                        fp_ld(18, 0);
                        fp_ld(16, rm);
                        FCMPd(18, 16);
                    } // fucomi/fcomi
                    else {
                        report_unimpl(gpc, &I);
                        break;
                    }
                } else if (op == 0xDF) {
                    if (reg == 4 && rm == 0) {
                        emit_fpsw_with_top();
                        e_bfi(RAX, 16, 0, 16, 1);
                    } // fnstsw ax
                    else if (reg == 5 || reg == 6) {
                        fp_ld(18, 0);
                        fp_ld(16, rm);
                        FCMPd(18, 16);
                        fp_settop(1);
                    } // fucomip/fcomip
                    else {
                        report_unimpl(gpc, &I);
                        break;
                    }
                } else if (op == 0xDA) { // fcmovcc ST0,ST(i) (reg 0/1/2/3 = B/E/BE/U)
                    if (reg <= 3) {      // condition from integer EFLAGS
                        int jcc = (reg == 0) ? 2 : (reg == 1) ? 4 : (reg == 2) ? 6 : 10; // jb/je/jbe/jp
                        int armc = x86cc_to_arm(jcc);
                        e_nzcv_load();
                        fp_ld(18, 0);
                        fp_ld(16, rm); // v18=ST0, v16=ST(i)
                        emit32(0x1E600C00u | (18 << 16) | ((armc & 0xF) << 12) | (16 << 5) |
                               17); // fcsel d17, STi, ST0, cond
                        fp_st(17, 0);
                    } else if (reg == 5 && rm == 1) { // DA E9: fucompp (compare ST0,ST1; pop twice)
                        fp_ld(18, 0);
                        fp_ld(16, 1);
                        e_fcom_setfpsw(18, 16);
                        fp_settop(1);
                        fp_settop(1);
                    } else {
                        report_unimpl(gpc, &I);
                        break;
                    }
                } else {
                    report_unimpl(gpc, &I);
                    break;
                }
#undef FAd
#undef FSd
#undef FMd
#undef FDd
#undef FCMPd
                gpc = next;
                continue;
            }
            if (op == 0x90) {
                gpc = next;
                continue;
            }
            // ---- int3 (CC): software breakpoint -> #BP, a TRAP delivered as SIGTRAP. rip points PAST
            // the int3 (trap semantics). Emit a host BRK so the SIGTRAP guard runs the guest handler (or
            // default-terminates); previously this fell through to report_unimpl -> engine abort (70).
            if (op == 0xCC) {
                emit_guest_signal(next, 5, 0x80); // int3 -> SIGTRAP (si_code SI_KERNEL), rip past the int3
                break;
            }
            if (op == 0xF1) { // ICEBP/INT1 (#DB): userspace delivery is SIGTRAP with si_code TRAP_BRKPT (Linux
                emit_guest_signal(next, 5, 1); // send_sigtrap), rip past the insn (a trap, not a fault, not an abort)
                break;
            }
            if (op >= 0x91 && op <= 0x97) {
                int r = (op - 0x90) | (I.rexB << 3);
                e_mov_rr(19, RAX, sf);
                e_mov_rr(RAX, r, sf);
                e_mov_rr(r, 19, sf);
                gpc = next;
                continue;
            }
            // ---- cbw/cwde/cdqe (98) and cwd/cdq/cqo (99) ----
            if (op == 0x98) {
                if (sf)
                    e_sxt(RAX, RAX, 4); // cdqe: rax = sext32(eax)
                else if (I.p66) {
                    emit32(0x13001C00u | (RAX << 5) | 16); // cbw: x16 = sext8(AL) (sxtb Wd,Wn)
                    e_bfi(RAX, 16, 0, 16, 1);              // AX = low 16, PRESERVE bits 63:16
                } else
                    emit32(0x13003C00u | (RAX << 5) | RAX); // cwde: eax = sext16(ax) (sxth Wd,Wn)
                gpc = next;
                continue;
            }
            if (op == 0x99) {
                if (sf)
                    e_asr_i(RDX, RAX, 63, 1); // cqo: rdx = rax>>63 (arith)
                else if (I.p66) {
                    e_asr_i(19, RAX, 15, 0);
                    e_bfi(RDX, 19, 0, 16, 1);
                } // cwd: dx=sign(ax)
                else
                    e_asr_i(RDX, RAX, 31, 0); // cdq: edx = eax>>31 (arith)
                gpc = next;
                continue;
            }
        } else {
            // ===== two-byte (0F xx) =====
            if (op == 0x05) {
                if (g_fastsys) { // S1: inline time fast path (no service round-trip for clock_gettime/gettimeofday)
                    emit_fast_syscall(next);
                    // The inline-served path falls through here; end the block with a chained branch to
                    // `next` (regs stay live, no spill) instead of decoding inline -- decoding past the
                    // syscall would run the decoder off the end of guest .text (SIGBUS). The slow path
                    // inside emit_fast_syscall already ended the block via emit_exit_const(next,R_SYSCALL).
                    emit_chain_exit(next);
                    break;
                }
                emit_exit_const(next, R_SYSCALL);
                break;
            } // syscall
            if (op == 0x0B) {
                emit_sigill(gpc);
                break;
            } // ud2 -> guest SIGILL (terminate like real Linux), not an engine abort
            // ===== SSE / SSE2 (guest xmm0..15 == host v0..v15) =====
            // mandatory prefix selects the variant: 66=packed-int/double, F3=scalar-single,
            // F2=scalar-double, none=packed-single. reg/rm fields index xmm directly.
            {
                mark_vdirty(); // SSE lowering writes guest xmm (v0..v15) -> mark cpu->V dirty
                int handled = 1;
                int vd = I.reg, vm = I.rm_reg;
                if (op == 0x6E) { // movd/movq xmm, r/m  (66)
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        if (I.rexW)
                            e_ldr_d(vd, 17);
                        else
                            e_ldr_s(vd, 17);
                    } else {
                        if (I.rexW)
                            e_fmov_to_d(vd, I.rm_reg);
                        else
                            e_fmov_to_s(vd, I.rm_reg);
                    }
                } else if (op == 0x7E && I.rep) { // F3 0F 7E: movq xmm, xmm/m64
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_d(vd, 17);
                    } else
                        e_vmov8(vd, vm);
                } else if (op == 0x7E) { // 66 0F 7E: movd/movq r/m, xmm
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        if (I.rexW)
                            e_str_d(vd, 17);
                        else
                            e_str_s(vd, 17);
                    } else {
                        if (I.rexW)
                            e_fmov_from_d(I.rm_reg, vd);
                        else
                            e_fmov_from_s(I.rm_reg, vd);
                    }
                } else if (op == 0xD6) { // 66 0F D6: movq xmm/m64, xmm
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_str_d(vd, 17);
                    } else
                        e_vmov8(vm, vd);
                } else if (op == 0x6F && !I.p66 && !I.rep && !I.repne) { // MMX movq mm, mm/m64 (NO prefix): 64-bit
                    // Plain 0F 6F is the 64-bit MMX movq (66=movdqa / F3=movdqu are the 128-bit forms below).
                    // Loading/storing 128 bits here read/WROTE 8 bytes past the 64-bit MMX operand -> a store
                    // corrupted the adjacent 8 bytes of guest memory. Keep MMX at its architectural 64-bit width.
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_d(vd, 17);
                    } else
                        e_vmov8(vd, vm);
                } else if (op == 0x7F && !I.p66 && !I.rep && !I.repne) { // MMX movq mm/m64, mm (NO prefix): 64-bit
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_str_d(vd, 17); // 64-bit store: must NOT clobber the 8 bytes after the MMX destination
                    } else
                        e_vmov8(vm, vd);
                } else if (op == 0x6F || op == 0x28 || (op == 0x10 && !I.rep && !I.repne)) { // load 128 -> xmm
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(vd, 17, 0);
                    } else
                        e_vmov(vd, vm);
                } else if (op == 0x7F || op == 0x29 || (op == 0x11 && !I.rep && !I.repne)) { // store xmm -> 128
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_str_q(vd, 17, 0);
                    } else
                        e_vmov(vm, vd);
                } else if ((op == 0x10 || op == 0x11) && I.rep) { // movss (32-bit)
                    int st = (op == 0x11);
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        if (st)
                            e_str_s(vd, 17);
                        else
                            e_ldr_s(vd, 17);
                    } else {
                        if (st)
                            emit32(0x6E040400u | (vd << 5) | vm);
                        else
                            emit32(0x6E040400u | (vm << 5) | vd);
                    } // ins .s[0]
                } else if ((op == 0x10 || op == 0x11) && I.repne) { // movsd (64-bit)
                    int st = (op == 0x11);
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        if (st)
                            e_str_d(vd, 17);
                        else
                            e_ldr_d(vd, 17);
                    } else {
                        if (st)
                            emit32(0x6E080400u | (vd << 5) | vm);
                        else
                            emit32(0x6E080400u | (vm << 5) | vd);
                    } // ins .d[0]
                } else if (op == 0x12 || op == 0x16) { // movlps/movhps (load) or movhlps/movlhps (reg)
                    int lane = (op == 0x16) ? 1 : 0;   // 12->low lane(d[0]), 16->high lane(d[1])
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_d(16, 17);
                        e_ins_d(vd, lane, 16, 0);
                    } else {
                        int srclane = (op == 0x12) ? 1 : 0; // movhlps: d[0]<-src d[1]; movlhps: d[1]<-src d[0]
                        e_ins_d(vd, lane, vm, srclane);
                    }
                } else if (op == 0x13 || op == 0x17) { // movlps/movhps store
                    int lane = (op == 0x17) ? 1 : 0;
                    emit_ea(&I, next);
                    e_ins_d(16, 0, vd, lane);
                    e_str_d(16, 17);
                } else if (op == 0x54 || op == 0x55 || op == 0x56 ||
                           op == 0x57) { // andps/andnps/orps/xorps (FP bitwise)
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    if (op == 0x54)
                        e_v3(0x4E201C00u, vd, vd, s); // and
                    else if (op == 0x55)
                        e_v3(0x4E601C00u, vd, s, vd); // andn: ~vd & s -> bic vd,s,vd
                    else if (op == 0x56)
                        e_v3(0x4EA01C00u, vd, vd, s); // or
                    else
                        e_v3(0x6E201C00u, vd, vd, s); // xor
                } else if (op == 0xC6 && I.p66) {     // shufpd: 64-bit lanes (d[0]<-dst, d[1]<-src)
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    unsigned im = (unsigned)I.imm;
                    e_vmov(18, vd);
                    e_ins_d(17, 0, 18, im & 1);
                    e_ins_d(17, 1, s, (im >> 1) & 1);
                    e_vmov(vd, 17);
                } else if (op == 0xC6) { // shufps xmm,xmm/m,imm8 (lanes 0,1 from dst; 2,3 from src)
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    unsigned im = (unsigned)I.imm;
                    e_vmov(18, vd);
                    e_ins_s(17, 0, 18, im & 3);
                    e_ins_s(17, 1, 18, (im >> 2) & 3);
                    e_ins_s(17, 2, s, (im >> 4) & 3);
                    e_ins_s(17, 3, s, (im >> 6) & 3);
                    e_vmov(vd, 17);
                } else if (op == 0x71 || op == 0x72 || op == 0x73) { // psrl/psra/psll w/d/q by imm8; psrldq/pslldq
                    int sub = I.reg & 7,
                        esz = op == 0x71   ? 16
                              : op == 0x72 ? 32
                                           : 64,
                        sh = (int)(I.imm & 0xff), x = I.rm_reg;
                    if (sub == 2)
                        e_vshr_imm(x, x, esz, sh, 0); // psrl
                    else if (sub == 4)
                        e_vshr_imm(x, x, esz, sh, 1); // psra
                    else if (sub == 6)
                        e_vshl_imm(x, x, esz, sh);                   // psll
                    else if (op == 0x73 && (sub == 3 || sub == 7)) { // psrldq / pslldq (byte shifts)
                        if (sh > 15) {                               // x86: count > 15 -> result is all-zero
                            e_v3(0x6E201C00u, x, x, x);
                        } else if (sh) { // count 0 is the identity -> emit nothing
                            if (!g_v26z || nosseopt()) e_v3(0x6E201C00u, 26, 26, 26); // hoisted zero (crypto.c claim)
                            g_v26z = 1;
                            if (sub == 3)
                                e_ext(x, x, 26, sh); // psrldq
                            else
                                e_ext(x, 26, x, 16 - sh); // pslldq
                        }
                    } else {
                        report_unimpl(gpc, &I);
                        break;
                    }
                } else if (op == 0x70 && I.p66) { // pshufd xmm, xmm/m, imm8
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    unsigned im = (unsigned)I.imm & 0xff;
                    // AES-endgame perf: single-insn forms for the shuffles crypto/ghash loops actually use.
                    if (im == 0xE4) { // identity {0,1,2,3}
                        if (vd != s) e_vmov(vd, s);
                    } else if (im == 0x4E) { // {2,3,0,1}: swap 64-bit halves = EXT #8
                        e_ext(vd, s, s, 8);
                    } else if (im == 0xB1) { // {1,0,3,2}: swap dwords in each qword = REV64 .4s
                        emit32(0x4EA00800u | (s << 5) | vd);
                    } else if (im == 0x00 || im == 0x55 || im == 0xAA || im == 0xFF) { // broadcast one lane
                        e_dup_s(vd, s, (int)(im & 3));
                    } else if (vd != s) { // no self-overlap: build directly in the destination (4 insns)
                        for (int i = 0; i < 4; i++)
                            e_ins_s(vd, i, s, (im >> (2 * i)) & 3);
                    } else {
                        for (int i = 0; i < 4; i++)
                            e_ins_s(17, i, s, (im >> (2 * i)) & 3); // build in v17
                        e_vmov(vd, 17);
                    }
                } else if (op == 0x70 && (I.rep || I.repne)) { // pshufhw(F3=high) / pshuflw(F2=low): shuffle 4 words
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    unsigned im = (unsigned)I.imm;
                    int hi = I.rep; // F3 shuffles the HIGH 4 words, F2 the LOW 4
                    e_vmov(17, s);  // v17 = src (the un-shuffled half is preserved)
                    for (int i = 0; i < 4; i++) {
                        int dlane = hi ? 4 + i : i;
                        int slane = (hi ? 4 : 0) + (int)((im >> (2 * i)) & 3);
                        // INS v17.H[dlane], s.H[slane]
                        emit32(0x6E000400u | ((((unsigned)dlane << 2) | 2u) << 16) | (((unsigned)slane << 1) << 11) |
                               (s << 5) | 17);
                    }
                    e_vmov(vd, 17);
                } else if (op == 0xEF) { // pxor
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    e_v3(0x6E201C00u, vd, vd, s);
                } else if (op == 0xDB || op == 0xEB || op == 0xDF) { // pand / por / pandn
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    if (op == 0xDB)
                        e_v3(0x4E201C00u, vd, vd, s);
                    else if (op == 0xEB)
                        e_v3(0x4EA01C00u, vd, vd, s);
                    else
                        e_v3(0x4E601C00u, vd, s, vd);                // pandn: vd = ~vd & s  -> BIC vd, s, vd
                } else if (op == 0x74 || op == 0x75 || op == 0x76) { // pcmpeqb/w/d
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    uint32_t b = op == 0x74 ? 0x6E208C00u : op == 0x75 ? 0x6E608C00u : 0x6EA08C00u;
                    e_v3(b, vd, vd, s);
                } else if (op == 0x64 || op == 0x65 || op == 0x66) { // pcmpgtb/w/d -> CMGT (signed)
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    uint32_t b = op == 0x64 ? 0x4E203400u : op == 0x65 ? 0x4E603400u : 0x4EA03400u;
                    e_v3(b, vd, vd, s);
                } else if (op == 0xDE || op == 0xDA || op == 0xEE || op == 0xEA) { // pmaxub/pminub/pmaxsw/pminsw
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    uint32_t b = op == 0xDE   ? 0x6E206400u  // pmaxub -> UMAX  .16B (lane-wise, NOT UMAXP)
                                 : op == 0xDA ? 0x6E206C00u  // pminub -> UMIN  .16B (lane-wise, NOT UMINP)
                                 : op == 0xEE ? 0x4E606400u  // pmaxsw -> SMAX  .8H  (lane-wise, NOT SMAXP)
                                              : 0x4E606C00u; // pminsw -> SMIN  .8H  (lane-wise, NOT SMINP)
                    e_v3(b, vd, vd, s);
                } else if (op == 0xFC || op == 0xFD || op == 0xFE || op == 0xD4) { // paddb/w/d/q
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    uint32_t b = op == 0xFC   ? 0x4E208400u
                                 : op == 0xFD ? 0x4E608400u
                                 : op == 0xFE ? 0x4EA08400u
                                              : 0x4EE08400u;
                    e_v3(b, vd, vd, s);
                } else if (op == 0xF8 || op == 0xF9 || op == 0xFA || op == 0xFB) { // psubb/w/d/q
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    uint32_t b = op == 0xF8   ? 0x6E208400u
                                 : op == 0xF9 ? 0x6E608400u
                                 : op == 0xFA ? 0x6EA08400u
                                              : 0x6EE08400u;
                    e_v3(b, vd, vd, s);
                } else if (op == 0xDC || op == 0xDD || op == 0xEC || op == 0xED || op == 0xD8 || op == 0xD9 ||
                           op == 0xE8 || op == 0xE9) { // saturating add/sub: paddus/padds/psubus/psubs b/w
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    uint32_t b = op == 0xDC   ? 0x6E200C00u  // paddusb -> UQADD .16b
                                 : op == 0xDD ? 0x6E600C00u  // paddusw -> UQADD .8h
                                 : op == 0xEC ? 0x4E200C00u  // paddsb  -> SQADD .16b
                                 : op == 0xED ? 0x4E600C00u  // paddsw  -> SQADD .8h
                                 : op == 0xD8 ? 0x6E202C00u  // psubusb -> UQSUB .16b
                                 : op == 0xD9 ? 0x6E602C00u  // psubusw -> UQSUB .8h
                                 : op == 0xE8 ? 0x4E202C00u  // psubsb  -> SQSUB .16b
                                              : 0x4E602C00u; // psubsw  -> SQSUB .8h
                    e_v3(b, vd, vd, s);
                } else if (op == 0xE0 || op == 0xE3) { // pavgb/pavgw: unsigned rounding average -> URHADD
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    e_v3(op == 0xE0 ? 0x6E201400u : 0x6E601400u, vd, vd, s); // .16b : .8h
                } else if (op == 0xD5) { // pmullw: packed signed 16x16 -> low 16 bits -> MUL .8h
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    e_v3(0x4E609C00u, vd, vd, s);
                } else if (op == 0xE5 || op == 0xE4) { // pmulhw(signed)/pmulhuw(unsigned): 16x16 -> high 16 bits
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    // widen-multiply the low/high 4 lanes to 32-bit products, then UZP2 picks the high 16 of each.
                    uint32_t lo = op == 0xE5 ? 0x0E60C000u : 0x2E60C000u; // SMULL/UMULL  v18.4s, vd.4h, s.4h
                    uint32_t hi = op == 0xE5 ? 0x4E60C000u : 0x6E60C000u; // SMULL2/UMULL2 v19.4s, vd.8h, s.8h
                    emit32(lo | (s << 16) | (vd << 5) | 18);
                    emit32(hi | (s << 16) | (vd << 5) | 19);
                    emit32(0x4E405800u | (19 << 16) | (18 << 5) | vd); // uzp2 vd.8h, v18.8h, v19.8h
                } else if (op == 0xF5) { // pmaddwd: signed 16x16, add adjacent pairs -> 32-bit lanes
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    emit32(0x0E60C000u | (s << 16) | (vd << 5) | 18);  // smull  v18.4s, vd.4h, s.4h
                    emit32(0x4E60C000u | (s << 16) | (vd << 5) | 19);  // smull2 v19.4s, vd.8h, s.8h
                    emit32(0x4EA0BC00u | (19 << 16) | (18 << 5) | vd); // addp  vd.4s, v18.4s, v19.4s
                } else if (op == 0xF1 || op == 0xF2 || op == 0xF3 || op == 0xD1 || op == 0xD2 || op == 0xD3 ||
                           op == 0xE1 || op == 0xE2) { // psll/psrl/psra w/d/q by xmm/m (variable count)
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    int left = (op == 0xF1 || op == 0xF2 || op == 0xF3);
                    int arith = (op == 0xE1 || op == 0xE2);
                    int esize = (op == 0xF1 || op == 0xD1 || op == 0xE1)   ? 16
                                : (op == 0xF2 || op == 0xD2 || op == 0xE2) ? 32
                                                                           : 64;
                    e_sse_var_shift(vd, vd, s, esize, left, arith);
                } else if (op == 0x14 || op == 0x15) { // unpckl/hp{s,d}: interleave float lanes -> ZIP1/ZIP2
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    int hi = (op == 0x15);  // unpckh* -> ZIP2
                    int sz = I.p66 ? 3 : 2; // 66=pd (64-bit lanes, .2d); none=ps (32-bit lanes, .4s)
                    uint32_t b = (hi ? 0x4E007800u : 0x4E003800u) | ((uint32_t)sz << 22);
                    e_v3(b, vd, vd, s);
                } else if (op == 0xE6 && I.rep) { // cvtdq2pd (F3): low 2 packed s32 -> 2 packed f64
                    int s = vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_d(16, 17);
                        s = 16;
                    }
                    emit32(0x0F20A400u | (s << 5) | 16);  // SXTL v16.2d, vs.2s  (sign-extend the 2 int32)
                    emit32(0x4E61D800u | (16 << 5) | vd); // SCVTF vd.2d, v16.2d (int64 -> double)
                } else if (op == 0xE6 && (I.p66 || I.repne)) {
                    // cvttpd2dq (66, truncate) / cvtpd2dq (F2, round-to-nearest): 2 packed f64 -> 2 packed
                    // s32 in the low 64 bits of dst; the high 64 bits are zeroed (SQXTN with Q=0).
                    int s = vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                        s = 16;
                    }
                    uint32_t cvt = I.p66 ? 0x4EE1B800u  // FCVTZS v16.2d, vs.2d (toward zero)
                                         : 0x4E61A800u; // FCVTNS v16.2d, vs.2d (round to nearest even)
                    // x86 CVT(T)PD2DQ yields the integer indefinite (0x80000000) on NaN or out-of-range input;
                    // ARM FCVT*S saturates NaN->0 and SQXTN saturates positive overflow to INT32_MAX (negative
                    // overflow already agrees at INT32_MIN == indefinite). Per lane force 0x80000000 where the
                    // rounded s64 exceeds INT32_MAX (catches the round-up boundary too) OR the source is NaN. The
                    // NaN mask MUST be taken from the source doubles BEFORE the convert (which writes v16 and, for
                    // a memory operand where s==16, would otherwise clobber the doubles first).
                    emit32(0x4E60E400u | (s << 16) | (s << 5) | 18);   // FCMEQ v18.2d, s, s  -> ordered (0 where NaN)
                    emit32(0x6E205800u | (18 << 5) | 18);              // NOT  v18.16b       -> NaN mask
                    emit32(cvt | (s << 5) | 16);                       // FCVT*S v16.2d = cvt(s)  (rounded s64/lane)
                    e_movconst(19, 0x7fffffffull);
                    emit32(0x4E080C00u | (19 << 5) | 19);              // DUP  v19.2d, x19   -> INT32_MAX threshold
                    emit32(0x4EE03400u | (19 << 16) | (16 << 5) | 17); // CMGT v17.2d, v16.2d, v19.2d -> overflow mask
                    e_v3(0x4EA01C00u, 17, 17, 18);                     // ORR  v17.16b       -> make-indefinite mask
                    emit32(0x0EA12800u | (17 << 5) | 17);              // XTN  v17.2s, v17.2d (mask -> low 2 lanes)
                    emit32(0x0EA14800u | (16 << 5) | 20);              // SQXTN v20.2s, v16.2d (saturating result)
                    e_movconst(19, 0x80000000ull);
                    emit32(0x0E040C00u | (19 << 5) | 18);              // DUP  v18.2s, w19   -> 0x80000000 per lane
                    emit32(0x2E601C00u | (20 << 16) | (18 << 5) | 17); // BSL  v17.8b -> mask?indef:result (hi 64=0)
                    e_vmov(vd, 17);
                } else if (op == 0x60 || op == 0x61 || op == 0x62 || op == 0x6C || op == 0x68 || op == 0x69 ||
                           op == 0x6A || op == 0x6D) { // punpck l/h bw/wd/dq/qdq -> ZIP1/ZIP2
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    int hi = (op == 0x68 || op == 0x69 || op == 0x6A || op == 0x6D); // punpckh*; 0x6C(lqdq) is LOW
                    int sz = (op == 0x60 || op == 0x68)   ? 0
                             : (op == 0x61 || op == 0x69) ? 1
                             : (op == 0x62 || op == 0x6A) ? 2
                                                          : 3;
                    uint32_t b = (hi ? 0x4E007800u : 0x4E003800u) | ((uint32_t)sz << 22);
                    e_v3(b, vd, vd, s);
                } else if (op == 0x67 || op == 0x63 || op == 0x6B) {
                    // pack with saturation: 0x67 PACKUSWB (16->u8), 0x63 PACKSSWB (16->s8),
                    // 0x6B PACKSSDW (32->s16). dst.low half from dst's lanes, dst.high half from src's.
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    uint32_t sz = (op == 0x6B) ? 1u : 0u;     // source element: 0x6B = 16-bit, else 8-bit dest
                    uint32_t lo = (op == 0x67) ? 0x2E212800u  // SQXTUN  (signed->unsigned narrow)
                                               : 0x0E214800u; // SQXTN   (signed->signed narrow)
                    uint32_t hi = (op == 0x67) ? 0x6E212800u : 0x4E214800u; // ...2 (Q=1, high half)
                    emit32(lo | (sz << 22) | (vd << 5) | 17);               // narrow dst's lanes -> v17 low
                    emit32(hi | (sz << 22) | (s << 5) | 17);                // narrow src's lanes -> v17 high
                    e_vmov(vd, 17);
                } else if (op == 0xD7 && !nosseopt()) { // pmovmskb -> NEON (W3b SSE-SIMD idiom upgrade)
                    // Gather the 16 byte-MSBs into the low 16 bits of I.reg with a cascading
                    // shift-accumulate that needs NO memory round-trip and NO constant load
                    // (the proven sse2neon _mm_movemask_epi8 sequence). 7 host insns vs ~51.
                    //   ushr v17.16b, vm.16b, #7      ; t[i] = byte[i] MSB  (bit0 of each byte)
                    //   usra v17.8h,  v17.8h,  #7     ; pack 2 bits -> low byte of each halfword
                    //   usra v17.4s,  v17.4s,  #14    ; pack 4 bits -> low byte of each word
                    //   usra v17.2d,  v17.2d,  #28    ; pack 8 bits -> byte[0] and byte[8]
                    //   umov w16,   v17.b[0]          ; mask bits 0..7
                    //   umov wREG,  v17.b[8]          ; mask bits 8..15
                    //   orr  wREG,  w16, wREG, lsl #8 ; combine (W-form zero-extends to 64)
                    g_pmovmskb_n++;
                    e_vshr_imm(17, vm, 8, 7, 0);                           // ushr v17.16b, vm.16b, #7
                    emit32(0x6F001400u | (25u << 16) | (17 << 5) | 17);    // usra v17.8h, v17.8h, #7
                    emit32(0x6F001400u | (50u << 16) | (17 << 5) | 17);    // usra v17.4s, v17.4s, #14
                    emit32(0x6F001400u | (100u << 16) | (17 << 5) | 17);   // usra v17.2d, v17.2d, #28
                    emit32(0x0E003C00u | (1u << 16) | (17 << 5) | 16);     // umov w16, v17.b[0]
                    emit32(0x0E003C00u | (17u << 16) | (17 << 5) | I.reg); // umov wREG, v17.b[8]
                    e_rrr(A_ORR, I.reg, 16, I.reg, 0, 8);                  // orr wREG, w16, wREG, lsl #8
                } else if (op == 0xD7) {       // pmovmskb scalar fallback (NOSSEOPT=1 -> baseline codegen)
                    e_str_q(vm, 28, OFF_MM);   // spill the 16 bytes to scratch
                    e_addi(17, 28, OFF_MM, 1); // x17 = &scratch
                    e_movz(I.reg, 0, 0);       // result = 0
                    for (int i = 0; i < 16; i++) {
                        emit32(0x39400000u | ((unsigned)i << 10) | (17 << 5) | 16); // ldrb w16,[x17,#i]
                        emit32(0x53071C00u | (16 << 5) | 16);                       // ubfx w16,w16,#7,#1
                        emit32(0x2A000000u | (16 << 16) | ((unsigned)i << 10) | (I.reg << 5) |
                               I.reg); // orr reg,reg,w16,lsl#i
                    }
                } else if (op == 0x2A) { // cvtsi2sd/ss: int r/m -> xmm (F2=double,F3=single)
                    int src;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_load(I.rexW ? 8 : 4, 16, 17);
                        src = 16;
                    } else
                        src = I.rm_reg;
                    emit32(0x1E220000u | (I.rexW ? 0x80000000u : 0) | (I.repne ? 0x00400000u : 0) | (src << 5) |
                           vd);                        // scvtf vd,src
                } else if (op == 0x2C || op == 0x2D) { // cvttsd2si(2C trunc)/cvtsd2si(2D round): xmm/m -> GPR
                    int s = vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        if (I.repne)
                            e_ldr_d(16, 17);
                        else
                            e_ldr_s(16, 17);
                        s = 16;
                    }
                    if (op == 0x2D) { // cvtsd2si: honor MXCSR.RC -> round to integral (FRINTI uses FPCR.RMode)...
                        uint32_t frinti = I.repne ? 0x1E67C000u : 0x1E27C000u; // double : single
                        emit32(frinti | (s << 5) | 18);                        // frinti d18, ds
                        s = 18; // ...then FCVTZS the integral value (exact)
                    }
                    // FCVTZS (toward zero): exact truncation for 0x2C; for 0x2D the FRINTI value is already integral.
                    emit32(0x1E380000u | (I.rexW ? 0x80000000u : 0) | (I.repne ? 0x00400000u : 0) | (s << 5) | I.reg);
                    // H13: x86 float->int yields the "integer indefinite" (INT_MIN bit pattern) on any
                    // out-of-range or NaN input, whereas ARM FCVTZS saturates (positive overflow -> INT_MAX,
                    // NaN -> 0). Negative overflow already agrees (both give INT_MIN). Detect the divergent
                    // cases -- (s >= 2^(destbits-1)) OR unordered(NaN) -- with an FCMP against the threshold
                    // and substitute INT_MIN. FCMP sets ARM C on "greater-than-or-equal-or-unordered", so the
                    // CS condition (C==1) is exactly true iff s>=threshold or s is NaN. (Guest x86 flags are
                    // safely in cpu->nzcv here -- g_fl_pending was flushed at top-of-loop -- so the live ARM
                    // NZCV is free scratch, same as the ucomisd path.)
                    {
                        int sf = I.rexW ? 1 : 0; // dest is 64-bit signed int
                        uint64_t thr = I.repne ? (sf ? 0x43E0000000000000ull : 0x41E0000000000000ull)  // dbl 2^63/2^31
                                               : (sf ? 0x5F000000ull : 0x4F000000ull);                 // sgl 2^63/2^31
                        e_movconst(20, thr);
                        if (I.repne)
                            e_fmov_to_d(19, 20);
                        else
                            e_fmov_to_s(19, 20);                                                  // v19 = threshold
                        emit32((I.repne ? 0x1E602000u : 0x1E202000u) | (19 << 16) | (s << 5));    // FCMP s, v19
                        e_movconst(20, sf ? 0x8000000000000000ull : 0x80000000ull);               // integer indefinite
                        e_csel(I.reg, 20, I.reg, 2 /*CS: s>=thr or NaN*/, sf);
                    }
                } else if (op == 0x5D || op == 0x5F) { // H10: minps/maxps/minpd/maxpd + scalar minss/minsd/maxss/maxsd
                    // x86 MIN(a,b) = (a<b)?a:b ; MAX(a,b) = (a>b)?a:b -- and if either operand is NaN, or they
                    // compare equal (incl +0/-0), the result is the SECOND source (the r/m operand). ARM
                    // FMIN/FMAX instead quiet-propagate NaN and select +-0 by sign, so lower to a compare+select:
                    //   mask = (op==min) ? FCMGT(src2,dst) : FCMGT(dst,src2)   -> 0 on NaN/equal/+-0 -> pick src2
                    //   result = mask ? dst : src2   via BSL. Byte-exact with x86 on NaN/+-0.
                    int packed = !I.repne && !I.rep;
                    int s = vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        if (packed)
                            e_ldr_q(16, 17, 0);
                        else if (I.repne)
                            e_ldr_d(16, 17);
                        else
                            e_ldr_s(16, 17);
                        s = 16;
                    }
                    uint32_t szb = (packed ? I.p66 : I.repne) ? 0x00400000u : 0;
                    uint32_t GT = (packed ? 0x6EA0E400u : 0x7EA0E400u) | szb; // FCMGT (Rd = Rn > Rm)
                    if (op == 0x5D)
                        emit32(GT | (vd << 16) | (s << 5) | 17); // v17 = (src2 > dst)  [min mask]
                    else
                        emit32(GT | (s << 16) | (vd << 5) | 17); // v17 = (dst > src2)  [max mask]
                    if (packed) {
                        e_v3(0x6E601C00u, 17, vd, s); // BSL v17.16b, dst.16b, src2.16b -> mask?dst:src2
                        e_vmov(vd, 17);
                    } else {
                        e_v3(0x2E601C00u, 17, vd, s); // BSL v17.8b (low lane) -> mask?dst:src2
                        // FMOV Dd/Sd, v17: keep only the low lane, zero the upper bits (matches the scalar
                        // arithmetic path's upper-lane convention).
                        emit32((I.repne ? 0x1E604000u : 0x1E204000u) | (17 << 5) | vd);
                    }
                } else if (op == 0x58 || op == 0x59 || op == 0x5C || op == 0x5E || op == 0x51) {
                    // add/mul/sub/div/min/max/sqrt. Prefix selects width: F2=scalar double, F3=scalar
                    // single, 66=PACKED double (.2d), none=PACKED single (.4s).
                    int packed = !I.repne && !I.rep;
                    int s = vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        if (packed)
                            e_ldr_q(16, 17, 0);
                        else if (I.repne)
                            e_ldr_d(16, 17);
                        else
                            e_ldr_s(16, 17);
                        s = 16;
                    }
                    int dbl = packed ? I.p66 : I.repne; // element type: double vs single
                    int fixnan = fpdnan_on();
                    if (fixnan) emit_dnan_pre(vd, s, op != 0x51, dbl); // capture "no input NaN" (uses v20/v21)
                    if (packed) { // vector FP: 66 -> .2d (sz bit), none -> .4s
                        uint32_t d = I.p66 ? 0x00400000u : 0;
                        uint32_t b = op == 0x58   ? 0x4E20D400u  // FADD
                                     : op == 0x59 ? 0x6E20DC00u  // FMUL
                                     : op == 0x5C ? 0x4EA0D400u  // FSUB
                                     : op == 0x5E ? 0x6E20FC00u  // FDIV
                                                  : 0x6EA1F800u; // FSQRT (2-reg)  [min/max: see 0x5D/0x5F above]
                        if (op == 0x51)
                            emit32(b | d | (s << 5) | vd); // FSQRT vd.T, s.T
                        else
                            emit32(b | d | (s << 16) | (vd << 5) | vd); // op vd.T, vd.T, s.T
                    } else {                                            // scalar FP: F2=double, F3=single
                        uint32_t ty = I.repne ? 0x00400000u : 0;
                        uint32_t b = op == 0x58   ? 0x1E202800u
                                     : op == 0x59 ? 0x1E200800u
                                     : op == 0x5C ? 0x1E203800u
                                     : op == 0x5E ? 0x1E201800u
                                                  : 0x1E21C000u; // FSQRT [min/max: see 0x5D/0x5F above]
                        if (op == 0x51)
                            emit32(b | ty | (s << 5) | vd); // FSQRT sd/ss, s
                        else
                            emit32(b | ty | (s << 16) | (vd << 5) | vd); // FADD/.../FMAX sd/ss
                    }
                    if (fixnan) emit_dnan_post(vd, dbl); // stamp x86's negative default-NaN sign on generated NaNs
                } else if (op == 0x5A) { // cvtsd2ss(F2) / cvtss2sd(F3)
                    int s = vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        if (I.repne)
                            e_ldr_d(16, 17);
                        else
                            e_ldr_s(16, 17);
                        s = 16;
                    }
                    if (I.repne)
                        emit32(0x1E624000u | (s << 5) | vd); // FCVT Sd, Dn (double->single)
                    else
                        emit32(0x1E22C000u | (s << 5) | vd); // FCVT Dd, Sn (single->double)
                } else if (op == 0xC4) { // pinsrw: insert low 16 bits of r/m16 into xmm H-lane (imm8 & 7)
                    int lane = (int)I.imm & 7;
                    int src;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_load(2, 16, 17); // w16 = [addr] (16-bit)
                        src = 16;
                    } else {
                        src = I.rm_reg; // guest GPR mapped to host reg
                    }
                    // INS vd.H[lane], Wsrc  (imm5 = lane<<2 | 0b10 selects H)
                    emit32(0x4E001C00u | ((((unsigned)lane << 2) | 2u) << 16) | (src << 5) | vd);
                } else if (op == 0xC5) { // pextrw: extract xmm H-lane (imm8 & 7) -> r32, zero-extended (reg src only)
                    int lane = (int)I.imm & 7;
                    // UMOV Wreg, Vm.H[lane]  (imm5 = lane<<2 | 0b10 selects H; zero-extends into the GPR)
                    emit32(0x0E003C00u | ((((unsigned)lane << 2) | 2u) << 16) | (vm << 5) | I.reg);
                } else if (op == 0xC2) { // cmpps/pd/ss/sd: FP compare with predicate imm -> all-1s/0 mask
                    int packed = !I.repne && !I.rep;
                    int s = vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        if (packed)
                            e_ldr_q(16, 17, 0);
                        else if (I.repne)
                            e_ldr_d(16, 17);
                        else
                            e_ldr_s(16, 17);
                        s = 16;
                    }
                    int pred = (int)I.imm & 7;
                    // sz bit (bit22): packed 66 / scalar F2 -> double, else single
                    uint32_t szb = (packed ? I.p66 : I.repne) ? 0x00400000u : 0;
                    uint32_t EQ = (packed ? 0x4E20E400u : 0x5E20E400u) | szb; // FCMEQ
                    uint32_t GE = (packed ? 0x6E20E400u : 0x7E20E400u) | szb; // FCMGE
                    uint32_t GT = (packed ? 0x6EA0E400u : 0x7EA0E400u) | szb; // FCMGT
                    uint32_t ANDb = packed ? 0x4E201C00u : 0x0E201C00u;       // AND Vd.16b/8b
                    uint32_t NOTb = packed ? 0x6E205800u : 0x2E205800u;       // NOT (MVN) Vd.16b/8b
                    if (pred == 3 || pred == 7) {                             // UNORD/ORD: ordered(a)&ordered(b)
                        emit32(EQ | (vd << 16) | (vd << 5) | 17);             // v17 = a==a (ordered a)
                        emit32(EQ | (s << 16) | (s << 5) | vd);               // vd  = b==b (ordered b)
                        emit32(ANDb | (17 << 16) | (vd << 5) | vd);           // vd  = ORD
                        if (pred == 3) emit32(NOTb | (vd << 5) | vd);         // UNORD = ~ORD
                    } else {
                        // predicates handled here: 0 EQ, 1 LT, 2 LE, 4 NEQ, 5 NLT, 6 NLE.
                        // LT/LE/NLT/NLE build the ordered comparison a<b / a<=b via the swapped GT/GE (a<b ==
                        // b>a); NEQ/NLT/NLE then invert. x86's N-forms are UNORDERED: they return all-ones when
                        // an operand is NaN. ARM FCMGT/FCMGE give 0 on NaN, so inverting the ordered result (NOT)
                        // yields the correct NaN->true mask for NLT/NLE (H12) exactly as it already did for NEQ.
                        int lt_like = (pred == 1 || pred == 2 || pred == 5 || pred == 6);
                        int use_ge = (pred == 2 || pred == 6);            // LE/NLE -> GE ; LT/NLT -> GT
                        int neg = (pred == 4 || pred == 5 || pred == 6);  // NEQ/NLT/NLE invert (NaN -> true)
                        int n = lt_like ? s : vd, m = lt_like ? vd : s;
                        uint32_t fc = (pred == 0 || pred == 4) ? EQ : use_ge ? GE : GT;
                        emit32(fc | (m << 16) | (n << 5) | vd);   // FCMxx vd, n, m
                        if (neg) emit32(NOTb | (vd << 5) | vd);   // invert -> NaN lane becomes all-ones
                    }
                } else if (op == 0x2E || op == 0x2F) { // ucomisd/comisd (66=double, none=single) -> FCMP + flags
                    int s = vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        if (I.p66)
                            e_ldr_d(16, 17);
                        else
                            e_ldr_s(16, 17);
                        s = 16;
                    }
                    emit32((I.p66 ? 0x1E602000u : 0x1E202000u) | (s << 16) | (vd << 5)); // FCMP Dvd, Ds  (Rd=0)
                    e_nzcv_save_fcmp();  // unordered fixup: x86 ZF=PF=CF=1, SF=0 (ARM FCMP gives N0 Z0 C1 V1)
                } else if (op == 0xF4) { // pmuludq: vd.u64[i] = (u32)vd.even32[i] * (u32)src.even32[i]
                    // W3b: was UNIMPL -> blocked glibc strchr/strrchr (byte-broadcast via pmuludq).
                    // Gather the even (0,2) 32-bit lanes of each operand into the low 2 lanes (UZP1),
                    // then widening multiply -> two 64-bit products. Bit-exact, 3 NEON insns.
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    emit32(0x4E801800u | (vd << 16) | (vd << 5) | 17); // uzp1 v17.4s, vd.4s, vd.4s -> [d0,d2,..]
                    emit32(0x4E801800u | (s << 16) | (s << 5) | 18);   // uzp1 v18.4s, s.4s,  s.4s  -> [s0,s2,..]
                    emit32(0x2EA0C000u | (18 << 16) | (17 << 5) | vd); // umull vd.2d, v17.2s, v18.2s
                } else if (op == 0x50) {               // movmskps(NP)/movmskpd(66): pack sign bits -> GPR
                    if (I.p66) {                       // 2 doubles -> low 2 bits
                        e_vshr_imm(17, vm, 64, 63, 0); // ushr v17.2d, vm.2d, #63
                        emit32(0x4E003C00u | ((0u * 16 + 8) << 16) | (17 << 5) | I.reg); // umov xREG, v17.d[0]
                        emit32(0x4E003C00u | ((1u * 16 + 8) << 16) | (17 << 5) | 19);    // umov x19, v17.d[1]
                        e_rrr(A_ORR, I.reg, I.reg, 19, 1, 1);                            // orr REG, REG, x19, lsl#1
                    } else {                                                             // 4 floats -> low 4 bits
                        e_vshr_imm(17, vm, 32, 31, 0);                                   // ushr v17.4s, vm.4s, #31
                        emit32(0x0E003C00u | ((0u * 8 + 4) << 16) | (17 << 5) | I.reg);  // umov wREG, v17.s[0]
                        for (int i = 1; i < 4; i++) {
                            emit32(0x0E003C00u | (((unsigned)i * 8 + 4) << 16) | (17 << 5) | 19); // umov w19, v17.s[i]
                            e_rrr(A_ORR, I.reg, I.reg, 19, 0, i); // orr wREG, wREG, w19, lsl#i
                        }
                    }
                } else if (op == 0x5B) { // cvtdq2ps(NP)/cvtps2dq(66)/cvttps2dq(F3): packed 4-lane int<->float
                    int s = vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                        s = 16;
                    }
                    if (I.rep)
                        emit32(0x4EA1B800u | (s << 5) | vd); // F3: cvttps2dq -> FCVTZS .4S (truncate)
                    else if (I.p66)
                        emit32(0x4E21A800u | (s << 5) | vd); // 66: cvtps2dq  -> FCVTNS .4S (round to nearest)
                    else
                        emit32(0x4E21D800u | (s << 5) | vd); // NP: cvtdq2ps  -> SCVTF  .4S (s32->f32)
                    if (I.rep || I.p66) {
                        // H13 (packed): x86 float->int32 gives the integer indefinite (0x80000000) per lane on
                        // out-of-range or NaN; ARM saturates positive overflow to INT_MAX and NaN to 0 (negative
                        // overflow already agrees at INT_MIN). Per lane, build the "make-indefinite" mask =
                        //   (s >= 2^31)  OR  (s != s)   -- FCMGE against a 2^31 broadcast, ORed with ~FCMEQ(s,s)
                        // and BSL 0x80000000 over the saturating result.
                        e_v3(0x4E20E400u, 17, s, s);       // FCMEQ v17.4s, s, s  -> ordered lanes (0 where NaN)
                        emit32(0x6E205800u | (17 << 5) | 17); // NOT v17.16b       -> NaN mask
                        e_movconst(19, 0x4F000000u);          // 2^31 as f32
                        emit32(0x4E040C00u | (19 << 5) | 18); // DUP v18.4s, w19   -> threshold
                        e_v3(0x6E20E400u, 18, s, 18);         // FCMGE v18.4s, s, thr -> s>=2^31 mask
                        e_v3(0x4EA01C00u, 17, 17, 18);        // ORR v17.16b       -> combined make-indefinite mask
                        e_movconst(19, 0x80000000u);
                        emit32(0x4E040C00u | (19 << 5) | 18); // DUP v18.4s, w19   -> 0x80000000 per lane
                        e_v3(0x6E601C00u, 17, 18, vd);        // BSL v17.16b, indef, result -> mask?indef:result
                        e_vmov(vd, 17);
                    }
                } else if (op == 0xF6) {                     // psadbw (66): sum of abs byte diffs per 64-bit half
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_ldr_q(16, 17, 0);
                    }
                    emit32(0x6E207400u | (s << 16) | (vd << 5) | 17); // uabd   v17.16b, vd.16b, s.16b
                    emit32(0x6E202800u | (17 << 5) | 17);             // uaddlp v17.8h,  v17.16b
                    emit32(0x6E602800u | (17 << 5) | 17);             // uaddlp v17.4s,  v17.8h
                    emit32(0x6EA02800u | (17 << 5) | 17);             // uaddlp v17.2d,  v17.4s
                    e_vmov(vd, 17);
                } else if (op == 0xE7 && I.p66) { // movntdq (66): non-temporal store xmm -> m128
                    emit_ea(&I, next);
                    e_str_q(vd, 17, 0);
                } else if (op == 0xF7 && I.p66) { // maskmovdqu (66): per-byte masked store xmm(vd) -> [RDI],
                    // mask = xmm(vm); only each mask byte's MSB selects. Read-modify-write blend at [RDI]
                    // (the region is writable; unselected bytes keep their memory value == architecturally
                    // "not stored"). sel = sshr(mask,#7) -> 0xFF where store; BSL sel?src:mem; store back.
                    e_vshr_imm(18, vm, 8, 7, 1);     // sshr v18.16b, vmask.16b, #7
                    e_mov_rr(17, RDI, 1);            // x17 = RDI (guest addr == host addr, in-process)
                    e_ldr_q(16, 17, 0);              // v16 = [RDI]
                    e_v3(0x6E601C00u, 18, vd, 16);   // bsl v18.16b, vsrc.16b, v16.16b (sel?src:mem)
                    e_str_q(18, 17, 0);              // [RDI] = blended
                } else if (op == 0x2B && I.is_mem) { // movntps (NP) / movntpd (66): non-temporal store xmm -> m128
                    emit_ea(&I, next);               // (aligned, non-temporal -> a plain 128-bit store on ARM;)
                    e_str_q(vd, 17, 0);
                } else
                    handled = 0;
                if (handled) {
                    gpc = next;
                    continue;
                }
            }
            if (op == 0xA2) {
                emit_exit_const(next, R_CPUID);
                break;
            } // cpuid -> dispatcher helper
            if (op == 0x31) {             // rdtsc: edx:eax = cntvct
                emit32(0xD53BE040u | 16); // mrs x16, cntvct_el0
                e_mov_rr(RAX, 16, 0);
                e_lsr_i(RDX, 16, 32, 1);
                gpc = next;
                continue;
            }
            if (op == 0x01 && I.has_modrm && I.modrm == 0xF9) { // rdtscp: edx:eax = cntvct, ecx = TSC_AUX (0)
                emit32(0xD53BE040u | 16);                       // mrs x16, cntvct_el0
                e_mov_rr(RAX, 16, 0);
                e_lsr_i(RDX, 16, 32, 1);
                e_movz(RCX, 0, 0); // TSC_AUX = 0
                gpc = next;
                continue;
            }
            if (op == 0x01 && I.has_modrm && I.modrm == 0xD0) { // xgetbv (ecx=0): XCR0 = x87+SSE (no AVX)
                e_movz(RAX, 3, 0);
                e_movz(RDX, 0, 0);
                gpc = next;
                continue;
            }
            if (op == 0x01 && I.has_modrm && I.modrm == 0xD5) { // xend (TSX): no transaction -> NOP
                gpc = next;
                continue;
            }
            if (op == 0xC3) { // movnti: non-temporal store r32/r64 -> m
                emit_ea(&I, next);
                e_store(I.opsize, I.reg, 17);
                gpc = next;
                continue;
            }
            if (op == 0xC7 && (I.reg & 7) == 1 && I.is_mem) { // cmpxchg16b: REX.W 0F C7 /1 (128-bit compare+swap)
                // x86 `lock cmpxchg16b` must be a single ATOMIC 128-bit compare-exchange across guest threads.
                // A two-loads-plus-stores sequence tears, and a hardware CASPAL LIVELOCKS on Apple Silicon
                // (128-bit store-forwarding replay). So stash the operand EA and exit to do_cmpxchg16(), which
                // does the DWCAS under a hashed spinlock (a 64-bit atomic lock is replay-immune + livelock-free).
                // cmpxchg16b affects ONLY ZF, so materialize any lazy flags first (the C helper edits ZF alone).
                if (g_fl_pending) flags_materialize();
                emit_ea(&I, next); // x17 = EA
                e_str(17, 28, OFF_X87EA);
                emit_exit_const(next, R_CMPXCHG16);
                break;
            }
            if (op == 0x1E && I.imm_bytes == 0) {
                gpc = next;
                continue;
            } // endbr (modrm consumed)
            if (op == 0x1F) {
                gpc = next;
                continue;
            } // nop r/m
            if (op == 0x18 || op == 0x0D || (op >= 0x19 && op <= 0x1D)) {
                gpc = next;
                continue;
            } // prefetch{nta,t0,t1,t2} (0F 18) / prefetchw (0F 0D) / reserved multi-byte NOP hints — hint only -> NOP
            // shld/shrd (0F A4 imm8, 0F A5 cl, 0F AC imm8, 0F AD cl):  dst=r/m, src=reg, count
            if (op == 0xA4 || op == 0xA5 || op == 0xAC || op == 0xAD) {
                int isleft = (op == 0xA4 || op == 0xA5), bycl = (op == 0xA5 || op == 0xAD);
                int w = I.opsize, mem;
                if (w == 2) {
                    // 16-bit SHLD/SHRD: EXTR can't do 16-bit lanes, so build a 32-bit concatenation and
                    // shift it. SHLD: t = (dst<<16)|src; t<<=n; result = t>>16. SHRD: t = (src<<16)|dst;
                    // t>>=n; result = t&0xffff. Exact for n in [0,16] (x86 leaves n>15 undefined for 16-bit).
                    int dst = rm_load(&I, next, 2, &mem), src = I.reg;
                    e_uxt(19, dst, 2); // x19 = dst & 0xffff
                    e_uxt(20, src, 2); // x20 = src & 0xffff
                    if (!bycl) {
                        int n = (int)(I.imm & 31);
                        if (n == 0) {
                            if (mem) e_store(2, dst, 17);
                            gpc = next;
                            continue;
                        } // count 0 -> no change, flags intact
                        if (isleft) {
                            e_lsl_i(19, 19, 16, 0);         // dst<<16
                            e_rrr(A_ORR, 19, 19, 20, 0, 0); // (dst<<16)|src
                            e_lsl_i(19, 19, n, 0);          // <<= n
                            e_lsr_i(16, 19, 16, 0);         // result = >>16
                        } else {
                            e_lsl_i(20, 20, 16, 0);         // src<<16
                            e_rrr(A_ORR, 19, 20, 19, 0, 0); // (src<<16)|dst
                            e_lsr_i(16, 19, n, 0);          // >>= n (low 16 = result)
                        }
                    } else {
                        e_movconst(23, 31);
                        e_rrr(A_AND, 17, RCX, 23, 0, 0); // n = cl & 31
                        if (isleft) {
                            e_lsl_i(19, 19, 16, 0);
                            e_rrr(A_ORR, 19, 19, 20, 0, 0); // (dst<<16)|src
                            e_shv(S_LSLV, 19, 19, 17, 0);   // <<= n
                            e_lsr_i(16, 19, 16, 0);
                        } else {
                            e_lsl_i(20, 20, 16, 0);
                            e_rrr(A_ORR, 19, 20, 19, 0, 0); // (src<<16)|dst
                            e_shv(S_LSRV, 16, 19, 17, 0);   // >>= n
                        }
                        // n==0: dst unchanged. The concat-shift already yields dst for n==0, so no csel needed.
                    }
                    e_lsl_i(21, 16, 16, 0); // 16-bit SF/ZF via high-bit test
                    e_tst(21, 0);
                    e_nzcv_save();
                    rm_store(&I, 2, 16);
                    gpc = next;
                    continue;
                }
                int ssf = (w == 8) ? 1 : 0, width = ssf ? 64 : 32;
                int dst = rm_load(&I, next, w, &mem), src = I.reg;
                if (!bycl) {
                    int n = (int)(I.imm & (ssf ? 63 : 31));
                    if (n == 0) {
                        if (mem) e_store(w, dst, 17);
                        gpc = next;
                        continue;
                    } // count 0 -> no change, flags intact
                    if (isleft)
                        e_extr(16, dst, src, width - n, ssf); // (dst<<n)|(src>>(W-n))
                    else
                        e_extr(16, src, dst, n, ssf); // (dst>>n)|(src<<(W-n))
                    // M: x86 flags. SF/ZF/PF from the result; CF = the LAST bit shifted out of the ORIGINAL
                    // dst -- SHLD: bit (W-n); SHRD: bit (n-1). n is a nonzero constant here. OF is defined
                    // only for n==1 (sign change); left undefined for the general case as x86 permits.
                    e_lsr_i(21, dst, isleft ? (width - n) : (n - 1), ssf);
                    e_movconst(19, 1);
                    e_rrr(A_AND, 21, 21, 19, 0, 0); // x21 = x86 CF (0/1)
                    e_tst(16, ssf);                 // N/Z from result
                    e_pf_save(16);                  // PF source = result low byte
                    e_nzcv_save_setcf(21);          // stored C = NOT CF, keep N/Z
                    rm_store(&I, w, 16);
                    gpc = next;
                    continue;
                }
                // ---- SHLD/SHRD by CL ----
                e_mov_rr(22, dst, ssf); // preserve orig dst for the n==0 select + CF
                e_movconst(19, ssf ? 63 : 31);
                e_rrr(A_AND, 17, RCX, 19, ssf, 0); // n = cl & (W-1)
                e_movconst(20, width);
                e_rrr(A_SUB, 20, 20, 17, ssf, 0); // 20 = W - n
                if (isleft) {
                    e_shv(S_LSLV, 19, dst, 17, ssf);
                    e_shv(S_LSRV, 20, src, 20, ssf);
                } else {
                    e_shv(S_LSRV, 19, dst, 17, ssf);
                    e_shv(S_LSLV, 20, src, 20, ssf);
                }
                e_rrr(A_ORR, 16, 19, 20, ssf, 0); // combined = t1 | t2
                e_tst(17, ssf);
                e_csel(16, 22, 16, 0 /*EQ: n==0*/, ssf); // n==0 -> dst unchanged
                // M: x86 flags. If the masked count n==0 ALL flags are unchanged; else SF/ZF/PF from the
                // result and CF = the last bit shifted out of the ORIGINAL dst (x22): SHLD bit (W-n), SHRD
                // bit (n-1). OF (n==1 only) left undefined. Mirrors the SHL/SHR/SAR count==0-preserve path.
                e_ldr(24, 28, OFF_NZCV);  // old stored flags (kept when n==0)
                e_tst(16, ssf);           // live N/Z from result
                emit32(0xD53B4200u | 20); // mrs x20, nzcv (N/Z valid; C/V stale)
                if (isleft) {
                    e_movconst(19, width);
                    e_rrr(A_SUB, 19, 19, 17, ssf, 0); // x19 = W - n
                } else {
                    e_subi(19, 17, 1, ssf); // x19 = n - 1
                }
                e_shv(S_LSRV, 21, 22, 19, ssf);
                e_movconst(19, 1);
                e_rrr(A_AND, 21, 21, 19, 0, 0);  // x21 = x86 CF (0/1)
                e_rrr(A_EOR, 21, 21, 19, 0, 0);  // x21 = NOT CF (stored borrow convention)
                e_movconst(19, 1u << 29);
                e_rrr(A_BIC, 20, 20, 19, 1, 0);  // clear stored C (bit 29)
                e_rrr(A_ORR, 20, 20, 21, 1, 29); // stored C = (NOT CF) << 29
                e_tst(17, ssf);                  // Z = (n == 0)
                e_csel(20, 24, 20, 0 /*EQ*/, 1); // n==0 -> keep old flags
                e_str(20, 28, OFF_NZCV);
                if (!g_pfaf_dead) { // PF: n==0 keeps old, else result low byte (live Z still = n==0 here)
                    e_ldr(25, 28, OFF_PF);
                    e_csel(23, 25, 16, 0 /*EQ*/, 1);
                    e_pf_save(23);
                }
                emit32(0xD51B4200u | 20); // sync live ARM NZCV to the stored value
                rm_store(&I, w, 16);
                gpc = next;
                continue;
            }
            // imul reg, r/m (0F AF)
            if (op == 0xAF) {
                int mem;
                int rmv = rm_load(&I, next, I.opsize, &mem);
                int af_co_live = !xblkalu_elide_on() || (x86_flags_livein(next, gpc) & XF_NZCV);
                e_imul2(I.reg, I.reg, rmv, I.opsize, af_co_live); // reg *= r/m, sets x86 CF/OF on overflow
                gpc = next;
                continue;
            }
            // bswap (0F C8+r): byte-reverse a register -> ARM REV
            if (op >= 0xC8 && op <= 0xCF) {
                int r = (op - 0xC8) | (I.rexB << 3);
                emit32((sf ? 0xDAC00C00u : 0x5AC00800u) | (r << 5) | r);
                gpc = next;
                continue;
            }
            // 0F AE: fences (lfence/mfence/sfence -> dmb), ldmxcsr/stmxcsr, fxsave/fxrstor (xmm area)
            if (op == 0xAE) {
                int sub = I.reg & 7;
                if (sub >= 5) {
                    emit32(0xD5033BBFu);
                    gpc = next;
                    continue;
                } // *fence -> dmb ish
                if (sub == 2) { // ldmxcsr: thread MXCSR.RC (bits 14:13) -> ARM FPCR.RMode (bits 23:22)
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        e_load(4, 23, 17);      // x23 = MXCSR (full, kept for the sticky-flag projection)
                        e_lsr_i(16, 23, 13, 0); // x16 = MXCSR >> 13
                        e_movconst(19, 3);
                        e_rrr(A_AND, 16, 16, 19, 0, 0); // x16 = RC (0..3): 00 nearest,01 down,10 up,11 zero
                        // ARM RMode swaps the two RC bits: 00 RN,01 RP(up),10 RM(down),11 RZ -> arm = bitrev2(RC)
                        e_movconst(19, 1);
                        e_rrr(A_AND, 20, 16, 19, 0, 0); // x20 = RC&1
                        e_lsr_i(21, 16, 1, 0);          // x21 = RC>>1
                        e_rrr(A_ORR, 20, 21, 20, 0, 1); // x20 = x21 | (RC&1)<<1  = ARM RMode
                        emit32(0xD53B4400u | 19);       // mrs x19, fpcr
                        e_movconst(21, 3u << 22);
                        e_rrr(A_BIC, 19, 19, 21, 1, 0);  // clear RMode
                        e_rrr(A_ORR, 19, 19, 20, 1, 22); // FPCR.RMode = ARM RMode
                        emit32(0xD51B4400u | 19);        // msr fpcr, x19
                        emit_mxcsr_to_fpsr(23);          // MXCSR sticky flags -> host FPSR (so feclearexcept clears)
                    }
                    gpc = next;
                    continue;
                }
                if (sub == 3) { // stmxcsr: report MXCSR default + current rounding mode (from FPCR.RMode)
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        emit32(0xD53B4400u | 19); // mrs x19, fpcr
                        e_lsr_i(19, 19, 22, 0);   // x19 = FPCR >> 22
                        e_movconst(20, 3);
                        e_rrr(A_AND, 19, 19, 20, 0, 0); // x19 = ARM RMode
                        e_movconst(20, 1);
                        e_rrr(A_AND, 21, 19, 20, 0, 0);
                        e_lsr_i(22, 19, 1, 0);
                        e_rrr(A_ORR, 19, 22, 21, 0, 1);  // x19 = x86 RC (swap back)
                        e_movconst(16, 0x1f80);          // default MXCSR (all exceptions masked, RC=00)
                        e_rrr(A_ORR, 16, 16, 19, 0, 13); // MXCSR |= RC << 13
                        emit_fpsr_to_mxcsr(16);          // + live sticky exception flags (IE/DE/ZE/OE/UE/PE)
                        e_store(4, 16, 17);
                    }
                    gpc = next;
                    continue;
                }
                if ((sub == 0 || sub == 1) && I.is_mem) { // fxsave / fxrstor: FCW@0, MXCSR@24, XMM0-15@160
                    emit_ea(&I, next);                    // x17 = base of the 512-byte FXSAVE area
                    for (int i = 0; i < 16; i++) {
                        if (sub == 0)
                            e_str_q(i, 17, 160 + i * 16);
                        else
                            e_ldr_q(i, 17, 160 + i * 16);
                    }
                    if (sub == 0) { // fxsave: also save MXCSR (from FPCR.RMode, mirrors stmxcsr) + FCW
                        emit32(0xD53B4400u | 19);       // mrs x19, fpcr
                        e_lsr_i(19, 19, 22, 0);         // x19 = FPCR >> 22
                        e_movconst(20, 3);
                        e_rrr(A_AND, 19, 19, 20, 0, 0); // x19 = ARM RMode
                        e_movconst(20, 1);
                        e_rrr(A_AND, 21, 19, 20, 0, 0);
                        e_lsr_i(22, 19, 1, 0);
                        e_rrr(A_ORR, 19, 22, 21, 0, 1);                    // x19 = x86 RC (swap the two bits back)
                        e_movconst(16, 0x1f80);                            // default MXCSR (all masks set, RC=00)
                        e_rrr(A_ORR, 16, 16, 19, 0, 13);                   // MXCSR |= RC << 13
                        emit_fpsr_to_mxcsr(16);                            // + live sticky exception flags
                        emit32(0xB9000000u | (6u << 10) | (17 << 5) | 16); // str  w16, [x17,#24]  (MXCSR)
                        e_ldr(16, 28, OFF_FPCW);
                        emit32(0x79000000u | (0u << 10) | (17 << 5) | 16); // strh w16, [x17,#0]   (FCW)
                    } else {                                               // fxrstor: restore MXCSR.RC -> FPCR + FCW
                        emit32(0xB9400000u | (6u << 10) | (17 << 5) | 23); // ldr  w23, [x17,#24]  (MXCSR, full)
                        emit_mxcsr_to_fpsr(23);                            // restore sticky exception flags -> FPSR
                        e_lsr_i(16, 23, 13, 0);                            // x16 = MXCSR >> 13
                        e_movconst(19, 3);
                        e_rrr(A_AND, 16, 16, 19, 0, 0); // x16 = RC (0..3)
                        e_movconst(19, 1);
                        e_rrr(A_AND, 20, 16, 19, 0, 0);
                        e_lsr_i(21, 16, 1, 0);
                        e_rrr(A_ORR, 20, 21, 20, 0, 1);  // x20 = ARM RMode (swap the two RC bits)
                        emit32(0xD53B4400u | 19);        // mrs x19, fpcr
                        e_movconst(21, 3u << 22);
                        e_rrr(A_BIC, 19, 19, 21, 1, 0);  // clear RMode
                        e_rrr(A_ORR, 19, 19, 20, 1, 22); // FPCR.RMode = ARM RMode
                        emit32(0xD51B4400u | 19);        // msr fpcr, x19
                        emit32(0x79400000u | (0u << 10) | (17 << 5) | 16); // ldrh w16, [x17,#0]   (FCW)
                        e_str(16, 28, OFF_FPCW);                           // cpu->fpcw = FCW
                    }
                    // The XMM/MXCSR/FCW area is done inline above; the x87-register DATA (ST0-7 @32, 80-bit)
                    // and FSW (@2) need the modeled x87 stack (c->st[]/fptop/fpsw), so finish in C. Spill the
                    // x87 shadow + any pending flags, stash the area base, and exit to do_fxsave/do_fxrstor.
                    if (g_fl_pending) flags_materialize();
                    if (g_fp_known) fp_drop();
                    e_str(17, 28, OFF_X87EA);
                    emit_exit_const(next, sub == 0 ? R_FXSAVE : R_FXRSTOR);
                    break;
                }
            }
            // bsf/tzcnt (0F BC), bsr/lzcnt (0F BD). The F3 prefix selects the BMI/ABM count form, which is
            // DISTINCT from the legacy bit-scan: tzcnt==bsf result for src!=0 but lzcnt != bsr (lzcnt = leading
            // ZERO COUNT = CLZ; bsr = bit INDEX = (w-1)-CLZ). Mixing them up silently corrupts BMI codegen
            // (e.g. tinystr length math) -- the bug behind uutils' garbled "en-US" locale.
            if (op == 0xBC || op == 0xBD) {
                int mem;
                int rmv = rm_load(&I, next, I.opsize, &mem);
                int cnt = I.rep; // F3 -> tzcnt/lzcnt (counts; src==0 -> opsize, the ARM CLZ result naturally)
                // The destination write below clobbers the source register when dest==src (e.g.
                // `bsf %edx,%edx` -- exactly the form Go's bytealg.IndexByteString emits). The flag
                // computation below reads the source AFTER that write, so without this guard the x86
                // ZF/CF would reflect the RESULT, not the source -> a no-match bsf wrongly clears ZF and
                // the caller mis-reads a hit. Preserve the source in a scratch (x23) so flags stay correct.
                int src = rmv;
                if (!mem && I.reg == rmv) {
                    e_mov_rr(23, rmv, sf);
                    src = 23;
                }
                // Legacy bsf/bsr (no F3) compute the bit INDEX into x22 first: x86 leaves the
                // DESTINATION UNCHANGED when src==0 (real-hw behavior that glibc memrchr relies on -- its
                // not-found tail is `bsr; je; ret <dest>`), so the index is csel'd in only when src!=0.
                // tzcnt/lzcnt (F3) instead DEFINE src==0 -> opsize and write the dest unconditionally.
                int bdst = cnt ? I.reg : 22;
                if (op == 0xBC) { // tzcnt / bsf: trailing zeros = RBIT+CLZ (same value; src==0 -> opsize)
                    e_rbit(bdst, src, sf);
                    e_clz(bdst, bdst, sf);
                } else if (cnt) { // lzcnt: leading zeros = CLZ
                    e_clz(I.reg, src, sf);
                } else { // bsr: (w-1) - clz
                    // clz lands in a scratch that can NEVER alias src: for a memory operand rm_load returns
                    // x16, so using x16 here would clobber the loaded source before the ZF test below reads
                    // it (a top-bit-set operand -> clz==0 -> ZF wrongly set -> csel keeps the old dest). x20
                    // is engine scratch (guest regs are x0..x15), so it is safe.
                    e_clz(20, src, sf);
                    e_movconst(19, sf ? 63 : 31);
                    e_rrr(A_SUB, 22, 19, 20, sf, 0);
                }
                if (cnt) { // tzcnt/lzcnt: x86 CF = (src==0), ZF = (result==0)
                    e_rrr(A_SUBS, 31, src, 31, sf, 0);
                    e_cset(19, 0 /*EQ*/, sf);               // x19 = (src==0) = x86 CF
                    e_rrr(A_ANDS, 31, I.reg, I.reg, sf, 0); // live N/Z from the result
                    e_nzcv_save_setcf(19);                  // store N/Z, stored C = NOT(src==0)
                } else {                                    // bsf/bsr: ZF = (src==0), dest UNCHANGED if src==0
                    e_rrr(A_ANDS, 31, src, src, sf, 0);     // Z = (src == 0)
                    e_csel(I.reg, I.reg, 22, 0 /*EQ*/, sf); // src==0 -> keep dest, else the computed index
                    e_nzcv_save();
                }
                gpc = next;
                continue;
            }
            // popcnt (F3 0F B8): dest = popcount(src). x86 sets ZF=(src==0) and clears CF/OF/SF/AF/PF.
            // (Without F3, 0F B8 is the reserved JMPE -> falls through to report_unimpl.)
            if (op == 0xB8 && I.rep) {
                int mem;
                int rmv = rm_load(&I, next, I.opsize, &mem);
                // dest==src (e.g. `popcnt %rax,%rax`): the result write clobbers the source before the
                // ZF computation reads it, so preserve the source in a scratch first (mirrors bsf above).
                int src = rmv;
                if (!mem && I.reg == rmv) {
                    e_mov_rr(23, rmv, sf);
                    src = 23;
                }
                // NEON popcount: move src into scratch v16 (upper lanes zeroed), per-byte CNT, sum via ADDV.
                if (sf)
                    e_fmov_to_d(16, src); // fmov d16, x[src]  (zeroes bits[64:128])
                else
                    e_fmov_to_s(16, src);             // fmov s16, w[src] (zeroes bits[32:128])
                emit32(0x0E205800u | (16 << 5) | 16); // cnt v16.8b, v16.8b  (per-byte popcount)
                emit32(0x0E31B800u | (16 << 5) | 16); // addv b16, v16.8b    (sum the 8 byte counts -> 0..64)
                e_fmov_from_s(I.reg, 16);           // dest = count; the W-write zero-extends (correct for both widths)
                e_rrr(A_ANDS, 31, src, src, sf, 0); // N/Z from the source: ZF = (src == 0)
                e_nzcv_save_c1();                   // store N/Z, force x86 CF=0/OF=0
                gpc = next;
                continue;
            }
            // bit ops: BT(A3) BTS(AB) BTR(B3) BTC(BB), and group BA /4..7 with imm8.
            if (op == 0xA3 || op == 0xAB || op == 0xB3 || op == 0xBB || op == 0xBA) {
                int isimm = (op == 0xBA);
                int sub = isimm ? (I.reg & 7) : (op == 0xA3 ? 4 : op == 0xAB ? 5 : op == 0xB3 ? 6 : 7);
                if (sub < 4) {
                    report_unimpl(gpc, &I);
                    break;
                }
                int w = I.opsize, mem, bits = w * 8;
                int logbits = w == 8 ? 6 : w == 4 ? 5 : 4; // log2(bits): 64/32/16
                int logw = w == 8 ? 3 : w == 4 ? 2 : 1;    // log2(w)
                int val;
                // x86 bit-string addressing: with a MEMORY base and a REGISTER bit offset, the high bits of
                // the (signed) offset select the addressed word (EA + (offset/bits)*w); only the low
                // log2(bits) bits index within it. (An immediate offset is taken modulo the operand size,
                // for both reg and mem.) Dropping the high bits -- the pre-fix behavior -- mis-tests a
                // 256-bit bitset (e.g. glibc/grep's DFA charclass `bt %reg,(%mem)`), the debian-grep miss.
                if (I.is_mem && !isimm) {
                    emit_ea(&I, next); // x17 = base EA
                    if (w == 8)
                        e_mov_rr(20, I.reg, 1);
                    else
                        e_sxt(20, I.reg, w);           // sxtw/sxth: index as a 64-bit signed value
                    e_asr_i(20, 20, logbits, 1);       // x20 = signed word offset = index >> log2(bits)
                    e_rrr(A_ADD, 17, 17, 20, 1, logw); // EA += wordoff * w
                    e_load(w, 16, 17);
                    val = 16;
                    mem = 1;
                } else {
                    val = rm_load(&I, next, w, &mem);
                }
                if (isimm)
                    e_movconst(19, (uint64_t)(((uint64_t)I.imm) & (bits - 1))); // idx -> x19
                else {
                    e_movconst(21, bits - 1);
                    e_rrr(A_AND, 19, I.reg, 21, sf, 0);
                }
                e_shv(S_LSRV, 21, val, 19, sf);
                e_movconst(22, 1);
                e_rrr(A_AND, 21, 21, 22, sf, 0); // x21 = bit
                e_rrr(A_SUBS, 31, 31, 21, 1, 0);
                e_nzcv_save();  // ARM C = !bit  (subs convention -> x86 CF)
                if (sub != 4) { // BTS/BTR/BTC: modify the bit + write back
                    e_movconst(22, 1);
                    e_shv(S_LSLV, 22, 22, 19, sf); // mask = 1<<idx
                    if (mem && I.lock) {
                        // LOCK BTS/BTR/BTC: the read-modify-write MUST be atomic or contending guest threads
                        // lose updates on a shared bitset word. Use the LSE atomic (LDSET/LDCLR/LDEOR); it
                        // returns the OLD memory value, so recompute CF from THAT -- the tested bit and the RMW
                        // are then a single atomic operation (a pre-load CF could be stale after a peer's flip).
                        uint32_t lse = (sub == 5) ? LSE_LDSET : (sub == 6) ? LSE_LDCLR : LSE_LDEOR;
                        e_lse(lse, w, 22, 23, 17); // x23 = old [m]; [m] {|=,&=~,^=} mask (acquire-release)
                        e_shv(S_LSRV, 24, 23, 19, sf);
                        e_movconst(25, 1);
                        e_rrr(A_AND, 24, 24, 25, sf, 0); // x24 = old tested bit
                        e_rrr(A_SUBS, 31, 31, 24, 1, 0);
                        e_nzcv_save(); // ARM C = !bit -> x86 CF, atomically consistent with the RMW above
                    } else {
                        int o = mem ? 16 : I.rm_reg;
                        if (sub == 5)
                            e_rrr(A_ORR, o, val, 22, sf, 0); // BTS
                        else if (sub == 6)
                            e_rrr(A_BIC, o, val, 22, sf, 0); // BTR
                        else
                            e_rrr(A_EOR, o, val, 22, sf, 0); // BTC
                        rm_store(&I, w, o);
                    }
                }
                gpc = next;
                continue;
            }
            // cmpxchg (0F B0 byte / B1): compare RAX with r/m; if eq, r/m=reg, ZF=1; else RAX=r/m.
            if (op == 0xB0 || op == 0xB1) {
                int w = op == 0xB0 ? 1 : I.opsize, sf2 = (w == 8);
                if (I.is_mem) {
                    emit_ea(&I, next);
                    e_mov_rr(19, RAX, sf2);    // expected
                    e_cas(w, 19, I.reg, 17);   // x19 = old; if old==expected [m]=reg
                    do_alu(7, -1, 19, RAX, w); // ZF = (old == rax)
                    if (w >= 4)
                        e_mov_rr(RAX, 19, sf2);
                    else
                        e_bfi(RAX, 19, 0, 8 * w, 1); // rax = old
                } else if (w >= 4) {
                    e_mov_rr(19, I.rm_reg, sf2);
                    do_alu(7, -1, 19, RAX, w);
                    e_csel(I.rm_reg, I.reg, 19, 0, sf2); // rm = ZF? reg : rm_old
                    e_csel(RAX, RAX, 19, 0, sf2);        // rax = ZF? rax : rm_old
                } else {
                    report_unimpl(gpc, &I);
                    break;
                }
                gpc = next;
                continue;
            }
            // xadd (0F C0 byte / C1): tmp=r/m; r/m += reg; reg = tmp (+ flags)
            if (op == 0xC0 || op == 0xC1) {
                int w = op == 0xC0 ? 1 : I.opsize, sf2 = (w == 8);
                if (I.is_mem) {
                    emit_ea(&I, next);
                    e_lse(LSE_LDADD, w, I.reg, 19, 17); // x19 = old; [m] += reg
                    do_alu(0, -1, 19, I.reg, w);        // flags from old+reg
                    if (w >= 4)
                        e_mov_rr(I.reg, 19, sf2);
                    else
                        e_bfi(I.reg, 19, 0, 8 * w, 1); // reg = old
                } else if (w >= 4) {
                    e_mov_rr(19, I.rm_reg, sf2); // old
                    e_rrr(A_ADDS, I.rm_reg, I.rm_reg, I.reg, sf2, 0);
                    e_nzcv_save_ci();         // rm += reg (x86 add carry)
                    e_mov_rr(I.reg, 19, sf2); // reg = old
                } else {
                    report_unimpl(gpc, &I);
                    break;
                }
                gpc = next;
                continue;
            }
            // jcc rel32 (0F 80-8F)
            if ((op & 0xF0) == 0x80) {
                int lo = op & 0xF, parity = (lo == 0xA || lo == 0xB);
                int cc;
                if (parity) {
                    cc = emit_parity_jcc_cond(lo); // jp/jnp: PF lane -> live ARM Z, branch off it
                } else {
                    cc = x86cc_to_arm(lo);
                    if (cc < 0) {
                        if (g_fl_pending) flags_materialize(); // materialize before boundary
                        report_unimpl(gpc, &I);
                        break;
                    }
                }
                uint64_t taken = next + (uint64_t)I.imm;
                // W5B tier-2: single-block self-loop (taken back-edge == block start). See jcc rel8.
                if (!parity && taken == start && !notier2x() && !loop_has_rmw_hazard((uint64_t)body, (uint64_t)g_cp)) {
                    int slot = g_tier2_build ? 0 : t2_slot(start);
                    if (g_tier2_build || slot >= 0) {
                        emit_selfloop_x86(cc, start, next, body, slot);
                        break;
                    }
                }
                uint64_t fall = next;
                int stitch_fall =
                    (STITCH_OK && fall != start && !seen_has(seen, nseen, fall) && !map_body(fall) && !trap_head(fall));
                int save_taken = 0, save_fall = 0;
                if (parity) {
                    // live ARM Z already holds (PF==0) from emit_parity_jcc_cond; flags spilled there.
                } else {
                    // x86-xflags edge-aware flag handling -- see jcc rel8 (identical semantics).
                    jcc_edge_flags(taken, fall, gpc, stitch_fall, &save_taken, &save_fall);
                }
                // STITCH (see jcc rel8): inline the fall-through, invert the cond, taken exit OOL.
                // Parity jcc: restore the canonical live NZCV on every edge (parity-edge fix).
                if (stitch_fall) {
                    int inv = (cc ^ 1) & 0xF;
                    uint32_t *patch = (uint32_t *)g_cp;
                    emit32(0);                     // b.inv -> fall (inline)
                    if (parity) e_nzcv_load();     // taken edge: restore canonical live NZCV
                    if (save_taken) e_nzcv_save(); // FL_SUB spill on the (flag-live) taken edge only
                    emit_chain_exit(taken);
                    int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
                    *patch = 0x54000000u | (((uint32_t)d & 0x7FFFF) << 5) | (uint32_t)inv;
                    if (parity) e_nzcv_load(); // inline fall: restore before continuing
                    seen[nseen++] = fall;
                    trace_blk++;
                    gpc = fall;
                    continue;
                }
                uint32_t *patch = (uint32_t *)g_cp;
                emit32(0);
                if (parity) e_nzcv_load();    // fall edge: restore canonical live NZCV
                if (save_fall) e_nzcv_save(); // FL_SUB spill for a flag-live fall successor
                emit_chain_exit(next);
                int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
                *patch = 0x54000000u | (((uint32_t)d & 0x7FFFF) << 5) | (cc & 0xF);
                if (parity) e_nzcv_load();     // taken edge: restore canonical live NZCV
                if (save_taken) e_nzcv_save(); // FL_SUB spill for a flag-live taken successor
                emit_chain_exit(taken);
                break;
            }
            // setcc (0F 90-9F) -> r/m8 (byte: preserve upper bits / hi-lo byte regs)
            if ((op & 0xF0) == 0x90) {
                int lo = op & 0xF;
                if (lo == 0xA || lo == 0xB) { // setp/setnp: real PF lane (integer parity or comisd unordered)
                    if (I.is_mem) emit_ea(&I, next);
                    e_pf_compute(19); // x19 = x86 PF (uses x16 as scratch; x17/EA preserved)
                    if (lo == 0xB) {
                        e_movconst(16, 1);
                        e_rrr(A_EOR, 19, 19, 16, 0, 0); // setnp = NOT PF
                    }
                    if (I.is_mem)
                        e_store(1, 19, 17);
                    else
                        byte_wb(&I, I.rm_reg, 19);
                    gpc = next;
                    continue;
                }
                int cc = x86cc_to_arm(op & 0xF);
                if (cc < 0) {
                    report_unimpl(gpc, &I);
                    break;
                }
                if (I.is_mem) {
                    emit_ea(&I, next); // EA -> x17 FIRST (emit_ea may clobber x16)
                    e_nzcv_load();
                    e_cset(16, cc, 0);
                    e_store(1, 16, 17);
                } else {
                    e_nzcv_load();
                    e_cset(16, cc, 0);
                    byte_wb(&I, I.rm_reg, 16);
                }
                gpc = next;
                continue;
            }
            // cmovcc (0F 40-4F), reg or mem source
            if ((op & 0xF0) == 0x40) {
                int lo = op & 0xF;
                if (lo == 0xA || lo == 0xB) { // cmovp / cmovnp: real PF lane
                    e_pf_compute(19);         // x19 = x86 PF (before rm_load, which reuses x16/x17)
                    int mem;
                    int rmv = rm_load(&I, next, I.opsize, &mem);
                    e_rrr(A_SUBS, 31, 19, 31, 0, 0);                    // Z = (PF == 0)
                    e_csel(I.reg, rmv, I.reg, (lo == 0xA) ? 1 : 0, sf); // cmovp: NE(PF==1); cmovnp: EQ(PF==0)
                    // parity-edge fix: the SUBS above clobbered the live ARM NZCV; restore the
                    // canonical flags (membank is current: the top-of-loop materialized any pending
                    // producer before this consumer) so a following block exit spills true flags.
                    e_nzcv_load();
                    gpc = next;
                    continue;
                }
                int cc = x86cc_to_arm(op & 0xF);
                if (cc < 0) {
                    report_unimpl(gpc, &I);
                    break;
                }
                int mem;
                int rmv = rm_load(&I, next, I.opsize, &mem);
                e_nzcv_load();
                e_csel(I.reg, rmv, I.reg, cc, sf);
                gpc = next;
                continue;
            }
            // movzx/movsx (0F B6/B7 zero, BE/BF sign). The dest operand size (I.opsize: 2/4/8) governs how
            // the extended value lands: a 32-bit dest must extend to 32 and ZERO bits 63:32; a 64-bit dest
            // extends to 64; a 16-bit dest (66 prefix) inserts the low 16 and preserves bits 63:16.
            if (op == 0xB6 || op == 0xB7 || op == 0xBE || op == 0xBF) {
                int w = (op & 1) ? 2 : 1; // source width: B6/BE byte, B7/BF word
                int signd = (op >= 0xBE);
                int dw = I.opsize;                // dest width 2/4/8
                int dst = (dw == 2) ? 16 : I.reg; // 16-bit dest extends into scratch, then bfi-merges
                if (I.is_mem) {
                    emit_ea(&I, next);
                    if (signd)
                        e_ldrs_w(w, dst, 17, dw == 8); // sign-extend; W form (dw<=4) clears bits 63:32
                    else
                        e_load(w, dst, 17); // ldrb/ldrh zero-extend into the full register
                } else {
                    int src = (w == 1) ? byte_val(&I, I.rm_reg, 16) : I.rm_reg; // byte source: ah/bh/ch/dh -> bits 8-15
                    if (signd)
                        e_sxt_to(dst, src, w, dw == 8); // sxtb/sxth into W (clears 63:32) or X
                    else
                        e_uxt(dst, src, w); // uxtb/uxth: 32-bit op clears bits 63:32
                }
                if (dw == 2) e_bfi(I.reg, 16, 0, 16, 1); // 16-bit dest: merge low 16, preserve bits 63:16
                gpc = next;
                continue;
            }
        }
        report_unimpl(gpc, &I);
        break;
    }
    // IRQSLIM: the out-of-line poll exit stub the body-entry cbnz targets (irq set -> exit to
    // the dispatcher at the block start, exactly like the legacy inline poll).
    if (g_irq_patch) {
        uint32_t *p = g_irq_patch;
        g_irq_patch = NULL;
        *p = 0xB5000000u | (((uint32_t)(((uint8_t *)g_cp - (uint8_t *)p) / 4) & 0x7FFFF) << 5) | 16; // cbnz x16
        emit_exit_const(start, R_BRANCH);
    }
    // BLKDUMP=<hex guest pc> diagnostic: print the emitted host words for one translated block (any tier).
    {
        static uint64_t s_blkdump = ~0ull;
        if (s_blkdump == ~0ull) {
            const char *e = getenv("BLKDUMP");
            s_blkdump = e ? strtoull(e, NULL, 16) : 0;
        }
        if (s_blkdump && (start == s_blkdump || (s_blkdump < 0x1000000ull && (start & 0xFFFFFFull) == s_blkdump))) {
            fprintf(stderr, "[blkdump] gpc=%llx tier%d:", (unsigned long long)start, g_tier2_build ? 2 : 1);
            for (uint32_t *p = (uint32_t *)host; (uint8_t *)p < g_cp; p++)
                fprintf(stderr, " %08x", *p);
            fprintf(stderr, "\n");
        }
    }
    // W5B tier-2: the promoter (g_tier2_build) recompiles in place and updates the EXISTING map entry
    // itself, so don't insert a duplicate and don't chain pending edges here (the promoter does both
    // AFTER icache-flushing the new code). Expose the body for it.
    g_last_body = body;
    if (!g_tier2_build) {
        map_put(start, host, body);
        if (!g_threaded) patch_links_to(start, body); // chaining mutates live blocks -> off when threaded
    }
    return host;
#undef STITCH_OK
}

// W5B tier-2: promote a hot self-loop (its in-cache counter hit threshold and exited R_TIER2 with
// rip == gpc). Recompile the block with the folded back-edge (+ dead-flag-save elision), then SWAP it in
// under live execution: emit+icache-flush the tier-2 code, redirect the old body, repoint the live map
// entry + still-pending chains, and drop a stale IBTC entry. The old tier-1 code is left as dead bytes.
// Single-threaded only (skipped once a guest thread exists -- promotion mutates the cache outside the
// threaded lock discipline; the loop keeps running tier-1, still correct). Caller is the dispatcher
// between block runs, so guest state is fully spilled. Reuses the shared jit/cache.c substrate
// (g_tier2_build/g_last_body/g_prof_t2/map_idx/patch_links_to/g_ibtc).
static void tier2_promote(uint64_t gpc) {
    if (g_threaded || notier2x()) return;
    int mi = map_idx(gpc);
    if (mi < 0) return;
    pthread_jit_write_protect_np(0);
    g_emit_start = g_cp;
    g_tier2_build = 1;
    void *nh = translate_block(gpc); // folded recompile; no counter, no map_put, no chain
    void *nb = g_last_body;
    g_tier2_build = 0;
    if (getenv("T2DUMP")) {
        fprintf(stderr, "[t2dump] gpc=%llx body+%ld:", (unsigned long long)gpc, (long)((uint8_t *)nb - (uint8_t *)nh));
        for (uint32_t *p = (uint32_t *)nb; (uint8_t *)p < g_cp; p++)
            fprintf(stderr, " %08x", *p);
        fprintf(stderr, "\n");
    }
    // make the tier-2 code coherent BEFORE anything can branch into it
    sys_icache_invalidate(g_emit_start, (size_t)(g_cp - g_emit_start));
    // redirect the OLD tier-1 body to tier-2 (predecessor chains were resolved to the old body when they
    // were translated; patch_links_to only fixes still-PENDING edges) -- overwrite its first insn with
    // `b nb`. Costs one branch per loop ENTRY (negligible vs the loop body).
    void *old_body = g_map[mi].body;
    int64_t bd = ((uint8_t *)nb - (uint8_t *)old_body) / 4;
    *(uint32_t *)old_body = 0x14000000u | ((uint32_t)bd & 0x3FFFFFFu);
    // IRQSLIM: forward chains enter at body+8 (past the 2-insn poll) and would miss the body+0
    // bounce -- give the poll-skipping entry its own bounce to nb+8 (tier-2 has the same layout).
    if (g_fwdskip) {
        int64_t bd8 = (((uint8_t *)nb + 8) - ((uint8_t *)old_body + 8)) / 4;
        ((uint32_t *)old_body)[2] = 0x14000000u | ((uint32_t)bd8 & 0x3FFFFFFu);
    }
    sys_icache_invalidate(old_body, 4 + (g_fwdskip ? 8 : 0));
    // swap the live map entry: future dispatcher lookups + IBTC fills resolve to tier-2 directly
    g_map[mi].host = nh;
    g_map[mi].body = nb;
    patch_links_to(gpc, nb); // repoint any still-unresolved chains to this gpc straight at tier-2
    uint32_t h = (uint32_t)((gpc >> 2) & (IBTC_N - 1)); // drop a stale IBTC entry (refills to tier-2)
    if (g_ibtc[h].target == gpc) {
        g_ibtc[h].target = 0;
        g_ibtc[h].body = NULL;
    }
    pthread_jit_write_protect_np(1);
    g_prof_t2++;
}

static void report_unimpl(uint64_t pc, struct insn *I) {
    const uint8_t *p = (const uint8_t *)pc;
    fprintf(stderr, "[dd] UNIMPL %s opcode 0x%02x at rip=%llx  bytes:", I->two ? "0F" : "1B", I->op,
            (unsigned long long)pc);
    for (int i = 0; i < (I->len ? I->len : 8); i++)
        fprintf(stderr, " %02x", p[i]);
    fprintf(stderr, "\n");
    // emit a clean exit that terminates the guest (so we don't run off into garbage).
    emit_spill();
    e_movconst(16, 0xDEAD0000u | I->op);
    e_str(16, 28, OFF_RIP);
    e_movconst(16, 99);
    e_str(16, 28, OFF_RSN); // reason 99 -> dispatcher aborts
    emit_host_ptr(16, (uint64_t)block_return, PRELOC_BLOCKRET);
    e_br(16);
}

// ---------------- host entry trampolines (adapted from jit.c, x86 reg set) ----------------
__attribute__((naked)) static void run_block(struct cpu *cpu, void *code) {
    __asm__ volatile( // x0=cpu, x1=code
        "str x19,[x0,#176]\n str x20,[x0,#184]\n str x21,[x0,#192]\n str x22,[x0,#200]\n"
        "str x23,[x0,#208]\n str x24,[x0,#216]\n str x25,[x0,#224]\n str x26,[x0,#232]\n"
        "str x27,[x0,#240]\n str x28,[x0,#248]\n str x29,[x0,#256]\n str x30,[x0,#264]\n"
        "str q8,[x0,#272]\n str q9,[x0,#288]\n str q10,[x0,#304]\n str q11,[x0,#320]\n"
        "str q12,[x0,#336]\n str q13,[x0,#352]\n str q14,[x0,#368]\n str q15,[x0,#384]\n"
        "mov x9, sp\n str x9,[x0,#168]\n" // host_sp
        "br x1\n");                       // -> emitted prologue (sets x28=cpu)
}

__attribute__((naked)) static void block_return(void) {
    __asm__ volatile( // x28 == &cpu (pinned through the block)
        "ldr x19,[x28,#176]\n ldr x20,[x28,#184]\n ldr x21,[x28,#192]\n ldr x22,[x28,#200]\n"
        "ldr x23,[x28,#208]\n ldr x24,[x28,#216]\n ldr x25,[x28,#224]\n ldr x26,[x28,#232]\n"
        "ldr x27,[x28,#240]\n ldr x29,[x28,#256]\n ldr x30,[x28,#264]\n"
        "ldr q8,[x28,#272]\n ldr q9,[x28,#288]\n ldr q10,[x28,#304]\n ldr q11,[x28,#320]\n"
        "ldr q12,[x28,#336]\n ldr q13,[x28,#352]\n ldr q14,[x28,#368]\n ldr q15,[x28,#384]\n"
        "ldr x9,[x28,#168]\n mov sp, x9\n" // host sp
        "ldr x28,[x28,#248]\n"             // restore host x28 LAST (was using it as base)
        "ret\n");
}
