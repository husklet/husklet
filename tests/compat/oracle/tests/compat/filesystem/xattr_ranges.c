// getxattr/listxattr size-buffer contract: a zero-size call is a length probe returning the
// exact stored size, a non-zero-but-too-small buffer is ERANGE, a missing attribute is ENODATA.
// These are fixed-ABI errno/length classes the engine must emulate to Linux. On a filesystem
// without user-xattr support ENOTSUP is a recorded, deterministic outcome (mirrors the existing
// xattr-roundtrip case), so the verdict line is host-invariant either way.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>
#include <sys/xattr.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_xr_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[192];
    snprintf(path, sizeof path, "%s/file", dir);
    close(open(path, O_CREAT | O_WRONLY, 0644));

    int rc = setxattr(path, "user.k", "hello", 5, 0);
    if (rc != 0 && errno == ENOTSUP) {
        unlink(path); rmdir(dir);
        printf("xattr-ranges supported=0\n");
        return 0;
    }

    // Zero-size probe returns the stored value length.
    long probe = getxattr(path, "user.k", NULL, 0);
    int probe_ok = probe == 5;
    // Too-small buffer -> ERANGE.
    char sb[2];
    errno = 0;
    int erange = getxattr(path, "user.k", sb, 2) == -1 && errno == ERANGE;
    // Missing attribute -> ENODATA.
    errno = 0;
    int missing = getxattr(path, "user.absent", sb, sizeof sb) == -1 && errno == ENODATA;
    // listxattr zero-size probe returns a positive length; too-small buffer -> ERANGE.
    long lp = listxattr(path, NULL, 0);
    int list_probe_ok = lp >= 7; // at least "user.k\0"
    errno = 0;
    char lb[1];
    int list_erange = listxattr(path, lb, 1) == -1 && errno == ERANGE;

    unlink(path); rmdir(dir);
    printf("xattr-ranges supported=1 probe=%d erange=%d missing=%d list_probe=%d list_erange=%d\n",
           probe_ok, erange, missing, list_probe_ok, list_erange);
    return 0;
}
