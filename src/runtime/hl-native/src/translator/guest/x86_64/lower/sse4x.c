// translator/guest/x86_64 -- 0F38/0F3A GPR + lane "glue" lowering (AES-GCM endgame, perf wave 2).
//
// WHY: the shipped crypto/shuffle glue (lower/crypto.c) lowered AES-NI/PCLMUL/SHA-NI + the SSSE3/SSE4
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
// AESKEYGENASSIST remains with the other AES operations in lower/crypto.c.
//
// Same quality bar as lower/crypto.c: byte-exact vs the qemu oracle (hl-tests completeness guest),
// no EFLAGS interaction (none of these ops read or write flags), scratch = x16 (GPR), v16..v19 (vector).
// A/B kill-switch: NOSSEOPT=1 falls back to the do_sse3b block-exit path (same gate as the shuffle glue).

#include "sse4x.h"
#include "x87.h"

#include "../encoding.h"

#define A_EOR16 0x6E201C00u

// ---- tiny GPR encoders local to this class ----
static void e4_rev(int rd, int rn, int nb) { // byte-reverse: REV16 (nb=2) / REV Wd (4) / REV Xd (8)
    uint32_t registers = ((uint32_t)rn << 5) | (uint32_t)rd;
    emit32((nb == 2 ? UINT32_C(0x5AC00400) : nb == 4 ? UINT32_C(0x5AC00800) : UINT32_C(0xDAC00C00)) | registers);
}

// INS Vd.<T>[i], Rn (general): imm5 selects lane size+index. nb = element bytes (1/2/4/8).
static void e4_ins_g(int vd, int i, int rn, int nb) {
    unsigned lane = (unsigned)i;
    unsigned imm5 = (nb == 1)   ? (lane << 1) | 1u
                    : (nb == 2) ? (lane << 2) | 2u
                    : (nb == 4) ? (lane << 3) | 4u
                                : (lane << 4) | 8u;
    emit32(UINT32_C(0x4E001C00) | (imm5 << 16) | ((uint32_t)rn << 5) | (uint32_t)vd);
}

// UMOV Rd, Vn.<T>[i]: zero-extending lane read into a GPR (W for nb<=4, X for nb=8).
static void e4_umov(int rd, int vn, int i, int nb) {
    unsigned lane = (unsigned)i;
    unsigned imm5 = (nb == 1)   ? (lane << 1) | 1u
                    : (nb == 2) ? (lane << 2) | 2u
                    : (nb == 4) ? (lane << 3) | 4u
                                : (lane << 4) | 8u;
    emit32((nb == 8 ? UINT32_C(0x4E003C00) : UINT32_C(0x0E003C00)) | (imm5 << 16) | ((uint32_t)vn << 5) | (uint32_t)rd);
}

// Lower a legacy 0F38/0F3A GPR/lane-glue opcode inline. Returns TX_NEXT if emitted, TX_FALL to defer
// to do_sse3b. Called AFTER translate_crypto (crypto.c) declined the opcode.
int hl_x86_lower_sse4x(struct insn *I, uint64_t next, const hl_x86_sse4x_state *state) {
    uint8_t op = I->op;
    int D = I->reg; // all scratch below is x16/x17 + v16..v19: the v26/v27 hoist claims are unaffected

    if (I->map3 == 2 && (op == 0xF0 || op == 0xF1)) {
        if (!state->optimize) return TX_FALL;
        if (I->repne) { // ---- CRC32 r32/64, r/m: x86 CRC32 is the Castagnoli poly == ARM CRC32C* ----
            int nb = (op == 0xF0) ? 1 : I->opsize;
            int src;
            if (I->is_mem) {
                emit_ea(I, next);
                emit_memory_guard(17, (uint64_t)nb, next - (uint64_t)I->len, X86_SOFT_READ);
                e_load(nb, 16, 17);
                src = 16;
            } else {
                src = (nb == 1) ? byte_val(I, I->rm_reg, 16) : I->rm_reg; // ah/ch/dh/bh via shift
            }
            static const uint32_t enc[4] = {0x1AC05000u, 0x1AC05400u, 0x1AC05800u, 0x9AC05C00u}; // cb/ch/cw/cx
            // acc = low 32 of the dest GPR; Wd write zero-extends (matches x86: r32, and r64 hi-32 zeroed)
            emit32(enc[nb == 1   ? 0
                       : nb == 2 ? 1
                       : nb == 4 ? 2
                                 : 3] |
                   ((uint32_t)src << 16) | ((uint32_t)D << 5) | (uint32_t)D);
            return TX_NEXT;
        }
        if (I->rep || !I->is_mem) return TX_FALL; // F3 form / reg-reg MOVBE are #UD -> softmulator path
        // ---- MOVBE: byte-swapping load (F0: r <- bswap(m)) / store (F1: m <- bswap(r)) ----
        int nb = I->opsize;
        emit_ea(I, next);
        if (op == 0xF0) {
            emit_memory_guard(17, (uint64_t)nb, next - (uint64_t)I->len, X86_SOFT_READ);
            e_load(nb, 16, 17); // zero-extending load
            if (nb == 2) {      // 16-bit dest merges into the low 16 bits of the guest reg
                e4_rev(16, 16, 2);
                e_bfi(D, 16, 0, 16, 1);
            } else {
                e4_rev(D, 16, nb); // W write zero-extends (32-bit); X full (64-bit)
            }
        } else {
            emit_memory_guard(17, (uint64_t)nb, next - (uint64_t)I->len, X86_SOFT_WRITE);
            e4_rev(16, D, nb); // rev16 leaves only the low halfword meaningful; the 2-byte store reads just that
            e_store(nb, 16, 17);
            if (emit_soft_memory_active()) emit_soft_store_commit((uint64_t)nb);
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
            if (!state->optimize) return TX_FALL;
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int nb = (op == 0x14) ? 1 : (op == 0x15) ? 2 : (op == 0x17) ? 4 : (I->rexW ? 8 : 4);
            int lane = imm & (16 / nb - 1);
            if (I->is_mem) {
                emit_ea(I, next);
                emit_memory_guard(17, (uint64_t)nb, next - (uint64_t)I->len, X86_SOFT_WRITE);
                e4_umov(16, D, lane, nb);
                e_store(nb, 16, 17);
                if (emit_soft_memory_active()) emit_soft_store_commit((uint64_t)nb);
            } else {
                e4_umov(I->rm_reg, D, lane, nb); // zero-extends into the full GPR (x86 r32 semantics)
            }
            return TX_NEXT;
        }
        case 0x20:   // PINSRB xmm, r32/m8, imm
        case 0x22: { // PINSRD/Q xmm, r/m32|64, imm  (REX.W selects Q)
            if (!state->optimize) return TX_FALL;
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int nb = (op == 0x20) ? 1 : (I->rexW ? 8 : 4);
            int lane = imm & (16 / nb - 1);
            int src;
            if (I->is_mem) {
                emit_ea(I, next);
                emit_memory_guard(17, (uint64_t)nb, next - (uint64_t)I->len, X86_SOFT_READ);
                e_load(nb, 16, 17);
                src = 16;
            } else {
                src = I->rm_reg; // PINSRB reg form reads the LOW byte of a r32 (never ah/…): reg direct
            }
            e4_ins_g(D, lane, src, nb);
            return TX_NEXT;
        }
        case 0x21: { // INSERTPS xmm, xmm/m32, imm: insert one dword lane, then zero lanes per imm[3:0]
            if (!state->optimize) return TX_FALL;
            if (hl_x86_x87_known()) hl_x86_x87_drop();
            int countD = (imm >> 4) & 3, zmask = imm & 0xf;
            if (I->is_mem) {
                emit_ea(I, next);
                emit_memory_guard(17, 4, next - (uint64_t)I->len, X86_SOFT_READ);
                hl_x86_emit_load_scalar32(19, 17); // m32 -> lane 0 of v19 (rest zeroed)
                hl_x86_emit_insert_scalar32(D, countD, 19, 0);
            } else {
                hl_x86_emit_insert_scalar32(D, countD, I->rm_reg, (imm >> 6) & 3);
            }
            if (zmask) {
                hl_x86_emit_vector3(A_EOR16, 16, 16, 16);
                for (int j = 0; j < 4; j++)
                    if (zmask & (1 << j)) hl_x86_emit_insert_scalar32(D, j, 16, 0);
            }
            return TX_NEXT;
        }
        default: return TX_FALL;
        }
    }
    return TX_FALL;
}
