// SO_RCVTIMEO bounds a blocking recv: with no data, recv returns -1/EAGAIN after the timeout
// rather than blocking forever.
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
#include <errno.h>
#include <sys/time.h>
int main(void){
    int ls = socket(AF_INET, SOCK_STREAM, 0);
    int on = 1; setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, &on, sizeof on);
    struct sockaddr_in a; memset(&a, 0, sizeof a);
    a.sin_family = AF_INET; a.sin_addr.s_addr = htonl(INADDR_LOOPBACK); a.sin_port = 0;
    bind(ls, (struct sockaddr *)&a, sizeof a);
    listen(ls, 1);
    socklen_t alen = sizeof a; getsockname(ls, (struct sockaddr *)&a, &alen);
    int cs = socket(AF_INET, SOCK_STREAM, 0);
    connect(cs, (struct sockaddr *)&a, sizeof a);
    int as = accept(ls, NULL, NULL);
    struct timeval tv = {0, 50*1000};
    setsockopt(as, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv);
    char b[8]; errno = 0;
    ssize_t n = recv(as, b, sizeof b, 0); int e = errno;
    printf("rcvtimeo recv=%zd timed_out=%d\n", n, n == -1 && (e == EAGAIN || e == EWOULDBLOCK));  // -1 1
    return 0;
}
