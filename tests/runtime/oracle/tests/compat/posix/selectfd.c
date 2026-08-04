// select/pselect on a pipe: readability edges, timeout expiry, and FD_SETSIZE macro sanity.
#include <sys/select.h>
#include <unistd.h>
#include <signal.h>
#include <string.h>
#include <stdio.h>

int main(void) {
    int fds[2];
    if (pipe(fds) != 0) { printf("selectfd pipe=0\n"); return 0; }

    // Nothing written yet: select times out (not ready).
    fd_set rf;
    FD_ZERO(&rf);
    FD_SET(fds[0], &rf);
    struct timeval tv = {0, 50000};
    int r0 = select(fds[0] + 1, &rf, NULL, NULL, &tv);
    int timed_out = r0 == 0 && !FD_ISSET(fds[0], &rf);

    // After a write, the read end is ready.
    write(fds[1], "x", 1);
    FD_ZERO(&rf);
    FD_SET(fds[0], &rf);
    struct timeval tv2 = {1, 0};
    int r1 = select(fds[0] + 1, &rf, NULL, NULL, &tv2);
    int ready = r1 == 1 && FD_ISSET(fds[0], &rf);

    // FD_SETSIZE bit manipulation near the top of the set.
    fd_set big;
    FD_ZERO(&big);
    int hi = FD_SETSIZE - 1;
    FD_SET(hi, &big);
    int hi_ok = FD_ISSET(hi, &big) && !FD_ISSET(hi - 1, &big) && FD_SETSIZE >= 1024;

    // pselect with a signal mask and a short timeout.
    char c;
    read(fds[0], &c, 1); // drain
    sigset_t empty;
    sigemptyset(&empty);
    FD_ZERO(&rf);
    FD_SET(fds[0], &rf);
    struct timespec ts = {0, 30000000};
    int r2 = pselect(fds[0] + 1, &rf, NULL, NULL, &ts, &empty);
    int psel_timeout = r2 == 0;

    close(fds[0]);
    close(fds[1]);
    printf("selectfd timeout=%d ready=%d hi=%d psel=%d\n",
           timed_out, ready, hi_ok, psel_timeout);
    return 0;
}
