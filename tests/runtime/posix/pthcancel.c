// pthread deferred cancellation at a cancellation point + cleanup handler ordering.
#define _GNU_SOURCE
#include <pthread.h>
#include <stdio.h>
#include <unistd.h>

static int cleanup_run = 0;
static int order = 0;
static int c1 = 0, c2 = 0;

static void cleanup(void *arg) {
    int *slot = (int *)arg;
    *slot = ++order;
    cleanup_run++;
}

static void *worker(void *arg) {
    (void)arg;
    pthread_setcancelstate(PTHREAD_CANCEL_ENABLE, NULL);
    pthread_setcanceltype(PTHREAD_CANCEL_DEFERRED, NULL);
    pthread_cleanup_push(cleanup, &c2);
    pthread_cleanup_push(cleanup, &c1);
    while (1) {
        pause(); // cancellation point
    }
    pthread_cleanup_pop(0);
    pthread_cleanup_pop(0);
    return NULL;
}

int main(void) {
    pthread_t t;
    pthread_create(&t, NULL, worker, NULL);
    usleep(50000);
    pthread_cancel(t);
    void *rv = NULL;
    pthread_join(t, &rv);
    int canceled = rv == PTHREAD_CANCELED;
    // Cleanup handlers run LIFO: the last pushed (c1) runs first.
    printf("cancel joined=%d canceled=%d cleanups=%d lifo=%d\n",
           1, canceled, cleanup_run, c1 == 1 && c2 == 2);
    return 0;
}
