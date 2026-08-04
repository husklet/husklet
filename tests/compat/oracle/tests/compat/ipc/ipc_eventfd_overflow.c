// Writing the maximum (0xfffffffffffffffe) leaves the counter one below overflow; a further
// write that would overflow blocks, so EFD_NONBLOCK reports EAGAIN. Writing UINT64_MAX is EINVAL.
#include <sys/eventfd.h>
#include <unistd.h>
#include <stdint.h>
#include <stdio.h>
#include <errno.h>
#include <string.h>
int main(void){
    int fd = eventfd(0, EFD_NONBLOCK);
    uint64_t max = 0xfffffffffffffffeULL;
    ssize_t big = write(fd, &max, 8);
    uint64_t one = 1;
    errno = 0;
    ssize_t of = write(fd, &one, 8); int e_of = errno;
    uint64_t bad = 0xffffffffffffffffULL;
    // drain first so the invalid-value check is isolated
    uint64_t drain; read(fd, &drain, 8);
    errno = 0;
    ssize_t inv = write(fd, &bad, 8); int e_inv = errno;
    printf("overflow maxwrite=%zd\n", big);                       // 8
    printf("overflow blocked=%zd errno=%s\n", of, strerror(e_of));// -1 EAGAIN
    printf("overflow invalid=%zd errno=%s\n", inv, strerror(e_inv));// -1 EINVAL
    return 0;
}
