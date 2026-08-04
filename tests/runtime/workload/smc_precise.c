#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>

enum { SLOT_WORDS = 32, FUNCTIONS = 12, ITERATIONS = 200000 };
static uint32_t *arena;
static atomic_int start, failed;

static void write_function(unsigned index, uint16_t value) {
    uint32_t *code = arena + index * SLOT_WORDS;
    code[0] = 0x52800000u | ((uint32_t)value << 5);
    code[1] = 0xd65f03c0u;
}
static uint32_t call_function(unsigned index) {
    uint32_t (*function)(void) = (uint32_t (*)(void))(arena + index * SLOT_WORDS);
    return function();
}
__attribute__((noinline)) static void queue_line(void *line) {
    __asm__ volatile("ic ivau, %0" : : "r"(line) : "memory");
}
__attribute__((noinline)) static void commit_lines(void) {
    __asm__ volatile("dsb ish\n\tisb" : : : "memory");
}
static void publish_disjoint_lines(void) {
    for (unsigned i = 0; i < 9; i++) queue_line(arena + i * SLOT_WORDS);
    commit_lines();
}
static void *unrelated_reader(void *unused) {
    (void)unused;
    while (!atomic_load_explicit(&start, memory_order_acquire)) {}
    for (unsigned i = 0; i < ITERATIONS; i++)
        if (call_function(FUNCTIONS - 1) != 0x4567)
            atomic_store_explicit(&failed, 1, memory_order_relaxed);
    return NULL;
}

int main(void) {
    size_t bytes = FUNCTIONS * SLOT_WORDS * sizeof(*arena);
    arena = mmap(NULL, bytes, PROT_READ | PROT_WRITE | PROT_EXEC,
                 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (arena == MAP_FAILED) return 1;
    for (unsigned i = 0; i < FUNCTIONS - 1; i++) write_function(i, (uint16_t)(100 + i));
    write_function(FUNCTIONS - 1, 0x4567);
    __builtin___clear_cache((char *)arena, (char *)arena + bytes);
    for (unsigned i = 0; i < FUNCTIONS; i++) (void)call_function(i);

    pthread_t reader;
    pthread_create(&reader, NULL, unrelated_reader, NULL);
    atomic_store_explicit(&start, 1, memory_order_release);
    for (unsigned i = 0; i < 9; i++) write_function(i, (uint16_t)(900 + i));
    publish_disjoint_lines();
    for (unsigned i = 0; i < 9; i++)
        if (call_function(i) != 900 + i) atomic_store(&failed, 1);

    for (unsigned i = 0; i < 4; i++) write_function(i, (uint16_t)(1200 + i));
    __builtin___clear_cache((char *)arena, (char *)(arena + 4 * SLOT_WORDS));
    for (unsigned i = 0; i < 4; i++)
        if (call_function(i) != 1200 + i) atomic_store(&failed, 1);

    pthread_join(reader, NULL);
    printf("smc_precise %s\n", atomic_load(&failed) ? "fail" : "ok");
    return atomic_load(&failed) ? 1 : 0;
}
