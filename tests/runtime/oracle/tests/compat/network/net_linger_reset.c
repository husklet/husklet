// SO_LINGER with l_onoff=1, l_linger=0 forces an abortive close (RST) instead of a
// graceful FIN: the peer's next recv fails with ECONNRESET rather than returning EOF.
// Uses loopback TCP with data left unread to trigger the reset. Deterministic errno.
#include "net_util.h"
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    int ls = socket(AF_INET, SOCK_STREAM, 0);
    int one = 1;
    setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    struct sockaddr_in a = {0};
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    bind(ls, (struct sockaddr *)&a, sizeof a);
    listen(ls, 1);
    socklen_t al = sizeof a;
    getsockname(ls, (struct sockaddr *)&a, &al);

    pid_t pid = fork();
    if (pid == 0) {
        int cs = socket(AF_INET, SOCK_STREAM, 0);
        connect(cs, (struct sockaddr *)&a, sizeof a);
        struct linger lg = {1, 0};
        setsockopt(cs, SOL_SOCKET, SO_LINGER, &lg, sizeof lg);
        send(cs, "data", 4, 0);
        close(cs); // abortive -> RST
        _exit(0);
    }
    int as = accept(ls, NULL, NULL);
    // let the child send and abortively close before we read
    char b[16];
    ssize_t n;
    int saw_reset = 0, saw_eof = 0;
    for (int i = 0; i < 5; i++) {
        errno = 0;
        n = recv(as, b, sizeof b, 0);
        if (n < 0 && errno == ECONNRESET) { saw_reset = 1; break; }
        if (n == 0) { saw_eof = 1; break; }
        // n > 0: got the "data", keep reading until reset/eof
    }
    int st = 0;
    waitpid(pid, &st, 0);
    printf("reset=%d eof=%d\n", saw_reset, saw_eof);
    close(as);
    close(ls);
    return 0;
}
