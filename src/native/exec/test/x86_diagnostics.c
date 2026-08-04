#define _POSIX_C_SOURCE 200809L
#include "support.h"
#include "../src/translation.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "x86_diagnostics:%d: %s\n", __LINE__, #value); return 1; } } while (0)
#define BENCHMARK_ITERATIONS 20000000u

#if defined(__aarch64__)
static hl_native_status executable_begin(void *opaque) {
    test_memory *memory = opaque;
    return mprotect(memory->writable, (size_t)memory->capacity, PROT_READ | PROT_WRITE) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

static hl_native_status executable_end(void *opaque) {
    test_memory *memory = opaque;
    __builtin___clear_cache((char *)memory->writable, (char *)memory->writable + memory->capacity);
    return mprotect(memory->writable, (size_t)memory->capacity, PROT_READ | PROT_EXEC) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

static int diagnostics_contract(void) {
    static const uint8_t clean[] = {0x0f, 0x05};
    static const uint8_t dirty[] = {0x66, 0x0f, 0xef, 0xc0, 0xeb, 0x02};
    hl_native_source_span spans[] = {{0x7000, clean, sizeof(clean), 7, 9},
                                     {0x7100, dirty, sizeof(dirty), 7, 9},
                                     {0x7108, clean, sizeof(clean), 7, 9}};
    hl_native_source source = {spans, 3, 7, 9};
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    memory.write_begin = executable_begin;
    memory.write_end = executable_end;
    hl_native_config config = test_config(&memory, HL_NATIVE_DIAGNOSTICS);
    hl_native_executor *executor = NULL;
    hl_native_x86_64_cpu state = {0};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_X86_64, .mapping_epoch = 7, .budget = 8, .source = &source};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    hl_native_diagnostics diagnostic = {.abi = HL_NATIVE_ABI, .size = sizeof(diagnostic)};
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    state.program = 0x7000;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK &&
          output.kind == HL_NATIVE_EXIT_SYSCALL && state.vector_dirty == 0);
    CHECK(hl_native_diagnose(executor, &diagnostic) == HL_NATIVE_OK &&
          diagnostic.x86_public_exits == 1 && diagnostic.x86_public_syscalls == 1 &&
          diagnostic.x86_syscall_vector_dirty == 0);
    state = (hl_native_x86_64_cpu){.program = 0x7100};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK &&
          output.kind == HL_NATIVE_EXIT_SYSCALL && state.program == 0x710a && state.vector_dirty == 0);
    state = (hl_native_x86_64_cpu){.program = 0x7100};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK &&
          output.kind == HL_NATIVE_EXIT_SYSCALL && state.vector_dirty == 0);
    CHECK(hl_native_diagnose(executor, &diagnostic) == HL_NATIVE_OK &&
          diagnostic.x86_public_syscalls == 3 && diagnostic.x86_syscall_vector_dirty == 1);
    state = (hl_native_x86_64_cpu){.program = 0x7000, .vector_dirty = 1};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK && state.vector_dirty == 0);
    CHECK(hl_native_diagnose(executor, &diagnostic) == HL_NATIVE_OK &&
          diagnostic.x86_public_syscalls == 4 && diagnostic.x86_syscall_vector_dirty == 1);
    struct legacy { unsigned abi, size; unsigned char body[184]; } legacy = {HL_NATIVE_ABI, 192, {0}};
    CHECK(hl_native_diagnose(executor, (hl_native_diagnostics *)&legacy) == HL_NATIVE_OK);
    hl_native_destroy(executor);
    return 0;
}

static int benchmark(int enabled) {
    static const uint8_t dirty[] = {0x66, 0x0f, 0xef, 0xc0, 0xeb, 0x02};
    static const uint8_t syscall[] = {0x0f, 0x05};
    hl_native_source_span spans[] = {{0x7100, dirty, sizeof(dirty), 7, 9},
                                     {0x7108, syscall, sizeof(syscall), 7, 9}};
    hl_native_source source = {spans, 2, 7, 9};
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    memory.write_begin = executable_begin; memory.write_end = executable_end;
    hl_native_config config = test_config(&memory, enabled ? HL_NATIVE_DIAGNOSTICS : 0);
    hl_native_executor *executor = NULL;
    hl_native_x86_64_cpu state = {0};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_X86_64, .mapping_epoch = 7, .budget = 8, .source = &source};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    hl_native_diagnostics diagnostic = {.abi = HL_NATIVE_ABI, .size = sizeof(diagnostic)};
    struct timespec start, finish;
    uint64_t checksum = 0;
    uint64_t exits = 0;
    uint64_t syscalls = 0;
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    for (unsigned warm = 0; warm < 2; ++warm) {
        state = (hl_native_x86_64_cpu){.program = 0x7100};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK &&
              output.kind == HL_NATIVE_EXIT_SYSCALL);
        exits++;
        syscalls++;
    }
    CHECK(clock_gettime(CLOCK_MONOTONIC, &start) == 0);
    for (unsigned iteration = 0; iteration < BENCHMARK_ITERATIONS; ++iteration) {
        state = (hl_native_x86_64_cpu){.program = 0x7100};
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK &&
              output.kind == HL_NATIVE_EXIT_SYSCALL);
        exits++;
        syscalls++;
        checksum += state.executed + state.program + state.vectors[0];
    }
    CHECK(clock_gettime(CLOCK_MONOTONIC, &finish) == 0);
    if (enabled) CHECK(hl_native_diagnose(executor, &diagnostic) == HL_NATIVE_OK &&
                       diagnostic.x86_public_exits == exits &&
                       diagnostic.x86_public_syscalls == syscalls);
    uint64_t ns = (uint64_t)((finish.tv_sec - start.tv_sec) * INT64_C(1000000000) +
                             finish.tv_nsec - start.tv_nsec);
    printf("mode=%s ns=%llu checksum=%llu exits=%llu syscalls=%llu dirty=%llu\n",
           enabled ? "on" : "off", (unsigned long long)ns, (unsigned long long)checksum,
           (unsigned long long)exits, (unsigned long long)syscalls,
           (unsigned long long)diagnostic.x86_syscall_vector_dirty);
    hl_native_destroy(executor);
    return 0;
}
#endif

int main(int argc, char **argv) {
#if defined(__aarch64__)
    if (argc == 2 && strcmp(argv[1], "bench-off") == 0) return benchmark(0);
    if (argc == 2 && strcmp(argv[1], "bench-on") == 0) return benchmark(1);
    return diagnostics_contract();
#else
    (void)argc; (void)argv;
    return 0;
#endif
}
