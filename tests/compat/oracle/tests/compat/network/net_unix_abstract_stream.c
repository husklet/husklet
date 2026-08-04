// Abstract-namespace AF_UNIX stream socket (leading NUL in sun_path): bind/listen/
// connect/accept with no filesystem entry, then round-trip a payload. Confirms the
// Linux abstract-address path works for stream sockets. Deterministic echo -> oracle.
#include "net_util.h"
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    int ls = socket(AF_UNIX, SOCK_STREAM, 0);
    struct sockaddr_un a = {0};
    a.sun_family = AF_UNIX;
    memcpy(a.sun_path, "\0hl-abs-stream", 14);
    socklen_t al = offsetof(struct sockaddr_un, sun_path) + 14;
    if (bind(ls, (struct sockaddr *)&a, al) < 0) { perror("bind"); return 1; }
    listen(ls, 4);

    pid_t pid = fork();
    if (pid == 0) {
        int cs = socket(AF_UNIX, SOCK_STREAM, 0);
        connect(cs, (struct sockaddr *)&a, al);
        send(cs, "abstract", 8, 0);
        char b[16];
        ssize_t n = recv(cs, b, sizeof b, 0);
        (void)n;
        close(cs);
        _exit(0);
    }
    int as = accept(ls, NULL, NULL);
    char b[16] = {0};
    ssize_t n = recv(as, b, sizeof b - 1, 0);
    b[n > 0 ? n : 0] = 0;
    send(as, "ack", 3, 0);
    int st = 0;
    waitpid(pid, &st, 0);
    printf("abstract_recv=%s len=%zd\n", b, n);
    close(as);
    close(ls);
    return 0;
}
