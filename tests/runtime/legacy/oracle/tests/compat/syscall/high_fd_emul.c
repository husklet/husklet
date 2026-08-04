// Emulated fd state must keep working above fd 1024. multi-process application routinely runs
// with high-numbered eventfd/timerfd/memfd descriptors during IPC startup.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/timerfd.h>
#include <sys/uio.h>
#include <unistd.h>

#ifndef MFD_ALLOW_SEALING
#define MFD_ALLOW_SEALING 2U
#endif
#ifndef F_ADD_SEALS
#define F_ADD_SEALS 1033
#define F_GET_SEALS 1034
#endif
#ifndef F_SEAL_WRITE
#define F_SEAL_WRITE 0x0008
#endif

static int bump_fds(void) {
    struct rlimit rl = {4096, 4096};
    setrlimit(RLIMIT_NOFILE, &rl);
    int last = -1;
    for (int i = 0; i < 1100; i++) {
        int fd = open("/dev/null", O_RDONLY);
        if (fd < 0) break;
        last = fd;
    }
    return last >= 1024;
}

static int one_sealed_memfd(void) {
    int fd = memfd_create("hl_high_seal", MFD_ALLOW_SEALING);
    if (fd < 1024) return 0;
    if (write(fd, "seed", 4) != 4) return 0;
    if (fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE) != 0) return 0;
    return fd;
}

int main(void) {
    int high_base = bump_fds();

    int efd = eventfd(7, 0);
    uint64_t ev = 0;
    int event_ok = efd >= 1024 && read(efd, &ev, sizeof ev) == sizeof ev && ev == 7;

    int tfd = timerfd_create(CLOCK_MONOTONIC, 0);
    struct itimerspec its;
    memset(&its, 0, sizeof its);
    its.it_value.tv_nsec = 1000000;
    int timer_ok = 0;
    if (tfd >= 1024 && timerfd_settime(tfd, 0, &its, NULL) == 0) {
        struct pollfd p = {.fd = tfd, .events = POLLIN};
        uint64_t tv = 0;
        timer_ok = poll(&p, 1, 500) == 1 && read(tfd, &tv, sizeof tv) == sizeof tv && tv >= 1;
    }

    char x = 'x';
    struct iovec iov = {.iov_base = &x, .iov_len = 1};

    int fd_writev = one_sealed_memfd();
    int deny_writev = fd_writev >= 1024 && writev(fd_writev, &iov, 1) < 0 && errno == EPERM;

    int fd_pwrite = one_sealed_memfd();
    int deny_pwrite = fd_pwrite >= 1024 && pwrite(fd_pwrite, &x, 1, 0) < 0 && errno == EPERM;

    int fd_pwritev = one_sealed_memfd();
    int deny_pwritev = fd_pwritev >= 1024 && pwritev(fd_pwritev, &iov, 1, 0) < 0 && errno == EPERM;

    int fd_pwritev2 = one_sealed_memfd();
    errno = 0;
    long pwritev2_ret = syscall(SYS_pwritev2, fd_pwritev2, &iov, 1, 0, 0, 0);
    int deny_pwritev2 = fd_pwritev2 >= 1024 && pwritev2_ret < 0 && errno == EPERM;

    printf("highfd base=%d event=%d timer=%d seals=%d%d%d%d\n",
           high_base, event_ok, timer_ok, deny_writev, deny_pwrite, deny_pwritev, deny_pwritev2);
    return 0;
}
