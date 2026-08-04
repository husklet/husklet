// poll readiness matrix: a readable pipe reports POLLIN, a writable pipe reports POLLOUT, and a
// negative fd entry is ignored with zero revents.
#include <unistd.h>
#include <poll.h>
#include <stdio.h>
int main(void){
    int p[2];
    if (pipe(p)) return 1;
    write(p[1], "z", 1);
    struct pollfd pf[3] = {
        { .fd = p[0], .events = POLLIN },
        { .fd = p[1], .events = POLLOUT },
        { .fd = -1,   .events = POLLIN },
    };
    int n = poll(pf, 3, 100);
    printf("poll n=%d in=%d out=%d neg_revents=%d\n",
           n, (pf[0].revents & POLLIN) != 0, (pf[1].revents & POLLOUT) != 0, pf[2].revents);
    return 0;
}
