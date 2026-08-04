// A dup'd socket shares the same underlying connection: data written to one endpoint
// is readable through a dup() of the peer, and closing the dup leaves the original
// fd fully functional. Verifies descriptor aliasing semantics. -> oracle.
#include "net_util.h"
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int main(void) {
    net_watchdog(20);
    int sv[2];
    socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
    int d = dup(sv[1]);

    write(sv[0], "first", 5);
    char b1[8] = {0};
    ssize_t n1 = read(d, b1, sizeof b1 - 1); // read through the dup
    b1[n1 > 0 ? n1 : 0] = 0;
    close(d); // closing dup must not close sv[1]

    write(sv[0], "second", 6);
    char b2[8] = {0};
    ssize_t n2 = read(sv[1], b2, sizeof b2 - 1);
    b2[n2 > 0 ? n2 : 0] = 0;
    printf("via_dup=%s after_close=%s\n", b1, b2);
    close(sv[0]);
    close(sv[1]);
    return 0;
}
