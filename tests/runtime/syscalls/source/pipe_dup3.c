// fcntl F_GETPIPE_SZ / F_SETPIPE_SZ round-trip and dup3(2) flag semantics (EINVAL on oldfd==newfd,
// O_CLOEXEC propagation, EINVAL on unknown flags).
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>
#include <errno.h>

int main(void) {
    int p[2];
    pipe(p);
    long sz = fcntl(p[1], F_GETPIPE_SZ);
    printf("default_pipe_sz_pos=%d\n", sz > 0);
    long set = fcntl(p[1], F_SETPIPE_SZ, 8192);
    long got = fcntl(p[1], F_GETPIPE_SZ);
    printf("setpipe rc_pos=%d got_ge_8192=%d\n", set >= 0, got >= 8192);
    long s2 = fcntl(p[1], F_SETPIPE_SZ, 4096);
    long g2 = fcntl(p[1], F_GETPIPE_SZ);
    printf("shrink rc_pos=%d got=%ld\n", s2 >= 0, g2);
    close(p[0]);
    close(p[1]);

    int a[2];
    pipe(a);
    errno = 0;
    int d = dup3(a[0], a[0], 0);
    printf("dup3 same rc=%d errno=%d\n", d, d < 0 ? errno : 0);
    int nd = dup3(a[0], 20, O_CLOEXEC);
    int fl = fcntl(20, F_GETFD);
    printf("dup3 cloexec rc=%d cloexec=%d\n", nd, (fl & FD_CLOEXEC) != 0);
    errno = 0;
    int bd = dup3(a[0], 21, 0x4);
    printf("dup3 badflag rc=%d errno=%d\n", bd, bd < 0 ? errno : 0);
    close(a[0]);
    close(a[1]);
    return 0;
}
