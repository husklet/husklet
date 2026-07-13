// dd/runtime/frontend/x86_64 -- crypto instruction class: map the x86 AES-NI / PCLMULQDQ /
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

// r/m operand -> vector register number (v19 when memory-backed; EA computed into x17 by emit_ea).
static int crypto_rm_vec(struct insn *I, uint64_t next) {
    if (I->is_mem) {
        emit_ea(I, next);
        e_ldr_q(19, 17, 0);
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
    emit32(0x4F00E400u | ((imm8 & 0xe0u) << 11) | ((imm8 & 0x1fu) << 5) | vd);
}

// INS Vd.H[di], Vn.H[si]  (copy one 16-bit lane)
static void e_ins_h(int vd, int di, int vn, int si) {
    emit32(0x6E000400u | ((unsigned)(((di << 2) | 2)) << 16) | ((unsigned)(si << 1) << 11) | (vn << 5) | vd);
}

// sized FP/SIMD loads from [x17] (byte / halfword) -- for pmov narrow memory operands (avoid 16B over-read)
static void e_ldr_h(int t, int rn) {
    emit32(0x7D400000u | (rn << 5) | t);
} // ldr h<t>,[xn]

// SXTL/UXTL Vd.<2*e> , Vn.<e>  (widen the low half one size step; immh selects source element size).
//   immh_w: 0x080000=8B->8H, 0x100000=4H->4S, 0x200000=2S->2D.  sgn: 1=SXTL (sign), 0=UXTL (zero).
static void e_xtl(int vd, int vn, unsigned immh_w, int sgn) {
    emit32((sgn ? 0x0F00A400u : 0x2F00A400u) | immh_w | (vn << 5) | vd);
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

// DUP Vd.4S, Vn.S[i]  (broadcast a 32-bit lane)
static void e_dup_s(int vd, int vn, int i) {
    emit32(0x4E000400u | ((unsigned)(((i << 3) | 4)) << 16) | (vn << 5) | vd);
}

// DUP Vd.4S, Wn  (broadcast a GPR)
static void e_dup_w4s(int vd, int rn) {
    emit32(0x4E040C00u | (rn << 5) | vd);
}

static void e_rev64_4s(int vd, int vn) {
    emit32(A_REV64_4S | (vn << 5) | vd);
}

// full .4s lane reversal (a,b,c,d) -> (d,c,b,a): rev64 swaps within halves, ext #8 swaps the halves.
static void e_rev4s(int vd, int vn) {
    e_rev64_4s(vd, vn);
    e_ext(vd, vd, vd, 8);
}

// AES-endgame perf: straight-line hoisting of the two loop-invariant scratch constants the crypto glue
// re-materializes per instruction: the ZERO round key the AESENC/AESDEC mapping feeds AESE/AESD, and the
// 0x8f control mask PSHUFB ANDs into its TBL index. openssl's hot loops run 60+ back-to-back aesenc and
// 4 pshufb per ghash iteration -- re-emitting the constant each time wastes an insn AND a rename
// dependency per op. The constants live in v26 (zero) / v27 (0x8f): v20..v31 are used by NOTHING in the
// x86 translator except the SHA-NI cases below (verified by sweep), so the claims survive the ordinary
// legacy-SSE/GPR lowerings interleaved in real loops (all scratch there is x16..x25 / v16..v19).
//   g_v26z / g_v27m == 1  =>  the emitted code at this point provably left v26 == 0 / v27 == 0x8f.
// Cleared at: translate_block entry, the SHA-NI cases (clobber v20+), and the rep-movs/stos string
// idiom (its ERMS funnel `blr`s a host helper, which may clobber all of v16..v31). Stitched superblock
// constituents are never entered externally (no map entry is registered for them) and every C-emulation
// transition ends the emitted block, so the straight-line assumption holds everywhere else.
// NOSSEOPT=1 disables the hoist (constants re-emitted every op -- the pre-hoist codegen) for A/B.
static int g_v26z, g_v27m;

// Try to lower a legacy 0F38/0F3A crypto opcode to inline ARM crypto. Returns TX_NEXT if emitted,
// TX_FALL to defer to the C softmulator (do_sse3b) for the rare ops we don't map (pcmpistri, ...).
static int translate_crypto(struct insn *I, uint64_t next) {
    uint8_t op = I->op;
    int D = I->reg; // dst xmm == src1 (destructive) for AES/PCLMUL

    if (I->map3 == 2) { // 0F38
        switch (op) {
        case 0xDC:   // AESENC
        case 0xDD:   // AESENCLAST
        case 0xDE:   // AESDEC
        case 0xDF: { // AESDECLAST
            if (g_fp_known) fp_drop();
            int key = crypto_rm_vec(I, next);
            int enc = (op == 0xDC || op == 0xDD);
            int last = (op == 0xDD || op == 0xDF);
            uint32_t aes = enc ? A_AESE : A_AESD, mc = enc ? A_AESMC : A_AESIMC;
            if (!g_v26z || nosseopt()) e_v3(A_EOR16, 26, 26, 26); // v26 = 0 ("zero round key"); hoisted across runs
            g_v26z = 1; // v26 stays zero through this case (AESE/AESD only READ it)
            if (key == D) {
                // AESENC xmm,xmm aliases state==key: the ^key reads the ORIGINAL state, so keep D intact
                // and compute through scratch v17 (5 insns).
                e_vmov(17, D);
                emit32(aes | (26 << 5) | 17);
                if (!last) emit32(mc | (17 << 5) | 17);
                e_v3(A_EOR16, D, 17, key);
            } else {
                // hot path (key != state): transform D in place, XOR the round key after (3 insns).
                emit32(aes | (26 << 5) | D); // D = Sub(Shift(D ^ 0))
                if (!last) emit32(mc | (D << 5) | D);
                e_v3(A_EOR16, D, D, key); // d = MixColumns(...) ^ round key
            }
            return TX_NEXT;
        }
        case 0xDB: { // AESIMC: d = InvMixColumns(s)  (non-destructive)
            if (g_fp_known) fp_drop();
            int s = crypto_rm_vec(I, next);
            emit32(A_AESIMC | (s << 5) | D);
            return TX_NEXT;
        }
        case 0xC8: { // SHA1NEXTE d, s: d = s + (0,0,0, rol30(d.lane3))   [E folded into the next W0]
            if (g_fp_known) fp_drop();
            g_v26z = g_v27m = 0; // the SHA round lowering clobbers v20..v31
            int s = crypto_rm_vec(I, next);
            e_dup_s(16, D, 3);                // v16.s[0] = d.lane3 (the A from 4 rounds ago)
            emit32(A_SHA1H | (16 << 5) | 16); // s16 = rol30(d3)
            e_movi16b(17, 0);
            e_ins_s(17, 3, 16, 0);   // v17 = (0,0,0,rol30(d3))
            e_v3(A_ADD4S, D, s, 17); // lanes 0..2 = s passthrough, lane 3 = s3 + rol30(d3)
            return TX_NEXT;
        }
        case 0xC9: { // SHA1MSG1 d, s: d ^= (s2,s3,d0,d1)  [W xor of the schedule, no rotate]
            if (g_fp_known) fp_drop();
            int s = crypto_rm_vec(I, next);
            e_ext(16, s, D, 8);          // v16 = (s2,s3,d0,d1)
            e_v3(0x6E201C00u, D, D, 16); // eor
            return TX_NEXT;
        }
        case 0xCA: { // SHA1MSG2 d, s: rol1 schedule finish with the lane-3 -> lane-0 chain (see header)
            if (g_fp_known) fp_drop();
            int s = crypto_rm_vec(I, next);
            e_rev4s(16, D);                     // v16 = (d3,d2,d1,d0)
            e_rev4s(17, s);                     // v17 = (s3,s2,s1,s0)
            emit32(A_SHA1SU1 | (17 << 5) | 16); // v16 = (r3,r2,r1,r0): T=(d3^s2,d2^s1,d1^s0,d0), chain in lane 3
            e_rev4s(D, 16);                     // d = (r0,r1,r2,r3) back in x86 lane order
            return TX_NEXT;
        }
        case 0xCB: { // SHA256RNDS2 d, s, <xmm0>: 2 rounds via ARM SHA256H/H2 (see header derivation).
            if (g_fp_known) fp_drop();
            int s = crypto_rm_vec(I, next);
            e_rev64_4s(16, s);                               // v16 = (E,F,A,B)
            e_rev64_4s(17, D);                               // v17 = (G,H,C,D)
            e_v3(A_ZIP2_2D, 20, 16, 17);                     // v20 = X0 = (A,B,C,D)
            e_v3(A_ZIP1_2D, 21, 16, 17);                     // v21 = Y0 = (E,F,G,H)
            e_vmov(16, 20);                                  // save X0 for SHA256H2
            emit32(A_SHA256H | (0 << 16) | (21 << 5) | 20);  // v20 = X4 = (A4,A3,A2,A1); WK = xmm0(v0),
            emit32(A_SHA256H2 | (0 << 16) | (16 << 5) | 21); // v21 = Y4 = (E4,E3,E2,E1); lanes2,3 garbage-OK
            e_rev64_4s(16, 21);                              // v16 = (E3,E4,E1,E2)
            e_rev64_4s(17, 20);                              // v17 = (A3,A4,A1,A2)
            e_v3(A_ZIP2_2D, D, 16, 17);                      // d = (E1,E2,A1,A2) = (F2,E2,B2,A2)
            return TX_NEXT;
        }
        case 0xCC: { // SHA256MSG1 d, s: exactly ARM SHA256SU0 (see header)
            if (g_fp_known) fp_drop();
            int s = crypto_rm_vec(I, next);
            emit32(A_SHA256SU0 | (s << 5) | D);
            return TX_NEXT;
        }
        case 0xCD: { // SHA256MSG2 d, s: ARM SHA256SU1 with Vn=0 + lane-3 pre-cancel (see header)
            if (g_fp_known) fp_drop();
            int s = crypto_rm_vec(I, next);
            if (s == D) { // aliased d==s: SU1 must read the ORIGINAL src after we adjust d
                e_vmov(20, s);
                s = 20;
            }
            e_movi16b(16, 0); // v16 = 0 (the SU1 Vn operand)
            e_movi16b(17, 0);
            e_ins_s(17, 3, s, 0);    // v17 = (0,0,0,s0)
            e_v3(A_SUB4S, D, D, 17); // cancel SU1's T0 lane-3 (+s0) contribution
            emit32(A_SHA256SU1 | (s << 16) | (16 << 5) | D);
            return TX_NEXT;
        }
        case 0x00: { // PSHUFB d, s: d[i] = (s[i] & 0x80) ? 0 : d[s[i] & 0x0f]  (byte permute, hi-bit zeroes)
            if (nosseopt()) return TX_FALL;
            if (g_fp_known) fp_drop();
            int s = crypto_rm_vec(I, next);
            // x86 uses low 4 index bits + bit7-zeroing; ARM TBL zeroes when index >= 16. Mask control to
            // 0x8f: bit7-clear -> 0..15 (valid lookup), bit7-set -> 128..143 (>=16 -> TBL yields 0). Exact.
            if (!g_v27m || nosseopt()) e_movi16b(27, 0x8f); // loop-invariant mask, hoisted across the run
            g_v27m = 1;
            e_v3(A_AND16, 16, s, 27);                  // v16 = control & 0x8f
            emit32(A_TBL | (16 << 16) | (D << 5) | D); // D = table[D] indexed by v16 (out-of-range -> 0)
            return TX_NEXT;
        }
        case 0x10:   // PBLENDVB d, s (mask = xmm0, per-byte top bit)
        case 0x14:   // BLENDVPS d, s (mask = xmm0, per-dword top bit)
        case 0x15: { // BLENDVPD d, s (mask = xmm0, per-qword top bit)  -- select s where mask bit set
            if (nosseopt()) return TX_FALL;
            if (g_fp_known) fp_drop();
            int s = crypto_rm_vec(I, next);
            int esz = (op == 0x10) ? 8 : (op == 0x14) ? 32 : 64;
            e_vshr_imm(16, 0, esz, esz - 1, 1); // v16 = replicate top bit of each lane of xmm0 (v0) -> mask
            e_v3(A_BIT, D, s, 16);              // D = (mask ? s : D)
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
            if (nosseopt()) return TX_FALL;
            if (g_fp_known) fp_drop();
            int sgn = op < 0x30;  // 0x2x = signed, 0x3x = unsigned
            int form = op & 0x0f; // 0=bw 1=bd 2=bq 3=wd 4=wq 5=dq
            int src_bytes = (form == 0 || form == 3 || form == 5) ? 8 : (form == 1 || form == 4) ? 4 : 2;
            int s;
            if (I->is_mem) { // narrow source: load exactly src_bytes to avoid a 16B over-read past the page
                emit_ea(I, next);
                s = 19;
                if (src_bytes == 8)
                    e_ldr_d(19, 17);
                else if (src_bytes == 4)
                    e_ldr_s(19, 17);
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
            if (g_fp_known) fp_drop();
            int s = crypto_rm_vec(I, next);
            int f = (int)I->imm & 3;
            static const uint32_t K1[4] = {0x5A827999u, 0x6ED9EBA1u, 0x8F1BBCDCu, 0xCA62C1D6u};
            static const uint32_t OPC[4] = {A_SHA1C, A_SHA1P, A_SHA1M, A_SHA1P}; // Ch/Parity/Maj/Parity
            e_rev4s(16, D);                                                      // v16 = X0 = (A,B,C,D)
            e_rev4s(17, s);                        // v17 = (W0,W1,W2,W3)  (W0 already has E folded by NEXTE)
            e_movz(16 /*x16*/, K1[f] & 0xffff, 0); // w16 = K (x16/x17 are engine GPR scratch in-block)
            e_movk(16, K1[f] >> 16, 1);
            e_dup_w4s(18, 16);                            // v18.4s = K broadcast
            e_v3(A_ADD4S, 17, 17, 18);                    // v17 = W + K (ARM adds Vm lane e per round, no internal K)
            e_movi16b(18, 0);                             // E0 = 0 (x86 RNDS4 defines E0=0; E arrives via W0)
            emit32(OPC[f] | (17 << 16) | (18 << 5) | 16); // v16 = X4 = (A4,B4,C4,D4)
            e_rev4s(D, 16);                               // d = (D4,C4,B4,A4)
            return TX_NEXT;
        }
        case 0x44: { // PCLMULQDQ d, s, imm8
            if (g_fp_known) fp_drop();
            int imm = (int)I->imm;
            int selA = imm & 1;        // which 64-bit half of d
            int selB = (imm >> 4) & 1; // which 64-bit half of s
            int s = crypto_rm_vec(I, next);
            if (selA && selB) { // imm 0x11 (both high halves): single PMULL2 reads lane 1 of each source
                emit32(A_PMULL2 | (s << 16) | (D << 5) | D);
                return TX_NEXT;
            }
            // PMULL reads lane 0 (low 64) of each source. If the low half is selected, feed the operand
            // register directly; only stage the HIGH half into lane 0 of a scratch when selected.
            int vn = D, vm = s;
            if (selA) {
                e_ins_d(16, 0, D, 1);
                vn = 16;
            }
            if (selB) {
                e_ins_d(17, 0, s, 1);
                vm = 17;
            }
            emit32(A_PMULL | (vm << 16) | (vn << 5) | D); // d.1q = clmul(halfd, halfs)
            return TX_NEXT;
        }
        case 0x0E: { // PBLENDW d, s, imm8: word i <- s if imm bit i set, else keep d  (8 x 16-bit)
            if (nosseopt()) return TX_FALL;
            if (g_fp_known) fp_drop();
            int imm = (int)I->imm & 0xff;
            int s = crypto_rm_vec(I, next);
            for (int i = 0; i < 8; i++)
                if (imm & (1 << i)) e_ins_h(D, i, s, i); // copy selected 16-bit lanes from s into d
            return TX_NEXT;
        }
        case 0x0F: { // PALIGNR d, s, imm8: r = ((d:s) >> imm*8) bytes, with s in the low 16 bytes
            if (nosseopt()) return TX_FALL;
            if (g_fp_known) fp_drop();
            int imm = (int)I->imm & 0xff;
            int s = crypto_rm_vec(I, next);
            if (imm >= 32) {
                e_v3(A_EOR16, D, D, D); // fully shifted out -> 0
            } else if (imm >= 16) {
                // bytes come only from d, zero-filled: EXT (d : zero) starting at imm-16
                e_v3(A_EOR16, 16, 16, 16);
                e_ext(D, D, 16, imm - 16);
            } else {
                // EXT concatenates (Vn low : Vm high); x86 puts s low, d high -> Vn=s, Vm=d, idx=imm
                e_ext(D, s, D, imm);
            }
            return TX_NEXT;
        }
        default: return TX_FALL;
        }
    }
    return TX_FALL;
}
