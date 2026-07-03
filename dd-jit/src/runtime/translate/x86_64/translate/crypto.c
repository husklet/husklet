// dd/runtime/frontend/x86_64 -- crypto instruction class (task #342): map the x86 AES-NI / PCLMULQDQ /
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
#define A_AESE 0x4E284800u  // AESE  Vd.16B, Vn.16B  = SubBytes(ShiftRows(Vd ^ Vn))
#define A_AESD 0x4E285800u  // AESD  Vd.16B, Vn.16B  = InvSubBytes(InvShiftRows(Vd ^ Vn))
#define A_AESMC 0x4E286800u // AESMC Vd.16B, Vn.16B  = MixColumns(Vn)
#define A_AESIMC 0x4E287800u // AESIMC Vd.16B, Vn.16B = InvMixColumns(Vn)
#define A_EOR16 0x6E201C00u  // EOR   Vd.16B, Vn.16B, Vm.16B  (via e_v3)
#define A_PMULL 0x0EE0E000u  // PMULL Vd.1Q, Vn.1D, Vm.1D     (poly multiply, low lanes)

// r/m operand -> vector register number (v19 when memory-backed; EA computed into x17 by emit_ea).
static int crypto_rm_vec(struct insn *I, uint64_t next) {
    if (I->is_mem) {
        emit_ea(I, next);
        e_ldr_q(19, 17, 0);
        return 19;
    }
    return I->rm_reg;
}

// ---- SHA-256 (SHA-NI) inline NEON. x86 SHA-NI packs the round differently from ARM's SHA256H (2 vs 4
// rounds, {A,B,E,F}|{C,D,G,H} vs {A,B,C,D}|{E,F,G,H}), so a 1:1 ARM-crypto map is impossible; we lower
// the exact round math (matching do_sse3b bit-for-bit, verified vs the oracle) to NEON. Each working var
// lives broadcast across a .4s scratch reg (v20..v31); only lane 0 is load-bearing. ----
#define A_AND16 0x4E201C00u
#define A_ORR16 0x4EA01C00u
#define A_BIC16 0x4E601C00u // BIC Vd,Vn,Vm = Vn & ~Vm
#define A_ADD4S 0x4EA08400u
// DUP Vd.4S, Vn.S[i]  (broadcast a 32-bit lane)
static void e_dup_s(int vd, int vn, int i) { emit32(0x4E000400u | ((unsigned)(((i << 3) | 4)) << 16) | (vn << 5) | vd); }
// ROR32 across .4s: vd = (vn >>> n).  uses scratch v16/v17.
static void e_ror32(int vd, int vn, int n) {
    e_vshr_imm(16, vn, 32, n, 0);      // v16 = vn >> n   (logical)
    e_vshl_imm(17, vn, 32, 32 - n);    // v17 = vn << (32-n)
    e_v3(A_ORR16, vd, 16, 17);
}
// vd = ror(vn,a) ^ ror(vn,b) ^ ror(vn,c)  (uper/lower sigma pattern). uses v16/v17/v18/v19.
static void e_sha_sig3(int vd, int vn, int a, int b, int c) {
    e_ror32(18, vn, a);
    e_ror32(19, vn, b);
    e_v3(0x6E201C00u, 18, 18, 19); // eor
    e_ror32(19, vn, c);
    e_v3(0x6E201C00u, vd, 18, 19);
}
// vd = ror(vn,a) ^ ror(vn,b) ^ (vn >> shr)  (message sigma). uses v16/v17/v18/v19.
static void e_sha_msgsig(int vd, int vn, int a, int b, int shr) {
    e_ror32(18, vn, a);
    e_ror32(19, vn, b);
    e_v3(0x6E201C00u, 18, 18, 19);
    e_vshr_imm(19, vn, 32, shr, 0);
    e_v3(0x6E201C00u, vd, 18, 19);
}

// Try to lower a legacy 0F38/0F3A crypto opcode to inline ARM crypto. Returns TX_NEXT if emitted,
// TX_FALL to defer to the C softmulator (do_sse3b) for the ops we don't map (SHA-1, aeskeygenassist, ...).
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
            e_v3(A_EOR16, 16, 16, 16); // v16 = 0  (the "zero round key" for AESE/AESD)
            e_vmov(17, D);             // v17 = state
            emit32((enc ? A_AESE : A_AESD) | (16 << 5) | 17); // v17 = Sub(Shift(state ^ 0))
            if (!last) emit32((enc ? A_AESMC : A_AESIMC) | (17 << 5) | 17); // v17 = MixColumns(v17)
            e_v3(A_EOR16, D, 17, key); // d = v17 ^ round key
            return TX_NEXT;
        }
        case 0xDB: { // AESIMC: d = InvMixColumns(s)  (non-destructive)
            if (g_fp_known) fp_drop();
            int s = crypto_rm_vec(I, next);
            emit32(A_AESIMC | (s << 5) | D);
            return TX_NEXT;
        }
        case 0xCB: { // SHA256RNDS2 d, s, <xmm0>: 2 SHA-256 rounds. d={C,D,G,H}, s={A,B,E,F}, xmm0={WK}.
            if (g_fp_known) fp_drop();
            int s = crypto_rm_vec(I, next);
            enum { A = 20, B = 21, C = 22, Dd = 23, E = 24, F = 25, G = 26, H = 27 };
            e_dup_s(A, s, 3); e_dup_s(B, s, 2); e_dup_s(E, s, 1); e_dup_s(F, s, 0);
            e_dup_s(C, D, 3); e_dup_s(Dd, D, 2); e_dup_s(G, D, 1); e_dup_s(H, D, 0);
            for (int i = 0; i < 2; i++) {
                e_sha_sig3(28, E, 6, 11, 25); // v28 = S1(E)      [clobbers v16..v19]
                e_sha_sig3(29, A, 2, 13, 22); // v29 = S0(A)      [clobbers v16..v19]
                e_v3(A_AND16, 16, E, F);      // ch = (E&F) ^ (G&~E)
                e_v3(A_BIC16, 17, G, E);
                e_v3(0x6E201C00u, 30, 16, 17);          // v30 = ch
                e_v3(A_AND16, 16, A, B);                 // maj = (A&B)^(A&C)^(B&C)
                e_v3(A_AND16, 17, A, C);
                e_v3(0x6E201C00u, 16, 16, 17);
                e_v3(A_AND16, 17, B, C);
                e_v3(0x6E201C00u, 31, 16, 17);          // v31 = maj
                e_dup_s(16, 0, i);                       // v16 = WK_i  (implicit xmm0 == v0)
                e_v3(A_ADD4S, 17, H, 28);                // t1 = H + S1 + ch + WK
                e_v3(A_ADD4S, 17, 17, 30);
                e_v3(A_ADD4S, 17, 17, 16);               // v17 = t1
                e_v3(A_ADD4S, 18, 17, 29);               // An = t1 + S0 + maj
                e_v3(A_ADD4S, 18, 18, 31);               // v18 = An
                e_v3(A_ADD4S, 19, 17, Dd);               // v19 = En = t1 + D
                // rotate: H=G,G=F,F=E,E=En ; D=C,C=B,B=A,A=An
                e_vmov(H, G); e_vmov(G, F); e_vmov(F, E); e_vmov(E, 19);
                e_vmov(Dd, C); e_vmov(C, B); e_vmov(B, A); e_vmov(A, 18);
            }
            // dst = {F2, E2, B2, A2} in lanes 0..3
            e_ins_s(D, 0, F, 0); e_ins_s(D, 1, E, 0); e_ins_s(D, 2, B, 0); e_ins_s(D, 3, A, 0);
            return TX_NEXT;
        }
        case 0xCC: { // SHA256MSG1 d, s: d[i] = W[i] + sigma0(W[i+1]); W0..3=d, W4=s[31:0]
            if (g_fp_known) fp_drop();
            int s = crypto_rm_vec(I, next);
            e_ext(28, D, s, 4);            // v28 = {W1,W2,W3,W4}
            e_sha_msgsig(29, 28, 7, 18, 3); // v29 = sigma0(shifted)
            e_v3(A_ADD4S, D, D, 29);        // d = W + sigma0
            return TX_NEXT;
        }
        case 0xCD: { // SHA256MSG2 d, s: W16..19 with the W16->W18, W17->W19 dependency chain
            if (g_fp_known) fp_drop();
            int s = crypto_rm_vec(I, next);
            e_ext(28, s, s, 8);             // {sw2,sw3,sw0,sw1}: lanes0,1 = sw2,sw3
            e_sha_msgsig(29, 28, 17, 19, 10); // lanes0,1 = sig1(sw2),sig1(sw3)
            e_v3(A_ADD4S, 28, D, 29);       // v28 = R1: lane0=W16, lane1=W17
            e_sha_msgsig(29, 28, 17, 19, 10); // lanes0,1 = sig1(W16),sig1(W17)
            e_v3(0x6E201C00u, 31, 31, 31);  // v31 = 0
            e_ext(30, 31, 29, 8);           // v30 = {0,0,sig1(W16),sig1(W17)}
            e_v3(A_ADD4S, 29, D, 30);       // v29 = R2: lane2=W18, lane3=W19
            e_vmov(D, 28);                  // lanes0,1 = W16,W17
            e_ins_d(D, 1, 29, 1);           // lanes2,3 = W18,W19
            return TX_NEXT;
        }
        default: return TX_FALL;
        }
    }

    if (I->map3 == 3) { // 0F3A
        switch (op) {
        case 0x44: { // PCLMULQDQ d, s, imm8
            if (g_fp_known) fp_drop();
            int imm = (int)I->imm;
            int selA = imm & 1;         // which 64-bit half of d
            int selB = (imm >> 4) & 1;  // which 64-bit half of s
            int s = crypto_rm_vec(I, next);
            // stage chosen halves into lane 0 of v16 (from d) and v17 (from s)
            if (selA)
                e_ins_d(16, 0, D, 1);
            else
                e_vmov(16, D);
            if (selB)
                e_ins_d(17, 0, s, 1);
            else
                e_vmov(17, s);
            emit32(A_PMULL | (17 << 16) | (16 << 5) | D); // d.1q = clmul(v16.1d, v17.1d)
            return TX_NEXT;
        }
        default: return TX_FALL;
        }
    }
    return TX_FALL;
}
