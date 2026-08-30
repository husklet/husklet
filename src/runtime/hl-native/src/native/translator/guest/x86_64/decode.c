// translator/guest/x86_64 -- the x86-64 instruction decoder (prefixes, ModRM/SIB, the insn IR).

#include "decoder.h"
#include "../../../engine/options.h"
#include "../../../linux_abi/logical_vma.h"

#include <string.h>
#include <stdlib.h>
#if !defined(_WIN32)
#include <sys/mman.h>
#endif
#if defined(HL_NATIVE_TEST_HOOKS) && !defined(_WIN32)
#include <pthread.h>
#include <sched.h>
#include <sys/wait.h>
#include <unistd.h>
#endif

static hl_x86_instruction_fetch_fn g_instruction_fetch;
static _Atomic uint64_t g_decode_authorized_hits;
static _Atomic uint64_t g_decode_authorized_hits_after_fork;
static _Atomic uint64_t *g_decode_authorized_hits_after_fork_shared;
static _Atomic int g_decode_after_fork;
static int g_decode_diagnostics;

enum { DECODE_MEMO_SLOTS = HL_X86_DECODE_MEMO_SLOTS, X86_MAX_INSN = HL_X86_MAX_INSN };
typedef hl_x86_decode_memo_entry decode_memo_entry;

_Static_assert(sizeof(decode_memo_entry) == 216, "decode memo entry footprint changed");
_Static_assert(offsetof(decode_memo_entry, authority_epoch) == 208, "decode memo epoch left reclaimed padding");
_Static_assert(offsetof(hl_x86_hot_context, memo) == 0, "decode memo must lead its context");
_Static_assert(sizeof(((hl_x86_hot_context *)0)->memo) == 216 * DECODE_MEMO_SLOTS,
               "decode memo table footprint changed");
_Static_assert(sizeof(hl_x86_hot_context) == 221320, "decode hot context footprint changed");

static _Thread_local decode_memo_entry g_decode_memo[DECODE_MEMO_SLOTS];

#if defined(HL_NATIVE_TEST_HOOKS)
static _Thread_local uint64_t g_decode_memo_decodes;
static _Thread_local uint64_t g_decode_memo_hits;
static _Thread_local uint64_t g_decode_authority_samples;
static _Thread_local int g_decode_authority_begin_between_loads;
static _Thread_local unsigned g_decode_transaction_samples;
static _Thread_local unsigned g_decode_transaction_invalidate_sample;
static _Thread_local int g_decode_transaction_invalidate_commit;
static _Atomic int g_hot_context_test_fail_allocation;
static _Atomic int g_hot_context_test_live;
#endif

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

typedef uint64_t decode_authority;

static int decode_authority_stable(decode_authority authority) {
    return authority != 0 && !(authority & HL_GUEST_FETCH_AUTHORITY_DISABLED) &&
           !(authority & HL_GUEST_FETCH_AUTHORITY_ACTIVE_MASK);
}

static decode_authority decode_authority_sample(const hl_x86_hot_context *context) {
#if defined(HL_NATIVE_TEST_HOOKS)
    ++g_decode_authority_samples;
#endif
    if (context == NULL || context->authority_source == NULL) return 0;
    if (context->authority_state == 1) {
#if defined(HL_NATIVE_TEST_HOOKS)
        ++g_decode_transaction_samples;
        if (g_decode_transaction_samples == g_decode_transaction_invalidate_sample) {
            g_decode_transaction_invalidate_sample = 0;
            int begun = hl_guest_fetch_authority_begin();
            hl_guest_fetch_authority_end(begun);
        }
#endif
        return context->authority_epoch;
    }
#if defined(HL_NATIVE_TEST_HOOKS)
    if (g_decode_authority_begin_between_loads) {
        g_decode_authority_begin_between_loads = 0;
        atomic_fetch_add_explicit((_Atomic uint64_t *)context->authority_source,
                                  HL_GUEST_FETCH_AUTHORITY_VERSION_ONE + 1, memory_order_release);
    }
#endif
    return atomic_load_explicit(context->authority_source, memory_order_acquire) &
           ~HL_GUEST_FETCH_AUTHORITY_READER_MASK;
}

static int decode_authority_equal(decode_authority left, decode_authority right) {
    return left == right;
}

static int decode_with(hl_x86_hot_context *context, uint64_t pc, hl_x86_insn *I, decode_memo_entry *entries,
                       hl_x86_context_fetch_fn context_fetch, void *fetch_opaque) {
    uint8_t bytes[X86_MAX_INSN] = {0};
    decode_memo_entry *memo = &entries[(pc ^ (pc >> 10)) & (DECODE_MEMO_SLOTS - 1)];
    int key_matches = memo->length != 0 && memo->pc == pc;
    decode_authority before = 0;
#define FETCH(address, destination, length)                                                                            \
    (context_fetch != NULL ? context_fetch(fetch_opaque, address, destination, length)                                \
                           : instruction_fetch(address, destination, length))
    if (key_matches) {
        before = decode_authority_sample(context);
    }
    if (key_matches && decode_authority_stable(before) && memo->authority_epoch == before) {
        *I = memo->instruction;
        if (context->count_authorized_hits) {
            if (context->authority_state == 1)
                ++context->authority_logical_generation;
            else
                atomic_fetch_add_explicit(&g_decode_authorized_hits, 1, memory_order_relaxed);
            if (atomic_load_explicit(&g_decode_after_fork, memory_order_relaxed))
                atomic_fetch_add_explicit(&g_decode_authorized_hits_after_fork, 1, memory_order_relaxed);
            if (atomic_load_explicit(&g_decode_after_fork, memory_order_relaxed) &&
                g_decode_authorized_hits_after_fork_shared != NULL)
                atomic_fetch_add_explicit(g_decode_authorized_hits_after_fork_shared, 1, memory_order_relaxed);
        }
#if defined(HL_NATIVE_TEST_HOOKS)
        ++g_decode_memo_hits;
#endif
        return memo->length;
    }
    if (key_matches && FETCH(pc, bytes, memo->length) == 0 &&
        memcmp(bytes, memo->bytes, memo->length) == 0) {
        *I = memo->instruction;
        decode_authority after = decode_authority_sample(context);
        if (decode_authority_stable(before) && decode_authority_equal(before, after))
            memo->authority_epoch = before;
        else
            memo->authority_epoch = 0;
#if defined(HL_NATIVE_TEST_HOOKS)
        ++g_decode_memo_hits;
#endif
        return memo->length;
    }

    size_t available = 4096u - (size_t)(pc & UINT64_C(4095));
    if (available > sizeof bytes) available = sizeof bytes;
    if (FETCH(pc, bytes, available) != 0) {
        memset(I, 0, sizeof *I);
        return -1;
    }
    int length = decode_bytes(bytes, I);
    if (length > (int)available) {
        /*
         * Only touch the following guest page when decoding proves that the
         * instruction actually reaches it.  Eagerly fetching all fifteen bytes
         * would incorrectly fault a short instruction at the end of an executable
         * VMA merely because the following page is inaccessible.
         */
        if (FETCH(pc, bytes, sizeof bytes) != 0) {
            memset(I, 0, sizeof *I);
            return -1;
        }
        length = decode_bytes(bytes, I);
    }

    if (length > 0 && length <= X86_MAX_INSN) {
        memo->pc = pc;
        memo->instruction = *I;
        memcpy(memo->bytes, bytes, (size_t)length);
        memo->length = (uint8_t)length;
        decode_authority after = key_matches ? decode_authority_sample(context) : (decode_authority){0};
        memo->authority_epoch = key_matches && decode_authority_stable(before) && decode_authority_equal(before, after)
                                    ? before
                                    : 0;
#if defined(HL_NATIVE_TEST_HOOKS)
        ++g_decode_memo_decodes;
#endif
    }
    return length;
#undef FETCH
}

uint64_t hl_x86_decode_authorized_hits(void) {
    return atomic_load_explicit(&g_decode_authorized_hits, memory_order_relaxed);
}
uint64_t hl_x86_decode_authorized_hits_after_fork(void) {
    return g_decode_authorized_hits_after_fork_shared != NULL
               ? atomic_load_explicit(g_decode_authorized_hits_after_fork_shared, memory_order_relaxed)
               : atomic_load_explicit(&g_decode_authorized_hits_after_fork, memory_order_relaxed);
}
void hl_x86_decode_after_fork_rebind(void) {
    atomic_store_explicit(&g_decode_authorized_hits_after_fork, 0, memory_order_relaxed);
    atomic_store_explicit(&g_decode_after_fork, 1, memory_order_release);
}

void hl_x86_decode_set_diagnostics(int enabled) {
    g_decode_diagnostics = enabled != 0;
#if !defined(_WIN32)
    if (g_decode_diagnostics && g_decode_authorized_hits_after_fork_shared == NULL) {
        void *shared = mmap(NULL, sizeof *g_decode_authorized_hits_after_fork_shared,
                            PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
        if (shared != MAP_FAILED) g_decode_authorized_hits_after_fork_shared = shared;
    }
#endif
}

int hl_x86_decode(uint64_t pc, hl_x86_insn *I) {
    return decode_with(NULL, pc, I, g_decode_memo, NULL, NULL);
}

hl_x86_hot_context *hl_x86_hot_context_create(hl_x86_context_fetch_fn fetch, void *opaque,
                                              const _Atomic uint64_t *byte_unstable) {
#if defined(HL_NATIVE_TEST_HOOKS)
    if (atomic_exchange_explicit(&g_hot_context_test_fail_allocation, 0, memory_order_relaxed)) return NULL;
#endif
    hl_x86_hot_context *context = calloc(1, sizeof *context);
    if (context != NULL) {
        context->fetch_fn = fetch;
        context->fetch_opaque = opaque != NULL ? opaque : &context->fetch;
        context->authority_source = byte_unstable != NULL ? hl_guest_fetch_authority_source() : NULL;
        context->count_authorized_hits = (uint8_t)g_decode_diagnostics;
        context->riprel_readonly_enabled =
            (uint8_t)hl_option_flag_value("HL_TRANSLIT_RIPREL_READONLY", 0);
#if defined(HL_NATIVE_TEST_HOOKS)
        atomic_fetch_add_explicit(&g_hot_context_test_live, 1, memory_order_relaxed);
#endif
    }
    return context;
}

void hl_x86_hot_context_destroy(hl_x86_hot_context *context) {
    if (context == NULL) return;
#if defined(HL_NATIVE_TEST_HOOKS)
    atomic_fetch_sub_explicit(&g_hot_context_test_live, 1, memory_order_relaxed);
#endif
    free(context);
}

int hl_x86_decode_context(hl_x86_hot_context *context, uint64_t pc, hl_x86_insn *I) {
    return decode_with(context, pc, I, context->memo, context->fetch_fn, context->fetch_opaque);
}

int hl_x86_decode_transaction_begin(hl_x86_hot_context *context) {
    if (context == NULL || context->authority_source == NULL) return 0;
    decode_authority authority = atomic_load_explicit(context->authority_source, memory_order_acquire) &
                                 ~HL_GUEST_FETCH_AUTHORITY_READER_MASK;
    if (!decode_authority_stable(authority)) return 0;
    context->authority_epoch = authority;
    context->authority_state = 1;
    context->authority_logical_generation = 0;
#if defined(HL_NATIVE_TEST_HOOKS)
    g_decode_transaction_samples = 0;
#endif
    return 1;
}

int hl_x86_decode_transaction_commit(hl_x86_hot_context *context) {
    if (context == NULL || context->authority_state != 1) return 1;
#if defined(HL_NATIVE_TEST_HOOKS)
    if (g_decode_transaction_invalidate_commit) {
        g_decode_transaction_invalidate_commit = 0;
        int begun = hl_guest_fetch_authority_begin();
        hl_guest_fetch_authority_end(begun);
    }
#endif
    if (hl_guest_fetch_authority_lease(context->authority_epoch)) {
        context->authority_state = 3;
        return 1;
    }
    context->authority_state = 2;
    return 0;
}

int hl_x86_decode_transaction_rejected(const hl_x86_hot_context *context) {
    return context != NULL && context->authority_state == 2;
}

void hl_x86_decode_transaction_abort(hl_x86_hot_context *context) {
    if (context == NULL) return;
    uint64_t rejected = context->authority_epoch;
    if (rejected != 0)
        for (size_t slot = 0; slot < DECODE_MEMO_SLOTS; ++slot)
            if (context->memo[slot].authority_epoch == rejected) context->memo[slot].authority_epoch = 0;
    context->authority_epoch = 0;
    context->authority_logical_generation = 0;
    context->authority_state = 0;
}

void hl_x86_decode_transaction_release(hl_x86_hot_context *context) {
    if (context == NULL) return;
    if (context->authority_state == 3) {
        if (context->count_authorized_hits && context->authority_logical_generation != 0)
            atomic_fetch_add_explicit(&g_decode_authorized_hits, context->authority_logical_generation,
                                      memory_order_relaxed);
        if (context->count_authorized_hits && context->authority_logical_generation != 0 &&
            atomic_load_explicit(&g_decode_after_fork, memory_order_relaxed))
            atomic_fetch_add_explicit(&g_decode_authorized_hits_after_fork,
                                      context->authority_logical_generation, memory_order_relaxed);
        if (context->count_authorized_hits && context->authority_logical_generation != 0 &&
            atomic_load_explicit(&g_decode_after_fork, memory_order_relaxed) &&
            g_decode_authorized_hits_after_fork_shared != NULL)
            atomic_fetch_add_explicit(g_decode_authorized_hits_after_fork_shared,
                                      context->authority_logical_generation, memory_order_relaxed);
        hl_guest_fetch_authority_unlease();
    }
    context->authority_epoch = 0;
    context->authority_logical_generation = 0;
    context->authority_state = 0;
}

#if defined(HL_NATIVE_TEST_HOOKS)
void hl_x86_decode_test_transaction_invalidate_on_sample(unsigned sample) {
    g_decode_transaction_invalidate_sample = sample;
}

void hl_x86_decode_test_transaction_invalidate_before_commit(void) {
    g_decode_transaction_invalidate_commit = 1;
}
#endif

#if defined(HL_NATIVE_TEST_HOOKS)
typedef struct {
    uint64_t pc;
    uint8_t bytes[X86_MAX_INSN];
    int first_page_executable;
    int second_page_executable;
    uint64_t fetches;
    _Atomic uint64_t *authority_to_bump;
    _Atomic uint64_t *authority_to_disable;
    _Atomic uint64_t *unstable_to_latch;
} decode_memo_fixture;

static _Thread_local decode_memo_fixture *g_decode_memo_fixture;

static int decode_memo_fetch(uint64_t guest, void *destination, size_t length) {
    decode_memo_fixture *fixture = g_decode_memo_fixture;
    if (fixture != NULL) fixture->fetches++;
    if (fixture == NULL || guest != fixture->pc || length > sizeof fixture->bytes || !fixture->first_page_executable)
        return -1;
    size_t first_page = 4096u - (size_t)(guest & UINT64_C(4095));
    if (length > first_page && !fixture->second_page_executable) return -1;
    memcpy(destination, fixture->bytes, length);
    if (fixture->authority_to_bump != NULL) {
        atomic_fetch_add_explicit(fixture->authority_to_bump, 2 * HL_GUEST_FETCH_AUTHORITY_VERSION_ONE,
                                  memory_order_release);
    }
    if (fixture->authority_to_disable != NULL)
        atomic_fetch_or_explicit(fixture->authority_to_disable, HL_GUEST_FETCH_AUTHORITY_DISABLED,
                                 memory_order_release);
    if (fixture->unstable_to_latch != NULL)
        atomic_store_explicit(fixture->unstable_to_latch, 1, memory_order_release);
    return 0;
}

static int decode_context_fixture_fetch(void *opaque, uint64_t guest, void *destination, size_t length) {
    decode_memo_fixture *fixture = opaque;
    decode_memo_fixture *saved = g_decode_memo_fixture;
    g_decode_memo_fixture = fixture;
    int result = decode_memo_fetch(guest, destination, length);
    g_decode_memo_fixture = saved;
    return result;
}

int hl_x86_hot_context_test(void) {
    decode_memo_fixture first_fixture = {.pc = UINT64_C(0x50000100), .bytes = {0x90},
                                         .first_page_executable = 1, .second_page_executable = 1};
    decode_memo_fixture second_fixture = first_fixture;
    second_fixture.bytes[0] = 0xc3;
    hl_x86_hot_context *first = hl_x86_hot_context_create(decode_context_fixture_fetch, &first_fixture, NULL);
    hl_x86_hot_context *second = hl_x86_hot_context_create(decode_context_fixture_fetch, &second_fixture, NULL);
    if (first == NULL || second == NULL) {
        hl_x86_hot_context_destroy(first);
        hl_x86_hot_context_destroy(second);
        return -40;
    }
    hl_x86_insn a, b;
    int result = 0;
    if (hl_x86_decode_context(first, first_fixture.pc, &a) != 1 || a.op != 0x90 ||
        hl_x86_decode_context(second, second_fixture.pc, &b) != 1 || b.op != 0xc3)
        result = -41;
    first_fixture.bytes[0] = 0xc3;
    if (result == 0 && (hl_x86_decode_context(first, first_fixture.pc, &a) != 1 || a.op != 0xc3)) result = -42;
#if !defined(_WIN32)
    if (result == 0) {
        pid_t child = fork();
        if (child < 0) result = -43;
        else if (child == 0) {
            first_fixture.bytes[0] = 0x90;
            _exit(hl_x86_decode_context(first, first_fixture.pc, &a) == 1 && a.op == 0x90 ? 0 : 1);
        } else {
            int status = 0;
            if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) result = -44;
        }
    }
#endif
    hl_x86_hot_context_destroy(first);
    hl_x86_hot_context_destroy(second);
    return result;
}

#if !defined(_WIN32)
typedef struct { uint8_t opcode; int result; } hot_context_thread_fixture;

typedef struct {
    _Atomic uint64_t *authority;
    _Atomic int entered;
    _Atomic int begun;
    _Atomic int payload;
} authority_writer_fixture;

static void *authority_writer_worker(void *opaque) {
    authority_writer_fixture *fixture = opaque;
    atomic_store_explicit(&fixture->entered, 1, memory_order_release);
    int begun = hl_guest_fetch_authority_test_begin_observed(fixture->authority, &fixture->begun);
    atomic_store_explicit(&fixture->payload, 1, memory_order_relaxed);
    hl_guest_fetch_authority_test_end(fixture->authority, begun);
    return NULL;
}
static void *hot_context_thread_worker(void *opaque) {
    hot_context_thread_fixture *thread = opaque;
    _Atomic uint64_t unstable = 0, logical = 2, direct = 2;
    decode_memo_fixture fixture = {.pc = UINT64_C(0x50000100), .bytes = {thread->opcode},
                                   .first_page_executable = 1, .second_page_executable = 1};
    hl_x86_hot_context *context = hl_x86_hot_context_create(decode_context_fixture_fetch, &fixture, &unstable);
    if (context != NULL) {
        context->logical_generation_source = &logical;
        context->direct_generation_source = &direct;
    }
    hl_x86_insn instruction, validated, repeated;
    thread->result = context != NULL && hl_x86_decode_context(context, fixture.pc, &instruction) == 1 &&
                             hl_x86_decode_context(context, fixture.pc, &validated) == 1 &&
                             hl_x86_decode_context(context, fixture.pc, &repeated) == 1 &&
                             instruction.op == thread->opcode && validated.op == thread->opcode &&
                             repeated.op == thread->opcode && fixture.fetches == 2
                         ? 0 : -1;
    hl_x86_hot_context_destroy(context);
    return NULL;
}

int hl_x86_hot_context_thread_test(void) {
    hot_context_thread_fixture first = {.opcode = 0x90}, second = {.opcode = 0xc3};
    pthread_t a, b;
    if (pthread_create(&a, NULL, hot_context_thread_worker, &first) != 0) return -50;
    if (pthread_create(&b, NULL, hot_context_thread_worker, &second) != 0) {
        pthread_join(a, NULL);
        return -51;
    }
    if (pthread_join(a, NULL) != 0 || pthread_join(b, NULL) != 0) return -52;
    return first.result == 0 && second.result == 0 && atomic_load(&g_hot_context_test_live) == 0 ? 0 : -53;
}
#else
int hl_x86_hot_context_thread_test(void) { return 0; }
#endif

int hl_x86_hot_context_allocation_test(void) {
    int before = atomic_load_explicit(&g_hot_context_test_live, memory_order_relaxed);
    atomic_store_explicit(&g_hot_context_test_fail_allocation, 1, memory_order_relaxed);
    if (hl_x86_hot_context_create(NULL, NULL, NULL) != NULL) return -60;
    if (atomic_load_explicit(&g_hot_context_test_live, memory_order_relaxed) != before) return -61;
    hl_x86_hot_context *context = hl_x86_hot_context_create(NULL, NULL, NULL);
    if (context == NULL) return -62;
    hl_x86_hot_context_destroy(context);
    return atomic_load_explicit(&g_hot_context_test_live, memory_order_relaxed) == before ? 0 : -63;
}

int hl_x86_decode_authority_test(uint32_t scenario, uint64_t *fetches) {
    _Atomic uint64_t unstable = 0;
    _Atomic uint64_t authority = HL_GUEST_FETCH_AUTHORITY_VERSION_ONE;
    decode_memo_fixture fixture = {
        .pc = scenario == 30 ? UINT64_C(0x50000fff) : UINT64_C(0x50000100),
        .bytes = {0x90}, .first_page_executable = 1, .second_page_executable = 1,
    };
    if (scenario == 30) {
        fixture.bytes[0] = 0x66;
        fixture.bytes[1] = 0x90;
    }
    if (scenario == 32 || scenario == 35) fixture.authority_to_bump = &authority;
    hl_x86_hot_context *context = hl_x86_hot_context_create(decode_context_fixture_fetch, &fixture, &unstable);
    if (context == NULL) return -70;
    context->authority_source = (scenario == 40 || scenario == 41) ? hl_guest_fetch_authority_source() : &authority;
    g_decode_authority_samples = 0;
    hl_x86_insn first, second;
    int result = 0;
    if (hl_x86_decode_context(context, fixture.pc, &first) <= 0) result = -71;
    switch (scenario) {
    case 26: /* Stable bytes authorize a decode-memo hit without another fetch. */
        if (result == 0 && (hl_x86_decode_context(context, fixture.pc, &second) != first.len ||
                            second.op != first.op || fixture.fetches != 2)) result = -72;
        if (result == 0 && (hl_x86_decode_context(context, fixture.pc, &second) != first.len ||
                            second.op != first.op || fixture.fetches != 2)) result = -72;
        break;
    case 27: /* Once writable/executable aliasing is observed, exact byte validation remains mandatory. */
        atomic_fetch_or_explicit(&authority, HL_GUEST_FETCH_AUTHORITY_DISABLED, memory_order_release);
        atomic_store_explicit(&unstable, 1, memory_order_release);
        fixture.bytes[0] = 0xc3;
        if (result == 0 && (hl_x86_decode_context(context, fixture.pc, &second) != 1 ||
                            second.op != 0xc3 || fixture.fetches != 3)) result = -73;
        break;
    case 28: /* MAP_FIXED/unmap-remap changes the direct-map authority. */
        atomic_fetch_add_explicit(&authority, 2 * HL_GUEST_FETCH_AUTHORITY_VERSION_ONE, memory_order_release);
        fixture.bytes[0] = 0xc3;
        if (result == 0 && (hl_x86_decode_context(context, fixture.pc, &second) != 1 ||
                            second.op != 0xc3 || fixture.fetches != 3)) result = -74;
        break;
    case 29: /* Checkpoint/exec replacement changes logical VMA authority. */
        atomic_fetch_add_explicit(&authority, 2 * HL_GUEST_FETCH_AUTHORITY_VERSION_ONE, memory_order_release);
        fixture.bytes[0] = 0xc3;
        if (result == 0 && (hl_x86_decode_context(context, fixture.pc, &second) != 1 ||
                            second.op != 0xc3 || fixture.fetches != 3)) result = -75;
        break;
    case 30: /* A crossing instruction is invalidated when either page loses execute authority. */
        atomic_fetch_add_explicit(&authority, 2 * HL_GUEST_FETCH_AUTHORITY_VERSION_ONE, memory_order_release);
        fixture.second_page_executable = 0;
        if (result == 0 && hl_x86_decode_context(context, fixture.pc, &second) != -1) result = -76;
        break;
    case 31: /* PROT_NONE invalidates a warm same-page entry before use. */
        atomic_fetch_add_explicit(&authority, 2 * HL_GUEST_FETCH_AUTHORITY_VERSION_ONE, memory_order_release);
        fixture.first_page_executable = 0;
        if (result == 0 && hl_x86_decode_context(context, fixture.pc, &second) != -1) result = -77;
        break;
    case 32: /* A generation change during fill cannot authorize that fill. */
        {
        size_t slot = (fixture.pc ^ (fixture.pc >> 10)) & (DECODE_MEMO_SLOTS - 1);
        if (result == 0 && context->memo[slot].authority_epoch != 0) result = -78;
        if (result == 0 && hl_x86_decode_context(context, fixture.pc, &second) != first.len) result = -78;
        fixture.authority_to_bump = NULL;
        if (result == 0 && (fixture.fetches != 2 || context->memo[slot].authority_epoch != 0)) result = -79;
        }
        break;
    case 35: { /* Logical publication racing either fetch path cannot authorize its bytes. */
        size_t slot = (fixture.pc ^ (fixture.pc >> 10)) & (DECODE_MEMO_SLOTS - 1);
        if (result == 0 && context->memo[slot].authority_epoch != 0) result = -84;
        if (result == 0 && hl_x86_decode_context(context, fixture.pc, &second) != first.len) result = -85;
        fixture.authority_to_bump = NULL;
        if (result == 0 && (fixture.fetches != 2 || context->memo[slot].authority_epoch != 0)) result = -86;
        break;
    }
    case 36: /* Cold and colliding memo keys reach fetch and decode without sampling authority. */
        {
        size_t slot = (fixture.pc ^ (fixture.pc >> 10)) & (DECODE_MEMO_SLOTS - 1);
        if (result == 0 && (fixture.fetches != 1 || g_decode_authority_samples != 0 ||
                            context->memo[slot].authority_epoch != 0)) result = -87;
        fixture.pc += UINT64_C(0x401); /* Same 10-bit memo hash as 0x50000100, but a different key. */
        fixture.fetches = 0;
        g_decode_authority_samples = 0;
        if (result == 0 && (hl_x86_decode_context(context, fixture.pc, &second) != 1 ||
                            second.op != first.op || fixture.fetches != 1 || g_decode_authority_samples != 0 ||
                            context->memo[slot].authority_epoch != 0))
            result = -88;
        break;
        }
    case 37: { /* Instability latched during byte validation cannot authorize that memo entry. */
        size_t slot = (fixture.pc ^ (fixture.pc >> 10)) & (DECODE_MEMO_SLOTS - 1);
        fixture.authority_to_disable = &authority;
        fixture.unstable_to_latch = &unstable;
        if (result == 0 && hl_x86_decode_context(context, fixture.pc, &second) != first.len) result = -89;
        if (result == 0 && (fixture.fetches != 2 || atomic_load_explicit(&unstable, memory_order_acquire) != 1 ||
                            context->memo[slot].authority_epoch != 0)) result = -90;
        break;
    }
    case 38: { /* A writer beginning between the ordered loads defeats an otherwise-authorized hit. */
        size_t slot = (fixture.pc ^ (fixture.pc >> 10)) & (DECODE_MEMO_SLOTS - 1);
        if (result == 0 && hl_x86_decode_context(context, fixture.pc, &second) != first.len) result = -91;
        if (result == 0 && (fixture.fetches != 2 || context->memo[slot].authority_epoch == 0)) result = -92;
        g_decode_authority_begin_between_loads = 1;
        if (result == 0 && hl_x86_decode_context(context, fixture.pc, &second) != first.len) result = -93;
        if (result == 0 && (fixture.fetches != 3 || context->memo[slot].authority_epoch != 0)) result = -94;
        break;
    }
    case 39: { /* Overlapping writers keep authority unequal until both have completed. */
        size_t slot = (fixture.pc ^ (fixture.pc >> 10)) & (DECODE_MEMO_SLOTS - 1);
        _Atomic uint32_t first_payload = 0;
        _Atomic uint32_t second_payload = 0;
        if (result == 0 && hl_x86_decode_context(context, fixture.pc, &second) != first.len) result = -95;
        int first_writer = hl_guest_fetch_authority_test_begin(&authority);
        int second_writer = hl_guest_fetch_authority_test_begin(&authority);
        if (result == 0 && hl_x86_decode_context(context, fixture.pc, &second) != first.len) result = -96;
        atomic_store_explicit(&second_payload, UINT32_C(0x22222222), memory_order_relaxed);
        hl_guest_fetch_authority_test_end(&authority, second_writer);
        if (result == 0 && hl_x86_decode_context(context, fixture.pc, &second) != first.len) result = -97;
        atomic_store_explicit(&first_payload, UINT32_C(0x11111111), memory_order_relaxed);
        hl_guest_fetch_authority_test_end(&authority, first_writer);
        if (result == 0 && hl_x86_decode_context(context, fixture.pc, &second) != first.len) result = -98;
        uint64_t settled = atomic_load_explicit(&authority, memory_order_acquire);
        if (result == 0 &&
            (!decode_authority_stable(settled) ||
             atomic_load_explicit(&first_payload, memory_order_relaxed) != UINT32_C(0x11111111) ||
             atomic_load_explicit(&second_payload, memory_order_relaxed) != UINT32_C(0x22222222)))
            result = -109;
        if (result == 0 && hl_x86_decode_context(context, fixture.pc, &second) != first.len) result = -99;
        if (result == 0 && (fixture.fetches != 5 || context->memo[slot].authority_epoch == 0)) result = -100;
        break;
    }
    case 40: { /* Logical-ledger publication alone invalidates an authorized entry. */
        hl_logical_vma_ledger ledger;
        size_t slot = (fixture.pc ^ (fixture.pc >> 10)) & (DECODE_MEMO_SLOTS - 1);
        if (result == 0 && hl_x86_decode_context(context, fixture.pc, &second) != first.len) result = -101;
        if (result == 0 && context->memo[slot].authority_epoch == 0) result = -106;
        if (result == 0 && hl_logical_vma_init(&ledger) != 0) result = -102;
        if (result == 0) hl_logical_vma_destroy(&ledger);
        fixture.bytes[0] = 0xc3;
        if (result == 0 && (hl_x86_decode_context(context, fixture.pc, &second) != 1 || second.op != 0xc3)) result = -103;
        break;
    }
    case 41: { /* Direct executable-registry publication alone invalidates an authorized entry. */
        size_t slot = (fixture.pc ^ (fixture.pc >> 10)) & (DECODE_MEMO_SLOTS - 1);
        if (result == 0 && hl_x86_decode_context(context, fixture.pc, &second) != first.len) result = -104;
        if (result == 0 && context->memo[slot].authority_epoch == 0) result = -107;
        hl_x86_decode_test_invalidate_direct_registry();
        fixture.bytes[0] = 0xc3;
        if (result == 0 && (hl_x86_decode_context(context, fixture.pc, &second) != 1 || second.op != 0xc3)) result = -105;
        break;
    }
    case 42: { /* Registered writer intent drains a commit lease before publishing. */
        uint64_t token = atomic_load_explicit(&authority, memory_order_acquire);
        authority_writer_fixture writer = {.authority = &authority};
        pthread_t thread;
        if (!hl_guest_fetch_authority_test_lease(&authority, token)) {
            result = -110;
            break;
        }
        if (hl_guest_fetch_authority_test_lease(&authority, token) ||
            (atomic_load_explicit(&authority, memory_order_acquire) &
             HL_GUEST_FETCH_AUTHORITY_READER_MASK) != HL_GUEST_FETCH_AUTHORITY_READER_ONE) {
            result = -115;
            hl_guest_fetch_authority_test_unlease(&authority);
            break;
        }
        if (pthread_create(&thread, NULL, authority_writer_worker, &writer) != 0) {
            hl_guest_fetch_authority_test_unlease(&authority);
            result = -110;
            break;
        }
        while (!atomic_load_explicit(&writer.entered, memory_order_acquire)) sched_yield();
        while (!atomic_load_explicit(&writer.begun, memory_order_acquire)) sched_yield();
        uint64_t draining = atomic_load_explicit(&authority, memory_order_acquire);
        if (atomic_load_explicit(&writer.payload, memory_order_relaxed) != 0 ||
            (draining & HL_GUEST_FETCH_AUTHORITY_READER_MASK) != HL_GUEST_FETCH_AUTHORITY_READER_ONE ||
            !(draining & HL_GUEST_FETCH_AUTHORITY_ACTIVE_MASK) ||
            hl_guest_fetch_authority_test_lease(&authority, token))
            result = -111;
        hl_guest_fetch_authority_test_unlease(&authority);
        if (pthread_join(thread, NULL) != 0 || atomic_load_explicit(&writer.payload, memory_order_relaxed) != 1)
            result = -112;
        break;
    }
    case 43: { /* A completed writer between capture and commit rejects the old token. */
        uint64_t token = atomic_load_explicit(&authority, memory_order_acquire);
        int begun = hl_guest_fetch_authority_test_begin(&authority);
        hl_guest_fetch_authority_test_end(&authority, begun);
        if (hl_guest_fetch_authority_test_lease(&authority, token)) result = -113;
        uint64_t current = atomic_load_explicit(&authority, memory_order_acquire);
        if (result == 0 && !hl_guest_fetch_authority_test_lease(&authority, current)) result = -114;
        hl_guest_fetch_authority_test_unlease(&authority);
        break;
    }
    case 33: /* Epoch wrap clears old entries rather than aliasing epoch zero. */
        atomic_store_explicit(&authority,
                              HL_GUEST_FETCH_AUTHORITY_DISABLED - HL_GUEST_FETCH_AUTHORITY_VERSION_ONE,
                              memory_order_release);
        if (hl_guest_fetch_authority_test_begin(&authority) != 0 ||
            !(atomic_load_explicit(&authority, memory_order_acquire) & HL_GUEST_FETCH_AUTHORITY_DISABLED))
            result = -108;
        fixture.bytes[0] = 0xc3;
        if (result == 0 && (hl_x86_decode_context(context, fixture.pc, &second) != 1 ||
                            second.op != 0xc3 || context->memo[(fixture.pc ^ (fixture.pc >> 10)) &
                                                               (DECODE_MEMO_SLOTS - 1)].authority_epoch != 0)) result = -80;
        break;
#if !defined(_WIN32)
    case 34: { /* A fork child cannot authorize bytes changed in its inherited address-space view. */
        int inherited_writer = hl_guest_fetch_authority_test_begin(&authority);
        pid_t child = fork();
        if (child < 0) result = -81;
        else if (child == 0) {
            atomic_fetch_add_explicit(&authority, 2 * HL_GUEST_FETCH_AUTHORITY_VERSION_ONE, memory_order_release);
            fixture.bytes[0] = 0xc3;
            _exit(hl_x86_decode_context(context, fixture.pc, &second) == 1 && second.op == 0xc3 ? 0 : 1);
        } else {
            int status = 0;
            if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) result = -82;
        }
        hl_guest_fetch_authority_test_end(&authority, inherited_writer);
        break;
    }
    case 44: { /* A private fork child cannot inherit an undischargeable commit lease. */
        uint64_t token = atomic_load_explicit(&authority, memory_order_acquire);
        if (!hl_guest_fetch_authority_test_lease(&authority, token)) {
            result = -115;
            break;
        }
        pid_t child = fork();
        if (child < 0) result = -116;
        else if (child == 0) {
            hl_guest_fetch_authority_test_after_fork_child(&authority);
            hl_guest_fetch_authority_test_after_fork_rebind(&authority);
            uint64_t fresh = atomic_load_explicit(&authority, memory_order_acquire);
            int old_rejected = !hl_guest_fetch_authority_test_lease(&authority, token);
            int fresh_accepted = decode_authority_stable(fresh) &&
                                 hl_guest_fetch_authority_test_lease(&authority, fresh);
            if (fresh_accepted) hl_guest_fetch_authority_test_unlease(&authority);
            fixture.bytes[0] = 0xc3;
            int revalidated = hl_x86_decode_context(context, fixture.pc, &second) == 1 && second.op == 0xc3;
            _exit(old_rejected && fresh_accepted && revalidated ? 0 : 1);
        } else {
            int status = 0;
            if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) result = -117;
        }
        hl_guest_fetch_authority_test_unlease(&authority);
        break;
    }
#endif
    default: result = -83;
    }
    *fetches = fixture.fetches;
    hl_x86_hot_context_destroy(context);
    return result;
}

/* Invoked through the existing target-local scenario/count hook. */
int hl_x86_decode_memo_test(uint32_t scenario, uint64_t *decodes) {
    decode_memo_fixture fixture = {
        .pc = scenario == 7 ? UINT64_C(0x50000fff) : UINT64_C(0x50000100),
        .bytes = {0x90},
        .first_page_executable = 1,
        .second_page_executable = 1,
    };
    size_t slot = (fixture.pc ^ (fixture.pc >> 10)) & (DECODE_MEMO_SLOTS - 1);
    decode_memo_entry saved = g_decode_memo[slot];
    hl_x86_instruction_fetch_fn saved_fetch = g_instruction_fetch;
    memset(&g_decode_memo[slot], 0, sizeof g_decode_memo[slot]);
    g_decode_memo_decodes = 0;
    g_decode_memo_hits = 0;
    g_decode_memo_fixture = &fixture;
    g_instruction_fetch = decode_memo_fetch;
    hl_x86_insn first;
    hl_x86_insn second;
    int result = 0;

    if (scenario == 7) {
        fixture.bytes[0] = 0x66;
        fixture.bytes[1] = 0x90;
    }
    if (hl_x86_decode(fixture.pc, &first) <= 0) result = -20;
    switch (scenario) {
    case 5: /* Stable PC: one decode followed by permission-and-byte-checked hits. */
        for (int i = 0; result == 0 && i < 31; ++i)
            if (hl_x86_decode(fixture.pc, &second) != first.len || second.op != first.op) result = -21;
        if (g_decode_memo_decodes != 1 || g_decode_memo_hits != 31) result = -22;
        break;
    case 6:  /* In-place instruction rewrite invalidates by byte comparison. */
    case 9:  /* MAP_FIXED/unmap-remap replacement at the same virtual PC. */
    case 10: /* A guest exec image replacing the same virtual PC cannot reuse stale IR. */
        fixture.bytes[0] = 0xc3;
        if (hl_x86_decode(fixture.pc, &second) != 1 || second.op != 0xc3 || first.op == second.op) result = -23;
        if (g_decode_memo_decodes != 2 || g_decode_memo_hits != 0) result = -24;
        break;
#if !defined(_WIN32)
    case 11: { /* A fork-inherited memo cannot authorize stale bytes in the child. */
        pid_t child = fork();
        if (child < 0) {
            result = -29;
            break;
        }
        if (child == 0) {
            fixture.bytes[0] = 0xc3;
            int child_ok = hl_x86_decode(fixture.pc, &second) == 1 && second.op == 0xc3 && first.op != second.op;
            _exit(child_ok ? 0 : 1);
        }
        int status = 0;
        if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) result = -30;
        break;
    }
#endif
    case 7: /* A crossing instruction revalidates both pages on every hit. */
        if (hl_x86_decode(fixture.pc, &second) != 2 || g_decode_memo_hits != 1) result = -25;
        fixture.second_page_executable = 0;
        if (hl_x86_decode(fixture.pc, &second) != -1) result = -26;
        break;
    case 8: /* mprotect(PROT_NONE) rejects a warm entry before it can be used. */
        fixture.first_page_executable = 0;
        if (hl_x86_decode(fixture.pc, &second) != -1) result = -27;
        break;
    default: result = -28;
    }

    *decodes = g_decode_memo_decodes;
    g_instruction_fetch = saved_fetch;
    g_decode_memo_fixture = NULL;
    g_decode_memo[slot] = saved;
    return result;
}
#endif
