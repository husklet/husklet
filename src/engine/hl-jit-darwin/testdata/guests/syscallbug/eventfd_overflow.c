// syscall-compat regression: an eventfd write that would overflow the counter must return EAGAIN and
// preserve the prior value (Linux caps at ULLONG_MAX-1), not wrap the counter to zero and lose it.
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/eventfd.h>
#include <unistd.h>

int main(void) {
    int fd = eventfd(0, EFD_NONBLOCK);
    uint64_t big = 0xfffffffffffffffeULL; // the maximum eventfd counter value
    ssize_t w1 = write(fd, &big, 8);
    uint64_t one = 1;
    ssize_t w2 = write(fd, &one, 8); // would overflow -> EAGAIN, counter unchanged
    int e2 = (w2 == -1) ? errno : 0;
    uint64_t got = 0;
    ssize_t r = read(fd, &got, 8);
    printf("w1=%zd w2=%zd e2=%d r=%zd got=%llu\n", w1, w2, e2, r, (unsigned long long)got);
    return 0;
}
