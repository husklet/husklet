// MSG_PEEK on a stream socket returns data without consuming it: a peek reads the
// bytes, a following plain recv returns the same bytes again, and only then is the
// queue drained. Confirms non-destructive peek semantics. Deterministic -> oracle.
#include "net_util.h"
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    int sv[2];
    socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
    write(sv[0], "peekme", 6);
    char p[8] = {0}, r[8] = {0};
    ssize_t pn = recv(sv[1], p, sizeof p - 1, MSG_PEEK);
    p[pn > 0 ? pn : 0] = 0;
    ssize_t rn = recv(sv[1], r, sizeof r - 1, 0);
    r[rn > 0 ? rn : 0] = 0;
    // second consuming recv would block; use MSG_DONTWAIT to confirm queue is empty
    errno = 0;
    ssize_t en = recv(sv[1], r, sizeof r, MSG_DONTWAIT);
    printf("peek=%s peek_len=%zd recv=%s recv_len=%zd empty_errno=%s empty_ret=%zd\n", p, pn, r, rn,
           err_name(errno), en);
    close(sv[0]);
    close(sv[1]);
    return 0;
}
