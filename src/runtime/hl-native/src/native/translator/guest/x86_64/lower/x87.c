// translator/guest/x86_64 -- x87 translate-time stack-top (fptop) tracking.
// The baseline x87 path keeps ST(0..7) in cpu->st[] with the live top in cpu->fptop, and every
// ST(i) touch recomputes the wrapped slot at runtime (e_st_addr: ldr fptop; add #i; and #7; add
// base; add idx,lsl#3 -- 5 insns) while each push/pop does a ldr/modify/str of cpu->fptop.
//
// When the absolute top is statically known at translate time we instead:
//   * resolve ST(i) to its concrete shadow-stack slot and address it with ONE `add xa,x28,#off`,
//   * keep push/pop as pure translate-time bookkeeping (no cpu->fptop traffic), writing the shadow
//     back to cpu->fptop only when guest state may escape (a faulting guest memory access, a C-helper
//     exit, or any non-x87 instruction / block boundary) -- exactly as lazy flags spill to cpu->nzcv.
// Storage stays cpu->st[] at double precision, the ops/condition codes/fpsw paths are untouched, so
// results are bit-identical to the baseline; only the addressing of ST(i) and the timing of the
// cpu->fptop store change, and the escape-point materialize keeps cpu->fptop observably current.
//
// ===== H11 (KNOWN GAP, NOT fixed here): x87 stack is 64-bit double, not 80-bit extended ==========
// The architectural x87 register file is 80-bit extended precision (64-bit explicit mantissa, 15-bit
// exponent). This engine carries ST(0..7) as IEEE-754 binary64 in cpu->st[] (see hl_x86_x87_load/hl_x86_x87_store here
// and hl_x86_x87_load_ext80/hl_x86_x87_store_ext80_pop in x87state.c). Precisely what that loses:
//   * mantissa: 64 explicit bits -> 52 -> every D8-DF arithmetic op (fadd/fmul/fsub/fdiv/fsqrt/frndint/
//     fprem/fscale/fxtract above) and every transcendental (x87_func) rounds each intermediate to 53
//     significant bits instead of 64, so long chains of x87 math and C `long double` computations drift
//     in the low ~11 bits vs a real FPU;
//   * exponent: 15-bit (range ~1e+-4932) -> 11-bit (~1e+-308), so values with |exp| beyond binary64's
//     range flush to 0/Inf where a real x87 would keep them (fscale clamps to the binary64 exponent);
//   * round-trips: FLD m80 / FSTP m80 (the x87state.c helpers) narrow to double on load and re-widen a
//     53-bit value on store, so an 80-bit value written by the guest and read back loses its tail;
//     `printf("%Lf", ...)`, C `long double`, and 80-bit `fldt/fstpt` object files see the drift.
// A true fix needs an 80-bit (or software-emulated ext80) carrier for cpu->st[] plus reworking every op
// and both m80 converters -- a large, cross-cutting change spanning ops.c (outside this file's
// cluster). It is NOT attempted here, and cannot be cheaply approximated on this host: the macOS/arm64
// build ABI makes `long double` == `double` (64-bit), so widening the host carrier to `long double`
// would buy nothing. Related: #248/#249. Everything below is deliberately double-precision-limited.
//
// The top is "known" only after a `finit` anchors it (top=0) within an unbroken run of x87
// instructions; any non-x87 instruction ends the run (materialize + drop to the runtime model), and
// any x87 op we cannot statically track falls back to the baseline helpers. NOX87OPT forces the
// runtime-top path everywhere -> byte-identical to the pre-opt engine.
//
// ===== TAG STATE and the #IS stack faults ==========================================================
// cpu->fptop bits 15:8 are the per-PHYSICAL-slot EMPTY bits and bit 16 is ARMED (x87state.h has the
// encoding and why it lives there). Everything below keeps them: materialize writes TOP with a bfi rather
// than the plain str that used to clear 63:3, push/pop/store retag, and FFREE -- once a no-op -- sets a bit.
// x22 is RESERVED across an x87 instruction as the "an operand slot was EMPTY" predicate: hl_x86_x87_live()
// raises the #IS and sets it, and hl_x86_x87_indefinite() then forces the QNaN indefinite into whatever the
// instruction writes. Nothing else in this file or in translate.c's x87 arm may use x22.
#include "x87.h"

#include "../cpu.h"
#include "../encoding.h"
#include "../x87state.h"
#include "primitives.h"
#include "x87_stack.h"

static struct hl_x87_stack g_fp_stack;

int hl_x86_x87_optimized(void) {
    return 1;
}

void hl_x86_x87_reset(void) {
    hl_x87_stack_reset(&g_fp_stack);
}

void hl_x86_x87_anchor(unsigned top) {
    hl_x87_stack_anchor(&g_fp_stack, top);
}

// &cpu->st[shadow slot i] -> xdst, single add (OFF_ST + slot*8 fits the add imm12).
static void fp_slot_addr(int xdst, int i) {
    unsigned off = (unsigned)OFF_ST + hl_x87_stack_slot(&g_fp_stack, i) * 8u;
    emit32(0x91000000u | (off << 10) | (28u << 5) | (unsigned)xdst); // add xdst, x28, #off
}

// Make cpu->fptop reflect the shadow (idempotent). Keeps the shadow known. Read-modify-write, not the plain
// 64-bit str this used to be: bits 63:3 now carry the tag word and the ARMED bit.
void hl_x86_x87_materialize(void) {
    if (hl_x87_stack_known(&g_fp_stack) && hl_x87_stack_dirty(&g_fp_stack)) {
        e_ldr(16, 28, OFF_FPTOP);
        e_movconst(17, (uint64_t)hl_x87_stack_top(&g_fp_stack));
        e_bfi(16, 17, 0, 3, 1);
        e_str(16, 28, OFF_FPTOP);
        hl_x87_stack_materialized(&g_fp_stack);
    }
}

// Leave the static-top model (run boundary / untrackable op): spill the shadow, go runtime-top.
void hl_x86_x87_drop(void) {
    hl_x86_x87_materialize();
    hl_x87_stack_drop(&g_fp_stack);
}

int hl_x86_x87_known(void) {
    return hl_x87_stack_known(&g_fp_stack);
}

#define FP_STATIC (hl_x86_x87_optimized() && hl_x86_x87_known())

// ---- tag bits ----------------------------------------------------------------------------------------
// x16 = cpu->fptop, ARMED. A zero-initialised cpu carries no tag information -- deliberately, so an image
// written before the tag word existed still restores -- so the FIRST x87 instruction arms it with every
// slot empty, which is the architectural state at that point. interp_x87_arm() does the same in C.
static void fp_tags_load(void) {
    e_ldr(16, 28, OFF_FPTOP);
    emit32(0xB2000000u | (1u << 22) | (56u << 16) | (8u << 10) | (16 << 5) | 20); // orr x20,x16,#0x1ff00
    emit32(0xF2000000u | (1u << 22) | (48u << 16) | (16 << 5) | 31);              // tst x16,#0x10000
    e_csel(16, 16, 20, 1 /*NE: already armed*/, 1);
}

// x17 = the tag-bit index (8 + physical slot) of ST(i), a constant while the top is statically known.
static void fp_tag_index(int i) {
    if (FP_STATIC) {
        e_movconst(17, 8u + hl_x87_stack_slot(&g_fp_stack, i));
        return;
    }
    e_mov_rr(17, 16, 0); // the runtime model keeps cpu->fptop's low three bits current
    if (i > 0) e_addi(17, 17, (unsigned)i, 0);
    if (i < 0) e_subi(17, 17, (unsigned)-i, 0);
    emit32(0x12000800u | (17 << 5) | 17); // and w17,w17,#7
    e_addi(17, 17, 8, 0);
}

static void fp_tag_test(int xdst) {      // xdst = 1 if x16's bit x17 is set (slot empty); clobbers x20
    e_shv(0x1AC02400u, xdst, 16, 17, 1); // lsrv
    e_movconst(20, 1);
    e_rrr(A_AND, xdst, xdst, 20, 1, 0);
}

static void fp_tag_mark(int empty) { // x16's bit x17 <- empty; clobbers x20
    e_movconst(20, 1);
    e_shv(0x1AC02000u, 20, 20, 17, 1); // lslv
    e_rrr(empty ? A_ORR : A_BIC, 16, 16, 20, 1, 0);
}

static void fp_tags_store(void) {
    e_str(16, 28, OFF_FPTOP);
}

// #IS, predicated on x22. C1 tells the two apart: 1 = OVERFLOW (a push onto a non-empty slot), 0 =
// UNDERFLOW (a read of an empty one). FSW.IE goes into the host FPSR, which is where the JIT projects
// FSW[0..5] from; SF(6) and C1 live in cpu->fpsw. Branchless, so the no-fault path is straight-line.
static void fp_stack_fault(int overflow) {
    emit32(0xD53B4420u | 20); // mrs x20, fpsr
    e_movconst(21, 1);
    e_rrr(A_ORR, 21, 20, 21, 1, 0);
    e_subi_s(31, 22, 0, 1); // cmp x22, #0
    e_csel(20, 21, 20, 1 /*NE*/, 1);
    emit32(0xD51B4420u | 20); // msr fpsr, x20
    e_ldr(20, 28, OFF_FPSW);
    if (overflow) {
        e_movconst(21, 0x240); // SF | C1
        e_rrr(A_ORR, 21, 20, 21, 1, 0);
    } else {
        e_movconst(21, 0x200);
        e_rrr(A_BIC, 21, 20, 21, 1, 0); // C1 = 0
        e_movconst(23, 0x40);
        e_rrr(A_ORR, 21, 21, 23, 1, 0); // SF
    }
    e_subi_s(31, 22, 0, 1);
    e_csel(20, 21, 20, 1, 1);
    e_str(20, 28, OFF_FPSW);
}

// x22 = 1 when ST(a) -- or ST(b), when b >= 0 -- is EMPTY, in which case #IS underflow has been raised and
// the caller must write the indefinite into the instruction's DESTINATION. The empty source stays empty.
void hl_x86_x87_live(int a, int b) {
    fp_tags_load();
    fp_tag_index(a);
    fp_tag_test(22);
    if (b >= 0) {
        fp_tag_index(b);
        fp_tag_test(21);
        e_rrr(A_ORR, 22, 22, 21, 1, 0);
    }
    fp_tags_store(); // arming, if this was the first x87 instruction
    fp_stack_fault(0);
}

// vd <- the QNaN indefinite when hl_x86_x87_live() faulted. Clobbers x20 and v23.
void hl_x86_x87_indefinite(int vd) {
    e_movconst(20, HL_X87_INDEFINITE_BITS);
    e_fmov_to_d(23, 20);
    e_subi_s(31, 22, 0, 1);
    emit32(0x1E600C00u | (23u << 16) | ((uint32_t)vd << 5) | (uint32_t)vd); // fcsel dvd, dvd, d23, eq
}

// Tag ST(i) empty (FFREE) or live, without touching TOP or the data register.
void hl_x86_x87_tag(int i, int empty) {
    fp_tags_load();
    fp_tag_index(i);
    fp_tag_mark(empty);
    fp_tags_store();
}

void hl_x86_x87_load(int vd, int i) { // vd = ST(i)
    if (FP_STATIC) {
        fp_slot_addr(17, i);
        g_ldr_d(vd, 17);
    } else
        e_fp_ld(vd, i);
}

void hl_x86_x87_store(int vs, int i) { // ST(i) = vs; any write FILLS the slot, so it retags live
    hl_x86_x87_tag(i, 0);
    if (FP_STATIC) {
        fp_slot_addr(17, i);
        g_str_d(vs, 17);
    } else
        e_fp_st(vs, i);
}

// A push onto a NON-EMPTY slot is #IS overflow: TOP still decrements and the destination is overwritten
// with the indefinite, destroying what was there (measured: 9x fld1 from FNINIT leaves fsw=3a41).
void hl_x86_x87_push(int vs) { // push vs -> ST(0)  (top -= 1)
    fp_tags_load();
    fp_tag_index(-1);
    fp_tag_test(22);
    e_movconst(20, 1);
    e_rrr(A_EOR, 22, 22, 20, 1, 0); // x22 = overflow = destination NOT empty
    fp_tag_mark(0);
    fp_tags_store();
    fp_stack_fault(1);
    hl_x86_x87_indefinite(vs);
    if (FP_STATIC) {
        hl_x87_stack_push(&g_fp_stack);
        fp_slot_addr(17, 0);
        g_str_d(vs, 17);
    } else
        e_fp_push(vs);
}

// A pop tags the vacated slot empty; FINCSTP/FDECSTP rotate TOP alone (measured: they never retag, and
// never fault -- `fdecstp` onto a full stack is quiet).
void hl_x86_x87_pop(void) {
    hl_x86_x87_tag(0, 1);
    hl_x86_x87_adjust_top(1);
}

void hl_x86_x87_adjust_top(int delta) { // top += delta
    if (FP_STATIC) {
        hl_x87_stack_adjust(&g_fp_stack, delta);
    } else
        e_fp_settop(delta);
}

// ---- FCW.RC / FCW.PC ---------------------------------------------------------------------------------
// x87 has its OWN rounding domain, separate from SSE MXCSR, and both share ARM FPCR.RMode -- so every x87
// operation that rounds runs inside a saved/restored FPCR whose RMode comes from FCW[11:10]. Without this
// `1/3` under RC=up stored to m64 was ...555 where hardware gives ...556. Scratch x17/x20/x21; x23 carries
// the saved FPCR across to hl_x86_x87_rc_leave, and x22 (the #IS predicate) is deliberately untouched.
void hl_x86_x87_rc_enter(void) {
    e_ldr(20, 28, OFF_FPCW);
    emit32(0x53000000u | (10u << 16) | (11u << 10) | (20 << 5) | 20); // ubfx w20,w20,#10,#2 -> RC
    e_movconst(21, 1);
    e_rrr(A_AND, 17, 20, 21, 0, 0); // w17 = RC & 1
    e_lsr_i(21, 20, 1, 0);
    e_rrr(A_ORR, 17, 21, 17, 0, 1); // w17 = ARM RMode (the x87 RC bits swapped)
    emit32(0xD53B4400u | 23);       // mrs x23, fpcr
    e_movconst(21, 3u << 22);
    e_rrr(A_BIC, 20, 23, 21, 1, 0);
    e_rrr(A_ORR, 20, 20, 17, 1, 22);
    emit32(0xD51B4400u | 20); // msr fpcr, x20
}

void hl_x86_x87_rc_leave(void) {
    emit32(0xD51B4400u | 23);
} // msr fpcr, x23

// FCW.PC (bits 9:8): 00 = 24 significand bits, 10 = 53, 11 = 64 (the FNINIT default). The carrier is a
// double, so PC=53 is exact and PC=64 is the model's known shortfall; PC=24 IS exact, because re-rounding a
// 53-bit result to 24 is innocuous double rounding (53 >= 2*24+2). x87 keeps the full 15-bit exponent range
// at PC=24, so the significand is rounded by scaling the exponent to 1023, round-tripping through f32 --
// which rounds to 24 bits under the FPCR.RMode hl_x86_x87_rc_enter() just set, and raises the #P hardware
// raises -- and scaling back. Both scalings are pure exponent arithmetic, hence exact. Call inside the RC
// scope. The round trip is emitted unconditionally and SELECTED at the end, so the FPSR is snapshotted and
// put back when PC is not 24: otherwise `1 + 2^-30` at PC=53, exact in the carrier, reported #P.
// Clobbers x16/x17/x20/x21 and v22/v23; x22 (the #IS predicate) and x23 (the saved FPCR) survive.
void hl_x86_x87_narrow(int vd) {
    emit32(0xD53B4420u | 16); // mrs x16, fpsr
    e_fmov_from_d(20, vd);
    emit32(0xD3400000u | (52u << 16) | (62u << 10) | (20 << 5) | 21); // ubfx x21,x20,#52,#11 -> biased exp
    e_movconst(17, 2046);
    e_rrr(A_SUB, 17, 17, 21, 1, 0);
    e_lsl_i(17, 17, 52, 1);
    e_fmov_to_d(22, 17);
    emit32(0x1E600800u | (22u << 16) | ((uint32_t)vd << 5) | 22u); // fmul d22, dvd, d22
    emit32(0x1E624000u | (22 << 5) | 22);                          // fcvt s22, d22        (the 24-bit rounding, and #P)
    emit32(0x1E22C000u | (22 << 5) | 22);                          // fcvt d22, s22
    e_lsl_i(17, 21, 52, 1);
    e_fmov_to_d(23, 17);
    emit32(0x1E600800u | (23 << 16) | (22 << 5) | 22); // fmul d22, d22, d23   (exact)
    e_ldr(20, 28, OFF_FPCW);
    e_movconst(17, 0x300);
    e_rrr(A_AND, 20, 20, 17, 1, 0); // PC
    e_subi(17, 21, 1, 1);
    e_subi_s(31, 17, 2045, 1);                                      // exponent in [1,2046]: LS
    emit32(0xFA400800u | (0u << 16) | (9u << 12) | (20 << 5) | 0u); // ccmp x20,#0,#0,ls
    e_cset(20, 0 /*EQ*/, 1);                                        // x20 = PC is 24 and the value is normal
    emit32(0xD53B4420u | 17);                                       // mrs x17, fpsr
    e_subi_s(31, 20, 0, 1);
    e_csel(17, 17, 16, 1 /*NE*/, 1); // otherwise the round trip never happened
    emit32(0xD51B4420u | 17);        // msr fpsr, x17
    e_subi_s(31, 20, 0, 1);
    emit32(0x1E600C00u | ((uint32_t)vd << 16) | (1u << 12) | (22u << 5) | (uint32_t)vd);
}

#undef FP_STATIC

// ===== x87 D9 Fx remainder / scale / extract group (computed on host doubles) ===========
// These ops have no SSE counterpart; emulate them on f64 with the same write-back-to-cpu->st[]
// and FPSW condition-code conventions as the inline D8-DF arithmetic. Scratch: GP x16/x17/x19/x21,
// FP v16/v17/v18 (v0..v15 are guest xmm; v16+ are free). The
// hl_x86_x87_load/hl_x86_x87_store/hl_x86_x87_push/hl_x86_x87_adjust_top calls keep the translate-time static-top
// shadow consistent exactly like the surrounding ops.

// x87 GENERATED-NaN sign fixup -- the scalar-double analogue of emit_dnan_pre/post (SSE).
// x87 invalid operations (fsqrt of a negative, 0/0, inf/inf, inf-inf, 0*inf) deliver the QNaN
// floating-point INDEFINITE with the sign bit SET (0xFFF8000000000000); ARM's FADD/FSUB/FMUL/FDIV/
// FSQRT deliver the DEFAULT NaN with the sign CLEAR -- identical payload, opposite sign. A NaN
// PROPAGATED from an input keeps that input's sign on both ISAs, so only a GENERATED NaN may be
// stamped: "result is NaN AND no input was NaN". PRE runs while both inputs are still live (the
// ARM forms are destructive, so the mask cannot be built afterwards); POST runs on the result.
// Scratch: v22/v23, which nothing in the D8-DF lowering (v16/v17/v18/v20 only) uses.
#define FCMEQd(d, n, m) emit32(0x5E60E400u | ((uint32_t)(m) << 16) | ((uint32_t)(n) << 5) | (uint32_t)(d))

void hl_x86_x87_dnan_pre(int n, int m) {
    FCMEQd(22, n, n);                                  // d22 = (n == n)   (all-ones iff n is NOT NaN)
    FCMEQd(23, m, m);                                  // d23 = (m == m)
    emit32(0x0E201C00u | (23 << 16) | (22 << 5) | 22); // AND v22.8b, v22, v23  -> both inputs ordered
}

void hl_x86_x87_dnan_post(int d) {
    FCMEQd(23, d, d);                                    // d23 = (result == result)
    emit32(0x0E601C00u | (23 << 16) | (22 << 5) | 22);   // BIC v22.8b -> ordered inputs AND NaN result
    emit32(0x4F005400u | (127u << 16) | (22 << 5) | 22); // SHL v22.2d, v22, #63 -> the sign bit, or 0
    emit32(0x0EA01C00u | (22u << 16) | ((uint32_t)d << 5) | (uint32_t)d);
}

// FSCALE: ST0 = ST0 * 2^trunc(ST1). Build 2^n straight into the double exponent field; clamping the
// biased exponent to [0,2047] gives +0.0 on underflow and +Inf on overflow, matching scalbn.
void hl_x86_x87_scale(void) {
    hl_x86_x87_load(18, 0);      // d18 = ST0
    hl_x86_x87_load(16, 1);      // d16 = ST1
    hl_x86_x87_dnan_pre(18, 16); // inf*0 -> x86's NEGATIVE indefinite, not ARM's default NaN
    // FSCALE of a NON-FINITE or ZERO ST0 is the IDENTITY (scalbn(+-inf,n)=+-inf, scalbn(+-0,n)=+-0,
    // scalbn(NaN,n)=NaN) for EVERY n. The exponent clamp below cannot express that: a large negative
    // ST1 clamps 2^n to +0.0, so inf*0 came out as the indefinite instead of inf (and symmetrically a
    // large positive ST1 turned +-0 into a NaN via 0*inf). Capture ST0 and the "not finite, or zero"
    // predicate now, and select ST0 back over the product at the end.
    emit32(0x1E60C000u | (18 << 5) | 19); // fabs  d19, d18            (|ST0|)
    e_movconst(20, 0x7ff0000000000000ull);
    e_fmov_to_d(20, 20);                               // d20 = +inf
    emit32(0x7EE0E400u | (19 << 16) | (20 << 5) | 21); // FCMGT d21, d20, d19       (|ST0| < inf, ordered)
    emit32(0x2E205800u | (21 << 5) | 21);              // NOT   v21.8b              -> inf or NaN
    emit32(0x5EE0D800u | (19 << 5) | 20);              // FCMEQ d20, d19, #0.0      -> +-0
    emit32(0x0EA01C00u | (20 << 16) | (21 << 5) | 21); // ORR   v21.8b              -> "identity" mask
    emit32(0x0EA01C00u | (18 << 16) | (18 << 5) | 20); // MOV   v20.8b, v18         (save ST0)
    emit32(0x1E780000u | (16 << 5) | 16);              // fcvtzs w16, d16  (n = trunc(ST1), int32-saturating)
    e_sxt(16, 16, 4);                                  // sign-extend n to 64-bit
    e_addi(16, 16, 1023, 1);                           // biased exponent e = n + 1023
    e_movconst(19, 2047);
    e_subi_s(31, 16, 2047, 1);        // cmp e, #2047
    e_csel(16, 19, 16, 12 /*GT*/, 1); // e = (e > 2047) ? 2047 : e
    e_movconst(19, 0);
    e_subi_s(31, 16, 0, 1);                            // cmp e, #0
    e_csel(16, 19, 16, 11 /*LT*/, 1);                  // e = (e < 0)    ? 0    : e
    e_lsl_i(16, 16, 52, 1);                            // place e into the exponent field
    e_fmov_to_d(17, 16);                               // d17 = 2^n
    emit32(0x1E600800u | (17 << 16) | (18 << 5) | 18); // fmul d18, d18, d17
    emit32(0x2E601C00u | (18 << 16) | (20 << 5) | 21); // BSL v21.8b, v20(ST0), v18(prod) -> identity?ST0:prod
    emit32(0x0EA01C00u | (21 << 16) | (21 << 5) | 18); // MOV v18.8b, v21
    hl_x86_x87_dnan_post(18);
    hl_x86_x87_indefinite(18);
    hl_x86_x87_store(18, 0);
}

// FXTRACT: split ST0 into unbiased exponent and significand. ST0 <- significand (in [1,2) with ST0's
// sign), then the exponent is pushed so ST1 = exponent, ST0 = significand (normal operands).
void hl_x86_x87_extract(void) {
    hl_x86_x87_load(16, 0); // d16 = ST0
    e_fmov_from_d(16, 16);  // x16 = bit pattern
    e_lsr_i(17, 16, 52, 1);
    e_movconst(19, 0x7FF);
    e_rrr(A_AND, 17, 17, 19, 1, 0);       // exponent field
    e_subi(17, 17, 1023, 1);              // unbiased exponent (signed)
    emit32(0x9E620000u | (17 << 5) | 17); // scvtf d17, x17  (exponent -> double)
    e_movconst(19, ~(0x7FFULL << 52));
    e_rrr(A_AND, 16, 16, 19, 1, 0); // clear exponent field
    e_movconst(19, 1023ULL << 52);
    e_rrr(A_ORR, 16, 16, 19, 1, 0); // set exponent to bias -> significand in [1,2)
    e_fmov_to_d(18, 16);            // d18 = significand
    hl_x86_x87_indefinite(17);      // an empty ST0 gives BOTH results the indefinite (x22 is consumed here:
    hl_x86_x87_indefinite(18);      // hl_x86_x87_push below reuses it for its own overflow verdict)
    hl_x86_x87_store(17, 0);        // ST0 = exponent
    hl_x86_x87_push(18);            // push significand -> ST0 = significand, ST1 = exponent
}

// C1 after a rounding op means "the SIGNIFICAND was rounded up", i.e. the MAGNITUDE grew -- x87 is
// sign-magnitude, so this is NOT `result > original` (measured: -2.5 under RC=down gives -3 with C1=1).
// vr = the rounded value, vo = the original. Clobbers x20/x21/x23 and v20/v21; preserves x22.
void hl_x86_x87_rounded_up(int vr, int vo) {
    emit32(0x1E60C000u | ((uint32_t)vr << 5) | 20u); // fabs d20, dvr
    emit32(0x1E60C000u | ((uint32_t)vo << 5) | 21u); // fabs d21, dvo
    emit32(0x1E602000u | (21 << 16) | (20 << 5));    // fcmp d20, d21
    e_cset(20, 12 /*GT*/, 1);
    e_ldr(21, 28, OFF_FPSW);
    e_movconst(23, 0x200);
    e_rrr(A_BIC, 21, 21, 23, 1, 0);
    e_bfi(21, 20, 9, 1, 1);
    e_str(21, 28, OFF_FPSW);
}

// FRNDINT: round ST0 to an integral value using the CURRENT x87 rounding control. A bare frintx under the
// live (SSE, default-nearest) FPCR ignored fldcw's RC -- floorl/ceill/truncl, which set RC via fldcw around
// frndint, then all rounded to nearest.
void hl_x86_x87_round(void) {
    hl_x86_x87_load(16, 0);
    hl_x86_x87_indefinite(16);
    hl_x86_emit_vector_copy(19, 16); // keep the original for C1
    hl_x86_x87_rc_enter();
    emit32(0x1E67C000u | (16 << 5) | 16); // frinti d16, d16 (round to integral per FPCR.RMode)
    hl_x86_x87_rc_leave();
    hl_x86_x87_rounded_up(16, 19);
    hl_x86_x87_store(16, 0);
}

// FTST: compare ST0 with 0.0 and set the FPSW condition codes (same path as fcom).
void hl_x86_x87_test(void) {
    hl_x86_x87_load(18, 0);
    hl_x86_x87_indefinite(18); // an empty ST0 compares UNORDERED (C3:C2:C0 = 111) as well as faulting
    e_movconst(16, 0);
    e_fmov_to_d(16, 16);       // d16 = 0.0
    e_fcom_setfpsw(18, 16, 1); // ST0 : 0.0 -> C0/C2/C3; FTST signals on any NaN
}

// x87 FSW exception flags (bits IE0/DE1/ZE2/OE3/UE4/PE5) mirror the SSE MXCSR exception bits and, like
// them, are projected lazily from the host FPSR cumulative flags at read time (the x87 arithmetic ops
// execute as host NEON just like SSE, so the real exceptions already accumulate in the host FPSR). The
// per-bit map (FSW bit i <- FPSR bit) is IE<-IOC(0) DE<-IDC(7) ZE<-DZC(1) OE<-OFC(2) UE<-UFC(3) PE<-IXC(4),
// identical to the MXCSR projection in translate.c. fnclex/finit clear the host FPSR sticky flags.
static const int g_fsw_fpsr_bit[6] = {0, 7, 1, 2, 3, 4};

// OR the host FPSR sticky exception flags into x16 (the in-progress FSW) at bits 0..5, then set ES(7)
// and B(15) if any raised exception is UNMASKED per the current FCW mask bits (FCW[0..5], 1 = masked).
// Scratch: x17/x20/x21/x22 -- deliberately NOT x19, which holds the store EA at the fnstenv/fnstsw-m16
// call sites (x16 also survives as the running FSW word the callers store afterward).
static void fp_project_exceptions(void) {
    emit32(0xD53B4420u | 22); // mrs x22, fpsr
    e_movconst(21, 0);        // exception accumulator (FSW bits 0..5)
    e_movconst(20, 1);
    for (int i = 0; i < 6; i++) {
        e_lsr_i(17, 22, g_fsw_fpsr_bit[i], 0);
        e_rrr(A_AND, 17, 17, 20, 0, 0);
        e_rrr(A_ORR, 21, 21, 17, 0, i); // x21 |= bit << i
    }
    e_rrr(A_ORR, 16, 16, 21, 0, 0); // FSW |= exceptions (sticky, bits 0..5)
    e_ldr(17, 28, OFF_FPCW);        // w17 = FCW (bits 0..5 are the exception masks)
    e_rrr(A_BIC, 17, 21, 17, 0, 0); // x17 = raised & ~masked = unmasked exceptions
    e_movconst(20, 0x3f);
    e_rrr(A_AND, 17, 17, 20, 0, 0); // keep only bits 0..5
    e_subi_s(31, 17, 0, 1);         // cmp x17, #0
    e_cset(17, 1 /*NE*/, 1);        // x17 = (any unmasked exception)
    e_bfi(16, 17, 7, 1, 1);         // ES (error summary, bit 7)
    e_bfi(16, 17, 15, 1, 1);        // B  (busy, bit 15, mirrors ES)
}

// FNSTSW / FSTSW: the x87 status word reports TOP-of-stack (cpu->fptop) in bits 11-13 ORed with the
// condition codes held in cpu->fpsw and the exception flags projected from the host FPSR -- qemu does the
// same, and code that follows FNSTSW with SAHF relies on it. Result -> x16 (clobbers x17/x19..x22). The
// shadow top is materialized first so cpu->fptop is current under the static-top optimization.
void hl_x86_x87_status(void) {
    hl_x86_x87_materialize();
    e_ldr(16, 28, OFF_FPSW);
    e_movconst(17, 0x4740); // cpu->fpsw holds the condition codes and SF, nothing else
    e_rrr(A_AND, 16, 16, 17, 1, 0);
    e_ldr(17, 28, OFF_FPTOP);
    e_bfi(16, 17, 11, 3, 1); // status[13:11] = TOP
    fp_project_exceptions();
}

// FNCLEX: clear the sticky exception flags (host FPSR IOC/DZC/OFC/UFC/IXC/IDC + the projected FSW/ES/B),
// leaving the condition codes and TOP intact. Clobbers x16/x17.
void hl_x86_x87_clear_exceptions(void) {
    emit32(0xD53B4420u | 16);       // mrs x16, fpsr
    e_movconst(17, 0x9f);           // IOC|DZC|OFC|UFC|IXC|IDC (bits 0-4,7)
    e_rrr(A_BIC, 16, 16, 17, 0, 0); // clear the host sticky flags
    emit32(0xD51B4420u | 16);       // msr fpsr, x16
}

// FXAM: classify ST0 and set the FPSW condition codes (C1 = sign, {C3,C2,C0} = class), per the x87 spec.
// cpu->st[] is double precision, so 80-bit unsupported/pseudo-denormal forms cannot arise. Class codes
// {C3,C2,C0}: zero=100, NaN=001, Inf=011, denormal=110, normal=010, EMPTY=101. From the IEEE-754 fields
// this is C0=(exp==max), C3=(exp==0), C2=!(zero|NaN). FXAM never faults -- it is the instruction that
// REPORTS emptiness, which is what the tag word buys. Scratch: x16/x17/x19/x20/x21/x22/x23, v18.
void hl_x86_x87_classify(void) {
    fp_tags_load();
    fp_tag_index(0);
    fp_tag_test(23); // x23 = ST(0) is empty; survives the classification below
    fp_tags_store();
    hl_x86_x87_load(18, 0);
    e_fmov_from_d(16, 18);  // x16 = bit pattern of ST0
    e_lsr_i(21, 16, 63, 1); // x21 = sign            -> C1
    e_lsr_i(17, 16, 52, 1);
    e_movconst(19, 0x7FF);
    e_rrr(A_AND, 17, 17, 19, 1, 0); // x17 = exponent field
    e_movconst(19, (1ull << 52) - 1);
    e_rrr(A_AND, 16, 16, 19, 1, 0); // x16 = mantissa field
    e_subi_s(31, 17, 0, 1);
    e_cset(19, 0, 1); // x19 = (exp == 0)      -> C3
    e_subi_s(31, 17, 0x7FF, 1);
    e_cset(17, 0, 1); // x17 = (exp == max)    -> C0
    e_subi_s(31, 16, 0, 1);
    e_cset(16, 0, 1);               // x16 = (mantissa == 0)
    e_rrr(A_AND, 22, 19, 16, 1, 0); // x22 = zero = exp0 & mant0
    e_rrr(A_BIC, 16, 17, 16, 1, 0); // x16 = NaN  = expMax & ~mant0
    e_rrr(A_ORR, 22, 22, 16, 1, 0); // x22 = zero | NaN
    e_movconst(16, 1);
    e_rrr(A_EOR, 22, 22, 16, 1, 0); // x22 = C2 = !(zero | NaN)
    e_movconst(16, 0);
    e_bfi(16, 17, 8, 1, 1);  // C0 (bit 8)
    e_bfi(16, 21, 9, 1, 1);  // C1 (bit 9)
    e_bfi(16, 22, 10, 1, 1); // C2 (bit 10)
    e_bfi(16, 19, 14, 1, 1); // C3 (bit 14)
    e_movconst(21, 0x4100);
    e_rrr(A_ORR, 22, 16, 21, 1, 0); // empty: C3:C2:C0 = 101, C1 keeps the sign
    e_movconst(21, 0x400);
    e_rrr(A_BIC, 22, 22, 21, 1, 0);
    e_subi_s(31, 23, 0, 1);
    e_csel(16, 22, 16, 1 /*NE*/, 1);
    e_ldr(21, 28, OFF_FPSW); // SF is sticky across a condition-code write
    e_movconst(22, 0x40);
    e_rrr(A_AND, 21, 21, 22, 1, 0);
    e_rrr(A_ORR, 16, 16, 21, 1, 0);
    e_str(16, 28, OFF_FPSW);
}

// FLD m32/m64 of a SUBNORMAL raises #D at LOAD time -- widening it to the register's exponent range is what
// hardware calls a denormal operand (measured: fldl of 5e-324 leaves exc=02; the m80 form raises nothing).
// ARM raises IDC only under FPCR.FZ, so it has to be tested for. `bits` holds the raw loaded word; the test
// is `0 < |bits| < 2^mantissa`, done as one unsigned compare. Clobbers x17/x20/x21.
void hl_x86_x87_denormal(int bits, int single) {
    e_movconst(21, single ? 0x7fffffffull : 0x7fffffffffffffffull);
    e_rrr(A_AND, 17, bits, 21, 1, 0);
    e_movconst(21, single ? 0x7fffffull : 0xfffffffffffffull); // 2^mantissa - 1
    e_subi(17, 17, 1, 1);
    e_rrr(A_SUBS, 31, 17, 21, 1, 0);
    e_cset(17, 3 /*LO*/, 1);
    emit32(0xD53B4420u | 20); // mrs x20, fpsr
    e_movconst(21, 0x80);     // IDC -> FSW.DE
    e_rrr(A_ORR, 21, 20, 21, 1, 0);
    e_subi_s(31, 17, 0, 1);
    e_csel(20, 21, 20, 1 /*NE*/, 1);
    emit32(0xD51B4420u | 20); // msr fpsr, x20
}

// x87 transcendentals (the D9 F0-FF subset: F2XM1/FYL2X/FPTAN/FPATAN/FYL2XP1/FSINCOS/FSIN/FCOS) have
// no ARM/SSE counterpart and need host libm, so they exit the block to the C helper x87_func(), which
// computes the op on the double-precision ST stack. cpu->x87_ea carries the X87_* selector. The block
// ends here (like the m80 fld/fstp helpers); the caller breaks out of translation afterwards.
void hl_x86_x87_function(int fn, uint64_t next) {
    hl_x86_x87_drop(); // the helper reads/writes cpu->st[] and cpu->fptop directly -> spill the shadow top
    e_movconst(16, (uint64_t)fn);
    e_str(16, 28, OFF_X87EA);
    emit_exit_const(next, R_X87FUNC);
}
