#define _GNU_SOURCE
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>

static int fill(int wfd){
    int flags = fcntl(wfd, F_GETFL);
    fcntl(wfd, F_SETFL, flags | O_NONBLOCK);
    char buf[4096]; memset(buf,'x',sizeof buf);
    int total=0;
    for(;;){
        ssize_t n = write(wfd, buf, 1);
        if(n<0){ if(errno==EAGAIN) break; return -1; }
        total += (int)n;
        if(total > 5000000) break;
    }
    fcntl(wfd, F_SETFL, flags);
    return total;
}

int main(void){
    int p[2];
    if(pipe(p)) return 1;
    int def = fcntl(p[0], F_GETPIPE_SZ);
    int r4 = fcntl(p[0], F_SETPIPE_SZ, 4096);
    int g4 = fcntl(p[0], F_GETPIPE_SZ);
    int c4 = fill(p[1]);
    close(p[0]); close(p[1]);

    int q[2]; if(pipe(q)) return 1;
    int r70 = fcntl(q[0], F_SETPIPE_SZ, 70000);
    int c70 = fill(q[1]);
    close(q[0]); close(q[1]);

    int s[2]; if(pipe(s)) return 1;
    fcntl(s[0], F_SETPIPE_SZ, 1048576);
    char big[65536]; memset(big,'y',sizeof big);
    ssize_t wn = write(s[1], big, sizeof big); (void)wn;
    int shrink = fcntl(s[0], F_SETPIPE_SZ, 4096);
    int shrink_errno = shrink<0?errno:0;
    close(s[0]); close(s[1]);

    printf("def=%d r4=%d g4=%d cap4=%d r70=%d cap70=%d shrink=%d serr=%d\n",
           def, r4, g4, c4, r70, c70, shrink, shrink_errno);
    return 0;
}
