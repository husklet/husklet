#ifndef HL_TRANSLATOR_GUEST_X86_64_LOWER_CORE_H
#define HL_TRANSLATOR_GUEST_X86_64_LOWER_CORE_H

#include "trace.h"

int *hl_x86_integer_pending_flags(void);
void hl_x86_integer_reset_flags(void);
int hl_x86_integer_lazy_flags(void);
void hl_x86_integer_prepare_flags(const struct insn *instruction, uint64_t guest_pc, uint64_t next,
                                  const hl_x86_trace_state *trace_state);

#endif
