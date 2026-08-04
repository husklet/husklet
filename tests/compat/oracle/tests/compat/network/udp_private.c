#include <arpa/inet.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    int server = socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in address = {.sin_family = AF_INET, .sin_port = 0};
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (bind(server, (struct sockaddr *)&address, sizeof address) != 0) return 1;
    socklen_t address_length = sizeof address;
    if (getsockname(server, (struct sockaddr *)&address, &address_length) != 0 || !ntohs(address.sin_port)) return 2;

    pid_t child = fork();
    if (child == 0) {
        struct pollfd ready = {.fd = server, .events = POLLIN};
        if (poll(&ready, 1, 3000) != 1) _exit(10);
        char bytes[32] = {0};
        struct iovec iov = {.iov_base = bytes, .iov_len = sizeof bytes};
        struct sockaddr_in peer = {0};
        struct msghdr message = {.msg_name = &peer, .msg_namelen = sizeof peer, .msg_iov = &iov, .msg_iovlen = 1};
        ssize_t count = recvmsg(server, &message, 0);
        if (count != 7 || peer.sin_family != AF_INET || !ntohs(peer.sin_port)) _exit(11);
        iov.iov_len = (size_t)count;
        if (sendmsg(server, &message, 0) != count) _exit(12);
        close(server);
        _exit(0);
    }

    int alias = dup(server);
    close(server);
    close(alias); /* child still owns the bound OFD/path */
    int client = socket(AF_INET, SOCK_DGRAM, 0);
    if (connect(client, (struct sockaddr *)&address, sizeof address) != 0) return 3;
    /* BusyBox nc and other connected-UDP consumers use write(2), not sendto(2). */
    if (write(client, "private", 7) != 7) return 4;
    char reply[16] = {0};
    if (recv(client, reply, sizeof reply, 0) != 7 || strcmp(reply, "private")) return 5;
    int status = 0;
    waitpid(child, &status, 0);
    if (!WIFEXITED(status) || WEXITSTATUS(status)) return 6;
    puts("udp private connected-write sendmsg poll dup fork ok");
    return 0;
}
