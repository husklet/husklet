// shutdown(SHUT_WR) on one end delivers EOF (0-byte read) to the peer while still allowing the
// peer's own writes to flow back (SHUT_RD/SHUT_WR are directional).
#include <sys/socket.h>
#include <unistd.h>
#include <stdio.h>
int main(void){
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv)) return 1;
    shutdown(sv[0], SHUT_WR);
    char b[8];
    ssize_t eof = read(sv[1], b, sizeof b);   // 0: peer sees EOF
    // sv[1] can still send to sv[0]
    ssize_t w = write(sv[1], "back", 4);
    ssize_t r = read(sv[0], b, sizeof b);
    printf("shutdown eof=%zd back_write=%zd back_read=%zd\n", eof, w, r);  // 0 4 4
    return 0;
}
