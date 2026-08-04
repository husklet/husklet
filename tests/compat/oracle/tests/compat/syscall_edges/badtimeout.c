// syscall-compat regression: pselect6/ppoll must reject an out-of-range timeout nanoseconds field
// (tv_nsec < 0 or >= 1e9) with EINVAL, not treat it as a normal timeout. Raw syscalls so the engine's
// validation is exercised (glibc's select()/poll() wrappers would pre-validate).
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

static int ec(long nr, long a, long b, long c, long d, long e, long f) {
    long r = syscall(nr, a, b, c, d, e, f);
    return r == -1 ? errno : 0;
}

int main(void) {
    struct timespec hi = {0, 1000000000L}; // tv_nsec == 1e9 (invalid)
    struct timespec neg = {0, -1};         // tv_nsec < 0 (invalid)
    printf("pselect_hi=%d neg=%d\n", ec(SYS_pselect6, 0, 0, 0, 0, (long)&hi, 0),
           ec(SYS_pselect6, 0, 0, 0, 0, (long)&neg, 0));
    printf("ppoll_hi=%d neg=%d\n", ec(SYS_ppoll, 0, 0, (long)&hi, 0, 8, 0), ec(SYS_ppoll, 0, 0, (long)&neg, 0, 8, 0));
    return 0;
}
