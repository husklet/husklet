// O_PATH descriptor (Linux): an fd that only names a file. fstat works through it; it can be a
// dirfd for *at() calls; reads are rejected with EBADF. Linux-only -> native oracle.
#define _GNU_SOURCE
#include <errno.h>
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
    if (write(wf, "opathdata", 9) != 9) return 1;
    close(wf);

    int pfd = open(fpath, O_PATH);
    struct stat st;
    int stat_ok = fstat(pfd, &st) == 0 && st.st_size == 9;
    char buf[16];
    int read_ebadf = read(pfd, buf, 4) == -1;   // O_PATH fds cannot be read

    int dpath = open(dir, O_PATH | O_DIRECTORY);
    int subfd = openat(dpath, "f", O_RDONLY | O_NOCTTY); // glibc/fts includes harmless O_NOCTTY
    int reopened = subfd >= 0 && read(subfd, buf, 9) == 9 && memcmp(buf, "opathdata", 9) == 0;
    int directory_ebadf = read(dpath, buf, sizeof buf) == -1 && errno == EBADF;
    int cwd = open(".", O_RDONLY | O_DIRECTORY);
    int changed = fchdir(dpath) == 0;
    struct stat relative;
    int relative_stat = fstatat(dpath, "f", &relative, 0) == 0 && relative.st_size == 9;
    if (cwd >= 0) {
        if (fchdir(cwd) != 0) changed = 0;
        close(cwd);
    }
    if (subfd >= 0) close(subfd);
    close(pfd);
    close(dpath);
    int closed_ebadf = fchdir(dpath) == -1 && errno == EBADF;
    unlink(fpath);
    rmdir(dir);
    printf("opath stat=%d read_ebadf=%d reopened=%d dir_ebadf=%d fchdir=%d fstatat=%d closed=%d\n", stat_ok,
           read_ebadf, reopened, directory_ebadf, changed, relative_stat, closed_ebadf);
    return 0;
}
