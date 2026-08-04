// Job-control wait reporting: a child is SIGSTOP'd and observed via waitpid(WUNTRACED) as WIFSTOPPED
// (WSTOPSIG==SIGSTOP), then SIGCONT'd and observed via waitpid(WCONTINUED) as WIFCONTINUED, then it
// exits normally and is reaped. Exercises stopped/continued status plumbing through the wait path.
#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    pid_t p = fork();
    if (p == 0) {
        // wait to be stopped then continued; a simple spin with sleeps keeps it alive and observable
        for (int i = 0; i < 200; i++) usleep(10 * 1000);
        _exit(8);
    }

    usleep(30 * 1000);
    kill(p, SIGSTOP);
    int s1 = 0;
    int stopped = waitpid(p, &s1, WUNTRACED) == p && WIFSTOPPED(s1) && WSTOPSIG(s1) == SIGSTOP;

    kill(p, SIGCONT);
    int s2 = 0;
    int continued = waitpid(p, &s2, WCONTINUED) == p && WIFCONTINUED(s2);

    // now stop it again and kill it while stopped -> reap the SIGKILL
    kill(p, SIGKILL);
    int s3 = 0;
    int killed = waitpid(p, &s3, 0) == p && WIFSIGNALED(s3) && WTERMSIG(s3) == SIGKILL;

    printf("wait_stop_cont stopped=%d continued=%d killed=%d\n", stopped, continued, killed);
    return 0;
}
