#include "integer.h"
#include "arithmetic.h"
#include "primitives.h"
#include "trace.h"
#include "../glue.h"
#include "../cpu.h"
#include "../encoding.h"

#include <string.h>

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
// helpers in translate/<class>.c (#included above translate_block) can defer a rare unhandled form.
void report_unimpl(uint64_t pc, struct insn *I);

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

int *hl_x86_integer_pending_flags(void) { return &g_fl_pending; }
void hl_x86_integer_reset_flags(void) { g_fl_pending = FL_NONE; }
int hl_x86_legacy_pfaf_dead(void) { return g_pfaf_dead; }
int hl_x86_legacy_flags_pending(void) { return g_fl_pending; }
void hl_x86_legacy_flags_pending_clear(void) { g_fl_pending = FL_NONE; }

static int lazyflags_on(void) {
    return 1;
}

int hl_x86_integer_lazy_flags(void) { return lazyflags_on(); }

// Direct-write ALU dst: when an ALU (or group1) instruction's r/m operand is a REGISTER (not memory)
// at width>=4, compute the result straight into the guest reg's host home instead of into scratch x16
// followed by a store-back `mov guest,x16`. do_alu already writes any dst (including a guest x0..x15,
// as the dst==reg forms do) and computes PF/AF from the pristine a,b BEFORE overwriting `out`, so
// out==a is byte-identical to out==x16 + rm_store — one fewer instruction on the dependent chain.
// Gate NOXALUDIRECT=1 for A/B (elide-on default). Independent of the flag levers.
int xaludirect_on(void) {
    return 1;
}

// Spill the deferred flags to cpu->nzcv with the producer-correct finalizer (byte-identical to the
// old inline finalizer) and clear the pending state. Every finalizer also msr's the corrected value
// back, so the live ARM NZCV is left canonical for an immediately-following Jcc to branch off.
void flags_materialize(void) {
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
void e_bit_move(int dst, int src, int sp, int dp, int tmp) {
    emit32(0x53000000u | ((uint32_t)sp << 16) | ((uint32_t)sp << 10) | ((uint32_t)src << 5) |
           (uint32_t)tmp); // ubfx wtmp,wsrc,#sp,#1
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
void e_af_save(int reg) {
    e_str(reg, 28, OFF_AF);
}

// Compute x86 AF for an add/sub-class op: store (a ^ b ^ result) -- its bit 4 is the carry out of bit 3.
// `tmp` is a scratch reg (clobbered). Read a/b/res before they may be reused (they are value regs).
void e_af_addsub(int a, int b, int res, int tmp) {
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
void hl_x86_integer_prepare_flags(const struct insn *instruction, uint64_t guest_pc, uint64_t next,
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
