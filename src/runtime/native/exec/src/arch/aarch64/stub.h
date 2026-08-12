#ifndef HL_NATIVE_AARCH64_STUB_H
#define HL_NATIVE_AARCH64_STUB_H

#include "../../../include/executor.h"
#include "assembler.h"

#define HL_A64_STUB_MAX_BYTES 416u
#define HL_A64_EDGE_SPAN_WORDS 16u

typedef struct hl_a64_budget_guard {
  uint32_t *interrupt_branch;
  uint32_t *token_skip_branch;
  uint32_t *token_interrupt_branch;
  uint32_t *budget_branch;
  uint32_t *subtract;
  uint64_t pc;
} hl_a64_budget_guard;

int hl_a64_stub_emit(hl_a64_assembler *, uint32_t, uint64_t);
void hl_a64_stub_prologue(hl_a64_assembler *);
void hl_a64_stub_exit(hl_a64_assembler *, uint32_t, uint64_t);
uint32_t *hl_a64_stub_edge_reserve(hl_a64_assembler *);
void hl_a64_stub_exit_register(hl_a64_assembler *, uint32_t, int);
void hl_a64_stub_budget_begin(hl_a64_assembler *, uint64_t,
                              hl_a64_budget_guard *);
void hl_a64_stub_budget_finish(hl_a64_assembler *, hl_a64_budget_guard *,
                               uint32_t);
void hl_a64_stub_publish_execution_identity(hl_a64_assembler *);

#endif
