// unlink/rmdir/unlinkat edge errno: EISDIR (unlink dir), ENOTDIR (rmdir file),
// ENOTEMPTY (rmdir non-empty), ENOENT (missing), and correct AT_REMOVEDIR behavior.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_unlinkerr_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);

    mkdirat(dfd, "d", 0755);
    close(openat(dfd, "d/inside", O_CREAT | O_WRONLY, 0644));
    close(openat(dfd, "f", O_CREAT | O_WRONLY, 0644));

    // unlink on a directory -> EISDIR (Linux).
    errno = 0;
    int u = unlinkat(dfd, "d", 0);
    int eisdir = u != 0 && errno == EISDIR;

    // rmdir on a regular file -> ENOTDIR.
    errno = 0;
    int r1 = unlinkat(dfd, "f", AT_REMOVEDIR);
    int enotdir = r1 != 0 && errno == ENOTDIR;

    // rmdir on a non-empty directory -> ENOTEMPTY.
    errno = 0;
    int r2 = unlinkat(dfd, "d", AT_REMOVEDIR);
    int enotempty = r2 != 0 && errno == ENOTEMPTY;

    // "path/" with a trailing slash naming a file -> ENOTDIR.
    char slashy[256];
    snprintf(slashy, sizeof slashy, "%s/f/", dir);
    errno = 0;
    int r3 = unlink(slashy);
    int slash_enotdir = r3 != 0 && (errno == ENOTDIR || errno == EISDIR);

    // Remove the contents then the now-empty directory succeeds.
    unlinkat(dfd, "d/inside", 0);
    int rmdir_ok = unlinkat(dfd, "d", AT_REMOVEDIR) == 0;

    // Missing name -> ENOENT.
    errno = 0;
    int miss = unlinkat(dfd, "gone", 0);
    int enoent = miss != 0 && errno == ENOENT;

    unlinkat(dfd, "f", 0);
    close(dfd);
    rmdir(dir);
    printf("unlink-errno eisdir=%d enotdir=%d enotempty=%d slash-enotdir=%d rmdir=%d enoent=%d\n",
           eisdir, enotdir, enotempty, slash_enotdir, rmdir_ok, enoent);
    return 0;
}
