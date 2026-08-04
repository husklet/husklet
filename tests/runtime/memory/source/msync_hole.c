// msync(2) validates its flags and then the address range: a MAP_SHARED store is page-cache coherent so a
// plain msync succeeds, an invalid flag word is EINVAL, and a range containing an unmapped hole is ENOMEM
// (msync of a stale/never-mapped range must not read as a fake success). Page-size neutral.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    size_t len = (size_t)ps * 2;

    void *m = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) return 1;
    ((volatile unsigned char *)m)[0] = 1;

    int sync_ok = msync(m, len, MS_SYNC) == 0;

    errno = 0;
    int bad_flag = msync(m, len, 0x100) == -1 && errno == EINVAL; // undefined MS_* bit

    // Unmap the range, then msync it: the hole must be ENOMEM.
    munmap(m, len);
    errno = 0;
    int hole_enomem = msync(m, len, MS_SYNC) == -1 && errno == ENOMEM;

    printf("msync_hole sync_ok=%d bad_flag=%d hole_enomem=%d\n", sync_ok, bad_flag, hole_enomem);
    return sync_ok && bad_flag && hole_enomem ? 0 : 1;
}
