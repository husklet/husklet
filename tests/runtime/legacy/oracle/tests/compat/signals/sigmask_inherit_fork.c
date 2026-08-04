// A child inherits the parent's signal mask across fork(). With SIGUSR1 blocked, a child that
// raises SIGUSR1 keeps it pending (handler does not run) and sigpending reports it; the child exits
// normally to prove the blocked signal did not terminate it.
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile sig_atomic_t ran;
static void h(int s) { (void)s; ran++; }

int main(void) {
    signal(SIGUSR1, h);
    sigset_t block;
    sigemptyset(&block);
    sigaddset(&block, SIGUSR1);
    sigprocmask(SIG_BLOCK, &block, NULL);

    pid_t pid = fork();
    if (pid == 0) {
        raise(SIGUSR1);            // stays pending (inherited block)
        sigset_t pend;
        sigpending(&pend);
        int pending = sigismember(&pend, SIGUSR1);
        int no_run = ran == 0;
        _exit((pending ? 1 : 0) | (no_run ? 2 : 0)); // 3 == both true
    }
    int st = 0;
    waitpid(pid, &st, 0);
    int ok = WIFEXITED(st) && WEXITSTATUS(st) == 3;
    printf("sigmask_inherit_fork child_pending_and_blocked=%d\n", ok);
    return 0;
}
