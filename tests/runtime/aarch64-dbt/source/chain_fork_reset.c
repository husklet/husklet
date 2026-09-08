#include <pthread.h>
#include <stdint.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile uint64_t value = 1;
static volatile int ready;
static volatile int release_peer;

__attribute__((noinline)) static uint64_t branch_memory(unsigned rounds) {
    uint64_t sum = 0;
    for (unsigned i = 0; i < rounds; ++i) sum += (i & 1u) ? value : 2;
    return sum;
}

static void *peer(void *unused) {
    (void)unused;
    ready = 1;
    while (!release_peer) __asm__ volatile("yield" ::: "memory");
    return 0;
}

int main(void) {
    uint64_t expected = branch_memory(20000);
    pthread_t thread;
    if (pthread_create(&thread, 0, peer, 0) != 0) return 2;
    while (!ready) __asm__ volatile("yield" ::: "memory");
    pid_t child = fork();
    if (child == 0) _exit(branch_memory(20000) == expected ? 0 : 3);
    int status = 0;
    if (child < 0 || waitpid(child, &status, 0) != child || status != 0) return 4;
    release_peer = 1;
    if (pthread_join(thread, 0) != 0) return 5;
    return 42;
}
