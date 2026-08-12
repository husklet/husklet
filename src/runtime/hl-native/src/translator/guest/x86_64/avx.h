#ifndef HL_TRANSLATOR_GUEST_X86_64_AVX_H
#define HL_TRANSLATOR_GUEST_X86_64_AVX_H

#include <stddef.h>
#include <stdint.h>

struct cpu;

typedef struct hl_x86_avx_state {
    const uint64_t *nonpie_low;
    const uint64_t *nonpie_high;
    const uint64_t *nonpie_bias;
    /* Return 1 when a logical mapping handled the transfer, 0 to use the
       ordinary identity pointer, and -1 for an inaccessible logical span. */
    int (*memory_read)(uint64_t guest, void *destination, size_t length);
    int (*memory_write)(uint64_t guest, const void *source, size_t length);
} hl_x86_avx_state;

uint64_t hl_x86_avx_address(const hl_x86_avx_state *state, uint64_t address);
void hl_x86_avx_run(const hl_x86_avx_state *state, struct cpu *cpu);
void hl_x86_sse_run(const hl_x86_avx_state *state, struct cpu *cpu);
void hl_x86_avx_dump(void);

#endif
