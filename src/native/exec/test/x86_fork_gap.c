#include "support.h"

#include <pthread.h>
#include <stdio.h>
#include <sys/mman.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "x86_fork_gap:%d: %s\n", __LINE__, #value); return 1; } } while (0)

#if defined(__aarch64__)
typedef struct fixture {
    pthread_mutex_t mutex;
    pthread_cond_t changed;
    int entered;
    int released;
    hl_native_source_span resolved;
} fixture;

typedef struct worker {
    hl_native_executor *executor;
    hl_native_cpu *cpu;
    hl_native_run_request *request;
    hl_native_exit *output;
    hl_native_status status;
} worker;

static hl_native_status write_begin(void *opaque) {
    test_memory *memory = opaque;
    memory->begin_calls++;
    return mprotect(memory->writable, (size_t)memory->capacity, PROT_READ | PROT_WRITE) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

static hl_native_status write_end(void *opaque) {
    test_memory *memory = opaque;
    memory->end_calls++;
    __builtin___clear_cache(memory->writable, (char *)memory->writable + memory->capacity);
    return mprotect(memory->writable, (size_t)memory->capacity, PROT_READ | PROT_EXEC) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

static int resolve(void *opaque, uint64_t pc, uint64_t mapping, uint64_t epoch,
                   hl_native_source_span *output) {
    fixture *state = opaque;
    pthread_mutex_lock(&state->mutex);
    state->entered = 1;
    pthread_cond_broadcast(&state->changed);
    while (!state->released) pthread_cond_wait(&state->changed, &state->mutex);
    pthread_mutex_unlock(&state->mutex);
    if (pc != state->resolved.guest_first || mapping != state->resolved.mapping_incarnation ||
        epoch != state->resolved.instruction_epoch) return 0;
    *output = state->resolved;
    return 1;
}

static void *run_thread(void *opaque) {
    worker *state = opaque;
    state->status = hl_native_run(state->executor, state->cpu, state->request, state->output);
    return NULL;
}

static int contract(void) {
    static const uint8_t seed[] = {0xcc};
    static const uint8_t syscall[] = {0x0f, 0x05};
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    memory.write_begin = write_begin;
    memory.write_end = write_end;
    hl_native_config config = test_config(&memory, 0);
    hl_native_executor *executor = NULL;
    fixture blocked = {PTHREAD_MUTEX_INITIALIZER, PTHREAD_COND_INITIALIZER, 0, 0,
        {0x5000, syscall, sizeof(syscall), 7, 13}};
    hl_native_source_span span = {0x4000, seed, sizeof(seed), 7, 13};
    hl_native_source source = {&span, 1, 7, 13};
    hl_native_x86_64_cpu state = {.program = 0x5000, .indirect_site = 0x4444};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_X86_64, .mapping_epoch = 7, .budget = 1,
        .source = &source, .source_context = &blocked, .source_resolve = resolve};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    worker runner = {executor, &cpu, &request, &output, HL_NATIVE_STATE};
    pthread_t thread;
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    runner.executor = executor;
    CHECK(pthread_create(&thread, NULL, run_thread, &runner) == 0);
    pthread_mutex_lock(&blocked.mutex);
    while (!blocked.entered) pthread_cond_wait(&blocked.changed, &blocked.mutex);
    pthread_mutex_unlock(&blocked.mutex);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_STATE);
    hl_native_change invalidate = {.abi = HL_NATIVE_ABI, .size = sizeof(invalidate),
        .kind = HL_NATIVE_INVALIDATE, .first = 0x8000, .last = 0x9000};
    CHECK(hl_native_changed(executor, &invalidate, 1) == HL_NATIVE_OK);
    pthread_mutex_lock(&blocked.mutex);
    blocked.released = 1;
    pthread_cond_broadcast(&blocked.changed);
    pthread_mutex_unlock(&blocked.mutex);
    CHECK(pthread_join(thread, NULL) == 0 && runner.status == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_OK);
    CHECK(hl_native_after_fork(executor, 1) == HL_NATIVE_OK);
    blocked.entered = 0;
    state = (hl_native_x86_64_cpu){.program = 0x5000, .indirect_site = 0x4444};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL);
    CHECK(hl_native_destroy(executor) == HL_NATIVE_OK);
    CHECK(pthread_cond_destroy(&blocked.changed) == 0);
    CHECK(pthread_mutex_destroy(&blocked.mutex) == 0);
    CHECK(host.release_calls == 1);
    return 0;
}
#else
static int contract(void) { return 0; }
#endif

int main(void) { return contract(); }
