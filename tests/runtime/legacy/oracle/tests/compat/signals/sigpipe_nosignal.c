// On a stream socket whose peer is closed, send(MSG_NOSIGNAL) suppresses SIGPIPE and returns
// -1/EPIPE, whereas a plain send() would raise SIGPIPE. Verify no signal with MSG_NOSIGNAL and
// that the errno is EPIPE.
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <sys/socket.h>
#include <unistd.h>

static volatile sig_atomic_t got;
static void h(int s) { (void)s; got++; }

int main(void) {
    signal(SIGPIPE, h);
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) { printf("sigpipe_nosignal sp_fail\n"); return 1; }
    close(sv[1]); // peer closed

    errno = 0;
    ssize_t r = send(sv[0], "x", 1, MSG_NOSIGNAL);
    // first write may buffer/RST; loop until EPIPE
    int tries = 0;
    while (r != -1 && tries < 100) { errno = 0; r = send(sv[0], "x", 1, MSG_NOSIGNAL); tries++; }
    int nosig_epipe = r == -1 && errno == EPIPE && got == 0;

    printf("sigpipe_nosignal epipe_no_signal=%d\n", nosig_epipe);
    return 0;
}
