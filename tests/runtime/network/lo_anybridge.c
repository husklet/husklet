// #228: a server that binds INADDR_ANY (0.0.0.0) must be reachable from 127.0.0.1 in the SAME container,
// including when a user-defined network (bridge) is attached. Under the private-loopback / per-network
// AF_UNIX switch, a 0.0.0.0 bind lands on the bridge rendezvous path (keyed by our own IP) while a
// 127.0.0.1 client dials the loopback path; the connect must fall back to the bridge path (with a fresh
// socket — a failed BSD connect poisons the fd). Server + client are separate processes (fork), exactly
// like redis-server / redis-cli. Golden: the client always gets the reply. Linux-only (the switch is a
// Linux-container feature; on a bare host this is plain loopback and also passes).
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#define PORT 34567

int main(void) {
    int srv = socket(AF_INET, SOCK_STREAM, 0);
    int on = 1;
    setsockopt(srv, SOL_SOCKET, SO_REUSEADDR, &on, sizeof on);
    struct sockaddr_in a;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons(PORT);
    a.sin_addr.s_addr = htonl(INADDR_ANY); // 0.0.0.0
    if (bind(srv, (struct sockaddr *)&a, sizeof a) < 0) { printf("lo_any bind=FAIL(%s)\n", strerror(errno)); return 1; }
    if (listen(srv, 8) < 0) { printf("lo_any listen=FAIL(%s)\n", strerror(errno)); return 1; }
    int collision = socket(AF_INET, SOCK_STREAM, 0);
    setsockopt(collision, SOL_SOCKET, SO_REUSEADDR, &on, sizeof on);
    if (bind(collision, (struct sockaddr *)&a, sizeof a) == 0 || errno != EADDRINUSE) {
        printf("lo_any collision=FAIL(%s)\n", strerror(errno));
        return 1;
    }
    close(collision);
    pid_t pid = fork();
    if (pid == 0) {
        usleep(200000); // let the parent reach accept()
        int cs = socket(AF_INET, SOCK_STREAM, 0);
        struct sockaddr_in d;
        memset(&d, 0, sizeof d);
        d.sin_family = AF_INET;
        d.sin_port = htons(PORT);
        d.sin_addr.s_addr = htonl(0x7f000001); // 127.0.0.1
        if (connect(cs, (struct sockaddr *)&d, sizeof d) < 0) {
            printf("lo_any reply=CONNFAIL(%s)\n", strerror(errno));
            fflush(stdout);
            _exit(2);
        }
        write(cs, "PING", 4);
        char b[16] = {0};
        ssize_t n = read(cs, b, 15);
        printf("lo_any reply=%.*s\n", (int)(n < 0 ? 0 : n), b);
        fflush(stdout);
        _exit(0);
    }
    int cc = accept(srv, NULL, NULL);
    char b[16] = {0};
    ssize_t n = read(cc, b, 15);
    (void)n;
    write(cc, "PONG", 4);
    close(cc);
    close(srv);
    waitpid(pid, NULL, 0);
    return 0;
}
