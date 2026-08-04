// A non-blocking localhost client must complete against a wildcard listener in the same private network.
// curl and other event-loop clients rely on connect(EINPROGRESS) becoming POLLOUT before their deadline.
#include "net_util.h"
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#define PORT 34568

int main(void) {
    net_watchdog(10);
    int listener = socket(AF_INET, SOCK_STREAM, 0);
    int one = 1;
    setsockopt(listener, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    struct sockaddr_in address = {.sin_family = AF_INET,
                                  .sin_port = htons(PORT),
                                  .sin_addr.s_addr = htonl(INADDR_ANY)};
    if (bind(listener, (struct sockaddr *)&address, sizeof address) < 0 || listen(listener, 8) < 0) return 1;

    pid_t child = fork();
    if (child == 0) {
        int client = socket(AF_INET, SOCK_STREAM, 0);
        fcntl(client, F_SETFL, fcntl(client, F_GETFL) | O_NONBLOCK);
        address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        errno = 0;
        int connected = connect(client, (struct sockaddr *)&address, sizeof address);
        int started = connected == 0 || errno == EINPROGRESS;
        struct pollfd event = {.fd = client, .events = POLLOUT};
        int ready = poll(&event, 1, 1000);
        int error = -1;
        socklen_t size = sizeof error;
        getsockopt(client, SOL_SOCKET, SO_ERROR, &error, &size);
        printf("lo_any_nonblock started=%d ready=%d writable=%d error=%s\n", started, ready,
               (event.revents & POLLOUT) != 0, err_name(error));
        fflush(stdout);
        _exit(started && ready == 1 && (event.revents & POLLOUT) && error == 0 ? 0 : 1);
    }
    int accepted = accept(listener, NULL, NULL);
    if (accepted >= 0) close(accepted);
    int status = 0;
    waitpid(child, &status, 0);
    close(listener);
    int client = socket(AF_INET, SOCK_STREAM, 0);
    fcntl(client, F_SETFL, fcntl(client, F_GETFL) | O_NONBLOCK);
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    errno = 0;
    int connected = connect(client, (struct sockaddr *)&address, sizeof address);
    int started = connected == 0 || errno == EINPROGRESS;
    struct pollfd event = {.fd = client, .events = POLLOUT};
    int ready = poll(&event, 1, 1000);
    int error = -1;
    socklen_t size = sizeof error;
    getsockopt(client, SOL_SOCKET, SO_ERROR, &error, &size);
    printf("lo_any_closed started=%d ready=%d error=%s\n", started, ready, err_name(error));
    close(client);
    return WIFEXITED(status) && WEXITSTATUS(status) == 0 && ready == 1 && error == ECONNREFUSED ? 0 : 1;
}
