#include "support.h"
#include "../src/executor.h"

#include <stdio.h>
#include <sys/mman.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "x86_loop_accounting:%d: %s\n", __LINE__, #value); return 1; } } while (0)

#if defined(__aarch64__)
static hl_native_status executable_begin(void *opaque) {
    test_memory *memory = opaque;
    memory->begin_calls++;
    return mprotect(memory->writable, (size_t)memory->capacity, PROT_READ | PROT_WRITE) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

static hl_native_status executable_end(void *opaque) {
    test_memory *memory = opaque;
    memory->end_calls++;
    __builtin___clear_cache((char *)memory->writable, (char *)memory->writable + memory->capacity);
    return mprotect(memory->writable, (size_t)memory->capacity, PROT_READ | PROT_EXEC) == 0
        ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
}

/* A self-loop that walks its load off the end of the projected window, so the
 * run leaves through a fallback with completed iterations behind it. The chain
 * counter already spans those iterations; charging them again both overstates
 * the guest's progress and can exceed the budget the loop was granted. */
static int a_fallback_leaving_a_self_loop_charges_each_iteration_once(void) {
    static const uint8_t body[] = {
        0x8b, 0x03,             /* mov eax,[rbx]  */
        0x48, 0x83, 0xc3, 0x08, /* add rbx,8      */
        0x83, 0xf8, 0x00,       /* cmp eax,0      */
        0x74, 0xf5,             /* je  body       */
    };
    static uint8_t window[32] = {0};
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    memory.write_begin = executable_begin;
    memory.write_end = executable_end;
    hl_native_config config = test_config(&memory, 0);
    hl_native_executor *executor = NULL;
    hl_native_x86_64_cpu state = {0};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
        .architecture = HL_NATIVE_X86_64, .state.x86_64 = &state};
    hl_native_projection_view view = {.guest_first = 0x9000,
        .guest_last = 0x9000 + sizeof(window), .host_first = (uint64_t)(uintptr_t)window,
        .mapping_incarnation = 7, .permissions = 3, .write_policy = HL_NATIVE_WRITE_EXACT,
        .write_index = 0};
    hl_native_projection projection = {.views = &view, .count = 1,
        .mapping_incarnation = 7, .active = 0};
    hl_native_source_span span = {0x9200, body, sizeof(body), 7, 16};
    hl_native_source source = {&span, 1, 7, 16};
    hl_native_run_request request = {.abi = HL_NATIVE_ABI, .size = sizeof(request),
        .architecture = HL_NATIVE_X86_64, .mapping_epoch = 7, .projection = &projection,
        .source = &source, .budget = 64};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};

    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    state.program = 0x9200;
    state.registers[3] = 0x9000;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    /* Four eight-byte loads exhaust the window, so the fifth iteration leaves. */
    CHECK(output.kind == HL_NATIVE_EXIT_FALLBACK);
    CHECK(state.executed == 16 && state.budget == 48);
    CHECK(state.registers[3] == 0x9000 + sizeof(window));
    hl_native_destroy(executor);
    return 0;
}

#else
static int a_fallback_leaving_a_self_loop_charges_each_iteration_once(void) { return 0; }
#endif

int main(void) { return a_fallback_leaving_a_self_loop_charges_each_iteration_once(); }
