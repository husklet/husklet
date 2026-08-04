#define _GNU_SOURCE
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <time.h>
#include <unistd.h>

#define PATTERN_A 0x11223344u
#define PATTERN_B 0x55667788u
#define PROBERS 8
#define BUDGET_NS 1500000000LL

static _Alignas(64) volatile unsigned word = PATTERN_A;
static atomic_int stop, torn;
static atomic_uint witness;

static long long now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

static void *writer(void *unused) {
    (void)unused;
    long long deadline = now_ns() + BUDGET_NS;
    for (long i = 0; !atomic_load(&stop); i++) {
        __atomic_store_n((unsigned *)&word, (i & 1) ? PATTERN_B : PATTERN_A, __ATOMIC_SEQ_CST);
        if ((i & 0xffff) == 0 && now_ns() >= deadline) break;
    }
    atomic_store(&stop, 1);
    return 0;
}

static void *prober(void *argument) {
    int fd = *(int *)argument;
    while (!atomic_load(&stop))
        (void)!read(fd, (void *)&word, sizeof word);
    return 0;
}

static void *observer(void *unused) {
    (void)unused;
    while (!atomic_load(&stop)) {
        unsigned value = __atomic_load_n((unsigned *)&word, __ATOMIC_SEQ_CST);
        if (value != PATTERN_A && value != PATTERN_B) {
            atomic_store(&witness, value);
            atomic_store(&torn, 1);
            atomic_store(&stop, 1);
        }
    }
    return 0;
}

int main(void) {
    int fd = open("/dev/null", O_RDONLY);
    if (fd < 0) return 1;
    pthread_t w, p[PROBERS], o;
    pthread_create(&w, 0, writer, 0);
    for (int i = 0; i < PROBERS; i++)
        pthread_create(&p[i], 0, prober, &fd);
    pthread_create(&o, 0, observer, 0);
    pthread_join(w, 0);
    for (int i = 0; i < PROBERS; i++)
        pthread_join(p[i], 0);
    pthread_join(o, 0);
    close(fd);
    if (atomic_load(&torn))
        fprintf(stderr, "witness=0x%08x (neither 0x%08x nor 0x%08x)\n", atomic_load(&witness), PATTERN_A, PATTERN_B);
    printf("uaccess_write_probe intact=%d\n", atomic_load(&torn) == 0);
    return 0;
}
