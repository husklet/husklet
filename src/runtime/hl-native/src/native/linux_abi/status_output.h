#ifndef HL_LINUX_ABI_STATUS_OUTPUT_H
#define HL_LINUX_ABI_STATUS_OUTPUT_H

#include "hl/linux_abi.h"

#include <string.h>

static inline int hl_linux_file_status_output_prepare(hl_linux_file_status *output) {
    if (output == NULL) return 0;
    memset(output, 0, sizeof(*output));
    return 1;
}

#endif
