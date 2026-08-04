#define _GNU_SOURCE
// Loopback TCP round trip: SO_REUSEADDR bind, listen, accept4, connect, and a byte transfer.
// Port numbers are kernel-assigned and never printed, keeping output arch/run-neutral.
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
int main(void){
    int ls = socket(AF_INET, SOCK_STREAM, 0);
    int on = 1; setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, &on, sizeof on);
    struct sockaddr_in a; memset(&a, 0, sizeof a);
    a.sin_family = AF_INET; a.sin_addr.s_addr = htonl(INADDR_LOOPBACK); a.sin_port = 0;
    bind(ls, (struct sockaddr *)&a, sizeof a);
    listen(ls, 1);
    socklen_t alen = sizeof a;
    getsockname(ls, (struct sockaddr *)&a, &alen);
    int cs = socket(AF_INET, SOCK_STREAM, 0);
    int c = connect(cs, (struct sockaddr *)&a, sizeof a);
    int as = accept4(ls, NULL, NULL, SOCK_CLOEXEC);
    write(cs, "ping", 4);
    char b[4] = {0}; ssize_t n = read(as, b, 4);
    printf("tcp connect=%d accept_ok=%d recv=%zd data=%.4s\n", c, as >= 0, n, b);  // 0 1 4 ping
    return 0;
}
