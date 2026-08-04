// waitpid() blocked on a live child is restarted by an SA_RESTART SIGUSR1: the handler runs but
// wait does not return EINTR; it ultimately reaps the child with its real exit status.
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile sig_atomic_t ran;
static void h(int s) { (void)s; ran++; }

int main(void) {
    struct sigaction sa = {0};
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = SA_RESTART;
    sigaction(SIGUSR1, &sa, NULL);

    pid_t parent = getpid();
    pid_t worker = fork();
    if (worker == 0) {
        usleep(150 * 1000);
        _exit(9);
    }
    // helper to poke the parent mid-wait
    pid_t poker = fork();
    if (poker == 0) {
        usleep(50 * 1000);
        kill(parent, SIGUSR1);
        _exit(0);
    }

    int st = 0;
    errno = 0;
    pid_t r = waitpid(worker, &st, 0); // interrupted then restarted
    int ok = r == worker && WIFEXITED(st) && WEXITSTATUS(st) == 9 && ran >= 1;
    waitpid(poker, NULL, 0);
    printf("eintr_wait_restart restarted_reaped=%d\n", ok);
    return 0;
}
