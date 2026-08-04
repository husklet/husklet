#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/uio.h>
#include <unistd.h>

int main(void) {
    char path[128];
    snprintf(path, sizeof(path), "/mnt/hl-bound-uaccess-%ld", (long)getpid());
    int fd = open(path, O_CREAT | O_TRUNC | O_RDWR, 0600);
    if (fd < 0) return 2;

    char first[] = "abc";
    char second[] = "DEF";
    struct iovec output[2] = {{first, 3}, {second, 3}};
    int write_ok = writev(fd, output, 2) == 6 && pwrite(fd, "xy", 2, 2) == 2;

    char left[4] = {0}, right[4] = {0};
    struct iovec input[2] = {{left, 3}, {right, 3}};
    int read_ok = preadv(fd, input, 2, 0) == 6 && memcmp(left, "abx", 3) == 0 && memcmp(right, "yEF", 3) == 0;

    int available = -1;
    int ioctl_ok = lseek(fd, 1, SEEK_SET) == 1 && ioctl(fd, FIONREAD, &available) == 0 && available == 5;

    void *guard = mmap(NULL, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (guard == MAP_FAILED) return 3;
    errno = 0;
    int fault_read = pread(fd, guard, 1, 0) == -1 && errno == EFAULT;
    errno = 0;
    int fault_write = pwrite(fd, guard, 1, 0) == -1 && errno == EFAULT;
    int fault_ok = fault_read && fault_write;

    close(fd);
    unlink(path);
    printf("bound-uaccess write=%d read=%d ioctl=%d fault=%d\n", write_ok, read_ok, ioctl_ok, fault_ok);
    return !(write_ok && read_ok && ioctl_ok && fault_ok);
}
