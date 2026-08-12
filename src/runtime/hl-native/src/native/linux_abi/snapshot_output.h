#ifndef HL_LINUX_ABI_SNAPSHOT_OUTPUT_H
#define HL_LINUX_ABI_SNAPSHOT_OUTPUT_H

#include "hl/linux_abi.h"

static inline int hl_linux_fd_snapshot_output_prepare(hl_linux_fd_snapshot *snapshot) {
    if (snapshot == NULL) return 0;
    *snapshot = (hl_linux_fd_snapshot){.fd = HL_LINUX_FD_LIMIT, .host_handle = HL_HOST_HANDLE_INVALID};
    return 1;
}

#endif
