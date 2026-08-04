#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/uio.h>
#include <unistd.h>

#define LINUX_MAX_RW_COUNT UINT64_C(0x7ffff000)

int main(void) {
    int sink = open("/dev/null", O_WRONLY);
    if (sink < 0) return 2;
    unsigned char byte = 0x5a;
    struct iovec capped[2] = {
        {&byte, LINUX_MAX_RW_COUNT - 1},
        {&byte, 4096},
    };
    errno = 0;
    ssize_t capped_result = writev(sink, capped, 2);
    int capped_ok = capped_result == (ssize_t)LINUX_MAX_RW_COUNT;

    struct iovec signed_length = {&byte, (size_t)SSIZE_MAX + 1};
    errno = 0;
    ssize_t signed_result = writev(sink, &signed_length, 1);
    int signed_ok = signed_result == -1 && errno == EINVAL;

    dprintf(STDERR_FILENO, "vector-limits capped=%zd signed=%zd errno=%d\n",
            capped_result, signed_result, errno);
    close(sink);
    printf("vector-limits cap=%d signed=%d\n", capped_ok, signed_ok);
    return !(capped_ok && signed_ok);
}
