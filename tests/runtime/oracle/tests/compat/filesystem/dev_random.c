// /dev/urandom + /dev/random contract: both are character devices (S_ISCHR, rdev 1:9 urandom, 1:8 random),
// a 256-byte read succeeds non-blocking with a high distinct-byte count, two reads differ, a 64 KiB read
// fully returns, and /dev/random yields bytes without blocking (modern kernels don't block once seeded).
// Only stable booleans printed -> native == engine byte-for-byte.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <unistd.h>

static int read_full(int fd, unsigned char *buf, size_t n) {
    size_t off = 0;
    while (off < n) {
        ssize_t r = read(fd, buf + off, n - off);
        if (r <= 0) return 0;
        off += (size_t)r;
    }
    return 1;
}

int main(void) {
    // stat both device nodes.
    struct stat su, sr;
    int uchr = 0, rchr = 0, urdev = 0, rrdev = 0;
    if (stat("/dev/urandom", &su) == 0) {
        uchr = S_ISCHR(su.st_mode);
        urdev = (major(su.st_rdev) == 1) && (minor(su.st_rdev) == 9);
    }
    if (stat("/dev/random", &sr) == 0) {
        rchr = S_ISCHR(sr.st_mode);
        rrdev = (major(sr.st_rdev) == 1) && (minor(sr.st_rdev) == 8);
    }

    int uf = open("/dev/urandom", O_RDONLY);
    unsigned char a[256] = {0}, b[256] = {0};
    int got_a = read_full(uf, a, sizeof a);
    int got_b = read_full(uf, b, sizeof b);
    int differ = memcmp(a, b, sizeof a) != 0;

    int seen[256] = {0}, distinct = 0;
    for (size_t i = 0; i < sizeof a; i++)
        if (!seen[a[i]]) { seen[a[i]] = 1; distinct++; }
    int distinct_ok = distinct >= 100;

    // A large 64 KiB read fully returns.
    static unsigned char big[65536];
    int got_big = read_full(uf, big, sizeof big);
    close(uf);

    // /dev/random yields bytes without blocking on a seeded kernel.
    int rf = open("/dev/random", O_RDONLY);
    unsigned char c[64] = {0};
    int got_r = read_full(rf, c, sizeof c);
    close(rf);

    printf("uchr=%d urdev=%d rchr=%d rrdev=%d\n", uchr, urdev, rchr, rrdev);
    printf("got_a=%d got_b=%d differ=%d distinct_ok=%d got_big=%d got_random=%d\n", got_a, got_b, differ,
           distinct_ok, got_big, got_r);
    return 0;
}
