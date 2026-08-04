// SO_PEERCRED on a connected AF_UNIX pair reports the peer credentials. For a socketpair created
// in one process the uid/gid equal this process's; printed as booleans to stay run-neutral.
#define _GNU_SOURCE
#include <sys/socket.h>
#include <unistd.h>
#include <stdio.h>
int main(void){
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv)) return 1;
    struct ucred uc; socklen_t len = sizeof uc;
    int rc = getsockopt(sv[0], SOL_SOCKET, SO_PEERCRED, &uc, &len);
    printf("peercred rc=%d len_ok=%d uid_match=%d gid_match=%d pid_positive=%d\n",
           rc, len == sizeof uc, uc.uid == getuid(), uc.gid == getgid(), uc.pid > 0);
    return 0;
}
