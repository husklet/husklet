#ifndef HL_NATIVE_AARCH64_ATOMIC_H
#define HL_NATIVE_AARCH64_ATOMIC_H

#include "assembler.h"
#include "guard.h"
#include "memory.h"

#define HL_A64_ATOMIC_MAX_BYTES HL_A64_GUARDED_MAX_BYTES

/* Nonzero when the host implements FEAT_LSE, so a decoded single-instruction
 * LSE atomic may be re-emitted natively rather than interpreted. */
int hl_a64_atomic_host_supports(void);

/* Precise GPR results of an accepted LSE word, or zero when unsupported. */
int hl_a64_atomic_definitions(uint32_t, uint32_t *);

int hl_a64_atomic_emit(hl_a64_assembler *, uint32_t, uint64_t);
int hl_a64_atomic_body(hl_a64_assembler *, uint32_t, uint64_t, hl_a64_guard *,
                       hl_a64_memory_sites *);

#endif
