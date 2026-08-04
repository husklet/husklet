// sched_setaffinity error-path contract: the kernel validates the mask pointer, the target pid, and the
// resulting (online-intersected) mask in a fixed order. A bad mask pointer is -EFAULT (not a crash), a
// zero-length mask clears to empty and is -EINVAL, a pid naming no live task is -ESRCH, and an all-zero
// mask is -EINVAL. Deterministic across CPU counts.
#define _GNU_SOURCE
#include <sched.h>
#include <stdio.h>
#include <errno.h>
#include <sys/syscall.h>
#include <unistd.h>

static int call(long pid, size_t len, const void *mask) {
    errno = 0;
    long r = syscall(SYS_sched_setaffinity, pid, len, mask);
    return r < 0 ? errno : 0;
}

int main(void) {
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(0, &set);

    int badptr = call(0, sizeof(cpu_set_t), (const void *)0x1);       // unmapped -> EFAULT
    int badptr_len0 = call(0, 0, (const void *)0x1);                  // len 0, no copy -> empty -> EINVAL
    int badpid = call(0x7ffffff0, sizeof set, &set);                  // no such task -> ESRCH
    cpu_set_t empty;
    CPU_ZERO(&empty);
    int emptymask = call(0, sizeof empty, &empty);                    // selects no online cpu -> EINVAL

    printf("badptr=%d len0=%d badpid=%d empty=%d\n",
           badptr == EFAULT, badptr_len0 == EINVAL, badpid == ESRCH, emptymask == EINVAL);
    return 0;
}
