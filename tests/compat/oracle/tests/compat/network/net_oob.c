// TCP urgent (out-of-band) data: the sender marks one byte with MSG_OOB, then sends a
// normal trailing byte as a sync marker. With default (out-of-line) delivery the
// receiver sees POLLPRI, retrieves the urgent byte via recv(MSG_OOB), and reads the
// normal stream separately. Deterministic byte content -> oracle.
#include "net_util.h"
#include <arpa/inet.h>
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
        int cs = accept(ls, NULL, NULL);
        // wait for urgent notification
        struct pollfd pf = {cs, POLLPRI, 0};
        poll(&pf, 1, 2000);
        char oob = 0;
        ssize_t on = recv(cs, &oob, 1, MSG_OOB);
        // drain the normal byte
        char norm = 0;
        recv(cs, &norm, 1, 0);
        printf("oob_len=%zd oob=%c normal=%c pollpri=%d\n", on, oob ? oob : '?', norm ? norm : '?',
               (pf.revents & POLLPRI) != 0);
        fflush(stdout);
        close(cs);
        _exit(0);
    }
    int cs = socket(AF_INET, SOCK_STREAM, 0);
    connect(cs, (struct sockaddr *)&a, sizeof a);
    send(cs, "Z", 1, MSG_OOB); // urgent
    send(cs, "k", 1, 0);       // normal sync marker
    int st = 0;
    waitpid(pid, &st, 0);
    close(cs);
    close(ls);
    return 0;
}
