#ifndef HL_NATIVE_AARCH64_GUARD_H
#define HL_NATIVE_AARCH64_GUARD_H

#include "assembler.h"

/* Conservative complete standalone emission bound for a guarded memory
 * operation, including write journaling and the deferred cold miss path. */
#define HL_A64_GUARDED_MAX_BYTES 4096u

typedef struct hl_a64_guard {
    uint32_t *below;
    uint32_t *overflow;
    uint32_t *above;
    uint32_t *permission;
    uint8_t *resume;
    uint32_t required;
    uint64_t bytes;
    uint64_t pc;
    uint32_t completed;
    /* The one outlined archive routine this guarded access calls, or NULL
     * before the first call site emits it. */
    uint8_t *archive;
} hl_a64_guard;

typedef enum hl_a64_guard_mode {
    HL_A64_GUARD_LEGACY = 0,
    /* The trace-entry authenticator must set certificate_valid/delta and the
     * exact memory_{first,last,permissions} owner fields as one contract. */
    HL_A64_GUARD_AUTHENTICATED_MEMBER = 1,
} hl_a64_guard_mode;

void hl_a64_guard_begin(hl_a64_assembler *, uint64_t, uint32_t, hl_a64_guard *);
void hl_a64_guard_begin_mode(hl_a64_assembler *, uint64_t, uint32_t,
                             hl_a64_guard_mode, hl_a64_guard *);
void hl_a64_guard_direct_begin(hl_a64_assembler *, uint64_t, uint32_t, hl_a64_guard *);
void hl_a64_guard_finish(hl_a64_assembler *, const hl_a64_guard *);
void hl_a64_guard_write_begin(hl_a64_assembler *, uint64_t, uint64_t, hl_a64_guard *);
void hl_a64_guard_written(hl_a64_assembler *, uint64_t);

#endif
