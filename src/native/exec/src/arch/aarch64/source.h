#ifndef HL_NATIVE_AARCH64_SOURCE_H
#define HL_NATIVE_AARCH64_SOURCE_H

#include "../../../include/executor.h"

#define HL_A64_SOURCE_MAX_SPANS 8u
#define HL_A64_SOURCE_MAX_WORDS 64u

typedef hl_native_source_span hl_a64_source_span;
typedef hl_native_source hl_a64_source;

typedef struct hl_a64_fetch_result {
    size_t count;
    uint64_t source_first;
    uint64_t source_last;
    uint64_t fault_pc;
    uint32_t words[HL_A64_SOURCE_MAX_WORDS];
} hl_a64_fetch_result;

int hl_a64_source_validate(const hl_a64_source *);
size_t hl_a64_source_available(const hl_a64_source *, uint64_t, size_t);
int hl_a64_source_fetch(const hl_a64_source *, uint64_t, size_t, hl_a64_fetch_result *);

#endif
