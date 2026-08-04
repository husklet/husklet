// Default actions for uncaught fatal signals terminate the child, and the parent observes
// WIFSIGNALED with the exact WTERMSIG. This is the kernel side of the shell's "128+signo" rule.
// Cover a spread of standard fatal signals with portable numbers.
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

static int died_by(int sig) {
    pid_t pid = fork();
    if (pid == 0) {
        signal(sig, SIG_DFL);
        raise(sig);
        _exit(0); // should not reach
    }
    int st = 0;
    waitpid(pid, &st, 0);
    return WIFSIGNALED(st) && WTERMSIG(st) == sig;
}

int main(void) {
    int sigs[] = {SIGTERM, SIGINT, SIGQUIT, SIGHUP, SIGKILL, SIGUSR1, SIGALRM};
    int all = 1;
    for (unsigned i = 0; i < sizeof(sigs) / sizeof(sigs[0]); i++)
        if (!died_by(sigs[i])) all = 0;
    printf("default_exit_codes all_termsig_match=%d\n", all);
    return 0;
}
