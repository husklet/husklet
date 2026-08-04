// Per-thread names via pthread_setname_np / pthread_getname_np (backed by prctl(PR_SET_NAME) on the
// calling thread and /proc/<tid>/comm). Each of 6 threads sets a distinct <=15-char name and reads it
// back; the main thread's name is set and read independently. Deterministic count of correct roundtrips.
#define _GNU_SOURCE
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>

static atomic_int ok = 0;

static void *w(void *arg) {
    long idx = (long)arg;
    char want[16], got[16] = {0};
    snprintf(want, sizeof want, "hlworker%ld", idx);
    if (pthread_setname_np(pthread_self(), want) == 0 &&
        pthread_getname_np(pthread_self(), got, sizeof got) == 0 &&
        strcmp(want, got) == 0)
        atomic_fetch_add(&ok, 1);
    return 0;
}

int main(void) {
    pthread_setname_np(pthread_self(), "hlmain");
    char mgot[16] = {0};
    int main_ok = pthread_getname_np(pthread_self(), mgot, sizeof mgot) == 0 &&
                  strcmp(mgot, "hlmain") == 0;

    pthread_t t[6];
    for (long i = 0; i < 6; i++) pthread_create(&t[i], 0, w, (void *)i);
    for (int i = 0; i < 6; i++) pthread_join(t[i], 0);

    printf("thread_setname main_ok=%d worker_ok=%d\n", main_ok, atomic_load(&ok) == 6);
    return 0;
}
