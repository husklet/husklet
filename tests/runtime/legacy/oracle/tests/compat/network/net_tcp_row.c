// /proc/net/tcp must reflect the guest's OWN listening sockets (ss/netstat -l, monitoring agents parse the
// hex local_address + state). A LISTEN socket bound to an EPHEMERAL port (bind(:0)) must surface a row: the
// kernel assigns the port during bind, and /proc/net/tcp then shows local_address:PORT with state 0A.
// The engine records the guest-requested port at bind time, which is 0 for bind(:0); without writing the
// resolved ephemeral port back into the table the row was dropped entirely (a listener invisible to ss -l).
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>

static int slurp(const char *p, char *b, int c) {
    int fd = open(p, O_RDONLY); if (fd < 0) return -1;
    int n = 0, r; while (n < c - 1 && (r = read(fd, b + n, c - 1 - n)) > 0) n += r;
    close(fd); b[n] = 0; return n;
}

int main(void) {
    char b[65536];
    int ok = 1;

    int s = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in a; memset(&a, 0, sizeof a);
    a.sin_family = AF_INET; a.sin_addr.s_addr = htonl(INADDR_LOOPBACK); a.sin_port = htons(0);
    ok &= (bind(s, (struct sockaddr *)&a, sizeof a) == 0);
    ok &= (listen(s, 8) == 0);
    socklen_t al = sizeof a; getsockname(s, (struct sockaddr *)&a, &al);
    unsigned port = ntohs(a.sin_port);
    ok &= (port != 0);

    // the listener row: local_address ends with ":PORT" (uppercase hex, 4+ digits) and state is 0A (LISTEN).
    char want[16]; snprintf(want, sizeof want, ":%04X", port);
    ok &= (slurp("/proc/net/tcp", b, sizeof b) > 0);
    int row = 0;
    for (char *ln = b; ln && *ln;) {
        char *nl = strchr(ln, '\n');
        if (nl) *nl = 0;
        if (strstr(ln, want) && strstr(ln, " 0A ")) row = 1;
        ln = nl ? nl + 1 : 0;
    }
    ok &= row;
    close(s);

    printf("net_tcp_row ok=%d\n", ok);
    return 0;
}
