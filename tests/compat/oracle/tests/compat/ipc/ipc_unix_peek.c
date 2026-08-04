// MSG_PEEK returns data without consuming it; a following normal recv sees the same bytes.
#include <sys/socket.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
int main(void){
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv)) return 1;
    write(sv[1], "peekme", 6);
    char a[6] = {0}, b[6] = {0};
    ssize_t pn = recv(sv[0], a, sizeof a, MSG_PEEK);
    ssize_t rn = recv(sv[0], b, sizeof b, 0);
    printf("peek n=%zd data=%.6s consume n=%zd data=%.6s same=%d\n",
           pn, a, rn, b, memcmp(a, b, 6) == 0);   // 6 peekme 6 peekme 1
    return 0;
}
