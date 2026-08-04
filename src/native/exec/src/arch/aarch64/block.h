#ifndef HL_NATIVE_AARCH64_BLOCK_H
#define HL_NATIVE_AARCH64_BLOCK_H

#include "source.h"
#include "../../translation.h"

#define HL_A64_BLOCK_MAX_BYTES 900u

typedef enum hl_a64_block_state {
    HL_A64_BLOCK_BUILT,
    HL_A64_BLOCK_HIT,
    HL_A64_BLOCK_FALLBACK,
    HL_A64_BLOCK_FETCH,
} hl_a64_block_state;

typedef struct hl_a64_block_result {
    hl_a64_block_state state;
    uint64_t source_first;
    uint64_t source_last;
    uint64_t code_size;
    hl_native_provenance provenance;
} hl_a64_block_result;

int hl_a64_block_build(const hl_a64_source *, uint64_t, void *, size_t, hl_a64_block_result *);
hl_native_status hl_a64_block_cache(hl_native_executor *, const hl_a64_source *, uint64_t,
                                    void *, size_t, hl_native_code *, hl_a64_block_state *);

#endif
