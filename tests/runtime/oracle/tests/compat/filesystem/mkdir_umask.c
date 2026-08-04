// mkdir/mkdirat and open honor the process umask; explicit fchmod overrides it.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_umask_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);

    mode_t old = umask(022);

    mkdirat(dfd, "d777", 0777);
    struct stat sd;
    fstatat(dfd, "d777", &sd, 0);
    int dir_masked = (sd.st_mode & 07777) == 0755;   // 0777 & ~022

    close(openat(dfd, "f666", O_CREAT | O_WRONLY, 0666));
    struct stat sf;
    fstatat(dfd, "f666", &sf, 0);
    int file_masked = (sf.st_mode & 07777) == 0644;  // 0666 & ~022

    // Tighter umask.
    umask(077);
    mkdirat(dfd, "d700", 0777);
    struct stat s7;
    fstatat(dfd, "d700", &s7, 0);
    int tight_masked = (s7.st_mode & 07777) == 0700;

    // Explicit fchmod bypasses the umask entirely.
    int mfd = openat(dfd, "explicit", O_CREAT | O_RDWR, 0666);
    fchmod(mfd, 0664);
    struct stat se;
    fstat(mfd, &se);
    int explicit_ok = (se.st_mode & 07777) == 0664;
    close(mfd);

    umask(old);
    unlinkat(dfd, "d777", AT_REMOVEDIR);
    unlinkat(dfd, "d700", AT_REMOVEDIR);
    unlinkat(dfd, "f666", 0);
    unlinkat(dfd, "explicit", 0);
    close(dfd);
    rmdir(dir);
    printf("mkdir-umask dir=%d file=%d tight=%d explicit=%d\n",
           dir_masked, file_masked, tight_masked, explicit_ok);
    return 0;
}
