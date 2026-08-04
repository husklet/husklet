// MAP_FIXED atomically replaces part of an existing mapping. A 2-page RW anon map has its first page
// overmapped by a fresh anonymous MAP_FIXED page: the overmapped page reads back zero (new mapping) while
// the untouched second page keeps its data. Then a PROT_READ MAP_FIXED overmap makes that page read-only.
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

static sigjmp_buf jump;
static volatile sig_atomic_t armed;
static void fault(int s) { (void)s; if (armed) siglongjmp(jump, 1); }
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

    unsigned char *m = mmap(NULL, ps * 2, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) { printf("map fail\n"); return 2; }
    m[0] = 0xf0; m[ps] = 0xf1;

    void *r = mmap(m, ps, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    int replaced = r == (void *)m;
    int page0_zero = replaced && m[0] == 0;             // fresh mapping, old byte gone
    int page1_keep = m[ps] == 0xf1;                     // neighbor untouched

    void *ro = mmap(m, ps, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    int ro_ok = ro == (void *)m;
    int ro_read_zero = ro_ok && m[0] == 0;
    int ro_write_faults = wr(m, 0x33) == 1;             // now read-only

    munmap(m, ps * 2);
    printf("replaced=%d page0_zero=%d page1_keep=%d ro_ok=%d ro_read_zero=%d ro_write_faults=%d\n", replaced,
           page0_zero, page1_keep, ro_ok, ro_read_zero, ro_write_faults);
    return 0;
}
