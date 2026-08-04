// utimensat(2): explicit times, UTIME_OMIT to preserve, UTIME_NOW to touch.
// Linux -> deterministic golden verdict on every engine.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_utimensat_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[192];
    snprintf(path, sizeof path, "%s/file", dir);
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    close(fd);

    // Set both times to fixed epoch values.
    struct timespec set[2] = {{1000000000, 0}, {1234567890, 0}};
    int explicit_ok = utimensat(AT_FDCWD, path, set, 0) == 0;
    struct stat s1;
    stat(path, &s1);
    int atime_ok = s1.st_atim.tv_sec == 1000000000;
    int mtime_ok = s1.st_mtim.tv_sec == 1234567890;

    // OMIT atime, set mtime to a new fixed value: atime must be unchanged.
    struct timespec omit[2] = {{0, UTIME_OMIT}, {1500000000, 0}};
    int omit_ok = utimensat(AT_FDCWD, path, omit, 0) == 0;
    struct stat s2;
    stat(path, &s2);
    int preserved = s2.st_atim.tv_sec == 1000000000;
    int updated = s2.st_mtim.tv_sec == 1500000000;

    // NOW on mtime advances it past the old fixed value.
    struct timespec now[2] = {{0, UTIME_OMIT}, {0, UTIME_NOW}};
    utimensat(AT_FDCWD, path, now, 0);
    struct stat s3;
    stat(path, &s3);
    int advanced = s3.st_mtim.tv_sec > 1500000000;

    unlink(path);
    rmdir(dir);
    printf("utimensat-times explicit=%d atime=%d mtime=%d omit=%d preserved=%d updated=%d now-advanced=%d\n",
           explicit_ok, atime_ok, mtime_ok, omit_ok, preserved, updated, advanced);
    return 0;
}
