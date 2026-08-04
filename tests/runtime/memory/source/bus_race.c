#define _GNU_SOURCE
#include <pthread.h>
#include <setjmp.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

enum { PAGE_SIZE = 4096, ROUNDS = 1000 };

static _Atomic int ready;
static _Atomic int phase;
static _Atomic unsigned faults;
static _Atomic unsigned escaped;
static volatile unsigned char *target;
static _Thread_local sigjmp_buf fault_jump;
static _Thread_local int fault_armed;

static void on_bus(int signal_number) {
    (void)signal_number;
    if (!fault_armed) _exit(90);
    atomic_fetch_add_explicit(&faults, 1, memory_order_relaxed);
    siglongjmp(fault_jump, 1);
}

__attribute__((noinline)) static unsigned char hot_load(void) { return *target; }

static void *accessor(void *unused) {
    (void)unused;
    unsigned char value = 0;
    for (unsigned i = 0; i < 200000; ++i) value ^= hot_load();
    fault_armed = 1;
    atomic_store_explicit(&ready, 1, memory_order_release);
    while (atomic_load_explicit(&phase, memory_order_acquire) >= 0) {
        int observed_phase = atomic_load_explicit(&phase, memory_order_acquire);
        int expected_bus = observed_phase > 0 && (observed_phase & 1) != 0;
        if (sigsetjmp(fault_jump, 1) == 0) {
            value ^= hot_load();
            if (expected_bus && atomic_load_explicit(&phase, memory_order_acquire) == observed_phase)
                atomic_fetch_add_explicit(&escaped, 1, memory_order_relaxed);
        }
    }
    fault_armed = 0;
    return (void *)(uintptr_t)value;
}

static void *blocked_reader(void *argument) {
    char byte;
    return (void *)(intptr_t)(read(*(int *)argument, &byte, 1) == 1 && byte == 'x' ? 0 : 1);
}

struct toggler {
    void *base;
    int descriptor;
    _Atomic int done;
    int ok;
};

static void *toggle_mapping(void *argument) {
    struct toggler *toggle = argument;
    toggle->ok = 1;
    for (int i = 0; i < 16; ++i) {
        if (mmap(toggle->base, PAGE_SIZE * 2, PROT_READ | PROT_WRITE,
                 MAP_FIXED | MAP_PRIVATE, toggle->descriptor, 0) != toggle->base ||
            mmap(toggle->base, PAGE_SIZE * 2, PROT_READ | PROT_WRITE,
                 MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) != toggle->base) {
            toggle->ok = 0;
            break;
        }
    }
    atomic_store_explicit(&toggle->done, 1, memory_order_release);
    return NULL;
}

int main(void) {
    struct sigaction action = {0};
    char path[] = "/tmp/hl-bus-race-XXXXXX";
    unsigned char page[PAGE_SIZE];
    pthread_t worker, reader;
    int pipefd[2];
    int descriptor = mkstemp(path);
    memset(page, 0x5a, sizeof page);
    if (descriptor < 0 || write(descriptor, page, sizeof page) != (ssize_t)sizeof page) return 1;
    target = mmap(NULL, PAGE_SIZE * 2, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (target == MAP_FAILED) return 2;
    target += PAGE_SIZE;
    action.sa_handler = on_bus;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGBUS, &action, NULL) != 0 || pthread_create(&worker, NULL, accessor, NULL) != 0) return 3;
    while (!atomic_load_explicit(&ready, memory_order_acquire)) sched_yield();

    int blocked_ok = 0;
    if (pipe(pipefd) == 0 && pthread_create(&reader, NULL, blocked_reader, &pipefd[0]) == 0) {
        usleep(1000);
        void *base = (void *)(target - PAGE_SIZE);
        if (mmap(base, PAGE_SIZE * 2, PROT_READ | PROT_WRITE, MAP_FIXED | MAP_PRIVATE, descriptor, 0) == base) {
            atomic_store_explicit(&phase, 1, memory_order_release);
            while (atomic_load_explicit(&faults, memory_order_acquire) == 0) sched_yield();
            if (write(pipefd[1], "x", 1) == 1) {
                void *reader_result = NULL;
                blocked_ok = pthread_join(reader, &reader_result) == 0 && reader_result == NULL;
            }
            atomic_store_explicit(&phase, 2, memory_order_release);
            mmap(base, PAGE_SIZE * 2, PROT_READ | PROT_WRITE,
                 MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            atomic_store_explicit(&phase, 2, memory_order_release);
        }
        close(pipefd[0]);
        close(pipefd[1]);
    }

    unsigned completed = 0;
    for (unsigned round = 0; round < ROUNDS; ++round) {
        void *base = (void *)(target - PAGE_SIZE);
        unsigned before = atomic_load_explicit(&faults, memory_order_acquire);
        if (mmap(base, PAGE_SIZE * 2, PROT_READ | PROT_WRITE, MAP_FIXED | MAP_PRIVATE, descriptor, 0) != base)
            break;
        int bus_phase = 3 + (int)(round * 2);
        atomic_store_explicit(&phase, bus_phase, memory_order_release);
        while (atomic_load_explicit(&faults, memory_order_acquire) == before) sched_yield();
        atomic_store_explicit(&phase, bus_phase + 1, memory_order_release);
        if (mmap(base, PAGE_SIZE * 2, PROT_READ | PROT_WRITE,
                 MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) != base)
            break;
        completed++;
    }
    atomic_store_explicit(&phase, -1, memory_order_release);
    pthread_join(worker, NULL);

    /* Keep one BUS page live while cycling more than the filter's practical
       working set through distinct virtual pages.  The final unrelated load
       and persistent BUS prove that filter maintenance neither saturates into
       a global slow path nor loses a live interval during rebuild. */
    size_t churn_size = (size_t)ROUNDS * PAGE_SIZE * 3u;
    unsigned char *churn = mmap(NULL, churn_size, PROT_READ | PROT_WRITE,
                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    unsigned char *persistent = mmap(NULL, PAGE_SIZE * 2, PROT_READ | PROT_WRITE,
                                     MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    int churn_ok = churn != MAP_FAILED && persistent != MAP_FAILED;
    if (churn_ok && mmap(persistent, PAGE_SIZE * 2, PROT_READ | PROT_WRITE,
                         MAP_FIXED | MAP_PRIVATE, descriptor, 0) != persistent)
        churn_ok = 0;
    for (unsigned round = 0; churn_ok && round < ROUNDS; ++round) {
        void *base = churn + (size_t)round * PAGE_SIZE * 3u;
        if (mmap(base, PAGE_SIZE * 2, PROT_READ | PROT_WRITE,
                 MAP_FIXED | MAP_PRIVATE, descriptor, 0) != base ||
            mmap(base, PAGE_SIZE * 2, PROT_READ | PROT_WRITE,
                 MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) != base)
            churn_ok = 0;
    }
    if (churn_ok) {
        target = persistent + PAGE_SIZE;
        fault_armed = 1;
        if (sigsetjmp(fault_jump, 1) == 0) {
            (void)hot_load();
            churn_ok = 0;
        }
        fault_armed = 0;
        churn_ok = churn_ok && churn[0] == 0;
    }
    if (churn != MAP_FAILED) munmap(churn, churn_size);
    if (persistent != MAP_FAILED) munmap(persistent, PAGE_SIZE * 2);

    void *second = mmap(NULL, PAGE_SIZE * 2, PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    struct toggler toggles[2] = {{(void *)(target - PAGE_SIZE), descriptor, 0, 0},
                                 {second, descriptor, 0, 0}};
    pthread_t toggle_threads[2];
    int concurrent_ok = second != MAP_FAILED &&
                        pthread_create(&toggle_threads[0], NULL, toggle_mapping, &toggles[0]) == 0 &&
                        pthread_create(&toggle_threads[1], NULL, toggle_mapping, &toggles[1]) == 0;
    int fork_ok = concurrent_ok;
    int forks = 0;
    while (fork_ok && forks < 16 &&
           (!atomic_load_explicit(&toggles[0].done, memory_order_acquire) ||
            !atomic_load_explicit(&toggles[1].done, memory_order_acquire))) {
        pid_t child = fork();
        int status = 0;
        if (child == 0) _exit(0);
        if (child < 0 || waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            fork_ok = 0;
            break;
        }
        forks++;
    }
    if (concurrent_ok) {
        pthread_join(toggle_threads[0], NULL);
        pthread_join(toggle_threads[1], NULL);
        concurrent_ok = toggles[0].ok && toggles[1].ok;
    }
    printf("bus-race rounds=%d escaped=%d blocked=%d churn=%d concurrent=%d fork=%d\n", completed == ROUNDS,
           atomic_load_explicit(&escaped, memory_order_relaxed) == 0, blocked_ok, churn_ok, concurrent_ok, fork_ok);
    munmap((void *)(target - PAGE_SIZE), PAGE_SIZE * 2);
    if (second != MAP_FAILED) munmap(second, PAGE_SIZE * 2);
    close(descriptor);
    unlink(path);
    return completed == ROUNDS && atomic_load_explicit(&escaped, memory_order_relaxed) == 0 && blocked_ok &&
           churn_ok && concurrent_ok && fork_ok
               ? 0
               : 4;
}
