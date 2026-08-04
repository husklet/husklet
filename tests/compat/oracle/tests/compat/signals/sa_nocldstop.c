// SA_NOCLDSTOP suppresses SIGCHLD for child stop/continue transitions while still delivering it
// on child exit. We stop+continue a child (no SIGCHLD expected) then let it exit (SIGCHLD fires).
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile sig_atomic_t chld_count;
static void h(int s) { (void)s; chld_count++; }

int main(void) {
    struct sigaction sa = {0};
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = SA_NOCLDSTOP | SA_RESTART;
    sigaction(SIGCHLD, &sa, NULL);

    pid_t pid = fork();
    if (pid == 0) {
        // wait to be stopped/continued/killed by parent
        for (;;) pause();
        _exit(0);
    }
    usleep(50 * 1000);
    kill(pid, SIGSTOP);
    waitpid(pid, NULL, WUNTRACED); // observe stop
    usleep(50 * 1000);
    int after_stop = chld_count; // should still be 0 (NOCLDSTOP)
    kill(pid, SIGCONT);
    usleep(50 * 1000);
    int after_cont = chld_count; // still 0

    kill(pid, SIGKILL);
    waitpid(pid, NULL, 0); // exit -> SIGCHLD delivered
    usleep(50 * 1000);
    int after_exit = chld_count; // >= 1

    printf("sa_nocldstop stop_silent=%d cont_silent=%d exit_delivered=%d\n",
           after_stop == 0, after_cont == 0, after_exit >= 1);
    return 0;
}
