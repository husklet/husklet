// #229: an AF_UNIX DATAGRAM sendto() to a NAMED dest must be routed the same way bind/connect route it.
// This exercises the abstract-namespace variant (sun_path[0]=='\0'), which macOS lacks and hl maps to a
// per-namespace filesystem socket: bind, connect AND sendto/sendmsg must all agree on that mapping or the
// datagram is dropped. The pathname variant (overlay-routed, e.g. syslog to /dev/log) shares the same code
// path and is exercised by the container scenarios. Linux-only (abstract ns is a Linux feature); diffed vs
// the native-Linux oracle. Covers both sendto (unconnected, explicit dest) and sendmsg (iovec + msg_name).
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

static socklen_t abs_addr(struct sockaddr_un *a, const char *name) {
    memset(a, 0, sizeof *a);
    a->sun_family = AF_UNIX;
    a->sun_path[0] = '\0'; // abstract
    size_t nl = strlen(name);
    memcpy(a->sun_path + 1, name, nl);
    return (socklen_t)(sizeof(a->sun_family) + 1 + nl);
}

int main(void) {
    struct sockaddr_un sa;
    socklen_t alen = abs_addr(&sa, "hl_dgram_abs");
    int srv = socket(AF_UNIX, SOCK_DGRAM, 0);
    if (bind(srv, (struct sockaddr *)&sa, alen) < 0) { printf("dgabs bind_failed\n"); return 0; }
    pid_t pid = fork();
    if (pid == 0) {
        usleep(120000);
        // (1) sendto with an explicit abstract dest
        int c1 = socket(AF_UNIX, SOCK_DGRAM, 0);
        sendto(c1, "AA", 2, 0, (struct sockaddr *)&sa, alen);
        close(c1);
        // (2) sendmsg with msg_name = the same abstract dest + an iovec
        int c2 = socket(AF_UNIX, SOCK_DGRAM, 0);
        struct iovec iov = {(void *)"BBB", 3};
        struct msghdr mh;
        memset(&mh, 0, sizeof mh);
        mh.msg_name = &sa;
        mh.msg_namelen = alen;
        mh.msg_iov = &iov;
        mh.msg_iovlen = 1;
        sendmsg(c2, &mh, 0);
        close(c2);
        _exit(0);
    }
    char b1[16] = {0}, b2[16] = {0};
    ssize_t n1 = recvfrom(srv, b1, 15, 0, NULL, NULL);
    ssize_t n2 = recvfrom(srv, b2, 15, 0, NULL, NULL);
    close(srv);
    waitpid(pid, NULL, 0);
    // Order between the two datagrams isn't guaranteed; report a sorted, deterministic summary.
    const char *first = strcmp(b1, b2) <= 0 ? b1 : b2;
    const char *second = strcmp(b1, b2) <= 0 ? b2 : b1;
    printf("dgabs n1=%ld n2=%ld got=%s,%s\n", (long)n1, (long)n2, first, second); // 2 3 AA,BBB
    return 0;
}
