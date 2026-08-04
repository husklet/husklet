// Zero-length and boundary return codes for the mm family. Linux treats length 0 as a no-op success for
// mprotect/madvise/msync/mlock/munlock but as EINVAL for munmap. Each verdict is an errno-name.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

static const char *en(int e) {
    switch (e) {
    case 0: return "0";
    case EINVAL: return "EINVAL";
    case ENOMEM: return "ENOMEM";
    default: return "OTHER";
    }
}
static const char *rc(int r) { return r == 0 ? "0" : en(errno); }

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    unsigned char *m = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) { printf("map fail\n"); return 2; }

    errno = 0; const char *mp = rc(mprotect(m, 0, PROT_READ));
    errno = 0; const char *ma = rc(madvise(m, 0, MADV_DONTNEED));
    errno = 0; const char *ms = rc(msync(m, 0, MS_ASYNC));
    errno = 0; const char *ml = rc(mlock(m, 0));
    errno = 0; const char *mu = rc(munlock(m, 0));
    errno = 0; int ur = munmap(m, 0); const char *un = ur == 0 ? "0" : en(errno);

    munmap(m, ps);
    printf("mprotect0=%s madvise0=%s msync0=%s mlock0=%s munlock0=%s munmap0=%s\n", mp, ma, ms, ml, mu, un);
    return 0;
}
