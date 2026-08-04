// chmod/fchmod/fchmodat: setuid, setgid, and sticky bits round-trip through stat.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_chmodbits_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);

    int fd = openat(dfd, "file", O_CREAT | O_RDWR, 0600);

    // setuid + rwxr-xr-x via fchmod.
    fchmod(fd, 04755);
    struct stat s1;
    fstat(fd, &s1);
    int setuid_ok = (s1.st_mode & 07777) == 04755;

    // setgid + group perms via fchmodat.
    fchmodat(dfd, "file", 02750, 0);
    struct stat s2;
    fstatat(dfd, "file", &s2, 0);
    int setgid_ok = (s2.st_mode & 07777) == 02750;

    // Sticky bit on the directory.
    fchmod(dfd, 01755);
    struct stat s3;
    fstat(dfd, &s3);
    int sticky_ok = (s3.st_mode & 07777) == 01755;

    // All special bits together.
    fchmod(fd, 07644);
    struct stat s4;
    fstat(fd, &s4);
    int all_ok = (s4.st_mode & 07777) == 07644;

    close(fd);
    fchmod(dfd, 0755);
    unlinkat(dfd, "file", 0);
    close(dfd);
    rmdir(dir);
    printf("chmod-bits setuid=%d setgid=%d sticky=%d all-special=%d\n",
           setuid_ok, setgid_ok, sticky_ok, all_ok);
    return 0;
}
