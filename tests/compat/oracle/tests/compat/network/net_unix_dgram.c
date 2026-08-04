// AF_UNIX datagram sockets bound to filesystem paths: client sendto's the server's path, server
// echoes back to the client's bound path. Verifies named-unix dgram addressing. Portable, golden.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

static void fail(const char *operation) {
    perror(operation);
    exit(1);
}

static void timeout(int descriptor) {
    struct timeval value = {.tv_sec = 5};
    if (setsockopt(descriptor, SOL_SOCKET, SO_RCVTIMEO, &value, sizeof value) < 0)
        fail("setsockopt(SO_RCVTIMEO)");
}

int main(void) {
    char sp[sizeof(((struct sockaddr_un *)0)->sun_path)];
    char cp[sizeof(((struct sockaddr_un *)0)->sun_path)];
    if (snprintf(sp, sizeof sp, "/tmp/hl_unix_dg_srv_%ld.sock", (long)getpid()) >= (int)sizeof sp ||
        snprintf(cp, sizeof cp, "/tmp/hl_unix_dg_cli_%ld.sock", (long)getpid()) >= (int)sizeof cp)
        return 1;
    if ((unlink(sp) < 0 && access(sp, F_OK) == 0) || (unlink(cp) < 0 && access(cp, F_OK) == 0))
        fail("unlink");
    int srv = socket(AF_UNIX, SOCK_DGRAM, 0);
    if (srv < 0) fail("socket(server)");
    timeout(srv);
    struct sockaddr_un sa = {0}; sa.sun_family = AF_UNIX; strcpy(sa.sun_path, sp);
    if (bind(srv, (struct sockaddr *)&sa, sizeof sa) < 0) fail("bind(server)");
    pid_t pid = fork();
    if (pid < 0) fail("fork");
    if (pid == 0) {
        char buf[64]; struct sockaddr_un from; socklen_t fl = sizeof from;
        ssize_t n = recvfrom(srv, buf, 64, 0, (struct sockaddr *)&from, &fl);
        if (n < 0) fail("recvfrom(server)");
        if (sendto(srv, buf, (size_t)n, 0, (struct sockaddr *)&from, fl) != n)
            fail("sendto(server)");
        _exit(0);
    }
    int cl = socket(AF_UNIX, SOCK_DGRAM, 0);
    if (cl < 0) fail("socket(client)");
    timeout(cl);
    struct sockaddr_un ca = {0}; ca.sun_family = AF_UNIX; strcpy(ca.sun_path, cp);
    if (bind(cl, (struct sockaddr *)&ca, sizeof ca) < 0) fail("bind(client)");
    if (sendto(cl, "dgram-unix", 10, 0, (struct sockaddr *)&sa, sizeof sa) != 10)
        fail("sendto(client)");
    char buf[64] = {0};
    if (recvfrom(cl, buf, 63, 0, 0, 0) != 10) fail("recvfrom(client)");
    int status = 0;
    if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status) || WEXITSTATUS(status) != 0)
        return 1;
    if (unlink(sp) < 0 || unlink(cp) < 0) fail("unlink(cleanup)");
    printf("unix_dgram reply=%s\n", buf); // dgram-unix
    return 0;
}
