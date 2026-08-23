#include "x87_decode.h"

#include "../cpu.h"
#include "../encoding.h"
#include "../x87state.h"
#include "primitives.h"
#include "x87.h"

static hl_x86_x87_unimplemented_fn g_unimplemented;

static int condition_to_arm(int condition) {
    static const int conditions[16] = {6, 7, 3, 2, 0, 1, 9, 8, 4, 5, 6, 7, 11, 10, 13, 12};
    return conditions[condition & 0xF];
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

static int lower_x87_memory_state(struct insn *instruction, uint64_t guest_pc, uint64_t next, int reg) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xD9 && opcode != 0xDD && opcode != 0xDB && opcode != 0xDF) return TX_FALL;
    if (opcode == 0xD9) { // f32 mem
        if (reg == 0) {
            hl_x86_emit_load_scalar32(16, 19);
            emit32(0x1E260000u | ((16) << 5) | (20));
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
            emit32(0x1E270000u | ((20) << 5) | (17));
            e_subi_s(31, 22, 0, 1);
            emit32(0x1E200C00u | (17 << 16) | (0u << 12) | (16 << 5) | 16); // fcsel s16,s16,s17,eq
            hl_x86_emit_store_scalar32(16, 19);
            if (reg == 3) hl_x86_x87_pop();
        } // fst/fstp
        else if (reg == 5) {                      // fldcw m16: load the x87 control word (RC/PC/exception masks)
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
            g_unimplemented(guest_pc, instruction);
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
            g_unimplemented(guest_pc, instruction);
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
            g_unimplemented(guest_pc, instruction);
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
            g_unimplemented(guest_pc, instruction);
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
        g_unimplemented(guest_pc, instruction);
        return TX_BREAK;
    }
    return TX_NEXT;
}

static void emit_x87_integer_compare(int left, int right, int signaling) {
    emit32(0x1E602000u | (signaling ? 0x10u : 0u) | ((uint32_t)right << 16) | ((uint32_t)left << 5));
    hl_x86_emit_flags_save_fcompare();
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
            g_unimplemented(guest_pc, instruction);
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
            g_unimplemented(guest_pc, instruction);
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
            g_unimplemented(guest_pc, instruction);
            return TX_BREAK;
        }
    } else if (opcode == 0xDA) { // fcmovcc ST0,ST(i) (reg 0/1/2/3 = B/E/BE/U)
        if (reg <= 3) {          // condition from integer EFLAGS
            int jcc = (reg == 0) ? 2 : (reg == 1) ? 4 : (reg == 2) ? 6 : 10; // jb/je/jbe/jp
            int armc = condition_to_arm(jcc);
            hl_x86_emit_flags_load();
            hl_x86_x87_load(18, 0);
            hl_x86_x87_load(16, rm);                                                  // v18=ST0, v16=ST(i)
            emit32(0x1E600C00u | (18 << 16) | ((armc & 0xF) << 12) | (16 << 5) | 17); // fcsel d17, STi, ST0, cond
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
            g_unimplemented(guest_pc, instruction);
            return TX_BREAK;
        }
    } else {
        g_unimplemented(guest_pc, instruction);
        return TX_BREAK;
    }
    return TX_NEXT;
}

static int lower_x87_memory_arithmetic(struct insn *instruction, uint64_t guest_pc, int reg) {
    uint8_t opcode = instruction->op;
    if (opcode != 0xD8 && opcode != 0xDC && opcode != 0xDA && opcode != 0xDE) return TX_FALL;
    // declared memory type -- m32/m64 float (D8/DC) or a SIGNED 32/16-bit integer
    // (DA/DE: the fiadd/fimul/ficom/fisub/fidiv group) -- then share the reg-field
    // arith dispatch below (identical fadd(0)/fmul(1)/fcom(2)/fcomp(3)/fsub(4)/
    // fsubr(5)/fdiv(6)/fdivr(7) encoding for all four opcodes).
    if (opcode == 0xD8) { // m32 float
        hl_x86_emit_load_scalar32(16, 19);
        emit32(0x1E260000u | ((16) << 5) | (20));
        hl_x86_x87_denormal(20, 1);
        emit32(0x1E22C000u | (16 << 5) | 16); // fcvt d16, s16
    } else if (opcode == 0xDA) {              // m32 signed integer
        emit32(0xB9400000u | (19 << 5) | 16); // ldr   w16, [x19]
        emit32(0x1E620000u | (16 << 5) | 16); // scvtf d16, w16
    } else if (opcode == 0xDE) {              // m16 signed integer
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
        hl_x86_x87_add(18, 18, 16);
    else if (reg == 1)
        hl_x86_x87_multiply(18, 18, 16);
    else if (reg == 4)
        hl_x86_x87_subtract(18, 18, 16);
    else if (reg == 5)
        hl_x86_x87_subtract(18, 16, 18);
    else if (reg == 6)
        hl_x86_x87_divide(18, 18, 16);
    else if (reg == 7)
        hl_x86_x87_divide(18, 16, 18);
    else {
        g_unimplemented(guest_pc, instruction);
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
        hl_x86_x87_add(a, a, b);
    else if (reg == 1)
        hl_x86_x87_multiply(a, a, b);
    else if (reg == 4) {
        if (opcode == 0xD8)
            hl_x86_x87_subtract(a, a, b);
        else
            hl_x86_x87_subtract(a, b, a);
    } // DC/DE reverse sub
    else if (reg == 5) {
        if (opcode == 0xD8)
            hl_x86_x87_subtract(a, b, a);
        else
            hl_x86_x87_subtract(a, a, b);
    } else if (reg == 6) {
        if (opcode == 0xD8)
            hl_x86_x87_divide(a, a, b);
        else
            hl_x86_x87_divide(a, b, a);
    } else if (reg == 7) {
        if (opcode == 0xD8)
            hl_x86_x87_divide(a, b, a);
        else
            hl_x86_x87_divide(a, a, b);
    } else {
        g_unimplemented(guest_pc, instruction);
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

// Owns the x87 run boundary as well as opcode dispatch. Every memory form
// materializes the shadow stack before a potentially faulting access or C exit.
int hl_x86_lower_x87(struct insn *instruction, uint64_t guest_pc, uint64_t next,
                     hl_x86_x87_unimplemented_fn unimplemented) {
    if (instruction->op < 0xD8 || instruction->op > 0xDF) return TX_FALL;
    g_unimplemented = unimplemented;
    int reg = instruction->reg & 7;
    int rm = instruction->rm_reg & 7;
    if (instruction->is_mem) {
        hl_x86_x87_materialize();
        emit_ea(instruction, next);
        int bytes = (instruction->op == 0xD8 || instruction->op == 0xDA) ? 4
                    : instruction->op == 0xDC                            ? 8
                    : instruction->op == 0xDE                            ? 2
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
        if (bytes) emit_memory_guard(17, (uint64_t)bytes, guest_pc, store ? X86_SOFT_WRITE : X86_SOFT_READ);
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
