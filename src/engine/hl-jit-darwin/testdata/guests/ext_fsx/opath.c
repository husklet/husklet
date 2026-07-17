// O_PATH descriptor (Linux): an fd that only names a file. fstat works through it; it can be a
// dirfd for *at() calls; reads are rejected with EBADF. Linux-only -> native oracle.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_opath_%d", (int)getpid());
    mkdir(dir, 0755);
    char fpath[192];
    snprintf(fpath, sizeof fpath, "%s/f", dir);
    int wf = open(fpath, O_CREAT | O_RDWR | O_TRUNC, 0644);
    write(wf, "opathdata", 9);
    close(wf);

    int pfd = open(fpath, O_PATH);
    struct stat st;
    int stat_ok = fstat(pfd, &st) == 0 && st.st_size == 9;
    char buf[16];
    int read_ebadf = read(pfd, buf, 4) == -1;   // O_PATH fds cannot be read

    int dpath = open(dir, O_PATH | O_DIRECTORY);
    int subfd = openat(dpath, "f", O_RDONLY);   // O_PATH dirfd usable for openat
    int reopened = subfd >= 0 && read(subfd, buf, 9) == 9 && memcmp(buf, "opathdata", 9) == 0;
    if (subfd >= 0) close(subfd);
    close(pfd);
    close(dpath);
    unlink(fpath);
    rmdir(dir);
    printf("opath stat=%d read_ebadf=%d reopened=%d\n", stat_ok, read_ebadf, reopened);
    return 0;
}
