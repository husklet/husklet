// mremap(MREMAP_MAYMOVE|MREMAP_DONTUNMAP): the mapping is duplicated to a new address while the OLD range
// stays mapped as fresh zero-filled anonymous memory. Verifies the new copy carries the data and the old
// range is readable-but-rezeroed (Linux DONTUNMAP contract). Requires a movable private-anon source.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#ifndef MREMAP_DONTUNMAP
#define MREMAP_DONTUNMAP 4
#endif

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    unsigned char *m = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) { printf("map fail\n"); return 2; }
    memset(m, 0x8b, ps);

    void *r = mremap(m, ps, ps, MREMAP_MAYMOVE | MREMAP_DONTUNMAP);
    int moved = r != MAP_FAILED && r != m;
    int newdata = moved && ((unsigned char *)r)[0] == 0x8b && ((unsigned char *)r)[ps - 1] == 0x8b;
    int old_rezeroed = m[0] == 0 && m[ps - 1] == 0;    // old range now anonymous zero-fill
    int old_writable = (m[0] = 0x22, m[0] == 0x22);

    if (moved) munmap(r, ps);
    munmap(m, ps);
    printf("moved=%d newdata=%d old_rezeroed=%d old_writable=%d\n", moved, newdata, old_rezeroed, old_writable);
    return 0;
}
