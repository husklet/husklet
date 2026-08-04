// /dev character devices: correct S_IFCHR type, Linux-canonical rdev (major,minor), and real read/write
// semantics. Linux fixes these major/minor numbers (null 1,3; zero 1,5; full 1,7; random 1,8; urandom
// 1,9), so reporting the HOST device's rdev is a fidelity bug this catches.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <unistd.h>

static int chr_rdev(const char *p, int maj, int min) {
    struct stat s;
    if (stat(p, &s) != 0) return 0;
    return S_ISCHR(s.st_mode) && (int)major(s.st_rdev) == maj && (int)minor(s.st_rdev) == min;
}

int main(void) {
    int ok = 1;
    ok &= chr_rdev("/dev/null", 1, 3);
    ok &= chr_rdev("/dev/zero", 1, 5);
    ok &= chr_rdev("/dev/full", 1, 7);
    ok &= chr_rdev("/dev/random", 1, 8);
    ok &= chr_rdev("/dev/urandom", 1, 9);

    // read/write semantics
    char buf[16];
    int fd;

    fd = open("/dev/null", O_RDWR);
    ok &= fd >= 0 && read(fd, buf, sizeof buf) == 0;          // null: immediate EOF
    ok &= write(fd, "x", 1) == 1;                            // null: swallows writes
    if (fd >= 0) close(fd);

    fd = open("/dev/zero", O_RDONLY);
    memset(buf, 0xff, sizeof buf);
    ok &= fd >= 0 && read(fd, buf, 8) == 8 && buf[0] == 0 && buf[7] == 0; // zero: zero bytes
    if (fd >= 0) close(fd);

    fd = open("/dev/full", O_RDWR);
    memset(buf, 0xff, sizeof buf);
    ok &= fd >= 0 && read(fd, buf, 8) == 8 && buf[0] == 0;   // full: reads zeros
    errno = 0;
    ok &= write(fd, "x", 1) < 0 && errno == ENOSPC;         // full: writes ENOSPC
    if (fd >= 0) close(fd);

    fd = open("/dev/urandom", O_RDONLY);
    unsigned char r[32] = {0};
    int got = fd >= 0 ? (int)read(fd, r, sizeof r) : -1;
    int nonzero = 0;
    for (int i = 0; i < 32; i++) nonzero |= r[i];
    ok &= got == 32 && nonzero;                              // urandom: real entropy
    if (fd >= 0) close(fd);

    printf("devnodes ok=%d\n", ok);
    return 0;
}
