#ifndef HL_LINUX_ABI_DESCRIPTOR_OUTPUT_H
#define HL_LINUX_ABI_DESCRIPTOR_OUTPUT_H

#include "hl/linux_abi.h"

static inline int hl_linux_fd_output_prepare(hl_linux_fd *out_fd) {
    if (out_fd == NULL) return 0;
    *out_fd = HL_LINUX_FD_LIMIT;
    return 1;
}

#endif
