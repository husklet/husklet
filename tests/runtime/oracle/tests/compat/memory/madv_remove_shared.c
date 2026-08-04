// MADV_REMOVE punches a hole (zeroes the backing store) in a MAP_SHARED file mapping and the zeros are
// visible both through the mapping and via pread. The same advice on private-anon memory is rejected
// (EINVAL). Verdicts are errno-name / boolean, no addresses.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#ifndef MADV_REMOVE
#define MADV_REMOVE 9
#endif

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    char tmpl[] = "/tmp/madvrmXXXXXX";
    int fd = mkstemp(tmpl);
    if (fd < 0) { printf("open fail\n"); return 2; }
    char blk[128]; memset(blk, 0x5a, sizeof blk);
    for (long i = 0; i < ps; i += (long)sizeof blk) (void)!write(fd, blk, sizeof blk);

    char *sh = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (sh == MAP_FAILED) { printf("map fail\n"); return 2; }
    int before = sh[0] == 0x5a;
    int rc = madvise(sh, ps, MADV_REMOVE);
    int map_zero = rc == 0 && sh[0] == 0 && sh[ps - 1] == 0;
    char rb = 1; pread(fd, &rb, 1, 0);
    int file_zero = rc == 0 && rb == 0;
    munmap(sh, ps);

    unsigned char *an = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    errno = 0;
    int ar = madvise(an, ps, MADV_REMOVE);
    int anon_einval = ar == -1 && errno == EINVAL;
    munmap(an, ps);

    close(fd); unlink(tmpl);
    printf("before=%d rc=%d map_zero=%d file_zero=%d anon_einval=%d\n", before, rc, map_zero, file_zero,
           anon_einval);
    return 0;
}
