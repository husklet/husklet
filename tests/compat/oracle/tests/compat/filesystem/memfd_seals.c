// memfd sealing is per-open-file-description state: a fresh memfd without MFD_ALLOW_SEALING
// reports F_SEAL_SEAL, an allow-sealing memfd starts unsealed, F_SEAL_WRITE blocks writes and
// cannot be applied while a writable mapping exists, F_SEAL_SHRINK blocks truncating down, and
// F_SEAL_SEAL makes the seal set immutable.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

int main(void) {
    int plain = memfd_create("plain", 0);
    int s0 = fcntl(plain, F_GET_SEALS);
    int addplain = fcntl(plain, F_ADD_SEALS, F_SEAL_WRITE);
    int eaddplain = (addplain == -1) ? errno : 0;

    int fd = memfd_create("sealable", MFD_ALLOW_SEALING);
    int s1 = fcntl(fd, F_GET_SEALS);
    (void)!write(fd, "hello", 5);
    int t0 = ftruncate(fd, 4096);
    int a1 = fcntl(fd, F_ADD_SEALS, F_SEAL_SHRINK);
    int s2 = fcntl(fd, F_GET_SEALS);
    int shrink = ftruncate(fd, 16);
    int eshrink = (shrink == -1) ? errno : 0;
    int grow = ftruncate(fd, 8192);

    char *m = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    int a2 = fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE);
    int ea2 = (a2 == -1) ? errno : 0; // EBUSY: writable mapping outstanding
    munmap(m, 4096);
    int a3 = fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE);
    ssize_t w = write(fd, "x", 1);
    int ew = (w == -1) ? errno : 0;
    int a4 = fcntl(fd, F_ADD_SEALS, F_SEAL_SEAL);
    int a5 = fcntl(fd, F_ADD_SEALS, F_SEAL_GROW);
    int ea5 = (a5 == -1) ? errno : 0;
    printf("s0=%d addplain=%d eaddplain=%d s1=%d t0=%d a1=%d s2=%d shrink=%d eshrink=%d grow=%d a2=%d ea2=%d a3=%d w=%zd ew=%d a4=%d a5=%d ea5=%d\n",
           s0, addplain, eaddplain, s1, t0, a1, s2, shrink, eshrink, grow, a2, ea2, a3, w, ew, a4, a5, ea5);
    return 0;
}
