#define _GNU_SOURCE
// F_SETPIPE_SZ resizes the pipe buffer (rounded up to a page); F_GETPIPE_SZ reports the capacity,
// and PIPE_BUF is the constant atomic-write bound.
#include <unistd.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
int main(void){
    int p[2];
    if (pipe(p)) return 1;
    int def = fcntl(p[0], F_GETPIPE_SZ);
    int set = fcntl(p[0], F_SETPIPE_SZ, 4096);
    int now = fcntl(p[0], F_GETPIPE_SZ);
    printf("pipesz default_pos=%d set_ok=%d capacity_ge_4096=%d pipe_buf=%d\n",
           def > 0, set >= 0, now >= 4096, (int)PIPE_BUF);   // 1 1 1 4096
    return 0;
}
