#ifndef HL_LINUX_ABI_PROCESS_OUTPUT_H
#define HL_LINUX_ABI_PROCESS_OUTPUT_H

#include "hl/linux_abi.h"

static inline int hl_linux_process_output_prepare(hl_host_handle *out_process) {
    if (out_process == NULL) return 0;
    *out_process = HL_HOST_HANDLE_INVALID;
    return 1;
}

#endif
