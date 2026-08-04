// futex(2) wait error/return classes, all single-threaded and deterministic:
//   - FUTEX_WAIT with a mismatched expected value returns immediately with EAGAIN.
//   - FUTEX_WAIT with a relative timeout on a matching value returns ETIMEDOUT.
//   - FUTEX_WAIT_BITSET with an absolute (CLOCK_MONOTONIC) deadline in the past also times out.
//   - FUTEX_WAKE on an address with no waiters returns 0.
#define _GNU_SOURCE
#include <errno.h>
#include <linux/futex.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

static int word = 100;

static long futex(int *a, int op, int val, const struct timespec *ts, int *a2, int val3) {
    return syscall(SYS_futex, a, op, val, ts, a2, val3);
}

int main(void) {
    // 1. EAGAIN: expected value (999) != actual (100)
    errno = 0;
    long r1 = futex(&word, FUTEX_WAIT, 999, NULL, NULL, 0);
    int eagain = r1 == -1 && errno == EAGAIN;

    // 2. ETIMEDOUT: value matches (100) but a short relative timeout elapses
    struct timespec rel = {0, 30 * 1000000};
    errno = 0;
    long r2 = futex(&word, FUTEX_WAIT, 100, &rel, NULL, 0);
    int etimedout = r2 == -1 && errno == ETIMEDOUT;

    // 3. FUTEX_WAIT_BITSET with an absolute deadline already in the past -> ETIMEDOUT
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    struct timespec past = { now.tv_sec - 1, now.tv_nsec };
    errno = 0;
    // FUTEX_WAIT_BITSET's timeout is an ABSOLUTE CLOCK_MONOTONIC deadline by default.
    long r3 = futex(&word, FUTEX_WAIT_BITSET, 100, &past, NULL, FUTEX_BITSET_MATCH_ANY);
    int bitset_timeout = r3 == -1 && errno == ETIMEDOUT;

    // 4. WAKE with no waiters -> 0
    long r4 = futex(&word, FUTEX_WAKE, 1, NULL, NULL, 0);
    int wake_zero = r4 == 0;

    printf("futex_errors eagain=%d etimedout=%d bitset_timeout=%d wake_zero=%d\n",
           eagain, etimedout, bitset_timeout, wake_zero);
    return 0;
}
