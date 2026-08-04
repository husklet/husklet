// SOCK_DGRAM socketpair preserves message boundaries: two sends are received as two independent
// datagrams and never coalesced into one recv.
#include <sys/socket.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
int main(void){
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_DGRAM, 0, sv)) return 1;
    write(sv[1], "hello", 5);
    write(sv[1], "world", 5);
    char a[64] = {0}, b[64] = {0};
    ssize_t d1 = recv(sv[0], a, sizeof a, 0);
    ssize_t d2 = recv(sv[0], b, sizeof b, 0);
    printf("dgram d1=%zd a=%.5s d2=%zd b=%.5s separate=%d\n",
           d1, a, d2, b, (d1 == 5 && d2 == 5));   // 5 hello 5 world 1
    return 0;
}
