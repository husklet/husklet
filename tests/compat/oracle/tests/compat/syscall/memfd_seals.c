// F_ADD_SEALS(1033)/F_GET_SEALS(1034) differential probe. Two surfaces:
//   1. A NON-memfd fd on a sealing-capable filesystem (tmpfs /tmp): native reports the real seal state
//      (shmem files are born F_SEAL_SEAL, so F_GET_SEALS -> F_SEAL_SEAL and a further F_ADD_SEALS -> EPERM),
//      NOT the blanket EINVAL an unconditional memfd-only emulation would return. The engine forwards these
//      commands to the guest's real host fd on Linux, so the answer matches the host kernel.
//   2. A real memfd created with MFD_ALLOW_SEALING: seal round-trip, the F_SEAL_WRITE-while-mapped EBUSY
//      guard, write-after-seal EPERM, and F_SEAL_SHRINK ftruncate EPERM.
// Output is arch-neutral (booleans + errnos), so one golden covers aarch64 and x86_64.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#ifndef F_ADD_SEALS
#define F_ADD_SEALS 1033
#define F_GET_SEALS 1034
#endif
#ifndef F_SEAL_SEAL
#define F_SEAL_SEAL 0x0001
#define F_SEAL_SHRINK 0x0002
#define F_SEAL_GROW 0x0004
#define F_SEAL_WRITE 0x0008
#endif
#ifndef MFD_ALLOW_SEALING
#define MFD_ALLOW_SEALING 0x0002U
#endif

int main(void) {
    // ---- 1. non-memfd fd on tmpfs (/tmp) ----
    char path[] = "/tmp/memfdseals.XXXXXX";
    int fd = mkstemp(path);
    unlink(path);
    int gs = fcntl(fd, F_GET_SEALS);
    printf("tmpfile.getseals.ret=%d errno=%d\n", gs, gs < 0 ? errno : 0);
    int as = fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE);
    printf("tmpfile.addseal.ret=%d errno=%d\n", as, as < 0 ? errno : 0);
    close(fd);

    // ---- 2. real memfd with sealing allowed ----
    int m = memfd_create("hl-seal", MFD_ALLOW_SEALING);
    printf("memfd.created=%d\n", m >= 0);
    if (m >= 0) {
        if (ftruncate(m, 4096) != 0) return 2;
        printf("memfd.seals0=%d\n", fcntl(m, F_GET_SEALS));
        void *addr = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, m, 0);
        int aw = fcntl(m, F_ADD_SEALS, F_SEAL_WRITE);
        printf("memfd.seal_write_mapped.ret=%d errno=%d\n", aw, aw < 0 ? errno : 0);
        if (addr != MAP_FAILED) munmap(addr, 4096);
        int aw2 = fcntl(m, F_ADD_SEALS, F_SEAL_WRITE);
        printf("memfd.seal_write_unmapped.ret=%d errno=%d\n", aw2, aw2 < 0 ? errno : 0);
        printf("memfd.seals1=%d\n", fcntl(m, F_GET_SEALS));
        char b[4] = {1, 2, 3, 4};
        ssize_t w = pwrite(m, b, 4, 0);
        printf("memfd.write_after_seal.ret=%zd errno=%d\n", w, w < 0 ? errno : 0);
        fcntl(m, F_ADD_SEALS, F_SEAL_SHRINK);
        int t = ftruncate(m, 100);
        printf("memfd.shrink_after_seal.ret=%d errno=%d\n", t, t < 0 ? errno : 0);
        close(m);
    }
    return 0;
}
