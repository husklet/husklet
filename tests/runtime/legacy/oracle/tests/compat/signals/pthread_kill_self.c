// pthread_kill directs a signal at a specific thread. Sending to pthread_self runs the handler on
// this very thread synchronously (the signal is not blocked). Sending signal 0 is a validity probe
// returning 0.
#include <pthread.h>
#include <signal.h>
#include <stdio.h>

static volatile sig_atomic_t ran;
static pthread_t who;
static void h(int s) { (void)s; ran++; who = pthread_self(); }

int main(void) {
    struct sigaction sa = {0};
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGUSR1, &sa, NULL);

    int probe = pthread_kill(pthread_self(), 0);
    int r = pthread_kill(pthread_self(), SIGUSR1);
    int on_self = ran == 1 && pthread_equal(who, pthread_self());

    printf("pthread_kill_self probe0=%d sent=%d on_self=%d\n",
           probe == 0, r == 0, on_self);
    return 0;
}
