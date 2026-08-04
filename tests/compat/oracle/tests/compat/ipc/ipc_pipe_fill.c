// Pipe buffering state: F_GETPIPE_SZ reports the default, F_SETPIPE_SZ rounds up to a page,
// a non-blocking writer fills to exactly the capacity and then gets EAGAIN, and PIPE_BUF-sized
// writes stay atomic while a larger write is short once the buffer is nearly full.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    int fd[2];
    if (pipe2(fd, O_NONBLOCK)) return 1;
    int deflt = fcntl(fd[1], F_GETPIPE_SZ);
    int set = fcntl(fd[1], F_SETPIPE_SZ, 8192);
    int got = fcntl(fd[1], F_GETPIPE_SZ);
    int tiny = fcntl(fd[1], F_SETPIPE_SZ, 1);
    int gottiny = fcntl(fd[1], F_GETPIPE_SZ);
    int neg = fcntl(fd[1], F_SETPIPE_SZ, -1);
    int eneg = (neg == -1) ? errno : 0;

    static char blob[65536];
    memset(blob, 'x', sizeof blob);
    long total = 0;
    ssize_t w;
    while ((w = write(fd[1], blob, 1024)) > 0) total += w;
    int efull = (w == -1) ? errno : 0;
    int exact = (total == gottiny);
    // one more byte still fails
    ssize_t one = write(fd[1], blob, 1);
    int eone = (one == -1) ? errno : 0;
    // drain one chunk, then a PIPE_BUF write is refused wholesale rather than truncated
    char drain[512];
    ssize_t d = read(fd[0], drain, sizeof drain);
    ssize_t part = write(fd[1], blob, PIPE_BUF);
    int epart = (part == -1) ? errno : 0;
    ssize_t small = write(fd[1], blob, 256);
    printf("defok=%d set=%d got=%d tiny=%d gottiny=%d neg=%d eneg=%d exact=%d efull=%d one=%zd eone=%d d=%zd part=%zd epart=%d small=%zd pipebuf=%d\n",
           deflt > 0, set, got, tiny, gottiny, neg, eneg, exact, efull, one, eone, d, part, epart, small, PIPE_BUF);
    return 0;
}
