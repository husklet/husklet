#include "support.h"

#include <stdio.h>
#include <sys/mman.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "x86_diagnostics:%d: %s\n", __LINE__, #value); return 1; } } while (0)

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
    hl_native_source_span spans[] = {{0x7000, clean, sizeof(clean), 7, 9}};
    hl_native_source source = {spans, 1, 7, 9};
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
    state = (hl_native_x86_64_cpu){.program = 0x7000, .vector_dirty = 1};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK && state.vector_dirty == 0);
    CHECK(hl_native_diagnose(executor, &diagnostic) == HL_NATIVE_OK &&
          diagnostic.x86_public_syscalls == 2 && diagnostic.x86_syscall_vector_dirty == 0);
    struct legacy { unsigned abi, size; unsigned char body[184]; } legacy = {HL_NATIVE_ABI, 192, {0}};
    CHECK(hl_native_diagnose(executor, (hl_native_diagnostics *)&legacy) == HL_NATIVE_OK);
    hl_native_destroy(executor);
    return 0;
}
#endif

int main(void) {
#if defined(__aarch64__)
    return diagnostics_contract();
#else
    return 0;
#endif
}
