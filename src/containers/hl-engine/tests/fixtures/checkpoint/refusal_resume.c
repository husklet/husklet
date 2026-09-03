#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define CHILD_STATUS 42

static volatile sig_atomic_t released;

static void release_member(int signal) {
    (void)signal;
    released = 1;
}

static void write_all(const char *text) {
    size_t left = strlen(text);
    while (left != 0) {
        ssize_t written = write(STDOUT_FILENO, text, left);
        if (written < 0 && errno == EINTR) continue;
        if (written <= 0) _exit(70);
        text += written;
        left -= (size_t)written;
    }
}

int main(void) {
    pid_t member = fork();
    if (member < 0) return 70;
    if (member == 0) {
        struct sigaction action;
        memset(&action, 0, sizeof action);
        action.sa_handler = release_member;
        sigemptyset(&action.sa_mask);
        if (sigaction(SIGUSR1, &action, NULL) != 0) _exit(71);

        /* A real buffered pipe makes the member's descriptor dump irreversible: capture drains the byte
           from the kernel before the post-dump refusal hook fires. Both ends deliberately remain live so
           the original tree would observe the loss if a broker incorrectly advertised clean recovery. */
        int shared_pipe[2];
        if (pipe(shared_pipe) != 0) _exit(80);
        const char payload = 'x';
        if (write(shared_pipe[1], &payload, 1) != 1) _exit(81);
        if (close(shared_pipe[1]) != 0) _exit(82);

        pid_t child = fork();
        if (child < 0) _exit(72);
        if (child == 0) _exit(CHILD_STATUS);
        for (;;) {
            siginfo_t status;
            memset(&status, 0, sizeof status);
            if (waitid(P_PID, (id_t)child, &status, WEXITED | WNOWAIT | WNOHANG) != 0) _exit(73);
            if (status.si_pid == child) break;
            struct timespec pause = {0, 1000000};
            nanosleep(&pause, NULL);
        }
        write_all("REFUSAL-MEMBER-ZOMBIE\n");
        while (!released) pause();

        char preserved = 0;
        if (read(shared_pipe[0], &preserved, 1) != 1 || preserved != payload) _exit(83);

        int status = 0;
        if (waitpid(child, &status, 0) != child) _exit(74);
        if (!WIFEXITED(status) || WEXITSTATUS(status) != CHILD_STATUS) _exit(75);
        write_all("REFUSAL-MEMBER-REAPED\n");
        _exit(0);
    }

    char byte;
    ssize_t read_result;
    while ((read_result = read(STDIN_FILENO, &byte, 1)) < 0 && errno == EINTR) {}
    if (read_result != 1) return 76;
    if (kill(member, SIGUSR1) != 0) return 77;
    int status = 0;
    if (waitpid(member, &status, 0) != member) return 78;
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) return 79;
    write_all("REFUSAL-COORDINATOR-RESUMED\n");
    return 0;
}
