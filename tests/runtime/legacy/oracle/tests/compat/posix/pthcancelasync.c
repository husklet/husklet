// pthread asynchronous cancellation: a tight non-cancellation-point loop is still cancelable.
#define _GNU_SOURCE
#include <pthread.h>
#include <stdio.h>
#include <unistd.h>

static volatile unsigned long spins = 0;

static void *worker(void *arg) {
    (void)arg;
    pthread_setcancelstate(PTHREAD_CANCEL_ENABLE, NULL);
    pthread_setcanceltype(PTHREAD_CANCEL_ASYNCHRONOUS, NULL);
    for (;;) spins++;
    return NULL;
}

int main(void) {
    pthread_t t;
    pthread_create(&t, NULL, worker, NULL);
    usleep(50000);
    pthread_cancel(t);
    void *rv = NULL;
    int joined = pthread_join(t, &rv) == 0;
    printf("async joined=%d canceled=%d progressed=%d\n",
           joined, rv == PTHREAD_CANCELED, spins > 0);
    return 0;
}
