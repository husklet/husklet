// SCM_RIGHTS passes an open file descriptor across an AF_UNIX socketpair; the received fd is a
// distinct number that refers to the same open file (reads see the same bytes).
#include <sys/socket.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
#include <fcntl.h>
int main(void){
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv)) return 1;
    int fds[2]; pipe(fds);
    write(fds[1], "shared", 6);
    char cbuf[CMSG_SPACE(sizeof(int))]; memset(cbuf, 0, sizeof cbuf);
    char data = 'D';
    struct iovec io = { &data, 1 };
    struct msghdr mh = {0};
    mh.msg_iov = &io; mh.msg_iovlen = 1;
    mh.msg_control = cbuf; mh.msg_controllen = sizeof cbuf;
    struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
    c->cmsg_level = SOL_SOCKET; c->cmsg_type = SCM_RIGHTS; c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &fds[0], sizeof(int));
    sendmsg(sv[1], &mh, 0);
    char rdata; struct iovec rio = { &rdata, 1 };
    char rcbuf[CMSG_SPACE(sizeof(int))]; memset(rcbuf, 0, sizeof rcbuf);
    struct msghdr rmh = {0};
    rmh.msg_iov = &rio; rmh.msg_iovlen = 1;
    rmh.msg_control = rcbuf; rmh.msg_controllen = sizeof rcbuf;
    recvmsg(sv[0], &rmh, 0);
    struct cmsghdr *rc = CMSG_FIRSTHDR(&rmh);
    int newfd = -1; memcpy(&newfd, CMSG_DATA(rc), sizeof(int));
    char b[6] = {0}; ssize_t n = read(newfd, b, 6);
    printf("scm type_rights=%d distinct_fd=%d bytes=%zd data=%.6s\n",
           rc->cmsg_type == SCM_RIGHTS, newfd != fds[0], n, b);   // 1 1 6 shared
    return 0;
}
