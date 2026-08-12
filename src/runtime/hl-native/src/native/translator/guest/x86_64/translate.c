// translator/guest/x86_64 -- the x86-64 -> arm64 translator (flag synthesis, SSE/x87 lowering, the
// big translate_block) + host entry trampolines.

#include "../../../host/cpu.h" // HL_HOST_CPU_*: the host entry trampolines at the end are AArch64-only
#include "lower/primitives.h"
#include "lower/alu.h"
#include "lower/crypto.h"
#include "lower/mov.h"
#include "lower/repstr.h"
#include "lower/shift.h"
#include "lower/trace.h"
#include "lower/x87.h"

// ---------------- the translator ----------------
static void report_unimpl(uint64_t pc, struct insn *I);

// Single-threaded -> multi-threaded transition flush (x86 barrier-elision hook).
//
// While the guest is single-threaded, emit.c elides the x86-TSO DMB on every guest load/store
// (e_dmb_ish/e_dmb_ishld gate on g_threaded). Those barrier-free blocks are correct only as long as no
// second thread can observe guest memory. The clone service (linux_abi/thread.c) calls this via the
// G_THREAD_START_FLUSH hook exactly on the g_threaded 0->1 transition, while STILL single-threaded and
// BEFORE the first peer thread is created, so every barrier-elided block is discarded and re-translated
// WITH barriers under g_threaded == 1 before any peer executes a memory op.
//
// This is byte-for-byte the single-threaded wholesale cache-full flush from core/dispatch.c (reuse the
// 64MB arena in place: reset the bump pointer, drop the block map + both IBTCs + pending chains). It is
// race-free here because a guest `syscall` ALWAYS ends its translation block (emit_exit_const R_SYSCALL):
// at the clone service the parent holds no live PC in the arena, so resetting g_cp cannot pull the rug
// from under executing code, and no peer exists yet. The parent's next dispatcher round-trip misses the
// cleared map and re-translates with barriers; the peer starts on the same flushed cache. Returns 0 only
// on a host W^X reprotect failure (treated as clone failure by the caller). Never call once g_threaded is
// already 1 -- a live peer could be executing in the arena being reset.
static int hl_x86_flush_for_thread_start(void) {
    if (!jit_wprot(0)) return 0;
    g_cp = g_cache;
    map_clear();
    pend_reset();
    memset(g_ibtc, 0, sizeof g_ibtc);
    memset(g_xibtc, 0, sizeof g_xibtc); // opt2 2-way IBTC bodies point into the arena we just dropped
    return jit_wprot(1);
}

// The guest just established a MAP_SHARED mapping, so a peer PROCESS mapping the same object can now observe
// this process's stores -- x86-TSO ordering must hold for it even though g_threaded may still be 0. Force
// barriers from here on. The flag is set BEFORE the flush so any block re-translated after the flush sees it
// (setting it after would leave a window where a block is rebuilt still barrier-free). Idempotent.
static int hl_x86_force_barriers_for_shared(void) {
    if (g_shared_obs) return 1;
    g_shared_obs = 1;
    return hl_x86_flush_for_thread_start();
}

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
// x86-xflags: when `co_live`==0 the caller proved the WHOLE NZCV word imul defines (hl sets N=Z=0,
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
void e_rot_flags_const(int res, int k, int width, int cnt) {
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
void e_rot_flags_cl(int res, int k, int width) {
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
void e_nzcv_set_of(int ofreg) {
    e_ldr(20, 28, OFF_NZCV);
    e_movconst(23, 1u << 28);
    e_rrr(A_BIC, 20, 20, 23, 1, 0);     // clear V
    e_rrr(A_ORR, 20, 20, ofreg, 1, 28); // V = OF
    e_str(20, 28, OFF_NZCV);
    emit32(0xD51B4200u | 20); // msr nzcv, x20 (sync live flags)
}

// ALU operation selector from the primary opcode group (00..3D) or group1 /digit.
// returns: 0 ADD 1 OR 2 ADC 3 SBB 4 AND 5 SUB 6 XOR 7 CMP, or -1.
int alu_kind_primary(uint8_t op) {
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
int byte_val(struct insn *I, int regnum, int scratch) {
    if (is_hi8(I, regnum)) {
        e_lsr_i(scratch, regnum - 4, 8, 1);
        return scratch;
    }
    return regnum;
}

// write the low byte of `val` into an 8-bit register operand (preserving other bits).
void byte_wb(struct insn *I, int regnum, int val) {
    if (is_hi8(I, regnum))
        e_bfi(regnum - 4, val, 8, 8, 1);
    else
        e_bfi(regnum, val, 0, 8, 1);
}

// Link range and storage bias for a displaced ET_EXEC image. Architectural addresses remain in this low
// guest range; the bias is applied only when instruction or data bytes are dereferenced on the host.
static uint64_t g_nonpie_lo, g_nonpie_hi, g_nonpie_bias;

// A biased ET_EXEC executes from the host mapping at link_pc+bias, but the address pushed by x86 CALL is
// guest-visible architectural state.  Keep it in the ELF link-address domain so DWARF FDE ranges, dladdr,
// backtrace, and forced unwinding see the same PCs they would on Linux. RET returns that same guest value;
// instruction fetch alone projects it onto storage.
static uint64_t call_return_pc(uint64_t pc) {
    return pc;
}

// r/m operand: mem -> EA to x17, load value to x16 (returns 16); reg -> value reg.
void emit_ea(struct insn *I, uint64_t next_rip);

/*
 * Architectural continuation of the instruction currently being emitted.
 * Store helpers several layers below the decoder use this only when a
 * completed store must leave the block for queued SMC service.  It is assigned
 * immediately after successful decode, before any lowering helper runs.
 */

// 128-bit guest vector load/store of an r/m128 operand. The `base + index*scale` addressing mode x86
// uses for array traversal (`movaps (%rcx,%rax,1),%xmm0`) is exactly ARM's register-offset form, so
// when ea_reg_fold() proves the EA needs no bias/segment/bus-guard/wrap handling the separate
// `add x17, base, index` disappears and the access addresses [base, index] directly. Otherwise this
// is byte-for-byte the emit_ea + g_ldr_q/g_str_q pair every call site used to write inline; the
// x86-TSO barriers are the ones g_ldr_q/g_str_q emit, unchanged.
static void g_ldr_q_ea(int t, struct insn *I, uint64_t next) {
    if (emit_soft_memory_active()) {
        emit_ea(I, next);
        emit_memory_guard(17, 16, next - (uint64_t)I->len, X86_SOFT_READ);
        g_ldr_q(t, 17, 0);
        return;
    }
    int rn, rm, sh;
    if (ea_reg_fold(I, 16, &rn, &rm, &sh)) {
        e_ldr_q_reg(t, rn, rm, sh);
        e_dmb_ishld();
        return;
    }
    emit_ea(I, next);
    g_ldr_q(t, 17, 0);
}

static void g_ldr_d_ea(int t, struct insn *I, uint64_t next) {
    emit_ea(I, next);
    if (emit_soft_memory_active()) emit_memory_guard(17, 8, next - (uint64_t)I->len, X86_SOFT_READ);
    g_ldr_d(t, 17);
}

// Integer-SIMD r/m operand at the width the prefix selects. MMX is 8 bytes: a 128-bit load here reads 8
// bytes the guest never addressed and #PFs when the operand ends a mapped page. LDR D zero-extends, which
// is the invariant the lane-local arms below rely on.
static void g_ldr_vec_ea(int t, struct insn *I, uint64_t next, int mmx) {
    if (mmx)
        g_ldr_d_ea(t, I, next);
    else
        g_ldr_q_ea(t, I, next);
}

// The opcodes with BOTH a 64-bit MMX (no prefix) and a 128-bit SSE2 (66) form -- interp.c's integer_simd
// set, kept in sync with it. 0x77 (emms) and 0xE6/0xD6 (prefix-only) are deliberately outside.
static int sse_mmx_capable(int op) {
    return (op >= 0x60 && op <= 0x76) || op == 0x7E || op == 0x7F || op == 0xC4 || op == 0xC5 ||
           (op >= 0xD1 && op <= 0xD5) || (op >= 0xD7 && op <= 0xE5) || (op >= 0xE7 && op <= 0xEF) ||
           (op >= 0xF1 && op <= 0xFE);
}

static void g_str_d_ea(int t, struct insn *I, uint64_t next) {
    emit_ea(I, next);
    if (emit_soft_memory_active()) {
        emit_memory_guard(17, 8, next - (uint64_t)I->len, X86_SOFT_WRITE);
        e_dmb_ish();
        e_str_d(t, 17);
        emit_soft_store_commit(8);
        return;
    }
    g_str_d(t, 17);
}

static void g_str_q_ea(int t, struct insn *I, uint64_t next) {
    if (emit_soft_memory_active()) {
        emit_ea(I, next);
        emit_memory_guard(17, 16, next - (uint64_t)I->len, X86_SOFT_WRITE);
        e_dmb_ish();
        e_str_q(t, 17, 0);
        emit_soft_store_commit(16);
        return;
    }
    int rn, rm, sh;
    if (ea_reg_fold(I, 16, &rn, &rm, &sh)) {
        e_dmb_ish();
        e_str_q_reg(t, rn, rm, sh);
        return;
    }
    emit_ea(I, next);
    g_str_q(t, 17, 0);
}

// unimplemented-insn diagnostic (defined below translate_block); fwd-declared so the instruction-class
// helpers in translate/<class>.c (#included above translate_block) can defer a rare unhandled form.
static void report_unimpl(uint64_t pc, struct insn *I);

int rm_load(struct insn *I, uint64_t next, int w, int *mem) {
    if (I->is_mem) {
        emit_ea(I, next);
        emit_bus_guard(17, (uint64_t)w, next - (uint64_t)I->len);
        e_load(w, 16, 17);
        *mem = 1;
        return 16;
    }
    *mem = 0;
    if (w == 1) return byte_val(I, I->rm_reg, 23); // handle ah/ch/dh/bh
    return I->rm_reg;
}

int rm_load_access(struct insn *I, uint64_t next, int w, int *mem, uint32_t required) {
    if (!I->is_mem || !emit_soft_memory_active()) return rm_load(I, next, w, mem);
    emit_ea(I, next);
    emit_memory_guard(17, (uint64_t)w, next - (uint64_t)I->len, required);
    e_load(w, 16, 17);
    *mem = 1;
    return 16;
}

void rm_store(struct insn *I, int w, int val) { // val -> r/m (EA already in x17 if mem)
    if (I->is_mem) {
        if (val == 16) {
            e_mov_rr(19, 16, 1); /* host-call guard clobbers x16 */
            val = 19;
        }
        if (emit_soft_memory_active())
            emit_memory_guard(17, (uint64_t)w, g_emit_gpc, X86_SOFT_WRITE);
        else
            emit_bus_guard(17, (uint64_t)w, g_emit_gpc);
        e_store(w, val, 17);
        if (emit_soft_memory_active()) emit_soft_store_commit((uint64_t)w);
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

void rm_store_after_guard(struct insn *I, int w, int val) {
    if (!I->is_mem || !emit_soft_memory_active()) {
        rm_store(I, w, val);
        return;
    }
    if (val == 16) {
        e_mov_rr(19, 16, 1);
        val = 19;
    }
    e_store(w, val, 17);
    emit_soft_store_commit((uint64_t)w);
}

// RCL/RCR (group2 /2,/3): rotate the r/m operand THROUGH the x86 carry flag by a CONSTANT count -- the
// by-1 (D0/D1) and immediate (C0/C1) forms; the by-CL form is left to defer (report_unimpl). The operand
// and CF together form a (W+1)-bit value rotated by `ec`; only CF and -- for a 1-bit rotate -- OF are
// affected, with SF/ZF/PF preserved. Carry-in is taken from cpu->nzcv (stored ARM C = NOT x86 CF; the
// lazy-flag pre-pass has already materialized any pending producer), the result and the new CF/OF are
// emitted with compile-time-constant shifts, and CF/OF are written back to cpu->nzcv. Scratch x19..x24.
void emit_rcl_rcr(struct insn *I, uint64_t next, int w, int rcr, int cnt_raw) {
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
enum {
    FL_NONE = HL_X86_PENDING_NONE,
    FL_SUB = HL_X86_PENDING_SUB,
    FL_ADD = HL_X86_PENDING_ADD,
    FL_LOGIC = HL_X86_PENDING_LOGIC,
};

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
// Known forward/backward states emit a constant stride; dynamic means "unknown at translate
// time" (block entry, or after popfq) so the lowering loads cpu->df and picks the stride at runtime.
static enum hl_x86_direction g_df; // block-static shadow; the runtime truth is cpu->df

static int lazyflags_on(void) {
    return 1;
}

// Direct-write ALU dst: when an ALU (or group1) instruction's r/m operand is a REGISTER (not memory)
// at width>=4, compute the result straight into the guest reg's host home instead of into scratch x16
// followed by a store-back `mov guest,x16`. do_alu already writes any dst (including a guest x0..x15,
// as the dst==reg forms do) and computes PF/AF from the pristine a,b BEFORE overwriting `out`, so
// out==a is byte-identical to out==x16 + rm_store — one fewer instruction on the dependent chain.
// Gate NOXALUDIRECT=1 for A/B (elide-on default). Independent of the flag levers.
int xaludirect_on(void) {
    return 1;
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
    return 1;
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
    return 1;
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
void e_pf_save(int reg) {
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

// imm12 fast path for the ALU-with-immediate forms (group1 80/81/83 and the acc,imm forms). The
// generic path materializes every immediate into a scratch register (e_movconst) and then runs the
// register-register do_alu -- so `add $0x10,%rax` / `cmp $0x4000,%rax` each cost a `mov` the ARM
// encoding does not need: ADDS/SUBS take a 12-bit unsigned immediate, optionally shifted left by 12.
//
// Applies only when the emitted bytes are provably the SAME arithmetic the generic path would do:
//   * width >= 4 (narrow widths operate shifted into the high bits -- different code entirely),
//   * kind add(0) / sub(5) / cmp(7) (ARM has no flag-setting AND/ORR/EOR *immediate* in this form),
//   * g_pfaf_dead -- otherwise do_alu needs the immediate in a REGISTER for the PF source op and the
//     (a ^ b ^ result) AF chain, so materializing it is not wasted work,
//   * the immediate is a non-negative value that fits imm12 or imm12<<12 exactly. Negative
//     immediates are deliberately NOT rewritten into the opposite operation: x86 CF/OF for
//     `add $-16` are the ADD flags of a 64-bit -16, and the FL_ADD finalizer's C inversion is tied
//     to that, so folding it into a SUBS would need a different finalizer.
// Everything after the arithmetic (the deferred g_fl_pending state, and hence every consumer,
// boundary and finalizer downstream) is identical to the register path. Returns 0 -> caller falls
// back to e_movconst + do_alu.
int do_alu_imm12(int kind, int dst, int a, uint64_t imm, int w) {
    if (w < 4) return 0;
    if (kind != 0 && kind != 5 && kind != 7) return 0;
    if (g_pfaf_dead == 0) return 0;
    if (kind == 0 && !lazyflags_on()) return 0; // NOLAZY add spills inline -- keep the generic path
    int sf = (w == 8);
    uint64_t v = sf ? imm : (uint64_t)(uint32_t)imm; // 32-bit ops only see the low 32 bits
    unsigned im;
    int sh;
    if (v < 0x1000ull) {
        im = (unsigned)v;
        sh = 0;
    } else if (v < 0x1000000ull && (v & 0xFFFull) == 0) {
        im = (unsigned)(v >> 12);
        sh = 1;
    } else
        return 0;
    int out = dst < 0 ? 31 : dst;
    if (kind == 0) {
        e_addi_s_sh(out, a, im, sf, sh);
        g_fl_pending = FL_ADD;
    } else {
        e_subi_s_sh(out, a, im, sf, sh);
        g_fl_pending = FL_SUB;
    }
    return 1;
}

// Width-correct ALU: dst = a <kind> b, set cpu->nzcv.  dst<0 => cmp/test (no write).
// 4/8-byte: direct ARM op. 1/2-byte: operate in the HIGH bits (<<sh) so ARM NZCV matches
// x86 byte/word flags exactly, then merge the low w bytes back (preserving upper bits).
void do_alu(int kind, int dst, int a, int b, int w) {
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
void narrow_adcsbb(int adc, int dst, int a, int b, int w) {
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
int lock_rmw(int k, int w, int rs) {
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

#include "lower/sse4x.h"

// Gather the 16 byte-MSBs of vm into the low 16 bits of GPR `dst` (the proven sse2neon
// _mm_movemask_epi8 cascade). Scratch: host v17 and GPR x16 -- so `dst` must not be 16 and the
// caller must not need v17 preserved. Used by the PCMP*STR* fast path below (movemask of the
// pcmpeqb / cmeq-zero lanes) and mirrors the pmovmskb lowering.
static void emit_pmovmask(int vm, int dst) {
    e_vshr_imm(17, vm, 8, 7, 0);                         // ushr v17.16b, vm.16b, #7
    emit32(0x6F001400u | (25u << 16) | (17 << 5) | 17);  // usra v17.8h, v17.8h, #7
    emit32(0x6F001400u | (50u << 16) | (17 << 5) | 17);  // usra v17.4s, v17.4s, #14
    emit32(0x6F001400u | (100u << 16) | (17 << 5) | 17); // usra v17.2d, v17.2d, #28
    emit32(0x0E003C00u | (1u << 16) | (17 << 5) | 16);   // umov w16, v17.b[0]
    emit32(0x0E003C00u | (17u << 16) | (17 << 5) | dst); // umov wdst, v17.b[8]
    e_rrr(A_ORR, dst, 16, dst, 0, 8);                    // orr wdst, w16, wdst, lsl #8
}

// PCMPISTRI (0F3A 63), implicit-length EQUAL-EACH byte form -- the exact idiom glibc's SSE4.2
// strcmp/strncmp run once per 16 bytes. Emitting it inline (no dispatcher round-trip, no C
// softmulator, no full spill/reload) is ~20x faster than the R_SSE3B exit on the same data.
// Bit-for-bit mirror of avx.c's sse42_ilen/sse42_intres(agg=2)/sse42_index/sse42_flags, with the
// imm8 control (polarity bits [5:4], index-direction bit 6) resolved at TRANSLATE time. `av`/`bv`
// are the host vector regs holding operand1/operand2 (guest xmm == host v). Writes guest RCX (host
// x1), ARM NZCV (+ cpu->nzcv membank), and cpu->pf/af. Scratch: x16,x17,x19..x26,x20; v17..v21.
static void emit_pcmpistri_eqeach_byte(int av, int bv, int imm) {
    int neg = imm & 0x10, masked = imm & 0x20, msb = imm & 0x40;
    // per-byte comparisons into vector scratch v18/v19/v21 (v17 is the movemask scratch)
    emit32(0x6E208C00u | (bv << 16) | (av << 5) | 18); // cmeq v18.16b, av, bv  -> op1[i]==op2[i]
    emit32(0x4E209800u | (av << 5) | 19);              // cmeq v19.16b, av, #0  -> op1[i]==0 (nulls)
    emit32(0x4E209800u | (bv << 5) | 21);              // cmeq v21.16b, bv, #0  -> op2[i]==0 (nulls)
    emit_pmovmask(18, 19);                             // w19 = eqmask
    emit_pmovmask(19, 21);                             // w21 = op1 null mask
    emit_pmovmask(21, 24);                             // w24 = op2 null mask
    // la/lb = index of first null (implicit length), or 16 when none: ctz(mask | 0x10000)
    e_movz(16, 1, 1);               // x16 = 0x10000 (sentinel bit @16)
    e_rrr(A_ORR, 17, 21, 16, 0, 0); // w17 = op1nulls | 0x10000
    e_rbit(17, 17, 0);
    e_clz(22, 17, 0);               // la = ctz(...) -> w22
    e_rrr(A_ORR, 17, 24, 16, 0, 0); // w17 = op2nulls | 0x10000
    e_rbit(17, 17, 0);
    e_clz(23, 17, 0); // lb -> w23
    // valid-lane masks va=(1<<la)-1, vb=(1<<lb)-1  (la,lb in [0,16] -> fits 32-bit, 16 -> 0xFFFF)
    e_movz(16, 1, 0); // w16 = 1
    e_shv(S_LSLV, 25, 16, 22, 0);
    e_subi(25, 25, 1, 0); // va -> w25
    e_shv(S_LSLV, 26, 16, 23, 0);
    e_subi(26, 26, 1, 0); // vb -> w26
    // equal-each IntRes1 = (eqmask & va & vb) | ~(va|vb)   (both-valid: use eq; both-invalid: 1; else 0)
    e_rrr(A_AND, 17, 19, 25, 0, 0); // w17 = eqmask & va
    e_rrr(A_AND, 17, 17, 26, 0, 0); // w17 = eqmask & (va&vb)
    e_rrr(A_ORR, 16, 25, 26, 0, 0); // w16 = va | vb
    e_rrr(A_ORN, 17, 17, 16, 0, 0); // w17 = (eq & both_valid) | ~(va|vb)
    e_uxt(17, 17, 2);               // IntRes1 &= 0xFFFF (uxth: width in BYTES)
    // polarity (imm[5:4]) -> IntRes2 (w17); imm resolved at translate time
    if (neg) {
        if (masked)
            e_rrr(A_EOR, 17, 17, 26, 0, 0); // negate only valid op2 lanes (^ vb)
        else {
            e_movz(16, 0xFFFF, 0);
            e_rrr(A_EOR, 17, 17, 16, 0, 0); // negate all 16 lanes
        }
        e_uxt(17, 17, 2); // uxth: mask IntRes2 to 16 bits
    }
    // index (imm[6]) -> guest RCX (host x1), zero-extended
    if (!msb) {           // least-significant set bit, else n(=16)
        e_movz(16, 1, 1); // x16 = 0x10000
        e_rrr(A_ORR, 16, 17, 16, 0, 0);
        e_rbit(16, 16, 0);
        e_clz(1, 16, 0); // RCX = ctz(IntRes2 | 0x10000) -> 16 when IntRes2==0
    } else {             // most-significant set bit, else n
        e_clz(16, 17, 0);
        e_movz(25, 31, 0);
        e_rrr(A_SUB, 16, 25, 16, 0, 0); // w16 = 31 - clz (msb index; -1 when IntRes2==0)
        e_subi_s(31, 17, 0, 0);         // cmp IntRes2, #0
        e_movz(25, 16, 0);
        e_csel(1, 25, 16, 0, 0); // RCX = (IntRes2==0) ? 16 : msb
    }
    // flags: N=SF=(la<16), Z=ZF=(lb<16), C=(NOT x86CF)=(IntRes2==0), V=OF=IntRes2&1; PF=0, AF=0
    e_movz(20, 0, 0);
    e_subi_s(31, 22, 16, 0);
    e_cset(16, 3, 0);
    e_rrr(A_ORR, 20, 20, 16, 0, 31); // SF<<31 (LO: la<16)
    e_subi_s(31, 23, 16, 0);
    e_cset(16, 3, 0);
    e_rrr(A_ORR, 20, 20, 16, 0, 30); // ZF<<30 (lb<16)
    e_subi_s(31, 17, 0, 0);
    e_cset(16, 0, 0);
    e_rrr(A_ORR, 20, 20, 16, 0, 29); // (NOT CF)<<29 (EQ: IntRes2==0)
    e_movz(25, 1, 0);
    e_rrr(A_AND, 16, 17, 25, 0, 0);
    e_rrr(A_ORR, 20, 20, 16, 0, 28); // OF<<28 (IntRes2 bit0)
    e_str(20, 28, OFF_NZCV);
    emit32(0xD51B4200u | 20); // msr nzcv, x20
    e_movz(16, 1, 0);
    e_str(16, 28, OFF_PF); // cpu->pf = 1  => x86 PF = 0 (matches sse42_flags)
    e_str(31, 28, OFF_AF); // cpu->af = 0
}

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
    // The count clamp needs a flag-setting compare, but the LIVE ARM NZCV may be carrying the
    // guest's deferred x86 flags (lazy-flag producer not yet materialized) -- this instruction is
    // not a guest flag producer and must not disturb them. Save/restore around the compare.
    // Without this, `adcb`; `psraw %xmm,%xmm`; `rclb %cl,%bl` fed the carry consumer a carry bit
    // manufactured by the clamp instead of the one the adcb produced.
    emit32(0xD53B4200u | 22);                                        // mrs x22, nzcv
    e_rrr(A_SUBS, 31, 16, 19, 1, 0);                                 // cmp x16, esize
    e_csel(16, 19, 16, 8 /*HI*/, 1);                                 // x16 = (count u> esize) ? esize : count
    emit32(0xD51B4200u | 22);                                        // msr nzcv, x22
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
static int fpdnan_on(void) {
    return 1;
}

// PRE (emit BEFORE the arithmetic, while vd still holds src1): v20 <- per-lane PRESIGN mask =
// (x86 indefinite sign bit) on lanes whose inputs are ALL non-NaN, else 0. Building the sign here
// (off the arithmetic's result-dependency chain, overlapped with the long FP-op latency) shortens
// POST to a 2-op chain from the result. two_in: 1 for add/sub/mul/div (src1=vd, src2=s); 0 for sqrt.
// SHL.T #(esize-1) turns an all-ones "not-NaN" lane into exactly the sign bit (0x8000...), a 0 lane
// stays 0 -- no constant/DUP needed.
// NaN-INPUT gate for the PACKED SSE3 horizontal / addsub family (0F 7C haddp*, 0F 7D hsubp*,
// 0F D0 addsubp*), the exact analogue of the inline gate the vertical 0F 58/59/5C/5E arithmetic
// carries. Those NEON sequences (UZP1/UZP2 + FADD/FSUB, or FSUB/FADD + BSL) are bit-exact with x86
// for finite inputs and for a lane with a SINGLE NaN input, but when a result lane has TWO NaN
// inputs ARM picks SNaN-first-else-src1 where x86 picks QNaN-first-else-second-operand -- the exact
// mirror. Reproducing x86's per-lane priority inline would cost more than the op itself, so gate:
// if ANY lane of either source is a NaN, exit to the x86-exact C softmulator (R_SSE3B ->
// hl_x86_sse_run, which grew coverage for these three opcodes alongside this gate). Real FP code
// has no NaN inputs, so the inline path below is untouched. Emit while src1 is still live in vd.
// Scratch: v24/v25 and x16 (all dead here -- s is v16 at most, a different register file).
static void emit_nan_input_gate(int vd, int s, int dbl, uint64_t gpc);

static void emit_dnan_pre(int vd, int s, int two_in, int dbl) {
    uint32_t EQ = dbl ? 0x4E60E400u : 0x4E20E400u; // FCMEQ Vd.2d/.4s (all-ones per lane where NOT NaN)
    unsigned immhb = dbl ? (64u + 63u) : (32u + 31u);
    if (two_in) {
        emit32(EQ | (vd << 16) | (vd << 5) | 20); // v20 = (src1 == src1)
        emit32(EQ | (s << 16) | (s << 5) | 21);   // v21 = (src2 == src2)
        e_v3(0x4E201C00u, 20, 20, 21);            // v20 = in_notnan = src1nn & src2nn  (AND.16b)
    } else {
        emit32(EQ | (s << 16) | (s << 5) | 20); // v20 = (src == src)
    }
    emit32(0x4F005400u | (immhb << 16) | (20 << 5) | 20); // v20 = PRESIGN (SHL v20.T, v20, #esize-1)
}

// POST (emit AFTER the arithmetic; vd = result): OR the x86 indefinite sign into lanes that are a
// GENERATED default NaN (result is NaN AND no input was NaN). Payload already matches x86, so only the
// sign bit must be set. Critical path from the result is just 2 ops: FCMEQ then BIC (the ORR is off the
// vd->vd forwarding path only by the OR itself). BIC(PRESIGN, res_notnan) keeps the sign bit only on
// lanes where the result IS NaN (res_notnan==0), i.e. exactly the freshly generated default-NaN lanes.
static void emit_dnan_post(int vd, int dbl, int packed) {
    uint32_t EQ = dbl ? 0x4E60E400u : 0x4E20E400u;
    if (packed) {
        // Packed: keep the branchless per-lane fixup (a generated NaN can appear in any lane, and a
        // scalar FCMP cannot gate all lanes). Unchanged this round.
        emit32(EQ | (vd << 16) | (vd << 5) | 21); // v21 = (res == res)  (all-ones where result NOT NaN)
        e_v3(0x4E601C00u, 20, 20, 21);            // v20 = PRESIGN & ~res_notnan = sign on generated-NaN lanes
        e_v3(0x4EA01C00u, vd, vd, 20);            // vd |= sign mask  (ORR.16b)
        return;
    }
    // Scalar: the branchless ORR sat on the loop-carried vd->vd FP forwarding chain (~+12 cyc/iter). A
    // NaN input already routes to the C softmulator (NaN-INPUT gate above), so on this inline path a NaN
    // result can ONLY be a GENERATED NaN (0*inf, inf-inf, ...) -- extremely rare. Hoist the whole fixup
    // behind a predicted-not-taken branch keyed on the scalar result: FCMP dst,dst sets V iff dst is NaN.
    // The common (non-NaN) path is now just FCMP+b.vc, neither of which writes vd, so the fixup is off the
    // forwarding chain entirely. The taken (rare) path runs the ORIGINAL 3-op sequence verbatim, so it is
    // bit-for-bit identical to the old branchless output (which was a no-op whenever the result was not NaN,
    // and stamped the sign -- masking the always-zero scalar upper lane via ~res_notnan -- when it was).
    // The FCMP below WRITES the ARM NZCV, which may still be carrying the guest's deferred x86
    // flags (a scalar SSE op is not an x86 flag producer, so a later jcc/cmov/adc/rcl must see the
    // flags of whatever integer instruction preceded it). Bracket the whole hoisted fixup with a
    // save/restore so both the taken and the not-taken path leave NZCV exactly as they found it.
    emit32(0xD53B4200u | 22);                                           // mrs x22, nzcv
    emit32((dbl ? 0x1E602000u : 0x1E202000u) | (vd << 16) | (vd << 5)); // FCMP dst,dst  (V=1 iff dst is NaN)
    uint32_t *p_bvc = (uint32_t *)g_cp;
    emit32(0);                                // b.vc Lok  (NOT NaN -> skip fixup; patched below)
    emit32(EQ | (vd << 16) | (vd << 5) | 21); // v21 = (res == res)  (all-ones where result NOT NaN)
    e_v3(0x4E601C00u, 20, 20, 21);            // v20 = PRESIGN & ~res_notnan = sign on generated-NaN lanes
    e_v3(0x4EA01C00u, vd, vd, 20);            // vd |= sign mask  (ORR.16b)
    uint8_t *Lok = (uint8_t *)g_cp;
    *p_bvc = 0x54000000u | ((uint32_t)(((Lok - (uint8_t *)p_bvc) / 4) & 0x7FFFF) << 5) | 7; // b.vc (cond VC=7)
    emit32(0xD51B4200u | 22);                                                               // msr nzcv, x22
}

// See the forward declaration above for the rationale. Packed-only: every 0F 7C/7D/D0 form is packed.
static void emit_nan_input_gate(int vd, int s, int dbl, uint64_t gpc) {
    uint32_t EQ = dbl ? 0x4E60E400u : 0x4E20E400u; // FCMEQ .2d/.4s (all-ones per NON-NaN lane)
    emit32(EQ | (vd << 16) | (vd << 5) | 24);      // v24 = (src1 == src1)
    emit32(EQ | (s << 16) | (s << 5) | 25);        // v25 = (src2 == src2)
    e_v3(0x4E201C00u, 24, 24, 25);                 // v24 = src1nn & src2nn  (AND.16b)
    e_ext(25, 24, 24, 8);                          // fold the two 64-bit halves -> low 64 = all lanes
    e_v3(0x4E201C00u, 24, 24, 25);
    e_fmov_from_d(16, 24);          // x16 = lane mask (all-ones iff no NaN in ANY lane of either source)
    e_rrr(A_ORN, 16, 31, 16, 1, 0); // x16 = ~mask (0 iff clean; nonzero iff a NaN input)
    uint32_t *p_cbz = (uint32_t *)g_cp;
    emit32(0);                     // cbz x16, Lfast (patched below)
    emit_exit_const(gpc, R_SSE3B); // NaN present -> x86-exact C emulation of this instruction
    uint8_t *Lfast = (uint8_t *)g_cp;
    *p_cbz = 0xB4000000u | ((uint32_t)(((Lfast - (uint8_t *)p_cbz) / 4) & 0x7FFFF) << 5) | 16;
}

// ---- AVX2 FMA (vfmadd/vfmsub/vfnmadd/vfnmsub) -> NEON FMLA/FMLS ----
// x86 FMA computes  result = (+/-)(A*B) (+/-) C  with a SINGLE rounding (fused). ARM FMLA/FMLS are
// equally fused, so mapping the four sign variants onto FMLA/FMLS with an exact-negated addend keeps
// bit-exact results for finite inputs:
//   acc = neg ? -C : C   (FNEG is exact); then FMLA (acc += A*B) or FMLS (acc -= A*B).
//     fmadd : neg=0,fmls=0  -> A*B + C     fmsub : neg=1,fmls=0  -> A*B - C
//     fnmadd: neg=0,fmls=1  -> C - A*B     fnmsub: neg=1,fmls=1  -> -C - A*B
// Result is left in `acc`. Generated-NaN sign fixup (0*inf, inf-inf: x86 yields the negative QNaN
// indefinite, ARM the positive default NaN -- same payload, opposite sign) is applied over the THREE
// inputs exactly like the SSE emit_dnan_pre/post, keyed on "result is NaN AND no input was NaN".
// A NaN INPUT never reaches here: the caller's gate exits to R_AVX first, and avx.c's fma_x86_f32/f64
// owns the operand-selection rule, which no FMLA sequence reproduces (ARM is SNaN-first-then-addend,
// x86 is first-NaN-in-a*b+c-order). So the "no input was NaN" arm of the fixup is the only live one.
// rA/rB are the multiplicands, rC the addend (all distinct host vregs); acc/mt1/mt2 are scratch vregs
// distinct from the sources. dbl: 1 -> .2d (pd), 0 -> .4s (ps).
static void emit_fma_group(int rA, int rB, int rC, int acc, int mt1, int mt2, int neg, int fmls, int dbl) {
    int fixnan = fpdnan_on();
    uint32_t EQ = dbl ? 0x4E60E400u : 0x4E20E400u; // FCMEQ Vd.2d/.4s (all-ones per non-NaN lane)
    unsigned immhb = dbl ? (64u + 63u) : (32u + 31u);
    if (fixnan) {                                               // mt1 = PRESIGN on lanes with all inputs non-NaN
        emit32(EQ | (rA << 16) | (rA << 5) | mt1);              // mt1 = (A==A)
        emit32(EQ | (rB << 16) | (rB << 5) | mt2);              // mt2 = (B==B)
        e_v3(0x4E201C00u, mt1, mt1, mt2);                       // mt1 &= mt2  (AND.16b)
        emit32(EQ | (rC << 16) | (rC << 5) | mt2);              // mt2 = (C==C)
        e_v3(0x4E201C00u, mt1, mt1, mt2);                       // mt1 = all-inputs-notnan
        emit32(0x4F005400u | (immhb << 16) | (mt1 << 5) | mt1); // mt1 = SHL #(esize-1) -> sign bit per lane
    }
    if (neg)
        emit32((dbl ? 0x6EE0F800u : 0x6EA0F800u) | (rC << 5) | acc); // FNEG acc.T, C  (exact)
    else
        e_vmov(acc, rC);                                    // acc = C
    uint32_t fm = fmls ? (dbl ? 0x4EE0CC00u : 0x4EA0CC00u)  // FMLS acc -= A*B
                       : (dbl ? 0x4E60CC00u : 0x4E20CC00u); // FMLA acc += A*B
    e_v3(fm, acc, rA, rB);                                  // fused multiply-add/sub
    if (fixnan) {
        emit32(EQ | (acc << 16) | (acc << 5) | mt2); // mt2 = (res==res) (all-ones where result NOT NaN)
        e_v3(0x4E601C00u, mt1, mt1, mt2);            // mt1 = PRESIGN & ~res_notnan (sign on generated-NaN)
        e_v3(0x4EA01C00u, acc, acc, mt1);            // acc |= sign  (ORR.16b)
    }
}

// ---- VEX packed FP add/sub/mul/div (vaddps/pd, vsubps/pd, vmulps/pd, vdivps/pd) -> NEON ----
// The fast-path arithmetic once the caller's NaN-input gate has proven NO input lane is a NaN. Emits the
// native NEON FADD/FMUL/FSUB/FDIV (Vn=src1, Vm=src2) plus the emit_dnan x86-negative-QNaN-indefinite sign
// fixup for GENERATED NaNs (0*inf, inf-inf, 0/0, inf/inf from finite inputs: x86 yields the negative QNaN
// indefinite, ARM the positive default NaN -- same payload, opposite sign). A NaN INPUT never reaches here
// (the gate falls back to do_avx) because x86 and ARM diverge on two-NaN-per-lane operand selection; that
// path is left to the correctness-first do_avx. Scratch: v23 (presign), v24 (tmp).
static void emit_vex_fp(int vd, int src1, int src2, int op, int dbl) {
    uint32_t EQ = dbl ? 0x4E60E400u : 0x4E20E400u; // FCMEQ Vd.2d/.4s (all-ones per non-NaN lane)
    unsigned immhb = dbl ? (64u + 63u) : (32u + 31u);
    uint32_t szb = dbl ? 0x00400000u : 0;
    uint32_t base = op == 0x58   ? 0x4E20D400u  // FADD
                    : op == 0x59 ? 0x6E20DC00u  // FMUL
                    : op == 0x5C ? 0x4EA0D400u  // FSUB
                                 : 0x6E20FC00u; // FDIV (0x5E)
    base |= szb;                                // bit22 = sz: 0 -> .4s (ps), 1 -> .2d (pd)
    // PRE: v23 = presign (x86 indefinite sign bit) on lanes whose BOTH inputs are non-NaN, else 0.
    emit32(EQ | (src1 << 16) | (src1 << 5) | 23);         // v23 = (src1==src1)
    emit32(EQ | (src2 << 16) | (src2 << 5) | 24);         // v24 = (src2==src2)
    e_v3(0x4E201C00u, 23, 23, 24);                        // v23 = src1nn & src2nn  (AND.16b)
    emit32(0x4F005400u | (immhb << 16) | (23 << 5) | 23); // v23 = SHL #(esize-1) -> sign bit per lane
    e_v3(base, vd, src1, src2);                           // vd = src1 OP src2  (Vn=src1, Vm=src2)
    // POST: OR the x86 indefinite sign into lanes that are a GENERATED default NaN (result NaN, inputs not).
    emit32(EQ | (vd << 16) | (vd << 5) | 24); // v24 = (vd==vd)
    e_v3(0x4E601C00u, 23, 23, 24);            // v23 = presign & ~res_notnan (BIC.16b)
    e_v3(0x4EA01C00u, vd, vd, 23);            // vd |= sign  (ORR.16b)
}

// Deliver a guest trap SIGNAL (int3 -> SIGTRAP, UD1/UD2 -> SIGILL) by EXITING the block to the dispatcher with
// R_TRAP, rather than emitting a host BRK/UDF. On Apple Silicon a JIT'd BRK/UDF raises a Mach exception the
// x86 engine does not catch, so the host BSD SIGTRAP/SIGILL never reaches jit86_syncguard and the process
// dies (exit 133/132) instead of running the guest handler. Routing through the dispatcher (raise_guest_trap)
// is the same C-delivery path #DE already uses (raise_guest_de) and is host-trap-independent. lsig/code are
// packed into cpu->divop; emit_exit_const spills guest GPR+xmm and sets cpu->rip = the architectural PC.
static void emit_guest_signal(uint64_t rip, int lsig, int code) {
    if (g_fl_pending) flags_materialize();
    if (hl_x86_x87_known()) hl_x86_x87_drop();
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

// DE(1) cannot be taken at face value. ARM raises IDC ONLY when FPCR.FZ flushed a denormal input, i.e.
// exactly in the mode where x86 must NOT report #D -- FZ carries the guest's DAZ, which zeroes the source
// before the operation. Measured: with DAZ set no SSE op raises #D, against 105 of 192 probe lines that
// were spuriously #D here. It also keeps the word self-consistent: stmxcsr reports DAZ from this same bit.
// Costs, both accepted: a lone FTZ sets FZ too and x86 DOES raise #D there, but a lone FTZ already flushes
// INPUTS on this host so those results are wrong regardless; and a guest that loads DE=1 under FZ cannot
// read it back, the sticky flags living only in FPSR. The converse gap -- #D with DAZ clear, which ARM
// cannot report at all -- is left open on cost; see tests/compat/completeness/x86_64/denorm_flags.c.
static void emit_fpsr_to_mxcsr(int dst) { // OR the host FPSR sticky flags into `dst` at MXCSR bits 0..5
    emit32(0xD53B4420u | 22);             // mrs x22, fpsr
    e_movconst(21, 0);                    // accumulator
    e_movconst(19, 1);
    for (int i = 0; i < 6; i++) {
        e_lsr_i(20, 22, g_mxcsr_fpsr_bit[i], 0);
        e_rrr(A_AND, 20, 20, 19, 0, 0);
        e_rrr(A_ORR, 21, 21, 20, 0, i); // x21 |= bit << i
    }
    emit32(0xD53B4400u | 22);       // mrs x22, fpcr
    e_lsr_i(22, 22, 24, 0);         // x22 = FPCR>>24 (FZ -> bit0)
    e_rrr(A_AND, 22, 22, 19, 0, 0); // x19 is still 1
    e_rrr(A_BIC, 21, 21, 22, 0, 1); // FZ (== reported DAZ) -> drop DE
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

// ldmxcsr (0F AE /2): load MXCSR from memory and thread MXCSR.RC (bits 14:13) -> ARM FPCR.RMode (23:22),
// MXCSR.FTZ(15)|DAZ(6) -> FPCR.FZ(24), and the sticky exception flags -> host FPSR. Shared by the legacy
// and VEX (VEX.LZ.0F.WIG AE /2) encodings (semantically identical). Memory operand only.
static void emit_ldmxcsr(struct insn *I, uint64_t next) {
    if (!I->is_mem) return;
    emit_ea(I, next);
    emit_memory_guard(17, 4, next - (uint64_t)I->len, X86_SOFT_READ);
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
    // MXCSR.FTZ(15)|DAZ(6) -> host FPCR.FZ(24). ARM FPCR.FZ flushes both
    // denormal inputs and outputs, so the common FTZ+DAZ pair maps exactly;
    // a lone FTZ/DAZ over-flushes the other direction (documented approximation)
    // -- strictly better than the prior behavior of never flushing at all.
    e_lsr_i(16, 23, 15, 0);         // x16 = MXCSR>>15 (FTZ -> bit0)
    e_lsr_i(20, 23, 6, 0);          // x20 = MXCSR>>6  (DAZ -> bit0)
    e_rrr(A_ORR, 16, 16, 20, 0, 0); // x16 = FTZ|DAZ (junk in high bits)
    e_movconst(20, 1);
    e_rrr(A_AND, 16, 16, 20, 0, 0); // x16 = (FTZ|DAZ)&1
    e_movconst(20, 1u << 24);
    e_rrr(A_BIC, 19, 19, 20, 1, 0);  // clear FPCR.FZ
    e_rrr(A_ORR, 19, 19, 16, 1, 24); // FPCR.FZ = (FTZ|DAZ)
    emit32(0xD51B4400u | 19);        // msr fpcr, x19
    emit_mxcsr_to_fpsr(23);          // MXCSR sticky flags -> host FPSR (so feclearexcept clears)
}

// stmxcsr (0F AE /3): store MXCSR (default control + live rounding mode from FPCR.RMode + sticky flags +
// FTZ/DAZ from FPCR.FZ) to memory. Shared by the legacy and VEX (VEX.LZ.0F.WIG AE /3) encodings.
static void emit_stmxcsr(struct insn *I, uint64_t next) {
    if (!I->is_mem) return;
    emit_ea(I, next);
    emit_memory_guard(17, 4, next - (uint64_t)I->len, X86_SOFT_WRITE);
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
    // reflect host FPCR.FZ(24) back to MXCSR FTZ(15)+DAZ(6) so a guest that
    // saves/restores the control word preserves flush-to-zero mode.
    emit32(0xD53B4400u | 19); // mrs x19, fpcr
    e_lsr_i(19, 19, 24, 0);   // x19 = FPCR>>24 (FZ -> bit0)
    e_movconst(20, 1);
    e_rrr(A_AND, 19, 19, 20, 0, 0);  // x19 = FZ&1
    e_rrr(A_ORR, 16, 16, 19, 0, 15); // MXCSR |= FZ<<15 (FTZ)
    e_rrr(A_ORR, 16, 16, 19, 0, 6);  // MXCSR |= FZ<<6  (DAZ)
    e_store(4, 16, 17);
    if (emit_soft_memory_active()) emit_soft_store_commit(4);
}

// x87 fist/fistp round ST0 (already in d16) to an integral double using the CURRENT x87 rounding control
// (cpu->fpcw bits[11:10]), so the caller's FCVTZS then converts it exactly. x86 x87 defaults to round-to-
// NEAREST-even (not toward-zero) and honors fldcw's RC, but the old code emitted a bare FCVTZS (truncate) --
// so fistp(2.7) gave 2 instead of 3, and a round-up/down control word had no effect. x87 has its OWN rounding
// domain, SEPARATE from SSE MXCSR (both share ARM FPCR.RMode), so round under a SAVED/RESTORED FPCR: set
// FPCR.RMode from the x87 RC (same two-bit swap as ldmxcsr), FRINTI, then restore FPCR so SSE rounding is
// untouched. Scratch x20/x21/x22/x23; x19 (the store EA at every caller) is left intact.
static void emit_x87_round_st0(void) {
    hl_x86_emit_vector_copy(19, 16); // the unrounded value, for C1
    hl_x86_x87_rc_enter();
    emit32(0x1E67C000u | (16 << 5) | 16); // frinti d16, d16 (round to integral per FPCR.RMode)
    hl_x86_x87_rc_leave();
    hl_x86_x87_rounded_up(16, 19); // C1 = the magnitude grew
}

// A masked #IS on an integer store delivers the INTEGER indefinite of the destination width, and the form
// still pops. x22 carries hl_x86_x87_live()'s verdict; `value` holds the normally-converted result.
static void emit_x87_integer_indefinite(int value, int bytes) {
    e_movconst(17, bytes == 2 ? UINT64_C(0x8000) : bytes == 4 ? UINT64_C(0x80000000) : (UINT64_C(1) << 63));
    e_subi_s(31, 22, 0, 1);
    e_csel(value, value, 17, 0 /*EQ: live*/, 1);
}

// FNSTENV/FLDENV m28, FNSAVE/FRSTOR m108 -> hl_x86_x87_environment(). All four rewrite the tag word, three
// rewrite TOP and two convert eight ext80 registers; doing that inline would be two hundred instructions for
// four instructions that appear once per setjmp-shaped guest. x19 = the translated host EA. Ends the block.
static void emit_x87_environment(int selector, uint64_t next) {
    hl_x86_x87_drop(); // the helper owns cpu->fptop from here
    e_str(19, 28, OFF_X87EA);
    e_movconst(16, (uint64_t)selector);
    e_str(16, 28, OFF_DIVOP);
    emit_exit_const(next, R_X87ENV);
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
    uint64_t page_size = smc_page_size();
    uint64_t plo = lo & ~(page_size - 1), phi = (hi + page_size - 1) & ~(page_size - 1);
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
    map_clear();
    memset(g_ibtc, 0, sizeof g_ibtc);
    memset(g_xibtc, 0, sizeof g_xibtc);
    pend_reset();
}

static void jit86_drop_all_smc_translations(void) {
    if (!g_rwx_guest || g_smc_n == 0) return;
    g_smc_n = 0;
    map_clear();
    memset(g_ibtc, 0, sizeof g_ibtc);
    memset(g_xibtc, 0, sizeof g_xibtc);
    pend_reset();
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

// UD1/UD2: explicitly-undefined opcodes that real software (e.g. Chrome feature probes, ruby's
// unreachable/trap paths, libc CPU-feature probes) uses as deliberate traps. On x86 they raise #UD -> SIGILL; with a
// guest handler that runs, otherwise the process dies with status 128+SIGILL = 132. Route through the dispatcher so the
// guest handler receives it (or the default disposition terminates), instead of aborting translation. This is distinct
// from report_unimpl's "engine aborted" path (status 70), which would mislabel a legitimate guest fault as an
// unimplemented-opcode bug of ours.
static void emit_sigill(uint64_t pc) {
    // Quiet by default: undefined instructions frequently sit on never-taken paths (compiler
    // trap/unreachable slots) that get
    // translated as block fall-through but never run; an unconditional message would falsely imply delivery.
    emit_guest_signal(pc, 4, 2); // #UD -> SIGILL (si_code ILL_ILLOPN), rip = the faulting insn
}

// Restore the user-visible RFLAGS lanes from `src`.  POPFQ and IRETQ use the same architectural
// distribution: arithmetic condition codes live in cpu->nzcv, PF/AF have dedicated lanes, and ID/DF
// survive block boundaries in explicit cpu fields.  Keep this as one emitter so a context return
// cannot silently restore a smaller flag set than POPFQ.
static void emit_restore_rflags(int src) {
    e_movconst(17, 0);
    e_bit_move(17, src, 6, 30, 18);                                // ZF(bit6) -> NZCV.Z(30)
    e_bit_move(17, src, 7, 31, 18);                                // SF(bit7) -> NZCV.N(31)
    e_bit_move(17, src, 11, 28, 18);                               // OF(bit11) -> NZCV.V(28)
    emit32(0x53000000u | (0 << 16) | (0 << 10) | (src << 5) | 18); // ubfx w18,wSrc,#0,#1 (CF)
    e_movconst(19, 1);
    e_rrr(A_EOR, 18, 18, 19, 0, 0);  // stored borrow-C = NOT x86 CF
    e_rrr(A_ORR, 17, 17, 18, 0, 29); // -> NZCV.C(29)
    e_str(17, 28, OFF_NZCV);
    emit32(0xD51B4200u | 17);                                      // msr nzcv, x17
    emit32(0x53000000u | (2 << 16) | (2 << 10) | (src << 5) | 18); // ubfx w18,wSrc,#2,#1 (PF)
    e_movconst(19, 1);
    e_rrr(A_EOR, 18, 18, 19, 0, 0); // PF source byte = NOT PF (consumer computes even parity)
    e_str(18, 28, OFF_PF);
    e_af_save(src);                                                  // cpu->af keeps the source's bit 4
    emit32(0x53000000u | (21 << 16) | (21 << 10) | (src << 5) | 18); // ID
    e_str(18, 28, OFF_ID);
    emit32(0x53000000u | (10 << 16) | (10 << 10) | (src << 5) | 18); // DF
    e_str(18, 28, OFF_DF);
    g_df = HL_X86_DIRECTION_DYNAMIC;
    g_fl_pending = FL_NONE;
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

// ---- AVX/AVX2 VEX.128/.256 inline lowering (perf: avoid the per-insn do_avx round-trip) ----
// Guest ymm N (N<16): low 128 = host v[N] (== xmm; spilled by mark_vdirty at block exit); high 128 =
// cpu->vhi[2N] (memory); bits[511:256] = cpu->vz[4N] (memory). VEX zeroes every bit above the operation
// width (the AVX upper-zeroing rule), which avx_zero_upper reproduces. 3-operand non-destructive form:
// dest = ModRM.reg, src1 = VEX.vvvv, src2 = r/m (reg or mem). VEX.256 does the op on BOTH 128-bit halves
// (low in host regs, high round-tripped through cpu->vhi via scratch). Scratch host V: v16 (mem low),
// v20/v21/v22 (high halves); scratch GPR: x16 (cpu-rel address), x17 (guest EA from emit_ea).
static void avx_cpu_addr16(int off) { // x16 = x28 + off   (off < 4096)
    emit32(0x91000000u | ((unsigned)off << 10) | (28u << 5) | 16u);
}

static void avx_cpu_ldr_q(int t, int off) {
    avx_cpu_addr16(off);
    e_ldr_q(t, 16, 0);
}

static void avx_cpu_str_q(int t, int off) {
    avx_cpu_addr16(off);
    e_str_q(t, 16, 0);
}

static void avx_zero_upper(int d, int l256) { // zero destination bits above the written width
    if (!l256) {                              // VEX.128 wrote 128 -> also clear vhi (bits[255:128])
        e_str(31, 28, OFF_VHI + 16 * d);
        e_str(31, 28, OFF_VHI + 16 * d + 8);
    }
    for (int k = 0; k < 4; k++)
        e_str(31, 28, OFF_VZ + 32 * d + 8 * k); // clear vz (bits[511:256])
}

// Emit ONE 128-bit lane of an AVX2 variable shift (VPSLLV/VPSRLV/VPSRAV). op: 0x47 sllv (logical left),
// 0x45 srlv (logical right), 0x46 sravd (arithmetic right, dword only). val=data, cnt=UNSIGNED per-lane
// count, out=result (may alias val/cnt). es=4 (dword) or 8 (qword). Scratch: x16, v24, v25.
//
// x86 saturates the count PER LANE: for a count >= element-bit-width the logical result is 0 and the
// arithmetic result is the sign bit replicated. NEON USHL/SSHL instead read the low SIGNED byte of each
// count lane, so a raw USHL diverges for counts >= esize (or with high count bytes). Reproduce x86 exactly:
//   - dword (es=4): UMIN.4s the count with esize (32) [arith: 31], so the clamped amount is a small
//     positive value < 128 (valid signed byte); USHL by 32 gives 0, SSHL by -31 gives the sign fill --
//     both the exact x86 saturated result. (UMIN.4s exists for 32-bit lanes.)
//   - qword (es=8): NEON has no UMIN.2d, so USHL with the raw count and then BIC (zero) every lane whose
//     count is unsigned >= 64 (CMHS mask). The mask is built BEFORE OUT is written so OUT may alias cnt.
static void emit_avx_varshift_lane(int out, int val, int cnt, int op, int es) {
    uint32_t sz = (es == 8) ? 3u : 2u;
    uint32_t USHL = 0x6E204400u | (sz << 22);
    uint32_t SSHL = 0x4E204400u | (sz << 22);
    uint32_t NEG = 0x6E20B800u | (sz << 22);
    if (op == 0x46) { // arithmetic right (dword only)
        e_movconst(16, 31);
        emit32(0x4E040C00u | (16 << 5) | 24);                 // dup v24.4s, w16 (=31)
        emit32((0x6EA06C00u) | (24 << 16) | (cnt << 5) | 24); // umin v24.4s, cnt, 31
        emit32(NEG | (24 << 5) | 24);                         // neg v24 -> -min(cnt,31)
        emit32(SSHL | (24 << 16) | (val << 5) | out);         // sshl out, val, v24 (sign fill)
        return;
    }
    if (es == 4) { // logical dword: clamp via UMIN.4s
        e_movconst(16, 32);
        emit32(0x4E040C00u | (16 << 5) | 24);                 // dup v24.4s, w16 (=32)
        emit32((0x6EA06C00u) | (24 << 16) | (cnt << 5) | 24); // umin v24.4s = min(cnt,32)
        if (op == 0x45) emit32(NEG | (24 << 5) | 24);         // right shift -> negate amount
        emit32(USHL | (24 << 16) | (val << 5) | out);         // ushl out, val, v24
        return;
    }
    // logical qword: mask lanes with count >= 64 to 0 (build mask first so out may alias cnt).
    e_movconst(16, 64);
    emit32(0x4E080C00u | (16 << 5) | 24);                 // dup v24.2d, x16 (=64)
    emit32((0x6EE03C00u) | (24 << 16) | (cnt << 5) | 25); // cmhs v25.2d = (cnt u>= 64)
    if (op == 0x45) {                                     // logical right
        emit32(NEG | (cnt << 5) | 24);                    // v24 = -cnt
        emit32(USHL | (24 << 16) | (val << 5) | out);
    } else { // logical left
        emit32(USHL | (cnt << 16) | (val << 5) | out);
    }
    e_v3(0x4E601C00u, out, out, 25); // bic out, out, mask
}

// Emit ONE 128-bit lane of a VCMPPS/VCMPPD packed FP compare (op 0xC2). a=src1, b=src2 (host V regs),
// out=result mask (all-ones/all-zero per lane; may alias a/b). p = predicate (imm8 & 0x1F). dbl selects
// .2d (pd) vs .4s (ps). Scratch: v26/v27. Each predicate reproduces x86's NaN result exactly:
//   FCMEQ/FCMGE/FCMGT return false for any NaN operand; ORD = FCMEQ(a,a)&FCMEQ(b,b); UNORD = NOT ORD.
// Predicates 0x10-0x1F have the same relational result as 0x00-0x0F (they differ only in signaling), so
// only the low nibble selects the operation.
static void emit_vcmp_lane(int out, int a, int b, int p, int dbl) {
    uint32_t FCMEQ = dbl ? 0x4E60E400u : 0x4E20E400u;
    uint32_t FCMGE = dbl ? 0x6E60E400u : 0x6E20E400u;
    uint32_t FCMGT = dbl ? 0x6EE0E400u : 0x6EA0E400u;
    const uint32_t AND = 0x4E201C00u, ORR = 0x4EA01C00u, EOR = 0x6E201C00u;
    uint32_t MVN = 0x6E205800u; // NOT vd.16b, vn.16b
    switch (p & 0x0F) {
    case 0x0: e_v3(FCMEQ, out, a, b); break; // EQ_OQ:  a==b (false on NaN)
    case 0x1: e_v3(FCMGT, out, b, a); break; // LT_OS:  a<b  = b>a
    case 0x2: e_v3(FCMGE, out, b, a); break; // LE_OS:  a<=b = b>=a
    case 0x3:                                // UNORD_Q: either NaN  = NOT(ord)
        e_v3(FCMEQ, 26, a, a);
        e_v3(FCMEQ, 27, b, b);
        e_v3(AND, out, 26, 27);
        emit32(MVN | (out << 5) | out);
        break;
    case 0x4:
        e_v3(FCMEQ, out, a, b);
        emit32(MVN | (out << 5) | out);
        break; // NEQ_UQ: !(a==b) (true on NaN)
    case 0x5:
        e_v3(FCMGT, out, b, a);
        emit32(MVN | (out << 5) | out);
        break; // NLT_US: !(a<b)  (true on NaN)
    case 0x6:
        e_v3(FCMGE, out, b, a);
        emit32(MVN | (out << 5) | out);
        break; // NLE_US: !(a<=b) (true on NaN)
    case 0x7:  // ORD_Q: neither NaN
        e_v3(FCMEQ, 26, a, a);
        e_v3(FCMEQ, 27, b, b);
        e_v3(AND, out, 26, 27);
        break;
    case 0x8: // EQ_UQ: a==b OR unordered
        e_v3(FCMEQ, 26, a, a);
        e_v3(FCMEQ, 27, b, b);
        e_v3(AND, 26, 26, 27);
        emit32(MVN | (26 << 5) | 26); // v26 = unord
        e_v3(FCMEQ, 27, a, b);
        e_v3(ORR, out, 26, 27);
        break;
    case 0x9:
        e_v3(FCMGE, out, a, b);
        emit32(MVN | (out << 5) | out);
        break; // NGE_US: !(a>=b) (true on NaN)
    case 0xA:
        e_v3(FCMGT, out, a, b);
        emit32(MVN | (out << 5) | out);
        break;                             // NGT_US: !(a>b)  (true on NaN)
    case 0xB: e_v3(EOR, out, a, a); break; // FALSE_OQ: all zero
    case 0xC:                              // NEQ_OQ: a!=b AND ordered
        e_v3(FCMEQ, 26, a, a);
        e_v3(FCMEQ, 27, b, b);
        e_v3(AND, 26, 26, 27); // ord
        e_v3(FCMEQ, 27, a, b);
        emit32(MVN | (27 << 5) | 27); // !eq
        e_v3(AND, out, 26, 27);
        break;
    case 0xD: e_v3(FCMGE, out, a, b); break; // GE_OS
    case 0xE: e_v3(FCMGT, out, a, b); break; // GT_OS
    case 0xF:
        e_v3(EOR, out, a, a);
        emit32(MVN | (out << 5) | out);
        break; // TRUE_UQ: all ones
    default: __builtin_unreachable(); // p is masked to four bits above
    }
}

// ---- packed float32 (.4s) -> int32 (.4s) with x86 out-of-range/NaN "integer indefinite" (0x80000000).
// ARM FCVTZS saturates (NaN->0, +ovf->INT_MAX, -ovf->INT_MIN); x86 yields 0x80000000 for NaN and ANY
// overflow. -ovf already lands on INT_MIN==0x80000000 (matches), so only NaN and +ovf (f>=2^31) need
// fixing. Compute the NEON result, then blend 0x80000000 into every lane where (f>=2^31 OR f is NaN).
// `trunc`=1 truncates (FCVTZS direct); trunc=0 rounds under the current FPCR.RMode (== guest MXCSR.RC,
// threaded by ldmxcsr) via FRINTX, then FCVTZS the now-integral value.
//   c2p31 = 2^31 as f32 (0x4F000000) broadcast; cindef = 0x80000000 broadcast; t1,t2 scratch.
// FRINTX, not FRINTI: x86 raises #P for an inexact conversion and only the X form reports Inexact. The
// FRINTX trap that bites the f64 path (it also reports #P for an out-of-range inexact source, where x86
// raises #I alone) CANNOT arise at this width -- every f32 at or above 2^31 is already an integer, and no
// f32 below 2^31 can round up to it -- so here the X form is exactly x86 and needs no suppression.
static void emit_ps2dq_128(int out, int sf, int trunc, int c2p31, int cindef, int t1, int t2) {
    if (trunc) {
        emit32(0x4EA1B800u | (sf << 5) | out); // FCVTZS.4s out, sf   (round toward zero)
    } else {
        emit32(0x6E219800u | (sf << 5) | out);  // FRINTX.4s out, sf  (round to integral, current mode)
        emit32(0x4EA1B800u | (out << 5) | out); // FCVTZS.4s out, out (integral value -> exact)
    }
    emit32(0x6E20E400u | (c2p31 << 16) | (sf << 5) | t1); // FCMGE.4s t1, sf, 2^31   (all-ones where f>=2^31)
    emit32(0x4E20E400u | (sf << 16) | (sf << 5) | t2);    // FCMEQ.4s t2, sf, sf      (all-ones where NOT NaN)
    emit32(0x6E205800u | (t2 << 5) | t2);                 // MVN t2                   (all-ones where NaN)
    e_v3(0x4EA01C00u, t1, t1, t2);                        // ORR t1 = fixup mask (>=2^31 OR NaN)
    e_v3(0x6E601C00u, t1, cindef, out);                   // BSL t1 = mask ? 0x80000000 : out
    e_vmov(out, t1);
}

// ---- packed float64 (.2d) -> int32, one 128-bit source (2 doubles). Produces r = int64 lanes and m =
// per-64-bit fixup mask (all-ones where the x86 result must be 0x80000000). `trunc`=1 truncates, else
// rounds under current FPCR.RMode. c2p31d/cneg2p31d = +/-2^31 as f64 broadcast; t1,t2 scratch.
//
// f64 -> int32 is the ONE width pair with out-of-range NON-integers (every f32 above 2^31, and every f64
// above 2^63, is already integral), so it is the only place where x86's three flag rules can be told
// apart, and all three were wrong here. Measured on Zen 4 across the four RC modes:
//   * #I when the ROUNDED value leaves int32 -- so the mask must come from the rounded value, not the
//     source: 2147483647.5 rounds to 2^31 under RC=near/up and is then out of range. A .2d convert
//     targets int64 and cannot see that, so two scalar FCVTZS Wd,Dn over the (already integral) lanes
//     raise it instead -- the same idiom CVT[T]PD2PI uses.
//   * #P when the value stays in range and the rounding changed it -- so FRINTI, which reports nothing,
//     under-reports it.
//   * #P SUPPRESSED when the result is the indefinite, even from an inexact source -- so a bare FRINTX
//     over-reports it, and the truncating FCVTZS.2d does too (it is an in-int64-range inexact convert).
// Hence: round exception-free (FRINTZ/FRINTI), build the mask from the rounded value, take #I from the
// scalar pair, and take #P from an FRINTX over the source with the out-of-range lanes replaced by +0.0
// (exact, and it reports nothing). The result path itself is then flag-free.
static void emit_pd2i32_pieces(int r, int m, int sd, int trunc, int c2p31d, int cneg2p31d, int t1, int t2) {
    emit32((trunc ? 0x4EE19800u : 0x6EE19800u) | (sd << 5) | t2); // FRINTZ/FRINTI.2d t2, sd (no exception)
    emit32(0x6E60E400u | (c2p31d << 16) | (t2 << 5) | m);         // FCMGE.2d m, t2, 2^31    (rounded >= 2^31)
    emit32(0x6EE0E400u | (t2 << 16) | (cneg2p31d << 5) | t1);     // FCMGT.2d t1, -2^31, t2  (-2^31 > rounded)
    e_v3(0x4EA01C00u, m, m, t1);                                  // ORR m |= (rounded < -2^31)
    emit32(0x4E60E400u | (t2 << 16) | (t2 << 5) | t1);            // FCMEQ.2d t1, t2, t2     (NOT NaN)
    emit32(0x6E205800u | (t1 << 5) | t1);                         // MVN t1                  (NaN)
    e_v3(0x4EA01C00u, m, m, t1);                                  // ORR m |= NaN
    emit32(0x1E780000u | (t2 << 5) | 16);                         // FCVTZS w16, d(t2)  lane 0 -> #I only
    emit32(0x5E180400u | (t2 << 5) | t1);                         // DUP    d(t1), t2.d[1]
    emit32(0x1E780000u | (t1 << 5) | 16);                         // FCVTZS w16, d(t1)  lane 1 -> #I only
    e_v3(0x4E601C00u, t1, sd, m);                                 // BIC t1 = sd & ~m   (out-of-range -> +0.0)
    emit32(0x6E619800u | (t1 << 5) | t1);                         // FRINTX.2d t1, t1   -> #P only
    emit32(0x4EE1B800u | (t2 << 5) | r);                          // FCVTZS.2d r, t2    (integral -> exact)
}

// Returns 1 if the VEX insn was lowered inline (caller does gpc = next; continue), else 0 (fall through
// to the R_AVX do_avx exit). Correctness-first: only a vetted, bit-exact-vs-qemu subset is claimed here.
enum { AVX_LOWER_DECLINED = 0, AVX_LOWER_UNMATCHED = 2 };

static int avx_lower_control_and_moves(struct insn *I, uint64_t next) {
    int l256 = (I->vex_l == 1);
    int d = I->reg, s1 = I->vvvv, s2r = I->rm_reg, pp = I->vex_pp, map = I->vex_map, op = I->op;
    // ---- VEX vldmxcsr (VEX.LZ.0F.WIG AE /2) / vstmxcsr (/3): semantically identical to the legacy
    // ldmxcsr/stmxcsr. Route to the same emit so a guest using the VEX encoding does not fall through to
    // the do_avx unimplemented path (which aborts the engine with exit 70). Memory operand, no vvvv. ----
    if (map == 1 && op == 0xAE && pp == 0 && !l256 && I->is_mem) {
        int sub = I->reg & 7;
        if (sub == 2) {
            emit_ldmxcsr(I, next);
            return 1;
        }
        if (sub == 3) {
            emit_stmxcsr(I, next);
            return 1;
        }
    }

    // ---- vperm2i128 (46) / vperm2f128 (06) (VEX.256.66.0F3A.W0 /r ib): select each output 128-bit lane
    // from {src1.lo, src1.hi, src2.lo, src2.hi} per imm nibble. Low half uses imm[1:0] (imm[3]=1 -> zero),
    // high half uses imm[5:4] (imm[7]=1 -> zero). 256-bit only. Resolve imm8 at translate time -> two
    // 128-bit selections. Materialize all 4 candidate halves into scratch first so dest may alias a source.
    if (map == 3 && (op == 0x46 || op == 0x06) && pp == 1 && l256) {
        int imm = I->imm & 0xFF;
        mark_vdirty();
        e_vmov(20, s1);                       // v20 = src1.lo (host xmm)
        avx_cpu_ldr_q(21, OFF_VHI + 16 * s1); // v21 = src1.hi
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(22, 17, 0);  // v22 = src2.lo (mem)
            g_ldr_q(23, 17, 16); // v23 = src2.hi (mem+16)
        } else {
            e_vmov(22, s2r);                       // v22 = src2.lo (host xmm)
            avx_cpu_ldr_q(23, OFF_VHI + 16 * s2r); // v23 = src2.hi
        }
        static const int srcreg[4] = {20, 21, 22, 23};
        // low output -> host v[d]
        if (imm & 0x08)
            e_v3(0x6E201C00u, d, d, d); // EOR d,d,d = zero
        else
            e_vmov(d, srcreg[imm & 3]);
        // high output -> cpu->vhi[d]
        if (imm & 0x80) {
            e_v3(0x6E201C00u, 24, 24, 24);
            avx_cpu_str_q(24, OFF_VHI + 16 * d);
        } else
            avx_cpu_str_q(srcreg[(imm >> 4) & 3], OFF_VHI + 16 * d);
        avx_zero_upper(d, l256);
        return 1;
    }

    // ---- vpmovmskb (VEX.128/.256.66.0F.D7 /r): GPR(reg) <- byte-MSB mask of ymm/xmm(r/m). Source is a
    // register only (no memory form). Reuse the legacy pmovmskb NEON cascade (translate.c:4277); for VEX.256
    // run it twice, folding the high 16 bytes (cpu->vhi[s2r]) into result bits[31:16]. Result is 16 bits
    // (L=0) or 32 bits (L=1) in the dest GPR, upper bits zeroed by the W-form (32-bit) ORR/UMOV. ----
    if (map == 1 && op == 0xD7 && pp == 1 && !I->is_mem) {
        e_vshr_imm(17, s2r, 8, 7, 0);                        // ushr v17.16b, src.16b, #7
        emit32(0x6F001400u | (25u << 16) | (17 << 5) | 17);  // usra v17.8h, v17.8h, #7
        emit32(0x6F001400u | (50u << 16) | (17 << 5) | 17);  // usra v17.4s, v17.4s, #14
        emit32(0x6F001400u | (100u << 16) | (17 << 5) | 17); // usra v17.2d, v17.2d, #28
        emit32(0x0E003C00u | (1u << 16) | (17 << 5) | 16);   // umov w16, v17.b[0]  (bytes 0..7)
        emit32(0x0E003C00u | (17u << 16) | (17 << 5) | d);   // umov wD,  v17.b[8]  (bytes 8..15)
        e_rrr(A_ORR, d, 16, d, 0, 8);                        // wD = w16 | (wD<<8)  -> bits[15:0]
        if (l256) {
            avx_cpu_ldr_q(20, OFF_VHI + 16 * s2r); // v20 = src.hi (bytes 16..31)
            e_vshr_imm(18, 20, 8, 7, 0);           // ushr v18.16b, v20.16b, #7
            emit32(0x6F001400u | (25u << 16) | (18 << 5) | 18);
            emit32(0x6F001400u | (50u << 16) | (18 << 5) | 18);
            emit32(0x6F001400u | (100u << 16) | (18 << 5) | 18);
            emit32(0x0E003C00u | (1u << 16) | (18 << 5) | 16);  // umov w16, v18.b[0] (bytes 16..23)
            e_rrr(A_ORR, d, d, 16, 0, 16);                      // wD |= w16<<16
            emit32(0x0E003C00u | (17u << 16) | (18 << 5) | 16); // umov w16, v18.b[8] (bytes 24..31)
            e_rrr(A_ORR, d, d, 16, 0, 24);                      // wD |= w16<<24
        }
        return 1;
    }

    // ---- vmovd/vmovq (scalar, 128-bit only; VEX.256 form is #UD -> leave to do_avx). W0=32-bit, W1=64-bit.
    // Mirror the legacy movd/movq lowering; the VEX form additionally zeroes ymm bits above the written 128
    // (avx_zero_upper). 66.0F.6E = load GPR/mem -> xmm; F3.0F.7E = movq xmm/m64 -> xmm; 66.0F.7E = store
    // xmm -> GPR/mem. ----
    if (map == 1 && !l256 && (op == 0x6E || op == 0x7E)) {
        int w = I->vex_w;
        if (op == 0x6E && pp == 1) { // 66.0F.6E: (v)movd/q GPR|mem -> xmm, zero-extend to ymm width
            mark_vdirty();
            if (I->is_mem) {
                emit_ea(I, next);
                if (w)
                    g_ldr_d(d, 17);
                else
                    g_ldr_s(d, 17);
            } else if (w)
                e_fmov_to_d(d, s2r);
            else
                e_fmov_to_s(d, s2r);
            avx_zero_upper(d, 0);
            return 1;
        }
        if (op == 0x7E && pp == 2) { // F3.0F.7E: vmovq xmm/m64 -> xmm (low 64, zero upper)
            mark_vdirty();
            if (I->is_mem) {
                emit_ea(I, next);
                g_ldr_d(d, 17);
            } else
                e_vmov8(d, s2r);
            avx_zero_upper(d, 0);
            return 1;
        }
        if (op == 0x7E && pp == 1) { // 66.0F.7E: (v)movd/q xmm -> GPR|mem
            if (I->is_mem) {
                emit_ea(I, next);
                if (w)
                    g_str_d(d, 17);
                else
                    g_str_s(d, 17);
            } else if (w)
                e_fmov_from_d(s2r, d);
            else
                e_fmov_from_s(s2r, d);
            return 1;
        }
    }

    // ---- vzeroupper (VEX.128.0F.WIG 77) / vzeroall (VEX.256.0F.WIG 77): no operands. vzeroupper clears
    // bits[MAX:128] of all ymm0..15 (== avx_zero_upper for each: clears vhi + vz); vzeroall additionally
    // clears the low 128 (host v[n]). ----
    if (map == 1 && op == 0x77 && pp == 0) {
        if (l256) { // vzeroall: also zero the low 128 lanes
            mark_vdirty();
            for (int n = 0; n < 16; n++)
                e_v3(0x6E201C00u, n, n, n); // eor vn.16b -> 0
        }
        for (int n = 0; n < 16; n++)
            avx_zero_upper(n, 0); // clear vhi[n] and vz[n]
        return 1;
    }

    // ---- moves (2-operand: no vvvv) ----
    int is_load = 0, is_store = 0;
    if (map == 1) {
        if ((op == 0x6F && (pp == 1 || pp == 2)) || ((op == 0x10 || op == 0x28) && pp < 2))
            is_load = 1;
        else if ((op == 0x7F && (pp == 1 || pp == 2)) || ((op == 0x11 || op == 0x29) && pp < 2))
            is_store = 1;
        else if (op == 0xE7 && pp == 1 && I->is_mem)
            is_store = 1; // vmovntdq store xmm/ymm -> mem (plain STR)
    }
    if (is_load) {
        mark_vdirty();
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(d, 17, 0);
            if (l256) {
                g_ldr_q(20, 17, 16);
                avx_cpu_str_q(20, OFF_VHI + 16 * d);
            }
        } else {
            if (d != s2r) e_vmov(d, s2r);
            if (l256) {
                avx_cpu_ldr_q(20, OFF_VHI + 16 * s2r);
                avx_cpu_str_q(20, OFF_VHI + 16 * d);
            }
        }
        avx_zero_upper(d, l256);
        return 1;
    }
    if (is_store) {
        mark_vdirty();
        if (I->is_mem) {
            emit_ea(I, next);
            if (emit_soft_memory_active()) {
                emit_memory_guard(17, l256 ? 32u : 16u, next - (uint64_t)I->len, X86_SOFT_WRITE);
                e_dmb_ish();
                e_str_q(d, 17, 0);
            } else
                g_str_q(d, 17, 0);
            if (l256) {
                avx_cpu_ldr_q(20, OFF_VHI + 16 * d);
                if (emit_soft_memory_active())
                    e_str_q(20, 17, 16);
                else
                    g_str_q(20, 17, 16);
            }
            if (emit_soft_memory_active()) emit_soft_store_commit(l256 ? 32u : 16u);
        } else {
            int dst = s2r; // r/m register is the destination
            if (dst != d) e_vmov(dst, d);
            if (l256) {
                avx_cpu_ldr_q(20, OFF_VHI + 16 * d);
                avx_cpu_str_q(20, OFF_VHI + 16 * dst);
            }
            avx_zero_upper(dst, l256);
        }
        return 1;
    }

    return 2;
}

static int avx_lower_fused_arithmetic(struct insn *I, uint64_t next) {
    int l256 = (I->vex_l == 1);
    int d = I->reg, s1 = I->vvvv, s2r = I->rm_reg, pp = I->vex_pp, map = I->vex_map, op = I->op;
    // ---- AVX2 FMA: 0F38 (map 2), 66 (pp 1), packed ps(W0)/pd(W1). Native FMLA/FMLS, fused = bit-exact.
    // Only the plain packed even opcodes (fmadd/fmsub/fnmadd/fnmsub x 132/213/231); the fmaddsub/fmsubadd
    // (0x96/97,A6/A7,B6/B7) and scalar ss/sd (odd opcodes) forms fall through to do_avx.
    if (map == 2 && pp == 1) {
        int role = 0, ok = 1;
        switch (op) {
        case 0x98:
        case 0x9A:
        case 0x9C:
        case 0x9E: role = 132; break;
        case 0xA8:
        case 0xAA:
        case 0xAC:
        case 0xAE: role = 213; break;
        case 0xB8:
        case 0xBA:
        case 0xBC:
        case 0xBE: role = 231; break;
        default: ok = 0; break;
        }
        if (ok) {
            int dbl = I->vex_w; // W1 -> pd (.2d), W0 -> ps (.4s)
            int nib = op & 0x0F;
            int fmls = (nib == 0x0C || nib == 0x0E); // fnmadd/fnmsub: negate the product (FMLS)
            int neg = (nib == 0x0A || nib == 0x0E);  // fmsub/fnmsub: subtract C (FNEG the addend)
            mark_vdirty();
            int s2 = s2r;
            if (I->is_mem) {
                emit_ea(I, next);
                g_ldr_q(16, 17, 0);
                s2 = 16;
            } // op3 low -> v16
            // High halves of the three inputs (256-bit) live in cpu->vhi (or mem+16). Load them once,
            // BEFORE the NaN gate, so both the gate predicate and the fast arithmetic reuse them.
            if (l256) {
                avx_cpu_ldr_q(18, OFF_VHI + 16 * d);  // d.hi
                avx_cpu_ldr_q(19, OFF_VHI + 16 * s1); // s1.hi
                if (I->is_mem)
                    g_ldr_q(20, 17, 16);
                else
                    avx_cpu_ldr_q(20, OFF_VHI + 16 * s2r); // s2.hi
            }
            // ---- NaN-input gate ----
            // Native FMLA/FMLS is bit-exact to x86 FMA for finite inputs and for GENERATED NaNs (fixed
            // up below), but a PROPAGATED NaN diverges: with a single NaN input only the sign differs
            // (x86 keeps the input NaN's sign; ARM's product/addend negation flips it), and with two or
            // three NaN inputs the SELECTED NaN payload differs (x86 and ARM use different NaN priority).
            // Reproducing x86's 3-operand NaN priority + quieting inline is not worth it, so when ANY
            // input lane is a NaN we bail to the correctness-first do_avx path. NaN is absent from real
            // float kernels, so the fast path carries the hot traffic. Predicate: v24 = AND over all
            // inputs of FCMEQ(x,x) (all-ones per non-NaN lane); any zero bit => some NaN => exit.
            uint32_t EQ = dbl ? 0x4E60E400u : 0x4E20E400u;
            emit32(EQ | (d << 16) | (d << 5) | 24); // v24 = (d==d)
            emit32(EQ | (s1 << 16) | (s1 << 5) | 25);
            e_v3(0x4E201C00u, 24, 24, 25); // &= (s1==s1)
            emit32(EQ | (s2 << 16) | (s2 << 5) | 25);
            e_v3(0x4E201C00u, 24, 24, 25); // &= (op3==op3)
            if (l256) {
                emit32(EQ | (18 << 16) | (18 << 5) | 25);
                e_v3(0x4E201C00u, 24, 24, 25); // &= d.hi
                emit32(EQ | (19 << 16) | (19 << 5) | 25);
                e_v3(0x4E201C00u, 24, 24, 25); // &= s1.hi
                emit32(EQ | (20 << 16) | (20 << 5) | 25);
                e_v3(0x4E201C00u, 24, 24, 25); // &= op3.hi
            }
            e_ext(25, 24, 24, 8);           // v25.d[0] = v24.d[1] (fold the two 64-bit halves)
            e_v3(0x4E201C00u, 24, 24, 25);  // v24.d[0] = lane0 & lane1
            e_fmov_from_d(16, 24);          // x16 = combined mask (all-ones iff NO input NaN)
            e_rrr(A_ORN, 16, 31, 16, 1, 0); // x16 = ~x16 (0 iff clean; nonzero iff a NaN input)
            uint32_t *p_cbz = (uint32_t *)g_cp;
            emit32(0);                                       // cbz x16, Lfast  (patched below)
            emit_exit_const(next - (uint64_t)I->len, R_AVX); // NaN present -> emulate this insn in C (this insn's rip)
            uint8_t *Lfast = (uint8_t *)g_cp;
            *p_cbz = 0xB4000000u | ((uint32_t)(((Lfast - (uint8_t *)p_cbz) / 4) & 0x7FFFF) << 5) | 16;

            // ---- fast path: no input NaN ----
            // operand roles: dest(d)=op1, vvvv(s1)=op2, r/m(op3=s2)=op3.
            //   132: d = d*op3 + s1   -> mul={d,op3},  C=s1
            //   213: d = s1*d + op3   -> mul={s1,d},   C=op3
            //   231: d = s1*op3 + d   -> mul={s1,op3}, C=d
            int rA, rB, rC;
            if (role == 132) {
                rA = d;
                rB = s2;
                rC = s1;
            } else if (role == 213) {
                rA = s1;
                rB = d;
                rC = s2;
            } else {
                rA = s1;
                rB = s2;
                rC = d;
            }
            emit_fma_group(rA, rB, rC, 23, 24, 25, neg, fmls, dbl); // low 128 -> v23
            if (l256) {                                             // high 128 (highs already in v18/19/20)
                int hA, hB, hC;
                if (role == 132) {
                    hA = 18;
                    hB = 20;
                    hC = 19;
                } else if (role == 213) {
                    hA = 19;
                    hB = 18;
                    hC = 20;
                } else {
                    hA = 19;
                    hB = 20;
                    hC = 18;
                }
                emit_fma_group(hA, hB, hC, 21, 22, 25, neg, fmls, dbl); // high 128 -> v21
                e_vmov(d, 23);
                avx_cpu_str_q(21, OFF_VHI + 16 * d);
            } else {
                e_vmov(d, 23);
            }
            avx_zero_upper(d, l256);
            return 1;
        }
    }

    // ---- VEX packed FP add/sub/mul/div: map 1, ps(pp==0)/pd(pp==1). Native NEON FADD/FMUL/FSUB/FDIV +
    // generated-NaN sign fixup (emit_vex_fp), behind a NaN-INPUT GATE. Scalar ss(pp==2)/sd(pp==3) -> do_avx.
    if (map == 1 && (op == 0x58 || op == 0x59 || op == 0x5C || op == 0x5E) && pp < 2) {
        int dbl = (pp == 1); // 66 -> pd (.2d), none -> ps (.4s)
        mark_vdirty();
        int s2 = s2r;
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(16, 17, 0);
            s2 = 16;
        } // op2 low -> v16
        // High halves (256-bit) loaded once, BEFORE the gate, so gate predicate + fast arith reuse them.
        if (l256) {
            avx_cpu_ldr_q(20, OFF_VHI + 16 * s1); // s1.hi -> v20
            if (I->is_mem)
                g_ldr_q(21, 17, 16);
            else
                avx_cpu_ldr_q(21, OFF_VHI + 16 * s2r); // s2.hi -> v21
        }
        // ---- NaN-input gate ----
        // NEON FADD/FMUL/FSUB/FDIV + emit_dnan is bit-exact to x86 for finite inputs and for GENERATED NaNs
        // (fixed up below), and for a SINGLE NaN input (propagated, quieted, sign preserved -- both ISAs
        // agree). But when a lane has TWO NaN inputs, x86 selects the SECOND operand's NaN while ARM selects
        // the FIRST -- a divergence do_avx also mishandles. Rather than reproduce x86's two-NaN priority
        // inline, gate: v24 = AND over the two (or four, for 256) input lanes of FCMEQ(x,x); any zero bit =>
        // some NaN input => exit to do_avx (correctness-first; == prior behavior). Real float kernels have no
        // NaN inputs, so the hot path is unaffected. Inputs are src1(s1)/src2(s2) only -- dest(d) is write-only.
        uint32_t EQ = dbl ? 0x4E60E400u : 0x4E20E400u;
        emit32(EQ | (s1 << 16) | (s1 << 5) | 24); // v24 = (s1==s1)
        emit32(EQ | (s2 << 16) | (s2 << 5) | 25);
        e_v3(0x4E201C00u, 24, 24, 25); // &= (s2==s2)
        if (l256) {
            emit32(EQ | (20 << 16) | (20 << 5) | 25);
            e_v3(0x4E201C00u, 24, 24, 25); // &= s1.hi
            emit32(EQ | (21 << 16) | (21 << 5) | 25);
            e_v3(0x4E201C00u, 24, 24, 25); // &= s2.hi
        }
        e_ext(25, 24, 24, 8);           // v25.d[0] = v24.d[1] (fold the two 64-bit halves)
        e_v3(0x4E201C00u, 24, 24, 25);  // v24.d[0] = lane0 & lane1
        e_fmov_from_d(16, 24);          // x16 = combined mask (all-ones iff NO input NaN)
        e_rrr(A_ORN, 16, 31, 16, 1, 0); // x16 = ~x16 (0 iff clean; nonzero iff a NaN input)
        uint32_t *p_cbz = (uint32_t *)g_cp;
        emit32(0);                                       // cbz x16, Lfast  (patched below)
        emit_exit_const(next - (uint64_t)I->len, R_AVX); // NaN present -> emulate this insn in C (this rip)
        uint8_t *Lfast = (uint8_t *)g_cp;
        *p_cbz = 0xB4000000u | ((uint32_t)(((Lfast - (uint8_t *)p_cbz) / 4) & 0x7FFFF) << 5) | 16;

        // ---- fast path: no input NaN ----
        emit_vex_fp(d, s1, s2, op, dbl); // low 128 -> host v[d]
        if (l256) {
            emit_vex_fp(22, 20, 21, op, dbl); // high 128 -> v22 (highs in v20/v21)
            avx_cpu_str_q(22, OFF_VHI + 16 * d);
        }
        avx_zero_upper(d, l256);
        return 1;
    }

    // ---- VPBLENDVB / VBLENDVPS / VBLENDVPD (VEX.128/.256.66.0F3A.W0 4C/4A/4B /r /is4): variable blend
    // by the mask's per-lane sign bit. 4-operand: dst=reg, src1=vvvv, src2=r/m, mask=is4 (imm[7:4]).
    //   4C vpblendvb  -> per BYTE   (sign bit = bit 7)   dst[i] = mask[i].signbit ? src2 : src1
    //   4A vblendvps  -> per 32-bit lane (sign bit = bit 31)
    //   4B vblendvpd  -> per 64-bit lane (sign bit = bit 63)
    // NEON: sel = SSHR(mask, #esize-1) replicates each lane's sign across the whole lane (all-ones/all-zero);
    // BSL sel, src2, src1 (where sel bit=1 take src2, else src1). The BSL is byte-granular but sel is uniform
    // per lane, so 32/64-bit selection is exact. Verified bit-exact vs qemu over random + sign-corner masks,
    // 128 and 256, reg and mem src2. (vblendps/pd immediate forms 0x0C/0x0D still fall to do_avx.)
    if (map == 3 && (op == 0x4A || op == 0x4B || op == 0x4C) && pp == 1) {
        int mreg = (I->imm >> 4) & 0xF;
        if (mreg > 15) return 0;
        int esz = (op == 0x4C) ? 8 : (op == 0x4A) ? 32 : 64; // lane bit-width; sign shift = esz-1
        int msh = esz - 1;
        mark_vdirty();
        int s2 = s2r;
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(16, 17, 0);
            s2 = 16;
        } // src2 low -> v16
        if (l256) { // load the three high halves BEFORE writing d.hi (d may alias src1/src2/mask)
            avx_cpu_ldr_q(20, OFF_VHI + 16 * mreg); // mask.hi
            avx_cpu_ldr_q(21, OFF_VHI + 16 * s1);   // src1.hi
            if (I->is_mem)
                g_ldr_q(22, 17, 16);
            else
                avx_cpu_ldr_q(22, OFF_VHI + 16 * s2r); // src2.hi
        }
        e_vshr_imm(18, mreg, esz, msh, 1); // v18 = sshr mask, #esz-1 (lane all-ones where sign set)
        e_v3(0x6E601C00u, 18, s2, s1);     // BSL v18.16b, src2.16b, src1.16b -> mask?src2:src1
        if (l256) {
            e_vshr_imm(19, 20, esz, msh, 1); // v19 = sshr mask.hi, #esz-1
            e_v3(0x6E601C00u, 19, 22, 21);   // BSL v19.16b, src2.hi, src1.hi
            e_vmov(d, 18);
            avx_cpu_str_q(19, OFF_VHI + 16 * d);
        } else {
            e_vmov(d, 18);
        }
        avx_zero_upper(d, l256);
        return 1;
    }
    return 2;
}

static int avx_lower_blend_and_compare(struct insn *I, uint64_t next) {
    int l256 = (I->vex_l == 1);
    int d = I->reg, s1 = I->vvvv, s2r = I->rm_reg, pp = I->vex_pp, map = I->vex_map, op = I->op;

    // ---- VPBLENDW (VEX.128/.256.66.0F3A.W0 0E /r ib): blend 16-bit words by imm8. 3-operand non-destructive:
    // dst=reg, src1=vvvv, src2=r/m. For each word i in 0..7: imm8 bit i set -> take src2.word[i] else
    // src1.word[i]. For 256-bit the same imm8 is applied to BOTH 128-bit lanes (words 0..7 within each lane).
    // Lowered at translate time: start from src1, then INS dst.h[i] <- src2.h[i] for each set imm bit. Exact
    // (pure word select). Verified vs qemu over a representative imm8 set, 128 and 256, reg and mem src2.
    if (map == 3 && op == 0x0E && pp == 1) {
        int imm = I->imm & 0xFF;
        mark_vdirty();
        int s2 = s2r;
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(16, 17, 0);
            s2 = 16;
        } // src2 low -> v16
        if (l256) {                               // load highs before writing d (d may alias src1/src2)
            avx_cpu_ldr_q(21, OFF_VHI + 16 * s1); // src1.hi
            if (I->is_mem)
                g_ldr_q(22, 17, 16);
            else
                avx_cpu_ldr_q(22, OFF_VHI + 16 * s2r); // src2.hi
        }
        e_vmov(23, s1); // low  = src1
        for (int i = 0; i < 8; i++)
            if (imm & (1 << i))
                emit32(0x6E000400u | ((unsigned)(((i << 2) | 2)) << 16) | ((unsigned)(i << 1) << 11) | (s2 << 5) |
                       23); // INS v23.h[i], src2.h[i]
        if (l256) {
            e_vmov(24, 21); // high = src1.hi
            for (int i = 0; i < 8; i++)
                if (imm & (1 << i))
                    emit32(0x6E000400u | ((unsigned)(((i << 2) | 2)) << 16) | ((unsigned)(i << 1) << 11) | (22 << 5) |
                           24); // INS v24.h[i], src2.hi.h[i]
            e_vmov(d, 23);
            avx_cpu_str_q(24, OFF_VHI + 16 * d);
        } else {
            e_vmov(d, 23);
        }
        avx_zero_upper(d, l256);
        return 1;
    }

    // ---- VCMPPS / VCMPPD (VEX.128/.256.0F.WIG C2 /r ib): packed FP compare, imm8 = predicate. Produces an
    // all-ones/all-zero mask per lane. ps -> no prefix (pp==0, .4s); pd -> 66 (pp==1, .2d). Scalar ss/sd
    // (F3/F2, pp>=2) fall to do_avx. a=src1(vvvv), b=src2(r/m). NEON FCMEQ/FCMGE/FCMGT (+ negate / swap /
    // ordered-test) reproduce each predicate's NaN result exactly (FCMGT/FCMGE are false for any NaN operand;
    // FCMEQ(x,x) is false iff x is NaN). Predicates 0x00-0x0F implemented; 0x10-0x1F share the same relational
    // result (they differ only in signaling behavior) so are mapped identically via imm&0x0F. Verified
    // bit-exact vs qemu incl equal/less/greater/-0/inf/QNaN/SNaN(both signs), 128 and 256, reg and mem.
    if (map == 1 && op == 0xC2 && pp < 2) {
        int p = I->imm & 0x1F, dbl = (pp == 1);
        mark_vdirty();
        int s2 = s2r;
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(16, 17, 0);
            s2 = 16;
        } // src2 low -> v16
        if (l256) {
            avx_cpu_ldr_q(20, OFF_VHI + 16 * s1); // a.hi
            if (I->is_mem)
                g_ldr_q(21, 17, 16);
            else
                avx_cpu_ldr_q(21, OFF_VHI + 16 * s2r); // b.hi
        }
        emit_vcmp_lane(d, s1, s2, p, dbl); // low 128 -> host v[d]
        if (l256) {
            emit_vcmp_lane(22, 20, 21, p, dbl);
            avx_cpu_str_q(22, OFF_VHI + 16 * d);
        }
        avx_zero_upper(d, l256);
        return 1;
    }

    // ---- broadcasts (map 2, pp 1): DUP element 0 across the whole vector. reg source (xmm low element)
    // or a memory scalar. vpbroadcastb/w/d/q (0x78/0x79/0x58/0x59), vbroadcastss/sd (0x18/0x19). Both
    // 128-bit lanes of a 256-bit dst are identical, so the high half is just a copy of the low. ----
    if (map == 2 && pp == 1 && (op == 0x78 || op == 0x79 || op == 0x58 || op == 0x59 || op == 0x18 || op == 0x19)) {
        int es = (op == 0x78) ? 1 : (op == 0x79) ? 2 : (op == 0x18 || op == 0x58) ? 4 : 8;
        int imm5 = es; // DUP element selector: b=1,h=2,s=4,d=8 (index 0)
        mark_vdirty();
        if (I->is_mem) {
            emit_ea(I, next);
            e_load(es, 16, 17);                                 // x16 = zero-extended es-byte scalar
            emit32(0x4E000C00u | (imm5 << 16) | (16 << 5) | d); // dup d.T, w16/x16
        } else {
            emit32(0x4E000400u | (imm5 << 16) | (s2r << 5) | d); // dup d.T, src.T[0]
        }
        if (l256) avx_cpu_str_q(d, OFF_VHI + 16 * d); // high lane == low lane
        avx_zero_upper(d, l256);
        return 1;
    }

    // ---- AVX2 variable shift (map 2, pp 1): 0x47 vpsllvd/q, 0x45 vpsrlvd/q, 0x46 vpsravd. Per-lane
    // USHL/SSHL with x86's >=esize saturation reproduced exactly (see emit_avx_varshift_lane). count = rm,
    // data = vvvv. VEX.W selects dword(0)/qword(1); 0x46 is dword-only. ----
    if (map == 2 && pp == 1 && (op == 0x45 || op == 0x46 || op == 0x47)) {
        int es = I->vex_w ? 8 : 4;
        if (op == 0x46 && es != 4) return 0; // vpsravq is AVX-512-only; leave to do_avx
        mark_vdirty();
        int s2 = s2r;
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(16, 17, 0);
            s2 = 16;
        } // count.lo -> v16
        if (l256) {                               // load highs before writing d
            avx_cpu_ldr_q(20, OFF_VHI + 16 * s1); // data.hi
            if (I->is_mem)
                g_ldr_q(21, 17, 16);
            else
                avx_cpu_ldr_q(21, OFF_VHI + 16 * s2r); // count.hi
        }
        emit_avx_varshift_lane(d, s1, s2, op, es); // low -> v[d]
        if (l256) {
            emit_avx_varshift_lane(22, 20, 21, op, es);
            avx_cpu_str_q(22, OFF_VHI + 16 * d);
        }
        avx_zero_upper(d, l256);
        return 1;
    }

    // ---- vpshufd (map 1, 66, 0x70): per-128-lane dword shuffle by imm8 (dword j <- src.dword[imm[2j+1:2j]]).
    // Resolve the lane selection at translate time into 4 INS.s per 128-bit lane. 2-operand (rm=src). The
    // F2/F3 forms (vpshuflw/hw) have pp!=1 and fall to do_avx. ----
    if (map == 1 && op == 0x70 && pp == 1) {
        int imm = I->imm & 0xFF;
        mark_vdirty();
        int src = s2r;
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(16, 17, 0);
            src = 16;
        }
        for (int j = 0; j < 4; j++)
            e_ins_s(23, j, src, (imm >> (2 * j)) & 3); // low -> v23
        if (l256) {
            int srch = 20;
            if (I->is_mem)
                g_ldr_q(20, 17, 16);
            else
                avx_cpu_ldr_q(20, OFF_VHI + 16 * s2r);
            for (int j = 0; j < 4; j++)
                e_ins_s(24, j, srch, (imm >> (2 * j)) & 3); // high -> v24
            e_vmov(d, 23);
            avx_cpu_str_q(24, OFF_VHI + 16 * d);
        } else {
            e_vmov(d, 23);
        }
        avx_zero_upper(d, l256);
        return 1;
    }

    // ---- vpunpckl/h bw/wd/dq/qdq (map 1, 66): per-128-lane interleave -> NEON ZIP1/ZIP2. src1=vvvv, src2=rm.
    // x86 interleaves within each 128-bit lane, exactly ZIP1/ZIP2 on the two 128-bit Q inputs. ----
    if (map == 1 && pp == 1) {
        int zip2 = -1, zsz = -1;
        switch (op) {
        case 0x60:
            zsz = 0;
            zip2 = 0;
            break; // vpunpcklbw
        case 0x61:
            zsz = 1;
            zip2 = 0;
            break; // vpunpcklwd
        case 0x62:
            zsz = 2;
            zip2 = 0;
            break; // vpunpckldq
        case 0x6C:
            zsz = 3;
            zip2 = 0;
            break; // vpunpcklqdq
        case 0x68:
            zsz = 0;
            zip2 = 1;
            break; // vpunpckhbw
        case 0x69:
            zsz = 1;
            zip2 = 1;
            break; // vpunpckhwd
        case 0x6A:
            zsz = 2;
            zip2 = 1;
            break; // vpunpckhdq
        case 0x6D:
            zsz = 3;
            zip2 = 1;
            break; // vpunpckhqdq
        default: break;
        }
        if (zsz >= 0) {
            uint32_t zbase = (zip2 ? 0x4E007800u : 0x4E003800u) | ((uint32_t)zsz << 22);
            mark_vdirty();
            int s2 = s2r;
            if (I->is_mem) {
                emit_ea(I, next);
                g_ldr_q(16, 17, 0);
                s2 = 16;
            }
            if (l256) {
                if (I->is_mem)
                    g_ldr_q(21, 17, 16);
                else
                    avx_cpu_ldr_q(21, OFF_VHI + 16 * s2r);
                avx_cpu_ldr_q(20, OFF_VHI + 16 * s1);
                e_v3(zbase, 22, 20, 21); // high = zip(s1.hi, s2.hi)
                e_v3(zbase, d, s1, s2);  // low
                avx_cpu_str_q(22, OFF_VHI + 16 * d);
            } else {
                e_v3(zbase, d, s1, s2);
            }
            avx_zero_upper(d, l256);
            return 1;
        }
    }

    // ---- vpermd / vpermps (map 2, 66, 0x36 / 0x16): full cross-lane 32-bit permute across the whole 256
    // bits: dst.dword[i] = data.dword[ctrl.dword[i] & 7]. data=rm, ctrl=vvvv. Lowered as a TBL over the
    // 32-byte table {data.lo, data.hi}: build a per-output byte index = (ctrl.dword[i]&7)*4 + {0,1,2,3}.
    //   sel  = ctrl & 7            (AND.4s)          -- x86's index&7, exact for any control value
    //   base = sel << 2            (SHL.4s #2)       -- byte offset of the selected dword (0..28)
    //   rep  = base * 0x01010101   (MUL.4s)          -- replicate the byte across the dword (no carry, <256)
    //   idx  = rep + 0x03020100    (ADD.16b)         -- the 4 consecutive source bytes of that dword
    //   out  = TBL {data.lo,data.hi}, idx            -- gather. VEX.256 only (no 128-bit encoding). ----
    return 2;
}

static int avx_lower_permute_and_convert(struct insn *I, uint64_t next) {
    int l256 = (I->vex_l == 1);
    int d = I->reg, s1 = I->vvvv, s2r = I->rm_reg, pp = I->vex_pp, map = I->vex_map, op = I->op;
    if (map == 2 && pp == 1 && (op == 0x36 || op == 0x16) && l256) {
        mark_vdirty();
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(20, 17, 0);
            g_ldr_q(21, 17, 16);
        } // table {lo,hi}
        else {
            e_vmov(20, s2r);
            avx_cpu_ldr_q(21, OFF_VHI + 16 * s2r);
        }
        avx_cpu_ldr_q(25, OFF_VHI + 16 * s1); // ctrl.hi (ctrl.lo stays in v[s1])
        e_movconst(16, 7);
        emit32(0x4E040C00u | (16 << 5) | 26); // v26.4s = 7
        e_movconst(16, 0x01010101);
        emit32(0x4E040C00u | (16 << 5) | 27); // v27.4s = 0x01010101
        e_movconst(16, 0x03020100);
        emit32(0x4E040C00u | (16 << 5) | 28); // v28.4s = 0x03020100
        // low output dwords 0..3 (from ctrl.lo = v[s1]) -> v22
        e_v3(0x4E201C00u, 24, s1, 26);                     // sel = ctrl.lo & 7
        e_vshl_imm(24, 24, 32, 2);                         // base = sel*4
        e_v3(0x4EA09C00u, 24, 24, 27);                     // rep  = base*0x01010101
        e_v3(0x4E208400u, 24, 24, 28);                     // idx  = rep + {0,1,2,3}
        emit32(0x4E002000u | (24 << 16) | (20 << 5) | 22); // tbl v22.16b, {v20,v21}, v24
        // high output dwords 4..7 (from ctrl.hi = v25) -> v23
        e_v3(0x4E201C00u, 24, 25, 26);
        e_vshl_imm(24, 24, 32, 2);
        e_v3(0x4EA09C00u, 24, 24, 27);
        e_v3(0x4E208400u, 24, 24, 28);
        emit32(0x4E002000u | (24 << 16) | (20 << 5) | 23);
        e_vmov(d, 22);
        avx_cpu_str_q(23, OFF_VHI + 16 * d);
        avx_zero_upper(d, l256);
        return 1;
    }

    // ---- vpackusdw (VEX.128/.256.66.0F38.W0 2B): pack 2x4 signed dwords -> 8 UNSIGNED words with unsigned
    // saturation, WITHIN each 128-bit lane. x86: value<0 -> 0, >0xFFFF -> 0xFFFF; dst.lane = {sat(src1.lane),
    // sat(src2.lane)}. NEON SQXTUN (signed 32 -> unsigned 16 saturating) reproduces x86's saturation exactly.
    // src1=vvvv, src2=rm. SQXTUN fills the low 4h (and zeroes bits[127:64]); SQXTUN2 fills the high 4h. Per-128
    // lane packing (low result = sat(src1), high result = sat(src2)) matches x86's per-128-lane pack order for
    // 256-bit. Verified bit-exact vs qemu (neg / in-range / >0xFFFF / boundaries, 128+256, reg+mem).
    if (map == 2 && pp == 1 && op == 0x2B) {
        mark_vdirty();
        int s2 = s2r;
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(16, 17, 0);
            s2 = 16;
        }
        if (l256) {
            avx_cpu_ldr_q(20, OFF_VHI + 16 * s1); // src1.hi
            if (I->is_mem)
                g_ldr_q(21, 17, 16);
            else
                avx_cpu_ldr_q(21, OFF_VHI + 16 * s2r); // src2.hi
        }
        emit32(0x2E612800u | (s1 << 5) | 23); // sqxtun  v23.4h, src1.4s  (low 4 words = sat(src1))
        emit32(0x6E612800u | (s2 << 5) | 23); // sqxtun2 v23.8h, src2.4s  (high 4 words = sat(src2))
        if (l256) {
            emit32(0x2E612800u | (20 << 5) | 24);
            emit32(0x6E612800u | (21 << 5) | 24);
            e_vmov(d, 23);
            avx_cpu_str_q(24, OFF_VHI + 16 * d);
        } else {
            e_vmov(d, 23);
        }
        avx_zero_upper(d, l256);
        return 1;
    }

    // ---- vpshufb VEX (VEX.128/.256.66.0F38.W0 00): byte shuffle WITHIN each 128-bit lane. dst[i] =
    // (idx[i] & 0x80) ? 0 : data[idx[i] & 0x0F]; the index's low 4 bits select within the SAME 128-bit lane.
    // data=vvvv(src1), idx=rm(src2). Mirrors the legacy PSHUFB lowering (lower/crypto.c): AND the control with
    // 0x8f so ARM TBL (which zeroes for index >= 16) reproduces x86's bit7-zeroing exactly; TBL each 128-bit
    // lane separately since indices are lane-local. Verified vs qemu (MSB-set -> 0, in-lane select, 128+256, reg+mem).
    if (map == 2 && pp == 1 && op == 0x00) {
        mark_vdirty();
        int s2 = s2r;
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(16, 17, 0);
            s2 = 16;
        }
        if (l256) {
            avx_cpu_ldr_q(20, OFF_VHI + 16 * s1); // data.hi
            if (I->is_mem)
                g_ldr_q(21, 17, 16);
            else
                avx_cpu_ldr_q(21, OFF_VHI + 16 * s2r); // idx.hi
        }
        emit32(0x4F04E5E0u | 25);                          // movi v25.16b, #0x8f
        e_v3(0x4E201C00u, 18, s2, 25);                     // v18 = idx & 0x8f
        emit32(0x4E000000u | (18 << 16) | (s1 << 5) | 23); // tbl v23.16b, {data.16b}, v18
        if (l256) {
            e_v3(0x4E201C00u, 18, 21, 25);                     // v18 = idx.hi & 0x8f
            emit32(0x4E000000u | (18 << 16) | (20 << 5) | 24); // tbl v24.16b, {data.hi.16b}, v18
            e_vmov(d, 23);
            avx_cpu_str_q(24, OFF_VHI + 16 * d);
        } else {
            e_vmov(d, 23);
        }
        avx_zero_upper(d, l256);
        return 1;
    }

    // ---- vpsadbw (VEX.128/.256.66.0F.WIG F6): sum of absolute differences. For each 64-bit lane, sum |a[i]-b[i]|
    // over its 8 unsigned bytes -> a 16-bit result in bits[15:0] of that qword (bits[63:16] = 0). NEON: UABD
    // (unsigned |a-b| per byte), then a 3-step UADDLP pairwise-widening reduction (16b->8h->4s->2d) sums each
    // group of 8 bytes into the low 16 bits of its 64-bit lane, zero-extended -- exactly x86's layout (max sum
    // 8*255=2040 fits in 16 bits). src1=vvvv, src2=rm. Verified vs qemu (max diffs, result placement + zeros, 128+256).
    if (map == 1 && pp == 1 && op == 0xF6) {
        mark_vdirty();
        int s2 = s2r;
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(16, 17, 0);
            s2 = 16;
        }
        if (l256) {
            avx_cpu_ldr_q(20, OFF_VHI + 16 * s1); // src1.hi
            if (I->is_mem)
                g_ldr_q(21, 17, 16);
            else
                avx_cpu_ldr_q(21, OFF_VHI + 16 * s2r); // src2.hi
        }
        emit32(0x6E207400u | (s2 << 16) | (s1 << 5) | 23); // uabd   v23.16b, src1.16b, src2.16b
        emit32(0x6E202800u | (23 << 5) | 23);              // uaddlp v23.8h, v23.16b
        emit32(0x6E602800u | (23 << 5) | 23);              // uaddlp v23.4s, v23.8h
        emit32(0x6EA02800u | (23 << 5) | 23);              // uaddlp v23.2d, v23.4s
        if (l256) {
            emit32(0x6E207400u | (21 << 16) | (20 << 5) | 24);
            emit32(0x6E202800u | (24 << 5) | 24);
            emit32(0x6E602800u | (24 << 5) | 24);
            emit32(0x6EA02800u | (24 << 5) | 24);
            e_vmov(d, 23);
            avx_cpu_str_q(24, OFF_VHI + 16 * d);
        } else {
            e_vmov(d, 23);
        }
        avx_zero_upper(d, l256);
        return 1;
    }

    // ---- vcvtdq2ps(NP) / vcvtps2dq(66,round) / vcvttps2dq(F3,trunc): packed 32-bit int<->float, same lane
    // count. NP -> SCVTF.4s (rounds under current FPCR.RMode == guest MXCSR.RC, matching x86/qemu). The
    // float->int forms saturate on ARM but x86 yields 0x80000000 for NaN/overflow -- emit_ps2dq_128 blends
    // that in. 2-operand (src = r/m; vvvv unused). Verified bit-exact vs qemu over normal/rounding/negative/
    // zero/>INT_MAX/<INT_MIN/NaN/+-inf, 128 and 256, reg and mem. (pp==3/F2 is not a valid 0x5B -> do_avx.)
    if (map == 1 && op == 0x5B && pp <= 2) {
        mark_vdirty();
        int src = s2r, srch = 20;
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(16, 17, 0);
            src = 16;
        }
        if (l256) {
            if (I->is_mem)
                g_ldr_q(20, 17, 16);
            else
                avx_cpu_ldr_q(20, OFF_VHI + 16 * s2r);
        }
        if (pp == 0) {                                        // cvtdq2ps
            emit32(0x4E21D800u | (src << 5) | 23);            // SCVTF.4s v23, src
            if (l256) emit32(0x4E21D800u | (srch << 5) | 24); // SCVTF.4s v24, src.hi
        } else {                                              // cvtps2dq(pp==1 round) / cvttps2dq(pp==2 trunc)
            int trunc = (pp == 2);
            e_movconst(16, 0x4F000000u);
            emit32(0x4E040C00u | (16 << 5) | 25); // v25.4s = 2^31 (f32)
            e_movconst(16, 0x80000000u);
            emit32(0x4E040C00u | (16 << 5) | 26); // v26.4s = 0x80000000
            emit_ps2dq_128(23, src, trunc, 25, 26, 27, 28);
            if (l256) emit_ps2dq_128(24, srch, trunc, 25, 26, 27, 28);
        }
        e_vmov(d, 23);
        if (l256) avx_cpu_str_q(24, OFF_VHI + 16 * d);
        avx_zero_upper(d, l256);
        return 1;
    }

    // ---- vcvtps2pd(NP, widen 4f32->4f64) / vcvtpd2ps(66, narrow 4f64->4f32): packed float widen/narrow.
    // FCVTL/FCVTL2 (single->double is always exact) and FCVTN/FCVTN2 (double->single rounds under current
    // FPCR.RMode, and inf/overflow saturate to inf exactly as x86). 2-operand (src=r/m). The scalar ss/sd
    // forms (F3/F2, pp>=2) fall to do_avx. Verified bit-exact vs qemu, 128 and 256, reg and mem.
    if (map == 1 && op == 0x5A && pp < 2) {
        mark_vdirty();
        int src = s2r;
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(16, 17, 0);
            src = 16;
        }
        if (pp == 0) {                             // ps->pd: low 2 floats (and, for 256, high 2) widen to doubles
            emit32(0x0E617800u | (src << 5) | 23); // FCVTL.2d  v23, src.2s
            if (l256) emit32(0x4E617800u | (src << 5) | 24); // FCVTL2.2d v24, src.4s
            e_vmov(d, 23);
            if (l256) avx_cpu_str_q(24, OFF_VHI + 16 * d);
        } else { // pd->ps: 2 (or 4 for 256) doubles narrow to floats, all landing in the low 128
            emit32(0x0E616800u | (src << 5) | 23); // FCVTN.2s v23, src.2d  (low 2 floats)
            if (l256) {
                if (I->is_mem)
                    g_ldr_q(20, 17, 16);
                else
                    avx_cpu_ldr_q(20, OFF_VHI + 16 * s2r); // src.hi
                emit32(0x4E616800u | (20 << 5) | 23);      // FCVTN2.4s v23, src.hi.2d (high 2 floats)
            }
            e_vmov(d, 23);
        }
        avx_zero_upper(d, l256);
        return 1;
    }

    // ---- vcvtdq2pd(F3, int32->f64 widen) / vcvttpd2dq(66,trunc) / vcvtpd2dq(F2,round): 32-bit int <-> f64.
    // dq2pd: SXTL/SXTL2 int32->int64 then SCVTF.2d (exact). pd2dq: round/trunc to int64 then narrow to int32
    // (XTN/XTN2), with x86's 0x80000000 indefinite blended per emit_pd2i32_pieces. 2-operand (src=r/m).
    // Verified bit-exact vs qemu over the same corner set (incl overflow/NaN), 128 and 256, reg and mem.
    // (pp==0/NP is not a valid 0xE6 -> do_avx.)
    return 2;
}

static int avx_lower_conversion_edges(struct insn *I, uint64_t next) {
    int l256 = (I->vex_l == 1);
    int d = I->reg, s1 = I->vvvv, s2r = I->rm_reg, pp = I->vex_pp, map = I->vex_map, op = I->op;
    if (map == 1 && op == 0xE6 && pp >= 1) {
        mark_vdirty();
        int src = s2r;
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(16, 17, 0);
            src = 16;
        }
        if (pp == 2) {                             // cvtdq2pd: int32 -> double (exact widen)
            emit32(0x0F20A400u | (src << 5) | 23); // SXTL.2d  v23, src.2s
            emit32(0x4E61D800u | (23 << 5) | 23);  // SCVTF.2d v23, v23
            if (l256) {
                emit32(0x4F20A400u | (src << 5) | 24); // SXTL2.2d v24, src.4s (high 2 int32)
                emit32(0x4E61D800u | (24 << 5) | 24);  // SCVTF.2d v24, v24
            }
            e_vmov(d, 23);
            if (l256) avx_cpu_str_q(24, OFF_VHI + 16 * d);
        } else { // pd->dq: cvttpd2dq(pp==1 trunc) / cvtpd2dq(pp==3 round)
            int trunc = (pp == 1);
            e_movconst(16, 0x41E0000000000000ull);
            emit32(0x4E080C00u | (16 << 5) | 25); // v25.2d = 2^31 (f64)
            e_movconst(16, 0xC1E0000000000000ull);
            emit32(0x4E080C00u | (16 << 5) | 26); // v26.2d = -2^31
            e_movconst(16, 0x80000000u);
            emit32(0x4E040C00u | (16 << 5) | 27); // v27.4s = 0x80000000
            // Compute the int64 results + per-64 fixup masks for BOTH halves first (they consume the +/-2^31
            // consts in v25/v26), THEN narrow -- the narrow step reuses v25 for the packed 32-bit mask.
            emit_pd2i32_pieces(22, 18, src, trunc, 25, 26, 28, 21); // lo: r=v22, mask=v18
            if (l256) {
                if (I->is_mem)
                    g_ldr_q(20, 17, 16);
                else
                    avx_cpu_ldr_q(20, OFF_VHI + 16 * s2r);             // src.hi
                emit_pd2i32_pieces(23, 19, 20, trunc, 25, 26, 28, 21); // hi: r=v23, mask=v19
            }
            emit32(0x0EA12800u | (22 << 5) | 24); // XTN.2s  v24, v22  (low 2 int32)
            emit32(0x0EA12800u | (18 << 5) | 25); // XTN.2s  v25, v18  (low 2 mask lanes)
            if (l256) {
                emit32(0x4EA12800u | (23 << 5) | 24); // XTN2.4s v24, v23 (high 2 int32)
                emit32(0x4EA12800u | (19 << 5) | 25); // XTN2.4s v25, v19 (high 2 mask lanes)
            }
            e_v3(0x6E601C00u, 25, 27, 24); // BSL v25 = mask ? 0x80000000 : result
            e_vmov(d, 25);
        }
        avx_zero_upper(d, l256);
        return 1;
    }

    // ---- vpermilps imm (VEX.66.0F3A.W0 04 /r ib): per-128-lane dword permute, dst.dword[j] <-
    // src.dword[imm[2j+1:2j]]. Single source (r/m); same imm applied to both 128-bit lanes. Resolved to 4
    // INS.s per lane (== the vpshufd lowering, float lanes). Verified bit-exact vs qemu, 128+256, reg+mem.
    if (map == 3 && op == 0x04 && pp == 1) {
        int imm = I->imm & 0xFF;
        mark_vdirty();
        int src = s2r;
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(16, 17, 0);
            src = 16;
        }
        for (int j = 0; j < 4; j++)
            e_ins_s(23, j, src, (imm >> (2 * j)) & 3);
        if (l256) {
            if (I->is_mem)
                g_ldr_q(20, 17, 16);
            else
                avx_cpu_ldr_q(20, OFF_VHI + 16 * s2r);
            for (int j = 0; j < 4; j++)
                e_ins_s(24, j, 20, (imm >> (2 * j)) & 3);
            e_vmov(d, 23);
            avx_cpu_str_q(24, OFF_VHI + 16 * d);
        } else {
            e_vmov(d, 23);
        }
        avx_zero_upper(d, l256);
        return 1;
    }

    // ---- vpermilpd imm (VEX.66.0F3A.W0 05 /r ib): per-128-lane qword permute; dst.qword[k] <-
    // src.qword[imm bit], consecutive imm bits across the (up to 4) qwords. Single source. 2 INS.d per lane.
    if (map == 3 && op == 0x05 && pp == 1) {
        int imm = I->imm & 0xFF;
        mark_vdirty();
        int src = s2r;
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(16, 17, 0);
            src = 16;
        }
        e_ins_d(23, 0, src, imm & 1);
        e_ins_d(23, 1, src, (imm >> 1) & 1);
        if (l256) {
            if (I->is_mem)
                g_ldr_q(20, 17, 16);
            else
                avx_cpu_ldr_q(20, OFF_VHI + 16 * s2r);
            e_ins_d(24, 0, 20, (imm >> 2) & 1);
            e_ins_d(24, 1, 20, (imm >> 3) & 1);
            e_vmov(d, 23);
            avx_cpu_str_q(24, OFF_VHI + 16 * d);
        } else {
            e_vmov(d, 23);
        }
        avx_zero_upper(d, l256);
        return 1;
    }

    // ---- vpermilps var (VEX.66.0F38.W0 0C /r): per-128-lane dword permute by a vector control. data=vvvv,
    // control=r/m; dst.dword[j] = data.dword[ctrl.dword[j] & 3] within each 128-bit lane. Lowered to a
    // per-lane TBL over the lane's 16-byte data: idx = (ctrl&3)*4 + {0,1,2,3} byte pattern. Verified vs qemu.
    if (map == 2 && pp == 1 && op == 0x0C) {
        mark_vdirty();
        int ctl = s2r;
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(16, 17, 0);
            ctl = 16;
        }
        e_movconst(16, 3);
        emit32(0x4E040C00u | (16 << 5) | 25); // v25.4s = 3
        e_movconst(16, 0x01010101);
        emit32(0x4E040C00u | (16 << 5) | 26); // v26.4s = 0x01010101
        e_movconst(16, 0x03020100);
        emit32(0x4E040C00u | (16 << 5) | 27);              // v27.4s = 0x03020100
        e_v3(0x4E201C00u, 28, ctl, 25);                    // sel = ctrl & 3
        e_vshl_imm(28, 28, 32, 2);                         // base = sel*4
        e_v3(0x4EA09C00u, 28, 28, 26);                     // rep  = base*0x01010101
        e_v3(0x4E208400u, 28, 28, 27);                     // idx  = rep + {0,1,2,3}
        emit32(0x4E000000u | (28 << 16) | (s1 << 5) | 23); // TBL v23.16b, {data.lo}, idx
        if (l256) {
            if (I->is_mem)
                g_ldr_q(20, 17, 16);
            else
                avx_cpu_ldr_q(20, OFF_VHI + 16 * s2r); // ctrl.hi
            avx_cpu_ldr_q(21, OFF_VHI + 16 * s1);      // data.hi
            e_v3(0x4E201C00u, 28, 20, 25);
            e_vshl_imm(28, 28, 32, 2);
            e_v3(0x4EA09C00u, 28, 28, 26);
            e_v3(0x4E208400u, 28, 28, 27);
            emit32(0x4E000000u | (28 << 16) | (21 << 5) | 24); // TBL v24, {data.hi}, idx
            e_vmov(d, 23);
            avx_cpu_str_q(24, OFF_VHI + 16 * d);
        } else {
            e_vmov(d, 23);
        }
        avx_zero_upper(d, l256);
        return 1;
    }

    // ---- vpermilpd var (VEX.66.0F38.W0 0D /r): per-128-lane qword permute by a vector control. data=vvvv,
    // control=r/m; dst.qword[k] = data.qword[(ctrl.qword[k]>>1)&1] within each 128-bit lane. Only two source
    // qwords, so lower as: A=dup(data.q0), B=dup(data.q1), mask = sign-replicate(ctrl bit1) per 64, BSL.
    if (map == 2 && pp == 1 && op == 0x0D) {
        mark_vdirty();
        int ctl = s2r;
        if (I->is_mem) {
            emit_ea(I, next);
            g_ldr_q(16, 17, 0);
            ctl = 16;
        }
        emit32(0x4E080400u | (s1 << 5) | 25); // DUP v25.2d, data.d[0]  (A = both lanes = q0)
        emit32(0x4E180400u | (s1 << 5) | 26); // DUP v26.2d, data.d[1]  (B = both lanes = q1)
        e_vshl_imm(28, ctl, 64, 62);          // bring ctrl bit1 to bit63 of each qword
        e_vshr_imm(28, 28, 64, 63, 1);        // SSHR -> all-ones where bit1 set
        e_v3(0x6E601C00u, 28, 26, 25);        // BSL v28 = mask ? B(q1) : A(q0)
        e_vmov(23, 28);
        if (l256) {
            if (I->is_mem)
                g_ldr_q(20, 17, 16);
            else
                avx_cpu_ldr_q(20, OFF_VHI + 16 * s2r); // ctrl.hi
            avx_cpu_ldr_q(21, OFF_VHI + 16 * s1);      // data.hi
            emit32(0x4E080400u | (21 << 5) | 25);
            emit32(0x4E180400u | (21 << 5) | 26);
            e_vshl_imm(28, 20, 64, 62);
            e_vshr_imm(28, 28, 64, 63, 1);
            e_v3(0x6E601C00u, 28, 26, 25);
            e_vmov(24, 28);
            e_vmov(d, 23);
            avx_cpu_str_q(24, OFF_VHI + 16 * d);
        } else {
            e_vmov(d, 23);
        }
        avx_zero_upper(d, l256);
        return 1;
    }
    return 2;
}

static int avx_lower_logical_arithmetic(struct insn *I, uint64_t next) {
    int l256 = (I->vex_l == 1);
    int d = I->reg, s1 = I->vvvv, s2r = I->rm_reg, pp = I->vex_pp, map = I->vex_map, op = I->op;

    // ---- 3-operand arithmetic / logical ----
    uint32_t base = 0;
    int swap = 0; // operands reversed (pandn/andn: dst = ~src1 & src2 = BIC(vn=src2, vm=src1))
    if (map == 1) {
        switch (op) {
        // bitwise (element-agnostic .16b); unique opcodes -> no pp gate needed
        case 0xEF:
        case 0x57: base = 0x6E201C00u; break; // vpxor / vxorps,pd
        case 0xDB:
        case 0x54: base = 0x4E201C00u; break; // vpand / vandps,pd
        case 0xEB:
        case 0x56: base = 0x4EA01C00u; break; // vpor  / vorps,pd
        case 0xDF:
        case 0x55:
            base = 0x4E601C00u;
            swap = 1;
            break; // vpandn / vandnps,pd (BIC)
        default: break;
        }
        if (!base && pp == 1) switch (op) {       // 66-prefixed packed integer
            case 0xFC: base = 0x4E208400u; break; // vpaddb
            case 0xFD: base = 0x4E608400u; break; // vpaddw
            case 0xFE: base = 0x4EA08400u; break; // vpaddd
            case 0xD4: base = 0x4EE08400u; break; // vpaddq
            case 0xF8: base = 0x6E208400u; break; // vpsubb
            case 0xF9: base = 0x6E608400u; break; // vpsubw
            case 0xFA: base = 0x6EA08400u; break; // vpsubd
            case 0xFB: base = 0x6EE08400u; break; // vpsubq
            case 0x74: base = 0x6E208C00u; break; // vpcmpeqb (CMEQ)
            case 0x75: base = 0x6E608C00u; break; // vpcmpeqw
            case 0x76: base = 0x6EA08C00u; break; // vpcmpeqd
            case 0x64: base = 0x4E203400u; break; // vpcmpgtb (CMGT signed)
            case 0x65: base = 0x4E603400u; break; // vpcmpgtw
            case 0x66:
                base = 0x4EA03400u;
                break; // vpcmpgtd
            // integer min/max (bit-exact: NEON SMIN/UMIN/SMAX/UMAX == x86, no NaN concerns). map1 legacy forms.
            case 0xDA: base = 0x6E206C00u; break; // vpminub (UMIN.16b)
            case 0xDE: base = 0x6E206400u; break; // vpmaxub (UMAX.16b)
            case 0xEA: base = 0x4E606C00u; break; // vpminsw (SMIN.8h)
            case 0xEE: base = 0x4E606400u; break; // vpmaxsw (SMAX.8h)
            default: break;
            }
        // NOTE: packed FP add/sub/mul/div (0x58/0x59/0x5C/0x5E) are lowered above (emit_vex_fp), before
        // this generic base path, since they need the generated-NaN sign fixup the plain integer ops don't.
    } else if (map == 2 && pp == 1)
        switch (op) {                         // 0F38 SSE4.1 integer min/max + multiply
        case 0x40: base = 0x4EA09C00u; break; // vpmulld (MUL.4s)
        case 0x38: base = 0x4E206C00u; break; // vpminsb (SMIN.16b)
        case 0x39: base = 0x4EA06C00u; break; // vpminsd (SMIN.4s)
        case 0x3A: base = 0x6E606C00u; break; // vpminuw (UMIN.8h)
        case 0x3B: base = 0x6EA06C00u; break; // vpminud (UMIN.4s)
        case 0x3C: base = 0x4E206400u; break; // vpmaxsb (SMAX.16b)
        case 0x3D: base = 0x4EA06400u; break; // vpmaxsd (SMAX.4s)
        case 0x3E: base = 0x6E606400u; break; // vpmaxuw (UMAX.8h)
        case 0x3F: base = 0x6EA06400u; break; // vpmaxud (UMAX.4s)
        default: break;
        }
    if (!base) return 0;

    mark_vdirty();
    int s2 = s2r;
    if (I->is_mem) {
        emit_ea(I, next);
        g_ldr_q(16, 17, 0);
        s2 = 16;
    }
    if (swap)
        e_v3(base, d, s2, s1);
    else
        e_v3(base, d, s1, s2); // low 128 -> host v[d]
    if (l256) {                // high 128 via cpu->vhi
        if (I->is_mem)
            g_ldr_q(21, 17, 16);
        else
            avx_cpu_ldr_q(21, OFF_VHI + 16 * s2r);
        avx_cpu_ldr_q(20, OFF_VHI + 16 * s1);
        if (swap)
            e_v3(base, 22, 21, 20);
        else
            e_v3(base, 22, 20, 21);
        avx_cpu_str_q(22, OFF_VHI + 16 * d);
    }
    avx_zero_upper(d, l256);
    return 1;
}

// Returns 1 if the VEX insn was lowered inline (caller does gpc = next; continue), else 0 (fall through
// to the R_AVX do_avx exit). Correctness-first: only a vetted, bit-exact-vs-qemu subset is claimed here.
static int avx_lower(struct insn *I, uint64_t next) {
    /*
     * The C AVX/SSE emulator resolves logical mappings through the target
     * memory callbacks. Keep all memory-backed VEX/EVEX forms on that single
     * audited path while soft mappings are active; register-only forms retain
     * their inline fast path.
     */
    if (I->is_mem && emit_soft_memory_active()) return AVX_LOWER_DECLINED;
    if (!I->vex || I->evex || I->vex_l > 1) return AVX_LOWER_DECLINED;
    if (I->reg > 15 || I->vvvv > 15 || I->rm_reg > 15) return AVX_LOWER_DECLINED;

    int result = avx_lower_control_and_moves(I, next);
    if (result != AVX_LOWER_UNMATCHED) return result;
    result = avx_lower_fused_arithmetic(I, next);
    if (result != AVX_LOWER_UNMATCHED) return result;
    result = avx_lower_blend_and_compare(I, next);
    if (result != AVX_LOWER_UNMATCHED) return result;
    result = avx_lower_permute_and_convert(I, next);
    if (result != AVX_LOWER_UNMATCHED) return result;
    result = avx_lower_conversion_edges(I, next);
    if (result != AVX_LOWER_UNMATCHED) return result;
    result = avx_lower_logical_arithmetic(I, next);
    return result == AVX_LOWER_UNMATCHED ? AVX_LOWER_DECLINED : result;
}

// Emit the per-edge deferred-flag spill hl_x86_trace_jcc_flags requested for one Jcc edge stub:
// SUB -> e_nzcv_save (live NZCV already borrow-canonical); LOGIC -> e_nzcv_save_c1 (recompute x86
// CF=0/OF=0 from the live N/Z before the store); NONE -> nothing (successor overwrites first).
static void emit_jcc_edge_spill(int kind) {
    if (kind == HL_X86_JCC_SPILL_LOGIC)
        e_nzcv_save_c1();
    else if (kind != HL_X86_JCC_SPILL_NONE)
        e_nzcv_save();
}

// Handles vector encodings before the legacy flag pipeline. TX_FALL means the
// instruction is not a vector-family encoding; TX_NEXT advances the decode
// loop; TX_BREAK ends the translated block after emitting its exit.
static int lower_vector_family(struct insn *instruction, uint64_t guest_pc, uint64_t next,
                               hl_x86_crypto_state *crypto_state) {
    if (instruction->vex) {
        // EVEX map zero is reserved (and legacy BOUND is invalid in 64-bit mode).
        if (instruction->evex && instruction->vex_map == 0) {
            emit_guest_signal(guest_pc, 4, 2);
            return TX_BREAK;
        }
        if (g_fl_pending) flags_materialize();
        if (!nosseopt() && avx_lower(instruction, next)) return TX_NEXT;
        emit_exit_const(guest_pc, R_AVX);
        return TX_BREAK;
    }
    if (!instruction->map3) return TX_FALL;

    if (g_fl_pending) flags_materialize();
    if (hl_x86_lower_crypto(instruction, next, crypto_state) == TX_NEXT) {
        // Inline crypto and shuffle lowering writes guest XMM state.
        mark_vdirty();
        return TX_NEXT;
    }
    const hl_x86_sse4x_state sse4x_state = {.optimize = !nosseopt()};
    if (hl_x86_lower_sse4x(instruction, next, &sse4x_state) == TX_NEXT) return TX_NEXT;

    // PCMPISTRI equal-each byte is the SSE4.2 strcmp hot loop. Other forms use
    // the correctness-first C softmulator below.
    if (instruction->map3 == 3 && instruction->op == 0x63 && !nosseopt() &&
        (instruction->imm & 0x0D) == 0x08) {
        int right = 16;
        if (instruction->is_mem)
            g_ldr_q_ea(16, instruction, next);
        else
            right = instruction->rm_reg;
        emit_pcmpistri_eqeach_byte(instruction->reg, right, (int)instruction->imm);
        return TX_NEXT;
    }
    emit_exit_const(guest_pc, R_SSE3B);
    return TX_BREAK;
}

// Resolves deferred NZCV/PF/AF state before a legacy instruction is emitted.
// Vector families return before this boundary because their emulators own no
// legacy flag effects represented by this pipeline.
static void prepare_legacy_flags(const struct insn *instruction, uint64_t guest_pc, uint64_t next,
                                 const hl_x86_trace_state *trace_state) {
    uint8_t opcode = instruction->op;
    int conditional = (!instruction->two && opcode >= 0x70 && opcode <= 0x7F) ||
                      (instruction->two && (opcode & 0xF0) == 0x80);
    int transparent_edge = trace_state->flag_elision && !instruction->two &&
                           (opcode == 0xE9 || opcode == 0xEB || opcode == 0xE8);
    if (g_fl_pending && !conditional && !transparent_edge) {
        int lazy = lazyflags_on();
        if (lazy && insn_is_carry_consumer(instruction)) {
            // ADC/SBB consumes the live producer carry directly.
        } else if (lazy && insn_is_flagkill(instruction)) {
            g_fl_pending = FL_NONE;
        } else if (lazy && trace_state->flag_elision &&
                   !(hl_x86_trace_flags_livein(trace_state, guest_pc, guest_pc) & HL_X86_FLAG_NZCV)) {
            g_fl_pending = FL_NONE;
        } else {
            flags_materialize();
        }
    }

    g_pfaf_dead = 0;
    if (!pfaf_elim_on() || !insn_writes_pfaf(instruction)) return;
    struct insn following;
    if (hl_x86_decode(next, &following) < 0) memset(&following, 0, sizeof following);
    g_pfaf_dead = insn_kills_pfaf(&following);
    if (!g_pfaf_dead && trace_state->flag_elision && following.len > 0)
        g_pfaf_dead = hl_x86_trace_pfaf_dead(trace_state, &following, next, guest_pc);
    if (!g_pfaf_dead && trace_state->flag_elision)
        g_pfaf_dead =
            !(hl_x86_trace_flags_livein(trace_state, next, guest_pc) & (HL_X86_FLAG_PF | HL_X86_FLAG_AF));
}

// Tries the independent primary-opcode lowerers in their established order.
// TX_FALL preserves dispatch to the specialized handlers below; TX_NEXT and
// TX_BREAK are consumed by the translation loop without reinterpretation.
static int lower_primary_fast(struct insn *instruction, uint64_t guest_pc, uint64_t next,
                              const hl_x86_trace_state *trace_state) {
    const hl_x86_move_image image = {g_nonpie_lo, g_nonpie_hi, g_nonpie_bias};
    int result = hl_x86_lower_mov(instruction, next, &image);
    if (result != TX_FALL) return result;
    result = hl_x86_lower_alu(instruction, next);
    if (result != TX_FALL) return result;

    const hl_x86_shift_state shift_state = {
        .parity_aux_dead = g_pfaf_dead,
        .output_flags_dead = trace_state->flag_elision &&
                             !(hl_x86_trace_flags_livein(trace_state, next, guest_pc) &
                               (HL_X86_FLAG_ALL & ~HL_X86_FLAG_AF)),
        .direct_registers = 1,
    };
    return hl_x86_lower_shift(instruction, next, &shift_state);
}

static int lower_primary_string(struct insn *instruction, uint64_t next, hl_x86_crypto_state *crypto_state) {
    hl_x86_repstr_state state = {.direction = g_df, .optimize = 1};
    int result = hl_x86_lower_repstr(instruction, next, &state);
    g_df = state.direction;
    if (result == TX_NEXT) {
        // The ERMS funnel can clobber v16..v31, including hoisted constants.
        crypto_state->zero_ready = crypto_state->mask_ready = 0;
    }
    return result;
}

static int lower_group3_unary(struct insn *instruction, uint64_t next) {
    if (instruction->op != 0xF6 && instruction->op != 0xF7) return TX_FALL;
    int operation = instruction->reg & 7;
    int width = instruction->op == 0xF6 ? 1 : instruction->opsize;
    int memory;
    if (operation == 0) {
        int value = rm_load(instruction, next, width, &memory);
        e_movconst(19, (uint64_t)instruction->imm);
        do_alu(4, -1, value, 19, width);
        return TX_NEXT;
    }
    if (operation == 2) {
        int value = rm_load(instruction, next, width, &memory);
        emit32(0xAA2003E0u | (value << 16) | 16);
        rm_store(instruction, width, 16);
        return TX_NEXT;
    }
    if (operation != 3) return TX_FALL;

    int value = rm_load(instruction, next, width, &memory);
    if (width < 4) {
        do_alu(5, 16, 31, value, width);
    } else {
        e_rrr(A_SUBS, 16, 31, value, width == 8, 0);
        e_nzcv_save();
        e_pf_save(16);
        e_af_addsub(31, value, 16, 19);
    }
    rm_store(instruction, width, 16);
    return TX_NEXT;
}

static int lower_group3_narrow_muldiv(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    if (instruction->op != 0xF6 && instruction->op != 0xF7) return TX_FALL;
    int operation = instruction->reg & 7;
    int width = instruction->op == 0xF6 ? 1 : instruction->opsize;
    if (operation < 4 || width > 2) return TX_FALL;

    int memory;
    int value = rm_load(instruction, next, width, &memory);
    if (width == 1) {
        if (operation == 4 || operation == 5) {
            if (operation == 4) {
                e_uxt(19, RAX, 1);
                e_uxt(20, value, 1);
            } else {
                e_sxt(19, RAX, 1);
                e_sxt(20, value, 1);
            }
            e_mul(21, 19, 20, 0);
            e_mul_oc_narrow(21, operation, 1);
            e_bfi(RAX, 21, 0, 16, 1);
        } else {
            if (operation == 6) {
                e_uxt(19, RAX, 2);
                e_uxt(20, value, 1);
                emit_div_zero_check(20, guest_pc, 0);
                e_udiv(21, 19, 20, 0);
            } else {
                e_sxt(19, RAX, 2);
                e_sxt(20, value, 1);
                emit_div_zero_check(20, guest_pc, 1);
                e_sdiv(21, 19, 20, 0);
            }
            e_msub(22, 21, 20, 19, 0);
            emit_div_ovf_check(21, 23, 1, operation == 7, guest_pc, operation == 7);
            e_bfi(RAX, 21, 0, 8, 1);
            e_bfi(RAX, 22, 8, 8, 1);
        }
        return TX_NEXT;
    }

    if (operation == 4 || operation == 5) {
        if (operation == 4) {
            e_uxt(19, RAX, 2);
            e_uxt(20, value, 2);
        } else {
            e_sxt(19, RAX, 2);
            e_sxt(20, value, 2);
        }
        e_mul(21, 19, 20, 0);
        e_mul_oc_narrow(21, operation, 2);
        e_bfi(RAX, 21, 0, 16, 1);
        e_lsr_i(21, 21, 16, 0);
        e_bfi(RDX, 21, 0, 16, 1);
    } else {
        e_uxt(19, RAX, 2);
        e_bfi(19, RDX, 16, 16, 0);
        if (operation == 6) {
            e_uxt(20, value, 2);
            emit_div_zero_check(20, guest_pc, 0);
            e_udiv(21, 19, 20, 0);
        } else {
            e_sxt(20, value, 2);
            emit_div_zero_check(20, guest_pc, 1);
            e_sdiv(21, 19, 20, 0);
        }
        e_msub(22, 21, 20, 19, 0);
        emit_div_ovf_check(21, 23, 2, operation == 7, guest_pc, operation == 7);
        e_bfi(RAX, 21, 0, 16, 1);
        e_bfi(RDX, 22, 0, 16, 1);
    }
    return TX_NEXT;
}

static int lower_group3_wide_muldiv(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    if (instruction->op != 0xF7 || (instruction->opsize != 4 && instruction->opsize != 8)) return TX_FALL;
    int operation = instruction->reg & 7;
    if (operation < 4) return TX_FALL;
    int width = instruction->opsize;
    int memory;
    int value = rm_load(instruction, next, width, &memory);
    if (operation == 4 || operation == 5) {
        if (width == 4) {
            if (operation == 4) {
                e_uxt(20, RAX, 4);
                e_uxt(21, value, 4);
            } else {
                e_sxt(20, RAX, 4);
                e_sxt(21, value, 4);
            }
            e_mul(19, 20, 21, 1);
            e_lsr_i(RDX, 19, 32, 1);
            e_mov_rr(RAX, 19, 0);
            if (operation == 4) {
                e_lsr_i(22, 19, 32, 1);
                e_subi_s(23, 22, 0, 1);
            } else {
                e_sxt(22, 19, 4);
                e_rrr(A_SUBS, 23, 19, 22, 1, 0);
            }
        } else {
            e_mul(19, RAX, value, 1);
            if (operation == 4)
                e_umulh(RDX, RAX, value);
            else
                e_smulh(RDX, RAX, value);
            e_mov_rr(RAX, 19, 1);
            if (operation == 4) {
                e_mov_rr(22, RDX, 1);
                e_subi_s(23, 22, 0, 1);
            } else {
                e_asr_i(22, 19, 63, 1);
                e_rrr(A_SUBS, 23, RDX, 22, 1, 0);
            }
        }
        e_cset(21, 1 /*NE*/, 1);
        e_mul_set_oc(21);
        return TX_NEXT;
    }
    if (operation != 6 && operation != 7) {
        report_unimpl(guest_pc, instruction);
        return TX_BREAK;
    }
    if (width == 8) {
        emit_div64_fast(next, guest_pc, operation == 7, value);
        return TX_NEXT;
    }

    e_lsl_i(19, RDX, 32, 1);
    e_bfi(19, RAX, 0, 32, 1);
    if (operation == 6) {
        e_uxt(22, value, 4);
        emit_div_zero_check(22, guest_pc, 0);
        e_udiv(20, 19, 22, 1);
    } else {
        e_sxt(22, value, 4);
        emit_div_zero_check(22, guest_pc, 1);
        e_sdiv(20, 19, 22, 1);
    }
    e_msub(21, 20, 22, 19, 1);
    emit_div_ovf_check(20, 23, 4, operation == 7, guest_pc, operation == 7);
    e_mov_rr(RAX, 20, 0);
    e_mov_rr(RDX, 21, 0);
    return TX_NEXT;
}

static int lower_group45(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xFE && opcode != 0xFF) return TX_FALL;
    int operation = instruction->reg & 7;
    int width = opcode == 0xFE ? 1 : instruction->opsize;
    int wide = width == 8;
    int memory;
    if (operation == 0 || operation == 1) {
        uint32_t access = X86_SOFT_READ | X86_SOFT_WRITE;
        int value = rm_load_access(instruction, next, width, &memory, access);
        if (instruction->lock && memory) {
            e_movconst(19, operation == 0 ? 1 : (uint64_t)-1);
            e_lse(LSE_LDADD, width, 19, 20, 17);
            if (width >= 4) {
                if (operation == 0)
                    e_addi_s(21, 20, 1, wide);
                else
                    e_subi_s(21, 20, 1, wide);
                e_af_addsub(20, 21, 31, 19);
                e_nzcv_save_keepC();
                e_pf_save(21);
            } else {
                int shift = 8 * (4 - width);
                e_mov_rr(26, 20, 0);
                e_lsl_i(21, 20, shift, 0);
                e_movconst(19, 1u << shift);
                if (operation == 0)
                    e_rrr(A_ADDS, 21, 21, 19, 0, 0);
                else
                    e_rrr(A_SUBS, 21, 21, 19, 0, 0);
                e_nzcv_save_keepC();
                e_lsr_i(21, 21, shift, 0);
                e_pf_save(21);
                e_af_addsub(26, 21, 31, 19);
            }
            if (emit_soft_memory_active()) emit_soft_store_commit((uint64_t)width);
            return TX_NEXT;
        }
        int output = memory ? 16 : instruction->rm_reg;
        if (width >= 4) {
            e_mov_rr(26, value, wide);
            if (operation == 0)
                e_addi_s(output, value, 1, wide);
            else
                e_subi_s(output, value, 1, wide);
            e_nzcv_save_keepC();
            e_pf_save(output);
            e_af_addsub(26, output, 31, 19);
            rm_store_after_guard(instruction, width, output);
        } else {
            int shift = 8 * (4 - width);
            e_lsl_i(21, value, shift, 0);
            e_movconst(19, 1u << shift);
            if (operation == 0)
                e_rrr(A_ADDS, 21, 21, 19, 0, 0);
            else
                e_rrr(A_SUBS, 21, 21, 19, 0, 0);
            e_nzcv_save_keepC();
            e_lsr_i(21, 21, shift, 0);
            e_pf_save(21);
            e_af_addsub(value, 21, 31, 19);
            rm_store_after_guard(instruction, width, 21);
        }
        return TX_NEXT;
    }
    if (opcode == 0xFF && (operation == 4 || operation == 2)) {
        int target = rm_load(instruction, next, 8, &memory);
        if (target != 16) e_mov_rr(16, target, 1);
        e_movconst(19, guest_pc);
        e_str(19, 28, OFF_IBSRC);
        if (operation == 2) {
            e_subi(RSP, RSP, 8, 1);
            e_movconst(19, call_return_pc(next));
            e_store(8, 19, RSP);
        }
        emit_ibranch();
        return TX_BREAK;
    }
    if (opcode == 0xFF && operation == 6) {
        int value = rm_load(instruction, next, 8, &memory);
        if (value != 16) e_mov_rr(16, value, 1);
        e_subi(RSP, RSP, 8, 1);
        e_store(8, 16, RSP);
        return TX_NEXT;
    }
    return TX_FALL;
}

static int lower_exchange(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    if (instruction->op != 0x86 && instruction->op != 0x87) return TX_FALL;
    int width = (instruction->op & 1) ? instruction->opsize : 1;
    if (instruction->is_mem) {
        emit_ea(instruction, next);
        emit_memory_guard(17, (uint64_t)width, guest_pc, X86_SOFT_READ | X86_SOFT_WRITE);
        int source = width == 1 ? byte_val(instruction, instruction->reg, 19) : instruction->reg;
        // A memory XCHG is implicitly atomic even without a LOCK prefix.
        e_lse(LSE_SWP, width, source, 16, 17);
        if (width >= 4)
            e_mov_rr(instruction->reg, 16, width == 8);
        else if (width == 1)
            byte_wb(instruction, instruction->reg, 16);
        else
            e_bfi(instruction->reg, 16, 0, 8 * width, 1);
        if (emit_soft_memory_active()) emit_soft_store_commit((uint64_t)width);
        return TX_NEXT;
    }
    if (width == 1) {
        // Materialize both byte lanes before either write; they may alias.
        int left = byte_val(instruction, instruction->reg, 16);
        int right = byte_val(instruction, instruction->rm_reg, 17);
        e_mov_rr(19, left, 0);
        e_mov_rr(23, right, 0);
        byte_wb(instruction, instruction->reg, 23);
        byte_wb(instruction, instruction->rm_reg, 19);
    } else if (width == 2) {
        e_mov_rr(19, instruction->rm_reg, 1);
        e_bfi(instruction->rm_reg, instruction->reg, 0, 16, 1);
        e_bfi(instruction->reg, 19, 0, 16, 1);
    } else {
        int wide = width == 8;
        e_mov_rr(19, instruction->rm_reg, wide);
        e_mov_rr(instruction->rm_reg, instruction->reg, wide);
        e_mov_rr(instruction->reg, 19, wide);
    }
    return TX_NEXT;
}

static int lower_stack_control(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    if (instruction->op == 0x68 || instruction->op == 0x6A) {
        e_subi(RSP, RSP, 8, 1);
        e_movconst(16, (uint64_t)instruction->imm);
        e_store(8, 16, RSP);
        return TX_NEXT;
    }
    if (instruction->op == 0x8F) {
        if (instruction->is_mem) {
            // The destination address observes RSP after the pop.
            e_load(8, 19, RSP);
            e_addi(RSP, RSP, 8, 1);
            emit_ea(instruction, next);
            e_store(8, 19, 17);
        } else {
            e_load(8, 16, RSP);
            e_addi(RSP, RSP, 8, 1);
            e_mov_rr(instruction->rm_reg, 16, 1);
        }
        return TX_NEXT;
    }
    if (instruction->op == 0xC3 || instruction->op == 0xC2) {
        if (emit_soft_memory_active()) {
            e_mov_rr(17, RSP, 1);
            emit_memory_guard(17, 8, guest_pc, X86_SOFT_READ);
        }
        e_load(8, 16, emit_soft_memory_active() ? 17 : RSP);
        e_addi(RSP, RSP, 8, 1);
        if (instruction->op == 0xC2) {
            e_movconst(19, (uint64_t)(uint16_t)instruction->imm);
            e_rrr(A_ADD, RSP, RSP, 19, 1, 0);
        }
        e_movconst(19, guest_pc);
        e_str(19, 28, OFF_IBSRC);
        emit_ibranch();
        return TX_BREAK;
    }
    if (instruction->op != 0xC9) return TX_FALL;
    if (emit_soft_memory_active()) {
        e_mov_rr(17, RBP, 1);
        emit_memory_guard(17, 8, guest_pc, X86_SOFT_READ);
    }
    e_mov_rr(RSP, RBP, 1);
    e_load(8, RBP, emit_soft_memory_active() ? 17 : RSP);
    e_addi(RSP, RSP, 8, 1);
    return TX_NEXT;
}

static int lower_immediate_multiply(struct insn *instruction, uint64_t guest_pc, uint64_t next,
                                    const hl_x86_trace_state *trace_state) {
    if (instruction->op != 0x69 && instruction->op != 0x6B) return TX_FALL;
    int memory;
    int source = rm_load(instruction, next, instruction->opsize, &memory);
    e_movconst(19, (uint64_t)instruction->imm);
    int overflow_live = !trace_state->flag_elision ||
                        (hl_x86_trace_flags_livein(trace_state, next, guest_pc) & HL_X86_FLAG_NZCV);
    e_imul2(instruction->reg, source, 19, instruction->opsize, overflow_live);
    return TX_NEXT;
}

static int lower_direct_call_loop(struct insn *instruction, uint64_t guest_pc, uint64_t next,
                                  const hl_x86_trace_state *trace_state) {
    uint8_t opcode = instruction->op;
    uint64_t taken = next + (uint64_t)instruction->imm;
    if (opcode == 0xE8) {
        if (emit_soft_memory_active()) {
            e_subi(17, RSP, 8, 1);
            emit_memory_guard(17, 8, guest_pc, X86_SOFT_WRITE);
        }
        e_subi(RSP, RSP, 8, 1);
        e_movconst(16, call_return_pc(next));
        e_store(8, 16, emit_soft_memory_active() ? 17 : RSP);
        hl_x86_trace_flags_edge(trace_state, taken, guest_pc);
        emit_chain_exit(taken);
        return TX_BREAK;
    }
    if (opcode == 0xE3) {
        uint32_t cbz = instruction->addr32 ? 0x34000000u : 0xB4000000u;
        uint32_t *patch = (uint32_t *)g_cp;
        emit32(0);
        emit_chain_exit(next);
        int64_t distance = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
        *patch = cbz | (((uint32_t)distance & 0x7FFFF) << 5) | RCX;
        emit_chain_exit(taken);
        return TX_BREAK;
    }
    if (opcode != 0xE0 && opcode != 0xE1 && opcode != 0xE2) return TX_FALL;

    int wide = instruction->addr32 ? 0 : 1;
    uint32_t cbz = instruction->addr32 ? 0x34000000u : 0xB4000000u;
    uint32_t cbnz = instruction->addr32 ? 0x35000000u : 0xB5000000u;
    e_subi(RCX, RCX, 1, wide);
    if (opcode == 0xE2) {
        uint32_t *patch = (uint32_t *)g_cp;
        emit32(0);
        emit_chain_exit(next);
        int64_t distance = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
        *patch = cbnz | (((uint32_t)distance & 0x7FFFF) << 5) | RCX;
        emit_chain_exit(taken);
        return TX_BREAK;
    }

    e_nzcv_load();
    int fail_condition = opcode == 0xE1 ? 1 : 0;
    uint32_t *counter_patch = (uint32_t *)g_cp;
    emit32(0);
    uint32_t *flag_patch = (uint32_t *)g_cp;
    emit32(0);
    emit_chain_exit(taken);
    int64_t counter_distance = ((uint8_t *)g_cp - (uint8_t *)counter_patch) / 4;
    *counter_patch = cbz | (((uint32_t)counter_distance & 0x7FFFF) << 5) | RCX;
    int64_t flag_distance = ((uint8_t *)g_cp - (uint8_t *)flag_patch) / 4;
    *flag_patch = 0x54000000u | (((uint32_t)flag_distance & 0x7FFFF) << 5) | (uint32_t)fail_condition;
    emit_chain_exit(next);
    return TX_BREAK;
}

static int lower_flag_register_transfer(struct insn *instruction) {
    uint8_t opcode = instruction->op;
    if (opcode == 0x9E) {
        emit32(0x53083C00u | (RAX << 5) | 16); // AH
        emit32(0x53000000u | (16 << 5) | 17);  // CF
        e_movconst(20, 1);
        e_rrr(A_EOR, 17, 17, 20, 0, 0); // stored borrow-C = !CF
        e_lsl_i(17, 17, 29, 0);
        emit32(0x53061800u | (16 << 5) | 18); // ZF
        e_lsl_i(18, 18, 30, 0);
        e_rrr(A_ORR, 17, 17, 18, 0, 0);
        emit32(0x53071C00u | (16 << 5) | 18); // SF
        e_lsl_i(18, 18, 31, 0);
        e_rrr(A_ORR, 17, 17, 18, 0, 0);
        e_str(17, 28, OFF_NZCV);
        emit32(0xD51B4200u | 17);
        emit32(0x53020800u | (16 << 5) | 19); // PF
        e_rrr(A_EOR, 19, 19, 20, 0, 0);
        e_str(19, 28, OFF_PF);
        e_af_save(16);
        g_fl_pending = FL_NONE;
        return TX_NEXT;
    }
    if (opcode == 0x9F) {
        if (g_fl_pending) flags_materialize();
        e_ldr(16, 28, OFF_NZCV);
        emit32(0x53000000u | (31 << 16) | (31 << 10) | (16 << 5) | 17);
        e_lsl_i(17, 17, 7, 0);
        emit32(0x53000000u | (30 << 16) | (30 << 10) | (16 << 5) | 18);
        e_lsl_i(18, 18, 6, 0);
        e_rrr(A_ORR, 17, 17, 18, 0, 0);
        emit32(0x53000000u | (29 << 16) | (29 << 10) | (16 << 5) | 18);
        e_movconst(19, 1);
        e_rrr(A_EOR, 18, 18, 19, 0, 0);
        e_rrr(A_ORR, 17, 17, 18, 0, 0);
        e_movconst(18, 2);
        e_rrr(A_ORR, 17, 17, 18, 0, 0);
        e_pf_compute(18);
        e_rrr(A_ORR, 17, 17, 18, 0, 2);
        e_ldr(18, 28, OFF_AF);
        emit32(0x53000000u | (4 << 16) | (4 << 10) | (18 << 5) | 18);
        e_rrr(A_ORR, 17, 17, 18, 0, 4);
        e_bfi(RAX, 17, 8, 8, 1);
        return TX_NEXT;
    }
    if (opcode != 0xF8 && opcode != 0xF9 && opcode != 0xF5) return TX_FALL;
    e_nzcv_setcf_op(opcode == 0xF8 ? A_ORR : opcode == 0xF9 ? A_BIC : A_EOR);
    return TX_NEXT;
}

static int lower_flag_stack_control(struct insn *instruction, uint64_t guest_pc) {
    uint8_t opcode = instruction->op;
    if (opcode == 0x9C) {
        if (g_fl_pending) flags_materialize();
        e_ldr(16, 28, OFF_NZCV);
        e_movconst(17, 0x202u);
        e_ldr(18, 28, OFF_DF);
        e_rrr(A_ORR, 17, 17, 18, 0, 10);
        emit32(0x53000000u | (29 << 16) | (29 << 10) | (16 << 5) | 18);
        e_movconst(19, 1);
        e_rrr(A_EOR, 18, 18, 19, 0, 0);
        e_rrr(A_ORR, 17, 17, 18, 0, 0);
        e_bit_move(17, 16, 30, 6, 18);
        e_bit_move(17, 16, 31, 7, 18);
        e_bit_move(17, 16, 28, 11, 18);
        e_pf_compute(18);
        e_rrr(A_ORR, 17, 17, 18, 0, 2);
        e_ldr(18, 28, OFF_AF);
        emit32(0x53000000u | (4 << 16) | (4 << 10) | (18 << 5) | 18);
        e_rrr(A_ORR, 17, 17, 18, 0, 4);
        e_ldr(18, 28, OFF_ID);
        e_rrr(A_ORR, 17, 17, 18, 0, 21);
        if (emit_soft_memory_active()) {
            e_mov_rr(20, 17, 1);
            e_subi(17, RSP, 8, 1);
            emit_memory_guard(17, 8, guest_pc, X86_SOFT_WRITE);
            e_subi(RSP, RSP, 8, 1);
            e_store(8, 20, 17);
        } else {
            e_subi(RSP, RSP, 8, 1);
            e_store(8, 17, RSP);
        }
        return TX_NEXT;
    }
    if (opcode == 0x9D) {
        if (emit_soft_memory_active()) {
            e_mov_rr(17, RSP, 1);
            emit_memory_guard(17, 8, guest_pc, X86_SOFT_READ);
        }
        e_load(8, 16, emit_soft_memory_active() ? 17 : RSP);
        e_addi(RSP, RSP, 8, 1);
        emit_restore_rflags(16);
        return TX_NEXT;
    }
    if (opcode != 0xCF || !instruction->rexW) return TX_FALL;
    int frame = RSP;
    if (emit_soft_memory_active()) {
        e_mov_rr(17, RSP, 1);
        emit_memory_guard(17, 40, guest_pc, X86_SOFT_READ);
        frame = 17;
    }
    e_ldr(21, frame, 0);
    e_ldr(16, frame, 16);
    e_ldr(22, frame, 24);
    emit_restore_rflags(16);
    e_mov_rr(RSP, 22, 1);
    e_mov_rr(16, 21, 1);
    e_movconst(19, guest_pc);
    e_str(19, 28, OFF_IBSRC);
    emit_ibranch();
    return TX_BREAK;
}

static int lower_accumulator_legacy(struct insn *instruction, int sf) {
    uint8_t opcode = instruction->op;
    if (opcode == 0x90) {
        // 90 is XCHG eAX,rN and is only a NOP when N is rAX. REX.B selects r8.
        if (!instruction->rep && instruction->rexB) {
            int reg = instruction->rexB << 3;
            e_mov_rr(19, RAX, sf);
            e_mov_rr(RAX, reg, sf);
            e_mov_rr(reg, 19, sf);
        }
        return TX_NEXT;
    }
    if (opcode == 0x9B) return TX_NEXT; // fwait/wait: host FPU operations are synchronous
    if (opcode >= 0x91 && opcode <= 0x97) {
        int reg = (opcode - 0x90) | (instruction->rexB << 3);
        if (instruction->opsize == 2) {
            e_mov_rr(19, reg, 1);
            e_bfi(reg, RAX, 0, 16, 1);
            e_bfi(RAX, 19, 0, 16, 1);
        } else {
            e_mov_rr(19, RAX, sf);
            e_mov_rr(RAX, reg, sf);
            e_mov_rr(reg, 19, sf);
        }
        return TX_NEXT;
    }
    if (opcode == 0x98) {
        if (sf) {
            e_sxt(RAX, RAX, 4);
        } else if (instruction->p66) {
            emit32(0x13001C00u | (RAX << 5) | 16);
            e_bfi(RAX, 16, 0, 16, 1);
        } else {
            emit32(0x13003C00u | (RAX << 5) | RAX);
        }
        return TX_NEXT;
    }
    if (opcode != 0x99) return TX_FALL;
    if (sf) {
        e_asr_i(RDX, RAX, 63, 1);
    } else if (instruction->p66) {
        e_sxt(19, RAX, 2);
        e_asr_i(19, 19, 15, 0);
        e_bfi(RDX, 19, 0, 16, 1);
    } else {
        e_asr_i(RDX, RAX, 31, 0);
    }
    return TX_NEXT;
}

static int lower_bit_scan(struct insn *instruction, uint64_t next, int sf) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xBC && opcode != 0xBD) return TX_FALL;
    int mem;
    int rmv = rm_load(instruction, next, instruction->opsize, &mem);
    int count_form = instruction->rep;
    int source = rmv;
    if (!mem && instruction->reg == rmv) {
        e_mov_rr(23, rmv, sf);
        source = 23;
    }
    int word_form = instruction->opsize == 2;
    if (word_form) {
        e_movconst(19, 0xffff);
        e_rrr(A_AND, 23, source, 19, 0, 0);
        source = 23;
    }
    int destination = word_form ? 21 : instruction->reg;
    if (word_form) e_mov_rr(21, instruction->reg, 0);
    int bit_destination = count_form ? destination : 22;
    if (opcode == 0xBC) {
        e_rbit(bit_destination, source, sf);
        e_clz(bit_destination, bit_destination, sf);
    } else if (count_form) {
        e_clz(destination, source, sf);
    } else {
        e_clz(20, source, sf);
        e_movconst(19, sf ? 63 : 31);
        e_rrr(A_SUB, 22, 19, 20, sf, 0);
    }
    if (count_form) {
        e_rrr(A_SUBS, 31, source, 31, sf, 0);
        e_cset(19, 0, sf);
        e_rrr(A_ANDS, 31, destination, destination, sf, 0);
        e_nzcv_save_setcf(19);
    } else {
        e_rrr(A_ANDS, 31, source, source, sf, 0);
        e_csel(destination, destination, 22, 0, sf);
        e_nzcv_save();
    }
    if (word_form) e_bfi(instruction->reg, destination, 0, 16, 1);
    return TX_NEXT;
}

static int lower_population_count(struct insn *instruction, uint64_t next, int sf) {
    if (instruction->op != 0xB8 || !instruction->rep) return TX_FALL;
    int mem;
    int rmv = rm_load(instruction, next, instruction->opsize, &mem);
    int source = rmv;
    if (!mem && instruction->reg == rmv) {
        e_mov_rr(23, rmv, sf);
        source = 23;
    }
    int word_form = instruction->opsize == 2;
    if (word_form) {
        e_movconst(19, 0xffff);
        e_rrr(A_AND, 23, source, 19, 0, 0);
        source = 23;
    }
    if (sf)
        e_fmov_to_d(16, source);
    else
        e_fmov_to_s(16, source);
    emit32(0x0E205800u | (16 << 5) | 16);
    emit32(0x0E31B800u | (16 << 5) | 16);
    e_fmov_from_s(word_form ? 21 : instruction->reg, 16);
    if (word_form) e_bfi(instruction->reg, 21, 0, 16, 1);
    e_rrr(A_ANDS, 31, source, source, sf, 0);
    e_nzcv_save_popcnt();
    return TX_NEXT;
}

static int lower_compare_exchange(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xB0 && opcode != 0xB1) return TX_FALL;
    int width = opcode == 0xB0 ? 1 : instruction->opsize;
    int sf = width == 8;
    if (instruction->is_mem) {
        emit_ea(instruction, next);
        emit_memory_guard(17, (uint64_t)width, guest_pc, X86_SOFT_READ | X86_SOFT_WRITE);
        e_mov_rr(19, RAX, sf);
        e_cas(width, 19, instruction->reg, 17);
        do_alu(7, -1, RAX, 19, width);
        if (width >= 4)
            e_mov_rr(RAX, 19, sf);
        else
            e_bfi(RAX, 19, 0, 8 * width, 1);
        if (emit_soft_memory_active()) emit_soft_store_commit((uint64_t)width);
        return TX_NEXT;
    }
    if (width >= 4) {
        e_mov_rr(19, instruction->rm_reg, sf);
        do_alu(7, -1, RAX, 19, width);
        e_csel(instruction->rm_reg, instruction->reg, 19, 0, sf);
        e_csel(RAX, RAX, 19, 0, sf);
        return TX_NEXT;
    }
    int old_value = width == 1 ? byte_val(instruction, instruction->rm_reg, 19) : instruction->rm_reg;
    int source_value = width == 1 ? byte_val(instruction, instruction->reg, 24) : instruction->reg;
    if (old_value != 19) e_mov_rr(19, old_value, 1);
    do_alu(7, -1, RAX, 19, width);
    e_csel(21, source_value, 19, 0, 0);
    e_csel(22, RAX, 19, 0, 0);
    if (width == 1) {
        byte_wb(instruction, instruction->rm_reg, 21);
        e_bfi(RAX, 22, 0, 8, 1);
    } else {
        e_bfi(instruction->rm_reg, 21, 0, 16, 1);
        e_bfi(RAX, 22, 0, 16, 1);
    }
    return TX_NEXT;
}

static int lower_exchange_add(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xC0 && opcode != 0xC1) return TX_FALL;
    int width = opcode == 0xC0 ? 1 : instruction->opsize;
    int sf = width == 8;
    if (instruction->is_mem) {
        emit_ea(instruction, next);
        emit_memory_guard(17, (uint64_t)width, guest_pc, X86_SOFT_READ | X86_SOFT_WRITE);
        e_lse(LSE_LDADD, width, instruction->reg, 19, 17);
        do_alu(0, -1, 19, instruction->reg, width);
        if (width >= 4)
            e_mov_rr(instruction->reg, 19, sf);
        else
            e_bfi(instruction->reg, 19, 0, 8 * width, 1);
        if (emit_soft_memory_active()) emit_soft_store_commit((uint64_t)width);
        return TX_NEXT;
    }
    if (width >= 4) {
        e_mov_rr(19, instruction->rm_reg, sf);
        e_rrr(A_ADDS, instruction->rm_reg, instruction->rm_reg, instruction->reg, sf, 0);
        e_nzcv_save_ci();
        e_mov_rr(instruction->reg, 19, sf);
        return TX_NEXT;
    }
    int old_value = width == 1 ? byte_val(instruction, instruction->rm_reg, 19) : instruction->rm_reg;
    int addend = width == 1 ? byte_val(instruction, instruction->reg, 24) : instruction->reg;
    if (old_value != 19) e_mov_rr(19, old_value, 1);
    if (addend != 24) e_mov_rr(24, addend, 1);
    do_alu(0, -1, 19, 24, width);
    e_rrr(A_ADD, 26, 19, 24, 0, 0);
    if (width == 1) {
        byte_wb(instruction, instruction->reg, 19);
        byte_wb(instruction, instruction->rm_reg, 26);
    } else {
        e_bfi(instruction->reg, 19, 0, 16, 1);
        e_bfi(instruction->rm_reg, 26, 0, 16, 1);
    }
    return TX_NEXT;
}

static int lower_wide_compare_exchange(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    if (instruction->op != 0xC7 || (instruction->reg & 7) != 1 || !instruction->is_mem) return TX_FALL;
    if (g_fl_pending) flags_materialize();
    emit_ea(instruction, next);
    if (instruction->opsize == 8) {
        emit_memory_guard(17, 16, guest_pc, X86_SOFT_READ | X86_SOFT_WRITE);
        if (emit_soft_memory_active()) {
            emit_soft_store_commit(16);
            e_ldr(17, 28, OFF_BUS_EA);
        }
        e_str(17, 28, OFF_X87EA);
        emit_exit_const(next, R_CMPXCHG16);
        return TX_BREAK;
    }
    emit_memory_guard(17, 8, guest_pc, X86_SOFT_READ | X86_SOFT_WRITE);
    e_uxt(19, RAX, 4);
    e_bfi(19, RDX, 32, 32, 1);
    e_uxt(20, RBX, 4);
    e_bfi(20, RCX, 32, 32, 1);
    e_mov_rr(22, 19, 1);
    e_cas(8, 19, 20, 17);
    e_uxt(24, 19, 4);
    e_lsr_i(25, 19, 32, 1);
    e_rrr(A_SUBS, 31, 19, 22, 1, 0);
    e_csel(RAX, RAX, 24, 0, 1);
    e_csel(RDX, RDX, 25, 0, 1);
    e_ldr(21, 28, OFF_NZCV);
    e_movconst(23, 0x40000000u);
    e_rrr(A_BIC, 21, 21, 23, 1, 0);
    e_cset(23, 0, 1);
    e_lsl_i(23, 23, 30, 1);
    e_rrr(A_ORR, 21, 21, 23, 1, 0);
    e_str(21, 28, OFF_NZCV);
    emit32(0xD51B4200u | 21);
    if (emit_soft_memory_active()) emit_soft_store_commit(8);
    return TX_NEXT;
}

static int lower_system_query(struct insn *instruction, uint64_t next) {
    uint8_t opcode = instruction->op;
    if (opcode == 0xA2) {
        emit_exit_const(next, R_CPUID);
        return TX_BREAK;
    }
    if (opcode == 0x31) {
        emit32(0xD53BE040u | 16);
        e_mov_rr(RAX, 16, 0);
        e_lsr_i(RDX, 16, 32, 1);
        return TX_NEXT;
    }
    if (opcode != 0x01 || !instruction->has_modrm) return TX_FALL;
    if (instruction->modrm == 0xF9) {
        emit32(0xD53BE040u | 16);
        e_mov_rr(RAX, 16, 0);
        e_lsr_i(RDX, 16, 32, 1);
        e_movz(RCX, 0, 0);
        return TX_NEXT;
    }
    if (instruction->modrm == 0xD0) {
        e_movz(RAX, 3, 0);
        e_movz(RDX, 0, 0);
        return TX_NEXT;
    }
    if (instruction->modrm == 0xD5) return TX_NEXT;
    return TX_FALL;
}

static int lower_bit_test_modify(struct insn *instruction, uint64_t guest_pc, uint64_t next, int sf) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xA3 && opcode != 0xAB && opcode != 0xB3 && opcode != 0xBB && opcode != 0xBA)
        return TX_FALL;
    int immediate = opcode == 0xBA;
    int operation = immediate ? (instruction->reg & 7)
                              : (opcode == 0xA3 ? 4 : opcode == 0xAB ? 5 : opcode == 0xB3 ? 6 : 7);
    if (operation < 4) {
        report_unimpl(guest_pc, instruction);
        return TX_BREAK;
    }
    int width = instruction->opsize;
    int memory;
    int bits = width * 8;
    int log_bits = width == 8 ? 6 : width == 4 ? 5 : 4;
    int log_width = width == 8 ? 3 : width == 4 ? 2 : 1;
    int value;
    uint32_t access = operation == 4 ? X86_SOFT_READ : X86_SOFT_READ | X86_SOFT_WRITE;
    if (instruction->is_mem && !immediate) {
        emit_ea(instruction, next);
        if (width == 8)
            e_mov_rr(20, instruction->reg, 1);
        else
            e_sxt(20, instruction->reg, width);
        e_asr_i(20, 20, log_bits, 1);
        e_rrr(A_ADD, 17, 17, 20, 1, log_width);
        emit_memory_guard(17, (uint64_t)width, guest_pc, access);
        e_load(width, 16, 17);
        value = 16;
        memory = 1;
    } else {
        value = rm_load_access(instruction, next, width, &memory, access);
    }
    if (immediate) {
        e_movconst(19, (uint64_t)instruction->imm & (uint64_t)(bits - 1));
    } else {
        e_movconst(21, bits - 1);
        e_rrr(A_AND, 19, instruction->reg, 21, sf, 0);
    }
    e_shv(S_LSRV, 21, value, 19, sf);
    e_movconst(22, 1);
    e_rrr(A_AND, 21, 21, 22, sf, 0);
    e_rrr(A_SUBS, 31, 31, 21, 1, 0);
    e_nzcv_save();
    if (operation == 4) return TX_NEXT;
    e_movconst(22, 1);
    e_shv(S_LSLV, 22, 22, 19, sf);
    if (memory && instruction->lock) {
        uint32_t lse = operation == 5 ? LSE_LDSET : operation == 6 ? LSE_LDCLR : LSE_LDEOR;
        e_lse(lse, width, 22, 23, 17);
        e_shv(S_LSRV, 24, 23, 19, sf);
        e_movconst(25, 1);
        e_rrr(A_AND, 24, 24, 25, sf, 0);
        e_rrr(A_SUBS, 31, 31, 24, 1, 0);
        e_nzcv_save();
        if (emit_soft_memory_active()) emit_soft_store_commit((uint64_t)width);
        return TX_NEXT;
    }
    int output = memory || width < 4 ? 16 : instruction->rm_reg;
    if (operation == 5)
        e_rrr(A_ORR, output, value, 22, sf, 0);
    else if (operation == 6)
        e_rrr(A_BIC, output, value, 22, sf, 0);
    else
        e_rrr(A_EOR, output, value, 22, sf, 0);
    rm_store_after_guard(instruction, width, output);
    return TX_NEXT;
}

static int lower_extended_state(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    if (instruction->op == 0x77) return TX_NEXT;
    if (instruction->op != 0xAE) return TX_FALL;
    int operation = instruction->reg & 7;
    if (operation >= 5) {
        emit32(0xD5033BBFu);
        return TX_NEXT;
    }
    if (operation == 2) {
        emit_ldmxcsr(instruction, next);
        return TX_NEXT;
    }
    if (operation == 3) {
        emit_stmxcsr(instruction, next);
        return TX_NEXT;
    }
    if ((operation != 0 && operation != 1) || !instruction->is_mem) return TX_FALL;
    emit_ea(instruction, next);
    emit_memory_guard(17, 512, guest_pc, operation == 0 ? X86_SOFT_WRITE : X86_SOFT_READ);
    if (operation == 0 && emit_soft_memory_active()) {
        emit_soft_store_commit(512);
        e_ldr(17, 28, OFF_BUS_EA);
    }
    e_str(17, 28, OFF_X87EA);
    if (g_fl_pending) flags_materialize();
    if (hl_x86_x87_known()) hl_x86_x87_drop();
    emit_exit_const(next, operation == 0 ? R_FXSAVE : R_FXRSTOR);
    return TX_BREAK;
}

static int lower_multibyte_hint(const struct insn *instruction) {
    uint8_t opcode = instruction->op;
    if (opcode == 0x1E && instruction->imm_bytes == 0) return TX_NEXT;
    if (opcode == 0x1F || opcode == 0x18 || opcode == 0x0D || (opcode >= 0x19 && opcode <= 0x1D))
        return TX_NEXT;
    return TX_FALL;
}

// Lowers the two-byte SSE/MMX move family. Keeping these forms together makes
// their operand width and register-file rules explicit, especially where bare
// encodings name MMX while mandatory prefixes name XMM registers.
static int lower_sse_moves(struct insn *instruction, uint64_t guest_pc, uint64_t next, int vd, int vm,
                           int mmx) {
    uint8_t opcode = instruction->op;
    if (opcode == 0x6E) { // movd/movq xmm, r/m (bare form names MMX)
        if (instruction->is_mem) {
            emit_ea(instruction, next);
            emit_memory_guard(17, instruction->rexW ? 8u : 4u, guest_pc, X86_SOFT_READ);
            if (instruction->rexW)
                g_ldr_d(vd, 17);
            else
                g_ldr_s(vd, 17);
        } else if (instruction->rexW) {
            e_fmov_to_d(vd, instruction->rm_reg);
        } else {
            e_fmov_to_s(vd, instruction->rm_reg);
        }
        return TX_NEXT;
    }
    if (opcode == 0x7E && instruction->rep) { // F3 0F 7E: movq xmm, xmm/m64
        if (instruction->is_mem) {
            emit_ea(instruction, next);
            emit_memory_guard(17, 8, guest_pc, X86_SOFT_READ);
            g_ldr_d(vd, 17);
        } else {
            e_vmov8(vd, vm);
        }
        return TX_NEXT;
    }
    if (opcode == 0x7E) { // movd/movq r/m, xmm (bare form names MMX)
        if (instruction->is_mem) {
            emit_ea(instruction, next);
            emit_memory_guard(17, instruction->rexW ? 8u : 4u, guest_pc, X86_SOFT_WRITE);
            if (instruction->rexW)
                g_str_d(vd, 17);
            else
                g_str_s(vd, 17);
            if (emit_soft_memory_active()) emit_soft_store_commit(instruction->rexW ? 8u : 4u);
        } else if (instruction->rexW) {
            e_fmov_from_d(instruction->rm_reg, vd);
        } else {
            e_fmov_from_s(instruction->rm_reg, vd);
        }
        return TX_NEXT;
    }
    if (opcode == 0xD6) {
        // Mandatory prefixes select the register file: 66=MOVQ, F3=MOVQ2DQ,
        // F2=MOVDQ2Q. Bare 0F D6 and memory operands for F3/F2 are invalid.
        if ((!instruction->p66 && !instruction->rep && !instruction->repne) ||
            (instruction->is_mem && !instruction->p66)) {
            emit_sigill(guest_pc);
            return TX_BREAK;
        }
        if (instruction->rep)
            e_vmov8(vd, vm & 7);
        else if (instruction->repne)
            e_vmov8(vd & 7, vm);
        else if (instruction->is_mem) {
            emit_ea(instruction, next);
            emit_memory_guard(17, 8, guest_pc, X86_SOFT_WRITE);
            g_str_d(vd, 17);
            if (emit_soft_memory_active()) emit_soft_store_commit(8);
        } else {
            e_vmov8(vm, vd);
        }
        return TX_NEXT;
    }
    if (opcode == 0x6F && !instruction->p66 && !instruction->rep && !instruction->repne) {
        // Bare 0F 6F is the 64-bit MMX form, not a 128-bit XMM load.
        if (instruction->is_mem) {
            emit_ea(instruction, next);
            emit_memory_guard(17, 8, guest_pc, X86_SOFT_READ);
            g_ldr_d(vd, 17);
        } else {
            e_vmov8(vd, vm);
        }
        return TX_NEXT;
    }
    if (opcode == 0x7F && !instruction->p66 && !instruction->rep && !instruction->repne) {
        if (instruction->is_mem) {
            emit_ea(instruction, next);
            emit_memory_guard(17, 8, guest_pc, X86_SOFT_WRITE);
            g_str_d(vd, 17);
            if (emit_soft_memory_active()) emit_soft_store_commit(8);
        } else {
            e_vmov8(vm, vd);
        }
        return TX_NEXT;
    }
    if (opcode == 0xF0 && instruction->repne && instruction->is_mem) {
        g_ldr_q_ea(vd, instruction, next); // LDDQU: architectural result is an unaligned load.
        return TX_NEXT;
    }
    if (opcode == 0x6F || opcode == 0x28 ||
        (opcode == 0x10 && !instruction->rep && !instruction->repne)) {
        if (instruction->is_mem)
            g_ldr_q_ea(vd, instruction, next);
        else
            e_vmov(vd, vm);
        return TX_NEXT;
    }
    if (opcode == 0x7F || opcode == 0x29 ||
        (opcode == 0x11 && !instruction->rep && !instruction->repne)) {
        if (instruction->is_mem)
            g_str_q_ea(vd, instruction, next);
        else
            e_vmov(vm, vd);
        return TX_NEXT;
    }
    if ((opcode == 0x10 || opcode == 0x11) && instruction->rep) {
        int store = opcode == 0x11;
        if (instruction->is_mem) {
            emit_ea(instruction, next);
            emit_memory_guard(17, 4, guest_pc, store ? X86_SOFT_WRITE : X86_SOFT_READ);
            if (store) {
                g_str_s(vd, 17);
                if (emit_soft_memory_active()) emit_soft_store_commit(4);
            } else {
                g_ldr_s(vd, 17);
            }
        } else {
            emit32(0x6E040400u | ((store ? vd : vm) << 5) | (store ? vm : vd));
        }
        return TX_NEXT;
    }
    if ((opcode == 0x10 || opcode == 0x11) && instruction->repne) {
        int store = opcode == 0x11;
        if (instruction->is_mem) {
            emit_ea(instruction, next);
            emit_memory_guard(17, 8, guest_pc, store ? X86_SOFT_WRITE : X86_SOFT_READ);
            if (store) {
                g_str_d(vd, 17);
                if (emit_soft_memory_active()) emit_soft_store_commit(8);
            } else {
                g_ldr_d(vd, 17);
            }
        } else {
            emit32(0x6E080400u | ((store ? vd : vm) << 5) | (store ? vm : vd));
        }
        return TX_NEXT;
    }
    (void)mmx;
    return TX_FALL;
}

// Packed horizontal and alternating arithmetic shares NaN handling but not the
// lane ordering of ordinary vertical SSE arithmetic.
static int lower_sse_horizontal(struct insn *instruction, uint64_t guest_pc, uint64_t next, int vd, int vm) {
    uint8_t opcode = instruction->op;
    if (opcode != 0x7C && opcode != 0x7D && opcode != 0xD0) return TX_FALL;
    int source = vm;
    if (instruction->is_mem) {
        g_ldr_q_ea(16, instruction, next);
        source = 16;
    }
    int double_precision = instruction->p66 != 0;
    emit_nan_input_gate(vd, source, double_precision, guest_pc);
    int fix_nan = fpdnan_on();
    if (opcode == 0xD0) {
        // Compute each operation before selecting even/odd lanes. Flipping the
        // source sign before FADD would also flip an input NaN's sign.
        if (fix_nan) emit_dnan_pre(vd, source, 1, double_precision);
        if (double_precision) {
            e_v3(0x4EE0D400u, 17, vd, source);
            e_v3(0x4E60D400u, 18, vd, source);
            e_movconst(19, ~0ULL);
            emit32(0x9E670000u | (19 << 5) | 19);
        } else {
            e_v3(0x4EA0D400u, 17, vd, source);
            e_v3(0x4E20D400u, 18, vd, source);
            e_movconst(19, 0x00000000FFFFFFFFULL);
            emit32(0x4E080C00u | (19 << 5) | 19);
        }
        e_v3(0x6E601C00u, 19, 17, 18);
        e_vmov(vd, 19);
        if (fix_nan) emit_dnan_post(vd, double_precision, 1);
        return TX_NEXT;
    }

    uint32_t size = instruction->p66 ? 0x00400000u : 0;
    if (fix_nan) {
        uint32_t equal = double_precision ? 0x4E60E400u : 0x4E20E400u;
        unsigned sign_shift = double_precision ? 127u : 63u;
        emit32(equal | (vd << 16) | (vd << 5) | 20);
        emit32(equal | (source << 16) | (source << 5) | 21);
        e_v3(0x4E801800u | size, 22, 20, 21);
        e_v3(0x4E805800u | size, 21, 20, 21);
        e_v3(0x4E201C00u, 20, 22, 21);
        emit32(0x4F005400u | (sign_shift << 16) | (20 << 5) | 20);
    }
    e_v3(0x4E801800u | size, 17, vd, source);
    e_v3(0x4E805800u | size, 18, vd, source);
    if (opcode == 0x7C)
        e_v3(0x4E20D400u | size, vd, 18, 17); // HADD uses odd + even for x86 NaN selection.
    else
        e_v3(0x4EA0D400u | size, vd, 17, 18);
    if (fix_nan) emit_dnan_post(vd, double_precision, 1);
    return TX_NEXT;
}

// Returns the AArch64 SIMD encoding for packed integer operations whose x86
// semantics are one lane-wise binary instruction. Zero means another family.
static uint32_t sse_packed_binary_opcode(uint8_t opcode) {
    switch (opcode) {
    case 0xEF: return 0x6E201C00u; // PXOR
    case 0xDB: return 0x4E201C00u; // PAND
    case 0xEB: return 0x4EA01C00u; // POR
    case 0xDF: return 0x4E601C00u; // PANDN (operands reversed below)
    case 0x74: return 0x6E208C00u; // PCMPEQB
    case 0x75: return 0x6E608C00u; // PCMPEQW
    case 0x76: return 0x6EA08C00u; // PCMPEQD
    case 0x64: return 0x4E203400u; // PCMPGTB
    case 0x65: return 0x4E603400u; // PCMPGTW
    case 0x66: return 0x4EA03400u; // PCMPGTD
    case 0xDE: return 0x6E206400u; // PMAXUB
    case 0xDA: return 0x6E206C00u; // PMINUB
    case 0xEE: return 0x4E606400u; // PMAXSW
    case 0xEA: return 0x4E606C00u; // PMINSW
    case 0xFC: return 0x4E208400u; // PADDB
    case 0xFD: return 0x4E608400u; // PADDW
    case 0xFE: return 0x4EA08400u; // PADDD
    case 0xD4: return 0x4EE08400u; // PADDQ
    case 0xF8: return 0x6E208400u; // PSUBB
    case 0xF9: return 0x6E608400u; // PSUBW
    case 0xFA: return 0x6EA08400u; // PSUBD
    case 0xFB: return 0x6EE08400u; // PSUBQ
    case 0xDC: return 0x6E200C00u; // PADDUSB
    case 0xDD: return 0x6E600C00u; // PADDUSW
    case 0xEC: return 0x4E200C00u; // PADDSB
    case 0xED: return 0x4E600C00u; // PADDSW
    case 0xD8: return 0x6E202C00u; // PSUBUSB
    case 0xD9: return 0x6E602C00u; // PSUBUSW
    case 0xE8: return 0x4E202C00u; // PSUBSB
    case 0xE9: return 0x4E602C00u; // PSUBSW
    case 0xE0: return 0x6E201400u; // PAVGB
    case 0xE3: return 0x6E601400u; // PAVGW
    case 0xD5: return 0x4E609C00u; // PMULLW
    default: return 0;
    }
}

static int lower_sse_packed_binary(struct insn *instruction, uint64_t next, int vd, int vm, int mmx) {
    uint32_t encoding = sse_packed_binary_opcode(instruction->op);
    if (!encoding) return TX_FALL;
    int source = instruction->is_mem ? 16 : vm;
    if (instruction->is_mem) g_ldr_vec_ea(16, instruction, next, mmx);
    if (instruction->op == 0xDF)
        e_v3(encoding, vd, source, vd);
    else
        e_v3(encoding, vd, vd, source);
    return TX_NEXT;
}

static int lower_sse_widening_multiply(struct insn *instruction, uint64_t next, int vd, int vm, int mmx) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xE5 && opcode != 0xE4 && opcode != 0xF5) return TX_FALL;
    int source = instruction->is_mem ? 16 : vm;
    if (instruction->is_mem) g_ldr_vec_ea(16, instruction, next, mmx);
    if (opcode == 0xF5) { // PMADDWD: signed products followed by adjacent pair addition.
        emit32(0x0E60C000u | (source << 16) | (vd << 5) | 18);
        int high = mmx ? 18 : 19;
        if (!mmx) emit32(0x4E60C000u | (source << 16) | (vd << 5) | 19);
        emit32(0x4EA0BC00u | (high << 16) | (18 << 5) | vd);
        return TX_NEXT;
    }
    // PMULHW/PMULHUW widen each half independently; UZP2 selects the upper
    // word of each 32-bit product. MMX has only the low four input words.
    uint32_t low = opcode == 0xE5 ? 0x0E60C000u : 0x2E60C000u;
    uint32_t high = opcode == 0xE5 ? 0x4E60C000u : 0x6E60C000u;
    emit32(low | (source << 16) | (vd << 5) | 18);
    if (mmx) {
        emit32(0x0F108400u | (18 << 5) | vd);
    } else {
        emit32(high | (source << 16) | (vd << 5) | 19);
        emit32(0x4E405800u | (19 << 16) | (18 << 5) | vd);
    }
    return TX_NEXT;
}

static int lower_double_shift(struct insn *instruction, uint64_t next) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xA4 && opcode != 0xA5 && opcode != 0xAC && opcode != 0xAD) return TX_FALL;
    int isleft = (opcode == 0xA4 || opcode == 0xA5), bycl = (opcode == 0xA5 || opcode == 0xAD);
    int w = instruction->opsize, mem;
    if (w == 2) {
        // 16-bit SHLD/SHRD: EXTR can't do 16-bit lanes, so build a 32-bit concatenation and
        // shift it. SHLD: t = (dst<<16)|src; t<<=n; result = t>>16. SHRD: t = (src<<16)|dst;
        // t>>=n; result = t&0xffff. Exact for n in [0,16] (x86 leaves n>15 undefined for 16-bit).
        int dst = rm_load(instruction, next, 2, &mem), src = instruction->reg;
        e_uxt(19, dst, 2); // x19 = dst & 0xffff
        e_uxt(20, src, 2); // x20 = src & 0xffff
        if (!bycl) {
            int n = (int)(instruction->imm & 31);
            if (n == 0) {
                if (mem) e_store(2, dst, 17);
                return TX_NEXT;
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
        rm_store(instruction, 2, 16);
        return TX_NEXT;
    }
    int ssf = (w == 8) ? 1 : 0, width = ssf ? 64 : 32;
    int dst = rm_load(instruction, next, w, &mem), src = instruction->reg;
    if (!bycl) {
        int n = (int)(instruction->imm & (ssf ? 63 : 31));
        if (n == 0) {
            if (mem) e_store(w, dst, 17);
            return TX_NEXT;
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
        rm_store(instruction, w, 16);
        return TX_NEXT;
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
    e_rrr(A_AND, 21, 21, 19, 0, 0); // x21 = x86 CF (0/1)
    e_rrr(A_EOR, 21, 21, 19, 0, 0); // x21 = NOT CF (stored borrow convention)
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
    rm_store(instruction, w, 16);
    return TX_NEXT;
}

struct near_branch_context {
    hl_x86_trace_state *trace;
    uint64_t *seen;
    int *seen_count;
    int *block_count;
    int *condition_count;
    int stitch_ok;
    uint64_t start;
    void *body;
};

static int lower_near_conditional_branch(struct insn *instruction, uint64_t *guest_pc, uint64_t next,
                                         struct near_branch_context *context) {
    uint8_t opcode = instruction->op;
    if ((opcode & 0xF0) != 0x80) return TX_FALL;
    int low = opcode & 0xF;
    int parity = low == 0xA || low == 0xB;
    int condition;
    if (parity) {
        condition = emit_parity_jcc_cond(low);
    } else {
        condition = x86cc_to_arm(low);
        if (condition < 0) {
            if (g_fl_pending) flags_materialize();
            report_unimpl(*guest_pc, instruction);
            return TX_BREAK;
        }
    }
    uint64_t taken = next + (uint64_t)instruction->imm;
    if (!parity && taken == context->start && !notier2x() &&
        !hl_x86_trace_loop_hazard((uint64_t)context->body, (uint64_t)g_cp)) {
        int slot = g_tier2_build ? 0 : t2_slot(context->start);
        if (g_tier2_build || slot >= 0) {
            hl_x86_trace_self_loop(context->trace, condition, context->start, next, context->body, slot);
            return TX_BREAK;
        }
    }
    uint64_t fall = next;
    int stitch_fall = context->stitch_ok && fall != context->start &&
                      !hl_x86_trace_seen(context->seen, *context->seen_count, fall) && !map_body(fall) &&
                      !hl_x86_trace_trap_head(fall);
    int save_taken = 0;
    int save_fall = 0;
    if (!parity)
        hl_x86_trace_jcc_flags(context->trace, taken, fall, *guest_pc, stitch_fall, condition, &save_taken,
                               &save_fall);
    if (stitch_fall) {
        int inverse = (condition ^ 1) & 0xF;
        uint32_t *patch = (uint32_t *)g_cp;
        emit32(0);
        if (parity) e_nzcv_load();
        emit_jcc_edge_spill(save_taken);
        emit_chain_exit(taken);
        int64_t distance = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
        *patch = 0x54000000u | (((uint32_t)distance & 0x7FFFF) << 5) | (uint32_t)inverse;
        if (parity) e_nzcv_load();
        context->seen[(*context->seen_count)++] = fall;
        (*context->block_count)++;
        (*context->condition_count)++;
        *guest_pc = fall;
        return TX_NEXT;
    }
    uint32_t *patch = (uint32_t *)g_cp;
    emit32(0);
    if (parity) e_nzcv_load();
    emit_jcc_edge_spill(save_fall);
    emit_chain_exit(next);
    int64_t distance = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
    *patch = 0x54000000u | (((uint32_t)distance & 0x7FFFF) << 5) | (condition & 0xF);
    if (parity) e_nzcv_load();
    emit_jcc_edge_spill(save_taken);
    emit_chain_exit(taken);
    return TX_BREAK;
}

static int lower_conditional_data_move(struct insn *instruction, uint64_t guest_pc, uint64_t next, int sf) {
    uint8_t opcode = instruction->op;
    // setcc (0F 90-9F) -> r/m8 (byte: preserve upper bits / hi-lo byte regs)
    if ((opcode & 0xF0) == 0x90) {
        int lo = opcode & 0xF;
        if (lo == 0xA || lo == 0xB) { // setp/setnp: real PF lane (integer parity or comisd unordered)
            if (instruction->is_mem) emit_ea(instruction, next);
            e_pf_compute(19); // x19 = x86 PF (uses x16 as scratch; x17/EA preserved)
            if (lo == 0xB) {
                e_movconst(16, 1);
                e_rrr(A_EOR, 19, 19, 16, 0, 0); // setnp = NOT PF
            }
            if (instruction->is_mem)
                e_store(1, 19, 17);
            else
                byte_wb(instruction, instruction->rm_reg, 19);
            return TX_NEXT;
        }
        int cc = x86cc_to_arm(opcode & 0xF);
        if (cc < 0) {
            report_unimpl(guest_pc, instruction);
            return TX_BREAK;
        }
        if (instruction->is_mem) {
            emit_ea(instruction, next); // EA -> x17 FIRST (emit_ea may clobber x16)
            e_nzcv_load();
            e_cset(16, cc, 0);
            e_store(1, 16, 17);
        } else {
            e_nzcv_load();
            e_cset(16, cc, 0);
            byte_wb(instruction, instruction->rm_reg, 16);
        }
        return TX_NEXT;
    }
    // cmovcc (0F 40-4F), reg or mem source
    if ((opcode & 0xF0) == 0x40) {
        int lo = opcode & 0xF;
        if (lo == 0xA || lo == 0xB) { // cmovp / cmovnp: real PF lane
            e_pf_compute(19);         // x19 = x86 PF (before rm_load, which reuses x16/x17)
            int mem;
            int rmv = rm_load(instruction, next, instruction->opsize, &mem);
            e_rrr(A_SUBS, 31, 19, 31, 0, 0); // Z = (PF == 0)
            if (instruction->opsize == 2) {             // 16-bit cmov writes only bits 15:0
                e_csel(21, rmv, instruction->reg, (lo == 0xA) ? 1 : 0, 0);
                e_bfi(instruction->reg, 21, 0, 16, 1);
            } else
                e_csel(instruction->reg, rmv, instruction->reg, (lo == 0xA) ? 1 : 0, sf); // cmovp: NE; cmovnp: EQ
            // parity-edge fix: the SUBS above clobbered the live ARM NZCV; restore the
            // canonical flags (membank is current: the top-of-loop materialized any pending
            // producer before this consumer) so a following block exit spills true flags.
            e_nzcv_load();
            return TX_NEXT;
        }
        int cc = x86cc_to_arm(opcode & 0xF);
        if (cc < 0) {
            report_unimpl(guest_pc, instruction);
            return TX_BREAK;
        }
        int mem;
        int rmv = rm_load(instruction, next, instruction->opsize, &mem);
        e_nzcv_load();
        if (instruction->opsize == 2) { // CMOVcc r16: bits 63:16 of the destination are PRESERVED
            e_csel(21, rmv, instruction->reg, cc, 0);
            e_bfi(instruction->reg, 21, 0, 16, 1);
        } else
            e_csel(instruction->reg, rmv, instruction->reg, cc, sf);
        return TX_NEXT;
    }
    return TX_FALL;
}

static int lower_x87_memory_state(struct insn *instruction, uint64_t guest_pc, uint64_t next, int reg) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xD9 && opcode != 0xDD && opcode != 0xDB && opcode != 0xDF) return TX_FALL;
    if (opcode == 0xD9) {    // f32 mem
        if (reg == 0) {
            g_ldr_s(16, 19);
            e_fmov_from_s(20, 16);
            hl_x86_x87_denormal(20, 1);
            emit32(0x1E22C000u | (16 << 5) | 16);
            hl_x86_x87_push(16);
        } // fld m32
        else if (reg == 2 || reg == 3) {
            hl_x86_x87_live(0, -1);
            hl_x86_x87_load(16, 0);
            hl_x86_x87_rc_enter(); // the one x87 store that ROUNDS, so the one that needs RC
            emit32(0x1E624000u | (16 << 5) | 16);
            hl_x86_x87_rc_leave();
            e_movconst(20, 0xffc00000u); // masked #IS delivers the m32 REAL indefinite
            e_fmov_to_s(17, 20);
            e_subi_s(31, 22, 0, 1);
            emit32(0x1E200C00u | (17 << 16) | (0u << 12) | (16 << 5) | 16); // fcsel s16,s16,s17,eq
            g_str_s(16, 19);
            if (reg == 3) hl_x86_x87_pop();
        } // fst/fstp
        else if (reg == 5) { // fldcw m16: load the x87 control word (RC/PC/exception masks)
            emit32(0x79400000u | (19 << 5) | 16); // ldrh w16, [x19]
            e_movconst(17, 0x1f3f);               // FCW is not stored verbatim: bit 6 reads
            e_rrr(A_AND, 16, 16, 17, 1, 0);       // back 1, bit 7 and 15:13 as 0 (measured:
            e_movconst(17, 0x40);                 // fldcw ffff -> fnstcw 1f7f)
            e_rrr(A_ORR, 16, 16, 17, 1, 0);
            e_str(16, 28, OFF_FPCW); // cpu->fpcw = CW (honored by fist rounding)
        } else if (reg == 7) {       // fnstcw m16: store the live x87 control word
            e_ldr(16, 28, OFF_FPCW);
            emit32(0x79000000u | (19 << 5) | 16); // strh w16, [x19]
        } // fnstcw
        else if (reg == 4 || reg == 6) {
            emit_x87_environment(reg == 6 ? X87ENV_STORE : X87ENV_LOAD, next);
            return TX_BREAK;
        } // fnstenv / fldenv m28
        else {
            report_unimpl(guest_pc, instruction);
            return TX_BREAK;
        }
    } else if (opcode == 0xDD) { // f64 mem
        if (reg == 0) {
            g_ldr_d(16, 19);
            e_fmov_from_d(20, 16);
            hl_x86_x87_denormal(20, 0);
            hl_x86_x87_push(16);
        } // fld m64
        else if (reg == 2 || reg == 3) {
            hl_x86_x87_live(0, -1);
            hl_x86_x87_load(16, 0);
            hl_x86_x87_indefinite(16);
            g_str_d(16, 19);
            if (reg == 3) hl_x86_x87_pop();
        } // fst/fstp
        else if (reg == 4 || reg == 6) {
            emit_x87_environment(reg == 6 ? X87ENV_SAVE : X87ENV_RESTORE, next);
            return TX_BREAK;
        } // fnsave / frstor m108
        else if (reg == 7) {
            hl_x86_x87_status();
            emit32(0x79000000u | (19 << 5) | 16);
        } // fnstsw m16
        else {
            report_unimpl(guest_pc, instruction);
            return TX_BREAK;
        }
    } else if (opcode == 0xDB) { // i32 mem / m80
        if (reg == 0) {
            emit32(0xB9400000u | (19 << 5) | 16);
            emit32(0x1E620000u | (16 << 5) | 16);
            hl_x86_x87_push(16);
        } // fild m32
        else if (reg == 2 || reg == 3) {
            hl_x86_x87_live(0, -1);
            hl_x86_x87_load(16, 0);
            emit_x87_round_st0();                 // round per x87 control word (default: nearest)
            emit32(0x1E780000u | (16 << 5) | 16); // FCVTZS w16,d16 (exact: d16 already integral)
            emit_x87_integer_indefinite(16, 4);
            emit32(0xB9000000u | (19 << 5) | 16);
            if (reg == 3) hl_x86_x87_pop();
        } // fist/fistp m32
        else if (reg == 5) {
            e_str(19, 28, OFF_X87EA);
            emit_exit_const(next, R_X87FLD);
            return TX_BREAK;
        } // fld m80 -> C
        else if (reg == 7) {
            e_str(19, 28, OFF_X87EA);
            emit_exit_const(next, R_X87FSTP);
            return TX_BREAK;
        } // fstp m80 -> C
        else {
            report_unimpl(guest_pc, instruction);
            return TX_BREAK;
        }
    } else if (opcode == 0xDF) { // i16/i64 mem
        if (reg == 0) {
            emit32(0x79C00000u | (19 << 5) | 16);
            emit32(0x1E620000u | (16 << 5) | 16);
            hl_x86_x87_push(16);
        } // fild m16 (ldrsh)
        else if (reg == 3) {
            hl_x86_x87_live(0, -1);
            hl_x86_x87_load(16, 0);
            emit_x87_round_st0();                 // round per x87 control word (default: nearest)
            emit32(0x1E780000u | (16 << 5) | 16); // FCVTZS w16,d16 (exact: d16 already integral)
            emit_x87_integer_indefinite(16, 2);
            emit32(0x79000000u | (19 << 5) | 16);
            hl_x86_x87_pop();
        } // fistp m16
        else if (reg == 5) {
            e_ldr(16, 19, 0);
            emit32(0x9E620000u | (16 << 5) | 16);
            hl_x86_x87_push(16);
        } // fild m64
        else if (reg == 7) {
            hl_x86_x87_live(0, -1);
            hl_x86_x87_load(16, 0);
            emit_x87_round_st0();                 // round per x87 control word (default: nearest)
            emit32(0x9E780000u | (16 << 5) | 16); // FCVTZS x16,d16 (exact: d16 already integral)
            emit_x87_integer_indefinite(16, 8);
            e_str(16, 19, 0);
            hl_x86_x87_pop();
        } // fistp m64
        else {
            report_unimpl(guest_pc, instruction);
            return TX_BREAK;
        }
    }
    return TX_NEXT;
}

static int lower_x87_d9_register(struct insn *instruction, uint64_t guest_pc, uint64_t next, int reg, int rm) {
    if (instruction->op != 0xD9) return TX_FALL;
    if (reg == 0) {
        hl_x86_x87_live(rm, -1); // an empty SOURCE underflows; the PUSHED slot takes the
        hl_x86_x87_load(16, rm); // indefinite and the source stays empty
        hl_x86_x87_indefinite(16);
        hl_x86_x87_push(16);
    } // fld ST(i)
    else if (reg == 1) {
        // FXCH: the exchange happens either way, then ST0 takes the indefinite -- ST(i) keeps
        // the old ST0 and is tagged live (measured on `fld1; fxch %st(1)`).
        hl_x86_x87_live(0, rm);
        hl_x86_x87_load(16, 0);
        hl_x86_x87_load(18, rm);
        hl_x86_x87_indefinite(18);
        hl_x86_x87_store(18, 0);
        hl_x86_x87_store(16, rm);
    } // fxch
    else if (reg == 4 && rm == 0) {
        hl_x86_x87_live(0, -1);
        hl_x86_x87_load(16, 0);
        emit32(0x1E614000u | (16 << 5) | 16);
        hl_x86_x87_indefinite(16);
        hl_x86_x87_store(16, 0);
    } // fchs
    else if (reg == 4 && rm == 1) {
        hl_x86_x87_live(0, -1);
        hl_x86_x87_load(16, 0);
        emit32(0x1E60C000u | (16 << 5) | 16);
        hl_x86_x87_indefinite(16);
        hl_x86_x87_store(16, 0);
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
        hl_x86_x87_push(16);
    } else if (reg == 7 && rm == 2) {
        hl_x86_x87_live(0, -1);
        hl_x86_x87_load(16, 0);
        hl_x86_x87_indefinite(16);
        hl_x86_x87_rc_enter();
        hl_x86_x87_dnan_pre(16, 16); // fsqrt(-x): x86 yields the NEGATIVE indefinite
        emit32(0x1E61C000u | (16 << 5) | 16);
        hl_x86_x87_dnan_post(16);
        hl_x86_x87_narrow(16);
        hl_x86_x87_rc_leave();
        hl_x86_x87_store(16, 0);
    } // fsqrt
    else if (reg == 2 && rm == 0) { /* fnop */
    } else if (reg == 4 && rm == 4) {
        hl_x86_x87_live(0, -1);
        hl_x86_x87_test();
    } // ftst
    else if (reg == 4 && rm == 5) {
        hl_x86_x87_classify();
    } // fxam
    else if (reg == 6 && rm == 0) {
        hl_x86_x87_function(X87_F2XM1, next);
        return TX_BREAK;
    } // f2xm1
    else if (reg == 6 && rm == 1) {
        hl_x86_x87_function(X87_FYL2X, next);
        return TX_BREAK;
    } // fyl2x
    else if (reg == 6 && rm == 2) {
        hl_x86_x87_function(X87_FPTAN, next);
        return TX_BREAK;
    } // fptan
    else if (reg == 6 && rm == 3) {
        hl_x86_x87_function(X87_FPATAN, next);
        return TX_BREAK;
    } // fpatan
    else if (reg == 6 && rm == 4) {
        hl_x86_x87_live(0, -1);
        hl_x86_x87_extract();
    } // fxtract
    else if (reg == 6 && rm == 5) {
        hl_x86_x87_function(X87_FPREM1, next);
        return TX_BREAK;
    } // fprem1
    else if (reg == 6 && rm == 6) {
        hl_x86_x87_adjust_top(-1);
    } // fdecstp
    else if (reg == 6 && rm == 7) {
        hl_x86_x87_adjust_top(1);
    } // fincstp
    else if (reg == 7 && rm == 0) {
        hl_x86_x87_function(X87_FPREM, next);
        return TX_BREAK;
    } // fprem
    else if (reg == 7 && rm == 1) {
        hl_x86_x87_function(X87_FYL2XP1, next);
        return TX_BREAK;
    } // fyl2xp1
    else if (reg == 7 && rm == 3) {
        hl_x86_x87_function(X87_FSINCOS, next);
        return TX_BREAK;
    } // fsincos
    else if (reg == 7 && rm == 4) {
        hl_x86_x87_live(0, -1);
        hl_x86_x87_round();
    } // frndint
    else if (reg == 7 && rm == 5) {
        hl_x86_x87_live(0, 1);
        hl_x86_x87_scale();
    } // fscale
    else if (reg == 7 && rm == 6) {
        hl_x86_x87_function(X87_FSIN, next);
        return TX_BREAK;
    } // fsin
    else if (reg == 7 && rm == 7) {
        hl_x86_x87_function(X87_FCOS, next);
        return TX_BREAK;
    } // fcos
    else {
        report_unimpl(guest_pc, instruction);
        return TX_BREAK;
    }
    return TX_NEXT;
}

static void emit_x87_integer_compare(int left, int right, int signaling) {
    emit32(0x1E602000u | (signaling ? 0x10u : 0u) | (right << 16) | (left << 5));
    e_nzcv_save_fcmp();
}

static int lower_x87_register_control(struct insn *instruction, uint64_t guest_pc, int reg, int rm) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xDD && opcode != 0xDB && opcode != 0xDF && opcode != 0xDA) return TX_FALL;
    if (opcode == 0xDD) {
        if (reg == 0) {
            hl_x86_x87_tag(rm, 1); // FFREE: punch a hole no depth model can express
        } else if (reg == 2 || reg == 3) {
            hl_x86_x87_live(0, -1);
            hl_x86_x87_load(16, 0);
            hl_x86_x87_indefinite(16);
            hl_x86_x87_store(16, rm);
            if (reg == 3) hl_x86_x87_pop();
        } // fst/fstp ST(i)
        else if (reg == 4 || reg == 5) {
            hl_x86_x87_live(0, rm);
            hl_x86_x87_load(18, 0);
            hl_x86_x87_load(16, rm);
            hl_x86_x87_indefinite(18);
            hl_x86_x87_indefinite(16);
            e_fcom_setfpsw(18, 16, 0);
            if (reg == 5) hl_x86_x87_pop();
        } // fucom[p]
        else {
            report_unimpl(guest_pc, instruction);
            return TX_BREAK;
        }
    } else if (opcode == 0xDB) {
        if (reg == 4 && rm == 3) {
            // FNINIT: TOP=0 and every slot EMPTY (st[] itself is untouched, which is why a later
            // FLDENV can re-tag and read the old values), FCW=0x037f (all exceptions masked,
            // round-nearest, 64-bit), FSW and the host FPSR sticky exception flags cleared.
            e_movconst(16, HL_X87_EMPTY_ALL | HL_X87_ARMED);
            e_str(16, 28, OFF_FPTOP);
            e_movconst(16, 0);
            e_str(16, 28, OFF_FPSW);
            e_movconst(16, 0x037f);
            e_str(16, 28, OFF_FPCW);
            hl_x86_x87_clear_exceptions();
            if (hl_x86_x87_optimized()) { // anchor the translate-time shadow: top is now statically 0
                hl_x86_x87_anchor(0);     // memory and shadow agree
            }
        } // finit -> top=0
        else if (reg == 4 && rm == 2) {
            hl_x86_x87_clear_exceptions();
        } // fnclex: clear sticky exception flags
        else if (reg == 4) { /* fneni/fndisi/fnsetpm: no-op */
        } else if (reg == 5 || reg == 6) {
            hl_x86_x87_live(0, rm);
            hl_x86_x87_load(18, 0);
            hl_x86_x87_load(16, rm);
            hl_x86_x87_indefinite(18); // empty -> ZF=PF=CF=1, the unordered answer
            hl_x86_x87_indefinite(16);
            emit_x87_integer_compare(18, 16, reg == 6);
        } // fucomi(5, quiet) / fcomi(6, signals on any NaN)
        else {
            report_unimpl(guest_pc, instruction);
            return TX_BREAK;
        }
    } else if (opcode == 0xDF) {
        if (reg == 4 && rm == 0) {
            hl_x86_x87_status();
            e_bfi(RAX, 16, 0, 16, 1);
        } // fnstsw ax
        else if (reg == 5 || reg == 6) {
            hl_x86_x87_live(0, rm);
            hl_x86_x87_load(18, 0);
            hl_x86_x87_load(16, rm);
            hl_x86_x87_indefinite(18);
            hl_x86_x87_indefinite(16);
            emit_x87_integer_compare(18, 16, reg == 6);
            hl_x86_x87_pop();
        } // fucomip(5, quiet) / fcomip(6, signals on any NaN)
        else {
            report_unimpl(guest_pc, instruction);
            return TX_BREAK;
        }
    } else if (opcode == 0xDA) { // fcmovcc ST0,ST(i) (reg 0/1/2/3 = B/E/BE/U)
        if (reg <= 3) {      // condition from integer EFLAGS
            int jcc = (reg == 0) ? 2 : (reg == 1) ? 4 : (reg == 2) ? 6 : 10; // jb/je/jbe/jp
            int armc = x86cc_to_arm(jcc);
            e_nzcv_load();
            hl_x86_x87_load(18, 0);
            hl_x86_x87_load(16, rm); // v18=ST0, v16=ST(i)
            emit32(0x1E600C00u | (18 << 16) | ((armc & 0xF) << 12) | (16 << 5) |
                   17); // fcsel d17, STi, ST0, cond
            hl_x86_x87_store(17, 0);
        } else if (reg == 5 && rm == 1) { // DA E9: fucompp (compare ST0,ST1; pop twice)
            hl_x86_x87_live(0, 1);
            hl_x86_x87_load(18, 0);
            hl_x86_x87_load(16, 1);
            hl_x86_x87_indefinite(18);
            hl_x86_x87_indefinite(16);
            e_fcom_setfpsw(18, 16, 0);
            hl_x86_x87_pop();
            hl_x86_x87_pop();
        } else {
            report_unimpl(guest_pc, instruction);
            return TX_BREAK;
        }
    } else {
        report_unimpl(guest_pc, instruction);
        return TX_BREAK;
    }
    return TX_NEXT;
}

static void emit_x87_add(int destination, int left, int right) {
    emit32(0x1E602800u | (right << 16) | (left << 5) | destination);
}

static void emit_x87_subtract(int destination, int left, int right) {
    emit32(0x1E603800u | (right << 16) | (left << 5) | destination);
}

static void emit_x87_multiply(int destination, int left, int right) {
    emit32(0x1E600800u | (right << 16) | (left << 5) | destination);
}

static void emit_x87_divide(int destination, int left, int right) {
    emit32(0x1E601800u | (right << 16) | (left << 5) | destination);
}

static int lower_x87_memory_arithmetic(struct insn *instruction, uint64_t guest_pc, int reg) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xD8 && opcode != 0xDC && opcode != 0xDA && opcode != 0xDE) return TX_FALL;
    // declared memory type -- m32/m64 float (D8/DC) or a SIGNED 32/16-bit integer
    // (DA/DE: the fiadd/fimul/ficom/fisub/fidiv group) -- then share the reg-field
    // arith dispatch below (identical fadd(0)/fmul(1)/fcom(2)/fcomp(3)/fsub(4)/
    // fsubr(5)/fdiv(6)/fdivr(7) encoding for all four opcodes).
    if (opcode == 0xD8) { // m32 float
        g_ldr_s(16, 19);
        e_fmov_from_s(20, 16);
        hl_x86_x87_denormal(20, 1);
        emit32(0x1E22C000u | (16 << 5) | 16); // fcvt d16, s16
    } else if (opcode == 0xDA) {                  // m32 signed integer
        emit32(0xB9400000u | (19 << 5) | 16); // ldr   w16, [x19]
        emit32(0x1E620000u | (16 << 5) | 16); // scvtf d16, w16
    } else if (opcode == 0xDE) {                  // m16 signed integer
        emit32(0x79C00000u | (19 << 5) | 16); // ldrsh w16, [x19]
        emit32(0x1E620000u | (16 << 5) | 16); // scvtf d16, w16
    } else {                                  // 0xDC: m64 float
        g_ldr_d(16, 19);
        e_fmov_from_d(20, 16);
        hl_x86_x87_denormal(20, 0);
    }
    hl_x86_x87_live(0, -1);
    if (reg == 2 || reg == 3) {
        hl_x86_x87_load(18, 0);
        hl_x86_x87_indefinite(18); // an empty ST0 compares UNORDERED as well as faulting
        hl_x86_x87_indefinite(16);
        e_fcom_setfpsw(18, 16, 1);
        if (reg == 3) hl_x86_x87_pop();
        return TX_NEXT;
    } // fcom/fcomp
    hl_x86_x87_load(18, 0);
    hl_x86_x87_indefinite(18);
    hl_x86_x87_indefinite(16);
    hl_x86_x87_rc_enter();
    hl_x86_x87_dnan_pre(18, 16); // x86 indefinite is NEGATIVE; ARM's default NaN is not
    if (reg == 0)
        emit_x87_add(18, 18, 16);
    else if (reg == 1)
        emit_x87_multiply(18, 18, 16);
    else if (reg == 4)
        emit_x87_subtract(18, 18, 16);
    else if (reg == 5)
        emit_x87_subtract(18, 16, 18);
    else if (reg == 6)
        emit_x87_divide(18, 18, 16);
    else if (reg == 7)
        emit_x87_divide(18, 16, 18);
    else {
        report_unimpl(guest_pc, instruction);
        return TX_BREAK;
    }
    hl_x86_x87_dnan_post(18);
    hl_x86_x87_narrow(18); // FCW.PC, inside the RC scope so it rounds the same way
    hl_x86_x87_rc_leave();
    hl_x86_x87_store(18, 0);
    return TX_NEXT;
}

static int lower_x87_register_arithmetic(struct insn *instruction, uint64_t guest_pc, int reg, int rm) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xD8 && opcode != 0xDC && opcode != 0xDE) return TX_FALL;
        hl_x86_x87_live(0, rm);
        hl_x86_x87_load(18, 0);
        hl_x86_x87_load(16, rm); // v18=ST0, v16=ST(rm)
        hl_x86_x87_indefinite(18);
        hl_x86_x87_indefinite(16);
        int dst_i = (opcode == 0xD8) ? 0 : rm; // D8 -> ST0; DC/DE -> ST(i)
        if (reg == 2 || reg == 3) {
            e_fcom_setfpsw(18, 16, 1);
            if (opcode == 0xDE && rm == 1) hl_x86_x87_pop();
            if (reg == 3) hl_x86_x87_pop();
            return TX_NEXT;
        } // fcom[p]/fcompp
        int a = 18, b = 16;
        if (opcode != 0xD8) {
            a = 16;
            b = 18;
        } // DC/DE: dst=ST(i)=v16, other=ST0=v18
        hl_x86_x87_rc_enter();
        hl_x86_x87_dnan_pre(18, 16); // x86 indefinite is NEGATIVE; ARM's default NaN is not
        if (reg == 0)
            emit_x87_add(a, a, b);
        else if (reg == 1)
            emit_x87_multiply(a, a, b);
        else if (reg == 4) {
            if (opcode == 0xD8)
                emit_x87_subtract(a, a, b);
            else
                emit_x87_subtract(a, b, a);
        } // DC/DE reverse sub
        else if (reg == 5) {
            if (opcode == 0xD8)
                emit_x87_subtract(a, b, a);
            else
                emit_x87_subtract(a, a, b);
        } else if (reg == 6) {
            if (opcode == 0xD8)
                emit_x87_divide(a, a, b);
            else
                emit_x87_divide(a, b, a);
        } else if (reg == 7) {
            if (opcode == 0xD8)
                emit_x87_divide(a, b, a);
            else
                emit_x87_divide(a, a, b);
        } else {
            report_unimpl(guest_pc, instruction);
            return TX_BREAK;
        }
        hl_x86_x87_dnan_post(a);
        hl_x86_x87_narrow(a); // FCW.PC, inside the RC scope so it rounds the same way
        hl_x86_x87_rc_leave();
        hl_x86_x87_indefinite(a);
        hl_x86_x87_store(a, dst_i);
        if (opcode == 0xDE) hl_x86_x87_pop();
    return TX_NEXT;
}

static int lower_scalar_two_byte(struct insn *instruction, uint64_t guest_pc, uint64_t next, int sf,
                                 const hl_x86_trace_state *trace_state) {
    uint8_t opcode = instruction->op;
    if (opcode == 0xC3) {
        emit_ea(instruction, next);
        emit_memory_guard(17, (uint64_t)instruction->opsize, guest_pc, X86_SOFT_WRITE);
        e_store(instruction->opsize, instruction->reg, 17);
        if (emit_soft_memory_active()) emit_soft_store_commit((uint64_t)instruction->opsize);
        return TX_NEXT;
    }
    if (opcode == 0xAF) {
        int memory;
        int source = rm_load(instruction, next, instruction->opsize, &memory);
        int carry_overflow_live = !trace_state->flag_elision ||
                                  (hl_x86_trace_flags_livein(trace_state, next, guest_pc) & HL_X86_FLAG_NZCV);
        e_imul2(instruction->reg, instruction->reg, source, instruction->opsize, carry_overflow_live);
        return TX_NEXT;
    }
    if (opcode >= 0xC8 && opcode <= 0xCF) {
        int reg = (opcode - 0xC8) | (instruction->rexB << 3);
        emit32((sf ? 0xDAC00C00u : 0x5AC00800u) | (reg << 5) | reg);
        return TX_NEXT;
    }
    if (opcode != 0xB6 && opcode != 0xB7 && opcode != 0xBE && opcode != 0xBF) return TX_FALL;
    int source_width = (opcode & 1) ? 2 : 1;
    int signed_extension = opcode >= 0xBE;
    int destination_width = instruction->opsize;
    int destination = destination_width == 2 ? 16 : instruction->reg;
    if (instruction->is_mem) {
        emit_ea(instruction, next);
        emit_bus_guard(17, (uint64_t)source_width, guest_pc);
        if (signed_extension)
            e_ldrs_w(source_width, destination, 17, destination_width == 8);
        else
            e_load(source_width, destination, 17);
    } else {
        int source = source_width == 1 ? byte_val(instruction, instruction->rm_reg, 16) : instruction->rm_reg;
        if (signed_extension)
            e_sxt_to(destination, source, source_width, destination_width == 8);
        else
            e_uxt(destination, source, source_width);
    }
    if (destination_width == 2) e_bfi(instruction->reg, 16, 0, 16, 1);
    return TX_NEXT;
}

static int lower_one_byte_signal_and_lookup(struct insn *instruction, uint64_t next) {
    uint8_t opcode = instruction->op;
    if (opcode == 0xCC) {
        emit_guest_signal(next, 5, 0x80);
        return TX_BREAK;
    }
    if (opcode == 0xF1) {
        emit_guest_signal(next, 5, 1);
        return TX_BREAK;
    }
    if (opcode != 0xD7) return TX_FALL;
    e_uxt(19, RAX, 1);
    struct insn base = *instruction;
    base.is_mem = 1;
    base.m_hasbase = 1;
    base.m_base = RBX;
    base.m_hasindex = 0;
    base.rip_rel = 0;
    base.disp = 0;
    base.imm = 0;
    emit_ea(&base, next);
    e_rrr(A_ADD, 17, 17, 19, 1, 0);
    if (instruction->addr32) e_uxt(17, 17, 4);
    emit_bus_guard(17, 1, next - (uint64_t)instruction->len);
    e_load(1, 16, 17);
    byte_wb(instruction, RAX, 16);
    return TX_NEXT;
}

static int lower_two_byte_boundary(const struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    uint8_t opcode = instruction->op;
    if (opcode == 0x05) {
        if (g_fastsys) {
            emit_fast_syscall(next);
            emit_chain_exit(next);
        } else {
            emit_exit_const(next, R_SYSCALL);
        }
        return TX_BREAK;
    }
    if (opcode == 0x0B || opcode == 0xB9 || opcode == 0xFF) {
        emit_sigill(guest_pc);
        return TX_BREAK;
    }
    return TX_FALL;
}

static int lower_sse_shuffle(struct insn *instruction, uint64_t next, int vd, int vm, int mmx) {
    if (instruction->op != 0x70) return TX_FALL;
    int source = instruction->is_mem ? 16 : vm;
    if (instruction->is_mem) g_ldr_vec_ea(16, instruction, next, mmx);
    unsigned immediate = (unsigned)instruction->imm & 0xff;
    if (instruction->p66) {
        if (immediate == 0xE4) {
            if (vd != source) e_vmov(vd, source);
        } else if (immediate == 0x4E) {
            e_ext(vd, source, source, 8);
        } else if (immediate == 0xB1) {
            emit32(0x4EA00800u | (source << 5) | vd);
        } else if (immediate == 0x00 || immediate == 0x55 || immediate == 0xAA || immediate == 0xFF) {
            hl_x86_emit_vector_broadcast32(vd, source, (int)(immediate & 3));
        } else {
            int output = vd == source ? 17 : vd;
            for (int lane = 0; lane < 4; lane++)
                e_ins_s(output, lane, source, (immediate >> (2 * lane)) & 3);
            if (output != vd) e_vmov(vd, output);
        }
        return TX_NEXT;
    }
    if (instruction->rep || instruction->repne) {
        int high = instruction->rep;
        e_vmov(17, source);
        for (int lane = 0; lane < 4; lane++) {
            int destination_lane = high ? 4 + lane : lane;
            int source_lane = (high ? 4 : 0) + (int)((immediate >> (2 * lane)) & 3);
            emit32(0x6E000400u | ((((unsigned)destination_lane << 2) | 2u) << 16) |
                   (((unsigned)source_lane << 1) << 11) | (source << 5) | 17);
        }
        e_vmov(vd, 17);
        return TX_NEXT;
    }
    return TX_FALL;
}

static int lower_sse_sign_mask(struct insn *instruction, int vm, int mmx) {
    if (instruction->op == 0x50) {
        if (instruction->p66) {
            e_vshr_imm(17, vm, 64, 63, 0);
            emit32(0x4E003C00u | ((0u * 16 + 8) << 16) | (17 << 5) | instruction->reg);
            emit32(0x4E003C00u | ((1u * 16 + 8) << 16) | (17 << 5) | 19);
            e_rrr(A_ORR, instruction->reg, instruction->reg, 19, 1, 1);
        } else {
            e_vshr_imm(17, vm, 32, 31, 0);
            emit32(0x0E003C00u | ((0u * 8 + 4) << 16) | (17 << 5) | instruction->reg);
            for (int lane = 1; lane < 4; lane++) {
                emit32(0x0E003C00u | (((unsigned)lane * 8 + 4) << 16) | (17 << 5) | 19);
                e_rrr(A_ORR, instruction->reg, instruction->reg, 19, 0, lane);
            }
        }
        return TX_NEXT;
    }
    if (instruction->op != 0xD7) return TX_FALL;
    int source = vm;
    if (mmx) {
        e_vmov8(18, vm);
        source = 18;
    }
    if (!nosseopt()) {
        g_pmovmskb_n++;
        e_vshr_imm(17, source, 8, 7, 0);
        emit32(0x6F001400u | (25u << 16) | (17 << 5) | 17);
        emit32(0x6F001400u | (50u << 16) | (17 << 5) | 17);
        emit32(0x6F001400u | (100u << 16) | (17 << 5) | 17);
        emit32(0x0E003C00u | (1u << 16) | (17 << 5) | 16);
        emit32(0x0E003C00u | (17u << 16) | (17 << 5) | instruction->reg);
        e_rrr(A_ORR, instruction->reg, 16, instruction->reg, 0, 8);
    } else {
        e_str_q(source, 28, OFF_MM);
        e_addi(17, 28, OFF_MM, 1);
        e_movz(instruction->reg, 0, 0);
        for (int lane = 0; lane < 16; lane++) {
            emit32(0x39400000u | ((unsigned)lane << 10) | (17 << 5) | 16);
            emit32(0x53071C00u | (16 << 5) | 16);
            emit32(0x2A000000u | (16 << 16) | ((unsigned)lane << 10) | (instruction->reg << 5) |
                   instruction->reg);
        }
    }
    return TX_NEXT;
}

static int lower_mmx_fp_conversion(struct insn *instruction, uint64_t next, int vd, int vm) {
    uint8_t opcode = instruction->op;
    if ((opcode != 0x2A && opcode != 0x2C && opcode != 0x2D) || instruction->rep || instruction->repne)
        return TX_FALL;
    int truncate = opcode == 0x2C;
    if (opcode == 0x2A) {
        int source = vm & 7;
        if (instruction->is_mem) {
            g_ldr_d_ea(16, instruction, next);
            source = 16;
        }
        if (instruction->p66) {
            emit32(0x0F20A400u | (source << 5) | 18);
            emit32(0x4E61D800u | (18 << 5) | 18);
            e_vmov(vd, 18);
        } else {
            emit32(0x0E21D800u | (source << 5) | 18);
            e_ins_d(vd, 0, 18, 0);
        }
        return TX_NEXT;
    }
    if (instruction->p66) {
        int source = vm;
        if (instruction->is_mem) {
            g_ldr_q_ea(16, instruction, next);
            source = 16;
        }
        e_movconst(16, 0x41E0000000000000ull);
        emit32(0x4E080C00u | (16 << 5) | 19);
        e_movconst(16, 0xC1E0000000000000ull);
        emit32(0x4E080C00u | (16 << 5) | 20);
        emit_pd2i32_pieces(24, 22, source, truncate, 19, 20, 23, 21);
        emit32(0x0EA12800u | (24 << 5) | 24);
        emit32(0x0EA12800u | (22 << 5) | 22);
        e_movconst(16, 0x80000000ull);
        emit32(0x0E040C00u | (16 << 5) | 18);
        emit32(0x2E601C00u | (24 << 16) | (18 << 5) | 22);
        e_vmov8(vd & 7, 22);
        return TX_NEXT;
    }
    int source = vm;
    if (instruction->is_mem) {
        g_ldr_d_ea(16, instruction, next);
        source = 16;
    }
    if (truncate) {
        emit32(0x0EA1B800u | (source << 5) | 21);
    } else {
        emit32(0x2E219800u | (source << 5) | 21);
        emit32(0x0EA1B800u | (21 << 5) | 21);
    }
    e_movconst(16, 0x4F000000ull);
    emit32(0x0E040C00u | (16 << 5) | 17);
    emit32(0x2E20E400u | (17 << 16) | (source << 5) | 19);
    emit32(0x0E20E400u | (source << 16) | (source << 5) | 20);
    emit32(0x2E205800u | (20 << 5) | 20);
    e_v3(0x0EA01C00u, 19, 19, 20);
    e_movconst(16, 0x80000000ull);
    emit32(0x0E040C00u | (16 << 5) | 18);
    emit32(0x2E601C00u | (21 << 16) | (18 << 5) | 19);
    e_vmov8(vd & 7, 19);
    return TX_NEXT;
}

// Owns the x87 run boundary as well as opcode dispatch. Every memory form
// materializes the shadow stack before a potentially faulting access or C exit.
static int lower_x87_family(struct insn *instruction, uint64_t guest_pc, uint64_t next) {
    if (instruction->op < 0xD8 || instruction->op > 0xDF) return TX_FALL;
    mark_vdirty();
    int reg = instruction->reg & 7;
    int rm = instruction->rm_reg & 7;
    if (instruction->is_mem) {
        hl_x86_x87_materialize();
        emit_ea(instruction, next);
        int bytes = (instruction->op == 0xD8 || instruction->op == 0xDA) ? 4
                    : instruction->op == 0xDC                              ? 8
                    : instruction->op == 0xDE                              ? 2
                                                                           : 0;
        if (instruction->op == 0xD9)
            bytes = reg == 6 || reg == 4 ? 28 : (reg == 5 || reg == 7 ? 2 : 4);
        else if (instruction->op == 0xDD)
            bytes = reg == 7 ? 2 : (reg == 4 || reg == 6 ? 108 : 8);
        else if (instruction->op == 0xDB)
            bytes = reg == 5 || reg == 7 ? 10 : 4;
        else if (instruction->op == 0xDF)
            bytes = reg == 5 || reg == 7 ? 8 : 2;
        int store = (instruction->op == 0xD9 && (reg == 2 || reg == 3 || reg == 6 || reg == 7)) ||
                    (instruction->op == 0xDD && (reg == 2 || reg == 3 || reg == 6 || reg == 7)) ||
                    (instruction->op == 0xDB && (reg == 2 || reg == 3 || reg == 7)) ||
                    (instruction->op == 0xDF && (reg == 3 || reg == 7));
        if (bytes)
            emit_memory_guard(17, (uint64_t)bytes, guest_pc, store ? X86_SOFT_WRITE : X86_SOFT_READ);
        if (store && emit_soft_memory_active()) {
            emit_soft_store_commit((uint64_t)bytes);
            e_ldr(17, 28, OFF_BUS_EA);
        }
        e_mov_rr(19, 17, 1);
        int result = lower_x87_memory_state(instruction, guest_pc, next, reg);
        if (result != TX_FALL) return result;
        result = lower_x87_memory_arithmetic(instruction, guest_pc, reg);
        return result == TX_FALL ? TX_NEXT : result;
    }
    int result = lower_x87_d9_register(instruction, guest_pc, next, reg, rm);
    if (result != TX_FALL) return result;
    result = lower_x87_register_arithmetic(instruction, guest_pc, reg, rm);
    if (result != TX_FALL) return result;
    result = lower_x87_register_control(instruction, guest_pc, reg, rm);
    return result == TX_FALL ? TX_NEXT : result;
}

static int lower_sse_float_arithmetic(struct insn I, uint64_t guest_pc, uint64_t next, int vd, int vm) {
    uint8_t op = I.op;
    if (op != 0x58 && op != 0x59 && op != 0x5C && op != 0x5E && op != 0x51 &&
        !((op == 0x52 || op == 0x53) && !I.p66 && !I.repne))
        return TX_FALL;
    // add/mul/sub/div/min/max/sqrt. Prefix selects width: F2=scalar double, F3=scalar
    // single, 66=PACKED double (.2d), none=PACKED single (.4s).
    // 0F 52 RSQRTPS/SS and 0F 53 RCPPS/SS join the UNARY group with sqrt: baseline SSE1,
    // single-precision only (66/F2 are reserved, excluded above -> UNIMPL as before).
    int packed = !I.repne && !I.rep;
    int s = vm;
    if (I.is_mem) {
        if (packed) {
            g_ldr_q_ea(16, &I, next);
        } else {
            emit_ea(&I, next);
            if (emit_soft_memory_active()) emit_memory_guard(17, I.repne ? 8u : 4u, guest_pc, X86_SOFT_READ);
            if (I.repne)
                g_ldr_d(16, 17);
            else
                g_ldr_s(16, 17);
        }
        s = 16;
    }
    int dbl = packed ? I.p66 : I.repne; // element type: double vs single
    int unary = (op == 0x51 || op == 0x52 || op == 0x53);
    // RSQRT/RCP raise NO SIMD FP exception at all (SDM; measured against native for a
    // denormal, an overflow, a zero, a negative and both NaN classes). The FSQRT/FDIV
    // standing in for the hardware table DO raise #D/#O/#P/#Z, so park FPSR across the
    // whole sequence -- the same rule avx.c applies to the VEX forms.
    int park = (op == 0x52 || op == 0x53);
    if (park) emit32(0xD53B4420u | 16); // mrs x16, fpsr
    if (packed && !unary) {
        // ---- packed add/sub/mul/div: RESULT gate ----
        // Replaces the NaN-INPUT gate + emit_dnan_pre/post pair used below (which cost 16
        // host instructions for one guest op -- 30 of the 53 instructions the float_simd
        // inner loop compiled to). The two ways a bare NEON FADD/FSUB/FMUL/FDIV diverges
        // from x86 are (a) a lane with TWO NaN inputs (x86 and ARM select opposite
        // operands) and (b) a GENERATED default NaN (x86's indefinite carries the sign
        // bit, ARM's does not). BOTH are visible in the RESULT: these four ops propagate a
        // NaN operand to a NaN result unconditionally, so "some result lane is NaN" is a
        // sound superset of "this instruction needs the x86-exact path". So do the
        // arithmetic into SCRATCH v18 -- leaving the architectural vd, and hence the
        // R_SSE3B spill, exactly as the guest instruction found it -- test the result, and
        // on any NaN lane exit to the C softmulator, which re-executes the whole
        // instruction from unmodified guest state. Clean results commit with one MOV.
        //   f<op> v18.T, vd.T, s.T
        //   fcmeq v21.T, v18.T, v18.T   ; all-ones per NON-NaN lane
        //   uminv b21,   v21.16b        ; zero iff ANY lane is NaN
        //   fmov  w16,   s21
        //   cbnz  w16,   Lfast
        //   <exit R_SSE3B>
        //   Lfast: mov vd.16b, v18.16b
        // 7 host instructions against the old 16, and bit-identical on both paths: the old
        // fast path required no NaN INPUT, which for these ops implies a non-NaN result
        // except for a generated default NaN -- and that case now routes to C, which is the
        // same value the old emit_dnan_post stamped. v18/v21/w16 are translator scratch
        // (guest xmm0..15 live in v0..v15), so the exit spills the correct architectural
        // state. Scalar ss/sd forms keep the old input gate (their gate is already 6
        // instructions and their fixup is a predicted-not-taken FCMP branch).
        uint32_t d = I.p66 ? 0x00400000u : 0;
        uint32_t b = op == 0x58   ? 0x4E20D400u  // FADD
                     : op == 0x59 ? 0x6E20DC00u  // FMUL
                     : op == 0x5C ? 0x4EA0D400u  // FSUB
                                  : 0x6E20FC00u; // FDIV
        emit32(b | d | (s << 16) | (vd << 5) | 18);
        uint32_t EQ = dbl ? 0x4E60E400u : 0x4E20E400u; // FCMEQ .2d/.4s
        emit32(EQ | (18 << 16) | (18 << 5) | 21);
        emit32(0x6E31A800u | (21 << 5) | 21); // uminv b21, v21.16b
        emit32(0x1E260000u | (21 << 5) | 16); // fmov w16, s21
        uint32_t *p_cbnz = (uint32_t *)g_cp;
        emit32(0);                     // cbnz w16, Lfast (patched below)
        emit_exit_const(guest_pc, R_SSE3B); // any NaN lane -> x86-exact C emulation
        uint8_t *Lfast = (uint8_t *)g_cp;
        *p_cbnz = 0x35000000u | ((uint32_t)(((Lfast - (uint8_t *)p_cbnz) / 4) & 0x7FFFF) << 5) | 16;
        e_vmov(vd, 18);
    } else {
        if (!unary) {
            // ---- NaN-input gate ----
            // NEON FADD/FMUL/FSUB/FDIV + emit_dnan is bit-exact to x86 for finite inputs, for a
            // GENERATED NaN (fixed up below), and for a SINGLE NaN input (propagated + quieted,
            // sign preserved -- both ISAs agree). But when a lane has TWO NaN inputs, x86 selects
            // QNaN-priority-else-src2 while ARM selects SNaN-priority-else-src1 -- the exact
            // mirror, a silent wrong result. Rather than reproduce x86's per-lane priority inline
            // on the hot path, gate: if ANY checked input lane is a NaN, exit to the x86-exact C
            // softmulator (R_SSE3B -> hl_x86_sse_run). Real FP kernels have no NaN inputs, so the
            // fast path below is unaffected. src1 is still live in vd (arith not emitted yet),
            // src2 in s. Scalar ss/sd check ONLY the low lane; packed checks all lanes.
            uint32_t EQ = dbl ? 0x4E60E400u : 0x4E20E400u; // FCMEQ .2d/.4s (all-ones per non-NaN lane)
            emit32(EQ | (vd << 16) | (vd << 5) | 24);      // v24 = (src1==src1)
            emit32(EQ | (s << 16) | (s << 5) | 25);        // v25 = (src2==src2)
            e_v3(0x4E201C00u, 24, 24, 25);                 // v24 = src1nn & src2nn (AND.16b)
            if (packed) { // fold both 64-bit halves -> low 64 = all lanes
                e_ext(25, 24, 24, 8);
                e_v3(0x4E201C00u, 24, 24, 25);
            }
            e_fmov_from_d(16, 24);          // x16 = lane mask (all-ones iff no NaN in checked lanes)
            e_rrr(A_ORN, 16, 31, 16, 1, 0); // x16 = ~mask (0 iff clean; nonzero iff a NaN input)
            uint32_t *p_cbz = (uint32_t *)g_cp;
            emit32(0);                     // cbz {w,x}16, Lfast (patched below)
            emit_exit_const(guest_pc, R_SSE3B); // NaN present -> x86-exact C emulation of this insn
            uint8_t *Lfast = (uint8_t *)g_cp;
            // scalar single checks only the low 32 bits (cbz w16); packed / scalar double check 64 (cbz
            // x16)
            uint32_t cbz = (!packed && !dbl) ? 0x34000000u : 0xB4000000u;
            *p_cbz = cbz | ((uint32_t)(((Lfast - (uint8_t *)p_cbz) / 4) & 0x7FFFF) << 5) | 16;
        }
        int fixnan = fpdnan_on();
        if (fixnan) emit_dnan_pre(vd, s, !unary, dbl); // capture "no input NaN" (uses v20/v21)
        if (packed) {                                  // vector FP: 66 -> .2d (sz bit), none -> .4s
            uint32_t d = I.p66 ? 0x00400000u : 0;
            uint32_t b = op == 0x58   ? 0x4E20D400u  // FADD
                         : op == 0x59 ? 0x6E20DC00u  // FMUL
                         : op == 0x5C ? 0x4EA0D400u  // FSUB
                         : op == 0x5E ? 0x6E20FC00u  // FDIV
                                      : 0x6EA1F800u; // FSQRT (2-reg)  [min/max: see 0x5D/0x5F above]
            if (op == 0x52 || op == 0x53) {
                emit32(0x4F03F600u | 19); // fmov v19.4s, #1.0
                int n = s;
                if (op == 0x52) {
                    emit32(0x6EA1F800u | (s << 5) | 18); // fsqrt v18.4s, s.4s
                    n = 18;
                }
                emit32(0x6E20FC00u | (n << 16) | (19 << 5) | vd); // fdiv vd.4s, v19.4s, vn.4s
            } else if (op == 0x51)
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
            // ADDSS/SD, MULSS/SD, SUBSS/SD, DIVSS/SD and SQRTSS/SD write ONLY the low
            // element; the rest of the destination is architecturally PRESERVED. The ARM
            // scalar forms zero everything above the element, so land the result in
            // scratch v18 (which the default-NaN fixup then stamps) and INS it back.
            if (op == 0x52 || op == 0x53) {
                emit32(0x1E2E1000u | 19); // fmov s19, #1.0
                int n = s;
                if (op == 0x52) {
                    emit32(0x1E21C000u | (s << 5) | 18); // fsqrt s18, s
                    n = 18;
                }
                emit32(0x1E201800u | (n << 16) | (19 << 5) | 18); // fdiv s18, s19, sn
            } else if (op == 0x51)
                emit32(b | ty | (s << 5) | 18); // FSQRT s18/d18, s
            else
                emit32(b | ty | (s << 16) | (vd << 5) | 18); // FADD/... s18/d18, vd, s
        }
        int res = packed ? vd : 18;
        if (fixnan) emit_dnan_post(res, dbl, packed); // stamp x86's negative default-NaN sign
        if (!packed) {
            if (dbl)
                e_ins_d(vd, 0, 18, 0);
            else
                e_ins_s(vd, 0, 18, 0);
        }
    }
    if (park) emit32(0xD51B4420u | 16); // msr fpsr, x16
    return TX_NEXT;
}

// Translate the basic block at guest address gpc; returns host entry pointer.
static void *translate_block(uint64_t gpc) {
    /* Observe writes made through another MAP_SHARED alias before decoding
       an executable view backed by an emulated host-page snapshot. */
    uint64_t source_page = gpc & ~UINT64_C(0xfff);
    filemap_refresh_emulated(source_page, source_page + UINT64_C(0x1000));
    hl_x86_crypto_state crypto_state = {.optimize = !nosseopt()};
    hl_x86_trace_state trace_state = {
        .pending_flags = &g_fl_pending,
        .tier_counters = g_t2cnt,
        .flag_elisions = &g_prof_xflag,
        .flag_scans = &g_prof_xflag_scan,
        .tier_folds = &g_prof_t2fold,
        .materialize_flags = flags_materialize,
        .fix_add_flags = e_nzcv_fix_ci,
        .fix_logic_flags = e_nzcv_fix_c1,
        .emit_chain_exit = emit_chain_exit,
        .page_translated = txpg_has,
        .flag_elision = lazyflags_on(),
        .tier_two = g_tier2_build,
    };
    const int stitch = 1;
    uint64_t start = gpc;
    void *host = g_cp;
    emit_prologue();
    void *body = g_cp;
    // poll cpu->irq at the body entry so a caught async signal reaches a no-syscall guest loop.
    emit_irq_check(start);
    g_fl_pending = FL_NONE; // lazy flags: nothing deferred at block entry
    crypto_state.zero_ready = crypto_state.mask_ready =
        0;                           // crypto constant hoist: no v26==0 / v27==0x8f claim survives a block entry
    g_df = HL_X86_DIRECTION_DYNAMIC; // a prior block's std/popfq may have left it set
    hl_x86_x87_reset();              // x87: top unknown at block entry until a finit anchors it
    g_vmark_done = 0;                // fresh region -> first xmm write must re-mark cpu->vdirty
    // W3-A superblock state: guest block-starts already laid in this region + region budget.
    uint64_t seen[HL_X86_TRACE_MAX_BLOCKS];
    int nseen = 0, trace_blk = 0;
    int ncond = 0; /* opt4: conditional-jcc fall-throughs stitched inline so far in this region */
    seen[nseen++] = start;
    // Exact fault-PC provenance: record, per memory-accessing guest instruction, the host code range it
    // compiled to and its guest RIP, so a synchronous SIGSEGV/SIGBUS inside translated code recovers the
    // EXACT faulting instruction (crash reporters / JIT null-check-elimination read gregs[REG_RIP]).
    // Mirrors the aarch64 translator's provenance map. Deferred-by-one (close the previous insn's host
    // range at the next loop top, once g_cp has advanced past its emitted code); flushed after the loop.
    uint64_t prov_host = 0, prov_guest = 0;
    int prov_mem = 0;
    // opt4 conditional-stitch budget (mirrors the aarch64 translator): each conditional-jcc fall-through
    // laid inline is a SPECULATION -- the guest may instead take the (chain-exit) branch, leaving the
    // inlined tail dead. Deadness compounds per conditional passed (measured on x86 translation-heavy
    // workloads: 22-36% of decoded instructions sit at stitch-depth >= 3). Unconditional `jmp` edges follow
    // the guaranteed path and are NOT budgeted, so straight-line/loop-body traces still stitch freely; only
    // chains of hard-to-predict conditionals are cut. Ending a region early is always semantics-preserving:
    // intermediate block-starts are never registered in g_map, so the truncated successor self-heals as an
    // on-demand fresh translation via the ordinary chain-exit path (identical to NOSTITCH, re-anchored).
#ifndef STITCH_MAX_COND
#define STITCH_MAX_COND 3
#endif
#define STITCH_OK                                                                                                      \
    (stitch && !g_nochain && !g_trace && !g_itrace && trace_blk < HL_X86_TRACE_MAX_BLOCKS - 1 &&                       \
     ncond < STITCH_MAX_COND && (size_t)((uint8_t *)g_cp - (uint8_t *)host) < HL_X86_TRACE_MAX_BYTES)
    for (;;) {
        if (g_itrace && gpc != start) {
            if (g_fl_pending) flags_materialize(); // materialize before boundary
            hl_x86_x87_drop();                     // x87: spill the shadow top before the boundary
            emit_chain_exit(gpc);
            break;
        } // 1 insn/block: per-instruction register dump
        struct insn I;
        g_emit_gpc = gpc; // IRQSLIM: tag chain emission with the current branch's rip
        if (hl_x86_decode(gpc, &I) < 0) {
            /* Logical execute permission/range failure is a guest instruction
               fetch fault, not an engine-side dereference crash. */
            emit_guest_signal(gpc, 11, 2); /* SIGSEGV, SEGV_ACCERR */
            break;
        }
        uint64_t next = gpc + I.len;
        g_emit_next = next;
        if (prov_mem) jit_instruction_map_put(prov_host, (uint64_t)g_cp, prov_guest); // close previous insn
        prov_host = (uint64_t)g_cp;
        prov_guest = gpc;
        prov_mem = I.is_mem; // a memory operand -> this insn can raise a synchronous guest fault
        uint8_t op = I.op;
        int sf = I.opsize == 8;
        int vector_result = lower_vector_family(&I, gpc, next, &crypto_state);
        if (vector_result == TX_NEXT) {
            gpc = next;
            continue;
        }
        if (vector_result == TX_BREAK) break;
        if (g_trace)
            fprintf(stderr, "[dec] %llx %s%02x len=%d mod%d rm%d reg%d mem%d base%d idx%d disp=%lld imm=%lld\n",
                    (unsigned long long)gpc, I.two ? "0F " : "", op, I.len, I.mod, I.rm_reg, I.reg, I.is_mem,
                    I.m_hasbase ? I.m_base : -1, I.m_hasindex ? I.m_index : -1, (long long)I.disp, (long long)I.imm);

        prepare_legacy_flags(&I, gpc, next, &trace_state);

        // x87 static-top tracking ends at any non-x87 instruction: spill the shadow top to
        // cpu->fptop and drop to the runtime-top model (the run only spans consecutive x87 ops, so
        // no top assumption ever crosses a non-x87 op, a branch target, or a block boundary).
        if (hl_x86_x87_known() && !(!I.two && op >= 0xD8 && op <= 0xDF)) hl_x86_x87_drop();

        if (!I.two) {
            int primary_result = lower_primary_fast(&I, gpc, next, &trace_state);
            if (primary_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            if (primary_result == TX_BREAK) break;
            int unary_result = lower_group3_unary(&I, next);
            if (unary_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            int narrow_result = lower_group3_narrow_muldiv(&I, gpc, next);
            if (narrow_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            int wide_result = lower_group3_wide_muldiv(&I, gpc, next);
            if (wide_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            if (wide_result == TX_BREAK) break;
            if (op == 0xF6 || op == 0xF7) {
                report_unimpl(gpc, &I);
                break;
            }
            int group45_result = lower_group45(&I, gpc, next);
            if (group45_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            if (group45_result == TX_BREAK) break;
            int exchange_result = lower_exchange(&I, gpc, next);
            if (exchange_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            int stack_result = lower_stack_control(&I, gpc, next);
            if (stack_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            if (stack_result == TX_BREAK) break;
            int multiply_result = lower_immediate_multiply(&I, gpc, next, &trace_state);
            if (multiply_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            int string_result = lower_primary_string(&I, next, &crypto_state);
            if (string_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            if (string_result == TX_BREAK) break;
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
                if (STITCH_OK && tgt != start && !hl_x86_trace_seen(seen, nseen, tgt) && !map_body(tgt) &&
                    !hl_x86_trace_trap_head(tgt)) {
                    seen[nseen++] = tgt;
                    trace_blk++;
                    gpc = tgt;
                    continue;
                }
                // x86-xflags: chained/exit edge -- materialize unless the successor provably kills
                // the flags first (no-op when nothing is pending).
                hl_x86_trace_flags_edge(&trace_state, tgt, gpc);
                emit_chain_exit(tgt);
                break;
            }
            int direct_loop_result = lower_direct_call_loop(&I, gpc, next, &trace_state);
            if (direct_loop_result == TX_BREAK) break;
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
                if (!parity && taken == start && !notier2x() &&
                    !hl_x86_trace_loop_hazard((uint64_t)body, (uint64_t)g_cp)) {
                    int slot = g_tier2_build ? 0 : t2_slot(start);
                    if (g_tier2_build || slot >= 0) {
                        hl_x86_trace_self_loop(&trace_state, cc, start, next, body, slot);
                        break;
                    }
                }
                uint64_t fall = next;
                int stitch_fall = (STITCH_OK && fall != start && !hl_x86_trace_seen(seen, nseen, fall) &&
                                   !map_body(fall) && !hl_x86_trace_trap_head(fall));
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
                    hl_x86_trace_jcc_flags(&trace_state, taken, fall, gpc, stitch_fall, cc, &save_taken, &save_fall);
                }
                // STITCH: lay the fall-through (`next`) inline; the taken side becomes a tiny
                // out-of-line exit reached by the INVERTED condition. Both arms see canonical live
                // flags; the taken stub spills cpu->nzcv iff its successor may read it. A parity jcc
                // clobbered the live NZCV with its PF scratch -> restore the canonical membank flags
                // on EVERY outgoing edge (parity-edge fix; see emit_parity_jcc_cond).
                if (stitch_fall) {
                    int inv = (cc ^ 1) & 0xF; // not-taken -> branch over the taken exit (x86cc_to_arm is 0..13)
                    uint32_t *patch = (uint32_t *)g_cp;
                    emit32(0);                       // b.inv -> fall (inline)
                    if (parity) e_nzcv_load();       // taken edge: restore canonical live NZCV
                    emit_jcc_edge_spill(save_taken); // FL_SUB (e_nzcv_save) or FL_LOGIC (e_nzcv_save_c1) spill on the
                                                     // flag-live taken edge only
                    emit_chain_exit(taken);
                    int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
                    *patch = 0x54000000u | (((uint32_t)d & 0x7FFFF) << 5) | (uint32_t)inv;
                    if (parity) e_nzcv_load(); // inline fall: restore before continuing
                    seen[nseen++] = fall;
                    trace_blk++;
                    ncond++;
                    gpc = fall;
                    continue;
                }
                uint32_t *patch = (uint32_t *)g_cp;
                emit32(0);                      // b.cond -> taken
                if (parity) e_nzcv_load();      // fall edge: restore canonical live NZCV
                emit_jcc_edge_spill(save_fall); // FL_SUB/FL_LOGIC spill for a flag-live fall successor
                emit_chain_exit(next);
                int64_t d = ((uint8_t *)g_cp - (uint8_t *)patch) / 4;
                *patch = 0x54000000u | (((uint32_t)d & 0x7FFFF) << 5) | (cc & 0xF);
                if (parity) e_nzcv_load();       // taken edge: restore canonical live NZCV
                emit_jcc_edge_spill(save_taken); // FL_SUB/FL_LOGIC spill for a flag-live taken successor
                emit_chain_exit(taken);
                break;
            }
            int flag_register_result = lower_flag_register_transfer(&I);
            if (flag_register_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            int flag_stack_result = lower_flag_stack_control(&I, gpc);
            if (flag_stack_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            if (flag_stack_result == TX_BREAK) break;
            int x87_result = lower_x87_family(&I, gpc, next);
            if (x87_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            if (x87_result == TX_BREAK) break;
            int accumulator_result = lower_accumulator_legacy(&I, sf);
            if (accumulator_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            int one_byte_boundary_result = lower_one_byte_signal_and_lookup(&I, next);
            if (one_byte_boundary_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            if (one_byte_boundary_result == TX_BREAK) break;
        } else {
            // ===== two-byte (0F xx) =====
            int two_byte_boundary_result = lower_two_byte_boundary(&I, gpc, next);
            if (two_byte_boundary_result == TX_BREAK) break;
            // ===== SSE / SSE2 (guest xmm0..15 == host v0..v15) =====
            // mandatory prefix selects the variant: 66=packed-int/double, F3=scalar-single,
            // F2=scalar-double, none=packed-single. reg/rm fields index xmm directly.
            mark_vdirty(); // SSE lowering writes guest xmm (v0..v15) -> mark cpu->V dirty
                int handled = 1;
                int vd = I.reg, vm = I.rm_reg;
                // MMX = the no-prefix form of an integer-SIMD opcode: the SAME operation at 64 bits, on
                // mm0-7, which alias v0..v7's low halves here (see interp.c's register-file comment). Two
                // things carry the width: g_ldr_vec_ea makes the memory operand an 8-byte load, and
                // `mmx_wb` narrows the destination on write-back. Everything between is lane-local, so it
                // computes the right low 64 on the zero-extension and the narrow drops the rest -- the
                // cross-lane arms (punpck, pack, pmulh, pmaddwd, pmovmskb) are the ones that instead need
                // their own 64-bit form below. REX.R/REX.B do not extend an MMX operand, hence the mask.
                int mmx = sse_mmx_capable(op) && !I.p66 && !I.rep && !I.repne;
                if (mmx) {
                    vd &= 7;
                    vm &= 7;
                }
                int mmx_wb = mmx ? vd : -1; // vector register the 64-bit write-back must narrow, or -1
                int move_result = lower_sse_moves(&I, gpc, next, vd, vm, mmx);
                if (move_result == TX_BREAK) break;
                if (move_result == TX_NEXT) {
                    gpc = next;
                    continue;
                }
                int horizontal_result = lower_sse_horizontal(&I, gpc, next, vd, vm);
                if (horizontal_result == TX_NEXT) {
                    gpc = next;
                    continue;
                }
                if ((op == 0x12 || op == 0x16) && I.rep) { // SSE3 movsldup/movshdup
                    int s = vm;
                    if (I.is_mem) {
                        g_ldr_q_ea(16, &I, next);
                        s = 16;
                    }
                    if (op == 0x12)
                        e_v3(0x4E802800u, vd, s, s); // movsldup: TRN1 vd.4s, s, s = [s0,s0,s2,s2]
                    else
                        e_v3(0x4E806800u, vd, s, s); // movshdup: TRN2 vd.4s, s, s = [s1,s1,s3,s3]
                } else if (op == 0x12 && I.repne) {  // movddup: dst[0]=dst[1]=src low 64-bit double
                    int s = vm;
                    if (I.is_mem) {
                        g_ldr_d_ea(16, &I, next); // low 64-bit -> v16.d[0]
                        s = 16;
                    }
                    emit32(0x4E080400u | (s << 5) | vd); // dup vd.2d, vs.d[0]  (broadcast low lane)
                } else if (op == 0x12 || op == 0x16) {   // movlps/movhps (load) or movhlps/movlhps (reg)
                    int lane = (op == 0x16) ? 1 : 0;     // 12->low lane(d[0]), 16->high lane(d[1])
                    if (I.is_mem) {
                        g_ldr_d_ea(16, &I, next);
                        e_ins_d(vd, lane, 16, 0);
                    } else {
                        int srclane = (op == 0x12) ? 1 : 0; // movhlps: d[0]<-src d[1]; movlhps: d[1]<-src d[0]
                        e_ins_d(vd, lane, vm, srclane);
                    }
                } else if (op == 0x13 || op == 0x17) { // movlps/movhps store
                    int lane = (op == 0x17) ? 1 : 0;
                    e_ins_d(16, 0, vd, lane);
                    g_str_d_ea(16, &I, next);
                } else if (op == 0x54 || op == 0x55 || op == 0x56 ||
                           op == 0x57) { // andps/andnps/orps/xorps (FP bitwise)
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) { g_ldr_vec_ea(16, &I, next, mmx); }
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
                    if (I.is_mem) { g_ldr_vec_ea(16, &I, next, mmx); }
                    unsigned im = (unsigned)I.imm;
                    e_vmov(18, vd);
                    e_ins_d(17, 0, 18, im & 1);
                    e_ins_d(17, 1, s, (im >> 1) & 1);
                    e_vmov(vd, 17);
                } else if (op == 0xC6) { // shufps xmm,xmm/m,imm8 (lanes 0,1 from dst; 2,3 from src)
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) { g_ldr_vec_ea(16, &I, next, mmx); }
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
                        sh = (int)(I.imm & 0xff), x = vm; // the shift's destination is r/m, NOT reg
                    mmx_wb = mmx ? x : -1;                // ...so the MMX narrow must follow it there
                    if (sub == 2)
                        e_vshr_imm(x, x, esz, sh, 0); // psrl
                    else if (sub == 4)
                        e_vshr_imm(x, x, esz, sh, 1); // psra
                    else if (sub == 6)
                        e_vshl_imm(x, x, esz, sh);                           // psll
                    else if (op == 0x73 && (sub == 3 || sub == 7) && !mmx) { // psrldq / pslldq (66 only; #UD bare)
                        if (sh > 15) {                                       // x86: count > 15 -> result is all-zero
                            e_v3(0x6E201C00u, x, x, x);
                        } else if (sh) { // count 0 is the identity -> emit nothing
                            if (!crypto_state.zero_ready || nosseopt())
                                e_v3(0x6E201C00u, 26, 26, 26); // hoisted zero (crypto.c claim)
                            crypto_state.zero_ready = 1;
                            if (sub == 3)
                                e_ext(x, x, 26, sh); // psrldq
                            else
                                e_ext(x, 26, x, 16 - sh); // pslldq
                        }
                    } else {
                        report_unimpl(gpc, &I);
                        break;
                    }
                } else if (lower_sse_shuffle(&I, next, vd, vm, mmx) == TX_NEXT) {
                } else if (lower_sse_packed_binary(&I, next, vd, vm, mmx) == TX_NEXT) {
                    // The helper emitted the complete lane-wise operation.
                } else if (lower_sse_widening_multiply(&I, next, vd, vm, mmx) == TX_NEXT) {
                    // The helper emitted the complete widening multiply operation.
                } else if (op == 0xF1 || op == 0xF2 || op == 0xF3 || op == 0xD1 || op == 0xD2 || op == 0xD3 ||
                           op == 0xE1 || op == 0xE2) { // psll/psrl/psra w/d/q by xmm/m (variable count)
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) { g_ldr_vec_ea(16, &I, next, mmx); }
                    int left = (op == 0xF1 || op == 0xF2 || op == 0xF3);
                    int arith = (op == 0xE1 || op == 0xE2);
                    int esize = (op == 0xF1 || op == 0xD1 || op == 0xE1)   ? 16
                                : (op == 0xF2 || op == 0xD2 || op == 0xE2) ? 32
                                                                           : 64;
                    e_sse_var_shift(vd, vd, s, esize, left, arith);
                } else if (op == 0x14 || op == 0x15) { // unpckl/hp{s,d}: interleave float lanes -> ZIP1/ZIP2
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) { g_ldr_vec_ea(16, &I, next, mmx); }
                    int hi = (op == 0x15);  // unpckh* -> ZIP2
                    int sz = I.p66 ? 3 : 2; // 66=pd (64-bit lanes, .2d); none=ps (32-bit lanes, .4s)
                    uint32_t b = (hi ? 0x4E007800u : 0x4E003800u) | ((uint32_t)sz << 22);
                    e_v3(b, vd, vd, s);
                } else if (op == 0xE6 && I.rep) { // cvtdq2pd (F3): low 2 packed s32 -> 2 packed f64
                    int s = vm;
                    if (I.is_mem) {
                        g_ldr_d_ea(16, &I, next);
                        s = 16;
                    }
                    emit32(0x0F20A400u | (s << 5) | 16);  // SXTL v16.2d, vs.2s  (sign-extend the 2 int32)
                    emit32(0x4E61D800u | (16 << 5) | vd); // SCVTF vd.2d, v16.2d (int64 -> double)
                } else if (op == 0xE6 && (I.p66 || I.repne)) {
                    // cvttpd2dq (66, truncate) / cvtpd2dq (F2, rounds by MXCSR.RC): 2 packed f64 -> 2 packed
                    // s32 in the low 64 bits of dst; the high 64 bits are zeroed (the Q=0 XTN does that).
                    // Shares emit_pd2i32_pieces with the VEX form, which is the point: this arm used to emit
                    // FCVTNS unconditionally and so ignored MXCSR.RC while VEX honoured it -- legacy and VEX
                    // disagreeing with each other, on top of both being wrong about #I and #P.
                    int s = vm;
                    if (I.is_mem) {
                        g_ldr_q_ea(16, &I, next);
                        s = 16;
                    }
                    int trunc = I.p66 != 0;
                    e_movconst(19, 0x41E0000000000000ull);
                    emit32(0x4E080C00u | (19 << 5) | 25); // v25.2d = 2^31 (f64)
                    e_movconst(19, 0xC1E0000000000000ull);
                    emit32(0x4E080C00u | (19 << 5) | 26); // v26.2d = -2^31
                    e_movconst(19, 0x80000000ull);
                    emit32(0x0E040C00u | (19 << 5) | 27);                 // v27.2s = integer indefinite
                    emit_pd2i32_pieces(22, 18, s, trunc, 25, 26, 28, 21); // v22 = int64 lanes, v18 = fixup mask
                    emit32(0x0EA12800u | (22 << 5) | 24);                 // XTN v24.2s, v22.2d (result)
                    emit32(0x0EA12800u | (18 << 5) | 25);                 // XTN v25.2s, v18.2d (mask)
                    emit32(0x2E601C00u | (24 << 16) | (27 << 5) | 25);    // BSL v25.8b -> mask?indef:result
                    e_vmov(vd, 25);
                } else if (op == 0x60 || op == 0x61 || op == 0x62 || op == 0x6C || op == 0x68 || op == 0x69 ||
                           op == 0x6A || op == 0x6D) { // punpck l/h bw/wd/dq/qdq -> ZIP1/ZIP2
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) { g_ldr_vec_ea(16, &I, next, mmx); }
                    int hi = (op == 0x68 || op == 0x69 || op == 0x6A || op == 0x6D); // punpckh*; 0x6C(lqdq) is LOW
                    int sz = (op == 0x60 || op == 0x68)   ? 0
                             : (op == 0x61 || op == 0x69) ? 1
                             : (op == 0x62 || op == 0x6A) ? 2
                                                          : 3;
                    if (mmx && sz == 3) { // 0F 6C/6D have no MMX form; #UD, and .1D would be a reserved ZIP
                        report_unimpl(gpc, &I);
                        break;
                    }
                    uint32_t b = (hi ? 0x4E007800u : 0x4E003800u) | ((uint32_t)sz << 22);
                    // Q=0 is the whole MMX fix: ZIP1/ZIP2 .8B/.4H/.2S interleave the halves of a 64-bit
                    // operand, which is exactly punpckl/h at MMX width (Q=1 ZIP2 reads bytes 8..15 instead).
                    if (mmx) b &= ~0x40000000u;
                    e_v3(b, vd, vd, s);
                } else if (op == 0x67 || op == 0x63 || op == 0x6B) {
                    // pack with saturation: 0x67 PACKUSWB (16->u8), 0x63 PACKSSWB (16->s8),
                    // 0x6B PACKSSDW (32->s16). dst.low half from dst's lanes, dst.high half from src's.
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) { g_ldr_vec_ea(16, &I, next, mmx); }
                    uint32_t sz = (op == 0x6B) ? 1u : 0u;     // source element: 0x6B = 16-bit, else 8-bit dest
                    uint32_t lo = (op == 0x67) ? 0x2E212800u  // SQXTUN  (signed->unsigned narrow)
                                               : 0x0E214800u; // SQXTN   (signed->signed narrow)
                    uint32_t hi = (op == 0x67) ? 0x6E212800u : 0x4E214800u; // ...2 (Q=1, high half)
                    if (mmx) {
                        // MMX packs 4+4 source lanes into 8 bytes, so both operands' lanes must first sit
                        // in ONE 128-bit register: ZIP1 .2D concatenates the two 64-bit halves, then a
                        // single Q=0 narrow yields all 8 result bytes. (The Q=1 pair above narrows 8 of
                        // dst's lanes and 8 of src's -- at MMX width those upper lanes do not exist.)
                        emit32(0x4EC03800u | (s << 16) | (vd << 5) | 17); // zip1 v17.2d, vd.2d, s.2d
                        emit32(lo | (sz << 22) | (17 << 5) | vd);
                    } else {
                        emit32(lo | (sz << 22) | (vd << 5) | 17); // narrow dst's lanes -> v17 low
                        emit32(hi | (sz << 22) | (s << 5) | 17);  // narrow src's lanes -> v17 high
                        e_vmov(vd, 17);
                    }
                } else if (op == 0xD7) {
                    mmx_wb = -1;
                    (void)lower_sse_sign_mask(&I, vm, mmx);
                } else if (lower_mmx_fp_conversion(&I, next, vd, vm) == TX_NEXT) {
                    mmx_wb = -1;
                } else if (op == 0x2A) { // cvtsi2sd/ss: int r/m -> xmm (F2=double,F3=single)
                    int src;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        if (emit_soft_memory_active()) emit_memory_guard(17, I.rexW ? 8u : 4u, gpc, X86_SOFT_READ);
                        e_load(I.rexW ? 8 : 4, 16, 17);
                        src = 16;
                    } else
                        src = I.rm_reg;
                    // CVTSI2SS/CVTSI2SD write ONLY the low element and preserve the rest of the
                    // destination register, so convert into scratch v18 and merge lane 0 back.
                    emit32(0x1E220000u | (I.rexW ? 0x80000000u : 0) | (I.repne ? 0x00400000u : 0) | (src << 5) |
                           18); // scvtf d18/s18, src
                    if (I.repne)
                        e_ins_d(vd, 0, 18, 0);
                    else
                        e_ins_s(vd, 0, 18, 0);
                } else if (op == 0x2C || op == 0x2D) { // cvttsd2si(2C trunc)/cvtsd2si(2D round): xmm/m -> GPR
                    int s = vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        if (emit_soft_memory_active()) emit_memory_guard(17, I.repne ? 8u : 4u, gpc, X86_SOFT_READ);
                        if (I.repne)
                            g_ldr_d(16, 17);
                        else
                            g_ldr_s(16, 17);
                        s = 16;
                    }
                    int s0 = s;       // the source, before 0x2D rounds it (the #P probe below needs it)
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
                        uint64_t thr = I.repne ? (sf ? 0x43E0000000000000ull : 0x41E0000000000000ull) // dbl 2^63/2^31
                                               : (sf ? 0x5F000000ull : 0x4F000000ull);                // sgl 2^63/2^31
                        e_movconst(20, thr);
                        if (I.repne)
                            e_fmov_to_d(19, 20);
                        else
                            e_fmov_to_s(19, 20); // v19 = threshold
                        // CVTTSD2SI is not an x86 flag producer, but this FCMP writes the ARM NZCV
                        // -- which may still hold a deferred x86 flag producer's result. Save and
                        // restore it, so a later jcc/setcc/cmov/adc sees the integer flags it must.
                        // (The old comment claimed the top-of-loop had already flushed g_fl_pending
                        // here; a `cmp`; `cvttsd2si`; `js` sequence shows that it had not.)
                        uint32_t fcmp = I.repne ? 0x1E602000u : 0x1E202000u;
                        emit32(0xD53B4200u | 21);             // mrs x21, nzcv
                        emit32(fcmp | (19 << 16) | (s << 5)); // FCMP s, v19
                        if (op == 0x2D) {
                            // #P. FCVTZS above reports it for 0x2C (an in-range inexact truncation) but not
                            // for 0x2D, whose FRINTI value is already integral -- and FRINTI itself reports
                            // nothing. FRINTX would, but it also reports #P for an OUT-OF-RANGE inexact
                            // source, where x86 raises #I alone; f64 -> int32 is the one width pair where
                            // such a value exists, and 2147483648.25 is the case that proves it. So run
                            // FRINTX over the source with the out-of-range case replaced by +0.0, which is
                            // exact. Out of range is CS from the FCMP above (>= +thr, or NaN) or MI against
                            // -thr; the result path only needs the former, because negative overflow lands
                            // on FCVTZS's INT_MIN == the indefinite, but #P must be suppressed for both.
                            emit32(0xDA9F33E0u | 22);                                       // csetm x22, cs
                            emit32((I.repne ? 0x1E614000u : 0x1E214000u) | (19 << 5) | 20); // FNEG v20, v19 (-thr)
                            emit32(fcmp | (20 << 16) | (s << 5));                           // FCMP s, -thr
                            emit32(0xDA9F53E0u | 23);                                       // csetm x23, mi
                            e_rrr(A_ORR, 22, 22, 23, 1, 0);                                 // x22 = out-of-range mask
                            e_fmov_to_d(20, 22);
                            e_v3(0x0E601C00u, 20, s0, 20);                                  // BIC v20 = src & ~mask
                            emit32((I.repne ? 0x1E674000u : 0x1E274000u) | (20 << 5) | 20); // FRINTX v20 -> #P only
                            emit32(fcmp | (19 << 16) | (s << 5)); // redo FCMP: the CSEL below reads its NZCV
                        }
                        e_movconst(20, sf ? 0x8000000000000000ull : 0x80000000ull); // integer indefinite
                        e_csel(I.reg, 20, I.reg, 2 /*CS: s>=thr or NaN*/, sf);
                        emit32(0xD51B4200u | 21); // msr nzcv, x21
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
                        if (packed) {
                            g_ldr_q_ea(16, &I, next);
                        } else {
                            emit_ea(&I, next);
                            if (emit_soft_memory_active()) emit_memory_guard(17, I.repne ? 8u : 4u, gpc, X86_SOFT_READ);
                            if (I.repne)
                                g_ldr_d(16, 17);
                            else
                                g_ldr_s(16, 17);
                        }
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
                        // MINSS/MINSD/MAXSS/MAXSD write ONLY the low element; bits 127:64 (127:32 for
                        // the ss forms) of the destination are architecturally PRESERVED. Merge the
                        // low lane back with INS rather than FMOV, which would zero the upper bits.
                        if (I.repne)
                            e_ins_d(vd, 0, 17, 0);
                        else
                            e_ins_s(vd, 0, 17, 0);
                    }
                } else if (lower_sse_float_arithmetic(I, gpc, next, vd, vm) == TX_NEXT) {
                    // The helper emitted the complete packed or scalar floating-point operation.
                } else if (op == 0x5A) {
                    // 0F 5A is FOUR instructions, selected by the mandatory prefix:
                    //   F2 cvtsd2ss   F3 cvtss2sd   66 cvtpd2ps (PACKED)   none cvtps2pd (PACKED)
                    // The two PACKED forms used to fall into the `else` arm and were lowered as
                    // cvtss2sd -- i.e. legacy (non-VEX) CVTPS2PD/CVTPD2PS produced a single
                    // converted low element and garbage everywhere else.
                    int packed = !I.repne && !I.rep;
                    int s = vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        if (emit_soft_memory_active())
                            emit_memory_guard(17, I.rep ? 4u : (packed && I.p66) ? 16u : 8u, gpc, X86_SOFT_READ);
                        if (I.rep)
                            g_ldr_s(16, 17); // cvtss2sd: m32
                        else if (packed && I.p66)
                            g_ldr_q(16, 17, 0); // cvtpd2ps: m128
                        else
                            g_ldr_d(16, 17); // cvtsd2ss: m64 ; cvtps2pd: m64 (two floats)
                        s = 16;
                    }
                    if (packed) {
                        if (I.p66)
                            emit32(0x0E616800u | (s << 5) | vd); // FCVTN vd.2s, s.2d -- upper 64 zeroed
                        else
                            emit32(0x0E617800u | (s << 5) | vd); // FCVTL vd.2d, s.2s
                    } else if (I.repne) {
                        // The scalar forms write ONLY the low element (32 bits for cvtsd2ss, 64 for
                        // cvtss2sd) and preserve the rest of the destination: convert, then merge.
                        emit32(0x1E624000u | (s << 5) | 18); // FCVT S18, Dn (double->single)
                        e_ins_s(vd, 0, 18, 0);
                    } else {
                        emit32(0x1E22C000u | (s << 5) | 18); // FCVT D18, Sn (single->double)
                        e_ins_d(vd, 0, 18, 0);
                    }
                } else if (op == 0xC4) { // pinsrw: insert low 16 bits of r/m16 into xmm H-lane (imm8 & 7)
                    int lane = (int)I.imm & (mmx ? 3 : 7); // mm has 4 words: hardware wraps $4 to lane 0
                    int src;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        if (emit_soft_memory_active()) emit_memory_guard(17, 2, gpc, X86_SOFT_READ);
                        e_load(2, 16, 17); // w16 = [addr] (16-bit)
                        src = 16;
                    } else {
                        src = I.rm_reg; // guest GPR mapped to host reg
                    }
                    // INS vd.H[lane], Wsrc  (imm5 = lane<<2 | 0b10 selects H)
                    emit32(0x4E001C00u | ((((unsigned)lane << 2) | 2u) << 16) | (src << 5) | vd);
                } else if (op == 0xC5) { // pextrw: extract xmm H-lane (imm8 & 7) -> r32, zero-extended (reg src only)
                    int lane = (int)I.imm & (mmx ? 3 : 7); // mm has 4 words (see pinsrw)
                    mmx_wb = -1;                           // destination is a GPR
                    // UMOV Wreg, Vm.H[lane]  (imm5 = lane<<2 | 0b10 selects H; zero-extends into the GPR)
                    emit32(0x0E003C00u | ((((unsigned)lane << 2) | 2u) << 16) | (vm << 5) | I.reg);
                } else if (op == 0xC2) { // cmpps/pd/ss/sd: FP compare with predicate imm -> all-1s/0 mask
                    int packed = !I.repne && !I.rep;
                    int s = vm;
                    if (I.is_mem) {
                        if (packed) {
                            g_ldr_q_ea(16, &I, next);
                        } else {
                            emit_ea(&I, next);
                            if (emit_soft_memory_active()) emit_memory_guard(17, I.repne ? 8u : 4u, gpc, X86_SOFT_READ);
                            if (I.repne)
                                g_ldr_d(16, 17);
                            else
                                g_ldr_s(16, 17);
                        }
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
                    // CMPSS/CMPSD write ONLY the low element and preserve the rest of the
                    // destination, but the ARM scalar FCMxx/NOT forms zero everything above the
                    // element. So scalar results are built in v18 and inserted back into lane 0.
                    int res = packed ? vd : 18;
                    if (pred == 3 || pred == 7) {                       // UNORD/ORD: ordered(a)&ordered(b)
                        emit32(EQ | (vd << 16) | (vd << 5) | 17);       // v17 = a==a (ordered a)
                        emit32(EQ | (s << 16) | (s << 5) | res);        // res = b==b (ordered b)
                        emit32(ANDb | (17 << 16) | (res << 5) | res);   // res = ORD
                        if (pred == 3) emit32(NOTb | (res << 5) | res); // UNORD = ~ORD
                    } else {
                        // predicates handled here: 0 EQ, 1 LT, 2 LE, 4 NEQ, 5 NLT, 6 NLE.
                        // LT/LE/NLT/NLE build the ordered comparison a<b / a<=b via the swapped GT/GE (a<b ==
                        // b>a); NEQ/NLT/NLE then invert. x86's N-forms are UNORDERED: they return all-ones when
                        // an operand is NaN. ARM FCMGT/FCMGE give 0 on NaN, so inverting the ordered result (NOT)
                        // yields the correct NaN->true mask for NLT/NLE (H12) exactly as it already did for NEQ.
                        int lt_like = (pred == 1 || pred == 2 || pred == 5 || pred == 6);
                        int use_ge = (pred == 2 || pred == 6);           // LE/NLE -> GE ; LT/NLT -> GT
                        int neg = (pred == 4 || pred == 5 || pred == 6); // NEQ/NLT/NLE invert (NaN -> true)
                        int n = lt_like ? s : vd, m = lt_like ? vd : s;
                        uint32_t fc = (pred == 0 || pred == 4) ? EQ : use_ge ? GE : GT;
                        emit32(fc | (m << 16) | (n << 5) | res);  // FCMxx res, n, m
                        if (neg) emit32(NOTb | (res << 5) | res); // invert -> NaN lane becomes all-ones
                    }
                    if (!packed) { // merge the scalar lane back
                        if (I.repne)
                            e_ins_d(vd, 0, res, 0); // cmpsd: bits 63:0 only
                        else
                            e_ins_s(vd, 0, res, 0); // cmpss: bits 31:0 only
                    }
                } else if (op == 0x2E || op == 0x2F) { // ucomisd/comisd (66=double, none=single) -> FCMP + flags
                    int s = vm;
                    if (I.is_mem) {
                        emit_ea(&I, next);
                        if (emit_soft_memory_active()) emit_memory_guard(17, I.p66 ? 8u : 4u, gpc, X86_SOFT_READ);
                        if (I.p66)
                            g_ldr_d(16, 17);
                        else
                            g_ldr_s(16, 17);
                        s = 16;
                    }
                    // COMISS/COMISD (0x2F) is the SIGNALING ordered compare: it raises Invalid (IE)
                    // on ANY NaN operand, including qNaN. UCOMISS/UCOMISD (0x2E) is quiet: IE only for
                    // sNaN. Map 0x2F -> FCMPE (bit4 set) and 0x2E -> FCMP. EFLAGS result is identical
                    // for both (unordered -> N0 Z0 C1 V1), so the fixup below is unchanged.
                    emit32((I.p66 ? 0x1E602000u : 0x1E202000u) | (op == 0x2F ? 0x10u : 0u) | (s << 16) |
                           (vd << 5));   // FCMP/FCMPE Dvd, Ds  (Rd=0)
                    e_nzcv_save_fcmp();  // unordered fixup: x86 ZF=PF=CF=1, SF=0 (ARM FCMP gives N0 Z0 C1 V1)
                } else if (op == 0xF4) { // pmuludq: vd.u64[i] = (u32)vd.even32[i] * (u32)src.even32[i]
                    // W3b: was UNIMPL -> blocked glibc strchr/strrchr (byte-broadcast via pmuludq).
                    // Gather the even (0,2) 32-bit lanes of each operand into the low 2 lanes (UZP1),
                    // then widening multiply -> two 64-bit products. Bit-exact, 3 NEON insns.
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) { g_ldr_vec_ea(16, &I, next, mmx); }
                    emit32(0x4E801800u | (vd << 16) | (vd << 5) | 17); // uzp1 v17.4s, vd.4s, vd.4s -> [d0,d2,..]
                    emit32(0x4E801800u | (s << 16) | (s << 5) | 18);   // uzp1 v18.4s, s.4s,  s.4s  -> [s0,s2,..]
                    emit32(0x2EA0C000u | (18 << 16) | (17 << 5) | vd); // umull vd.2d, v17.2s, v18.2s
                } else if (op == 0x50) {
                    (void)lower_sse_sign_mask(&I, vm, mmx);
                } else if (op == 0x5B) { // cvtdq2ps(NP)/cvtps2dq(66)/cvttps2dq(F3): packed 4-lane int<->float
                    int s = vm;
                    if (I.is_mem) {
                        g_ldr_q_ea(16, &I, next);
                        s = 16;
                    }
                    if (I.rep || I.p66) {
                        // Same emit as the VEX form: 66 cvtps2dq used to emit FCVTNS unconditionally and so
                        // ignored MXCSR.RC, while vcvtps2dq honoured it. emit_ps2dq_128 builds the
                        // "make-indefinite" mask from the SOURCE floats before converting, which the
                        // in-place `cvttps2dq %xmm7,%xmm7` form requires: reading it back from the integer
                        // result would see an all-ones lane (-1) as a NaN, and the indefinite (== -0.0f) as
                        // ordered.
                        e_movconst(19, 0x4F000000u);
                        emit32(0x4E040C00u | (19 << 5) | 25); // v25.4s = 2^31 (f32)
                        e_movconst(19, 0x80000000u);
                        emit32(0x4E040C00u | (19 << 5) | 26); // v26.4s = integer indefinite
                        emit_ps2dq_128(17, s, I.rep != 0, 25, 26, 27, 28);
                        e_vmov(vd, 17);
                    } else {
                        emit32(0x4E21D800u | (s << 5) | vd); // NP: cvtdq2ps -> SCVTF .4S (s32->f32)
                    }
                } else if (op == 0xF6) { // psadbw (66): sum of abs byte diffs per 64-bit half
                    int s = I.is_mem ? 16 : vm;
                    if (I.is_mem) { g_ldr_vec_ea(16, &I, next, mmx); }
                    emit32(0x6E207400u | (s << 16) | (vd << 5) | 17); // uabd   v17.16b, vd.16b, s.16b
                    emit32(0x6E202800u | (17 << 5) | 17);             // uaddlp v17.8h,  v17.16b
                    emit32(0x6E602800u | (17 << 5) | 17);             // uaddlp v17.4s,  v17.8h
                    emit32(0x6EA02800u | (17 << 5) | 17);             // uaddlp v17.2d,  v17.4s
                    e_vmov(vd, 17);
                } else if (op == 0xE7 && I.p66) { // movntdq (66): non-temporal store xmm -> m128
                    g_str_q_ea(vd, &I, next);
                } else if (op == 0xF7 && I.p66) { // maskmovdqu (66): per-byte masked store xmm(vd) -> [RDI],
                    // mask = xmm(vm); only each mask byte's MSB selects. Read-modify-write blend at [RDI]
                    // (the region is writable; unselected bytes keep their memory value == architecturally
                    // "not stored"). sel = sshr(mask,#7) -> 0xFF where store; BSL sel?src:mem; store back.
                    e_vshr_imm(18, vm, 8, 7, 1); // sshr v18.16b, vmask.16b, #7
                    e_mov_rr(17, RDI, 1);        // x17 = RDI (guest addr == host addr, in-process)
                    emit_memory_guard(17, 16, gpc, X86_SOFT_READ | X86_SOFT_WRITE);
                    g_ldr_q(16, 17, 0);            // v16 = [RDI]
                    e_v3(0x6E601C00u, 18, vd, 16); // bsl v18.16b, vsrc.16b, v16.16b (sel?src:mem)
                    g_str_q(18, 17, 0);            // [RDI] = blended
                    if (emit_soft_memory_active()) emit_soft_store_commit(16);
                } else if (op == 0x2B && I.is_mem) { // movntps (NP) / movntpd (66): non-temporal store xmm -> m128
                    g_str_q_ea(vd, &I, next);        // aligned, non-temporal -> a plain 128-bit store on ARM
                } else
                    handled = 0;
                if (handled) {
                    // MMX write-back: mm is 64 bits, so drop bits 127:64 that the arms above computed at
                    // NEON's 128-bit width. Without this a `paddb %mm0,%mm0` clobbers xmm0's high half,
                    // which the aliasing register model makes guest-visible.
                    if (mmx_wb >= 0) e_vmov8(mmx_wb, mmx_wb);
                    gpc = next;
                    continue;
                }
            int system_query_result = lower_system_query(&I, next);
            if (system_query_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            if (system_query_result == TX_BREAK) break;
            int scalar_two_byte_result = lower_scalar_two_byte(&I, gpc, next, sf, &trace_state);
            if (scalar_two_byte_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            int wide_compare_result = lower_wide_compare_exchange(&I, gpc, next);
            if (wide_compare_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            if (wide_compare_result == TX_BREAK) break;
            int hint_result = lower_multibyte_hint(&I);
            if (hint_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            int double_shift_result = lower_double_shift(&I, next);
            if (double_shift_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            // 0F AE: fences (lfence/mfence/sfence -> dmb), ldmxcsr/stmxcsr, fxsave/fxrstor (xmm area)
            int extended_state_result = lower_extended_state(&I, gpc, next);
            if (extended_state_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            if (extended_state_result == TX_BREAK) break;
            int bit_scan_result = lower_bit_scan(&I, next, sf);
            if (bit_scan_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            int population_result = lower_population_count(&I, next, sf);
            if (population_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            int bit_modify_result = lower_bit_test_modify(&I, gpc, next, sf);
            if (bit_modify_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            if (bit_modify_result == TX_BREAK) break;
            int compare_exchange_result = lower_compare_exchange(&I, gpc, next);
            if (compare_exchange_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            int exchange_add_result = lower_exchange_add(&I, gpc, next);
            if (exchange_add_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            if ((op & 0xF0) == 0x80) {
                struct near_branch_context near_context = {
                    .trace = &trace_state,
                    .seen = seen,
                    .seen_count = &nseen,
                    .block_count = &trace_blk,
                    .condition_count = &ncond,
                    .stitch_ok = STITCH_OK,
                    .start = start,
                    .body = body,
                };
                int near_branch_result = lower_near_conditional_branch(&I, &gpc, next, &near_context);
                if (near_branch_result == TX_NEXT) continue;
                if (near_branch_result == TX_BREAK) break;
            }
            int conditional_move_result = lower_conditional_data_move(&I, gpc, next, sf);
            if (conditional_move_result == TX_NEXT) {
                gpc = next;
                continue;
            }
            if (conditional_move_result == TX_BREAK) break;
        }
        report_unimpl(gpc, &I);
        break;
    }
    if (prov_mem) jit_instruction_map_put(prov_host, (uint64_t)g_cp, prov_guest); // close the final insn
    // IRQSLIM: the out-of-line poll exit stub the body-entry cbnz targets (irq set -> exit to
    // the dispatcher at the block start, exactly like the legacy inline poll).
    if (g_irq_patch) {
        uint32_t *p = g_irq_patch;
        g_irq_patch = NULL;
        *p = 0xB5000000u | (((uint32_t)(((uint8_t *)g_cp - (uint8_t *)p) / 4) & 0x7FFFF) << 5) | 16; // cbnz x16
        emit_exit_const(start, R_BRANCH);
    }
    // W5B tier-2: the promoter (g_tier2_build) recompiles in place and updates the EXISTING map entry
    // itself, so don't insert a duplicate and don't chain pending edges here (the promoter does both
    // AFTER icache-flushing the new code). Expose the body for it.
    g_last_body = body;
    if (!g_tier2_build) {
        map_put(start, start, gpc > start ? gpc : start + 1, host, body);
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
    if (!jit_wprot(0)) return;
    g_emit_start = g_cp;
    g_tier2_build = 1;
    void *nh = translate_block(gpc); // folded recompile; no counter, no map_put, no chain
    void *nb = g_last_body;
    g_tier2_build = 0;
    // make the tier-2 code coherent BEFORE anything can branch into it
    if (!jit_publish_code(g_emit_start, (size_t)(g_cp - g_emit_start))) {
        (void)jit_wprot(1);
        return;
    }
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
    if (!jit_publish_code(old_body, 4 + (g_fwdskip ? 8 : 0))) {
        (void)jit_wprot(1);
        return;
    }
    // swap the live map entry: future dispatcher lookups + IBTC fills resolve to tier-2 directly
    g_map[mi].host = nh;
    g_map[mi].body = nb;
    patch_links_to(gpc, nb); // repoint any still-unresolved chains to this gpc straight at tier-2
    uint32_t h = (uint32_t)((gpc >> 2) & (IBTC_N - 1)); // drop a stale IBTC entry (refills to tier-2)
    if (g_ibtc[h].target == gpc) {
        g_ibtc[h].target = 0;
        g_ibtc[h].body = NULL;
    }
    if (!jit_wprot(1)) return;
    g_prof_t2++;
}

static void report_unimpl(uint64_t pc, struct insn *I) {
    const uint8_t *p = (const uint8_t *)pc;
    fprintf(stderr, "[hl] UNIMPL %s opcode 0x%02x at rip=%llx  bytes:", I->two ? "0F" : "1B", I->op,
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
// The arch test is as load-bearing as the compiler test: both arms below are AArch64 assembly, and the
// guard once selected between them on the COMPILER alone. Same macro core/dispatch.c gates its copy on.
#if defined(__GNUC__) && !defined(__clang__) && defined(HL_HOST_CPU_AARCH64)
/* GCC ignores naked on AArch64 functions.  Define the two ABI trampolines as
   assembler functions so no compiler-generated prologue can corrupt SP or the
   callee-saved register image. */
extern void run_block(struct cpu *cpu, void *code) __attribute__((visibility("hidden")));
extern void block_return(void) __attribute__((visibility("hidden")));
__asm__(".hidden run_block\n"
        ".type run_block, %function\n"
        "run_block:\n"
        "str x19,[x0,#176]\n str x20,[x0,#184]\n str x21,[x0,#192]\n str x22,[x0,#200]\n"
        "str x23,[x0,#208]\n str x24,[x0,#216]\n str x25,[x0,#224]\n str x26,[x0,#232]\n"
        "str x27,[x0,#240]\n str x28,[x0,#248]\n str x29,[x0,#256]\n str x30,[x0,#264]\n"
        "str q8,[x0,#272]\n str q9,[x0,#288]\n str q10,[x0,#304]\n str q11,[x0,#320]\n"
        "str q12,[x0,#336]\n str q13,[x0,#352]\n str q14,[x0,#368]\n str q15,[x0,#384]\n"
        "mov x9,sp\n str x9,[x0,#168]\n br x1\n"
        ".size run_block, .-run_block\n"
        ".hidden block_return\n"
        ".type block_return, %function\n"
        "block_return:\n"
        "ldr x19,[x28,#176]\n ldr x20,[x28,#184]\n ldr x21,[x28,#192]\n ldr x22,[x28,#200]\n"
        "ldr x23,[x28,#208]\n ldr x24,[x28,#216]\n ldr x25,[x28,#224]\n ldr x26,[x28,#232]\n"
        "ldr x27,[x28,#240]\n ldr x29,[x28,#256]\n ldr x30,[x28,#264]\n"
        "ldr q8,[x28,#272]\n ldr q9,[x28,#288]\n ldr q10,[x28,#304]\n ldr q11,[x28,#320]\n"
        "ldr q12,[x28,#336]\n ldr q13,[x28,#352]\n ldr q14,[x28,#368]\n ldr q15,[x28,#384]\n"
        "ldr x9,[x28,#168]\n mov sp,x9\n ldr x28,[x28,#248]\n ret\n"
        ".size block_return, .-block_return\n");
#elif defined(HL_HOST_CPU_AARCH64)
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
#else
// Non-AArch64 host: the emitters here write ARM64, so no trampoline can enter anything. These exist only
// so the engine links -- block_return's ADDRESS is baked into emitted blocks and anchors cache.c's image
// slide, and dispatch.c CALLS run_block. `static` matches emit.c's declaration and keeps the dual
// archive's two definitions from colliding (findings 3.7). Abort: reaching either is a build error.
static void run_block(struct cpu *cpu, void *code) {
    (void)cpu;
    (void)code;
    fprintf(stderr, "[hl] x86-64 guest: no host back end for " HL_HOST_CPU_NAME " (the emitters target arm64)\n");
    abort();
}

static void block_return(void) {
    fprintf(stderr, "[hl] x86-64 guest: no host back end for " HL_HOST_CPU_NAME " (the emitters target arm64)\n");
    abort();
}
#endif
