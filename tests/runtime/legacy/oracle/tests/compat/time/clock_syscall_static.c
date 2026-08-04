#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

/* Keep the destination in static .bss. This is the LTP harness shape that
 * differs from libc clock_gettime using a stack-local result. */
static struct timespec result;

int main(void) {
    errno = 0;
    long status = syscall(SYS_clock_gettime, CLOCK_MONOTONIC, &result);
    printf("clock_syscall_static status=%ld errno=%d valid=%d\n", status, errno,
           status == 0 && result.tv_sec >= 0 && result.tv_nsec >= 0 && result.tv_nsec < 1000000000L);
    return status == 0 && result.tv_sec >= 0 && result.tv_nsec >= 0 && result.tv_nsec < 1000000000L ? 0 : 1;
}
