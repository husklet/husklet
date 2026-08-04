#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static int bound_route_errno(const char *dir) {
    char path[192];
    snprintf(path, sizeof path, "%s/hl_futimens_bound_%d", dir, (int)getpid());
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (fd < 0) return errno;
    struct timespec set[2] = {{1000000000, 0}, {1234567890, 0}};
    errno = 0;
    int status = futimens(fd, set);
    int failure = status == 0 ? 0 : errno;
    close(fd);
    unlink(path);
    return failure;
}

int main(void) {
    char path[128];
    snprintf(path, sizeof path, "/dev/shm/hl_futimens_nullpath_%d", (int)getpid());
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (fd < 0) return 1;
    struct timespec set[2] = {{1000000000, 0}, {1234567890, 0}};
    errno = 0;
    int explicit_ok = futimens(fd, set) == 0;
    int explicit_errno = explicit_ok ? 0 : errno;
    struct stat s1;
    fstat(fd, &s1);
    int atime_ok = s1.st_atim.tv_sec == 1000000000;
    int mtime_ok = s1.st_mtim.tv_sec == 1234567890;
    struct timespec omit[2] = {{0, UTIME_OMIT}, {1500000000, 0}};
    int omit_ok = futimens(fd, omit) == 0;
    struct stat s2;
    fstat(fd, &s2);
    int preserved = s2.st_atim.tv_sec == 1000000000;
    int updated = s2.st_mtim.tv_sec == 1500000000;
    int now_ok = futimens(fd, NULL) == 0;
    errno = 0;
    int bad = futimens(fd, (const struct timespec *)(uintptr_t)1) == -1 && errno == EFAULT;
    close(fd);
    unlink(path);
    printf("futimens-null-path explicit=%d errno=%d atime=%d mtime=%d omit=%d preserved=%d updated=%d "
           "now=%d badtimes=%d bound-errno=%d\n",
           explicit_ok, explicit_errno, atime_ok, mtime_ok, omit_ok, preserved, updated, now_ok, bad,
           bound_route_errno("/tmp"));
    return 0;
}
