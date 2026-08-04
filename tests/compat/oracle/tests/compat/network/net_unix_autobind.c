// bind() of an AF_UNIX socket with only the family (addrlen == sizeof(sa_family_t)) triggers Linux
// "autobind": the kernel assigns a unique abstract-namespace address. getsockname must then report an
// abstract name (leading NUL) with a non-trivial length, not an unnamed socket (family only).
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
#include <errno.h>
int main(void){
    int s = socket(AF_UNIX, SOCK_DGRAM, 0);
    if (s < 0) { printf("socket fail\n"); return 1; }
    struct sockaddr_un a; memset(&a, 0, sizeof a); a.sun_family = AF_UNIX;
    int br = bind(s, (struct sockaddr *)&a, sizeof(sa_family_t));
    struct sockaddr_un g; socklen_t gl = sizeof g; memset(&g, 0, sizeof g);
    int gr = getsockname(s, (struct sockaddr *)&g, &gl);
    // A connecting peer must be able to reach the autobound address (round-trip through the abstract name).
    int c = socket(AF_UNIX, SOCK_DGRAM, 0);
    int cr = connect(c, (struct sockaddr *)&g, gl);
    printf("bind=%d gr=%d abstract=%d longname=%d connect=%d\n",
           br, gr, g.sun_path[0] == 0, gl > (socklen_t)sizeof(sa_family_t), cr);
    close(c); close(s);
    return 0;
}
