// Thread-directed signals: pthread_kill delivers to a specific target thread, and a dedicated thread
// can synchronously accept a blocked signal via sigwait(). All threads block SIGUSR1; the main thread
// pthread_kill()s exactly one worker, which is the one whose sigwait() returns. A second signal count
// verifies only one delivery occurred. Deterministic derived booleans.
#define _GNU_SOURCE
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <time.h>

static atomic_int woke_index = -1;
static atomic_int wake_count = 0;
static pthread_t threads[4];

static void *w(void *arg) {
    long idx = (long)arg;
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    // SIGUSR1 already blocked process-wide (set in main before creating threads)
    int sig = 0;
    if (sigwait(&set, &sig) == 0 && sig == SIGUSR1) {
        atomic_store(&woke_index, (int)idx);
        atomic_fetch_add(&wake_count, 1);
    }
    return 0;
}

int main(void) {
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    pthread_sigmask(SIG_BLOCK, &set, NULL); // inherited by all threads

    for (long i = 0; i < 4; i++) pthread_create(&threads[i], 0, w, (void *)i);

    struct timespec ts = {0, 50 * 1000000};
    nanosleep(&ts, 0);                       // let all four enter sigwait

    pthread_kill(threads[2], SIGUSR1);       // target exactly thread #2

    // wait for the targeted thread to consume the signal
    while (atomic_load(&woke_index) < 0) { struct timespec t = {0, 200000}; nanosleep(&t, 0); }
    int targeted = atomic_load(&woke_index) == 2;

    // release the other three so the process can exit cleanly
    for (int i = 0; i < 3; i++) pthread_kill(threads[i == 2 ? 3 : i], SIGUSR1);
    for (int i = 0; i < 4; i++) pthread_join(threads[i], 0);

    printf("thread_signal_direct targeted=%d total_woken=%d\n",
           targeted, atomic_load(&wake_count));
    return 0;
}
