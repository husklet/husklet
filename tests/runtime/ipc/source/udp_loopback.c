// Loopback UDP: bound receiver, sendto/recvfrom preserving the datagram, and MSG_DONTWAIT EAGAIN
// when the socket is drained.
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
#include <errno.h>
int main(void){
    int rs = socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in a; memset(&a, 0, sizeof a);
    a.sin_family = AF_INET; a.sin_addr.s_addr = htonl(INADDR_LOOPBACK); a.sin_port = 0;
    bind(rs, (struct sockaddr *)&a, sizeof a);
    socklen_t alen = sizeof a; getsockname(rs, (struct sockaddr *)&a, &alen);
    int ss = socket(AF_INET, SOCK_DGRAM, 0);
    sendto(ss, "datagram", 8, 0, (struct sockaddr *)&a, sizeof a);
    char b[16] = {0}; ssize_t n = recv(rs, b, sizeof b, 0);
    errno = 0;
    ssize_t empty = recv(rs, b, sizeof b, MSG_DONTWAIT); int e = errno;
    printf("udp recv=%zd data=%.8s empty=%zd eagain=%d\n",
           n, b, empty, e == EAGAIN);   // 8 datagram -1 1
    return 0;
}
