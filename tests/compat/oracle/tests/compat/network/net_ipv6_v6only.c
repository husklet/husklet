// IPV6_V6ONLY get-after-set: read the default, disable it, enable it, and confirm
// each getsockopt reflects the last setsockopt. Pure option-plumbing check on an
// AF_INET6 socket with no traffic. Deterministic booleans -> oracle.
#include "net_util.h"
#include <arpa/inet.h>
#include <string.h>
#include <netinet/in.h>
#include <stdio.h>
#include <sys/socket.h>
#include <unistd.h>

static int v6only(int fd) {
    int v = -1;
    socklen_t l = sizeof v;
    getsockopt(fd, IPPROTO_IPV6, IPV6_V6ONLY, &v, &l);
    return v;
}

int main(void) {
    net_watchdog(20);
    int s = socket(AF_INET6, SOCK_STREAM, 0);
    if (s < 0) { printf("no_ipv6\n"); return 0; }
    int def = v6only(s);
    int off = 0, on = 1;
    setsockopt(s, IPPROTO_IPV6, IPV6_V6ONLY, &off, sizeof off);
    int after_off = v6only(s);
    setsockopt(s, IPPROTO_IPV6, IPV6_V6ONLY, &on, sizeof on);
    int after_on = v6only(s);
    printf("default_bool=%d after_off=%d after_on=%d\n", def != 0, after_off, after_on != 0);

    struct sockaddr_in6 a6;
    memset(&a6, 0, sizeof a6);
    a6.sin6_family = AF_INET6;
    a6.sin6_port = htons(34568);
    int bind6 = bind(s, (struct sockaddr *)&a6, sizeof a6);

    int s4 = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in a4;
    memset(&a4, 0, sizeof a4);
    a4.sin_family = AF_INET;
    a4.sin_port = htons(34568);
    int bind4 = bind(s4, (struct sockaddr *)&a4, sizeof a4);
    int dual = bind6 == 0 && bind4 == 0 && listen(s, 1) == 0 && listen(s4, 1) == 0;
    printf("dual_bind=%d\n", dual);
    close(s4);
    close(s);
    return dual ? 0 : 1;
}
