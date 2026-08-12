#ifndef HL_LINUX_ABI_RESERVATION_OUTPUT_H
#define HL_LINUX_ABI_RESERVATION_OUTPUT_H

#include "hl/linux_abi.h"

static inline int hl_linux_fd_reservation_output_prepare(hl_linux_fd_reservation *reservation) {
    if (reservation == NULL) return 0;
    *reservation = (hl_linux_fd_reservation){HL_LINUX_FD_LIMIT, 0};
    return 1;
}

#endif
