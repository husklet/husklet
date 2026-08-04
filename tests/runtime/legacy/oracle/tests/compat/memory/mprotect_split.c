// Partial-range mprotect that SPLITS a private-anon mapping: a 4-page RW map with only the middle two
// pages demoted to PROT_NONE. Verifies the engine tracks per-page protection across the split (edges stay
// writable, the interior faults) and that re-promoting the whole range restores access with data intact.
// Output is derived booleans/counts only, identical on any page size.
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

static sigjmp_buf jump;
static volatile sig_atomic_t armed;
static void fault(int s) { (void)s; if (armed) siglongjmp(jump, 1); }

static int rd(volatile unsigned char *p) {
    armed = 1;
    if (sigsetjmp(jump, 1) == 0) { volatile unsigned char v = *p; (void)v; armed = 0; return 0; }
    armed = 0; return 1;
}
static int wr(volatile unsigned char *p, unsigned char v) {
    armed = 1;
    if (sigsetjmp(jump, 1) == 0) { *p = v; armed = 0; return 0; }
    armed = 0; return 1;
}

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    struct sigaction sa = {0};
    sa.sa_handler = fault; sigemptyset(&sa.sa_mask);
    sigaction(SIGSEGV, &sa, NULL); sigaction(SIGBUS, &sa, NULL);

    unsigned char *m = mmap(NULL, ps * 4, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) { printf("map fail\n"); return 2; }
    for (int i = 0; i < 4; i++) m[i * ps] = (unsigned char)(0xa0 + i);

    int split = mprotect(m + ps, ps * 2, PROT_NONE) == 0;
    int edge0_wr = wr(m + 0 * ps, 0xb0) == 0;     // page 0 still writable
    int mid1_fault = rd(m + 1 * ps) == 1;          // page 1 faults
    int mid2_fault = wr(m + 2 * ps, 0xcc) == 1;    // page 2 faults on write
    int edge3_wr = wr(m + 3 * ps, 0xb3) == 0;      // page 3 still writable

    int restore = mprotect(m + ps, ps * 2, PROT_READ | PROT_WRITE) == 0;
    // Interior pages read back the zero-preserved originals (never written since they faulted).
    int mid_ok = m[1 * ps] == 0xa1 && m[2 * ps] == 0xa2;
    int edge_ok = m[0] == 0xb0 && m[3 * ps] == 0xb3;

    munmap(m, ps * 4);
    printf("split=%d edge0_wr=%d mid1_fault=%d mid2_fault=%d edge3_wr=%d restore=%d mid_ok=%d edge_ok=%d\n",
           split, edge0_wr, mid1_fault, mid2_fault, edge3_wr, restore, mid_ok, edge_ok);
    return 0;
}
