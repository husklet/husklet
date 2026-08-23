// readv/writev boundary: iovcnt 0 succeeds with 0, iovcnt above IOV_MAX and a
// negative iovcnt are EINVAL, and an invalid payload range in an entry before
// the aggregate overflows is EFAULT.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/uio.h>
#include <unistd.h>

int main(void) {
    int fd = open("/dev/null", O_RDWR);
    struct iovec one = {(void *)"x", 1};
    ssize_t z = writev(fd, &one, 0);
    int ez = (z == -1) ? errno : 0;
    static struct iovec many[IOV_MAX + 1];
    for (int i = 0; i <= IOV_MAX; i++) {
        many[i].iov_base = (void *)"x";
        many[i].iov_len = 0;
    }
    ssize_t big = writev(fd, many, IOV_MAX + 1);
    int ebig = (big == -1) ? errno : 0;
    volatile int negc = -1;
    ssize_t neg = writev(fd, &one, negc);
    int eneg = (neg == -1) ? errno : 0;
    struct iovec ov[2] = {{(void *)"x", (size_t)SSIZE_MAX}, {(void *)"y", (size_t)SSIZE_MAX}};
    ssize_t ovf = writev(fd, ov, 2);
    int eovf = (ovf == -1) ? errno : 0;
    struct iovec zero_bad = {(void *)UINTPTR_MAX, 0};
    errno = 0;
    ssize_t zero_write = writev(fd, &zero_bad, 1);
    int ezero_write = (zero_write == -1) ? errno : 0;
    errno = 0;
    ssize_t zero_read = readv(fd, &zero_bad, 1);
    int ezero_read = (zero_read == -1) ? errno : 0;
    ssize_t r = readv(fd, &one, 0);
    printf("z=%zd ez=%d big=%zd ebig=%d neg=%zd eneg=%d ovf=%zd eovf=%d zw=%zd ezw=%d zr=%zd ezr=%d r=%zd "
           "iovmax=%d\n",
           z, ez, big, ebig, neg, eneg, ovf, eovf, zero_write, ezero_write, zero_read, ezero_read, r, IOV_MAX);
    return 0;
}
