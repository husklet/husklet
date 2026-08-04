// pthread_sigmask gives each thread its own signal mask. A process-directed signal is delivered to
// some thread that does not block it. Block SIGUSR1 in main, leave it unblocked only in a worker,
// then kill() the process: the worker's handler must run (proving per-thread routing).
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <unistd.h>

static pthread_t worker_tid;
static volatile sig_atomic_t ran_in_worker, ran_in_other;

static void h(int s) {
    (void)s;
    if (pthread_equal(pthread_self(), worker_tid)) ran_in_worker++;
    else ran_in_other++;
}

static volatile sig_atomic_t ready, done;

static void *worker(void *arg) {
    (void)arg;
    worker_tid = pthread_self();
    sigset_t unblock;
    sigemptyset(&unblock);
    sigaddset(&unblock, SIGUSR1);
    pthread_sigmask(SIG_UNBLOCK, &unblock, NULL); // only this thread accepts SIGUSR1
    ready = 1;
    while (!done) usleep(1000);
    return NULL;
}

int main(void) {
    struct sigaction sa = {0};
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGUSR1, &sa, NULL);

    sigset_t block;
    sigemptyset(&block);
    sigaddset(&block, SIGUSR1);
    pthread_sigmask(SIG_BLOCK, &block, NULL); // main (and inherited worker) block it

    pthread_t t;
    pthread_create(&t, NULL, worker, NULL);
    while (!ready) usleep(1000);

    kill(getpid(), SIGUSR1); // process-directed -> routed to the worker
    usleep(100 * 1000);
    done = 1;
    pthread_join(t, NULL);

    printf("pthread_sigmask_route worker_only=%d\n",
           ran_in_worker == 1 && ran_in_other == 0);
    return 0;
}
