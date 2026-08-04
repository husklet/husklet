// sched_get_priority_max/min report a FIXED Linux ABI contract, not a host value:
// SCHED_FIFO and SCHED_RR span [1,99]; SCHED_OTHER/SCHED_BATCH/SCHED_IDLE are all 0/0;
// an unknown policy is EINVAL. These are kernel constants the engine must emulate identically
// on every host/backend, so only fixed numbers and errnos are printed. Arch-neutral.
#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <stdio.h>

int main(void) {
    printf("fifo_max=%d fifo_min=%d\n", sched_get_priority_max(SCHED_FIFO), sched_get_priority_min(SCHED_FIFO));
    printf("rr_max=%d rr_min=%d\n", sched_get_priority_max(SCHED_RR), sched_get_priority_min(SCHED_RR));
    printf("other_max=%d other_min=%d\n", sched_get_priority_max(SCHED_OTHER), sched_get_priority_min(SCHED_OTHER));
    printf("batch_max=%d batch_min=%d\n", sched_get_priority_max(SCHED_BATCH), sched_get_priority_min(SCHED_BATCH));
    printf("idle_max=%d idle_min=%d\n", sched_get_priority_max(SCHED_IDLE), sched_get_priority_min(SCHED_IDLE));
    // Unknown policy -> EINVAL(22) on both queries.
    printf("badpol_max_errno=%d badpol_min_errno=%d\n",
           sched_get_priority_max(0x1234) == -1 ? errno : 0,
           sched_get_priority_min(0x1234) == -1 ? errno : 0);
    return 0;
}
