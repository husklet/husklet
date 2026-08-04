// open(2) flag error semantics: O_EXCL, O_DIRECTORY, O_NOFOLLOW, O_CREAT on a missing
// parent, and O_WRONLY on a directory all map to their canonical errno.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_openerr_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);

    close(openat(dfd, "file", O_CREAT | O_WRONLY, 0644));
    mkdirat(dfd, "sub", 0755);
    symlinkat("file", dfd, "sym");

    // O_CREAT | O_EXCL on an existing file -> EEXIST.
    errno = 0;
    int excl = openat(dfd, "file", O_CREAT | O_EXCL | O_WRONLY, 0644);
    int eexist = excl < 0 && errno == EEXIST;

    // O_DIRECTORY on a regular file -> ENOTDIR.
    errno = 0;
    int notdir = openat(dfd, "file", O_RDONLY | O_DIRECTORY);
    int enotdir = notdir < 0 && errno == ENOTDIR;

    // O_NOFOLLOW on a final symlink component -> ELOOP.
    errno = 0;
    int nofollow = openat(dfd, "sym", O_RDONLY | O_NOFOLLOW);
    int eloop = nofollow < 0 && errno == ELOOP;

    // O_WRONLY on a directory -> EISDIR.
    errno = 0;
    int wrdir = openat(dfd, "sub", O_WRONLY);
    int eisdir = wrdir < 0 && errno == EISDIR;

    // Create under a missing parent directory -> ENOENT.
    errno = 0;
    int missing = openat(dfd, "nope/child", O_CREAT | O_WRONLY, 0644);
    int enoent = missing < 0 && errno == ENOENT;

    unlinkat(dfd, "sym", 0);
    unlinkat(dfd, "file", 0);
    unlinkat(dfd, "sub", AT_REMOVEDIR);
    close(dfd);
    rmdir(dir);
    printf("open-errno eexist=%d enotdir=%d eloop=%d eisdir=%d enoent=%d\n",
           eexist, enotdir, eloop, eisdir, enoent);
    return 0;
}
