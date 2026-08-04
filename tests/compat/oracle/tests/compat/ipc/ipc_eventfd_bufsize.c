// eventfd read/write buffer-size contract, matching the kernel's asymmetry (fs/eventfd.c):
//   read  rejects count <  8  (a larger buffer transfers one 8-byte counter and returns 8)
//   write rejects count != 8  (any size other than exactly 8 is EINVAL)
// Also: a write of value 0 succeeds (returns 8) but is a pure no-op.
#include <sys/eventfd.h>
#include <unistd.h>
#include <stdint.h>
#include <stdio.h>
#include <errno.h>
#include <string.h>

static int wr_errno(int fd, const void *p, size_t n) {
    errno = 0;
    return write(fd, p, n) < 0 ? errno : 0;
}

int main(void) {
    int fd = eventfd(0, EFD_NONBLOCK);

    // a valid 8-byte write sets the counter.
    uint64_t five = 5;
    ssize_t w8 = write(fd, &five, 8);

    // read with an oversized (16-byte) buffer: yields exactly one 8-byte counter, returns 8, and
    // leaves the bytes past the counter untouched.
    unsigned char wide[16];
    memset(wide, 0xAA, sizeof wide);
    ssize_t r16 = read(fd, wide, sizeof wide);
    printf("w8 ret=%zd r16 ret=%zd value=%lu tail_untouched=%d\n",
           w8, r16, (unsigned long)*(uint64_t *)wide, wide[8] == 0xAA);

    // write rejects any count that is not exactly 8: 16, 9, and 4 are all EINVAL.
    *(uint64_t *)wide = 1;
    printf("write16 errno=%d write9 errno=%d write4 errno=%d\n",
           wr_errno(fd, wide, 16), wr_errno(fd, wide, 9), wr_errno(fd, wide, 4));

    // read rejects a short (< 8) buffer with EINVAL.
    uint32_t half = 0;
    errno = 0;
    ssize_t sr = read(fd, &half, 4);
    printf("short_read ret=%zd errno=%s\n", sr, strerror(errno));

    // a write of value 0 succeeds (returns 8) but is a no-op: the counter stays 0 (still drained),
    // so the next non-blocking read returns EAGAIN with no spurious value.
    uint64_t zero = 0;
    ssize_t zw = write(fd, &zero, 8);
    uint64_t v = 12345;
    errno = 0;
    ssize_t zr = read(fd, &v, 8);
    printf("zero_write ret=%zd zero_noop_read ret=%zd errno=%s\n",
           zw, zr, strerror(errno));
    return 0;
}
