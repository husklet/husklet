// syscall-compat regression: mlock/mlockall must honor the guest's RLIMIT_MEMLOCK (the container runs
// unprivileged -- no CAP_IPC_LOCK), not fake-succeed. Linux mm/mlock.c: a soft limit of 0 refuses any lock
// (can_do_mlock -> EPERM); locking more than the soft limit -> ENOMEM; locking within it -> success.
// Page-size neutral: all amounts are multiples of the runtime page size, so the errno outcomes are identical
// whether the page is 4K or 16K. Oracle-diffed vs a native (unprivileged) aarch64 run.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <unistd.h>

int main(void) {
    long pg = sysconf(_SC_PAGESIZE);
    size_t two = (size_t)pg * 2, four = (size_t)pg * 4;
    char *m = mmap(0, four, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) {
        printf("mmap_failed\n");
        return 1;
    }

    // Soft limit = 2 pages (leave the hard limit untouched so an unprivileged setrlimit never EPERMs).
    struct rlimit rl;
    getrlimit(RLIMIT_MEMLOCK, &rl);
    rl.rlim_cur = two;
    setrlimit(RLIMIT_MEMLOCK, &rl);

    // Within the limit: locking exactly 2 pages succeeds.
    int e_ok = mlock(m, two) == 0 ? 0 : errno;
    // Over the limit: the 2 already-locked pages + 2 more = 4 pages > the 2-page limit -> ENOMEM (12).
    int e_big = mlock(m, four) == 0 ? 0 : errno;
    munlock(m, four);

    // Soft limit 0 -> can_do_mlock() fails: any mlock is refused with EPERM (1).
    getrlimit(RLIMIT_MEMLOCK, &rl);
    rl.rlim_cur = 0;
    setrlimit(RLIMIT_MEMLOCK, &rl);
    int e_zero = mlock(m, (size_t)pg) == 0 ? 0 : errno;

    printf("ok=%d big=%d zero=%d\n", e_ok, e_big, e_zero);
    return 0;
}
