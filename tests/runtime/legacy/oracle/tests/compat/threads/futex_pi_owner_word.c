#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#define FUTEX_LOCK_PI 6
#define FUTEX_UNLOCK_PI 7
#define FUTEX_PRIVATE_FLAG 128
#define THREADS 16
#define ITERS 500

static _Alignas(64) int word;
static long counter;
static atomic_int owner_bad, lock_bad, unlock_bad;

static long pi_lock(void) {
    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += 3;
    return syscall(SYS_futex, &word, FUTEX_LOCK_PI | FUTEX_PRIVATE_FLAG, 0, &deadline, 0, 0);
}

static void *worker(void *unused) {
    (void)unused;
    unsigned tid = (unsigned)syscall(SYS_gettid);
    for (int i = 0; i < ITERS; i++) {
        if (pi_lock() != 0) {
            atomic_store(&lock_bad, errno ? errno : 1);
            return 0;
        }
        unsigned held = __atomic_load_n((unsigned *)&word, __ATOMIC_SEQ_CST);
        if ((held & 0x3fffffffu) != tid) atomic_store(&owner_bad, (int)(held & 0x3fffffffu));
        counter++;
        if (syscall(SYS_futex, &word, FUTEX_UNLOCK_PI | FUTEX_PRIVATE_FLAG, 0, 0, 0, 0) != 0) {
            atomic_store(&unlock_bad, errno ? errno : 1);
            return 0;
        }
    }
    return 0;
}

int main(void) {
    pthread_t threads[THREADS];
    for (int i = 0; i < THREADS; i++)
        pthread_create(&threads[i], 0, worker, 0);
    for (int i = 0; i < THREADS; i++)
        pthread_join(threads[i], 0);
    if (atomic_load(&owner_bad) || atomic_load(&lock_bad) || atomic_load(&unlock_bad))
        fprintf(stderr, "owner_seen=%d lock_errno=%d unlock_errno=%d final_word=0x%08x\n", atomic_load(&owner_bad),
                atomic_load(&lock_bad), atomic_load(&unlock_bad), (unsigned)word);
    printf("futex_pi_owner_word owner_ok=%d lock_ok=%d unlock_ok=%d total_ok=%d\n", atomic_load(&owner_bad) == 0,
           atomic_load(&lock_bad) == 0, atomic_load(&unlock_bad) == 0, counter == (long)THREADS * ITERS);
    return 0;
}
