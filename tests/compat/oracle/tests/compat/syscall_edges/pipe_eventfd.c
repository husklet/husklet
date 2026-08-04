// syscall-compat regression: eventfd2 flag validation and eventfd counter semantics. eventfd2 with an
// undefined flag -> EINVAL; a semaphore-mode eventfd decrements by
// one per read; a non-semaphore eventfd read drains the whole counter; writing the reserved 0xffffffffffffffff
// -> EINVAL. Arch-neutral: errnos/values printed, deterministic.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/eventfd.h>
#include <unistd.h>

int main(void) {
    printf("eventfd2_badflag_errno=%d\n", eventfd(0, 0x4) == -1 ? errno : 0);

    // Semaphore mode: seed 3, each read returns 1.
    int sem = eventfd(3, EFD_SEMAPHORE);
    uint64_t v = 0;
    read(sem, &v, sizeof(v));
    uint64_t v2 = 0;
    read(sem, &v2, sizeof(v2));
    printf("sem_read1=%llu read2=%llu\n", (unsigned long long)v, (unsigned long long)v2);

    // Non-semaphore mode: seed 5, one read drains the full count.
    int ev = eventfd(5, 0);
    uint64_t d = 0;
    read(ev, &d, sizeof(d));
    printf("drain=%llu\n", (unsigned long long)d);

    // Writing the reserved max value -> EINVAL.
    uint64_t max = 0xffffffffffffffffULL;
    printf("write_max_errno=%d\n", write(ev, &max, sizeof(max)) == -1 ? errno : 0);

    // A short (4-byte) write on an eventfd -> EINVAL.
    uint32_t small = 1;
    printf("short_write_errno=%d\n", write(ev, &small, sizeof(small)) == -1 ? errno : 0);
    return 0;
}
