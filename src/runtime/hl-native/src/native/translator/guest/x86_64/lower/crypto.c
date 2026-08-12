#include "crypto.h"

#include "x87.h"

#include "../cpu.h"
#include "../encoding.h"

#include <stdint.h>

// translator/guest/x86_64 -- crypto instruction class: map the x86 AES-NI / PCLMULQDQ /
// SHA-NI opcodes to the ARMv8 crypto extension INLINE, instead of exiting the block to the C softmulator
// (do_sse3b). The per-instruction block-exit + re-decode + scalar C emulation runs the openssl AES-GCM /
// SHA-256 inner loops ~100x slower than native; emitting the native ARM crypto instructions in-line runs
// them at ~hardware speed and keeps the block chainable.
//
// Guest xmm0..15 live in host v0..v15 (emit.c). Scratch: v16 (zero / A-half), v17 (state / B-half),
// v19 (memory-operand load). emit_ea leaves the effective address in GPR x17.
//
// SEMANTIC MAPPING (x86 fuses steps in one op with a fixed order; ARM splits them and XORs the round key
// at a DIFFERENT point, so the composition below is exact, not a naive 1:1):
//
//   x86 AESENC     d,s = MixColumns(SubBytes(ShiftRows(d))) ^ s        [s = round key]
//   x86 AESENCLAST d,s =            SubBytes(ShiftRows(d))  ^ s
//   ARM AESE  t,k      = SubBytes(ShiftRows(t ^ k))     (key XOR BEFORE the round)
//   ARM AESMC t        = MixColumns(t)
//   => AESENC: AESE(t=d, k=0); AESMC(t); d = t ^ s      (give AESE a ZERO key, XOR the real key after)
//      AESENCLAST: AESE(t=d, k=0);        d = t ^ s
//
//   x86 AESDEC     d,s = InvMixColumns(InvSubBytes(InvShiftRows(d))) ^ s
//   x86 AESDECLAST d,s =               InvSubBytes(InvShiftRows(d))  ^ s
//   ARM AESD  t,k      = InvSubBytes(InvShiftRows(t ^ k))
//   ARM AESIMC t       = InvMixColumns(t)
//   => AESDEC: AESD(t=d, k=0); AESIMC(t); d = t ^ s
//      AESDECLAST: AESD(t=d, k=0);         d = t ^ s
//
//   x86 AESIMC d,s = InvMixColumns(s)   => ARM AESIMC d, s   (direct 1:1, non-destructive)
//
//   x86 PCLMULQDQ d,s,imm8 = clmul( half(d, imm8[0]), half(s, imm8[4]) ) -> 128-bit
//   ARM PMULL Vd.1Q, Vn.1D, Vm.1D = clmul of lane 0 of each  => stage the two chosen 64-bit halves into
//   lane 0 of v16/v17, then PMULL d, v16, v17.

// ---- ARM crypto instruction encodings (Rd in [4:0], Rn in [9:5], Rm in [20:16]) ----
#define A_AESE 0x4E284800u   // AESE  Vd.16B, Vn.16B  = SubBytes(ShiftRows(Vd ^ Vn))
#define A_AESD 0x4E285800u   // AESD  Vd.16B, Vn.16B  = InvSubBytes(InvShiftRows(Vd ^ Vn))
#define A_AESMC 0x4E286800u  // AESMC Vd.16B, Vn.16B  = MixColumns(Vn)
#define A_AESIMC 0x4E287800u // AESIMC Vd.16B, Vn.16B = InvMixColumns(Vn)
#define A_EOR16 0x6E201C00u  // EOR   Vd.16B, Vn.16B, Vm.16B  (via e_v3)
#define A_PMULL 0x0EE0E000u  // PMULL Vd.1Q, Vn.1D, Vm.1D     (poly multiply, low lanes)
#define A_PMULL2 0x4EE0E000u // PMULL2 Vd.1Q, Vn.2D, Vm.2D    (poly multiply, HIGH lanes)
#define A_TBL 0x4E000000u    // TBL   Vd.16B, {Vn.16B}, Vm.16B  (byte table lookup; idx>=16 -> 0)
#define A_BIT 0x6EA01C00u    // BIT   Vd.16B, Vn.16B, Vm.16B  = Vd ^ ((Vd ^ Vn) & Vm)  (Vm bit set -> Vn)

static uint32_t crypto_reg(int value) {
    return (uint32_t)value;
}

// r/m operand -> vector register number (v19 when memory-backed; EA computed into x17 by emit_ea).
static int crypto_rm_vec(struct insn *I, uint64_t next) {
    if (I->is_mem) {
        emit_ea(I, next);
        emit_memory_guard(17, 16, next - (uint64_t)I->len, X86_SOFT_READ);
        hl_x86_emit_vector_load128(19, 17, 0);
        return 19;
    }
    return I->rm_reg;
}

// ---- SSSE3/SSE4 "shuffle glue" (task perf-lever-#1) -------------------------------------------------
// The AES-GCM / SHA-NI inner loops interleave byte-shuffle ops (pshufb byte-swap of the CTR counter and
// GHASH input, palignr in the SHA message schedule, pblend/pmov widenings) with the crypto ops. Left to
// the C softmulator (do_sse3b) each one is a full block-exit (spill 16 GPR+16 vec+nzcv, C call, re-decode,
// scalar emulate, reload) EVERY 16-byte block -- shattering the chainable crypto loop. Lowering them inline
// (same recipe as) keeps the loop in translated code. Gated by NOSSEOPT for A/B (falls to do_sse3b).

// MOVI Vd.16B, #imm8   (materialize a per-byte constant)
static void e_movi16b(int vd, unsigned imm8) {
    emit32(0x4F00E400u | ((imm8 & 0xe0u) << 11) | ((imm8 & 0x1fu) << 5) | crypto_reg(vd));
}

// INS Vd.H[di], Vn.H[si]  (copy one 16-bit lane)
static void e_ins_h(int vd, int di, int vn, int si) {
    emit32(0x6E000400u | ((unsigned)(((di << 2) | 2)) << 16) | ((unsigned)(si << 1) << 11) | (crypto_reg(vn) << 5) |
           crypto_reg(vd));
}

// INS Vd.<T>[i], Rn (general): imm5 selects lane size+index. nb = element bytes (1/2/4/8).
static void crypto_ins_g(int vd, int i, int rn, int nb) {
    unsigned imm5 = (nb == 1)   ? ((unsigned)i << 1) | 1u
                    : (nb == 2) ? ((unsigned)i << 2) | 2u
                    : (nb == 4) ? ((unsigned)i << 3) | 4u
                                : ((unsigned)i << 4) | 8u;
    emit32(0x4E001C00u | (imm5 << 16) | ((unsigned)rn << 5) | (unsigned)vd);
}

static void crypto_vconst16(int vd, uint64_t lo, uint64_t hi) {
    e_movconst(16, lo);
    e_fmov_to_d(vd, 16);
    e_movconst(16, hi);
    crypto_ins_g(vd, 1, 16, 8);
}

// sized FP/SIMD loads from [x17] (byte / halfword) -- for pmov narrow memory operands (avoid 16B over-read)
static void e_ldr_h(int t, int rn) {
    emit32(0x7D400000u | (crypto_reg(rn) << 5) | crypto_reg(t));
} // ldr h<t>,[xn]

// SXTL/UXTL Vd.<2*e> , Vn.<e>  (widen the low half one size step; immh selects source element size).
//   immh_w: 0x080000=8B->8H, 0x100000=4H->4S, 0x200000=2S->2D.  sgn: 1=SXTL (sign), 0=UXTL (zero).
static void e_xtl(int vd, int vn, unsigned immh_w, int sgn) {
    emit32((sgn ? 0x0F00A400u : 0x2F00A400u) | immh_w | (crypto_reg(vn) << 5) | crypto_reg(vd));
}

// ---- SHA-NI (SHA-1 + SHA-256) -> ARMv8 SHA extension, per instruction (near 1:1) -------------------
//
// x86 SHA-NI and the ARM SHA extension pack the state DIFFERENTLY, so the map needs (a) pure lane
// permutations at instruction entry/exit and (b) for SHA256RNDS2 one algebraic observation. Everything
// below is a per-instruction lowering -- no multi-instruction pattern matching, so ANY scheduling
// (openssl's hand asm, gcc/clang intrinsics with branches between the round ops, aliased operands,
// memory operands) is handled by construction. Lane k below = dword bits [32k+31 : 32k].
//
// SHA-256 layouts:
//   x86 SHA256RNDS2 dst,src (implicit xmm0): dst-in lanes (0..3) = (H,G,D,C), src = (F,E,B,A),
//     xmm0 = (WK0,WK1,-,-). Two rounds; dst-out = (F2,E2,B2,A2).
//   ARM SHA256H  Qd,Qn,Vm.4S: Qd = X = (A,B,C,D), Qn = Y = (E,F,G,H), Vm = (WK0..WK3);
//     FOUR rounds, round e consumes Vm lane e; returns X4. SHA256H2 Qd(=Y),Qn(=ORIGINAL X),Vm
//     returns Y4. (ARM ARM SHA256hash: chs=Ch(Y0,Y1,Y2), majs=Maj(X0,X1,X2), t=Y3+S1(Y0)+chs+W[e],
//     X3+=t, Y3=t+S0(X0)+majs, then <Y,X> = ROL(Y:X,32) -- i.e. A lives in X lane 0, E in Y lane 0.)
//
// KEY OBSERVATION (2 x86 rounds via ARM's 4-round instruction): SHA-256's working vars form a shift
// register -- B_{r+1}=A_r, C_{r+1}=B_r, D_{r+1}=C_r and F_{r+1}=E_r, G_{r+1}=F_r, H_{r+1}=G_r. Hence
// after 4 ARM rounds X4 = (A4,A3,A2,A1) and Y4 = (E4,E3,E2,E1). The values x86 needs -- A2,A1,E2,E1 --
// are finalized by rounds 0..1, which consume ONLY WK lanes 0,1; rounds 2,3 merely shift them along.
// So we can run SHA256H/H2 with xmm0 as-is (lanes 2,3 hold garbage that only pollutes A4,A3,E4,E3,
// which we discard) and read the 2-round result out of lanes 2,3 of each output.
//
// Layout transforms (pure permutations; rev64.4s swaps lanes within each 64-bit half,
// zip1.2d(a,b)=(a.d0,b.d0), zip2.2d(a,b)=(a.d1,b.d1)):
//   entry: rev64(src=(F,E,B,A)) = (E,F,A,B); rev64(dst=(H,G,D,C)) = (G,H,C,D)
//          X0 = (A,B,C,D) = zip2.2d(rev64 src, rev64 dst);  Y0 = (E,F,G,H) = zip1.2d(rev64 src, rev64 dst)
//   exit:  rev64(Y4=(E4,E3,E2,E1)) = (E3,E4,E1,E2); rev64(X4) = (A3,A4,A1,A2)
//          dst-out = (F2,E2,B2,A2) = (E1,E2,A1,A2) = zip2.2d(rev64 Y4, rev64 X4)          [10 insns total]
//
//   x86 SHA256MSG1 dst,src: lane e of dst-out = W_e + sigma0(W_{e+1}), W_e = dst lane e, W4 = src lane 0.
//   ARM SHA256SU0 Vd,Vn: T = (Vd1,Vd2,Vd3,Vn0); result e = Vd_e + sigma0(T_e). IDENTICAL -- 1 insn.
//
//   x86 SHA256MSG2 dst,src: r0 = d0+sig1(s2); r1 = d1+sig1(s3); r2 = d2+sig1(r0); r3 = d3+sig1(r1).
//   ARM SHA256SU1 Vd,Vn,Vm: r_e = Vd_e + T0_e + sig1(m2,m3,r0,r1 chain), T0 = (Vn1,Vn2,Vn3,Vm0).
//   With Vn = 0: T0 = (0,0,0,src0) -- everything matches except an extra +src0 in lane 3, which we
//   cancel by pre-subtracting (0,0,0,src0) from dst (lane 3 feeds no sigma1 chain, so this is exact).
//
// SHA-1 layouts (state (A,B,C,D) with A in x86 lane 3 / ARM lane 0; full lane REVERSAL both ways;
// reversal = rev64.4s + ext #8):
//   x86 SHA1RNDS4 dst,src,imm2: dst-in = (D,C,B,A), src = (W3,W2,W1,W0+E), E0=0, K_imm2 added
//     internally each round; dst-out = (D4,C4,B4,A4).
//   ARM SHA1C/SHA1P/SHA1M Qd,Sn,Vm.4S: Qd = X = (A,B,C,D) lanes 0..3, Sn = E0 scalar, Vm = per-round
//     addends (must be W+K: ARM adds Vm lane e in round e, no internal K). Round fn: C=Ch, P=Parity,
//     M=Maj -- x86 imm2 0/1/2/3 selects Ch/Parity/Maj/Parity with K 5A827999/6ED9EBA1/8F1BBCDC/CA62C1D6.
//   => X = reverse(dst); WK = reverse(src) + dup(K); E0 = 0; sha1{c,p,m}; dst = reverse(X').
//   x86 SHA1NEXTE dst,src: out = src + (0,0,0,rol30(dst3)). ARM SHA1H Sd,Sn = rol30 scalar.
//   x86 SHA1MSG1 dst,src: out = dst ^ (s2,s3,d0,d1) = dst ^ ext(src,dst,#8) -- 2 insns.
//   x86 SHA1MSG2 dst,src: r3=rol1(d3^s2); r2=rol1(d2^s1); r1=rol1(d1^s0); r0=rol1(d0^r3).
//   ARM SHA1SU1 Vd,Vn: T = Vd ^ (Vn>>32 as 128-bit) = (d0^n1, d1^n2, d2^n3, d3); out lanes 0..2 =
//     rol1(T_e), lane 3 = rol1(T3) ^ rol2(T0) = rol1(T3 ^ rol1(T0)) (rol distributes over xor).
//     With Vd = reverse(dst), Vn = reverse(src): T = (d3^s2, d2^s1, d1^s0, d0) -- exactly the x86
//     recurrence in reversed lanes (the internal lane-3 chain = x86's lane-0 chain). Reverse back out.
//
// All verified bit-for-bit against the C softmulator semantics (avx.c 0xC8..0xCD/0xCC) and the qemu
// oracle by the comp-x86-crypto sha/sha-kat differential guests (random vectors + FIPS-180 KATs).
#define A_AND16 0x4E201C00u
#define A_ADD4S 0x4EA08400u
#define A_SUB4S 0x6EA08400u
#define A_SHA1C 0x5E000000u     // SHA1C     Qd, Sn, Vm.4S
#define A_SHA1P 0x5E001000u     // SHA1P     Qd, Sn, Vm.4S
#define A_SHA1M 0x5E002000u     // SHA1M     Qd, Sn, Vm.4S
#define A_SHA256H 0x5E004000u   // SHA256H   Qd, Qn, Vm.4S
#define A_SHA256H2 0x5E005000u  // SHA256H2  Qd, Qn, Vm.4S
#define A_SHA256SU1 0x5E006000u // SHA256SU1 Vd.4S, Vn.4S, Vm.4S
#define A_SHA1H 0x5E280800u     // SHA1H     Sd, Sn
#define A_SHA1SU1 0x5E281800u   // SHA1SU1   Vd.4S, Vn.4S
#define A_SHA256SU0 0x5E282800u // SHA256SU0 Vd.4S, Vn.4S
#define A_REV64_4S 0x4EA00800u  // REV64 Vd.4S, Vn.4S (swap lanes within each 64-bit half)
#define A_ZIP1_2D 0x4EC03800u   // ZIP1 Vd.2D, Vn.2D, Vm.2D = (Vn.d0, Vm.d0)
#define A_ZIP2_2D 0x4EC07800u   // ZIP2 Vd.2D, Vn.2D, Vm.2D = (Vn.d1, Vm.d1)

// DUP Vd.4S, Wn  (broadcast a GPR)
static void e_dup_w4s(int vd, int rn) {
    emit32(0x4E040C00u | (crypto_reg(rn) << 5) | crypto_reg(vd));
}

static void e_rev64_4s(int vd, int vn) {
    emit32(A_REV64_4S | (crypto_reg(vn) << 5) | crypto_reg(vd));
}

// full .4s lane reversal (a,b,c,d) -> (d,c,b,a): rev64 swaps within halves, ext #8 swaps the halves.
static void e_rev4s(int vd, int vn) {
    e_rev64_4s(vd, vn);
    hl_x86_emit_vector_extract(vd, vd, vd, 8);
}

// AES-endgame perf: straight-line hoisting of the two loop-invariant scratch constants the crypto glue
// re-materializes per instruction: the ZERO round key the AESENC/AESDEC mapping feeds AESE/AESD, and the
// 0x8f control mask PSHUFB ANDs into its TBL index. openssl's hot loops run 60+ back-to-back aesenc and
// 4 pshufb per ghash iteration -- re-emitting the constant each time wastes an insn AND a rename
// dependency per op. The constants live in v26 (zero) / v27 (0x8f): v20..v31 are used by NOTHING in the
// x86 translator except the SHA-NI cases below (verified by sweep), so the claims survive the ordinary
// legacy-SSE/GPR lowerings interleaved in real loops (all scratch there is x16..x25 / v16..v19).
//   state->zero_ready / state->mask_ready == 1  =>  the emitted code at this point provably left v26 == 0 / v27 ==
//   0x8f.
// Cleared at: translate_block entry, the SHA-NI cases (clobber v20+), and the rep-movs/stos string
// idiom (its ERMS funnel `blr`s a host helper, which may clobber all of v16..v31). Stitched superblock
// constituents are never entered externally (no map entry is registered for them) and every C-emulation
// transition ends the emitted block, so the straight-line assumption holds everywhere else.
// NOSSEOPT=1 disables the hoist (constants re-emitted every op -- the pre-hoist codegen) for A/B.
// Try to lower a legacy 0F38/0F3A crypto opcode to inline ARM crypto. Returns TX_NEXT if emitted,
// TX_FALL to defer to the C softmulator (do_sse3b) for the rare ops we don't map (pcmpistri, ...).
int hl_x86_lower_crypto(struct insn *I, uint64_t next, hl_x86_crypto_state *state) {
    uint8_t op = I->op;
    int D = I->reg; // dst xmm == src1 (destructive) for AES/PCLMUL

    if (I->map3 == 3 && op == 0xDF) { // AESKEYGENASSIST xmm, xmm/m128, imm8
        if (hl_x86_x87_known()) hl_x86_x87_drop();
        int s = crypto_rm_vec(I, next);
        if (!state->zero_ready || !state->optimize) hl_x86_emit_vector3(A_EOR16, 26, 26, 26);
        state->zero_ready = 1;
        hl_x86_emit_vector_copy(17, s);
        emit32(A_AESE | (26 << 5) | 17);
        crypto_vconst16(16, UINT64_C(0x040B0E010B0E0104), UINT64_C(0x0C0306090306090C));
        emit32(A_TBL | (16 << 16) | (17 << 5) | 17);
        hl_x86_emit_vector3(A_EOR16, 16, 16, 16);
        uint32_t rcon = (uint32_t)((uint64_t)I->imm & UINT64_C(0xff));
        if (rcon) {
            e_movz(16, rcon, 0);
            crypto_ins_g(16, 1, 16, 4);
            crypto_ins_g(16, 3, 16, 4);
        }
        hl_x86_emit_vector3(A_EOR16, D, 17, 16);
        return TX_NEXT;
    }

    if (I->map3 == 2) { // 0F38
        switch (op) {
        case 0xDC:   // AESENC
        case 0xDD:   // AESENCLAST
        case 0xDE:   // AESDEC
        case 0xDF: { // AESDECLAST
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int key = crypto_rm_vec(I, next);
            int enc = (op == 0xDC || op == 0xDD);
            int last = (op == 0xDD || op == 0xDF);
            uint32_t aes = enc ? A_AESE : A_AESD, mc = enc ? A_AESMC : A_AESIMC;
            if (!state->zero_ready || !state->optimize)
                hl_x86_emit_vector3(A_EOR16, 26, 26, 26); // v26 = 0 ("zero round key"); hoisted across runs
            state->zero_ready = 1;                        // v26 stays zero through this case (AESE/AESD only READ it)
            if (key == D) {
                // AESENC xmm,xmm aliases state==key: the ^key reads the ORIGINAL state, so keep D intact
                // and compute through scratch v17 (5 insns).
                hl_x86_emit_vector_copy(17, D);
                emit32(aes | (26 << 5) | 17);
                if (!last) emit32(mc | (17 << 5) | 17);
                hl_x86_emit_vector3(A_EOR16, D, 17, key);
            } else {
                // hot path (key != state): transform D in place, XOR the round key after (3 insns).
                emit32(aes | (26 << 5) | crypto_reg(D)); // D = Sub(Shift(D ^ 0))
                if (!last) emit32(mc | (crypto_reg(D) << 5) | crypto_reg(D));
                hl_x86_emit_vector3(A_EOR16, D, D, key); // d = MixColumns(...) ^ round key
            }
            return TX_NEXT;
        }
        case 0xDB: { // AESIMC: d = InvMixColumns(s)  (non-destructive)
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int s = crypto_rm_vec(I, next);
            emit32(A_AESIMC | (crypto_reg(s) << 5) | crypto_reg(D));
            return TX_NEXT;
        }
        case 0xC8: { // SHA1NEXTE d, s: d = s + (0,0,0, rol30(d.lane3))   [E folded into the next W0]
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            state->zero_ready = state->mask_ready = 0; // the SHA round lowering clobbers v20..v31
            int s = crypto_rm_vec(I, next);
            hl_x86_emit_vector_broadcast32(16, D, 3); // broadcast d.lane3 (the A from 4 rounds ago)
            emit32(A_SHA1H | (16 << 5) | 16);         // s16 = rol30(d3)
            e_movi16b(17, 0);
            hl_x86_emit_vector_insert32(17, 3, 16, 0); // v17 = (0,0,0,rol30(d3))
            hl_x86_emit_vector3(A_ADD4S, D, s, 17);    // lanes 0..2 = s passthrough, lane 3 = s3 + rol30(d3)
            return TX_NEXT;
        }
        case 0xC9: { // SHA1MSG1 d, s: d ^= (s2,s3,d0,d1)  [W xor of the schedule, no rotate]
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int s = crypto_rm_vec(I, next);
            hl_x86_emit_vector_extract(16, s, D, 8);    // v16 = (s2,s3,d0,d1)
            hl_x86_emit_vector3(0x6E201C00u, D, D, 16); // eor
            return TX_NEXT;
        }
        case 0xCA: { // SHA1MSG2 d, s: rol1 schedule finish with the lane-3 -> lane-0 chain (see header)
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int s = crypto_rm_vec(I, next);
            e_rev4s(16, D);                     // v16 = (d3,d2,d1,d0)
            e_rev4s(17, s);                     // v17 = (s3,s2,s1,s0)
            emit32(A_SHA1SU1 | (17 << 5) | 16); // v16 = (r3,r2,r1,r0): T=(d3^s2,d2^s1,d1^s0,d0), chain in lane 3
            e_rev4s(D, 16);                     // d = (r0,r1,r2,r3) back in x86 lane order
            return TX_NEXT;
        }
        case 0xCB: { // SHA256RNDS2 d, s, <xmm0>: 2 rounds via ARM SHA256H/H2 (see header derivation).
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int s = crypto_rm_vec(I, next);
            e_rev64_4s(16, s);                               // v16 = (E,F,A,B)
            e_rev64_4s(17, D);                               // v17 = (G,H,C,D)
            hl_x86_emit_vector3(A_ZIP2_2D, 20, 16, 17);      // v20 = X0 = (A,B,C,D)
            hl_x86_emit_vector3(A_ZIP1_2D, 21, 16, 17);      // v21 = Y0 = (E,F,G,H)
            hl_x86_emit_vector_copy(16, 20);                 // save X0 for SHA256H2
            emit32(A_SHA256H | (0 << 16) | (21 << 5) | 20);  // v20 = X4 = (A4,A3,A2,A1); WK = xmm0(v0),
            emit32(A_SHA256H2 | (0 << 16) | (16 << 5) | 21); // v21 = Y4 = (E4,E3,E2,E1); lanes2,3 garbage-OK
            e_rev64_4s(16, 21);                              // v16 = (E3,E4,E1,E2)
            e_rev64_4s(17, 20);                              // v17 = (A3,A4,A1,A2)
            hl_x86_emit_vector3(A_ZIP2_2D, D, 16, 17);       // d = (E1,E2,A1,A2) = (F2,E2,B2,A2)
            return TX_NEXT;
        }
        case 0xCC: { // SHA256MSG1 d, s: exactly ARM SHA256SU0 (see header)
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int s = crypto_rm_vec(I, next);
            emit32(A_SHA256SU0 | (crypto_reg(s) << 5) | crypto_reg(D));
            return TX_NEXT;
        }
        case 0xCD: { // SHA256MSG2 d, s: ARM SHA256SU1 with Vn=0 + lane-3 pre-cancel (see header)
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int s = crypto_rm_vec(I, next);
            if (s == D) { // aliased d==s: SU1 must read the ORIGINAL src after we adjust d
                hl_x86_emit_vector_copy(20, s);
                s = 20;
            }
            e_movi16b(16, 0); // v16 = 0 (the SU1 Vn operand)
            e_movi16b(17, 0);
            hl_x86_emit_vector_insert32(17, 3, s, 0); // v17 = (0,0,0,s0)
            hl_x86_emit_vector3(A_SUB4S, D, D, 17);   // cancel SU1's T0 lane-3 (+s0) contribution
            emit32(A_SHA256SU1 | (crypto_reg(s) << 16) | (16 << 5) | crypto_reg(D));
            return TX_NEXT;
        }
        // ---- SSE4.1 packed integer MIN/MAX + 32-bit MUL: each is a single lane-wise NEON op, bit-exact
        // with the C softmulator (do_sse3b). These fire heavily in autovectorized integer loops (pmulld) and
        // SIMD min/max reductions -- 20M+ per-insn C round-trips each in a real SSE4 workload. D = ModRM.reg
        // is src1 = dst (destructive); s = ModRM.r/m. Gated by state->optimize (NOSSEOPT -> do_sse3b A/B).
        case 0x38:   // PMINSB  d,s   (signed byte)
        case 0x39:   // PMINSD  d,s   (signed dword)
        case 0x3A:   // PMINUW  d,s   (unsigned word)
        case 0x3B:   // PMINUD  d,s   (unsigned dword)
        case 0x3C:   // PMAXSB  d,s   (signed byte)
        case 0x3D:   // PMAXSD  d,s   (signed dword)
        case 0x3E:   // PMAXUW  d,s   (unsigned word)
        case 0x3F:   // PMAXUD  d,s   (unsigned dword)
        case 0x40: { // PMULLD  d,s   (32-bit low product)
            if (!state->optimize) return TX_FALL;
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int s = crypto_rm_vec(I, next);
            uint32_t enc;
            switch (op) {
            case 0x38: enc = 0x4E206C00u; break; // SMIN .16B
            case 0x3C: enc = 0x4E206400u; break; // SMAX .16B
            case 0x3A: enc = 0x6E606C00u; break; // UMIN .8H
            case 0x3E: enc = 0x6E606400u; break; // UMAX .8H
            case 0x39: enc = 0x4EA06C00u; break; // SMIN .4S
            case 0x3D: enc = 0x4EA06400u; break; // SMAX .4S
            case 0x3B: enc = 0x6EA06C00u; break; // UMIN .4S
            case 0x3F: enc = 0x6EA06400u; break; // UMAX .4S
            default: enc = 0x4EA09C00u; break;   // 0x40 MUL .4S (low 32-bit product)
            }
            hl_x86_emit_vector3(enc, D, D, s);
            return TX_NEXT;
        }
        case 0x17: { // PTEST d, s (SSE4.1): read-only flag-setter feeding a Jcc/setcc/cmov.
            // ZF = ((d & s) == 0); CF = ((s & ~d) == 0); SF = OF = AF = PF = 0.  D = ModRM.reg (xmm1),
            // s = ModRM.r/m. Autovectorized all-zero / all-ones tests (`ptest x,x; jz/jnz`) fire it 50M+
            // times in an SSE4 workload, each a full R_SSE3B dispatcher round-trip. Bit-exact with the C
            // softmulator (avx.c do_sse3b PTEST): the x86 flag substrate stores ARM Z for x86 ZF and ARM C
            // = NOT x86 CF (bit29); SF(bit31)/OF(bit28) stay 0; PF/AF live in cpu->pf / cpu->af. Because we
            // write cpu->nzcv AND the live ARM NZCV here (and set no g_fl_pending), the immediately
            // following Jcc reads the correct flags off live NZCV, exactly as pcmpistri does.
            if (!state->optimize) return TX_FALL;
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int s = crypto_rm_vec(I, next);
            // ZF: is (D & s) all-zero?  AND -> v16; UMAXV reduces to the max byte (== 0 iff every bit is 0).
            hl_x86_emit_vector3(0x4E201C00u, 16, D, s); // AND   v16.16b, D.16b, s.16b   = D & s
            emit32(0x6E30A800u | (16 << 5) | 16);       // UMAXV b16, v16.16b
            emit32(0x0E013C00u | (16 << 5) | 18);       // UMOV  w18, v16.b[0]   x18 = max byte of (D & s)
            // CF: is (s & ~D) all-zero?  BIC v17 = s AND NOT D.
            hl_x86_emit_vector3(0x4E601C00u, 17, s, D); // BIC   v17.16b, s.16b, D.16b   = s & ~D
            emit32(0x6E30A800u | (17 << 5) | 17);       // UMAXV b17, v17.16b
            emit32(0x0E013C00u | (17 << 5) | 19);       // UMOV  w19, v17.b[0]   x19 = max byte of (s & ~D)
            // ZF = (x18 == 0) -> ARM Z (bit30); stored C (bit29) = NOT x86 CF = (x19 != 0).
            e_subi_s(31, 18, 0, 0);          // subs wzr, w18, #0
            e_cset(20, 0, 1);                // x20 = (AND all-zero) = ZF          (EQ)
            e_subi_s(31, 19, 0, 0);          // subs wzr, w19, #0
            e_cset(21, 1, 1);                // x21 = (s & ~D nonzero) = NOT CF = stored C   (NE)
            e_lsl_i(20, 20, 30, 1);          // ZF -> bit30
            e_rrr(A_ORR, 20, 20, 21, 1, 29); // | (stored C << 29)   (N/V stay 0 -> SF=OF=0)
            e_str(20, 28, OFF_NZCV);         // spill x86 flag substrate for a later boundary
            emit32(0xD51B4200u | 20);        // msr nzcv, x20   (live flags for the following Jcc)
            e_movconst(18, 1);               // PF source byte = 1 (odd popcount) -> x86 PF = 0
            e_str(18, 28, OFF_PF);
            e_movconst(18, 0);
            e_str(18, 28, OFF_AF); // AF = 0
            return TX_NEXT;
        }
        case 0x00: { // PSHUFB d, s: d[i] = (s[i] & 0x80) ? 0 : d[s[i] & 0x0f]  (byte permute, hi-bit zeroes)
            if (!state->optimize) return TX_FALL;
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int s = crypto_rm_vec(I, next);
            // x86 uses low 4 index bits + bit7-zeroing; ARM TBL zeroes when index >= 16. Mask control to
            // 0x8f: bit7-clear -> 0..15 (valid lookup), bit7-set -> 128..143 (>=16 -> TBL yields 0). Exact.
            if (!state->mask_ready || !state->optimize)
                e_movi16b(27, 0x8f); // loop-invariant mask, hoisted across the run
            state->mask_ready = 1;
            hl_x86_emit_vector3(A_AND16, 16, s, 27); // v16 = control & 0x8f
            emit32(A_TBL | (16 << 16) | (crypto_reg(D) << 5) | crypto_reg(D));
            return TX_NEXT;
        }
        case 0x10:   // PBLENDVB d, s (mask = xmm0, per-byte top bit)
        case 0x14:   // BLENDVPS d, s (mask = xmm0, per-dword top bit)
        case 0x15: { // BLENDVPD d, s (mask = xmm0, per-qword top bit)  -- select s where mask bit set
            if (!state->optimize) return TX_FALL;
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int s = crypto_rm_vec(I, next);
            int esz = (op == 0x10) ? 8 : (op == 0x14) ? 32 : 64;
            hl_x86_emit_vector_shift_right(16, 0, esz, esz - 1,
                                           1);    // v16 = replicate top bit of each lane of xmm0 (v0) -> mask
            hl_x86_emit_vector3(A_BIT, D, s, 16); // D = (mask ? s : D)
            return TX_NEXT;
        }
        case 0x20:
        case 0x21:
        case 0x22:
        case 0x23:
        case 0x24:
        case 0x25: // PMOVSX (sign-extend)
        case 0x30:
        case 0x31:
        case 0x32:
        case 0x33:
        case 0x34:
        case 0x35: { // PMOVZX (zero-extend)
            if (!state->optimize) return TX_FALL;
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int sgn = op < 0x30;  // 0x2x = signed, 0x3x = unsigned
            int form = op & 0x0f; // 0=bw 1=bd 2=bq 3=wd 4=wq 5=dq
            int src_bytes = (form == 0 || form == 3 || form == 5) ? 8 : (form == 1 || form == 4) ? 4 : 2;
            int s;
            if (I->is_mem) { // narrow source: load exactly src_bytes to avoid a 16B over-read past the page
                emit_ea(I, next);
                emit_memory_guard(17, (uint64_t)src_bytes, next - (uint64_t)I->len, X86_SOFT_READ);
                s = 19;
                if (src_bytes == 8)
                    hl_x86_emit_vector_load64(19, 17);
                else if (src_bytes == 4)
                    hl_x86_emit_vector_load32(19, 17);
                else
                    e_ldr_h(19, 17);
            } else
                s = I->rm_reg;
            // widen the low half one size step at a time (UXTL/SXTL). Each step consumes the low 64 bits.
            if (form == 0)
                e_xtl(D, s, 0x080000u, sgn); // bw: 8B->8H
            else if (form == 1) {
                e_xtl(D, s, 0x080000u, sgn);
                e_xtl(D, D, 0x100000u, sgn);
            } // bd
            else if (form == 2) {
                e_xtl(D, s, 0x080000u, sgn);
                e_xtl(D, D, 0x100000u, sgn);
                e_xtl(D, D, 0x200000u, sgn);
            } // bq
            else if (form == 3)
                e_xtl(D, s, 0x100000u, sgn); // wd: 4H->4S
            else if (form == 4) {
                e_xtl(D, s, 0x100000u, sgn);
                e_xtl(D, D, 0x200000u, sgn);
            } // wq
            else
                e_xtl(D, s, 0x200000u, sgn); // dq: 2S->2D
            return TX_NEXT;
        }
        default: return TX_FALL;
        }
    }

    if (I->map3 == 3) { // 0F3A
        switch (op) {
        case 0xCC: { // SHA1RNDS4 d, s, imm2: 4 SHA-1 rounds; fn/K by imm2 (see header derivation)
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int s = crypto_rm_vec(I, next);
            int f = (int)I->imm & 3;
            static const uint32_t K1[4] = {0x5A827999u, 0x6ED9EBA1u, 0x8F1BBCDCu, 0xCA62C1D6u};
            static const uint32_t OPC[4] = {A_SHA1C, A_SHA1P, A_SHA1M, A_SHA1P}; // Ch/Parity/Maj/Parity
            e_rev4s(16, D);                                                      // v16 = X0 = (A,B,C,D)
            e_rev4s(17, s);                        // v17 = (W0,W1,W2,W3)  (W0 already has E folded by NEXTE)
            e_movz(16 /*x16*/, K1[f] & 0xffff, 0); // w16 = K (x16/x17 are engine GPR scratch in-block)
            hl_x86_emit_constant_part(16, K1[f] >> 16, 1);
            e_dup_w4s(18, 16);                            // v18.4s = K broadcast
            hl_x86_emit_vector3(A_ADD4S, 17, 17, 18);     // v17 = W + K (ARM adds Vm lane e per round, no internal K)
            e_movi16b(18, 0);                             // E0 = 0 (x86 RNDS4 defines E0=0; E arrives via W0)
            emit32(OPC[f] | (17 << 16) | (18 << 5) | 16); // v16 = X4 = (A4,B4,C4,D4)
            e_rev4s(D, 16);                               // d = (D4,C4,B4,A4)
            return TX_NEXT;
        }
        case 0x44: { // PCLMULQDQ d, s, imm8
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int imm = (int)I->imm;
            int selA = imm & 1;        // which 64-bit half of d
            int selB = (imm >> 4) & 1; // which 64-bit half of s
            int s = crypto_rm_vec(I, next);
            if (selA && selB) { // imm 0x11 (both high halves): single PMULL2 reads lane 1 of each source
                emit32(A_PMULL2 | (crypto_reg(s) << 16) | (crypto_reg(D) << 5) | crypto_reg(D));
                return TX_NEXT;
            }
            // PMULL reads lane 0 (low 64) of each source. If the low half is selected, feed the operand
            // register directly; only stage the HIGH half into lane 0 of a scratch when selected.
            int vn = D, vm = s;
            if (selA) {
                hl_x86_emit_vector_insert64(16, 0, D, 1);
                vn = 16;
            }
            if (selB) {
                hl_x86_emit_vector_insert64(17, 0, s, 1);
                vm = 17;
            }
            emit32(A_PMULL | (crypto_reg(vm) << 16) | (crypto_reg(vn) << 5) | crypto_reg(D));
            return TX_NEXT;
        }
        // ---- SSE4.1 ROUNDPS/PD/SS/SD (0F3A 08/09/0A/0B) d, s, imm8: round f32/f64 to integral. Each is a
        // single native NEON FRINT -- a per-instruction C-softmulator round-trip (R_SSE3B -> sse_round_*)
        // in vectorized float rounding/floor/ceil/trunc + libm, 20M+ fires. NON-destructive: d = round(s).
        // imm[1:0] mode -> FRINTN(nearest-even)/M(-inf)/P(+inf)/Z(trunc), resolved at translate time. imm[2]=1
        // -> use MXCSR.RC, which ldmxcsr mirrors into the live FPCR.RMode, so FRINTI (round per FPCR.RMode)
        // is exact -- identical to the C reference's __builtin_rint. imm[3] (suppress-precision) affects only
        // the inexact FLAG, not the result: explicit modes use the non-signaling FRINTN/M/P/Z (bit-for-bit
        // matches qemu's result), plus an FRINTX when the flag is wanted (see below). NaN/inf/negative-zero
        // pass through like x86 round. Gated by state->optimize.
        case 0x08:   // ROUNDPS d, s, imm8  (.4s)
        case 0x09:   // ROUNDPD d, s, imm8  (.2d)
        case 0x0A:   // ROUNDSS d, s, imm8  (scalar 32, preserve d[127:32])
        case 0x0B: { // ROUNDSD d, s, imm8  (scalar 64, preserve d[127:64])
            if (!state->optimize) return TX_FALL;
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int s = crypto_rm_vec(I, next);
            int imm = (int)I->imm;
            int mode = imm & 3;
            int is_pd = (op == 0x09 || op == 0x0B); // double precision
            int is_scalar = (op == 0x0A || op == 0x0B);
            // imm[3] clear means x86 REPORTS the inexactness: #P iff the rounding changed the value
            // (measured against native for every imm encoding). Round-to-integral changes x iff x is not
            // already integral, which does not depend on the mode -- so FRINTX, the one FRINT that reports
            // Inexact, is exactly that predicate whatever direction it rounds in. Under imm[2] it also
            // rounds the way FRINTI would, so it REPLACES it and costs nothing; otherwise it runs beside
            // the mode-specific FRINT for the flag alone, one instruction. imm[3] is set by
            // _MM_FROUND_NO_EXC, i.e. by every _mm_floor_/_mm_ceil_ the compiler emits, so the hot path
            // pays nothing either way. #I from an SNaN needs no help: every FRINT raises it.
            int px = !(imm & 8);               // report #P
            int frintx_only = px && (imm & 4); // FRINTX also IS the result
            uint32_t enc;
            if (is_scalar) {
                // scalar FRINT (single/double) writing into scratch v16, then merge lane 0 into d so d's
                // upper lanes are preserved per x86 scalar semantics. imm[2]=1 -> FRINTI (current FPCR mode).
                static const uint32_t frs[6] = {0x1E244000u /*N*/, 0x1E254000u /*M*/, 0x1E24C000u /*P*/,
                                                0x1E25C000u /*Z*/, 0x1E27C000u /*I*/, 0x1E274000u /*X*/};
                static const uint32_t frd[6] = {0x1E644000u /*N*/, 0x1E654000u /*M*/, 0x1E64C000u /*P*/,
                                                0x1E65C000u /*Z*/, 0x1E67C000u /*I*/, 0x1E674000u /*X*/};
                enc = (imm & 4) ? (is_pd ? frd[frintx_only ? 5 : 4] : frs[frintx_only ? 5 : 4])
                                : (is_pd ? frd[mode] : frs[mode]);
                if (px && !frintx_only) // flag-only FRINTX beside the mode-specific round
                    emit32((is_pd ? frd[5] : frs[5]) | (crypto_reg(s) << 5) | 17);
                emit32(enc | (crypto_reg(s) << 5) | 16); // v16 = round(s.low)
                if (is_pd)
                    hl_x86_emit_vector_insert64(D, 0, 16, 0); // d[63:0] = rounded, keep d[127:64]
                else
                    hl_x86_emit_vector_insert32(D, 0, 16, 0); // d[31:0] = rounded, keep d[127:32]
            } else {
                // packed FRINT over all lanes (.4s / .2d). imm[2]=1 -> FRINTI (current FPCR mode).
                static const uint32_t fr4s[6] = {0x4E218800u /*N*/, 0x4E219800u /*M*/, 0x4EA18800u /*P*/,
                                                 0x4EA19800u /*Z*/, 0x6EA19800u /*I*/, 0x6E219800u /*X*/};
                uint32_t base = (imm & 4) ? fr4s[frintx_only ? 5 : 4] : fr4s[mode];
                enc = is_pd ? (base | 0x400000u) : base; // .2d sets the sz bit (bit22)
                if (px && !frintx_only) emit32((is_pd ? (fr4s[5] | 0x400000u) : fr4s[5]) | (crypto_reg(s) << 5) | 17);
                emit32(enc | (crypto_reg(s) << 5) | crypto_reg(D));
            }
            return TX_NEXT;
        }
        case 0x0E: { // PBLENDW d, s, imm8: word i <- s if imm bit i set, else keep d  (8 x 16-bit)
            if (!state->optimize) return TX_FALL;
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int imm = (int)I->imm & 0xff;
            int s = crypto_rm_vec(I, next);
            for (int i = 0; i < 8; i++)
                if (imm & (1 << i)) e_ins_h(D, i, s, i); // copy selected 16-bit lanes from s into d
            return TX_NEXT;
        }
        case 0x0F: { // PALIGNR d, s, imm8: r = ((d:s) >> imm*8) bytes, with s in the low 16 bytes
            if (!state->optimize) return TX_FALL;
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int imm = (int)I->imm & 0xff;
            int s = crypto_rm_vec(I, next);
            if (imm >= 32) {
                hl_x86_emit_vector3(A_EOR16, D, D, D); // fully shifted out -> 0
            } else if (imm >= 16) {
                // bytes come only from d, zero-filled: EXT (d : zero) starting at imm-16
                hl_x86_emit_vector3(A_EOR16, 16, 16, 16);
                hl_x86_emit_vector_extract(D, D, 16, imm - 16);
            } else {
                // EXT concatenates (Vn low : Vm high); x86 puts s low, d high -> Vn=s, Vm=d, idx=imm
                hl_x86_emit_vector_extract(D, s, D, imm);
            }
            return TX_NEXT;
        }
        default: return TX_FALL;
        }
    }
    return TX_FALL;
}
