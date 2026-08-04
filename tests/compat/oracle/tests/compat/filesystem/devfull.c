// /dev/full (Linux special device): reads yield an endless stream of zero bytes, but EVERY write fails
// with ENOSPC ("No space left on device") -- installers/test-suites probe it to check out-of-space paths.
// Linux-only: macOS has no /dev/full, so the container fs synthesizes it.
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    int fd = open("/dev/full", O_RDWR);
    int opened = fd >= 0;
    unsigned char z[8];
    memset(z, 0xff, sizeof z);
    ssize_t rd = read(fd, z, sizeof z);
    int zeros = rd == (ssize_t)sizeof z;
    for (size_t i = 0; i < sizeof z; i++)
        if (z[i]) zeros = 0;
    errno = 0;
    ssize_t wr = write(fd, "x", 1);
    int enospc = wr < 0 && errno == ENOSPC;
    int saved = dup(STDOUT_FILENO);
    if (saved < 0 || dup2(fd, STDOUT_FILENO) < 0) {
        enospc = 0;
    } else {
        errno = 0;
        wr = write(STDOUT_FILENO, "x", 1);
        enospc &= wr < 0 && errno == ENOSPC;
        if (dup2(saved, STDOUT_FILENO) < 0) enospc = 0;
    }
    if (saved >= 0) close(saved);
    close(fd);
    printf("devfull opened=%d zeros=%d enospc=%d\n", opened, zeros, enospc);
    return 0;
}
