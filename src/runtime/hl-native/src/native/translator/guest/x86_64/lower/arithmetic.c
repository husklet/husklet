#include "arithmetic.h"
#include "primitives.h"
#include "../cpu.h"
#include "../encoding.h"

// MUL/IMUL (group3 F6/F7 /4,/5) set x86 CF=OF when the high half of the product is significant
// (MUL: high half != 0; IMUL: high half != sign-extension of the low half); SF/ZF/AF/PF are
// x86-undefined. cfreg holds the computed CF/OF as 0/1. Write the stored NZCV using the engine's
// borrow convention (stored C = NOT x86 CF at bit 29, OF = V at bit 28) with N=Z=0; scratch x20/x23.
void e_mul_set_oc(int cfreg) {
    e_movconst(23, 1);
    e_rrr(A_EOR, 23, cfreg, 23, 0, 0); // x23 = NOT cf (cf is 0/1)
    e_movconst(20, 0);
    e_rrr(A_ORR, 20, 20, 23, 1, 29);    // stored C (bit 29) = NOT x86 CF
    e_rrr(A_ORR, 20, 20, cfreg, 1, 28); // V (bit 28) = OF = cf
    e_str(20, 28, OFF_NZCV);
    emit32(0xD51B4200u | 20); // msr nzcv, x20 (sync live flags)
}

// Two-/three-operand IMUL publishes SF/ZF from the truncated product (the deterministic
// undefined-flag convention shared with the interpreter) in addition to its defined CF/OF.
// nzreg contains an MRS snapshot whose N/Z bits came from testing that product.
static void e_imul_set_nzoc(int cfreg, int nzreg) {
    e_movconst(20, 3u << 28);
    e_rrr(A_BIC, nzreg, nzreg, 20, 1, 0); // retain N/Z, clear C/V
    e_movconst(23, 1);
    e_rrr(A_EOR, 23, cfreg, 23, 0, 0);     // stored C = NOT x86 CF
    e_rrr(A_ORR, nzreg, nzreg, 23, 1, 29);
    e_rrr(A_ORR, nzreg, nzreg, cfreg, 1, 28);
    e_str(nzreg, 28, OFF_NZCV);
    emit32(0xD51B4200u | (uint32_t)nzreg);
}

// imul reg<-a*b (two-/three-operand forms 0F AF, 69, 6B): truncated product into dst, and x86
// CF=OF = (the full signed product differs from the sign-extension of the truncated result).
// Scratch x21..x25 (x21 carries the 0/1 CF into e_mul_set_oc); callers must not pass a/b in those.
// x86-xflags: when `co_live`==0 the caller proved the WHOLE NZCV word imul defines (hl sets N=Z=0,
// C=NOT CF, V=OF) is dead before any read -> skip the entire overflow/flag synthesis (incl. the extra
// smulh, a real multiply that contends with the product mul on a dependent chain) and emit product-only.
void e_imul2(int dst, int a, int b, int w, int co_live) {
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
    if (w == 2) {
        // The architectural product is the low halfword. Move its sign bit to bit 31 so
        // a 32-bit TST publishes the halfword's SF while still deriving ZF from those 16 bits.
        e_lsl_i(19, 22, 16, 0);
        e_tst(19, 0);
    } else {
        e_tst(dst, w == 8);
    }
    emit32(0xD53B4200u | 19); // mrs x19,nzcv: N/Z from the truncated product
    e_imul_set_nzoc(21, 19);
}

// 8/16-bit one-operand MUL/IMUL (F6/F7 /4,/5) CF=OF: MUL -> the high half is nonzero; IMUL -> the result
// doesn't fit the low half (full signed product != sign-extension of the low `w` bytes). SF/ZF/AF/PF are
// x86-undefined. `prod` holds the product (2*w bytes) in a 32-bit reg; k==4 MUL / k==5 IMUL. Scratch
// x22/x23 (+ e_mul_set_oc's x20/x23); leaves `prod` intact.
void e_mul_oc_narrow(int prod, int k, int w) {
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
// PF/AF lanes. `cnt` is the (already masked, nonzero) immediate count -> OF written iff cnt==1. Scratch x19..x23 plus x27 (host x18 is reserved on Darwin).
void e_rot_flags_const(int res, int k, int width, int cnt) {
    int wsf = width == 64;
    e_ldr(27, 28, OFF_NZCV);
    e_lsr_i(20, res, k == 1 ? width - 1 : 0, wsf);
    e_movconst(21, 1);
    e_rrr(A_AND, 20, 20, 21, 0, 0); // x20 = x86 CF (0/1)
    e_movconst(21, 1u << 29);
    e_rrr(A_BIC, 27, 27, 21, 1, 0); // clear stored C
    e_movconst(21, 1);
    e_rrr(A_EOR, 22, 20, 21, 0, 0);  // x22 = NOT CF
    e_rrr(A_ORR, 27, 27, 22, 1, 29); // stored C = (NOT CF) << 29
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
        e_rrr(A_BIC, 27, 27, 21, 1, 0);  // clear V
        e_rrr(A_ORR, 27, 27, 22, 1, 28); // V = OF
    }
    e_str(27, 28, OFF_NZCV);
    emit32(0xD51B4200u | 27); // msr nzcv, x27 (sync live flags)
}

// ROL/ROR by CL: like e_rot_flags_const but the count is runtime (n = CL & (width-1)). When n==0 x86
// changes NO flags, so keep the old NZCV; otherwise set CF (and OF via the 1-bit formula -- for n>1 OF is
// x86-undefined, so emitting that legal value is fine). Reads CL (RCX); scratch x19..x25 plus x27 (host x18 is reserved on Darwin).
void e_rot_flags_cl(int res, int k, int width) {
    int wsf = width == 64;
    // "flags affected?" is decided by the 5-bit (0x1f) / 6-bit (0x3f, REX.W) masked count -- NOT the
    // rotate amount (count MOD width). For 8/16-bit rotates these differ: e.g. `rolb %cl` with CL=8 rotates
    // by 8%8==0 (value unchanged) but (CL&0x1f)==8!=0 so x86 DOES set CF = LSB(result). Masking by width-1
    // here (7 for a byte) wrongly took the count==0 keep-old path and left stale CF. Use the true x86 cmask;
    // for width 32/64 this is width-1 (unchanged), so only byte/word behavior moves.
    e_movconst(19, (width == 64) ? 63 : 31);
    e_rrr(A_ANDS, 24, RCX, 19, wsf, 0); // x24 = n = CL & cmask (x86 5/6-bit); Z = (n == 0) -> flags unchanged
    e_ldr(27, 28, OFF_NZCV);            // old NZCV (kept when n == 0)
    e_lsr_i(20, res, k == 1 ? width - 1 : 0, wsf);
    e_movconst(21, 1);
    e_rrr(A_AND, 20, 20, 21, 0, 0); // x20 = CF
    e_mov_rr(25, 27, 1);            // candidate = copy of old NZCV
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
    // all ops since the ANDS are flag-free, so its Z survives: n==0 -> keep old (x27), else candidate (x25).
    e_csel(27, 27, 25, 0 /*EQ*/, 1);
    e_str(27, 28, OFF_NZCV);
    emit32(0xD51B4200u | 27); // msr nzcv, x27 (sync live flags)
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
void alu_core(int kind, int out, int rn, int rm, int sf) {
    switch (kind) {
    case 0: e_rrr(A_ADDS, out, rn, rm, sf, 0); break; // add
    case 4: e_rrr(A_ANDS, out, rn, rm, sf, 0); break; // and / test
    case 5: e_rrr(A_SUBS, out, rn, rm, sf, 0); break; // sub / cmp
    case 1:
        e_rrr(A_ORR, out, rn, rm, sf, 0); // or
        emit32((sf ? 0xEA00001Fu : 0x6A00001Fu) | ((uint32_t)out << 16) | ((uint32_t)out << 5));
        break; // tst
    case 6:
        e_rrr(A_EOR, out, rn, rm, sf, 0); // xor
        emit32((sf ? 0xEA00001Fu : 0x6A00001Fu) | ((uint32_t)out << 16) | ((uint32_t)out << 5));
        break;
    default: break;
    }
}
