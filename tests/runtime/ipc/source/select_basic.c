// select detects a readable fd and honours a zero timeout returning 0 when nothing is ready.
#include <unistd.h>
#include <sys/select.h>
#include <stdio.h>
int main(void){
    int p[2];
    if (pipe(p)) return 1;
    fd_set r; FD_ZERO(&r); FD_SET(p[0], &r);
    struct timeval z = {0, 0};
    int none = select(p[0]+1, &r, NULL, NULL, &z);   // 0
    write(p[1], "q", 1);
    FD_ZERO(&r); FD_SET(p[0], &r);
    struct timeval t = {0, 100*1000};
    int some = select(p[0]+1, &r, NULL, NULL, &t);   // 1
    int isset = FD_ISSET(p[0], &r);
    printf("select none=%d some=%d isset=%d\n", none, some, isset);
    return 0;
}
