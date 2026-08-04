#include <errno.h>
#include <stdio.h>
#include <sys/socket.h>
#include <unistd.h>

static int rejected(int family, int type) {
    errno = 0;
    int descriptor = socket(family, type, 0);
    int result = descriptor < 0 && errno == ENOSYS;
    if (descriptor >= 0) close(descriptor);
    return result;
}

int main(void) {
    int stream4 = rejected(AF_INET, SOCK_STREAM);
    int datagram4 = rejected(AF_INET, SOCK_DGRAM);
    int stream6 = rejected(AF_INET6, SOCK_STREAM);
    printf("inet isolated stream4=%d datagram4=%d stream6=%d\n",
           stream4, datagram4, stream6);
    return !(stream4 && datagram4 && stream6);
}
