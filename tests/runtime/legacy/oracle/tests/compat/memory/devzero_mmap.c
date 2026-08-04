// mmap of a character device must not be bounded by st_size. /dev/zero (and /dev/full, which the container
// backs with /dev/zero) report st_size 0 like an empty regular file, but on Linux they mmap to an unlimited
// stream of zero pages -- reading them never faults. The engine's past-EOF SIGBUS emulation is keyed off
// st_size; when it treated a char device as a 0-length file it armed the whole mapping for guest SIGBUS, so
// a plain read of an mmap'd /dev/zero page terminated the guest (exit 135) where native returns zero bytes.
// Regression: MAP_PRIVATE and MAP_SHARED maps of /dev/zero, plus an offset map, all read back zero.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

static int map_reads_zero(const char *dev, int mflags, off_t off, size_t len) {
    int fd = open(dev, MAP_SHARED == mflags ? O_RDWR : O_RDONLY);
    if (fd < 0) return -1;
    void *m = mmap(NULL, len, PROT_READ, mflags, fd, off);
    if (m == MAP_FAILED) { close(fd); return -2; }
    int zero = 1;
    for (size_t i = 0; i < len; i++)
        if (((volatile unsigned char *)m)[i] != 0) { zero = 0; break; }
    munmap(m, len);
    close(fd);
    return zero;
}

int main(void) {
    int priv = map_reads_zero("/dev/zero", MAP_PRIVATE, 0, 8192);
    int shared = map_reads_zero("/dev/zero", MAP_SHARED, 0, 4096);
    int offset = map_reads_zero("/dev/zero", MAP_PRIVATE, 4096, 4096);
    printf("devzero-mmap priv=%d shared=%d offset=%d\n", priv, shared, offset);
    return 0;
}
