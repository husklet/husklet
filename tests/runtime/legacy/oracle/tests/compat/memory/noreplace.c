#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#ifndef MAP_FIXED_NOREPLACE
#define MAP_FIXED_NOREPLACE 0x100000
#endif

int main(void) {
    const size_t host_granule = 16384;
    size_t reserve_size = host_granule * 3;
    unsigned char payload[host_granule];
    unsigned char *arena;
    unsigned char *occupied;
    unsigned char *vacant;
    unsigned char *exact;
    void *collision;
    int fd = open("/data", O_RDWR);
    memset(payload, 0x6d, sizeof payload);
    if (fd < 0 || ftruncate(fd, (off_t)sizeof payload) != 0 ||
        pwrite(fd, payload, sizeof payload, 0) != (ssize_t)sizeof payload) return 1;

    arena = mmap(NULL, reserve_size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (arena == MAP_FAILED) return 1;
    occupied = mmap(NULL, host_granule, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (occupied == MAP_FAILED) return 1;
    memset(occupied, 0xa7, host_granule);
    vacant = (unsigned char *)(((uintptr_t)arena + host_granule - 1) & ~(uintptr_t)(host_granule - 1));
    if (vacant + host_granule > arena + reserve_size || munmap(vacant, host_granule) != 0) return 1;

    errno = 0;
    collision = mmap(occupied, host_granule, PROT_READ, MAP_PRIVATE | MAP_FIXED_NOREPLACE, fd, 0);
    int rejected = collision == MAP_FAILED && errno == EEXIST;
    int preserved = occupied[0] == 0xa7 && occupied[host_granule - 1] == 0xa7;
    exact = mmap(vacant, host_granule, PROT_READ, MAP_PRIVATE | MAP_FIXED_NOREPLACE, fd, 0);
    int placed = exact == vacant;
    int contents = placed && exact[0] == 0x6d && exact[host_granule - 1] == 0x6d;

    printf("noreplace rejected=%d preserved=%d placed=%d contents=%d\n",
           rejected, preserved, placed, contents);
    if (placed) munmap(exact, host_granule);
    munmap(occupied, host_granule);
    munmap(arena, reserve_size);
    close(fd);
    return rejected && preserved && placed && contents ? 0 : 1;
}
