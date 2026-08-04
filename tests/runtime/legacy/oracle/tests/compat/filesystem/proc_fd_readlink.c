// /proc/self/fd magic symlinks: an open file's fd link resolves to its real path; a pipe
// fd resolves to a "pipe:" magic name; /proc/self/fd counts the open descriptors.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_procfd_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[192];
    snprintf(path, sizeof path, "%s/file", dir);
    int fd = open(path, O_CREAT | O_RDWR, 0644);

    char proc[64], target[256] = {0};
    snprintf(proc, sizeof proc, "/proc/self/fd/%d", fd);
    ssize_t n = readlink(proc, target, sizeof target - 1);
    int file_ok = n > 0 && strcmp(target, path) == 0;

    // A pipe read end resolves to a "pipe:[...]" magic link.
    int pf[2];
    pipe(pf);
    char ppath[64], ptarget[128] = {0};
    snprintf(ppath, sizeof ppath, "/proc/self/fd/%d", pf[0]);
    ssize_t pn = readlink(ppath, ptarget, sizeof ptarget - 1);
    int pipe_ok = pn > 0 && strncmp(ptarget, "pipe:", 5) == 0;

    // stat through the magic link reaches the real inode.
    struct stat viaproc, direct;
    int stat_same = stat(proc, &viaproc) == 0 && stat(path, &direct) == 0 &&
                    viaproc.st_ino == direct.st_ino;

    close(pf[0]); close(pf[1]);
    close(fd);
    unlink(path);
    rmdir(dir);
    printf("proc-fd-readlink file=%d pipe=%d stat-same=%d\n", file_ok, pipe_ok, stat_same);
    return 0;
}
