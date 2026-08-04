// writev(2) iovcnt validation: an iovcnt above IOV_MAX(1024) or negative fails with EINVAL; exactly
// IOV_MAX zero-length vectors is legal and transfers zero bytes.
#define _GNU_SOURCE
#include <stdio.h>
#include <unistd.h>
#include <errno.h>
#include <sys/uio.h>

int main(void) {
    int p[2];
    pipe(p);
    static struct iovec iov[1025];
    char c = 'x';
    for (int i = 0; i < 1025; i++) { iov[i].iov_base = &c; iov[i].iov_len = 0; }
    errno = 0;
    ssize_t w = writev(p[1], iov, 1025);
    printf("writev 1025 rc=%zd errno=%d\n", w, w < 0 ? errno : 0);
    errno = 0;
    ssize_t w2 = writev(p[1], iov, -1);
    printf("writev neg rc=%zd errno=%d\n", w2, w2 < 0 ? errno : 0);
    errno = 0;
    ssize_t w3 = writev(p[1], iov, 1024);
    printf("writev 1024 rc=%zd errno=%d\n", w3, w3 < 0 ? errno : 0);
    close(p[0]);
    close(p[1]);
    return 0;
}
