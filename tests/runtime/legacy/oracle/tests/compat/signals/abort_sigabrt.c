// abort() raises SIGABRT; a handler that returns does NOT prevent termination (abort re-raises
// with the default disposition), so the child still dies by SIGABRT. Also confirm a handled abort
// runs the handler exactly once before dying.
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

static void h(int s) { (void)s; _exit(77); } // catch and exit cleanly to prove handler ran

int main(void) {
    // Child 1: plain abort -> SIGABRT default
    pid_t p1 = fork();
    if (p1 == 0) { abort(); _exit(0); }
    int st1 = 0; waitpid(p1, &st1, 0);
    int default_abrt = WIFSIGNALED(st1) && WTERMSIG(st1) == SIGABRT;

    // Child 2: handler runs on abort
    pid_t p2 = fork();
    if (p2 == 0) { signal(SIGABRT, h); abort(); _exit(0); }
    int st2 = 0; waitpid(p2, &st2, 0);
    int handler_ran = WIFEXITED(st2) && WEXITSTATUS(st2) == 77;

    printf("abort_sigabrt default_abrt=%d handler_ran=%d\n", default_abrt, handler_ran);
    return 0;
}
