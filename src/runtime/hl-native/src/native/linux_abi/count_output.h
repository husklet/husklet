#ifndef HL_LINUX_ABI_COUNT_OUTPUT_H
#define HL_LINUX_ABI_COUNT_OUTPUT_H

#include <stddef.h>
#include <stdint.h>

static inline int hl_linux_count_output_prepare(uint32_t *output) {
    if (output == NULL) return 0;
    *output = 0;
    return 1;
}

#endif
