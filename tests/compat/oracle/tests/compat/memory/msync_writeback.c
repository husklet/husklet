// Shared vs private file-mapping write-back. A store into a MAP_SHARED file map is flushed by msync(MS_SYNC)
// and visible via pread; a store into a MAP_PRIVATE (COW) map of the same file is NOT written back. Locks
// the file-backed coherence direction of the mapping cache.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    char tmpl[] = "/tmp/msyncwbXXXXXX";
    int fd = mkstemp(tmpl);
    if (fd < 0) { printf("open fail\n"); return 2; }
    char zero[64]; memset(zero, 'A', sizeof zero);
    for (long i = 0; i < ps; i += (long)sizeof zero) (void)!write(fd, zero, sizeof zero);

    char *sh = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    int mapped = sh != MAP_FAILED;
    if (!mapped) { printf("map fail\n"); return 2; }
    sh[0] = 'S'; sh[10] = 'T';
    int synced = msync(sh, ps, MS_SYNC) == 0;

    char rb[2] = {0, 0};
    pread(fd, &rb[0], 1, 0); pread(fd, &rb[1], 1, 10);
    int shared_written = rb[0] == 'S' && rb[1] == 'T';
    munmap(sh, ps);

    char *pr = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
    pr[0] = 'Z';
    msync(pr, ps, MS_SYNC);
    char rp = 0; pread(fd, &rp, 1, 0);
    int private_not_written = rp == 'S';    // still the shared-phase value, private store not flushed
    munmap(pr, ps);

    close(fd); unlink(tmpl);
    printf("mapped=%d synced=%d shared_written=%d private_not_written=%d\n", mapped, synced, shared_written,
           private_not_written);
    return 0;
}
