// translator/guest/x86_64 -- the x86-64 -> arm64 translator (flag synthesis, SSE/x87 lowering, the
// big translate_block) + host entry trampolines.

#include "../../../host/cpu.h" // HL_HOST_CPU_*: the host entry trampolines at the end are AArch64-only
#include "lower/primitives.h"
#include "lower/execution_control.h"
#include "lower/arithmetic.h"
#include "lower/alu.h"
#include "lower/avx_inline.h"
#include "lower/crypto.h"
#include "lower/mov.h"
#include "lower/repstr.h"
#include "lower/shift.h"
#include "lower/trace.h"
#include "lower/legacy.h"
#include "lower/sse.h"
#include "lower/simd.h"
#include "lower/x87.h"
#include "lower/x87_decode.h"

// ---------------- the translator ----------------
void report_unimpl(uint64_t pc, struct insn *I);

void hl_x86_legacy_jcc_spill(int kind) {
    if (kind == HL_X86_JCC_SPILL_LOGIC)
        e_nzcv_save_c1();
    else if (kind != HL_X86_JCC_SPILL_NONE)
        e_nzcv_save();
}

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



// Link range and storage bias for a displaced ET_EXEC image. Architectural addresses remain in this low
// guest range; the bias is applied only when instruction or data bytes are dereferenced on the host.
static uint64_t g_nonpie_lo, g_nonpie_hi, g_nonpie_bias;

// A biased ET_EXEC executes from the host mapping at link_pc+bias, but the address pushed by x86 CALL is
// guest-visible architectural state.  Keep it in the ELF link-address domain so DWARF FDE ranges, dladdr,
// backtrace, and forced unwinding see the same PCs they would on Linux. RET returns that same guest value;
// instruction fetch alone projects it onto storage.
uint64_t call_return_pc(uint64_t pc) {
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
void g_ldr_q_ea(int t, struct insn *I, uint64_t next) {
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

void g_ldr_d_ea(int t, struct insn *I, uint64_t next) {
    emit_ea(I, next);
    if (emit_soft_memory_active()) emit_memory_guard(17, 8, next - (uint64_t)I->len, X86_SOFT_READ);
    g_ldr_d(t, 17);
}

// Integer-SIMD r/m operand at the width the prefix selects. MMX is 8 bytes: a 128-bit load here reads 8
// bytes the guest never addressed and #PFs when the operand ends a mapped page. LDR D zero-extends, which
// is the invariant the lane-local arms below rely on.
void g_ldr_vec_ea(int t, struct insn *I, uint64_t next, int mmx) {
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

void g_str_d_ea(int t, struct insn *I, uint64_t next) {
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

void g_str_q_ea(int t, struct insn *I, uint64_t next) {
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


// x86 direction flag (DF). The AUTHORITATIVE copy is now the RUNTIME bit cpu->df (OFF_DF), maintained by
// cld/std/popfq and read at runtime by pushfq and the string-op lowering -- so a `std` (or popfq-set DF)
// whose `rep movs/stos/scas` lands in a LATER block honors the backward direction (previously it silently
// ran forward). g_df additionally tracks the STATICALLY-known value within the current block for codegen:
// Known forward/backward states emit a constant stride; dynamic means "unknown at translate
// time" (block entry, or after popfq) so the lowering loads cpu->df and picks the stride at runtime.
static enum hl_x86_direction g_df; // block-static shadow; the runtime truth is cpu->df

void hl_x86_legacy_image(uint64_t *lo, uint64_t *hi, uint64_t *bias) {
    *lo = g_nonpie_lo;
    *hi = g_nonpie_hi;
    *bias = g_nonpie_bias;
}
enum hl_x86_direction hl_x86_legacy_direction(void) { return g_df; }
void hl_x86_legacy_direction_set(enum hl_x86_direction direction) { g_df = direction; }


#include "lower/sse4x.h"
#include "lower/branch.h"
#include "lower/integer.h"

static int fpdnan_on(void) {
    return 1;
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
void hl_x86_emit_fma_group(int rA, int rB, int rC, int acc, int mt1, int mt2, int neg, int fmls, int dbl) {
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
void hl_x86_emit_vex_fp(int vd, int src1, int src2, int op, int dbl) {
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
    if (hl_x86_legacy_flags_pending()) flags_materialize();
    if (hl_x86_x87_known()) hl_x86_x87_drop();
    e_movconst(16, (uint64_t)((lsig & 0xff) | ((code & 0xff) << 8)));
    e_str(16, 28, OFF_DIVOP); // (linux_signo | si_code<<8) -> cpu->divop for raise_guest_trap
    emit_exit_const(rip, R_TRAP);
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
            smc_forget_page(g_smc_pg[i]);
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
    smc_forget_all();
    map_clear();
    memset(g_ibtc, 0, sizeof g_ibtc);
    memset(g_xibtc, 0, sizeof g_xibtc);
    pend_reset();
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
void emit_restore_rflags(int src) {
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
    hl_x86_integer_reset_flags();
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

void hl_x86_emit_vector_dirty(void) { mark_vdirty(); }
void hl_x86_emit_memory_barrier(void) { e_dmb_ish(); }

// AVX/VEX inline lowering lives in lower/avx_inline.c.

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
        if (hl_x86_legacy_flags_pending()) flags_materialize();
        if (!nosseopt() && hl_x86_lower_avx_inline(instruction, next)) return TX_NEXT;
        emit_exit_const(guest_pc, R_AVX);
        return TX_BREAK;
    }
    if (!instruction->map3) return TX_FALL;

    if (hl_x86_legacy_flags_pending()) flags_materialize();
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
        hl_x86_emit_pcmpistri_eqeach_byte(instruction->reg, right, (int)instruction->imm);
        return TX_NEXT;
    }
    emit_exit_const(guest_pc, R_SSE3B);
    return TX_BREAK;
}

// Tries the independent primary-opcode lowerers in their established order.
// TX_FALL preserves dispatch to the specialized handlers below; TX_NEXT and
// TX_BREAK are consumed by the translation loop without reinterpretation.
/* legacy integer/control lowering lives in lower/legacy.c. */

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
    hl_x86_emit_nan_input_gate(vd, source, double_precision, guest_pc);
    int fix_nan = fpdnan_on();
    if (opcode == 0xD0) {
        // Compute each operation before selecting even/odd lanes. Flipping the
        // source sign before FADD would also flip an input NaN's sign.
        if (fix_nan) hl_x86_emit_dnan_pre(vd, source, 1, double_precision);
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
        if (fix_nan) hl_x86_emit_dnan_post(vd, double_precision, 1);
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
    if (fix_nan) hl_x86_emit_dnan_post(vd, double_precision, 1);
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
    if (!hl_x86_legacy_pfaf_dead()) { // PF: n==0 keeps old, else result low byte (live Z still = n==0 here)
        e_ldr(25, 28, OFF_PF);
        e_csel(23, 25, 16, 0 /*EQ*/, 1);
        e_pf_save(23);
    }
    emit32(0xD51B4200u | 20); // sync live ARM NZCV to the stored value
    rm_store(instruction, w, 16);
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

static int lower_sse_precision_conversion(struct insn I, uint64_t guest_pc, uint64_t next, int vd, int vm) {
    if (I.op != 0x5A) return TX_FALL;
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
            emit_memory_guard(17, I.rep ? 4u : (packed && I.p66) ? 16u : 8u, guest_pc, X86_SOFT_READ);
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
    return TX_NEXT;
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
        if (fixnan) hl_x86_emit_dnan_pre(vd, s, !unary, dbl); // capture "no input NaN" (uses v20/v21)
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
        if (fixnan) hl_x86_emit_dnan_post(res, dbl, packed); // stamp x86's negative default-NaN sign
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

static int lower_one_byte_family(struct insn *instruction, uint64_t *guest_pc, uint64_t next,
                                 hl_x86_trace_state *trace_state, hl_x86_crypto_state *crypto_state,
                                 hl_x86_branch_region *branch_region) {
    uint64_t current = *guest_pc;
    int result = lower_primary_fast(instruction, current, next, trace_state);
    if (result == TX_NEXT) goto advance;
    if (result == TX_BREAK) return TX_BREAK;
    result = lower_group3_unary(instruction, next);
    if (result == TX_NEXT) goto advance;
    result = lower_group3_narrow_muldiv(instruction, current, next);
    if (result == TX_NEXT) goto advance;
    result = lower_group3_wide_muldiv(instruction, current, next);
    if (result == TX_NEXT) goto advance;
    if (result == TX_BREAK) return TX_BREAK;
    if (instruction->op == 0xF6 || instruction->op == 0xF7) {
        report_unimpl(current, instruction);
        return TX_BREAK;
    }
    result = lower_group45(instruction, current, next);
    if (result == TX_NEXT) goto advance;
    if (result == TX_BREAK) return TX_BREAK;
    result = lower_exchange(instruction, current, next);
    if (result == TX_NEXT) goto advance;
    result = lower_stack_control(instruction, current, next);
    if (result == TX_NEXT) goto advance;
    if (result == TX_BREAK) return TX_BREAK;
    result = lower_immediate_multiply(instruction, current, next, trace_state);
    if (result == TX_NEXT) goto advance;
    result = lower_primary_string(instruction, next, crypto_state);
    if (result == TX_NEXT) goto advance;
    if (result == TX_BREAK) return TX_BREAK;
    result = hl_x86_lower_direct_jump(instruction, guest_pc, next, trace_state, branch_region);
    if (result != TX_FALL) return result;
    result = lower_direct_call_loop(instruction, current, next, trace_state);
    if (result == TX_BREAK) return TX_BREAK;
    result = hl_x86_lower_short_branch(instruction, guest_pc, next, trace_state, branch_region);
    if (result != TX_FALL) return result;
    result = lower_flag_register_transfer(instruction);
    if (result == TX_NEXT) goto advance;
    result = lower_flag_stack_control(instruction, current);
    if (result == TX_NEXT) goto advance;
    if (result == TX_BREAK) return TX_BREAK;
    if (instruction->op >= 0xD8 && instruction->op <= 0xDF) mark_vdirty();
    result = hl_x86_lower_x87(instruction, current, next, report_unimpl);
    if (result == TX_NEXT) goto advance;
    if (result == TX_BREAK) return TX_BREAK;
    result = lower_accumulator_legacy(instruction, instruction->opsize == 8);
    if (result == TX_NEXT) goto advance;
    result = lower_one_byte_signal_and_lookup(instruction, next);
    if (result == TX_NEXT) goto advance;
    if (result == TX_BREAK) return TX_BREAK;
    return TX_FALL;

advance:
    *guest_pc = next;
    return TX_NEXT;
}

static int lower_sse_precision_conversion(struct insn, uint64_t, uint64_t, int, int);
static int lower_sse_float_arithmetic(struct insn, uint64_t, uint64_t, int, int);

static int lower_sse_family(struct insn *instruction, uint64_t guest_pc, uint64_t next,
                            hl_x86_crypto_state *crypto_state) {
    int vd = instruction->reg;
    int vm = instruction->rm_reg;
    int mmx = sse_mmx_capable(instruction->op) && !instruction->p66 && !instruction->rep && !instruction->repne;
    if (mmx) {
        vd &= 7;
        vm &= 7;
    }
    int writeback = mmx ? vd : -1;
#define SSE_TRY(expression)     \
    do {                        \
        int result = expression; \
        if (result != TX_FALL) { \
            if (result == TX_NEXT && writeback >= 0) e_vmov8(writeback, writeback); \
            return result;      \
        }                       \
    } while (0)
    SSE_TRY(lower_sse_moves(instruction, guest_pc, next, vd, vm, mmx));
    SSE_TRY(lower_sse_horizontal(instruction, guest_pc, next, vd, vm));
    SSE_TRY(lower_sse_packed_shift(instruction, guest_pc, next, vd, vm, mmx, &writeback, crypto_state));
    SSE_TRY(lower_sse_move_lane(instruction, next, vd, vm));
    SSE_TRY(lower_sse_bitwise(instruction, next, vd, vm, mmx));
    SSE_TRY(lower_sse_two_source_shuffle(instruction, next, vd, vm, mmx));
    SSE_TRY(lower_sse_shuffle(instruction, next, vd, vm, mmx));
    SSE_TRY(lower_sse_packed_binary(instruction, next, vd, vm, mmx));
    SSE_TRY(lower_sse_widening_multiply(instruction, next, vd, vm, mmx));
    SSE_TRY(lower_sse_float_unpack(instruction, next, vd, vm, mmx));
    SSE_TRY(lower_sse_packed_double_integer(instruction, next, vd, vm));
    SSE_TRY(lower_sse_unpack(instruction, next, vd, vm, mmx));
    SSE_TRY(lower_sse_saturating_pack(instruction, next, vd, vm, mmx));
    if (instruction->op == 0xD7 || instruction->op == 0x50) {
        writeback = -1;
        SSE_TRY(lower_sse_sign_mask(instruction, vm, mmx));
    }
    if (lower_mmx_fp_conversion(instruction, next, vd, vm) == TX_NEXT) return TX_NEXT;
    SSE_TRY(lower_sse_integer_to_scalar(instruction, guest_pc, next, vd));
    SSE_TRY(lower_sse_scalar_to_integer(instruction, guest_pc, next, vm));
    SSE_TRY(lower_sse_minmax(instruction, guest_pc, next, vd, vm));
    SSE_TRY(lower_sse_float_arithmetic(*instruction, guest_pc, next, vd, vm));
    SSE_TRY(lower_sse_precision_conversion(*instruction, guest_pc, next, vd, vm));
    SSE_TRY(lower_sse_word_lane(instruction, guest_pc, next, vd, vm, mmx, &writeback));
    SSE_TRY(lower_sse_compare(*instruction, guest_pc, next, vd, vm));
    SSE_TRY(lower_sse_flag_compare(*instruction, guest_pc, next, vd, vm));
    SSE_TRY(lower_sse_widening_integer(instruction, next, vd, vm, mmx));
    SSE_TRY(lower_sse_packed_conversion(*instruction, next, vd, vm));
    SSE_TRY(lower_sse_nontemporal_store(instruction, guest_pc, next, vd, vm));
#undef SSE_TRY
    return TX_FALL;
}

static int lower_two_byte_family(struct insn *instruction, uint64_t *guest_pc, uint64_t next,
                                 hl_x86_trace_state *trace_state, hl_x86_crypto_state *crypto_state,
                                 hl_x86_branch_region *branch_region) {
    uint64_t current = *guest_pc;
    int sf = instruction->opsize == 8;
    int result = lower_two_byte_boundary(instruction, current, next);
    if (result == TX_BREAK) return TX_BREAK;
    mark_vdirty();
    result = lower_sse_family(instruction, current, next, crypto_state);
    if (result == TX_NEXT) goto advance;
    if (result == TX_BREAK) return TX_BREAK;
    result = lower_system_query(instruction, next);
    if (result == TX_NEXT) goto advance;
    if (result == TX_BREAK) return TX_BREAK;
    result = lower_scalar_two_byte(instruction, current, next, sf, trace_state);
    if (result == TX_NEXT) goto advance;
    result = lower_wide_compare_exchange(instruction, current, next);
    if (result == TX_NEXT) goto advance;
    if (result == TX_BREAK) return TX_BREAK;
    result = lower_multibyte_hint(instruction);
    if (result == TX_NEXT) goto advance;
    result = lower_double_shift(instruction, next);
    if (result == TX_NEXT) goto advance;
    result = lower_extended_state(instruction, current, next);
    if (result == TX_NEXT) goto advance;
    if (result == TX_BREAK) return TX_BREAK;
    result = lower_bit_scan(instruction, next, sf);
    if (result == TX_NEXT) goto advance;
    result = lower_population_count(instruction, next, sf);
    if (result == TX_NEXT) goto advance;
    result = lower_bit_test_modify(instruction, current, next, sf);
    if (result == TX_NEXT) goto advance;
    if (result == TX_BREAK) return TX_BREAK;
    result = lower_compare_exchange(instruction, current, next);
    if (result == TX_NEXT) goto advance;
    result = lower_exchange_add(instruction, current, next);
    if (result == TX_NEXT) goto advance;
    result = hl_x86_lower_near_branch(instruction, guest_pc, next, trace_state, branch_region);
    if (result != TX_FALL) return result;
    result = hl_x86_lower_conditional_move(instruction, current, next, sf);
    if (result == TX_NEXT) goto advance;
    if (result == TX_BREAK) return TX_BREAK;
    return TX_FALL;

advance:
    *guest_pc = next;
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
        .pending_flags = hl_x86_integer_pending_flags(),
        .tier_counters = g_t2cnt,
        .flag_elisions = &g_prof_xflag,
        .flag_scans = &g_prof_xflag_scan,
        .tier_folds = &g_prof_t2fold,
        .materialize_flags = flags_materialize,
        .fix_add_flags = e_nzcv_fix_ci,
        .fix_logic_flags = e_nzcv_fix_c1,
        .emit_chain_exit = emit_chain_exit,
        .page_translated = txpg_has,
        .flag_elision = hl_x86_integer_lazy_flags(),
        .tier_two = g_tier2_build,
    };
    const int stitch = 1;
    uint64_t start = gpc;
    void *host = g_cp;
    emit_prologue();
    void *body = g_cp;
    // poll cpu->irq at the body entry so a caught async signal reaches a no-syscall guest loop.
    emit_irq_check(start);
    hl_x86_integer_reset_flags(); // lazy flags: nothing deferred at block entry
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
            if (hl_x86_legacy_flags_pending()) flags_materialize(); // materialize before boundary
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

        hl_x86_integer_prepare_flags(&I, gpc, next, &trace_state);

        // x87 static-top tracking ends at any non-x87 instruction: spill the shadow top to
        // cpu->fptop and drop to the runtime-top model (the run only spans consecutive x87 ops, so
        // no top assumption ever crosses a non-x87 op, a branch target, or a block boundary).
        if (hl_x86_x87_known() && !(!I.two && op >= 0xD8 && op <= 0xDF)) hl_x86_x87_drop();

        if (!I.two) {
            hl_x86_branch_region branch_region = {start, body, seen, &nseen, &trace_blk, &ncond, STITCH_OK,
                                                  g_tier2_build, notier2x, t2_slot, map_body};
            int one_byte_result = lower_one_byte_family(&I, &gpc, next, &trace_state, &crypto_state, &branch_region);
            if (one_byte_result == TX_NEXT) continue;
            if (one_byte_result == TX_BREAK) break;
        } else {
            hl_x86_branch_region branch_region = {start, body, seen, &nseen, &trace_blk, &ncond, STITCH_OK,
                                                  g_tier2_build, notier2x, t2_slot, map_body};
            int two_byte_result = lower_two_byte_family(&I, &gpc, next, &trace_state, &crypto_state, &branch_region);
            if (two_byte_result == TX_NEXT) continue;
            if (two_byte_result == TX_BREAK) break;
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

void report_unimpl(uint64_t pc, struct insn *I) {
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
