#include "avx_inline.h"

#include "execution_control.h"
#include "primitives.h"
#include "../cpu.h"
#include "../encoding.h"
#include "../glue.h"

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
    hl_x86_emit_vector_load128(t, 16, 0);
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
void emit_ps2dq_128(int out, int sf, int trunc, int c2p31, int cindef, int t1, int t2) {
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
void emit_pd2i32_pieces(int r, int m, int sd, int trunc, int c2p31d, int cneg2p31d, int t1, int t2) {
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
        hl_x86_emit_vector_dirty();
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
            hl_x86_emit_vector_dirty();
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
            hl_x86_emit_vector_dirty();
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
                    hl_x86_emit_store_scalar32(d, 17);
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
            hl_x86_emit_vector_dirty();
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
        hl_x86_emit_vector_dirty();
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
        hl_x86_emit_vector_dirty();
        if (I->is_mem) {
            emit_ea(I, next);
            if (emit_soft_memory_active()) {
                emit_memory_guard(17, l256 ? 32u : 16u, next - (uint64_t)I->len, X86_SOFT_WRITE);
                hl_x86_emit_memory_barrier();
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
            hl_x86_emit_vector_dirty();
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
            uint32_t *p_cbz = hl_x86_emit_cursor();
            emit32(0);                                       // cbz x16, Lfast  (patched below)
            emit_exit_const(next - (uint64_t)I->len, R_AVX); // NaN present -> emulate this insn in C (this insn's rip)
            uint8_t *Lfast = (uint8_t *)hl_x86_emit_cursor();
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
            hl_x86_emit_fma_group(rA, rB, rC, 23, 24, 25, neg, fmls, dbl); // low 128 -> v23
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
                hl_x86_emit_fma_group(hA, hB, hC, 21, 22, 25, neg, fmls, dbl); // high 128 -> v21
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
        hl_x86_emit_vector_dirty();
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
        uint32_t *p_cbz = hl_x86_emit_cursor();
        emit32(0);                                       // cbz x16, Lfast  (patched below)
        emit_exit_const(next - (uint64_t)I->len, R_AVX); // NaN present -> emulate this insn in C (this rip)
        uint8_t *Lfast = (uint8_t *)hl_x86_emit_cursor();
        *p_cbz = 0xB4000000u | ((uint32_t)(((Lfast - (uint8_t *)p_cbz) / 4) & 0x7FFFF) << 5) | 16;

        // ---- fast path: no input NaN ----
        hl_x86_emit_vex_fp(d, s1, s2, op, dbl); // low 128 -> host v[d]
        if (l256) {
            hl_x86_emit_vex_fp(22, 20, 21, op, dbl); // high 128 -> v22 (highs in v20/v21)
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
        hl_x86_emit_vector_dirty();
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
        hl_x86_emit_vector_dirty();
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
        hl_x86_emit_vector_dirty();
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
        hl_x86_emit_vector_dirty();
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
        hl_x86_emit_vector_dirty();
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
        hl_x86_emit_vector_dirty();
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
            hl_x86_emit_vector_dirty();
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
        hl_x86_emit_vector_dirty();
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
        hl_x86_emit_vector_dirty();
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
        hl_x86_emit_vector_dirty();
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
        hl_x86_emit_vector_dirty();
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
        hl_x86_emit_vector_dirty();
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
        hl_x86_emit_vector_dirty();
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
        hl_x86_emit_vector_dirty();
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
        hl_x86_emit_vector_dirty();
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
        hl_x86_emit_vector_dirty();
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
        hl_x86_emit_vector_dirty();
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
        hl_x86_emit_vector_dirty();
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

    hl_x86_emit_vector_dirty();
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
int hl_x86_lower_avx_inline(struct insn *I, uint64_t next) {
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
