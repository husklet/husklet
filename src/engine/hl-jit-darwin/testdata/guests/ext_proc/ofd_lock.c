#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/syscall.h>

int main(void) {
    char path[] = "/tmp/hl-ofd-lock-XXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) return 2;

    struct flock lock = {
        .l_type = F_WRLCK,
        .l_whence = SEEK_SET,
        .l_start = 0,
        .l_len = 0,
    };
    int set = fcntl(fd, F_OFD_SETLKW, &lock) == 0;

    lock.l_type = F_UNLCK;
    int unlock = fcntl(fd, F_OFD_SETLK, &lock) == 0;

    lock.l_type = F_WRLCK;
    int query = fcntl(fd, F_OFD_GETLK, &lock) == 0 && lock.l_type == F_UNLCK;

    int empty_path = syscall(__NR_fchmodat2, fd, "", 0600, AT_EMPTY_PATH) == 0;

    close(fd);
    unlink(path);
    printf("ofd_lock set=%d unlock=%d query=%d empty_path=%d\n", set, unlock, query, empty_path);
    return set && unlock && query && empty_path ? 0 : 1;
}
