// translator/guest/x86_64 -- the x86-64 instruction decoder (prefixes, ModRM/SIB, the insn IR).

#include "decoder.h"

#include <string.h>

static hl_x86_instruction_fetch_fn g_instruction_fetch;

void hl_x86_decode_set_instruction_fetch(hl_x86_instruction_fetch_fn fetch) {
    g_instruction_fetch = fetch;
}

static int instruction_fetch(uint64_t guest, void *destination, size_t length) {
    if (g_instruction_fetch != NULL) return g_instruction_fetch(guest, destination, length);
    memcpy(destination, (const void *)(uintptr_t)guest, length);
    return 0;
}

// ---------------- x86-64 decoder ----------------
static int op_has_modrm(int two, uint8_t op) {
    if (two) {
        if (op == 0x05) return 0;                             // syscall
        if (op == 0xA2 || op == 0x31 || op == 0x77) return 0; // cpuid / rdtsc / emms (no modrm)
        if (op >= 0xC8 && op <= 0xCF) return 0;               // bswap reg (encoded in opcode)
        if (op == 0x1E) return 1;                             // endbr (modrm follows)
        if ((op & 0xF0) == 0x80) return 0;                    // jcc rel32
        if ((op & 0xF0) == 0x90) return 1;                    // setcc
        if ((op & 0xF0) == 0x40) return 1;                    // cmovcc
        if (op == 0xB6 || op == 0xB7 || op == 0xBE || op == 0xBF || op == 0xAF) return 1; // movzx/sx/imul
        if (op == 0x1F) return 1;                                                         // nop r/m
        if (op == 0x10 || op == 0x11 || op == 0x28 || op == 0x29 || op == 0x6E || op == 0x7E || op == 0x6F ||
            op == 0x7F || op == 0xD6 || op == 0xEF || op == 0x57 || op == 0x54)
            return 1; // SSE
        return 1;
    }
    if (op >= 0x50 && op <= 0x5F) return 0;                             // push/pop r
    if (op >= 0x70 && op <= 0x7F) return 0;                             // jcc rel8
    if (op == 0xE8 || op == 0xE9 || op == 0xEB || op == 0xE3) return 0; // call/jmp rel, jrcxz
    if (op == 0xE0 || op == 0xE1 || op == 0xE2) return 0;               // loopne/loope/loop rel8
    if (op == 0xC3 || op == 0xC2 || op == 0xC9 || op == 0x90 || op == 0xF4 || op == 0x99 || op == 0x98) return 0;
    if (op >= 0x91 && op <= 0x97) return 0;                                           // xchg eax, rN
    if (op == 0x9B || op == 0x9C || op == 0x9D || op == 0x9E || op == 0x9F) return 0; // fwait/pushf/popf/sahf/lahf
    if (op == 0x9C || op == 0x9D || op == 0xFC || op == 0xFD || op == 0xCC || op == 0xF5 || op == 0xF8 || op == 0xF9)
        return 0;                                                // pushf/popf/cld/std/int3/cmc/clc/stc
    if (op >= 0xA0 && op <= 0xA3) return 0;                      // mov AL/eAX/rAX <-> moffs (direct addr imm, no modrm)
    if (op >= 0xA4 && op <= 0xAF) return 0;                      // movs/cmps/stos/lods/scas + test al,imm(A8/A9)
    if (op >= 0xB0 && op <= 0xBF) return 0;                      // mov r8/r, imm
    if (op < 0x40 && ((op & 7) == 4 || (op & 7) == 5)) return 0; // ALU al/eAX, imm (04/05,0C/0D,...,3C/3D)
    if (op == 0xA8 || op == 0xA9) return 0;                      // test al/eax, imm
    if (op == 0x68 || op == 0x6A) return 0;                      // push imm
    if (op == 0xCC || op == 0xF1) return 0;
    if (op == 0xD7) return 0; // XLATB: implicit operands (AL, DS:RBX), no ModRM
    // ALU group, mov, lea, test, group1/2/3, etc. all have modrm
    return 1;
}

// immediate size (bytes) for the opcodes we handle; 0 if none.
static int op_imm_bytes(struct insn *I) {
    int two = I->two;
    uint8_t op = I->op;
    int os = I->opsize;
    if (I->map3) return I->map3 == 3 ? 1 : 0; // legacy 0F3A carries an imm8; 0F38 carries none
    if (I->vex) {
        // VEX/EVEX immediates: the 0F3A map is almost entirely "...,imm8" forms; in the 0F map only the
        // shuffle/compare/insert group carries an imm8; the 0F38 map carries none. (vex_l/W never add bytes.)
        if (I->vex_map == 3) return 1;
        if (I->vex_map == 1 && (op == 0x70 || op == 0x71 || op == 0x72 || op == 0x73 || op == 0xC2 || op == 0xC4 ||
                                op == 0xC5 || op == 0xC6))
            return 1;
        return 0;
    }
    if (two) {
        if ((op & 0xF0) == 0x80) return 4;      // jcc rel32
        if (op == 0xBA) return 1;               // bt/bts/btr/btc r/m, imm8
        if (op == 0xA4 || op == 0xAC) return 1; // shld/shrd r/m, r, imm8
        if (op == 0x70 || op == 0x71 || op == 0x72 || op == 0x73 || op == 0xC2 || op == 0xC4 || op == 0xC5 ||
            op == 0xC6)
            return 1; // SSE imm
        return 0;
    }
    if (op == 0xC2) return 2;                             // ret imm16
    if (op >= 0x70 && op <= 0x7F) return 1;               // jcc rel8
    if (op == 0xEB || op == 0xE3) return 1;               // jmp rel8 / jrcxz rel8
    if (op == 0xE0 || op == 0xE1 || op == 0xE2) return 1; // loopne/loope/loop rel8
    if (op == 0xE9 || op == 0xE8) return 4;               // jmp/call rel32
    if (op >= 0xA0 && op <= 0xA3)
        return I->addr32 ? 4 : 8;           // mov moffs: address-size direct offset (64-bit default; 4 under 0x67)
    if (op >= 0xB0 && op <= 0xB7) return 1; // mov r8, imm8
    if (op >= 0xB8 && op <= 0xBF) return os == 8 ? 8 : (os == 2 ? 2 : 4); // mov r,imm (movabs if W)
    if (op < 0x40 && (op & 7) == 4) return 1;                             // ALU al, imm8
    if (op < 0x40 && (op & 7) == 5) return os == 2 ? 2 : 4;               // ALU eAX, imm16/32
    if (op == 0xA8) return 1;
    if (op == 0xA9) return os == 2 ? 2 : 4; // test
    if (op == 0x6A) return 1;
    if (op == 0x68) return os == 2 ? 2 : 4; // push imm
    if (op == 0x80) return 1;               // group1 r/m8, ib
    if (op == 0x81) return os == 2 ? 2 : 4; // group1 r/m, iz
    if (op == 0x83) return 1;               // group1 r/m, ib (sign-ext)
    if (op == 0xC6) return 1;               // mov r/m8, ib
    if (op == 0xC7) return os == 2 ? 2 : 4; // mov r/m, iz
    if (op == 0xC0 || op == 0xC1) return 1; // shift r/m, ib
    // & 7: I->reg carries REX.R, but a group's /reg is an OPCODE EXTENSION and REX.R does not extend it.
    // Without the mask `47 f6 c0 ib` decoded as NOT (no immediate) and the length came out short, so the
    // immediate byte was executed as the next instruction.
    if (op == 0xF6) return ((I->reg & 7) <= 1) ? 1 : 0; // test r/m8,ib only for /0,/1
    if (op == 0xF7) return ((I->reg & 7) <= 1) ? (os == 2 ? 2 : 4) : 0;
    if (op == 0x69) return os == 2 ? 2 : 4; // imul r,r/m,iz
    if (op == 0x6B) return 1;               // imul r,r/m,ib
    return 0;
}

// returns instruction length, fills I. On a decode it can't handle for length, returns
// the bytes consumed so far so the reporter can show them.
static int decode_bytes(const uint8_t bytes[15], hl_x86_insn *I) {
    memset(I, 0, sizeof *I);
    const uint8_t *p = bytes;
    int n = 0;
    I->opsize = 4;
    I->m_scale = 0;
    // legacy prefixes
    for (;;) {
        uint8_t b = p[n];
        if (b == 0x66) {
            I->opsize = 2;
            I->p66 = 1;
            n++;
            continue;
        }
        if (b == 0x67) {
            I->addr32 = 1;
            n++;
            continue;
        }
        if (b == 0xF0) {
            I->lock = 1;
            n++;
            continue;
        }
        if (b == 0xF2) {
            I->repne = 1;
            n++;
            continue;
        }
        if (b == 0xF3) {
            I->rep = 1;
            n++;
            continue;
        }
        if (b == 0x64) {
            I->seg = 1;
            n++;
            continue;
        } // fs
        if (b == 0x65) {
            I->seg = 2;
            n++;
            continue;
        } // gs
        if (b == 0x2E || b == 0x36 || b == 0x3E || b == 0x26) {
            n++;
            continue;
        }
        break;
    }
    // VEX (C5 2-byte / C4 3-byte) and EVEX (62) -- AVX/AVX2/AVX-512. These REPLACE REX + the 0F escape:
    // the opcode map (0F/0F38/0F3A), an implied mandatory prefix (pp), the vvvv 2nd source, vector length
    // (L) and W are packed in. We decode them so the instruction LENGTH is correct (otherwise the whole
    // block desyncs) and avx.c can emulate. C4/C5/62 are unambiguous in 64-bit mode (their legacy meanings
    // LES/LDS/BOUND are invalid in long mode), so the lead byte alone disambiguates.
    uint8_t op;
    if (p[n] == 0xC5) { // 2-byte VEX: C5 R̄v̄v̄v̄v̄Lpp  (map fixed to 0F)
        uint8_t b1 = p[n + 1];
        n += 2;
        I->vex = 1;
        I->two = 1;
        I->rexR = ((b1 >> 7) & 1) ^ 1;
        I->vvvv = (~(b1 >> 3)) & 0xF;
        I->vex_l = (b1 >> 2) & 1;
        I->vex_pp = b1 & 3;
        I->vex_map = 1;
        if (I->vex_pp == 1) I->p66 = 1;
        op = p[n++];
    } else if (p[n] == 0xC4) { // 3-byte VEX: C4 R̄X̄B̄mmmmm  Wv̄v̄v̄v̄Lpp
        uint8_t b1 = p[n + 1], b2 = p[n + 2];
        n += 3;
        I->vex = 1;
        I->two = 1;
        I->rexR = ((b1 >> 7) & 1) ^ 1;
        I->rexX = ((b1 >> 6) & 1) ^ 1;
        I->rexB = ((b1 >> 5) & 1) ^ 1;
        I->vex_map = b1 & 0x1F;
        I->vex_w = (b2 >> 7) & 1;
        if (I->vex_w) I->opsize = 8;
        I->vvvv = (~(b2 >> 3)) & 0xF;
        I->vex_l = (b2 >> 2) & 1;
        I->vex_pp = b2 & 3;
        if (I->vex_pp == 1) I->p66 = 1;
        op = p[n++];
    } else if (p[n] == 0x62) { // EVEX: 62 R̄X̄B̄R̄'00mm  Wv̄v̄v̄v̄1pp  z L'L b V̄' aaa
        uint8_t e0 = p[n + 1], e1 = p[n + 2], e2 = p[n + 3];
        n += 4;
        I->vex = 1;
        I->evex = 1;
        I->two = 1;
        I->rexR = ((e0 >> 7) & 1) ^ 1;
        I->rexX = ((e0 >> 6) & 1) ^ 1;
        I->rexB = ((e0 >> 5) & 1) ^ 1;
        I->vex_map = e0 & 3;
        I->vex_w = (e1 >> 7) & 1;
        if (I->vex_w) I->opsize = 8;
        I->vvvv = ((~(e1 >> 3)) & 0xF) | (((e2 >> 3) & 1) ? 0 : 16); // V' extends vvvv to 5 bits
        I->vex_pp = e1 & 3;
        if (I->vex_pp == 1) I->p66 = 1;
        I->vex_l = (e2 >> 5) & 3; // L'L: 0=128, 1=256, 2=512
        I->evex_z = (e2 >> 7) & 1;
        I->evex_b = (e2 >> 4) & 1;
        I->evex_mask = e2 & 7;
        op = p[n++];
    } else {
        // REX (legacy)
        if ((p[n] & 0xF0) == 0x40) {
            uint8_t rex = p[n++];
            I->has_rex = 1;
            I->rexW = (rex >> 3) & 1;
            I->rexR = (rex >> 2) & 1;
            I->rexX = (rex >> 1) & 1;
            I->rexB = rex & 1;
            if (I->rexW) I->opsize = 8;
        }
        op = p[n++];
        if (op == 0x0F) {
            I->two = 1;
            op = p[n++];
            if (op == 0x38 || op == 0x3A) { // legacy 3-byte escape (SSSE3/SSE4/AES/SHA/CRC32/MOVBE)
                I->map3 = (op == 0x38) ? 2 : 3;
                op = p[n++];
            }
        }
    }
    I->op = op;
    // modrm + sib + disp. Every VEX/EVEX insn we handle carries a ModRM except vzeroupper/vzeroall (0F 77).
    if (I->vex ? (op != 0x77) : (I->map3 ? 1 : op_has_modrm(I->two, op))) { // every 0F38/0F3A op has ModRM
        uint8_t m = p[n++];
        I->has_modrm = 1;
        I->modrm = m;
        I->mod = m >> 6;
        I->reg = ((m >> 3) & 7) | (I->rexR << 3);
        I->rm = m & 7;
        if (I->mod == 3) {
            I->rm_reg = I->rm | (I->rexB << 3);
        } else {
            I->is_mem = 1;
            int base, idx = -1, scale = 0;
            if (I->rm == 4) { // SIB
                uint8_t s = p[n++];
                scale = s >> 6;
                idx = ((s >> 3) & 7) | (I->rexX << 3);
                base = (s & 7);
                // VSIB (AVX2 gather VEX 0F38 90/91/92/93): the SIB index is a VECTOR register, so the
                // GPR-only "index field == 4 means no index" rule does NOT apply -- ymm4 is a valid index.
                int is_vsib = I->vex && I->vex_map == 2 && (op == 0x90 || op == 0x91 || op == 0x92 || op == 0x93);
                if (((s >> 3) & 7) == 4 && !I->rexX && !is_vsib) idx = -1; // no index
                if ((s & 7) == 5 && I->mod == 0) {
                    I->m_hasbase = 0;
                } else {
                    I->m_hasbase = 1;
                    I->m_base = base | (I->rexB << 3);
                }
            } else if (I->rm == 5 && I->mod == 0) { // RIP-relative
                I->rip_rel = 1;
            } else {
                I->m_hasbase = 1;
                I->m_base = I->rm | (I->rexB << 3);
            }
            if (idx >= 0) {
                I->m_hasindex = 1;
                I->m_index = idx;
                I->m_scale = scale;
            }
            // displacement
            if (I->mod == 1) {
                I->disp = (int8_t)p[n];
                n += 1;
            } else if (I->rip_rel || I->mod == 2 || (!I->m_hasbase && I->rm == 4)) {
                I->disp = (int32_t)((uint32_t)p[n] | ((uint32_t)p[n + 1] << 8) | ((uint32_t)p[n + 2] << 16) |
                                    ((uint32_t)p[n + 3] << 24));
                n += 4;
            }
        }
    }
    // immediate
    int ib = op_imm_bytes(I);
    I->imm_bytes = ib;
    if (ib) {
        uint64_t v = 0;
        for (int i = 0; i < ib; i++)
            v |= (uint64_t)p[n + i] << (8 * i);
        I->imm = (ib == 1) ? (int8_t)v : (ib == 2) ? (int16_t)v : (ib == 4) ? (int32_t)v : (int64_t)v;
        n += ib;
    }
    I->len = n;
    return n;
}

int hl_x86_decode(uint64_t pc, hl_x86_insn *I) {
    uint8_t bytes[15] = {0};
    size_t available = 4096u - (size_t)(pc & UINT64_C(4095));
    if (available > sizeof bytes) available = sizeof bytes;
    if (instruction_fetch(pc, bytes, available) != 0) {
        memset(I, 0, sizeof *I);
        return -1;
    }
    int length = decode_bytes(bytes, I);
    if (length <= (int)available) return length;

    /*
     * Only touch the following guest page when decoding proves that the
     * instruction actually reaches it.  Eagerly fetching all fifteen bytes
     * would incorrectly fault a short instruction at the end of an executable
     * VMA merely because the following page is inaccessible.
     */
    if (instruction_fetch(pc, bytes, sizeof bytes) != 0) {
        memset(I, 0, sizeof *I);
        return -1;
    }
    return decode_bytes(bytes, I);
}
