#ifndef HL_TRANSLATOR_GUEST_X86_64_LOWER_LEGACY_H
#define HL_TRANSLATOR_GUEST_X86_64_LOWER_LEGACY_H

#include <stdint.h>
#include "../decoder.h"
#include "crypto.h"
#include "repstr.h"
#include "trace.h"

enum hl_x86_direction hl_x86_legacy_direction(void);
void hl_x86_legacy_direction_set(enum hl_x86_direction);

int lower_primary_fast(struct insn *, uint64_t, uint64_t, const hl_x86_trace_state *);
int lower_primary_string(struct insn *, uint64_t, hl_x86_crypto_state *);
int lower_group3_unary(struct insn *, uint64_t);
int lower_group3_narrow_muldiv(struct insn *, uint64_t, uint64_t);
int lower_group3_wide_muldiv(struct insn *, uint64_t, uint64_t);
int lower_group45(struct insn *, uint64_t, uint64_t);
int lower_exchange(struct insn *, uint64_t, uint64_t);
int lower_stack_control(struct insn *, uint64_t, uint64_t);
int lower_immediate_multiply(struct insn *, uint64_t, uint64_t, const hl_x86_trace_state *);
int lower_direct_call_loop(struct insn *, uint64_t, uint64_t, hl_x86_trace_state *);
int lower_flag_register_transfer(struct insn *);
int lower_flag_stack_control(struct insn *, uint64_t);
int lower_accumulator_legacy(struct insn *, int);
int lower_bit_scan(struct insn *, uint64_t, int);
int lower_population_count(struct insn *, uint64_t, int);
int lower_compare_exchange(struct insn *, uint64_t, uint64_t);
int lower_exchange_add(struct insn *, uint64_t, uint64_t);
int lower_wide_compare_exchange(struct insn *, uint64_t, uint64_t);
int lower_system_query(struct insn *, uint64_t);
int lower_bit_test_modify(struct insn *, uint64_t, uint64_t, int);
int lower_extended_state(struct insn *, uint64_t, uint64_t);
int lower_multibyte_hint(const struct insn *);

#endif
