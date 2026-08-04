// statx(2) argument validation is a fixed-ABI contract: an unknown AT_ flag bit is EINVAL, a
// reserved mask bit (STATX__RESERVED, 0x80000000) is EINVAL, and a valid call always reports the
// STATX_BASIC_STATS the engine synthesizes deterministically (type/mode/ino/nlink present bits
// set for a directory queried via AT_EMPTY_PATH). Errno classes and mandated present-bits only,
// so identical emulated-on-Linux and on the macOS backend.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    int dfd = open("/tmp", O_RDONLY | O_DIRECTORY);
    struct statx stx;

    errno = 0;
    int badflag = statx(dfd, "", 0x40000000, STATX_BASIC_STATS, &stx) == -1 ? errno : 0;
    errno = 0;
    int badmask = statx(dfd, "", AT_EMPTY_PATH, 0x80000000 | STATX_BASIC_STATS, &stx) == -1 ? errno : 0;

    int ok = statx(dfd, "", AT_EMPTY_PATH, STATX_BASIC_STATS, &stx) == 0;
    int have_type = ok && (stx.stx_mask & STATX_TYPE) && S_ISDIR(stx.stx_mode);
    int have_core = ok && (stx.stx_mask & STATX_MODE) && (stx.stx_mask & STATX_INO)
                       && (stx.stx_mask & STATX_NLINK);

    close(dfd);
    printf("statx-validate badflag=%d badmask=%d have_type=%d have_core=%d\n",
           badflag, badmask, have_type, have_core);
    return 0;
}
