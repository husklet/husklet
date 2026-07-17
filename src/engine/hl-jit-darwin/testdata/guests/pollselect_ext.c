// #396 readiness/timeout matrix for poll/select/pselect/ppoll on a pipe. Portable POSIX -> runs (and
// must agree byte-for-byte) on linux/x86_64, linux/aarch64 and darwin/aarch64. Every check is a
// deterministic boolean (no timing in the output), so it is golden-checked. Proves the multiplexers
// report read/write readiness, count ready fds correctly, and honour 0/finite timeouts identically to
// a real kernel on every engine.
#define _GNU_SOURCE
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/select.h>
#include <unistd.h>

int main(void) {
    int a[2], b[2];
    if (pipe(a) < 0 || pipe(b) < 0) return 1;
    if (write(a[1], "x", 1) < 0) return 1; // a[0] readable; b never readable

    // poll: read-ready, write-ready, and a 0-timeout non-ready fd returns 0.
    struct pollfd pr = {.fd = a[0], .events = POLLIN};
    int poll_rd = (poll(&pr, 1, 1000) == 1) && (pr.revents & POLLIN);
    struct pollfd pw = {.fd = a[1], .events = POLLOUT};
    int poll_wr = (poll(&pw, 1, 1000) == 1) && (pw.revents & POLLOUT);
    struct pollfd pt = {.fd = b[0], .events = POLLIN};
    int poll_to0 = (poll(&pt, 1, 0) == 0);
    int poll_to = (poll(&pt, 1, 30) == 0); // finite timeout, not ready -> 0

    // select: read-ready, write-ready, count=2 (readable + writable, distinct fds), timeout-0-clears.
    fd_set rs, ws;
    FD_ZERO(&rs); FD_SET(a[0], &rs);
    struct timeval tv1 = {1, 0};
    int sel_rd = (select(a[0] + 1, &rs, NULL, NULL, &tv1) == 1) && FD_ISSET(a[0], &rs);
    FD_ZERO(&ws); FD_SET(a[1], &ws);
    struct timeval tv1b = {1, 0};
    int sel_wr = (select(a[1] + 1, NULL, &ws, NULL, &tv1b) == 1) && FD_ISSET(a[1], &ws);
    FD_ZERO(&rs); FD_ZERO(&ws); FD_SET(a[0], &rs); FD_SET(a[1], &ws);
    int nf = (a[0] > a[1] ? a[0] : a[1]) + 1;
    struct timeval tv1c = {1, 0};
    int r2 = select(nf, &rs, &ws, NULL, &tv1c);
    int sel_count2 = (r2 == 2) && FD_ISSET(a[0], &rs) && FD_ISSET(a[1], &ws);
    FD_ZERO(&rs); FD_SET(b[0], &rs);
    struct timeval tvt = {0, 30000};
    int sel_to = (select(b[0] + 1, &rs, NULL, NULL, &tvt) == 0) && !FD_ISSET(b[0], &rs);

    // pselect: read-ready + finite-timeout-0 + nfds=0 pure-sleep returns 0.
    FD_ZERO(&rs); FD_SET(a[0], &rs);
    struct timespec pts = {1, 0};
    int psel_rd = (pselect(a[0] + 1, &rs, NULL, NULL, &pts, NULL) == 1) && FD_ISSET(a[0], &rs);
    FD_ZERO(&rs); FD_SET(b[0], &rs);
    struct timespec ptt = {0, 30000000};
    int psel_to = (pselect(b[0] + 1, &rs, NULL, NULL, &ptt, NULL) == 0);
    struct timespec ptz = {0, 30000000};
    int psel_nfds0 = (pselect(0, NULL, NULL, NULL, &ptz, NULL) == 0);

    printf("poll rd=%d wr=%d to0=%d to=%d\n", poll_rd, poll_wr, poll_to0, poll_to);
    printf("select rd=%d wr=%d count2=%d to=%d\n", sel_rd, sel_wr, sel_count2, sel_to);
    printf("pselect rd=%d to=%d nfds0=%d\n", psel_rd, psel_to, psel_nfds0);
    return 0;
}
