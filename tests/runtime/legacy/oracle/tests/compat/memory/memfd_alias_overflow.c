#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

enum { ALIAS_COUNT = 600, WRITE_ALIAS = 550 };

int main(void) {
    const size_t page = 4096;
    unsigned char *aliases[ALIAS_COUNT];
    int fd = (int)syscall(SYS_memfd_create, "alias-overflow", 0u);
    if (fd < 0 || ftruncate(fd, (off_t)(page * 2)) != 0) return 2;

    /*
     * Offset one Linux page deliberately disagrees with a 16 KiB host page.
     * MAP_FIXED prevents the engine from moving the returned guest address to
     * compensate, forcing its shared read-snapshot path on those hosts.
     */
    unsigned char *snapshot = mmap(NULL, page, PROT_NONE,
                                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (snapshot == MAP_FAILED ||
        mmap(snapshot, page, PROT_READ, MAP_SHARED | MAP_FIXED, fd,
             (off_t)page) != snapshot)
        return 3;

    for (int i = 0; i < ALIAS_COUNT; ++i) {
        aliases[i] = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_SHARED, fd,
                          (off_t)page);
        if (aliases[i] == MAP_FAILED) return 4;
    }
    if (close(fd) != 0) return 5;

    aliases[WRITE_ALIAS][37] = 0xa7;
    aliases[WRITE_ALIAS][page - 19] = 0x5c;
    volatile unsigned char *fresh = snapshot;
    int coherent = fresh[37] == 0xa7 && fresh[page - 19] == 0x5c;

    int unmapped = 1;
    for (int i = 0; i < ALIAS_COUNT; ++i)
        if (munmap(aliases[i], page) != 0) unmapped = 0;
    if (munmap(snapshot, page) != 0) unmapped = 0;

    unsigned char *reused = mmap(snapshot, page, PROT_READ | PROT_WRITE,
                                 MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS,
                                 -1, 0);
    if (reused != MAP_FAILED) reused[37] = 0x31;
    int reuse = unmapped && reused == snapshot && reused[37] == 0x31;
    if (reused != MAP_FAILED && munmap(reused, page) != 0) reuse = 0;

    printf("memfd-alias-overflow aliases=%d beyond512=%d coherent=%d unmapped=%d reuse=%d\n",
           ALIAS_COUNT, WRITE_ALIAS, coherent, unmapped, reuse);
    return coherent && unmapped && reuse ? 0 : 1;
}
