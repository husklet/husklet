#include <sys/socket.h>
#include <unistd.h>

int main(void) {
    int sockets[2];
    char byte;
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sockets) != 0) { return 20; }
    return read(sockets[0], &byte, sizeof(byte)) < 0 ? 21 : 22;
}
