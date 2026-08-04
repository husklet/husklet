// Hard links: link count tracks additions and removals; all names share one inode and
// content; a hard link to a directory is refused (EPERM).
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_hardlink_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);

    int fd = openat(dfd, "a", O_CREAT | O_RDWR, 0644);
    write(fd, "shared", 6);
    close(fd);

    linkat(dfd, "a", dfd, "b", 0);
    linkat(dfd, "a", dfd, "c", 0);
    struct stat sa, sb;
    fstatat(dfd, "a", &sa, 0);
    fstatat(dfd, "b", &sb, 0);
    int nlink3 = sa.st_nlink == 3;
    int same_inode = sa.st_ino == sb.st_ino;

    // Writing through one name is visible through another.
    int wf = openat(dfd, "b", O_WRONLY);
    pwrite(wf, "X", 1, 0);
    close(wf);
    char buf[8] = {0};
    int rf = openat(dfd, "c", O_RDONLY);
    read(rf, buf, 6);
    close(rf);
    int content_shared = memcmp(buf, "Xhared", 6) == 0;

    // Removing one name decrements the count; the inode persists via the others.
    unlinkat(dfd, "a", 0);
    struct stat s2;
    fstatat(dfd, "b", &s2, 0);
    int nlink2 = s2.st_nlink == 2;

    // Hard-linking a directory is refused.
    mkdirat(dfd, "d", 0755);
    errno = 0;
    int link_dir = linkat(dfd, "d", dfd, "d2", 0);
    int dir_eperm = link_dir != 0 && (errno == EPERM || errno == EACCES);

    unlinkat(dfd, "b", 0);
    unlinkat(dfd, "c", 0);
    unlinkat(dfd, "d", AT_REMOVEDIR);
    close(dfd);
    rmdir(dir);
    printf("hardlink-nlink nlink3=%d same-inode=%d content-shared=%d nlink2=%d dir-eperm=%d\n",
           nlink3, same_inode, content_shared, nlink2, dir_eperm);
    return 0;
}
