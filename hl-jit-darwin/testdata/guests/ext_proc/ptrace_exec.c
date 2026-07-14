// ext_proc/ptrace_exec.c -- exercises hl's ptrace exec-stop (bug #238), the strace -f initial event.
// A parent forks a child; the child PTRACE_TRACEME's itself then execve()s THIS SAME binary in a trivial
// "exec-child" mode (argv[1] == "x"). Real Linux stops the tracee with SIGTRAP right after the exec, before
// the new image runs a single instruction; the parent asserts it observed that exec-stop, then PTRACE_CONT
// to let the re-exec'd child run to a clean exit.
//
// Golden line (both arches): ptrace-exec ok execstop=1 exit=0

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/ptrace.h>
#include <sys/wait.h>
#include <signal.h>

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "x") == 0) return 0; // re-exec'd child: succeed silently

    char self[4096];
    ssize_t n = readlink("/proc/self/exe", self, sizeof self - 1);
    if (n <= 0) { printf("ptrace-exec self-fail\n"); return 1; }
    self[n] = 0;

    pid_t child = fork();
    if (child < 0) { printf("ptrace-exec fork-fail\n"); return 1; }
    if (child == 0) {
        if (ptrace(PTRACE_TRACEME, 0, 0, 0) != 0) _exit(90);
        char *a[] = {self, (char *)"x", NULL};
        execve(self, a, (char *[]){NULL});
        _exit(91); // execve failed
    }

    int status = 0, execstop = 0, child_exit = -1;
    // 1) the post-execve SIGTRAP exec-stop
    if (waitpid(child, &status, 0) == child && WIFSTOPPED(status) && WSTOPSIG(status) == SIGTRAP) execstop = 1;
    // 2) let it run to completion
    ptrace(PTRACE_CONT, child, 0, 0);
    for (;;) {
        if (waitpid(child, &status, 0) != child) break;
        if (WIFEXITED(status)) { child_exit = WEXITSTATUS(status); break; }
        if (WIFSIGNALED(status)) { child_exit = 128 + WTERMSIG(status); break; }
        // any further stop: keep it moving
        ptrace(PTRACE_CONT, child, 0, 0);
    }

    printf("ptrace-exec ok execstop=%d exit=%d\n", execstop, child_exit);
    return 0;
}
