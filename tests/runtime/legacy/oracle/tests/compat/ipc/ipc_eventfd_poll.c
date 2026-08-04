// eventfd readiness through poll: writable when it can accept a value, readable once nonzero.
#include <sys/eventfd.h>
#include <poll.h>
#include <unistd.h>
#include <stdint.h>
#include <stdio.h>
int main(void){
    int fd = eventfd(0, EFD_NONBLOCK);
    struct pollfd pf = { .fd = fd, .events = POLLIN | POLLOUT };
    poll(&pf, 1, 0);
    int in0 = (pf.revents & POLLIN) != 0, out0 = (pf.revents & POLLOUT) != 0;
    uint64_t v = 5; write(fd, &v, 8);
    pf.revents = 0; poll(&pf, 1, 0);
    int in1 = (pf.revents & POLLIN) != 0, out1 = (pf.revents & POLLOUT) != 0;
    printf("poll empty in=%d out=%d\n", in0, out0);   // 0 1
    printf("poll nonzero in=%d out=%d\n", in1, out1); // 1 1
    return 0;
}
