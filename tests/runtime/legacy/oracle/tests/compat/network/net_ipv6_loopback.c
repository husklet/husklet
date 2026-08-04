// IPv6 ::1 loopback TCP echo: bind/listen/accept/connect over AF_INET6, round-trip
// a payload, and confirm getsockname reports the loopback address. Exercises the
// IPv6 sockaddr path independent of any external network. Deterministic -> oracle.
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
    int ls = socket(AF_INET6, SOCK_STREAM, 0);
    if (ls < 0) { printf("no_ipv6\n"); return 0; }
    int one = 1;
    setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    struct sockaddr_in6 a = {0};
    a.sin6_family = AF_INET6;
    a.sin6_addr = in6addr_loopback;
    if (bind(ls, (struct sockaddr *)&a, sizeof a) < 0) { perror("bind"); return 1; }
    listen(ls, 4);
    socklen_t al = sizeof a;
    getsockname(ls, (struct sockaddr *)&a, &al);
    int is_lo = IN6_IS_ADDR_LOOPBACK(&a.sin6_addr);

    pid_t pid = fork();
    if (pid == 0) {
        int cs = accept(ls, NULL, NULL);
        char b[32];
        ssize_t n = recv(cs, b, sizeof b, 0);
        send(cs, b, n, 0);
        close(cs);
        _exit(0);
    }
    int cs = socket(AF_INET6, SOCK_STREAM, 0);
    connect(cs, (struct sockaddr *)&a, sizeof a);
    send(cs, "v6ping", 6, 0);
    char b[32] = {0};
    ssize_t n = recv(cs, b, sizeof b - 1, 0);
    b[n > 0 ? n : 0] = 0;
    int st = 0;
    waitpid(pid, &st, 0);
    printf("v6 loopback=%d echo=%s\n", is_lo, b);
    close(cs);
    close(ls);
    return 0;
}
