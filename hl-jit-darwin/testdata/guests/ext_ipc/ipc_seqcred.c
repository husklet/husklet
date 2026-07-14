// SEQPACKET fidelity guard for the Mojo-IPC path (Chromium NodeChannel). Exercises three DGRAM-backed
// SOCK_SEQPACKET emulation properties that all broke chromium before the os/linux net.c fixes, all
// oracle-diffed vs native Linux (booleans/lengths only, so host vs guest credentials/pids don't diverge):
//   1. NO premature EOF when the parent closes the child's fork-inherited socketpair end while keeping its
//      own: a later recvmsg on the retained end must still deliver the child's real record (the bug injected
//      a zero-length "EOF" datagram into the parent's own end -> recv returned 0 -> "did not receive ping").
//   2. SCM_RIGHTS fd passing over SEQPACKET (not just STREAM): the received fd reads back the sent bytes.
//   3. SO_PASSCRED -> a synthesized SCM_CREDENTIALS record on recvmsg whose ucred.uid == getuid() (chromium
//      aborts "missing credentials" without it). Record boundary: a 5-byte send arrives as one 5-byte read.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <fcntl.h>
#include <unistd.h>

int main(void) {
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sv) < 0) { printf("seqcred socketpair_failed\n"); return 0; }
    const char *path = "/tmp/hl_seqcred_payload";
    pid_t pid = fork();
    if (pid == 0) {
        close(sv[0]); // child keeps sv[1]
        int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
        write(fd, "payload42", 9);
        lseek(fd, 0, SEEK_SET);
        char cbuf[CMSG_SPACE(sizeof(int))];
        memset(cbuf, 0, sizeof cbuf);
        struct iovec io = {(void *)"hello", 5};
        struct msghdr mh = {0};
        mh.msg_iov = &io; mh.msg_iovlen = 1;
        mh.msg_control = cbuf; mh.msg_controllen = sizeof cbuf;
        struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
        c->cmsg_level = SOL_SOCKET; c->cmsg_type = SCM_RIGHTS; c->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(c), &fd, sizeof(int));
        sendmsg(sv[1], &mh, 0); // one record: "hello" + the fd
        close(fd);
        _exit(0);
    }
    int on = 1;
    setsockopt(sv[0], SOL_SOCKET, SO_PASSCRED, &on, sizeof on); // ask for peer credentials on recv
    close(sv[1]); // drop the child's inherited end while KEEPING sv[0] -> must NOT self-inject an EOF
    char data[64] = {0};
    char cbuf[CMSG_SPACE(sizeof(int)) + CMSG_SPACE(sizeof(struct ucred))];
    memset(cbuf, 0, sizeof cbuf);
    struct iovec io = {data, sizeof data};
    struct msghdr mh = {0};
    mh.msg_iov = &io; mh.msg_iovlen = 1;
    mh.msg_control = cbuf; mh.msg_controllen = sizeof cbuf;
    ssize_t n = recvmsg(sv[0], &mh, 0); // blocks until the child's record arrives (live peer)
    int rfd = -1, have_cred = 0, uid_match = 0;
    for (struct cmsghdr *c = CMSG_FIRSTHDR(&mh); c; c = CMSG_NXTHDR(&mh, c)) {
        if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS)
            memcpy(&rfd, CMSG_DATA(c), sizeof(int));
        else if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_CREDENTIALS) {
            struct ucred uc;
            memcpy(&uc, CMSG_DATA(c), sizeof uc);
            have_cred = 1;
            uid_match = (uc.uid == getuid());
        }
    }
    char fdbuf[16] = {0};
    int fn = (rfd >= 0) ? (int)read(rfd, fdbuf, sizeof fdbuf - 1) : -1;
    waitpid(pid, NULL, 0);
    unlink(path);
    // record boundary n=5, data=hello, got_fd + fd bytes, credentials present with matching uid
    printf("seqcred n=%ld data=%s got_fd=%d fd_data=%s cred=%d uidmatch=%d\n", (long)n, data, rfd >= 0, fdbuf,
           have_cred, uid_match);
    return 0;
}
