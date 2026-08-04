// AF_UNIX SOCK_SEQPACKET over an abstract-namespace address: message boundaries are
// preserved (unlike a stream) so two sends arrive as two distinct records with their
// exact lengths. Uses socketpair-free listen/accept/connect. Deterministic -> oracle.
#include "net_util.h"
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    int ls = socket(AF_UNIX, SOCK_SEQPACKET, 0);
    if (ls < 0) { printf("no_seqpacket\n"); return 0; }
    struct sockaddr_un a = {0};
    a.sun_family = AF_UNIX;
    memcpy(a.sun_path, "\0hl-seqpkt", 10); // abstract
    socklen_t al = offsetof(struct sockaddr_un, sun_path) + 10;
    bind(ls, (struct sockaddr *)&a, al);
    listen(ls, 4);

    pid_t pid = fork();
    if (pid == 0) {
        int cs = socket(AF_UNIX, SOCK_SEQPACKET, 0);
        connect(cs, (struct sockaddr *)&a, al);
        send(cs, "alpha", 5, 0);
        send(cs, "bt", 2, 0);
        close(cs);
        _exit(0);
    }
    int as = accept(ls, NULL, NULL);
    char b[32];
    ssize_t n1 = recv(as, b, sizeof b, 0);
    ssize_t n2 = recv(as, b, sizeof b, 0);
    int st = 0;
    waitpid(pid, &st, 0);
    printf("rec1=%zd rec2=%zd\n", n1, n2);
    close(as);
    close(ls);
    return 0;
}
