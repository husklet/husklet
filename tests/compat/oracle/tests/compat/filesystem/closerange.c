// close_range(2) (Linux 5.9+): bulk-close a contiguous fd range. Linux-only -> native oracle.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
    char path[128];
    snprintf(path, sizeof path, "/tmp/hl_closerange_%d", (int)getpid());
    int lo = -1, hi = -1;
    for (int i = 0; i < 8; i++) {
        int fd = open(path, O_CREAT | O_RDWR, 0644);
        if (fd < 0) continue;
        if (lo < 0) lo = fd;
        hi = fd;
    }
    // still open just before the bulk close
    int open_before = fcntl(hi, F_GETFD) != -1;
    int rc = close_range(lo, hi, 0);
    int closed = fcntl(hi, F_GETFD) == -1 && fcntl(lo, F_GETFD) == -1;
    unlink(path);
    printf("close_range rc=%d open_before=%d closed=%d\n", rc, open_before, closed);
    return 0;
}
