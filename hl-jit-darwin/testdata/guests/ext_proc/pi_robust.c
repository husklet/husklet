// PI (priority-inheritance) mutex + robust mutex: the two futex-op families hl used to FAKE.
//
//   * PTHREAD_PRIO_INHERIT mutex -> FUTEX_LOCK_PI/UNLOCK_PI under contention. hl's old "other ops -> return 0"
//     fake-acquired WITHOUT blocking, so two threads could both "own" it and race the counter -> a sum below
//     the total. Real mutual exclusion => the sum is exactly N.
//   * PTHREAD_MUTEX_ROBUST mutex whose owner dies still holding it: set_robust_list + the OWNER_DIED handoff
//     must let the next locker recover with EOWNERDEAD. hl's old no-op set_robust_list left the word owned by
//     a dead thread forever -> the next lock deadlocked.
//
// .out() golden (NOT .oracle()): qemu-user x86_64 -- the x86 oracle on an aarch64 host -- cannot run PI
// futexes (hangs), so the golden below is the correct native-Linux result, checked on both hl Linux engines.
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdio.h>

#define NTHREAD 4
#define NITER 2000 // 4*2000 = 8000; enough contention to catch a double-owner, light enough for the x86 DBT

static pthread_mutex_t pim;
static long pisum = 0;

static void *pi_worker(void *a) {
    (void)a;
    for (int i = 0; i < NITER; i++) {
        pthread_mutex_lock(&pim);
        pisum++; // guarded by the PI mutex; a fake-acquire would let two threads race and lose increments
        pthread_mutex_unlock(&pim);
    }
    return NULL;
}

static pthread_mutex_t rbm;

static void *rob_owner(void *a) {
    (void)a;
    pthread_mutex_lock(&rbm); // lock a ROBUST mutex and exit WITHOUT unlocking -> next locker gets EOWNERDEAD
    return NULL;
}

int main(void) {
    // --- PI mutex under contention ---
    pthread_mutexattr_t pa;
    pthread_mutexattr_init(&pa);
    pthread_mutexattr_setprotocol(&pa, PTHREAD_PRIO_INHERIT);
    pthread_mutex_init(&pim, &pa);
    pthread_t pw[NTHREAD];
    for (int i = 0; i < NTHREAD; i++) pthread_create(&pw[i], NULL, pi_worker, NULL);
    for (int i = 0; i < NTHREAD; i++) pthread_join(pw[i], NULL);
    printf("pi_mutex sum=%ld\n", pisum);

    // --- robust mutex: owner dies holding it, next lock recovers via EOWNERDEAD ---
    pthread_mutexattr_t ra;
    pthread_mutexattr_init(&ra);
    pthread_mutexattr_setrobust(&ra, PTHREAD_MUTEX_ROBUST);
    pthread_mutex_init(&rbm, &ra);
    pthread_t ro;
    pthread_create(&ro, NULL, rob_owner, NULL);
    pthread_join(ro, NULL); // owner has exited still holding rbm
    int rc = pthread_mutex_lock(&rbm);
    int eod = (rc == EOWNERDEAD);
    if (eod) pthread_mutex_consistent(&rbm);
    pthread_mutex_unlock(&rbm);
    printf("robust eownerdead=%d\n", eod);
    return 0;
}
