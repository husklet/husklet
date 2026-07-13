// dd/runtime/frontend/x86_64 -- 0F38/0F3A GPR + lane "glue" lowering (AES-GCM endgame, perf wave 2).
//
// WHY: the shipped crypto/shuffle glue (translate/crypto.c) lowered AES-NI/PCLMUL/SHA-NI + the SSSE3/SSE4
// vector shuffles inline, but real openssl's stitched aesni_ctr32_encrypt_blocks loop STILL exited to the
// C softmulator (do_sse3b) once per instruction for the GPR-side glue. Measured on the real
// `openssl speed -evp aes-128-gcm` hot loop (EXITSTAT histogram, postgres:15 libcrypto 3.5.6):
//     movbe (0F38 F1, CTR counter byte-swap)    23.6M exits/s   -- 6 sites in the 6-block CTR loop
//     pinsrd/q (0F3A 22, counter injection)     416K exits/s
//     aeskeygenassist (0F3A DF, key schedule)   232K exits/s
// Each exit is a full block shatter (spill 16 GPR + 16 vec + nzcv, C call, re-decode, scalar emulate,
// reload) -- movbe alone accounted for the 6x microbench-vs-real gap. This file lowers the WHOLE
// remaining 0F38/0F3A GPR/lane families inline (COMPLETENESS):
//     0F38 F0/F1    MOVBE r,m / MOVBE m,r (16/32/64)  and  CRC32 r,r/m8|16|32|64 (the F2 form)
//     0F3A 14..17   PEXTRB / PEXTRW / PEXTRD/Q / EXTRACTPS   (reg or mem destination)
//     0F3A 20..22   PINSRB / INSERTPS / PINSRD/Q             (reg or mem source)
//     0F3A DF       AESKEYGENASSIST (one AESE + one TBL -- ShiftRows folded into the index vector)
//
// Same quality bar as translate/crypto.c: byte-exact vs the qemu oracle (dd-tests completeness guest),
// no EFLAGS interaction (none of these ops read or write flags), scratch = x16 (GPR), v16..v19 (vector).
// A/B kill-switch: NOSSEOPT=1 falls back to the do_sse3b block-exit path (same gate as the shuffle glue).

// ---- tiny GPR encoders local to this class ----
static void e4_rev(int rd, int rn, int nb) { // byte-reverse: REV16 (nb=2) / REV Wd (4) / REV Xd (8)
    emit32(nb == 2   ? (0x5AC00400u | (rn << 5) | rd)
           : nb == 4 ? (0x5AC00800u | (rn << 5) | rd)
                     : (0xDAC00C00u | (rn << 5) | rd));
}

// INS Vd.<T>[i], Rn (general): imm5 selects lane size+index. nb = element bytes (1/2/4/8).
static void e4_ins_g(int vd, int i, int rn, int nb) {
    unsigned imm5 = (nb == 1)   ? ((i << 1) | 1)
                    : (nb == 2) ? ((i << 2) | 2)
                    : (nb == 4) ? ((i << 3) | 4)
                                : ((i << 4) | 8);
    emit32(0x4E001C00u | (imm5 << 16) | (rn << 5) | vd);
}

// UMOV Rd, Vn.<T>[i]: zero-extending lane read into a GPR (W for nb<=4, X for nb=8).
static void e4_umov(int rd, int vn, int i, int nb) {
    unsigned imm5 = (nb == 1)   ? ((i << 1) | 1)
                    : (nb == 2) ? ((i << 2) | 2)
                    : (nb == 4) ? ((i << 3) | 4)
                                : ((i << 4) | 8);
    emit32(((nb == 8) ? 0x4E003C00u : 0x0E003C00u) | (imm5 << 16) | (vn << 5) | rd);
}

// materialize an arbitrary 16-byte constant into v<vd> (via x16: movconst + fmov d / ins d[1])
static void e4_vconst16(int vd, uint64_t lo, uint64_t hi) {
    e_movconst(16, lo);
    e_fmov_to_d(vd, 16); // vd.d[0] = lo, upper 64 zeroed
    e_movconst(16, hi);
    e4_ins_g(vd, 1, 16, 8); // vd.d[1] = hi
}

// Lower a legacy 0F38/0F3A GPR/lane-glue opcode inline. Returns TX_NEXT if emitted, TX_FALL to defer
// to do_sse3b. Called AFTER translate_crypto (crypto.c) declined the opcode.
static int translate_sse4x(struct insn *I, uint64_t next) {
    uint8_t op = I->op;
    int D = I->reg; // all scratch below is x16/x17 + v16..v19: the v26/v27 hoist claims are unaffected

    if (I->map3 == 2 && (op == 0xF0 || op == 0xF1)) {
        if (nosseopt()) return TX_FALL;
        if (I->repne) { // ---- CRC32 r32/64, r/m: x86 CRC32 is the Castagnoli poly == ARM CRC32C* ----
            int nb = (op == 0xF0) ? 1 : I->opsize;
            int src;
            if (I->is_mem) {
                emit_ea(I, next);
                e_load(nb, 16, 17);
                src = 16;
            } else {
                src = (nb == 1) ? byte_val(I, I->rm_reg, 16) : I->rm_reg; // ah/ch/dh/bh via shift
            }
            static const uint32_t enc[4] = {0x1AC05000u, 0x1AC05400u, 0x1AC05800u, 0x9AC05C00u}; // cb/ch/cw/cx
            // acc = low 32 of the dest GPR; Wd write zero-extends (matches x86: r32, and r64 hi-32 zeroed)
            emit32(enc[nb == 1 ? 0 : nb == 2 ? 1 : nb == 4 ? 2 : 3] | (src << 16) | (D << 5) | D);
            return TX_NEXT;
        }
        if (I->rep || !I->is_mem) return TX_FALL; // F3 form / reg-reg MOVBE are #UD -> softmulator path
        // ---- MOVBE: byte-swapping load (F0: r <- bswap(m)) / store (F1: m <- bswap(r)) ----
        int nb = I->opsize;
        emit_ea(I, next);
        if (op == 0xF0) {
            e_load(nb, 16, 17); // zero-extending load
            if (nb == 2) {      // 16-bit dest merges into the low 16 bits of the guest reg
                e4_rev(16, 16, 2);
                e_bfi(D, 16, 0, 16, 1);
            } else {
                e4_rev(D, 16, nb); // W write zero-extends (32-bit); X full (64-bit)
            }
        } else {
            e4_rev(16, D, nb); // rev16 leaves only the low halfword meaningful; the 2-byte store reads just that
            e_store(nb, 16, 17);
        }
        return TX_NEXT;
    }

    if (I->map3 == 3) {
        int imm = (int)I->imm & 0xff;
        switch (op) {
        case 0x14:   // PEXTRB r/m8,  xmm, imm  (reg dest: zero-extended into r32/64)
        case 0x15:   // PEXTRW r/m16, xmm, imm
        case 0x16:   // PEXTRD/Q r/m, xmm, imm  (REX.W selects Q)
        case 0x17: { // EXTRACTPS r/m32, xmm, imm  (== PEXTRD of lane imm&3)
            if (nosseopt()) return TX_FALL;
            if (g_fp_known) fp_drop();
            int nb = (op == 0x14) ? 1 : (op == 0x15) ? 2 : (op == 0x17) ? 4 : (I->rexW ? 8 : 4);
            int lane = imm & (16 / nb - 1);
            if (I->is_mem) {
                emit_ea(I, next);
                e4_umov(16, D, lane, nb);
                e_store(nb, 16, 17);
            } else {
                e4_umov(I->rm_reg, D, lane, nb); // zero-extends into the full GPR (x86 r32 semantics)
            }
            return TX_NEXT;
        }
        case 0x20:   // PINSRB xmm, r32/m8, imm
        case 0x22: { // PINSRD/Q xmm, r/m32|64, imm  (REX.W selects Q)
            if (nosseopt()) return TX_FALL;
            if (g_fp_known) fp_drop();
            int nb = (op == 0x20) ? 1 : (I->rexW ? 8 : 4);
            int lane = imm & (16 / nb - 1);
            int src;
            if (I->is_mem) {
                emit_ea(I, next);
                e_load(nb, 16, 17);
                src = 16;
            } else {
                src = I->rm_reg; // PINSRB reg form reads the LOW byte of a r32 (never ah/…): reg direct
            }
            e4_ins_g(D, lane, src, nb);
            return TX_NEXT;
        }
        case 0x21: { // INSERTPS xmm, xmm/m32, imm: insert one dword lane, then zero lanes per imm[3:0]
            if (nosseopt()) return TX_FALL;
            if (g_fp_known) fp_drop();
            int countD = (imm >> 4) & 3, zmask = imm & 0xf;
            if (I->is_mem) {
                emit_ea(I, next);
                e_ldr_s(19, 17); // m32 -> lane 0 of v19 (rest zeroed)
                e_ins_s(D, countD, 19, 0);
            } else {
                e_ins_s(D, countD, I->rm_reg, (imm >> 6) & 3);
            }
            if (zmask) {
                e_v3(A_EOR16, 16, 16, 16);
                for (int j = 0; j < 4; j++)
                    if (zmask & (1 << j)) e_ins_s(D, j, 16, 0);
            }
            return TX_NEXT;
        }
        case 0xDF: { // AESKEYGENASSIST xmm, xmm/m128, imm8 (rcon)
            // DEST = {SubWord(X1), RotWord(SubWord(X1))^rcon, SubWord(X3), RotWord(SubWord(X3))^rcon}
            // AESE(x,0) = SubBytes(ShiftRows(x)); instead of pre-applying InvShiftRows, fold ShiftRows
            // into the extraction: after AESE, S(in[j]) sits at out[shiftrows^-1(j)], so ONE TBL both
            // undoes ShiftRows and lays out the SubWord/RotWord dwords. Then XOR rcon into dwords 1,3.
            //   needed bytes:  S(in[4,5,6,7]) at out[4,1,14,11];  S(in[12..15]) at out[12,9,6,3]
            //   index vector:  {4,1,14,11, 1,14,11,4, 12,9,6,3, 9,6,3,12}
            if (g_fp_known) fp_drop();
            int s = crypto_rm_vec(I, next);                       // v19 when memory-backed
            if (!g_v26z || nosseopt()) e_v3(A_EOR16, 26, 26, 26); // v26 = 0 (hoisted zero round key)
            g_v26z = 1;
            e_vmov(17, s);
            emit32(A_AESE | (26 << 5) | 17); // v17 = SubBytes(ShiftRows(src))
            e4_vconst16(16, 0x040B0E010B0E0104ull, 0x0C0306090306090Cull);
            // (bytes 0..7 = 04 01 0E 0B 01 0E 0B 04; bytes 8..15 = 0C 09 06 03 09 06 03 0C)
            emit32(A_TBL | (16 << 16) | (17 << 5) | 17); // v17 = laid-out SubWord/RotWord dwords
            e_v3(A_EOR16, 16, 16, 16);
            if (imm) {
                e_movz(16, (uint32_t)imm, 0); // rcon (imm8, zero-extended)
                e4_ins_g(16, 1, 16, 4);       // v16.s[1] = rcon
                e4_ins_g(16, 3, 16, 4);       // v16.s[3] = rcon
            }
            e_v3(A_EOR16, D, 17, 16); // dest = tbl ^ {0,rcon,0,rcon}
            return TX_NEXT;
        }
        default: return TX_FALL;
        }
    }
    return TX_FALL;
}
