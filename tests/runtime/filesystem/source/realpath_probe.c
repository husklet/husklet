// realpath(3): symlink and ".."/"." components canonicalize; two paths to one inode
// resolve to the same string; a missing tail yields ENOENT.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char base[128];
    snprintf(base, sizeof base, "/tmp/hl_realpath_%d", (int)getpid());
    char sub[192], file[256], link[256], messy[320];
    snprintf(sub, sizeof sub, "%s/sub", base);
    snprintf(file, sizeof file, "%s/file", sub);
    snprintf(link, sizeof link, "%s/link", base);
    snprintf(messy, sizeof messy, "%s/./sub/../sub/file", base);
    mkdir(base, 0755);
    mkdir(sub, 0755);
    close(open(file, O_CREAT | O_WRONLY, 0644));
    symlink("sub/file", link);

    char direct[PATH_MAX], via_link[PATH_MAX], via_messy[PATH_MAX];
    int d = realpath(file, direct) != NULL;
    int l = realpath(link, via_link) != NULL;
    int m = realpath(messy, via_messy) != NULL;

    // All three must canonicalize to the identical absolute path.
    int link_same = d && l && strcmp(direct, via_link) == 0;
    int messy_same = d && m && strcmp(direct, via_messy) == 0;
    // The canonical path ends with the real components (no "." / ".." / symlink left).
    int tail_ok = d && strstr(direct, "/sub/file") != NULL && strstr(direct, "..") == NULL;

    // A nonexistent final component fails with ENOENT.
    char miss[320], out[PATH_MAX];
    snprintf(miss, sizeof miss, "%s/sub/nope", base);
    errno = 0;
    int enoent_ok = realpath(miss, out) == NULL && errno == ENOENT;

    unlink(link);
    unlink(file);
    rmdir(sub);
    rmdir(base);
    printf("realpath-probe link-same=%d messy-same=%d tail=%d enoent=%d\n",
           link_same, messy_same, tail_ok, enoent_ok);
    return 0;
}
