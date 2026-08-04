// A handler installed WITH SA_RESTART transparently restarts a blocking read(): the syscall does
// not return EINTR; instead it resumes and completes once data arrives. A child both fires the
// signal and later writes, so the read must ultimately succeed with the byte.
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <unistd.h>

static volatile sig_atomic_t ran;
static void h(int s) { (void)s; ran++; }

int main(void) {
    int p[2];
    if (pipe(p) != 0) { printf("eintr_restart_read pipe_fail\n"); return 1; }

    struct sigaction sa = {0};
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = SA_RESTART;
    sigaction(SIGUSR1, &sa, NULL);

    pid_t parent = getpid();
    pid_t pid = fork();
    if (pid == 0) {
        close(p[0]);
        usleep(50 * 1000);
        kill(parent, SIGUSR1);   // interrupts the read -> restarted
        usleep(50 * 1000);
        (void)!write(p[1], "X", 1);
        _exit(0);
    }
    close(p[1]);
    char buf[1] = {0};
    errno = 0;
    ssize_t r = read(p[0], buf, 1); // restarted across SIGUSR1, then completes
    int ok = r == 1 && buf[0] == 'X' && ran >= 1;
    printf("eintr_restart_read restarted_ok=%d\n", ok);
    return 0;
}
