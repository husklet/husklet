// memfd_create + ftruncate + two independent MAP_SHARED mappings of the same descriptor. A store through
// one mapping is visible through the other and through pread(), proving shared page-cache coherence for an
// anonymous file object.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    int fd = (int)syscall(SYS_memfd_create, "mfd", 0u);
    if (fd < 0) { printf("memfd unsupported\n"); return 2; }
    if (ftruncate(fd, ps) != 0) { printf("truncate fail\n"); return 2; }

    unsigned char *a = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    unsigned char *b = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    int mapped = a != MAP_FAILED && b != MAP_FAILED;
    if (!mapped) { printf("map fail\n"); return 2; }

    a[0] = 0x4d; a[ps - 1] = 0x5e;
    int b_sees = b[0] == 0x4d && b[ps - 1] == 0x5e;

    unsigned char buf[2] = {0, 0};
    int r0 = (int)pread(fd, &buf[0], 1, 0);
    int rn = (int)pread(fd, &buf[1], 1, ps - 1);
    int file_sees = r0 == 1 && rn == 1 && buf[0] == 0x4d && buf[1] == 0x5e;

    // A write through the file descriptor is visible in the shared mapping too.
    unsigned char w = 0x9c;
    pwrite(fd, &w, 1, 0);
    int map_sees_write = a[0] == 0x9c && b[0] == 0x9c;

    munmap(a, ps); munmap(b, ps); close(fd);
    printf("mapped=%d b_sees=%d file_sees=%d map_sees_write=%d\n", mapped, b_sees, file_sees, map_sees_write);
    return 0;
}
