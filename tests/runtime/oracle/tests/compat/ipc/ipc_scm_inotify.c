// SCM_RIGHTS passes a LIVE inotify fd across an AF_UNIX socketpair; the receiver reads watch events off
// the received fd. inotify is an engine-emulated object keyed by fd number; the received fd carries none
// of that routing state, so its read currently does not drain the watch queue (excluded-known-bug:
// emulated-fd identity not transferred across SCM_RIGHTS -- same root class as ipc_scm_timerfd, fixed).
#define _GNU_SOURCE
#include <sys/socket.h>
#include <sys/inotify.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>
#include <fcntl.h>
#include <poll.h>
int main(void) {
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv)) return 1;
    int ino = inotify_init1(IN_NONBLOCK);
    if (ino < 0) return 1;
    char dir[] = "/tmp/hlscminoXXXXXX";
    char *d = mkdtemp(dir);
    if (!d) return 1;
    int wd = inotify_add_watch(ino, d, IN_CREATE);

    char cbuf[CMSG_SPACE(sizeof(int))];
    memset(cbuf, 0, sizeof cbuf);
    char data = 'I';
    struct iovec io = {&data, 1};
    struct msghdr mh = {0};
    mh.msg_iov = &io;
    mh.msg_iovlen = 1;
    mh.msg_control = cbuf;
    mh.msg_controllen = sizeof cbuf;
    struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &ino, sizeof(int));
    sendmsg(sv[1], &mh, 0);

    char rdata;
    struct iovec rio = {&rdata, 1};
    char rcbuf[CMSG_SPACE(sizeof(int))];
    memset(rcbuf, 0, sizeof rcbuf);
    struct msghdr rmh = {0};
    rmh.msg_iov = &rio;
    rmh.msg_iovlen = 1;
    rmh.msg_control = rcbuf;
    rmh.msg_controllen = sizeof rcbuf;
    recvmsg(sv[0], &rmh, 0);
    struct cmsghdr *rc = CMSG_FIRSTHDR(&rmh);
    int rino = -1;
    if (rc && rc->cmsg_type == SCM_RIGHTS) memcpy(&rino, CMSG_DATA(rc), sizeof(int));

    char path[300];
    snprintf(path, sizeof path, "%s/f", d);
    int cf = creat(path, 0600);
    if (cf >= 0) close(cf);
    struct pollfd pf = {rino, POLLIN, 0};
    int pr = poll(&pf, 1, 2000);
    char eb[512];
    ssize_t n = (rino >= 0) ? read(rino, eb, sizeof eb) : -1;
    int create_seen = 0;
    if (n > 0) {
        struct inotify_event *ev = (void *)eb;
        create_seen = (ev->mask & IN_CREATE) != 0;
    }
    printf("wd_ok=%d poll=%d read_pos=%d create_seen=%d\n", wd >= 0, pr, n > 0, create_seen); // 1 1 1 1
    return 0;
}
