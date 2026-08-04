// Non-blocking connect to a live loopback listener: connect returns immediately with
// EINPROGRESS (or 0), poll reports POLLOUT once writable, and SO_ERROR reads back 0 to
// confirm success. Then a payload round-trips. Bounded poll timeout -> oracle.
#include "net_util.h"
#include <arpa/inet.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    int ls = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in a = {0};
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    bind(ls, (struct sockaddr *)&a, sizeof a);
    listen(ls, 4);
    socklen_t al = sizeof a;
    getsockname(ls, (struct sockaddr *)&a, &al);

    int cs = socket(AF_INET, SOCK_STREAM, 0);
    fcntl(cs, F_SETFL, fcntl(cs, F_GETFL) | O_NONBLOCK);
    errno = 0;
    int cr = connect(cs, (struct sockaddr *)&a, sizeof a);
    int in_progress = cr == 0 || errno == EINPROGRESS;

    struct pollfd pf = {cs, POLLOUT, 0};
    int pr = poll(&pf, 1, 2000);
    int soerr = -1;
    socklen_t sl = sizeof soerr;
    getsockopt(cs, SOL_SOCKET, SO_ERROR, &soerr, &sl);

    int as = accept(ls, NULL, NULL);
    send(cs, "ok", 2, 0);
    char b[8] = {0};
    ssize_t n = recv(as, b, sizeof b - 1, 0);
    b[n > 0 ? n : 0] = 0;
    printf("inprogress=%d poll=%d pollout=%d soerr=%s got=%s\n", in_progress, pr,
           (pf.revents & POLLOUT) != 0, err_name(soerr), b);
    close(as);
    close(cs);
    close(ls);
    return 0;
}
