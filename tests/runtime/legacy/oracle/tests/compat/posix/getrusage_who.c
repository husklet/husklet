// getrusage argument validation and monotonic-shape contract fixed by the Linux ABI:
//   - RUSAGE_SELF, RUSAGE_CHILDREN and RUSAGE_THREAD all succeed.
//   - an unknown `who` is EINVAL.
//   - a NULL usage pointer is EFAULT.
//   - the reported maxrss is non-negative and ru_nvcsw/ru_nivcsw are non-negative (monotonic invariants).
// No absolute counter VALUES are printed (those are host/timing dependent) -- only booleans and
// errnos -- so the golden is host-invariant. Arch-neutral.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/resource.h>

#ifndef RUSAGE_THREAD
#define RUSAGE_THREAD 1
#endif

int main(void) {
    struct rusage ru;
    printf("self_ok=%d children_ok=%d thread_ok=%d\n",
           getrusage(RUSAGE_SELF, &ru) == 0,
           getrusage(RUSAGE_CHILDREN, &ru) == 0,
           getrusage(RUSAGE_THREAD, &ru) == 0);
    // Unknown who -> EINVAL.
    errno = 0;
    printf("badwho_errno=%d\n", getrusage(12345, &ru) == -1 ? errno : 0);
    // NULL usage -> EFAULT.
    errno = 0;
    printf("null_errno=%d\n", getrusage(RUSAGE_SELF, NULL) == -1 ? errno : 0);
    // Monotonic shape invariants.
    getrusage(RUSAGE_SELF, &ru);
    printf("maxrss_nonneg=%d ctxsw_nonneg=%d\n",
           ru.ru_maxrss >= 0, ru.ru_nvcsw >= 0 && ru.ru_nivcsw >= 0);
    return 0;
}
