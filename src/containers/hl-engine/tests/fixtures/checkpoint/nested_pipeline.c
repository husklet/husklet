#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

static void fail(const char *operation) {
    perror(operation);
    _exit(70);
}

static void write_all(const char *text) {
    size_t length = 0;
    while (text[length] != 0)
        length++;
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

static void alive(const char *marker) {
    for (;;) {
        usleep(100000);
        write_all(marker);
    }
}

static int shell(void) {
    if (setpgid(0, 0) != 0) fail("shell-setpgid");
    int gate[2];
    if (pipe(gate) != 0) fail("pipe");
    pid_t member = fork();
    if (member < 0) fail("fork-member");
    if (member == 0) {
        close(gate[1]);
        char byte;
        while (read(gate[0], &byte, 1) < 0 && errno == EINTR) {}
        close(gate[0]);
        alive("NESTED-MEMBER-ALIVE\n");
    }

    pid_t leader = fork();
    if (leader < 0) fail("fork-leader");
    if (leader == 0) {
        close(gate[0]);
        close(gate[1]);
        if (setpgid(0, 0) != 0) fail("leader-setpgid");
        alive("NESTED-LEADER-ALIVE\n");
    }

    close(gate[0]);
    if (setpgid(leader, leader) != 0 && errno != EACCES) fail("parent-set-leader");
    if (setpgid(member, leader) != 0) fail("parent-set-member");
    if (write(gate[1], "x", 1) != 1) fail("release-member");
    close(gate[1]);
    foreground(leader);
    write_all("NESTED-PIPELINE-SLEEPING\n");

    int member_status = 0;
    int leader_status = 0;
    while (waitpid(member, &member_status, 0) < 0 && errno == EINTR) {}
    while (waitpid(leader, &leader_status, 0) < 0 && errno == EINTR) {}
    if (!WIFSIGNALED(member_status) || WTERMSIG(member_status) != SIGINT || !WIFSIGNALED(leader_status) ||
        WTERMSIG(leader_status) != SIGINT)
        return 71;
    foreground(getpgrp());
    write_all("NESTED-SHELL-SURVIVED\n");
    return 0;
}

int main(void) {
    if (!isatty(STDIN_FILENO)) fail("terminal");
    pid_t child = fork();
    if (child < 0) fail("fork-shell");
    if (child == 0) _exit(shell());
    int status = 0;
    while (waitpid(child, &status, 0) < 0 && errno == EINTR) {}
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) return 72;
    write_all("NESTED-INIT-SURVIVED\n");
    return 0;
}
