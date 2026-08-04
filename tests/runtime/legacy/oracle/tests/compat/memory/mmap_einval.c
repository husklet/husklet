// mmap/munmap/mprotect argument validation: zero length, unaligned offset, unaligned or
// unaligned-length MAP_FIXED, an unmapped-region munmap, and a bad fd for MAP_SHARED.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    void *a = mmap(NULL, 0, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    int ea = (a == MAP_FAILED) ? errno : 0;
    void *b = mmap(NULL, ps, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 1);
    int eb = (b == MAP_FAILED) ? errno : 0;
    void *c = mmap((void *)0x1001, ps, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    int ec = (c == MAP_FAILED) ? errno : 0;
    void *d = mmap(NULL, ps, PROT_READ, MAP_SHARED, -1, 0);
    int ed = (d == MAP_FAILED) ? errno : 0;
    void *e = mmap(NULL, ps, PROT_READ, 0, -1, 0); // neither SHARED nor PRIVATE
    int ee = (e == MAP_FAILED) ? errno : 0;

    char *ok = mmap(NULL, ps * 2, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    int m0 = munmap(ok, 0);
    int em0 = (m0 == -1) ? errno : 0;
    int m1 = munmap(ok + 1, ps);
    int em1 = (m1 == -1) ? errno : 0;
    int m2 = munmap(ok, ps * 2);
    int m3 = munmap(ok, ps * 2); // already gone: still succeeds on Linux
    int p0 = mprotect((void *)((unsigned long)ok + 1), ps, PROT_READ);
    int ep0 = (p0 == -1) ? errno : 0;
    char *valid = mmap(NULL, ps, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    int pi = mprotect(valid, ps, PROT_READ | 0x10000000);
    int epi = pi == -1 ? errno : 0;
    valid[0] = 0x5a;
    int preserved = valid[0] == 0x5a;
    printf("ea=%d eb=%d ec=%d ed=%d ee=%d m0=%d em0=%d m1=%d em1=%d m2=%d m3=%d p0=%d ep0=%d pi=%d epi=%d preserved=%d\n",
           ea, eb, ec, ed, ee, m0, em0, m1, em1, m2, m3, p0, ep0, pi, epi,
           preserved);
    return 0;
}
