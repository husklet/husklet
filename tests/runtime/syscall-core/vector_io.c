#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/uio.h>
#include <unistd.h>

int main(void) {
    const char *path = "/tmp/hl-vector-io";
    int fd = open(path, O_CREAT | O_TRUNC | O_RDWR, 0600);
    struct iovec output[2] = {{(void *)"vector-", 7}, {(void *)"payload", 7}};
    long written = pwritev2(fd, output, 2, 3, 0);
    char first[8] = {0}, second[8] = {0};
    struct iovec input[2] = {{first, 7}, {second, 7}};
    long read_count = preadv2(fd, input, 2, 3, 0);
    int vector_ok = written == 14 && read_count == 14 && !strcmp(first, "vector-") && !strcmp(second, "payload");

    lseek(fd, 3, SEEK_SET);
    char current[7] = {0};
    struct iovec current_iov = {current, 6};
    long current_count = preadv2(fd, &current_iov, 1, -1, 0);
    int current_ok = current_count == 6 && !memcmp(current, "vector", 6) && lseek(fd, 0, SEEK_CUR) == 9;

    errno = 0;
    long flagged = preadv2(fd, &current_iov, 1, 0, (int)UINT32_C(0x80000000));
    int flag_errno = errno;
    int flags_ok = flagged == -1 && flag_errno == EOPNOTSUPP;
    int advisory_ok = readahead(fd, 0, 4096) == 0;
    errno = 0;
    int badfd_ok = readahead(-1, 0, 1) == -1 && errno == EBADF;

    printf("vector-io vectors=%d current=%d flags=%d errno=%d advisory=%d badfd=%d\n", vector_ok, current_ok,
           flags_ok, flag_errno, advisory_ok, badfd_ok);
    close(fd);
    unlink(path);
    return vector_ok && current_ok && flags_ok && advisory_ok && badfd_ok ? 0 : 2;
}
