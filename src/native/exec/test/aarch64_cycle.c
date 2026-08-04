#include "../include/executor.h"
#include "../src/arch/aarch64/projection.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "aarch64-cycle:%d: %s\n", __LINE__, #x); return 1; } } while (0)

typedef struct executable_memory { uint8_t *address; uint64_t capacity; } executable_memory;

static hl_native_status reserve(void *opaque, uint64_t capacity, uint64_t alignment,
                                uint32_t dual, hl_native_mapping *output) {
    executable_memory *memory = opaque;
    (void)alignment;
    if (dual != 0) return HL_NATIVE_PLATFORM;
    void *address = mmap(NULL, (size_t)capacity, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (address == MAP_FAILED) return HL_NATIVE_MEMORY;
    memory->address = address;
    memory->capacity = capacity;
    *output = (hl_native_mapping){.abi = HL_NATIVE_ABI, .size = sizeof(*output), .handle = 1,
        .writable = (uint64_t)(uintptr_t)address, .executable = (uint64_t)(uintptr_t)address,
        .capacity = capacity};
    return HL_NATIVE_OK;
}

static hl_native_status release(void *opaque, hl_native_handle handle) {
    executable_memory *memory = opaque;
    if (handle != 1 || memory->address == NULL) return HL_NATIVE_ARGUMENT;
    if (munmap(memory->address, (size_t)memory->capacity) != 0) return HL_NATIVE_PLATFORM;
    memory->address = NULL;
    return HL_NATIVE_OK;
}

static hl_native_status publish(void *opaque, hl_native_handle handle, uint64_t offset, uint64_t size) {
    executable_memory *memory = opaque;
    return handle == 1 && offset <= memory->capacity && size <= memory->capacity - offset
        ? HL_NATIVE_OK : HL_NATIVE_ARGUMENT;
}

static hl_native_status repair(void *opaque, hl_native_mapping *mapping, uint32_t preserve) {
    executable_memory *memory = opaque;
    (void)preserve;
    return mapping->handle == 1 && mapping->writable == (uint64_t)(uintptr_t)memory->address
        ? HL_NATIVE_OK : HL_NATIVE_ARGUMENT;
}

static hl_native_status begin(void *opaque) {
    executable_memory *memory = opaque;
    return mprotect(memory->address, (size_t)memory->capacity, PROT_READ | PROT_WRITE) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

static hl_native_status end(void *opaque) {
    executable_memory *memory = opaque;
    __builtin___clear_cache((char *)memory->address, (char *)memory->address + memory->capacity);
    return mprotect(memory->address, (size_t)memory->capacity, PROT_READ | PROT_EXEC) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    executable_memory host = {0};
    hl_native_memory memory = {.abi = HL_NATIVE_ABI, .size = sizeof(memory), .context = &host,
        .reserve = reserve, .release = release, .publish = publish, .repair = repair,
        .write_begin = begin, .write_end = end};
    hl_native_config config = {.abi = HL_NATIVE_ABI, .size = sizeof(config),
        .capacity = 64u << 20, .alignment = 4096, .memory = &memory};
    hl_native_executor *executor = NULL;
    hl_native_change replace = {.abi = HL_NATIVE_ABI, .size = sizeof(replace),
                                .kind = HL_NATIVE_REPLACE, .mapping_epoch = 7};
    hl_native_aarch64_cpu state = {0};
    static _Alignas(16) uint8_t stack[4096];
    uint64_t stack_first = (uint64_t)(uintptr_t)stack;
    uint64_t stack_last = (uint64_t)(uintptr_t)(stack + sizeof(stack));
    const hl_a64_view view = {stack_first, stack_last, stack_first, 7,
                              HL_A64_PERMISSION_READ | HL_A64_PERMISSION_WRITE, 0};
    const hl_a64_projection projection = {&view, 1, 7, 0};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
                         .architecture = HL_NATIVE_AARCH64, .state.aarch64 = &state};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
                                     .architecture = HL_NATIVE_AARCH64, .mapping_epoch = 7,
                                     .projection = &projection};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);

    const uint32_t cycle_words[] = {
        UINT32_C(0x14000002), /* b 0xc108 */
        UINT32_C(0xd503201f),
        UINT32_C(0x17fffffe), /* b 0xc100 */
    };
    const hl_native_source_span cycle_span = {
        0xc100, (const uint8_t *)cycle_words, sizeof(cycle_words), 7, 8};
    const hl_native_source cycle_source = {&cycle_span, 1, 7, 8};
    request.source = &cycle_source;
    request.budget = 3;
    state.program = 0xc100;
    state.stack = stack_last;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD && state.executed == 3 && state.budget == 0);
    request.budget = 3;
    state.interrupt = 1;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_INTERRUPT && state.executed == 0 && state.budget == 3);

    const uint32_t syscall_words[] = {
        UINT32_C(0xd2801640), /* mov x0,#178 */
        UINT32_C(0x94000005), /* bl 0xd018 */
        UINT32_C(0xf1000673), /* subs x19,x19,#1 */
        UINT32_C(0x8b000294), /* add x20,x20,x0 */
        UINT32_C(0x54ffff81), /* b.ne 0xd000 */
        UINT32_C(0xd4000001), /* final svc */
        UINT32_C(0xd4000001), /* loop svc */
        UINT32_C(0xd65f03c0), /* ret */
    };
    const hl_native_source_span syscall_span = {
        0xd000, (const uint8_t *)syscall_words, sizeof(syscall_words), 7, 8};
    const hl_native_source syscall_source = {&syscall_span, 1, 7, 8};
    request.source = &syscall_source;
    request.budget = 16;
    memset(&state, 0, sizeof(state));
    state.program = 0xd000;
    state.stack = stack_last;
    state.registers[19] = 3;
    for (uint64_t iteration = 0; iteration < 3; ++iteration) {
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
        CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && state.program == 0xd018);
        state.registers[0] = iteration + 1;
        state.program += 4;
    }
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && state.program == 0xd014 &&
          state.registers[19] == 0 && state.registers[20] == 6);

    CHECK(hl_native_destroy(executor) == HL_NATIVE_OK);
    CHECK(host.address == NULL);
    return 0;
#endif
}
