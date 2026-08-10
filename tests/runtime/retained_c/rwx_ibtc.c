#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <time.h>

#define CALLS 4000000

typedef uint64_t (*target_fn)(uint64_t);

__attribute__((noinline)) static uint64_t target(uint64_t value) {
    return value + 1;
}

__asm__(
    ".text\n"
    ".global call_br_x16\n"
    "call_br_x16:\n mov x16, x1\n br x16\n"
    ".global call_ldr_br_x16\n"
    "call_ldr_br_x16:\n ldr x16, [x1]\n br x16\n"
    ".global call_br_x17\n"
    "call_br_x17:\n mov x17, x1\n br x17\n"
    ".global call_ldr_br_x17\n"
    "call_ldr_br_x17:\n ldr x17, [x1]\n br x17\n"
    ".global call_blr_x30\n"
    "call_blr_x30:\n stp x19, x30, [sp, #-16]!\n mov x19, x30\n mov x30, x1\n blr x30\n mov x30, x19\n ldp x19, x30, [sp], #16\n ret\n");

extern uint64_t call_br_x16(uint64_t, target_fn);
extern uint64_t call_ldr_br_x16(uint64_t, target_fn *);
extern uint64_t call_br_x17(uint64_t, target_fn);
extern uint64_t call_ldr_br_x17(uint64_t, target_fn *);
extern uint64_t call_blr_x30(uint64_t, target_fn);

int main(void) {
    void *rwx = mmap(NULL, 4096, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (rwx == MAP_FAILED) return 2;
    target_fn pointer = target;
    uint64_t sum = 0;
    clock_t begin = clock();
    for (uint64_t index = 0; index < CALLS; ++index) {
        sum += call_br_x16(index, target);
        sum += call_ldr_br_x16(index, &pointer);
        sum += call_br_x17(index, target);
        sum += call_ldr_br_x17(index, &pointer);
        sum += call_blr_x30(index, target);
    }
    double seconds = (double)(clock() - begin) / CLOCKS_PER_SEC;
    munmap(rwx, 4096);
    if (seconds > 8.0) {
        printf("rwx-ibtc slow\n");
        return 3;
    }
    printf("rwx-ibtc sum=%llu\n", (unsigned long long)sum);
    return 0;
}
