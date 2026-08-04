// SA_RESTART timer preemption of a blocking wait(): Linux runs a signal handler on EVERY interrupted slow
// syscall BEFORE restarting it, so a SIGALRM handler installed with SA_RESTART fires while the process is
// blocked in wait4() -- exactly the `timeout 1 sleep 5` pattern (the handler kills the child, so the wait
// then returns). A translator that restarts the interrupted host syscall IN PLACE without first delivering
// the handler strands it until the child exits on its own, and this program HANGS. Deterministic: the child
// pauses forever, so the only way wait() ever returns is the handler running and killing it.
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile pid_t g_child;
static volatile sig_atomic_t g_fired;
static void on_alarm(int s) {
    (void)s;
    g_fired = 1;
    kill(g_child, SIGKILL); // unblock the parent's wait() by ending the child
}

int main(void) {
    g_child = fork();
    if (g_child == 0) {
        for (;;) pause();
        _exit(0);
    }
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_alarm;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = SA_RESTART; // glibc/busybox signal()/alarm() semantics
    sigaction(SIGALRM, &sa, NULL);

    struct itimerval it;
    memset(&it, 0, sizeof it);
    it.it_value.tv_usec = 200000; // 200 ms one-shot ITIMER_REAL
    setitimer(ITIMER_REAL, &it, NULL);

    int st = 0;
    while (waitpid(g_child, &st, 0) < 0) {}
    int killed = WIFSIGNALED(st) && WTERMSIG(st) == SIGKILL;
    printf("restart-wait fired=%d killed=%d\n", (int)g_fired, killed);
    return 0;
}
