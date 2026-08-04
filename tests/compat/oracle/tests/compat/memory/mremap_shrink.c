// mremap shrink-in-place (no MAYMOVE): a 4-page mapping shrunk to 1 page keeps its base address and prefix
// bytes, and the released tail is truly unmapped (access faults). Then grow it back with MREMAP_MAYMOVE and
// confirm the surviving prefix byte is preserved.
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
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

    unsigned char *m = mmap(NULL, ps * 4, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) { printf("map fail\n"); return 2; }
    memset(m, 0x3c, ps * 4);

    void *r = mremap(m, ps * 4, ps, 0);
    int stayed = r == m;
    int prefix_ok = stayed && ((unsigned char *)r)[0] == 0x3c && ((unsigned char *)r)[ps - 1] == 0x3c;
    int tail_faults = touch((unsigned char *)m + ps) == 1;   // released page no longer mapped

    void *g = mremap(r, ps, ps * 4, MREMAP_MAYMOVE);
    int grew = g != MAP_FAILED;
    int survived = grew && ((unsigned char *)g)[0] == 0x3c;
    int tail_writable = grew && (((unsigned char *)g)[ps * 4 - 1] = 0x5a, ((unsigned char *)g)[ps * 4 - 1] == 0x5a);

    if (grew) munmap(g, ps * 4);
    printf("stayed=%d prefix_ok=%d tail_faults=%d grew=%d survived=%d tail_writable=%d\n", stayed, prefix_ok,
           tail_faults, grew, survived, tail_writable);
    return 0;
}
