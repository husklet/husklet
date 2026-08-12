#ifndef HL_TRANSLATOR_GUEST_X86_64_REP_H
#define HL_TRANSLATOR_GUEST_X86_64_REP_H

#include <stdint.h>

struct cpu;

void hl_x86_rep_compare(struct cpu *cpu, uint64_t nonpie_low, uint64_t nonpie_high, uint64_t nonpie_bias);

#endif
