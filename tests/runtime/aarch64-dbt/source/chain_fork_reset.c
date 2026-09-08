#include <stdint.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile uint64_t value = 1;

__attribute__((noinline)) static uint64_t branch_memory(unsigned rounds) {
    uint64_t sum = 0;
    for (unsigned i = 0; i < rounds; ++i) sum += (i & 1u) ? value : 2;
    return sum;
}

__attribute__((cold, noinline)) static uint64_t cold_target(void) {
    return 9;
}

__attribute__((noinline)) static uint64_t cold_edge(int take) {
    if (take) return cold_target();
    return 7;
}

int main(void) {
    uint64_t expected = branch_memory(20000);
    if (cold_edge(0) != 7) return 2;
    pid_t child = fork();
    if (child == 0) {
        _exit(branch_memory(20000) == expected && cold_edge(1) == 9 ? 0 : 3);
    }
    int status = 0;
    if (child < 0 || waitpid(child, &status, 0) != child || status != 0) return 4;
    return 42;
}
