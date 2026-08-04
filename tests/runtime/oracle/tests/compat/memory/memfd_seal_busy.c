// memfd F_SEAL_WRITE with an OUTSTANDING mapping: Linux (mm/shmem.c) refuses the seal with EBUSY while a
// MAP_SHARED mapping of the memfd is still live. A memfd is always opened read-write, so ANY shared mapping
// carries VM_MAYWRITE and blocks the seal regardless of its PROT (a PROT_READ shared map, or a shared map
// later mprotect'd read-only, still blocks); a MAP_PRIVATE (COW) mapping never does. Once every shared
// mapping is unmapped the seal succeeds, after which a fresh writable shared mmap is refused with EPERM.
// This is the reverse of memfd_seal.c, which seals before any mapping exists.
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

static int seal_errno(int fd) {
    errno = 0;
    return fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE) < 0 ? errno : 0;
}

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);

    // A MAP_PRIVATE writable mapping does NOT block the seal.
    int fp = (int)syscall(SYS_memfd_create, "priv", MFD_ALLOW_SEALING);
    if (fp < 0) { printf("memfd unsupported\n"); return 2; }
    ftruncate(fp, ps);
    void *pv = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_PRIVATE, fp, 0);
    int private_seal_ok = seal_errno(fp) == 0;
    if (pv != MAP_FAILED) munmap(pv, ps);
    close(fp);

    // A live writable shared mapping blocks F_SEAL_WRITE with EBUSY; unmapping it lets the seal through,
    // and a subsequent writable shared map is then refused with EPERM.
    int fd = (int)syscall(SYS_memfd_create, "shared", MFD_ALLOW_SEALING);
    ftruncate(fd, ps);
    void *w = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    int wr_ebusy = seal_errno(fd) == EBUSY;

    // A read-only shared mapping also blocks the seal (VM_MAYWRITE on a writable fd).
    munmap(w, ps);
    void *ro = mmap(NULL, ps, PROT_READ, MAP_SHARED, fd, 0);
    int ro_ebusy = seal_errno(fd) == EBUSY;
    munmap(ro, ps);

    int seal_ok = seal_errno(fd) == 0;
    errno = 0;
    void *after = mmap(NULL, ps, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    int after_eperm = after == MAP_FAILED && errno == EPERM;
    if (after != MAP_FAILED) munmap(after, ps);
    close(fd);

    printf("private_seal_ok=%d wr_ebusy=%d ro_ebusy=%d seal_ok=%d after_eperm=%d\n", private_seal_ok,
           wr_ebusy, ro_ebusy, seal_ok, after_eperm);
    return 0;
}
