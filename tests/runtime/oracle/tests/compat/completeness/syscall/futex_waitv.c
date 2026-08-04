/* futex_waitv (multi-futex wait, syscall 449) argument validation. Without doing a real wait we probe
   the error paths that any conformant implementation must produce: a zero-count call is EINVAL, an
   over-large count is EINVAL, and a bogus flags word is EINVAL — or the whole syscall is ENOSYS where
   unimplemented. A correct engine returns those canonical errnos, never a crash or EFAULT. Derived
   boolean verdict, arch-neutral. */
#include "compat.h"
#include <stdio.h>
#ifndef __NR_futex_waitv
#define __NR_futex_waitv 449
#endif

static int rejected(long rc) {
    return (rc < 0) && (errno == EINVAL || errno == ENOSYS || errno == E2BIG);
}

int main(void) {
    long zero = syscall(__NR_futex_waitv, (void *)0, 0u, 0u, (void *)0, 0u);
    long huge = syscall(__NR_futex_waitv, (void *)0, 1000000u, 0u, (void *)0, 0u);
    long badf = syscall(__NR_futex_waitv, (void *)0, 1u, 0xFFFFFFFFu, (void *)0, 0u);

    printf("futex_waitv zero_rejected=%d huge_rejected=%d badflags_rejected=%d\n",
           rejected(zero), rejected(huge), rejected(badf));
    return 0;
}
