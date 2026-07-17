// SCM_CREDENTIALS peer-pid IDENTITY guard for the Chromium Mojo ports bootstrap. The Mojo NodeChannel uses
// the SCM_CREDENTIALS ucred.pid the kernel attaches on recv as the peer's ports-node identity, so two
// distinct children must present two DISTINCT pids, and neither may equal the RECEIVER's own pid (a peer
// that reports the receiver's own pid looks like a self/loopback node and the ports node-merge never
// finalizes). Under hl, macOS reports the socketpair CREATOR's pid via LOCAL_PEERPID on BOTH ends (never
// updated on fork), so before the fix the creating parent read its OWN pid as every child's peer credential
// and container_pid() collapsed them all to guest pid 1 -> colliding, self-equal identities. The engine now
// stamps each socketpair end with a distinct synthetic peer node id. Output is booleans only (distinct /
// non-self / both-present), so native Linux (real child pids) and the guest (synthetic ids) agree exactly.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

// Fork a child over a fresh SEQPACKET socketpair; the parent enables SO_PASSCRED and recvmsg's the child's
// one-byte record, returning the peer's ucred.pid (0 if none delivered). Mirrors Chromium's NodeChannel.
static int recv_peer_pid(void) {
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sv) < 0) return 0;
    pid_t pid = fork();
    if (pid == 0) {
        close(sv[0]);
        struct iovec io = {(void *)"x", 1};
        struct msghdr mh = {0};
        mh.msg_iov = &io;
        mh.msg_iovlen = 1;
        sendmsg(sv[1], &mh, 0);
        _exit(0);
    }
    int on = 1;
    setsockopt(sv[0], SOL_SOCKET, SO_PASSCRED, &on, sizeof on);
    char data[8] = {0};
    char cbuf[CMSG_SPACE(sizeof(struct ucred))];
    memset(cbuf, 0, sizeof cbuf);
    struct iovec io = {data, sizeof data};
    struct msghdr mh = {0};
    mh.msg_iov = &io;
    mh.msg_iovlen = 1;
    mh.msg_control = cbuf;
    mh.msg_controllen = sizeof cbuf;
    recvmsg(sv[0], &mh, 0);
    int cpid = 0;
    for (struct cmsghdr *c = CMSG_FIRSTHDR(&mh); c; c = CMSG_NXTHDR(&mh, c))
        if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_CREDENTIALS) {
            struct ucred uc;
            memcpy(&uc, CMSG_DATA(c), sizeof uc);
            cpid = uc.pid;
        }
    waitpid(pid, NULL, 0);
    close(sv[0]);
    close(sv[1]);
    return cpid;
}

int main(void) {
    int p1 = recv_peer_pid();
    int p2 = recv_peer_pid();
    int me = (int)getpid();
    int both = (p1 > 0 && p2 > 0);
    int distinct = (p1 != p2);
    int nonself = (p1 != me && p2 != me);
    printf("credpid both=%d distinct=%d nonself=%d\n", both, distinct, nonself);
    return 0;
}
