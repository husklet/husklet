// Unmapping the MIDDLE page of a 3-page mapping splits it into two live mappings with a hole between. The
// edges stay accessible, the hole faults, and a fresh MAP_FIXED mapping can be dropped back into the hole.
// Exercises interval-tree split/hole/refill bookkeeping.
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

static sigjmp_buf jump;
static volatile sig_atomic_t armed;
static void fault(int s) { (void)s; if (armed) siglongjmp(jump, 1); }
static int touch(volatile unsigned char *p) {
    armed = 1;
    if (sigsetjmp(jump, 1) == 0) { volatile unsigned char v = *p; (void)v; armed = 0; return 0; }
    armed = 0; return 1;
}

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    struct sigaction sa = {0};
    sa.sa_handler = fault; sigemptyset(&sa.sa_mask);
    sigaction(SIGSEGV, &sa, NULL); sigaction(SIGBUS, &sa, NULL);

    unsigned char *m = mmap(NULL, ps * 3, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) { printf("map fail\n"); return 2; }
    m[0] = 0xa0; m[ps] = 0xa1; m[2 * ps] = 0xa2;

    int unmapped = munmap(m + ps, ps) == 0;
    int edge0_ok = touch(m) == 0 && m[0] == 0xa0;
    int hole_faults = touch(m + ps) == 1;
    int edge2_ok = touch(m + 2 * ps) == 0 && m[2 * ps] == 0xa2;

    void *r = mmap(m + ps, ps, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    int refilled = r == (void *)(m + ps);
    int hole_zero_now = refilled && m[ps] == 0 && (m[ps] = 0x55, m[ps] == 0x55);

    munmap(m, ps); munmap(m + 2 * ps, ps);
    if (refilled) munmap(m + ps, ps);
    printf("unmapped=%d edge0_ok=%d hole_faults=%d edge2_ok=%d refilled=%d hole_zero_now=%d\n", unmapped,
           edge0_ok, hole_faults, edge2_ok, refilled, hole_zero_now);
    return 0;
}
