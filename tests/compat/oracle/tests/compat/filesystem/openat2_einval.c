// openat2(2) struct-open_how validation is a fixed-ABI contract: a too-small size (0 or short)
// is EINVAL, an unknown resolve bit is EINVAL, an unknown/high open flag bit is EINVAL, an
// oversized how with a non-zero trailing byte is E2BIG, and an oversized all-zero how is accepted
// (forward-compatible). These are errno classes the engine must emulate to Linux; the earlier
// openat2-resolve case pins the RESOLVE_* containment path, this one pins argument validation.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/openat2.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

static int oa2(int dfd, const char *p, void *how, size_t sz) {
    return (int)syscall(__NR_openat2, dfd, p, how, sz);
}

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_oa2e_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);

    struct open_how h = {.flags = O_RDONLY};
    errno = 0; int size0 = oa2(dfd, ".", &h, 0) < 0 ? errno : 0;
    errno = 0; int small = oa2(dfd, ".", &h, sizeof h - 4) < 0 ? errno : 0;

    struct open_how hr = {.flags = O_RDONLY, .resolve = 0x1000};
    errno = 0; int badresolve = oa2(dfd, ".", &hr, sizeof hr) < 0 ? errno : 0;

    struct open_how hf = {.flags = 0x1ULL << 40};
    errno = 0; int badflags = oa2(dfd, ".", &hf, sizeof hf) < 0 ? errno : 0;

    struct big { struct open_how h; char pad[16]; };
    struct big bj; memset(&bj, 0, sizeof bj); bj.h.flags = O_RDONLY; bj.pad[0] = 1;
    errno = 0; int bigjunk = oa2(dfd, ".", &bj, sizeof bj) < 0 ? errno : 0;
    struct big bz; memset(&bz, 0, sizeof bz); bz.h.flags = O_RDONLY;
    int bzr = oa2(dfd, ".", &bz, sizeof bz);
    int bigzero_ok = bzr >= 0;
    if (bzr >= 0) close(bzr);

    close(dfd);
    rmdir(dir);
    printf("openat2-einval size0=%d small=%d badresolve=%d badflags=%d bigjunk=%d bigzero_ok=%d\n",
           size0, small, badresolve, badflags, bigjunk, bigzero_ok);
    return 0;
}
