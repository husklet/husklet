// epoll interest follows the OPEN FILE DESCRIPTION, not the fd number. Register a pipe read end, then
// close it while a dup keeps the same OFD alive: Linux KEEPS the registration (readiness must persist),
// returning the original udata. hl's kqueue knote used to die with the fd number, losing readiness.
// Deterministic -> oracle-checked against native Linux.
#include <stdio.h>
#include <sys/epoll.h>
#include <unistd.h>

int main(void) {
    int fds[2];
    if (pipe(fds) < 0) { perror("pipe"); return 1; }
    int rfd = fds[0], wfd = fds[1];
    int rdup = dup(rfd); // a 2nd fd referring to the SAME open file description

    int ep = epoll_create1(0);
    struct epoll_event ev = {.events = EPOLLIN, .data.u64 = 0xABCDULL};
    epoll_ctl(ep, EPOLL_CTL_ADD, rfd, &ev);

    close(rfd); // the watched fd NUMBER is gone, but rdup keeps the OFD (pipe) open
    if (write(wfd, "Q", 1) < 0) { perror("write"); return 1; }

    struct epoll_event out[4];
    int n = epoll_wait(ep, out, 4, 1000);
    int wait = (n >= 1);
    int udata_ok = (n >= 1 && out[0].data.u64 == 0xABCDULL);
    char ch = 0;
    int rd = (n >= 1 && read(rdup, &ch, 1) == 1);
    printf("epoll_dup wait=%d udata_ok=%d read=%d char=%c\n", wait, udata_ok, rd, ch ? ch : '-');
    close(rdup);
    close(wfd);
    close(ep);
    return 0;
}
