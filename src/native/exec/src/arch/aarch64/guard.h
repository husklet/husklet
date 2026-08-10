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
    /* Fixed-aperture guards use below as their page-cross or unconditional
     * rejection branch and never reinterpret the aperture as permissions. */
    uint32_t aperture;
} hl_a64_guard;

void hl_a64_guard_begin(hl_a64_assembler *, uint64_t, uint32_t, hl_a64_guard *);
void hl_a64_guard_direct_begin(hl_a64_assembler *, uint64_t, uint32_t, hl_a64_guard *);
void hl_a64_guard_aperture_attempt(hl_a64_assembler *);
void hl_a64_guard_aperture_begin(hl_a64_assembler *, uint64_t, uint32_t, hl_a64_guard *);
void hl_a64_guard_aperture_reject(hl_a64_assembler *, uint64_t, uint32_t, hl_a64_guard *);
void hl_a64_guard_aperture_hit(hl_a64_assembler *);
void hl_a64_guard_finish(hl_a64_assembler *, const hl_a64_guard *);
void hl_a64_guard_write_begin(hl_a64_assembler *, uint64_t, uint64_t, hl_a64_guard *);
void hl_a64_guard_written(hl_a64_assembler *, uint64_t);

#endif
