#ifndef HL_TRANSLATOR_X86_64_DECODER_H
#define HL_TRANSLATOR_X86_64_DECODER_H

#include <stddef.h>
#include <stdint.h>

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

int hl_x86_decode(uint64_t pc, hl_x86_insn *insn);
typedef int (*hl_x86_instruction_fetch_fn)(uint64_t, void *, size_t);
void hl_x86_decode_set_instruction_fetch(hl_x86_instruction_fetch_fn);

#endif
