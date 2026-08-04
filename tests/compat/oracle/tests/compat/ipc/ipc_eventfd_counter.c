// Non-semaphore eventfd accumulates writes; a single read returns and clears the whole sum.
#include <sys/eventfd.h>
#include <unistd.h>
#include <stdint.h>
#include <stdio.h>
#include <errno.h>
#include <string.h>
int main(void){
    int fd = eventfd(0, EFD_NONBLOCK);
    uint64_t a = 10, b = 32;
    write(fd, &a, 8); write(fd, &b, 8);
    uint64_t v = 0; ssize_t n = read(fd, &v, 8);
    // second read: counter is 0 -> EAGAIN
    uint64_t v2; ssize_t n2 = read(fd, &v2, 8); int e2 = errno;
    printf("counter read=%zd sum=%lu\n", n, (unsigned long)v);    // 8 42
    printf("counter empty=%zd errno=%s\n", n2, strerror(e2));     // -1 EAGAIN
    return 0;
}
