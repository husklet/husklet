// listen() backlog with multiple queued connects: three clients connect before the
// server accepts any, then all three are accepted and each round-trips a distinct
// byte. Confirms the pending-connection queue delivers every client. -> oracle.
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
    listen(ls, 8);
    socklen_t al = sizeof a;
    getsockname(ls, (struct sockaddr *)&a, &al);

    int cli[3];
    for (int i = 0; i < 3; i++) {
        cli[i] = socket(AF_INET, SOCK_STREAM, 0);
        connect(cli[i], (struct sockaddr *)&a, sizeof a);
        char c = 'A' + i;
        send(cli[i], &c, 1, 0);
    }
    int total = 0;
    for (int i = 0; i < 3; i++) {
        int as = accept(ls, NULL, NULL);
        char c = 0;
        recv(as, &c, 1, 0);
        total += c;
        close(as);
    }
    // 'A'+'B'+'C' = 65+66+67 = 198, order-independent
    printf("accepted_sum=%d\n", total);
    for (int i = 0; i < 3; i++) close(cli[i]);
    close(ls);
    return 0;
}
