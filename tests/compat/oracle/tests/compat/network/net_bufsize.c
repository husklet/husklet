// SO_RCVBUF / SO_SNDBUF get-after-set: setting a buffer size yields a readable value
// at least as large as requested (Linux rounds up, often doubling). Asserts the
// monotonic ">= requested" contract as booleans so the exact kernel figure never
// leaks into the golden output. Deterministic and arch-neutral. -> oracle.
#include "net_util.h"
#include <stdio.h>
#include <sys/socket.h>
#include <unistd.h>

static int getbuf(int fd, int opt) {
    int v = 0;
    socklen_t l = sizeof v;
    getsockopt(fd, SOL_SOCKET, opt, &v, &l);
    return v;
}

int main(void) {
    net_watchdog(20);
    int s = socket(AF_INET, SOCK_STREAM, 0);
    int want = 65536;
    int r1 = setsockopt(s, SOL_SOCKET, SO_RCVBUF, &want, sizeof want);
    int r2 = setsockopt(s, SOL_SOCKET, SO_SNDBUF, &want, sizeof want);
    int rcv = getbuf(s, SO_RCVBUF);
    int snd = getbuf(s, SO_SNDBUF);
    printf("set_ok=%d rcv_at_least=%d snd_at_least=%d rcv_positive=%d\n", (r1 | r2) == 0,
           rcv >= want, snd >= want, rcv > 0);
    close(s);
    return 0;
}
