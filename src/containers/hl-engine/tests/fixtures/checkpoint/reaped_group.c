#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/prctl.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

static volatile sig_atomic_t released;

static void fail(const char *operation) {
    perror(operation);
    _exit(70);
}

static void write_all(const char *text) {
    size_t length = 0;
    while (text[length] != 0) length++;
    while (length != 0) {
        ssize_t written = write(STDOUT_FILENO, text, length);
        if (written < 0 && errno == EINTR) continue;
        if (written <= 0) fail("write");
        text += written;
        length -= (size_t)written;
    }
}

static void foreground(pid_t group) {
    sigset_t blocked;
    sigset_t saved;
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGTTOU);
    if (sigprocmask(SIG_BLOCK, &blocked, &saved) != 0) fail("block-sigttou");
    if (tcsetpgrp(STDIN_FILENO, group) != 0) fail("tcsetpgrp");
    if (sigprocmask(SIG_SETMASK, &saved, NULL) != 0) fail("restore-sigmask");
}

static void release_member(int signal) {
    (void)signal;
    released = 1;
}

static void expect_esrch(int status, const char *operation) {
    if (status != -1 || errno != ESRCH) fail(operation);
}

int main(void) {
    if (!isatty(STDIN_FILENO)) fail("terminal");
    if (prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0) fail("subreaper");
    int identities[2];
    if (pipe(identities) != 0) fail("pipe");

    pid_t leader = fork();
    if (leader < 0) fail("fork-leader");
    if (leader == 0) {
        close(identities[0]);
        if (setpgid(0, 0) != 0) fail("leader-setpgid");
        pid_t member = fork();
        if (member < 0) fail("fork-member");
        if (member == 0) {
            struct sigaction action = {0};
            action.sa_handler = release_member;
            sigemptyset(&action.sa_mask);
            if (sigaction(SIGTERM, &action, NULL) != 0) fail("sigaction");
            pid_t identity = getpid();
            if (write(identities[1], &identity, sizeof(identity)) != sizeof(identity)) fail("publish-member");
            close(identities[1]);
            while (!released) pause();
            write_all("REAPED-GROUP-MEMBER-SIGNALED\n");
            _exit(0);
        }
        close(identities[1]);
        _exit(0);
    }

    close(identities[1]);
    pid_t member = 0;
    if (read(identities[0], &member, sizeof(member)) != sizeof(member)) fail("read-member");
    close(identities[0]);
    int status = 0;
    while (waitpid(leader, &status, 0) < 0 && errno == EINTR) {}
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) return 71;
    foreground(leader);
    write_all("REAPED-GROUP-READY\n");

    char byte;
    while (read(STDIN_FILENO, &byte, 1) < 0 && errno == EINTR) {}

    if (getpgid(member) != leader) fail("member-getpgid");
    if (getsid(member) != getsid(0)) fail("member-getsid");
    if (tcgetpgrp(STDIN_FILENO) != leader) fail("foreground-group");

    errno = 0;
    expect_esrch(kill(leader, 0), "stale-process-kill");
    if (kill(-leader, 0) != 0) fail("surviving-group-kill-zero");

    const pid_t unknown = 2000000000;
    errno = 0;
    expect_esrch(getpgid(unknown), "unknown-getpgid");
    errno = 0;
    expect_esrch(getsid(unknown), "unknown-getsid");
    errno = 0;
    expect_esrch(kill(unknown, 0), "unknown-process-kill");
    errno = 0;
    expect_esrch(kill(-unknown, 0), "unknown-group-kill");

    if (kill(-leader, SIGTERM) != 0) fail("signal-surviving-group");
    foreground(getpgrp());
    write_all("REAPED-GROUP-IDENTITIES-PRESERVED\n");
    return 0;
}
