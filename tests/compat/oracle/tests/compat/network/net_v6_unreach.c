// #261 — hl models an IPv4-only container network (like Docker's default bridge: eth0 has no global IPv6
// address and the IPv6 routing table is empty). So a connect() to a genuine external (global-unicast) IPv6
// address has NO ROUTE and must fail *immediately* with ENETUNREACH — never hang on the host's v6 stack.
// That instant failure is what lets a happy-eyeballs client (apt/curl) that tried the AAAA record first fall
// back to IPv4 in milliseconds, so `apt-get update` works without Acquire::ForceIPv4. Fixed Linux golden
// (hl's contract deliberately differs from a raw v6-capable host, matching a real IPv4-only container).
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

static long ms_since(struct timespec *t0) {
    struct timespec t1;
    clock_gettime(CLOCK_MONOTONIC, &t1);
    return (t1.tv_sec - t0->tv_sec) * 1000 + (t1.tv_nsec - t0->tv_nsec) / 1000000;
}

int main(void) {
    int fd = socket(AF_INET6, SOCK_STREAM, 0);
    if (fd < 0) {
        printf("v6_connect socket_fail\n");
        return 1;
    }
    struct sockaddr_in6 a;
    memset(&a, 0, sizeof a);
    a.sin6_family = AF_INET6;
    a.sin6_port = htons(80);
    // 2001:4860:4860::8888 — a routable global-unicast address (Google public DNS). On a real IPv4-only
    // container this is unreachable at once; the point is hl must say so too, not dial it over the host.
    inet_pton(AF_INET6, "2001:4860:4860::8888", &a.sin6_addr);

    struct timespec t0;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    int r = connect(fd, (struct sockaddr *)&a, sizeof a);
    long ms = ms_since(&t0);
    int enetunreach = (r < 0 && errno == ENETUNREACH);
    int fast = (ms < 1000); // must fail immediately (no 2-minute host-v6 timeout)
    close(fd);
    printf("v6_connect enetunreach=%d fast=%d\n", enetunreach, fast); // 1 1
    return 0;
}
