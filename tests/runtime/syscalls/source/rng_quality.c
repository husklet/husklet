// getrandom(2) security contract, exercised at the raw-syscall level (SYS_getrandom) so the engine's
// syscall-278 handler is what answers on both sides -- glibc's newer vDSO getrandom wrapper skips flag
// validation and userspace-faults on NULL, so the libc wrapper is not a faithful syscall oracle. Checks:
// a full-buffer draw returns the count; two draws differ (a duplicate/constant fill is a critical weak-key
// bug); gross-failure detection over 256 samples (not all-equal, no short repeating pattern, high distinct
// byte count, both high and low bytes); n=0 -> 0; a bad buffer -> EFAULT (no partial); an unknown flag ->
// EINVAL; GRND_RANDOM|GRND_INSECURE -> EINVAL; GRND_NONBLOCK never blocks. Plus AT_RANDOM (the glibc
// stack-canary/pointer-guard seed) is a non-zero 16-byte block. Only stable booleans printed so
// native == engine byte-for-byte.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef GRND_NONBLOCK
#define GRND_NONBLOCK 0x0001
#endif
#ifndef GRND_RANDOM
#define GRND_RANDOM 0x0002
#endif
#ifndef GRND_INSECURE
#define GRND_INSECURE 0x0004
#endif

static long grand(void *buf, size_t len, unsigned flags) {
    return syscall(SYS_getrandom, buf, len, flags);
}

int main(void) {
    unsigned char a[256] = {0}, b[256] = {0};

    // Full-buffer fill returns the requested count on both a plain and a GRND_NONBLOCK draw.
    long n1 = grand(a, sizeof a, 0);
    long n2 = grand(b, sizeof b, GRND_NONBLOCK);
    int filled = (n1 == 256) && (n2 == 256);

    // Two independent draws must differ (a duplicate/constant fill is a critical weak-key bug).
    int differ = memcmp(a, b, sizeof a) != 0;

    // Gross-failure detection over the 256 samples in a[].
    int all_equal = 1;
    for (size_t i = 1; i < sizeof a; i++)
        if (a[i] != a[0]) { all_equal = 0; break; }
    int period2 = 1; // repeating 2-byte pattern
    for (size_t i = 2; i < sizeof a; i++)
        if (a[i] != a[i - 2]) { period2 = 0; break; }
    int seen[256] = {0}, distinct = 0, has_hi = 0, has_lo = 0;
    for (size_t i = 0; i < sizeof a; i++) {
        if (!seen[a[i]]) { seen[a[i]] = 1; distinct++; }
        if (a[i] >= 0x80) has_hi = 1;
        else has_lo = 1;
    }
    // 256 uniform draws yield ~163 distinct values on average; a healthy CSPRNG clears 100 with
    // overwhelming probability, a gross failure (constant/sequential/tiny period) falls far below.
    int distinct_ok = distinct >= 100;

    // n == 0 returns 0 and touches nothing.
    int zero_ok = grand(a, 0, 0) == 0;

    // A bad destination pointer faults entirely (no partial fill), EFAULT.
    errno = 0;
    int efault = (grand((void *)0, 16, 0) == -1) && (errno == EFAULT);

    // Unknown flag bit -> EINVAL.
    errno = 0;
    int einval_flag = (grand(a, 16, 0x10) == -1) && (errno == EINVAL);

    // GRND_RANDOM|GRND_INSECURE is a defined-but-invalid combination -> EINVAL.
    errno = 0;
    int einval_combo = (grand(a, 16, GRND_RANDOM | GRND_INSECURE) == -1) && (errno == EINVAL);

    // AT_RANDOM: 16 startup-provided random bytes, non-zero.
    unsigned long atr = getauxval(AT_RANDOM);
    int atr_present = atr != 0;
    int atr_nonzero = 0;
    if (atr_present) {
        const unsigned char *r = (const unsigned char *)atr;
        for (int i = 0; i < 16; i++)
            if (r[i]) { atr_nonzero = 1; break; }
    }

    printf("filled=%d differ=%d all_equal=%d period2=%d distinct_ok=%d has_hi=%d has_lo=%d\n", filled, differ,
           all_equal, period2, distinct_ok, has_hi, has_lo);
    printf("zero=%d efault=%d einval_flag=%d einval_combo=%d\n", zero_ok, efault, einval_flag, einval_combo);
    printf("at_random_present=%d at_random_nonzero=%d\n", atr_present, atr_nonzero);
    return 0;
}
