#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    int fd = open("/data", O_RDWR);
    char zero = 0;
    if (fd < 0 || ftruncate(fd, 4096) != 0 || pwrite(fd, &zero, 1, 0) != 1) return 1;
    void *reservation = mmap(NULL, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (reservation == MAP_FAILED) return 2;
    char *mapped = mmap(reservation, 4096, PROT_READ, MAP_PRIVATE | MAP_FIXED, fd, 0);
    if (mapped == MAP_FAILED || mapped != reservation || mapped[0] != 0) return 3;
    struct stat status;
    int ok = fstatat(fd, mapped, &status, AT_EMPTY_PATH) == 0 && S_ISREG(status.st_mode);
    printf("fixed-file-protection empty-path=%d\n", ok);
    munmap(mapped, 4096);
    close(fd);
    return ok ? 0 : 4;
}
