#ifndef HL_TRANSLATOR_GUEST_X86_64_LOWER_X87_DECODE_H
#define HL_TRANSLATOR_GUEST_X86_64_LOWER_X87_DECODE_H

#include <stdint.h>

#include "../decoder.h"

typedef void (*hl_x86_x87_unimplemented_fn)(uint64_t guest_pc, hl_x86_insn *instruction);

int hl_x86_lower_x87(hl_x86_insn *instruction, uint64_t guest_pc, uint64_t next,
                     hl_x86_x87_unimplemented_fn unimplemented);

#endif
