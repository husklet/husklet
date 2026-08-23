#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

static void fail(const char *operation) {
    perror(operation);
    _exit(70);
}

static void write_all(const char *text) {
    size_t length = strlen(text);
    while (length != 0) {
        ssize_t written = write(STDOUT_FILENO, text, length);
        if (written < 0 && errno == EINTR) continue;
        if (written <= 0) fail("write");
        text += written;
        length -= (size_t)written;
    }
}

static void wait_ok(pid_t child) {
    int status = 0;
    while (waitpid(child, &status, 0) < 0 && errno == EINTR) {}
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) fail("wait-child");
}

static void wait_release(void) {
    char bytes[32];
    for (;;) {
        ssize_t count = read(STDIN_FILENO, bytes, sizeof(bytes));
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) fail("read-release");
        if (memchr(bytes, '\n', (size_t)count) != NULL) return;
    }
}

static void credential_boundary(void) {
    int sockets[2];
    if (socketpair(AF_UNIX, SOCK_DGRAM, 0, sockets) != 0) fail("socketpair-credentials");
    int enabled = 1;
    if (setsockopt(sockets[0], SOL_SOCKET, SO_PASSCRED, &enabled, sizeof(enabled)) != 0) fail("passcred");
    struct ucred peer = {0};
    socklen_t peer_size = sizeof(peer);
    if (getsockopt(sockets[0], SOL_SOCKET, SO_PEERCRED, &peer, &peer_size) != 0) fail("peercred");
    if (peer.pid != getpid()) fail("peercred-guest-pid");

    pid_t sender = fork();
    if (sender < 0) fail("fork-credential-sender");
    if (sender == 0) {
        close(sockets[0]);
        if (send(sockets[1], "x", 1, 0) != 1) fail("send-credential");
        _exit(0);
    }
    close(sockets[1]);
    char byte = 0;
    char control[CMSG_SPACE(sizeof(struct ucred))] = {0};
    struct iovec vector = {.iov_base = &byte, .iov_len = 1};
    struct msghdr message = {0};
    message.msg_iov = &vector;
    message.msg_iovlen = 1;
    message.msg_control = control;
    message.msg_controllen = sizeof(control);
    if (recvmsg(sockets[0], &message, 0) != 1) fail("recv-credential");
    struct cmsghdr *header = CMSG_FIRSTHDR(&message);
    if (header == NULL || header->cmsg_level != SOL_SOCKET || header->cmsg_type != SCM_CREDENTIALS)
        fail("missing-credential");
    struct ucred received;
    memcpy(&received, CMSG_DATA(header), sizeof(received));
    if (received.pid != sender) fail("credential-guest-pid");
    close(sockets[0]);
    wait_ok(sender);
}

static pid_t spawn_paused(void) {
    pid_t child = fork();
    if (child < 0) fail("fork-paused");
    if (child == 0)
        for (;;)
            pause();
    return child;
}

int main(void) {
    write_all("DYNAMIC-IDENTITY-READY\n");
    wait_release();

    pid_t short_lived = fork();
    if (short_lived < 0) fail("fork-short");
    if (short_lived == 0) _exit(0);
    if (getpgid(short_lived) != getpgrp()) fail("fork-getpgid");
    if (kill(short_lived, 0) != 0) fail("fork-kill-zero");
    wait_ok(short_lived);
    errno = 0;
    if (kill(short_lived, 0) != -1 || errno != ESRCH) fail("reaped-kill");
    write_all("DYNAMIC-FORK-IDENTITY-PRESERVED\n");
    credential_boundary();
    write_all("DYNAMIC-CREDENTIALS-PRESERVED\n");

    pid_t session = fork();
    if (session < 0) fail("fork-session");
    if (session == 0) {
        if (setsid() != getpid()) fail("setsid");
        if (getsid(0) != getpid() || getpgid(0) != getpid()) fail("new-session-identity");
        for (;;)
            pause();
    }
    pid_t leader = spawn_paused();
    if (setpgid(leader, leader) != 0) fail("set-group-leader");
    pid_t sibling = spawn_paused();
    if (setpgid(sibling, leader) != 0) fail("set-sibling-group");
    if (getpgid(sibling) != leader || getpgid(leader) != leader) fail("sibling-visible-group");
    write_all("DYNAMIC-GROUPS-CREATED\n");

    char identities[128];
    int count = snprintf(identities, sizeof(identities), "DYNAMIC-CHILDREN %d %d %d\n", session, leader, sibling);
    if (count <= 0 || write(STDOUT_FILENO, identities, (size_t)count) != count) fail("publish-identities");
    wait_release();

    if (getsid(session) != session || getpgid(session) != session) fail("restored-session-identity");
    if (getpgid(leader) != leader || getpgid(sibling) != leader) fail("restored-sibling-group");
    if (kill(session, SIGKILL) != 0) fail("kill-session-child");
    if (kill(-leader, SIGKILL) != 0) fail("kill-dynamic-group");
    int status;
    while (waitpid(session, &status, 0) < 0 && errno == EINTR) {}
    while (waitpid(leader, &status, 0) < 0 && errno == EINTR) {}
    while (waitpid(sibling, &status, 0) < 0 && errno == EINTR) {}
    write_all("DYNAMIC-IDENTITY-PRESERVED\n");
    return 0;
}
