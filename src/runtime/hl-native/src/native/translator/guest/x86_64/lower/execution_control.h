#ifndef HL_TRANSLATOR_GUEST_X86_64_LOWER_EXECUTION_CONTROL_H
#define HL_TRANSLATOR_GUEST_X86_64_LOWER_EXECUTION_CONTROL_H

#include <stdint.h>

#include "../decoder.h"

void emit_ldmxcsr(struct insn *, uint64_t);
void emit_stmxcsr(struct insn *, uint64_t);
void emit_div_zero_check(int, uint64_t, int);
void emit_div_ovf_check(int, int, int, int, uint64_t, int);
void emit_div64_fast(uint64_t, uint64_t, int, int);

#endif
