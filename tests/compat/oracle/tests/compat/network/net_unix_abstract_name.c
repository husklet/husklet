// An explicit abstract-namespace AF_UNIX address (sun_path[0]==0) must round-trip through getsockname and
// getpeername as the exact guest name -- never the engine's private backing filesystem path. Connecting to
// an unbound abstract name yields ECONNREFUSED (an abstract name is not a filesystem object -> not ENOENT).
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
#include <errno.h>
int main(void){
    // Server binds an explicit abstract name.
    int srv = socket(AF_UNIX, SOCK_STREAM, 0);
    struct sockaddr_un a; memset(&a, 0, sizeof a); a.sun_family = AF_UNIX;
    memcpy(a.sun_path + 1, "hlname", 6); // sun_path[0] stays NUL
    socklen_t alen = (socklen_t)(2 + 1 + 6);
    int br = bind(srv, (struct sockaddr *)&a, alen);
    listen(srv, 4);
    struct sockaddr_un sg; socklen_t sgl = sizeof sg; memset(&sg, 0, sizeof sg);
    getsockname(srv, (struct sockaddr *)&sg, &sgl);
    int self_abs = sg.sun_path[0] == 0 && memcmp(sg.sun_path + 1, "hlname", 6) == 0 && sgl == alen;

    // Client connects; getpeername echoes the server's abstract name.
    int cl = socket(AF_UNIX, SOCK_STREAM, 0);
    int cr = connect(cl, (struct sockaddr *)&a, alen);
    struct sockaddr_un pg; socklen_t pgl = sizeof pg; memset(&pg, 0, sizeof pg);
    getpeername(cl, (struct sockaddr *)&pg, &pgl);
    int peer_abs = pg.sun_path[0] == 0 && memcmp(pg.sun_path + 1, "hlname", 6) == 0 && pgl == alen;

    // Connecting to an unbound abstract name -> ECONNREFUSED.
    int cl2 = socket(AF_UNIX, SOCK_STREAM, 0);
    struct sockaddr_un b; memset(&b, 0, sizeof b); b.sun_family = AF_UNIX;
    memcpy(b.sun_path + 1, "hl_unbound_zzz", 14);
    int rr = connect(cl2, (struct sockaddr *)&b, (socklen_t)(2 + 1 + 14));
    int refused = rr < 0 && errno == ECONNREFUSED;

    printf("bind=%d self_abs=%d connect=%d peer_abs=%d refused=%d\n", br, self_abs, cr, peer_abs, refused);
    close(cl2); close(cl); close(srv);
    return 0;
}
