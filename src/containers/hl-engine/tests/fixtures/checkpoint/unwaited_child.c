// A guest holding a child's exit status that it has NOT yet collected, across a checkpoint.
//
// THE WINDOW, CONSTRUCTED RATHER THAN WAITED FOR. The production shape is a shell running
// `printf x >> progress; sleep .05` in a loop: busybox `sleep` is an external applet, so the shell spends
// most of its life blocked in wait4 for a transient child, and a freeze that lands while that child is a
// zombie is an ordinary event rather than a rare one. Waiting for that race is what the reproduction that
// found this defect had to do -- it appeared at round 7 of 19 under a load of ~16-37. Here the same state is
// built on purpose: the child exits immediately, waitid(WNOWAIT) OBSERVES the termination without consuming
// it, and the guest then parks in a read on its terminal. From that point the child is a corpse nobody has
// collected and the guest cannot collect it, so the freeze lands inside the window every time.
//
// WHY THE PARK IS A READ AND NOT A WAIT. If the guest blocked in wait4 instead, the kernel would hand it the
// status immediately and the capture would have nothing left to destroy -- the fixture would pass whether or
// not the engine preserves anything. The status must still be pending, in the kernel, when the coordinator's
// rendezvous reap runs.
//
// The assertion is the parent's: after the restore it must reap ITS OWN CHILD, by the guest pid fork gave
// it, with the exact status the child exited with. Before the fix the coordinator's
// `waitpid(-1, WNOHANG)` consumed that status outright -- there is no engine-side pending-status table
// behind a guest zombie, the kernel corpse IS the state -- and the restored parent waited on a pid that
// would never exist again.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define UNWAITED_CHILD_EXIT_CODE 42

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

int main(void) {
    pid_t child = fork();
    if (child < 0) fail("fork");
    if (child == 0) _exit(UNWAITED_CHILD_EXIT_CODE);

    for (;;) {
        siginfo_t reported;
        memset(&reported, 0, sizeof reported);
        if (waitid(P_PID, (id_t)child, &reported, WEXITED | WNOWAIT | WNOHANG) != 0) {
            if (errno == EINTR) continue;
            fail("waitid-peek");
        }
        if (reported.si_pid == child) break;
        struct timespec pause = {0, 1000000};
        (void)nanosleep(&pause, NULL);
    }
    write_all("UNWAITED-CHILD-ZOMBIE\n");

    char byte = 0;
    ssize_t received;
    while ((received = read(STDIN_FILENO, &byte, 1)) < 0 && errno == EINTR) {}
    if (received != 1) fail("resume-read");

    int status = 0;
    pid_t reaped;
    while ((reaped = waitpid(child, &status, 0)) < 0 && errno == EINTR) {}
    if (reaped != child) fail("waitpid");
    if (!WIFEXITED(status) || WEXITSTATUS(status) != UNWAITED_CHILD_EXIT_CODE) {
        write_all("UNWAITED-CHILD-WRONG-STATUS\n");
        return 72;
    }
    write_all("UNWAITED-CHILD-REAPED\n");
    return 0;
}
