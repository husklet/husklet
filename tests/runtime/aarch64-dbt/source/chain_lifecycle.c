#include <pthread.h>
#include <stdint.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile uint64_t value = 1;

__attribute__((noinline)) static uint64_t branch_memory(unsigned rounds) {
    uint64_t sum = 0;
    for (unsigned i = 0; i < rounds; ++i) {
        if ((i & 1u) != 0)
            sum += value;
        else
            sum += 2;
    }
    return sum;
}

static void *thread_main(void *unused) {
    (void)unused;
    return (void *)(uintptr_t)branch_memory(20000);
}

int main(void) {
    uint64_t parent = branch_memory(20000);
    pid_t child = fork();
    if (child == 0) _exit(branch_memory(20000) == parent ? 0 : 1);
    int status = 0;
    if (child < 0 || waitpid(child, &status, 0) != child || status != 0) return 2;
    pthread_t thread;
    if (pthread_create(&thread, 0, thread_main, 0) != 0) return 3;
    void *answer = 0;
    if (pthread_join(thread, &answer) != 0 || (uint64_t)(uintptr_t)answer != parent) return 4;
    return parent == 30000 ? 42 : 5;
}
