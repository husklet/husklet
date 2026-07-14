// wl_shm pool foundation (dd display pipeline, M0): the EXACT CPU zero-copy path a Wayland client's
// wl_shm pool takes to the compositor. A `memfd_create`'d anonymous file is ftruncate'd to a pool
// size, mmap(MAP_SHARED)'d, and a test pattern written into it. The fd is then handed to another
// process over an AF_UNIX socket via SCM_RIGHTS (exactly how libwayland sends the pool fd to the
// server). The receiver mmap(MAP_SHARED)s the SAME fd at the SAME size and must (a) see the producer's
// pixels and (b) write back a byte the producer then observes through its own mapping — proving the two
// mappings alias the same physical pages across a process + fd-passing boundary. If memfd_create,
// ftruncate-on-memfd, MAP_SHARED coherency, or SCM_RIGHTS fd translation is wrong, the bytes diverge.
// Diffed vs a native Linux oracle.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <stdint.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#define POOL 8192  // 2 pages: a realistic small wl_shm pool

int main(void) {
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) < 0) { perror("socketpair"); return 1; }

    pid_t pid = fork();
    if (pid == 0) {
        // ---- Producer (the "client"): create the pool, fill it, send the fd. ----
        close(sv[0]);
        int fd = memfd_create("hl-wl-shm-pool", 0);
        if (fd < 0) { perror("memfd_create"); _exit(10); }
        if (ftruncate(fd, POOL) < 0) { perror("ftruncate"); _exit(11); }
        uint8_t *pix = mmap(NULL, POOL, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        if (pix == MAP_FAILED) { perror("mmap producer"); _exit(12); }
        // A recognizable gradient "test pattern".
        for (int i = 0; i < POOL; i++) pix[i] = (uint8_t)(i * 7 + 3);

        char cbuf[CMSG_SPACE(sizeof(int))];
        memset(cbuf, 0, sizeof cbuf);
        char d = 'P';
        struct iovec io = {&d, 1};
        struct msghdr mh = {0};
        mh.msg_iov = &io; mh.msg_iovlen = 1;
        mh.msg_control = cbuf; mh.msg_controllen = sizeof cbuf;
        struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
        c->cmsg_level = SOL_SOCKET; c->cmsg_type = SCM_RIGHTS; c->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(c), &fd, sizeof(int));
        if (sendmsg(sv[1], &mh, 0) < 0) { perror("sendmsg"); _exit(13); }

        // Wait for the consumer to write its marker back through its own mapping, then verify we
        // (the producer) see it through OURS — proves shared coherency in the reverse direction too.
        char ack;
        if (read(sv[1], &ack, 1) != 1) _exit(14);
        int seen = (pix[POOL - 1] == 0xAB);
        munmap(pix, POOL);
        close(fd);
        _exit(seen ? 0 : 20);
    }

    // ---- Consumer (the "compositor"): receive the fd, map it, verify + mutate. ----
    close(sv[1]);
    char cbuf[CMSG_SPACE(sizeof(int))];
    memset(cbuf, 0, sizeof cbuf);
    char d;
    struct iovec io = {&d, 1};
    struct msghdr mh = {0};
    mh.msg_iov = &io; mh.msg_iovlen = 1;
    mh.msg_control = cbuf; mh.msg_controllen = sizeof cbuf;
    if (recvmsg(sv[0], &mh, 0) < 0) { perror("recvmsg"); return 1; }
    int rfd = -1;
    struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
    if (c && c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS)
        memcpy(&rfd, CMSG_DATA(c), sizeof(int));

    int got_fd = (rfd >= 0);
    int pattern_ok = 0, size_ok = 0;
    if (got_fd) {
        // The server learns the pool size out-of-band (BUFFER_ATTACH in DDP / wl_shm_create_pool arg);
        // it maps at that size and the mapping must succeed against the passed fd.
        uint8_t *pix = mmap(NULL, POOL, PROT_READ | PROT_WRITE, MAP_SHARED, rfd, 0);
        if (pix != MAP_FAILED) {
            size_ok = 1;
            pattern_ok = 1;
            for (int i = 0; i < POOL; i++)
                if (pix[i] != (uint8_t)(i * 7 + 3)) { pattern_ok = 0; break; }
            pix[POOL - 1] = 0xAB;  // write-back marker the producer will re-read
            munmap(pix, POOL);
        }
        close(rfd);
    }
    char ack = 'A';
    if (write(sv[0], &ack, 1) != 1) { /* producer already gone */ }
    int st = 0;
    waitpid(pid, &st, 0);
    int producer_saw_writeback = (WIFEXITED(st) && WEXITSTATUS(st) == 0);

    // 1 1 1 1 : fd passed, mapped at size, pattern matched, reverse-coherency confirmed.
    printf("wlshm got_fd=%d size_ok=%d pattern_ok=%d coherent=%d\n",
           got_fd, size_ok, pattern_ok, producer_saw_writeback);
    return 0;
}
