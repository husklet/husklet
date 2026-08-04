// memfd write-sealing: after F_ADD_SEALS(F_SEAL_WRITE) a shared writable mmap is refused with EPERM, while
// a PROT_READ shared map and a PROT_WRITE MAP_PRIVATE (COW) map are still allowed. Confirms the engine
// honors seal-derived mmap permission checks.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef MFD_ALLOW_SEALING
#define MFD_ALLOW_SEALING 0x0002U
#endif
#ifndef F_ADD_SEALS
#define F_ADD_SEALS 1033
#endif
#ifndef F_SEAL_WRITE
#define F_SEAL_WRITE 0x0008
#endif

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    int fd = (int)syscall(SYS_memfd_create, "sealed", MFD_ALLOW_SEALING);
    if (fd < 0) { printf("memfd unsupported\n"); return 2; }
    if (ftruncate(fd, ps) != 0) { printf("truncate fail\n"); return 2; }

    int sealed = fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE) == 0;

    errno = 0;
    void *w = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    int shared_wr_eperm = w == MAP_FAILED && errno == EPERM;
    if (w != MAP_FAILED) munmap(w, ps);

    void *ro = mmap(NULL, ps, PROT_READ, MAP_SHARED, fd, 0);
    int shared_ro_ok = ro != MAP_FAILED;
    if (ro != MAP_FAILED) munmap(ro, ps);

    void *pr = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
    int private_wr_ok = pr != MAP_FAILED;
    if (pr != MAP_FAILED) munmap(pr, ps);

    close(fd);
    printf("sealed=%d shared_wr_eperm=%d shared_ro_ok=%d private_wr_ok=%d\n", sealed, shared_wr_eperm,
           shared_ro_ok, private_wr_ok);
    return 0;
}
