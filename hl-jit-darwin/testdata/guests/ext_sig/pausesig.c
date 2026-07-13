// pause() blocks until a signal is delivered, then returns -1/EINTR after the handler runs.
// A child sends SIGUSR1 to the parent. Portable POSIX -> golden verdict on every engine.
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile sig_atomic_t got = 0;
static void h(int s) { (void)s; got = 1; }

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGUSR1, &sa, NULL);

    // block SIGUSR1 so we can wait for it deterministically without a race
    sigset_t block, old;
    sigemptyset(&block);
    sigaddset(&block, SIGUSR1);
    sigprocmask(SIG_BLOCK, &block, &old);

    pid_t parent = getpid();
    pid_t pid = fork();
    if (pid == 0) {
        usleep(100000);
        kill(parent, SIGUSR1);
        _exit(0);
    }
    // unblock and pause; the pending/arriving SIGUSR1 wakes us
    errno = 0;
    sigprocmask(SIG_SETMASK, &old, NULL);
    int rc = pause();
    int eintr = rc == -1 && errno == EINTR;
    waitpid(pid, NULL, 0);
    printf("pausesig got=%d eintr=%d\n", (int)got, eintr);
    return 0;
}
