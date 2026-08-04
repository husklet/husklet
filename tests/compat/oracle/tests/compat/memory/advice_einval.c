// Advice/flag validation is a fixed-ABI errno contract, not a host measurement: madvise and
// posix_fadvise reject an unknown advice with EINVAL, mlockall rejects unknown flag bits with
// EINVAL (flag check precedes the RLIMIT_MEMLOCK capability check, so no host permission state
// leaks in), and msync rejects an unknown flag with EINVAL. MADV_NORMAL still succeeds. Every
// value printed is a class the engine must emulate to Linux regardless of host kernel or backend.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    char *m = mmap(0, ps, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (m == MAP_FAILED) { printf("mmap_failed\n"); return 1; }

    errno = 0;
    int madv_bad = madvise(m, ps, 9999) == -1 ? errno : 0;
    int madv_ok = madvise(m, ps, MADV_NORMAL) == 0;
    // posix_fadvise returns the errno directly (0 on success).
    int fd = open("/tmp", O_RDONLY | O_DIRECTORY);
    int fadv_bad = posix_fadvise(fd, 0, 0, 9999);
    close(fd);
    errno = 0;
    int mlockall_bad = mlockall(0x40) == -1 ? errno : 0;
    errno = 0;
    int msync_bad = msync(m, ps, 0x8) == -1 ? errno : 0;
    munmap(m, ps);

    printf("advice-einval madv=%d fadv=%d mlockall=%d msync=%d madv_ok=%d\n",
           madv_bad, fadv_bad, mlockall_bad, msync_bad, madv_ok);
    return 0;
}
