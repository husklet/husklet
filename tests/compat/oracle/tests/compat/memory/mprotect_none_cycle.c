// PROT_NONE reservation promoted through RW -> RO -> NONE on a private-anon page, asserting the exact
// access outcome at each protection and that a store made while RW survives the RO/NONE cycle. This is the
// private-anon protection-tracking path where the engine previously mishandled committed reservations.
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

static sigjmp_buf jump;
static volatile sig_atomic_t armed;
static void fault(int s) { (void)s; if (armed) siglongjmp(jump, 1); }
static int rd(volatile unsigned char *p, unsigned char *out) {
    armed = 1;
    if (sigsetjmp(jump, 1) == 0) { unsigned char v = *p; if (out) *out = v; armed = 0; return 0; }
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

    unsigned char *m = mmap(NULL, ps, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) { printf("map fail\n"); return 2; }
    int none_rd_fault = rd(m, NULL) == 1;
    int none_wr_fault = wr(m, 1) == 1;

    int to_rw = mprotect(m, ps, PROT_READ | PROT_WRITE) == 0;
    int store_ok = wr(m, 0x5e) == 0;
    unsigned char got = 0;
    int read_back = rd(m, &got) == 0 && got == 0x5e;

    int to_ro = mprotect(m, ps, PROT_READ) == 0;
    int ro_rd = rd(m, &got) == 0 && got == 0x5e;   // data preserved, still readable
    int ro_wr_fault = wr(m, 0x11) == 1;            // write now faults

    int to_none = mprotect(m, ps, PROT_NONE) == 0;
    int none_rd_fault2 = rd(m, NULL) == 1;

    int back_rw = mprotect(m, ps, PROT_READ | PROT_WRITE) == 0;
    int survived = rd(m, &got) == 0 && got == 0x5e; // store from RW phase survived the cycle

    munmap(m, ps);
    printf("none_rd_fault=%d none_wr_fault=%d to_rw=%d store_ok=%d read_back=%d to_ro=%d ro_rd=%d "
           "ro_wr_fault=%d to_none=%d none_rd_fault2=%d back_rw=%d survived=%d\n",
           none_rd_fault, none_wr_fault, to_rw, store_ok, read_back, to_ro, ro_rd, ro_wr_fault, to_none,
           none_rd_fault2, back_rw, survived);
    return 0;
}
