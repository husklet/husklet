#ifndef HL_TRANSLATOR_X86_64_DECODER_H
#define HL_TRANSLATOR_X86_64_DECODER_H

#include <stddef.h>
#include <stdint.h>
#include "../../guest_fetch.h"

typedef struct insn {
    int len;
    int rexW, rexR, rexX, rexB, has_rex;
    int opsize;
    int p66;
    int addr32;
    int seg;
    int lock, rep, repne;
    int two;
    int map3;
    uint8_t op;
    int has_modrm;
    uint8_t modrm;
    int mod, reg, rm;
    int is_mem;
    int m_base, m_index, m_scale;
    int64_t disp;
    int rip_rel;
    int m_hasbase, m_hasindex;
    int rm_reg;
    int64_t imm;
    int imm_bytes;
    int vex, evex;
    int vex_map, vex_pp, vex_l, vex_w, vvvv;
    int evex_mask, evex_z, evex_b;
} hl_x86_insn;

enum { HL_X86_DECODE_MEMO_SLOTS = 1024, HL_X86_MAX_INSN = 15 };
typedef struct {
    uint64_t pc;
    hl_x86_insn instruction;
    uint8_t bytes[HL_X86_MAX_INSN];
    uint8_t length;
    /* Zero means invalid. The old validity byte plus its seven padding bytes carry this
       context-local authority epoch without increasing the 216-byte entry. */
    uint64_t authority_epoch;
} hl_x86_decode_memo_entry;
typedef int (*hl_x86_context_fetch_fn)(void *, uint64_t, void *, size_t);
typedef struct {
    hl_x86_decode_memo_entry memo[HL_X86_DECODE_MEMO_SLOTS];
    hl_guest_fetch_context fetch;
    hl_x86_context_fetch_fn fetch_fn;
    void *fetch_opaque;
    const _Atomic uint64_t *byte_unstable;
    const _Atomic uint64_t *logical_generation_source;
    const _Atomic uint64_t *direct_generation_source;
    uint64_t authority_logical_generation;
    uint64_t authority_direct_generation;
    uint64_t authority_epoch;
    uint8_t authority_state;
} hl_x86_hot_context;

hl_x86_hot_context *hl_x86_hot_context_create(hl_x86_context_fetch_fn fetch, void *opaque,
                                              const _Atomic uint64_t *byte_unstable);
void hl_x86_hot_context_destroy(hl_x86_hot_context *context);
int hl_x86_decode_context(hl_x86_hot_context *context, uint64_t pc, hl_x86_insn *insn);

int hl_x86_decode(uint64_t pc, hl_x86_insn *insn);
typedef int (*hl_x86_instruction_fetch_fn)(uint64_t, void *, size_t);
void hl_x86_decode_set_instruction_fetch(hl_x86_instruction_fetch_fn);
#if defined(HL_NATIVE_TEST_HOOKS)
int hl_x86_decode_memo_test(uint32_t scenario, uint64_t *decodes);
int hl_x86_hot_context_test(void);
int hl_x86_hot_context_thread_test(void);
int hl_x86_hot_context_allocation_test(void);
int hl_x86_decode_authority_test(uint32_t scenario, uint64_t *fetches);
#endif

#endif
