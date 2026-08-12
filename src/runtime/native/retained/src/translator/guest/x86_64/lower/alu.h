#ifndef HL_TRANSLATOR_GUEST_X86_64_LOWER_ALU_H
#define HL_TRANSLATOR_GUEST_X86_64_LOWER_ALU_H

#include <stdint.h>

struct insn;

int hl_x86_lower_alu(struct insn *insn, uint64_t next);

#endif
