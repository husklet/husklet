// Writing to a pipe whose read end is closed raises SIGPIPE by default; with SIGPIPE ignored the
// write instead fails with EPIPE.
#include <unistd.h>
#include <signal.h>
#include <stdio.h>
#include <errno.h>
#include <string.h>
static volatile sig_atomic_t got;
static void h(int s){ (void)s; got = 1; }
int main(void){
    signal(SIGPIPE, h);
    int p[2];
    if (pipe(p)) return 1;
    close(p[0]);
    ssize_t n = write(p[1], "x", 1);
    printf("sigpipe delivered=%d write=%zd errno=%s\n", got, n, strerror(errno));  // 1 -1 EPIPE
    signal(SIGPIPE, SIG_IGN);
    int q[2]; if (pipe(q)) return 1; close(q[0]);
    errno = 0;
    ssize_t m = write(q[1], "y", 1); int e = errno;
    printf("sigpipe ignored_write=%zd errno=%s\n", m, strerror(e));  // -1 EPIPE
    return 0;
}
