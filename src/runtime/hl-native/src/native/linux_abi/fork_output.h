#ifndef HL_LINUX_ABI_FORK_OUTPUT_H
#define HL_LINUX_ABI_FORK_OUTPUT_H

#include "hl/linux_abi.h"

static inline int hl_linux_fork_plan_output_prepare(hl_linux_fork_plan *plan) {
    if (plan == NULL || plan->abi != HL_LINUX_ABI_VERSION || plan->size < sizeof(*plan)) return 0;
    plan->count = 0;
    plan->armed = 0;
    plan->host_completed = 0;
    return 1;
}

#endif
