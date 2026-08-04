// Non-blocking connect to a refused port surfaces the failure asynchronously:
// connect returns EINPROGRESS, poll wakes on POLLERR/POLLOUT, and SO_ERROR reads back
// ECONNREFUSED. A bound-then-closed loopback port guarantees no listener. -> oracle.
#include "net_util.h"
#include <arpa/inet.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <stdio.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    // Reserve an ephemeral port, then close it so nothing listens there.
    int probe = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in a = {0};
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    bind(probe, (struct sockaddr *)&a, sizeof a);
    socklen_t al = sizeof a;
    getsockname(probe, (struct sockaddr *)&a, &al);
    close(probe);

    int cs = socket(AF_INET, SOCK_STREAM, 0);
    fcntl(cs, F_SETFL, fcntl(cs, F_GETFL) | O_NONBLOCK);
    errno = 0;
    int cr = connect(cs, (struct sockaddr *)&a, sizeof a);
    int started = cr == 0 || errno == EINPROGRESS || errno == ECONNREFUSED;

    struct pollfd pf = {cs, POLLOUT, 0};
    int pr = poll(&pf, 1, 2000);
    int soerr = -1;
    socklen_t sl = sizeof soerr;
    getsockopt(cs, SOL_SOCKET, SO_ERROR, &soerr, &sl);
    printf("started=%d poll=%d soerr=%s\n", started, pr > 0, err_name(soerr));
    close(cs);
    return 0;
}
