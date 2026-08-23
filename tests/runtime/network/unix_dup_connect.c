#include "socket_util.h"
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

static int unnamed(int descriptor, int peer) {
    struct sockaddr_un address;
    memset(&address, 0, sizeof address);
    socklen_t length = sizeof address;
    int status = peer ? getpeername(descriptor, (struct sockaddr *)&address, &length)
                      : getsockname(descriptor, (struct sockaddr *)&address, &length);
    return status == 0 && address.sun_family == AF_UNIX && length == sizeof(sa_family_t);
}

static int named(int descriptor, const char *path) {
    struct sockaddr_un address;
    memset(&address, 0, sizeof address);
    socklen_t length = sizeof address;
    return getpeername(descriptor, (struct sockaddr *)&address, &length) == 0 && address.sun_family == AF_UNIX &&
           strcmp(address.sun_path, path) == 0;
}

int main(void) {
    net_watchdog(20);
    char path[sizeof(((struct sockaddr_un *)0)->sun_path)];
    snprintf(path, sizeof path, "/tmp/hl-unix-dup-%ld", (long)getpid());
    unlink(path);

    int listener = socket(AF_UNIX, SOCK_STREAM, 0);
    struct sockaddr_un address;
    memset(&address, 0, sizeof address);
    address.sun_family = AF_UNIX;
    snprintf(address.sun_path, sizeof address.sun_path, "%s", path);
    int bound = bind(listener, (struct sockaddr *)&address, sizeof address) == 0 && listen(listener, 4) == 0;

    int client = socket(AF_UNIX, SOCK_STREAM, 0);
    int alias = dup(client);
    int connected = connect(client, (struct sockaddr *)&address, sizeof address) == 0;
    int accepted = accept(listener, NULL, NULL);
    int local_unnamed = unnamed(alias, 0);
    int peer_named = named(alias, path);
    int accepted_peer_unnamed = accepted >= 0 && unnamed(accepted, 1);

    char sent = 'x', received = 0;
    int data = write(alias, &sent, 1) == 1 && accepted >= 0 && read(accepted, &received, 1) == 1 && received == sent;
    printf("bound=%d connected=%d local-unnamed=%d peer-named=%d accepted-peer-unnamed=%d data=%d\n", bound, connected,
           local_unnamed, peer_named, accepted_peer_unnamed, data);

    if (accepted >= 0) close(accepted);
    close(alias);
    close(client);
    close(listener);
    unlink(path);
    return !(bound && connected && local_unnamed && peer_named && accepted_peer_unnamed && data);
}
