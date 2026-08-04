// Adversarial robustness: getdents64 into a result buffer that straddles an unmapped page must return
// EFAULT (as Linux copy_to_user does), never fault the engine that services the call. The buffer is
// written directly by the syscall handler, so a missing mapping check would crash the host engine
// process instead of reporting a clean errno to the guest. A fully mapped buffer must still enumerate.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdint.h>
#include <unistd.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>

static long getdents64(int fd, void *buf, size_t count) {
    errno = 0;
    return syscall(SYS_getdents64, fd, buf, count);
}

int main(void) {
    long page = sysconf(_SC_PAGESIZE);
    if (page <= 0) return 10;

    // Deterministic directory with a couple of known entries.
    const char *dir = "/tmp/hl-getdents-fault";
    mkdir(dir, 0700);
    for (int i = 0; i < 3; i++) {
        char p[64];
        snprintf(p, sizeof p, "%s/e%d", dir, i);
        int fd = open(p, O_CREAT | O_WRONLY | O_TRUNC, 0600);
        if (fd >= 0) close(fd);
    }

    int dfd = open(dir, O_RDONLY | O_DIRECTORY);
    if (dfd < 0) return 11;

    // Two pages, then unmap the second so a buffer near the boundary straddles into unmapped memory.
    unsigned char *base = mmap(NULL, (size_t)page * 2, PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (base == MAP_FAILED) return 12;
    if (munmap(base + page, (size_t)page) != 0) return 13;

    // Only 8 bytes of mapped room before the hole: no dirent fits, so the first copy faults -> EFAULT.
    void *straddle = base + page - 8;
    long r = getdents64(dfd, straddle, (size_t)page);
    int fault_efault = (r == -1 && errno == EFAULT);

    // A fully mapped buffer on the same fd still enumerates the directory.
    lseek(dfd, 0, SEEK_SET);
    long good = getdents64(dfd, base, (size_t)page);
    int valid_ok = (good > 0);

    printf("getdents64 straddle_efault=%d valid_ok=%d\n", fault_efault, valid_ok);
    return (fault_efault && valid_ok) ? 0 : 1;
}
