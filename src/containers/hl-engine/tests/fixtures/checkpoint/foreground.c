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

int main(void) {
    sigset_t original_mask;
    sigemptyset(&original_mask);
    sigaddset(&original_mask, SIGUSR1);
    if (sigprocmask(SIG_BLOCK, &original_mask, NULL) != 0) fail("initial-sigmask");
    struct termios mode;
    if (!isatty(STDIN_FILENO) || tcgetattr(STDIN_FILENO, &mode) != 0) fail("terminal");
    mode.c_lflag |= ISIG;
    mode.c_cc[VINTR] = 3;
    if (tcsetattr(STDIN_FILENO, TCSANOW, &mode) != 0) fail("tcsetattr");
    if (setpgid(0, 0) != 0 && errno != EACCES && errno != EPERM) fail("setpgid-parent");

    pid_t child = fork();
    if (child < 0) fail("fork");
    if (child == 0) {
        if (setpgid(0, 0) != 0) fail("setpgid-child");
        for (;;) {
            usleep(100000);
            write_all("CHILD-ALIVE\n");
        }
    }
    if (setpgid(child, child) != 0 && errno != EACCES) fail("setpgid-child-parent");
    foreground(child);
    write_all("SLEEPING\n");

    int status = 0;
    while (waitpid(child, &status, 0) < 0) {
        if (errno != EINTR) fail("waitpid");
    }
    if (!WIFSIGNALED(status) || WTERMSIG(status) != SIGINT) {
        write_all("WRONG-CHILD-STATUS\n");
        return 71;
    }
    write_all("CHILD-SIGINT\n");
    foreground(getpgrp());
    sigset_t resumed_mask;
    if (sigprocmask(SIG_BLOCK, NULL, &resumed_mask) != 0 || !sigismember(&resumed_mask, SIGUSR1) ||
        sigismember(&resumed_mask, SIGTTOU)) {
        write_all("WRONG-SIGNAL-MASK\n");
        return 72;
    }
    write_all("MASK-RESTORED\n");
    write_all("PROMPT-SURVIVED\n");
    return 0;
}
