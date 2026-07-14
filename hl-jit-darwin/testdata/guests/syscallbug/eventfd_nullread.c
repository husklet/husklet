// syscall-compat regression: read(eventfd, NULL, 8) must return EFAULT, not a fake 8-byte success.
// (Only the errno is asserted: Linux consumes-then-faults while hl faults-before-consuming; both agree
// on the EFAULT return, which is what this finding is about.)
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <sys/eventfd.h>
#include <unistd.h>

int main(void) {
    int fd = eventfd(5, 0);
    ssize_t bad = read(fd, NULL, 8);
    printf("nullread=%zd errno=%d\n", bad, (bad == -1) ? errno : 0);
    return 0;
}
